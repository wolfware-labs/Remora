//! The journal holds prompts and commands, which can contain secrets: everything is owner-only.
#![cfg(unix)]
#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use remora_journal::{EntryKind, EntryRecord, Journal};
use tempfile::TempDir;

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn record() -> EntryRecord {
    EntryRecord {
        session_id: "s".into(),
        kind: EntryKind::Turn,
        agent: "claude-code".into(),
        started_at_ms: 1,
        ended_at_ms: 2,
        snapshot_id: None,
        prompt: Some("export API_KEY=secret".into()),
        commands: vec![],
        changes: vec![],
    }
}

#[test]
fn the_journal_directory_and_database_are_owner_only() {
    let parent = TempDir::new().unwrap();
    let dir = parent.path().join("journal");

    let mut j = Journal::open(&dir).unwrap();
    j.append(record()).unwrap();

    assert_eq!(mode(&dir), 0o700);
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        assert_eq!(mode(&path), 0o600, "{}", path.display());
    }
}

#[test]
fn reopening_keeps_existing_entries() {
    let parent = TempDir::new().unwrap();
    let dir = parent.path().join("journal");
    Journal::open(&dir).unwrap().append(record()).unwrap();

    let j = Journal::open(&dir).unwrap();

    assert_eq!(
        j.entries().unwrap().len(),
        1,
        "opening must never truncate journal.db"
    );
}
