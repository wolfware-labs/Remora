use std::path::{Path, PathBuf};

use remora_journal::{
    CommandRecord, Entry, EntryKind, EntryRecord, FileChange, FileChangeKind, Journal, SigningKey,
};
use remora_snap::{ChangeKind, CopySnapshotter, SnapOptions, SnapshotId, Snapshotter};

use crate::error::io_at;
use crate::lock::ProjectLock;
use crate::session::{self, OpenTurn, SessionRef};
use crate::{EngineError, fsutil};

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

    /// The journal, for inspection and verification.
    pub fn journal(&self) -> &Journal {
        &self.journal
    }
}
