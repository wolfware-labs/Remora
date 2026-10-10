//! Open-turn state, one JSON file per session, so a turn survives a daemon restart.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use remora_journal::CommandRecord;
use remora_snap::SnapshotId;
use serde::{Deserialize, Serialize};

use crate::error::io_at;
use crate::{EngineError, fsutil};

/// Identifies who produced an event: the agent's session id and the agent's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    pub id: String,
    pub agent: String,
}

impl SessionRef {
    pub fn new(id: impl Into<String>, agent: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            agent: agent.into(),
        }
    }
}

/// A session id becomes a file name, so it must be short and plain.
pub(crate) fn check_session_id(id: &str) -> Result<(), EngineError> {
    let valid = (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(EngineError::InvalidSessionId(id.to_owned()))
    }
}

/// A turn that has started and not yet ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OpenTurn {
    pub session_id: String,
    pub agent: String,
    pub prompt: Option<String>,
    pub started_at_ms: i64,
    /// The snapshot taken before the turn changed anything.
    pub pre_snapshot: String,
    pub commands: Vec<CommandRecord>,
    /// When the turn last saw an event. Absent in files written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_ms: Option<i64>,
    /// The start snapshot finished after the agent stopped waiting for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub late_start: Option<bool>,
}

impl OpenTurn {
    /// The time of the turn's latest known event.
    pub(crate) fn last_activity_ms(&self) -> i64 {
        let commands = self.commands.iter().filter_map(|c| c.at_ms);
        commands
            .chain(self.last_event_ms)
            .fold(self.started_at_ms, i64::max)
    }

    /// Every snapshot id the turn refers to is well formed.
    fn snapshot_ids_are_valid(&self) -> bool {
        let valid = |id: &str| SnapshotId::parse(id).is_ok();
        valid(&self.pre_snapshot)
            && self
                .commands
                .iter()
                .filter_map(|c| c.snapshot_id.as_deref())
                .all(valid)
    }
}

pub(crate) fn path_for(sessions_dir: &Path, session_id: &str) -> PathBuf {
    sessions_dir.join(format!("{session_id}.json"))
}

pub(crate) fn load(path: &Path) -> Result<Option<OpenTurn>, EngineError> {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<OpenTurn>(&bytes) {
            Ok(turn) if turn.snapshot_ids_are_valid() => Ok(Some(turn)),
            // A damaged state file (bad JSON or an invalid snapshot id) must not wedge the
            // session, or break rewind for the whole project: set it aside (kept as
            // evidence) and carry on as if no turn were open.
            _ => {
                let aside = unused_aside_path(path);
                fs::rename(path, &aside).map_err(io_at(path))?;
                log::warn!("set aside a damaged session file as {}", aside.display());
                Ok(None)
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_at(path)(e)),
    }
}

/// `<name>.json.corrupt`, or `.corrupt.1`, `.corrupt.2`, ... so earlier evidence is kept.
fn unused_aside_path(path: &Path) -> PathBuf {
    let mut aside = path.with_extension("json.corrupt");
    let mut n = 1;
    while fs::symlink_metadata(&aside).is_ok() {
        aside = path.with_extension(format!("json.corrupt.{n}"));
        n += 1;
    }
    aside
}

pub(crate) fn save(path: &Path, turn: &OpenTurn) -> Result<(), EngineError> {
    let json = serde_json::to_vec(turn).expect("session state always serializes");
    fsutil::write_private_atomic(path, &json).map_err(io_at(path))
}

pub(crate) fn remove(path: &Path) -> Result<(), EngineError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_at(path)(e)),
    }
}

/// Every open turn in the project, oldest first.
pub(crate) fn load_all(sessions_dir: &Path) -> Result<Vec<OpenTurn>, EngineError> {
    let mut turns = Vec::new();
    for entry in fs::read_dir(sessions_dir).map_err(io_at(sessions_dir))? {
        let path = entry.map_err(io_at(sessions_dir))?.path();
        if path.extension().is_some_and(|e| e == "json")
            && let Some(turn) = load(&path)?
        {
            turns.push(turn);
        }
    }
    turns.sort_by(|a, b| (a.started_at_ms, &a.session_id).cmp(&(b.started_at_ms, &b.session_id)));
    Ok(turns)
}
