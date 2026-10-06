use std::path::{Path, PathBuf};

use remora_journal::{
    CommandRecord, Entry, EntryKind, EntryRecord, FileChange, FileChangeKind, Journal, SigningKey,
};
use remora_snap::{ChangeKind, CopySnapshotter, RestoreReport, SnapOptions, SnapshotId, Snapshotter};

use crate::error::io_at;
use crate::lock::ProjectLock;
use crate::session::{self, OpenTurn, SessionRef};
use crate::{EngineError, fsutil};

/// The session id recorded on journal entries for rewinds (they are user actions, not
/// agent turns).
pub const REWIND_SESSION: &str = "remora-rewind";
/// The agent name recorded on rewind entries and on turns opened implicitly.
pub const REMORA_AGENT: &str = "remora";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointKind {
    /// Before a turn changed anything.
    TurnStart,
    /// Just before a command ran (the command is the point's label).
    BeforeCommand,
    /// The state right before a rewind, so the rewind itself can be undone.
    BeforeRewind,
}

/// A state the project can be rewound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePoint {
    /// Position in the project's timeline; identifies the point unambiguously even when
    /// two points share a snapshot (identical trees).
    pub position: usize,
    pub snapshot: SnapshotId,
    pub kind: PointKind,
    pub session_id: String,
    /// When the turn (or rewind) this point belongs to started.
    pub at_ms: i64,
    /// The prompt (turn start) or the command (before command) this point precedes.
    pub label: Option<String>,
    /// True if the point belongs to a turn that has not ended yet.
    pub open: bool,
}

#[derive(Debug)]
pub struct RewindReport {
    pub restore: RestoreReport,
    /// The state just before the rewind; rewinding to it undoes the rewind.
    pub safety_snapshot: SnapshotId,
    /// Commands that ran after the restore point. Their effects outside the project
    /// (pushes, network calls, database writes) are not undone: review them.
    pub commands_after: Vec<CommandRecord>,
    pub entry: Entry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub root: PathBuf,
    pub open_sessions: Vec<String>,
    pub journal_entries: usize,
    pub restore_points: usize,
    /// Paths the most recent snapshot by this process could not protect.
    pub last_unprotected: Vec<PathBuf>,
}

enum Item {
    Point(RestorePoint),
    Command(CommandRecord),
}

/// One project's snapshots, journal and open turns.
pub struct Project {
    id: String,
    root: PathBuf,
    dir: PathBuf,
    snap: CopySnapshotter,
    journal: Journal,
    last_unprotected: Vec<PathBuf>,
}

