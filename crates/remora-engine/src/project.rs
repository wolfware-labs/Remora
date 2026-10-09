use std::path::{Path, PathBuf};

use remora_journal::{
    CommandRecord, Entry, EntryKind, EntryRecord, FileChange, FileChangeKind, Journal, SigningKey,
};
use remora_snap::{
    ChangeKind, CopySnapshotter, RestoreReport, SnapError, SnapOptions, SnapshotId, Snapshotter,
};
use serde::{Deserialize, Serialize};

use crate::error::io_at;
use crate::lock::ProjectLock;
use crate::session::{self, OpenTurn, SessionRef};
use crate::{EngineError, fsutil};

/// The session id recorded on journal entries for rewinds (they are user actions, not
/// agent turns).
pub const REWIND_SESSION: &str = "remora-rewind";
/// The agent name recorded on rewind entries and on turns opened implicitly.
pub const REMORA_AGENT: &str = "remora";
/// Restore point labels (prompts, commands) are cut to this many bytes, so listing points
/// stays small. Points are matched on the cut label, so their identity is unaffected.
pub const MAX_LABEL_BYTES: usize = 512;

/// `text` cut to [`MAX_LABEL_BYTES`] at a character boundary, with `…` when cut.
pub fn short_label(text: &str) -> String {
    if text.len() <= MAX_LABEL_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_LABEL_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointKind {
    /// Before a turn changed anything.
    TurnStart,
    /// Just before a command ran (the command is the point's label).
    BeforeCommand,
    /// The state right before a rewind, so the rewind itself can be undone.
    BeforeRewind,
}

/// A state the project can be rewound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorePoint {
    /// Position in the project's timeline; identifies the point unambiguously even when
    /// two points share a snapshot (identical trees).
    pub position: usize,
    pub snapshot: SnapshotId,
    pub kind: PointKind,
    pub session_id: String,
    /// When the point was taken.
    pub at_ms: i64,
    /// The prompt (turn start) or the command (before command) this point precedes.
    pub label: Option<String>,
    /// True if the point belongs to a turn that has not ended yet.
    pub open: bool,
    /// A checkpoint taken after the agent stopped waiting for it: the command may already
    /// have started, so the point may include its effects.
    #[serde(default)]
    pub late: bool,
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

/// What a rewind would change (see [`Project::preview`]). Paths are relative to the root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preview {
    /// Absent now, present at the point: brought back.
    pub restore: Vec<String>,
    /// Present now, absent at the point: removed.
    pub delete: Vec<String>,
    /// Different now: reverted.
    pub revert: Vec<String>,
    /// Commands after the point, oldest first: their effects outside the project stay.
    pub commands_after: Vec<CommandRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub root: PathBuf,
    pub open_sessions: Vec<String>,
    pub journal_entries: usize,
    pub restore_points: usize,
    /// Paths the most recent snapshot by this process could not protect.
    pub last_unprotected: Vec<PathBuf>,
    /// Where the project's blobs are stored.
    #[serde(default)]
    pub blob_store: PathBuf,
    /// Whether the most recent snapshot by this process that stored files cloned them
    /// (copy-on-write) or copied them; `None` until one has.
    #[serde(default)]
    pub copy_on_write: Option<bool>,
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
    copy_on_write: Option<bool>,
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
        blobs: Option<PathBuf>,
        key: SigningKey,
    ) -> Result<Self, EngineError> {
        let snap = match blobs {
            Some(blobs) => CopySnapshotter::with_blobs(dir.join("store"), blobs, SnapOptions::default())?,
            None => CopySnapshotter::new(dir.join("store"), SnapOptions::default())?,
        };
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
            copy_on_write: None,
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
        if s.stored_files > 0 {
            self.copy_on_write = Some(s.cloned_files > 0);
        }
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
            last_event_ms: Some(at_ms),
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
        tool_use_id: Option<&str>,
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
                last_event_ms: None,
            },
        };
        turn.commands.push(CommandRecord {
            command: command.to_owned(),
            exit_code: None,
            snapshot_id: Some(checkpoint.to_string()),
            at_ms: Some(at_ms),
            tool_use_id: tool_use_id.map(str::to_owned),
            late: None,
        });
        turn.last_event_ms = Some(at_ms);
        session::save(&path, &turn)?;
        Ok(checkpoint)
    }

    /// A command finished: record its exit code on the matching command, and note the turn
    /// as active. With a `tool_use_id` the command with that id matches; without one, the
    /// latest pending command with the same text. Ignored if no turn is open.
    pub fn after_command(
        &mut self,
        session: &SessionRef,
        command: &str,
        tool_use_id: Option<&str>,
        exit_code: Option<i32>,
        at_ms: i64,
    ) -> Result<(), EngineError> {
        session::check_session_id(&session.id)?;
        let _lock = self.lock()?;
        let path = session::path_for(&self.sessions_dir(), &session.id);
        if let Some(mut turn) = session::load(&path)? {
            let matches = |c: &CommandRecord| match tool_use_id {
                Some(id) => c.tool_use_id.as_deref() == Some(id),
                None => c.command == command && c.exit_code.is_none(),
            };
            if let Some(pending) = turn.commands.iter_mut().rev().find(|c| matches(c)) {
                pending.exit_code = exit_code;
            }
            turn.last_event_ms = Some(at_ms);
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

    /// Mark a command's checkpoint as late: it finished after the agent stopped waiting, so
    /// the command may already have started. The command is the one with `tool_use_id`, or
    /// the session's latest command without one. Ignored if there is no such command.
    pub fn mark_late(&mut self, session: &SessionRef, tool_use_id: Option<&str>) -> Result<(), EngineError> {
        session::check_session_id(&session.id)?;
        let _lock = self.lock()?;
        let path = session::path_for(&self.sessions_dir(), &session.id);
        if let Some(mut turn) = session::load(&path)? {
            let command = match tool_use_id {
                Some(id) => turn
                    .commands
                    .iter_mut()
                    .rev()
                    .find(|c| c.tool_use_id.as_deref() == Some(id)),
                None => turn.commands.last_mut(),
            };
            if let Some(command) = command {
                command.late = Some(true);
                session::save(&path, &turn)?;
            }
        }
        Ok(())
    }

    /// The agent's session ended: close its open turn, if any, as interrupted (a turn that
    /// ended normally was already closed by `end_turn`).
    pub fn end_session(&mut self, session: &SessionRef, at_ms: i64) -> Result<Option<Entry>, EngineError> {
        session::check_session_id(&session.id)?;
        let _lock = self.lock()?;
        let path = session::path_for(&self.sessions_dir(), &session.id);
        match session::load(&path)? {
            Some(turn) => self.close_turn(turn, at_ms, true).map(Some),
            None => Ok(None),
        }
    }

    /// Close, as interrupted, every open turn with no event since `idle_before_ms`: an agent
    /// that was interrupted never sends its end event. Returns the journaled entries.
    pub fn close_idle_turns(&mut self, idle_before_ms: i64, at_ms: i64) -> Result<Vec<Entry>, EngineError> {
        let _lock = self.lock()?;
        let mut closed = Vec::new();
        for turn in session::load_all(&self.sessions_dir())? {
            if turn.last_activity_ms() < idle_before_ms {
                let session_id = turn.session_id.clone();
                match self.close_turn(turn, at_ms, true) {
                    Ok(entry) => closed.push(entry),
                    // One turn that cannot be closed must not keep the others open.
                    Err(e) => log::warn!("cannot close the idle turn of session {session_id}: {e}"),
                }
            }
        }
        Ok(closed)
    }

    fn close_turn(&mut self, turn: OpenTurn, at_ms: i64, interrupted: bool) -> Result<Entry, EngineError> {
        let pre = SnapshotId::parse(&turn.pre_snapshot)?;
        let post = self.take_snapshot()?;
        // A pre-snapshot that can no longer be read (store damage) must not wedge the
        // session: journal the turn without its changes, marked incomplete.
        let (changes, incomplete) = match self.changes_between(&pre, &post) {
            Ok(changes) => (changes, None),
            Err(e) => {
                log::warn!(
                    "journaling the turn of session {} without its changes: {e}",
                    turn.session_id
                );
                (Vec::new(), Some(true))
            }
        };
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
            incomplete,
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

    /// Everything that happened, ordered by time. Within one turn the order is the order of
    /// events; at equal times journaled entries come before open turns.
    fn timeline(&self) -> Result<Vec<Item>, EngineError> {
        let mut events: Vec<(i64, Item)> = Vec::new();
        for entry in self.journal.entries()? {
            let r = &entry.record;
            match r.kind {
                EntryKind::Turn => push_turn(
                    &mut events,
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
                        events.push((
                            r.started_at_ms,
                            Item::Point(RestorePoint {
                                position: 0,
                                snapshot: SnapshotId::parse(safety)?,
                                kind: PointKind::BeforeRewind,
                                session_id: r.session_id.clone(),
                                at_ms: r.started_at_ms,
                                label: r.rewind_target.clone(),
                                open: false,
                                late: false,
                            }),
                        ));
                    }
                }
            }
        }
        for turn in session::load_all(&self.sessions_dir())? {
            push_turn(
                &mut events,
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
        // A stable sort: overlapping sessions interleave by time, and each turn keeps its order.
        events.sort_by_key(|(at_ms, _)| *at_ms);
        let mut items: Vec<Item> = events.into_iter().map(|(_, item)| item).collect();
        for (position, item) in items.iter_mut().enumerate() {
            if let Item::Point(point) = item {
                point.position = position;
            }
        }
        Ok(items)
    }

    fn points(&self) -> Result<Vec<RestorePoint>, EngineError> {
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

    /// Every state the project can be rewound to, newest first.
    pub fn restore_points(&self) -> Result<Vec<RestorePoint>, EngineError> {
        let _lock = self.lock()?;
        self.points()
    }

    /// Restore the project to `point`. Refused while a turn is open, unless `force`.
    ///
    /// A safety snapshot is taken first and the rewind is always journaled with what
    /// actually changed, so it can be undone even if the restore fails part-way (the entry
    /// is then marked `incomplete`, and a hard failure returns
    /// [`EngineError::RewindIncomplete`] carrying the safety snapshot).
    pub fn rewind(
        &mut self,
        point: &RestorePoint,
        at_ms: i64,
        force: bool,
    ) -> Result<RewindReport, EngineError> {
        let _lock = self.lock()?;
        self.refuse_while_a_turn_is_open(force)?;
        let commands_after = self.commands_after(point)?;
        self.restore_to(&point.snapshot, commands_after, at_ms)
    }

    /// Restore the project to a snapshot by id, for instance the safety snapshot of a
    /// rewind that did not finish. Journaled like a rewind; refused while a turn is open,
    /// unless `force`.
    pub fn restore_snapshot(
        &mut self,
        snapshot: &SnapshotId,
        at_ms: i64,
        force: bool,
    ) -> Result<RewindReport, EngineError> {
        let _lock = self.lock()?;
        self.refuse_while_a_turn_is_open(force)?;
        // An unknown id is refused before anything changes.
        match self.snap.manifest(snapshot) {
            Ok(_) => {}
            Err(SnapError::NotFound(_)) => {
                return Err(EngineError::UnknownRestorePoint(snapshot.to_string()));
            }
            Err(e) => return Err(e.into()),
        }
        self.restore_to(snapshot, Vec::new(), at_ms)
    }

    /// What rewinding to `point` would change, without changing anything: paths it would
    /// bring back, remove and revert, and the commands after it that a rewind cannot undo.
    pub fn preview(&mut self, point: &RestorePoint) -> Result<Preview, EngineError> {
        let _lock = self.lock()?;
        let commands_after = self.commands_after(point)?;
        let now = self.take_snapshot()?;
        let current = self.snap.manifest(&now)?;
        let target = self.snap.manifest(&point.snapshot)?;
        let mut preview = Preview {
            commands_after,
            ..Preview::default()
        };
        for change in current.diff(&target) {
            match change.kind {
                ChangeKind::Added => preview.restore.push(change.path),
                ChangeKind::Deleted => preview.delete.push(change.path),
                ChangeKind::Modified => preview.revert.push(change.path),
            }
        }
        Ok(preview)
    }

    fn refuse_while_a_turn_is_open(&self, force: bool) -> Result<(), EngineError> {
        match session::load_all(&self.sessions_dir())?.first() {
            Some(open) if !force => Err(EngineError::TurnInProgress {
                session: open.session_id.clone(),
            }),
            _ => Ok(()),
        }
    }

    /// The commands recorded after `point`, oldest first.
    fn commands_after(&self, point: &RestorePoint) -> Result<Vec<CommandRecord>, EngineError> {
        let timeline = self.timeline()?;
        let index = locate(&timeline, point)?;
        Ok(timeline[index + 1..]
            .iter()
            .filter_map(|item| match item {
                Item::Command(c) => Some(c.clone()),
                Item::Point(_) => None,
            })
            .collect())
    }

    fn restore_to(
        &mut self,
        target: &SnapshotId,
        commands_after: Vec<CommandRecord>,
        at_ms: i64,
    ) -> Result<RewindReport, EngineError> {
        let safety = self.take_snapshot()?;
        // From here on the project may change, so every failure carries the safety
        // snapshot: the rewind can always be undone.
        let incomplete = |source: EngineError| EngineError::RewindIncomplete {
            safety_snapshot: safety.to_string(),
            source: Box::new(source),
        };
        let restore = self.snap.restore(&self.root, target);
        let restored_cleanly = matches!(&restore, Ok(r) if r.failed.is_empty());
        let entry = self.journal_rewind(&safety, target, at_ms, restored_cleanly);
        match (restore, entry) {
            (Ok(restore), Ok(entry)) => Ok(RewindReport {
                restore,
                safety_snapshot: safety,
                commands_after,
                entry,
            }),
            (Err(source), _) => Err(incomplete(source.into())),
            (Ok(_), Err(source)) => Err(incomplete(source)),
        }
    }

    /// Journal a rewind with what really changed. If the after-state cannot be read, the
    /// entry is still written (no changes listed, marked incomplete) so the safety
    /// snapshot stays reachable as a restore point.
    fn journal_rewind(
        &mut self,
        safety: &SnapshotId,
        target: &SnapshotId,
        at_ms: i64,
        restored_cleanly: bool,
    ) -> Result<Entry, EngineError> {
        let observed = self
            .take_snapshot()
            .and_then(|after| self.changes_between(safety, &after));
        let (changes, complete) = match observed {
            Ok(changes) => (changes, restored_cleanly),
            Err(_) => (Vec::new(), false),
        };
        Ok(self.journal.append(EntryRecord {
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
            rewind_target: Some(target.to_string()),
            incomplete: (!complete).then_some(true),
        })?)
    }

    pub fn status(&self) -> Result<Status, EngineError> {
        let _lock = self.lock()?;
        Ok(Status {
            root: self.root.clone(),
            open_sessions: session::load_all(&self.sessions_dir())?
                .into_iter()
                .map(|t| t.session_id)
                .collect(),
            journal_entries: self.journal.entries()?.len(),
            restore_points: self.points()?.len(),
            last_unprotected: self.last_unprotected.clone(),
            blob_store: self.snap.blobs_dir().to_path_buf(),
            copy_on_write: self.copy_on_write,
        })
    }

    /// The journal, for inspection and verification.
    pub fn journal(&self) -> &Journal {
        &self.journal
    }
}

/// The same point: everything but its position and whether its turn is still open, both
/// of which change as other sessions record events or the turn ends.
fn same_point(a: &RestorePoint, b: &RestorePoint) -> bool {
    a.snapshot == b.snapshot
        && a.kind == b.kind
        && a.session_id == b.session_id
        && a.at_ms == b.at_ms
        && a.label == b.label
}

/// Find `point` in the timeline: at its recorded position if it is still there, else anywhere.
fn locate(timeline: &[Item], point: &RestorePoint) -> Result<usize, EngineError> {
    let at = |i: usize| matches!(timeline.get(i), Some(Item::Point(p)) if same_point(p, point));
    if at(point.position) {
        return Ok(point.position);
    }
    (0..timeline.len())
        .find(|&i| at(i))
        .ok_or_else(|| EngineError::UnknownRestorePoint(point.snapshot.to_string()))
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

fn push_turn(events: &mut Vec<(i64, Item)>, turn: &TurnView<'_>) -> Result<(), EngineError> {
    let point =
        |at_ms: i64, snapshot: &str, kind, label: Option<&str>, late: bool| -> Result<Item, EngineError> {
            Ok(Item::Point(RestorePoint {
                position: 0,
                snapshot: SnapshotId::parse(snapshot)?,
                kind,
                session_id: turn.session_id.to_owned(),
                at_ms,
                label: label.map(short_label),
                open: turn.open,
                late,
            }))
        };
    if let Some(pre) = turn.pre_snapshot {
        events.push((
            turn.at_ms,
            point(turn.at_ms, pre, PointKind::TurnStart, turn.prompt, false)?,
        ));
    }
    for cmd in turn.commands {
        // A clock that went backwards must not move a command before its own turn.
        let at_ms = cmd.at_ms.unwrap_or(turn.at_ms).max(turn.at_ms);
        if let Some(checkpoint) = &cmd.snapshot_id {
            events.push((
                at_ms,
                point(
                    at_ms,
                    checkpoint,
                    PointKind::BeforeCommand,
                    Some(&cmd.command),
                    cmd.late == Some(true),
                )?,
            ));
        }
        events.push((at_ms, Item::Command(cmd.clone())));
    }
    Ok(())
}
