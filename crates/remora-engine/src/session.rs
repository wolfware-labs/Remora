//! Open-turn state, one JSON file per session, so a turn survives a daemon restart.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use remora_journal::CommandRecord;
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
}

pub(crate) fn path_for(sessions_dir: &Path, session_id: &str) -> PathBuf {
    sessions_dir.join(format!("{session_id}.json"))
}

pub(crate) fn load(path: &Path) -> Result<Option<OpenTurn>, EngineError> {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(turn) => Ok(Some(turn)),
            // A damaged state file must not wedge the session forever: set it aside (kept
            // as evidence) and carry on as if no turn were open.
            Err(_) => {
                let aside = path.with_extension("json.corrupt");
                fs::rename(path, &aside).map_err(io_at(path))?;
                Ok(None)
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_at(path)(e)),
    }
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