// Hand-written so the signing key inside the journal can never end up in a log.
impl std::fmt::Debug for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Project")
            .field("id", &self.id)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl Project {
    pub(crate) fn open(
        id: String,
        root: PathBuf,
        dir: PathBuf,
        key: SigningKey,
    ) -> Result<Self, EngineError> {
        let snap = CopySnapshotter::new(dir.join("store"), SnapOptions::default())?;
        let journal = Journal::open(&dir.join("journal.db"), key)?;
        let sessions = dir.join("sessions");
        fsutil::private_dir(&sessions).map_err(io_at(&sessions))?;
        Ok(Self {
            id,
            root,
            dir,
            snap,
            journal,
            last_unprotected: Vec::new(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn sessions_dir(&self) -> PathBuf {
        self.dir.join("sessions")
    }

    fn lock(&self) -> Result<ProjectLock, EngineError> {
        ProjectLock::acquire(&self.dir.join("lock"))
    }

    fn take_snapshot(&mut self) -> Result<SnapshotId, EngineError> {
        // The agent may have deleted the whole project folder: recreate it empty, so the
        // snapshot records "everything deleted" instead of failing.
        if !self.root.exists() {
            std::fs::create_dir_all(&self.root).map_err(io_at(&self.root))?;
        }
        let s = self.snap.snapshot(&self.root)?;
        self.last_unprotected = s.unprotected;
        Ok(s.id)
    }

    /// A turn starts: snapshot the project before the agent changes anything.
    /// A turn still open in this session is closed first and marked interrupted.
    pub fn begin_turn(
        &mut self,
        session: &SessionRef,
        prompt: Option<&str>,
        at_ms: i64,
    ) -> Result<SnapshotId, EngineError> {
        session::check_session_id(&session.id)?;
        let _lock = self.lock()?;
        let path = session::path_for(&self.sessions_dir(), &session.id);
        if let Some(open) = session::load(&path)? {
            self.close_turn(open, at_ms, true)?;
        }
        let pre = self.take_snapshot()?;
        let turn = OpenTurn {
            session_id: session.id.clone(),
            agent: session.agent.clone(),
            prompt: prompt.map(str::to_owned),
            started_at_ms: at_ms,
            pre_snapshot: pre.to_string(),
            commands: Vec::new(),
        };
        session::save(&path, &turn)?;
        Ok(pre)
    }

    /// A command is about to run: take a checkpoint so its effects can be undone.
    /// Without an open turn, an implicit turn (no prompt) is opened first.
    pub fn before_command(
        &mut self,
        session: &SessionRef,
        command: &str,
        at_ms: i64,
    ) -> Result<SnapshotId, EngineError> {
        session::check_session_id(&session.id)?;
        let _lock = self.lock()?;
        let path = session::path_for(&self.sessions_dir(), &session.id);
        let checkpoint = self.take_snapshot()?;
        let mut turn = match session::load(&path)? {
            Some(turn) => turn,
            None => OpenTurn {
                session_id: session.id.clone(),
                agent: session.agent.clone(),
                prompt: None,
                started_at_ms: at_ms,
                pre_snapshot: checkpoint.to_string(),
                commands: Vec::new(),
            },
        };
        turn.commands.push(CommandRecord {
            command: command.to_owned(),
            exit_code: None,
            snapshot_id: Some(checkpoint.to_string()),
        });
        session::save(&path, &turn)?;
        Ok(checkpoint)
    }

    /// A command finished: record its exit code. Ignored if no matching command is pending.
    pub fn after_command(
        &mut self,
        session: &SessionRef,
        command: &str,
        exit_code: Option<i32>,
    ) -> Result<(), EngineError> {
        session::check_session_id(&session.id)?;
        let _lock = self.lock()?;
        let path = session::path_for(&self.sessions_dir(), &session.id);
        if let Some(mut turn) = session::load(&path)?
            && let Some(pending) = turn
                .commands
                .iter_mut()
                .rev()
                .find(|c| c.command == command && c.exit_code.is_none())
        {
            pending.exit_code = exit_code;
            session::save(&path, &turn)?;
        }
        Ok(())
    }

    /// A turn ended: journal what changed. Returns `None` if no turn was open.
    pub fn end_turn(&mut self, session: &SessionRef, at_ms: i64) -> Result<Option<Entry>, EngineError> {
        session::check_session_id(&session.id)?;
        let _lock = self.lock()?;
        let path = session::path_for(&self.sessions_dir(), &session.id);
        match session::load(&path)? {
            Some(turn) => self.close_turn(turn, at_ms, false).map(Some),
            None => Ok(None),
        }
    }

    fn close_turn(&mut self, turn: OpenTurn, at_ms: i64, interrupted: bool) -> Result<Entry, EngineError> {
        let pre = SnapshotId::parse(&turn.pre_snapshot)?;
        let post = self.take_snapshot()?;
        let changes = self.changes_between(&pre, &post)?;
        let entry = self.journal.append(EntryRecord {
            session_id: turn.session_id.clone(),
            kind: EntryKind::Turn,
            agent: turn.agent,
            started_at_ms: turn.started_at_ms,
            ended_at_ms: at_ms,
            snapshot_id: Some(turn.pre_snapshot),
            prompt: turn.prompt,
            commands: turn.commands,
            changes,
            interrupted: interrupted.then_some(true),
            rewind_target: None,
        })?;
        session::remove(&session::path_for(&self.sessions_dir(), &turn.session_id))?;
        Ok(entry)
    }

    fn changes_between(&self, from: &SnapshotId, to: &SnapshotId) -> Result<Vec<FileChange>, EngineError> {
        let older = self.snap.manifest(from)?;
        let newer = self.snap.manifest(to)?;
        Ok(older
            .diff(&newer)
            .into_iter()
            .map(|c| FileChange {
                path: c.path,
                kind: match c.kind {
                    ChangeKind::Added => FileChangeKind::Added,
                    ChangeKind::Modified => FileChangeKind::Modified,
                    ChangeKind::Deleted => FileChangeKind::Deleted,
                },
            })
            .collect())
    }

    /// Everything that happened, oldest first: journaled entries, then open turns.
    fn timeline(&self) -> Result<Vec<Item>, EngineError> {
        let mut items = Vec::new();
        for entry in self.journal.entries()? {
            let r = &entry.record;
            match r.kind {
                EntryKind::Turn => push_turn(
                    &mut items,
                    &TurnView {
                        session_id: &r.session_id,
                        at_ms: r.started_at_ms,
                        pre_snapshot: r.snapshot_id.as_deref(),
                        prompt: r.prompt.as_deref(),
                        commands: &r.commands,
                        open: false,
                    },
                )?,
                EntryKind::Rewind => {
                    if let Some(safety) = &r.snapshot_id {
                        let position = items.len();
                        items.push(Item::Point(RestorePoint {
                            position,
                            snapshot: SnapshotId::parse(safety)?,
                            kind: PointKind::BeforeRewind,
                            session_id: r.session_id.clone(),
                            at_ms: r.started_at_ms,
                            label: r.rewind_target.clone(),
                            open: false,
                        }));
                    }
                }
            }
        }
        for turn in session::load_all(&self.sessions_dir())? {
            push_turn(
                &mut items,
                &TurnView {
                    session_id: &turn.session_id,
                    at_ms: turn.started_at_ms,
                    pre_snapshot: Some(&turn.pre_snapshot),
                    prompt: turn.prompt.as_deref(),
                    commands: &turn.commands,
                    open: true,
                },
            )?;
        }
        Ok(items)
    }

    /// Every state the project can be rewound to, newest first.
    pub fn restore_points(&self) -> Result<Vec<RestorePoint>, EngineError> {
        let mut points: Vec<RestorePoint> = self
            .timeline()?
            .into_iter()
            .filter_map(|item| match item {
                Item::Point(p) => Some(p),
                Item::Command(_) => None,
            })
            .collect();
        points.reverse();
        Ok(points)
    }

    /// Restore the project to `point`. Refused while a turn is open, unless `force`.
    /// A safety snapshot is taken first, so the rewind can itself be undone.
    pub fn rewind(
        &mut self,
        point: &RestorePoint,
        at_ms: i64,
        force: bool,
    ) -> Result<RewindReport, EngineError> {
        let _lock = self.lock()?;
        if let Some(open) = session::load_all(&self.sessions_dir())?.first()
            && !force
        {
            return Err(EngineError::TurnInProgress {
                session: open.session_id.clone(),
            });
        }
        let timeline = self.timeline()?;
        let known = matches!(timeline.get(point.position), Some(Item::Point(p)) if p == point);
        if !known {
            return Err(EngineError::UnknownRestorePoint(point.snapshot.to_string()));
        }
        let commands_after = timeline[point.position + 1..]
            .iter()
            .filter_map(|item| match item {
                Item::Command(c) => Some(c.clone()),
                Item::Point(_) => None,
            })
            .collect();

        let safety = self.take_snapshot()?;
        let restore = self.snap.restore(&self.root, &point.snapshot)?;
        let changes = self.changes_between(&safety, &point.snapshot)?;
        let entry = self.journal.append(EntryRecord {
            session_id: REWIND_SESSION.to_owned(),
            kind: EntryKind::Rewind,
            agent: REMORA_AGENT.to_owned(),
            started_at_ms: at_ms,
            ended_at_ms: at_ms,
            snapshot_id: Some(safety.to_string()),
            prompt: None,
            commands: Vec::new(),
            changes,
            interrupted: None,
            rewind_target: Some(point.snapshot.to_string()),
        })?;
        Ok(RewindReport {
            restore,
            safety_snapshot: safety,
            commands_after,
            entry,
        })
    }

    pub fn status(&self) -> Result<Status, EngineError> {
        Ok(Status {
            root: self.root.clone(),
            open_sessions: session::load_all(&self.sessions_dir())?
                .into_iter()
                .map(|t| t.session_id)
                .collect(),
            journal_entries: self.journal.entries()?.len(),
            restore_points: self.restore_points()?.len(),
            last_unprotected: self.last_unprotected.clone(),
        })
    }

    /// The journal, for inspection and verification.
    pub fn journal(&self) -> &Journal {
        &self.journal
    }
}

/// A turn as the timeline sees it, whether journaled or still open.
struct TurnView<'a> {
    session_id: &'a str,
    at_ms: i64,
    pre_snapshot: Option<&'a str>,
    prompt: Option<&'a str>,
    commands: &'a [CommandRecord],
    open: bool,
}

fn push_turn(items: &mut Vec<Item>, turn: &TurnView<'_>) -> Result<(), EngineError> {
    let point = |position, snapshot: &str, kind, label: Option<&str>| -> Result<Item, EngineError> {
        Ok(Item::Point(RestorePoint {
            position,
            snapshot: SnapshotId::parse(snapshot)?,
            kind,
            session_id: turn.session_id.to_owned(),
            at_ms: turn.at_ms,
            label: label.map(str::to_owned),
            open: turn.open,
        }))
    };
    if let Some(pre) = turn.pre_snapshot {
        items.push(point(items.len(), pre, PointKind::TurnStart, turn.prompt)?);
    }
    for cmd in turn.commands {
        if let Some(checkpoint) = &cmd.snapshot_id {
            items.push(point(
                items.len(),
                checkpoint,
                PointKind::BeforeCommand,
                Some(&cmd.command),
            )?);
        }
        items.push(Item::Command(cmd.clone()));
    }
    Ok(())
}
