//! Helpers shared by the journal test suites.
#![allow(dead_code, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use remora_journal::{
    CommandRecord, EntryKind, EntryRecord, FileChange, FileChangeKind, Journal, SigningKey,
};
use rusqlite::Connection;

/// A fixed key, so tests are deterministic.
pub fn key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

pub fn db(dir: &Path) -> PathBuf {
    dir.join("journal.db")
}

pub fn open(dir: &Path) -> Journal {
    Journal::open(&db(dir), key()).unwrap()
}

pub fn record(prompt: &str) -> EntryRecord {
    EntryRecord {
        session_id: "session-1".into(),
        kind: EntryKind::Turn,
        agent: "claude-code".into(),
        started_at_ms: 1_000,
        ended_at_ms: 2_000,
        snapshot_id: Some("ab".repeat(32)),
        prompt: Some(prompt.into()),
        commands: vec![CommandRecord {
            command: "cargo test".into(),
            exit_code: Some(0),
            snapshot_id: None,
            at_ms: None,
            tool_use_id: None,
        }],
        changes: vec![FileChange {
            path: "src/lib.rs".into(),
            kind: FileChangeKind::Modified,
        }],
        interrupted: None,
        rewind_target: None,
        incomplete: None,
    }
}

/// Journal with entries "one", "two", "three" (seq 1, 2, 3).
pub fn journal_with_three(dir: &Path) -> Journal {
    let mut j = open(dir);
    for p in ["one", "two", "three"] {
        j.append(record(p)).unwrap();
    }
    j
}

/// A second connection that bypasses the Journal API, like an attacker editing the file.
pub fn raw_db(dir: &Path) -> Connection {
    Connection::open(db(dir)).unwrap()
}
