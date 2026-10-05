#![allow(clippy::unwrap_used)] // tests may panic on failure

use remora_journal::{
    CommandRecord, EntryKind, EntryRecord, FileChange, FileChangeKind, GENESIS_HASH, Journal, JournalError,
    compute_hash,
};
use tempfile::TempDir;

fn record(prompt: &str) -> EntryRecord {
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
        }],
        changes: vec![FileChange {
            path: "src/lib.rs".into(),
            kind: FileChangeKind::Modified,
        }],
    }
}

#[test]
fn appended_entries_round_trip() {
    let dir = TempDir::new().unwrap();
    let mut j = Journal::open(dir.path()).unwrap();
    for p in ["one", "two", "three"] {
        j.append(record(p)).unwrap();
    }

    let entries = j.entries().unwrap();

    let prompts: Vec<_> = entries.iter().map(|e| e.record.prompt.clone().unwrap()).collect();
    assert_eq!(prompts, ["one", "two", "three"]);
    assert_eq!(entries.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2, 3]);
}

#[test]
fn entries_chain_from_genesis() {
    let dir = TempDir::new().unwrap();
    let mut j = Journal::open(dir.path()).unwrap();
    for p in ["one", "two", "three"] {
        j.append(record(p)).unwrap();
    }

    let entries = j.entries().unwrap();

    assert_eq!(entries[0].prev_hash, GENESIS_HASH);
    assert_eq!(entries[1].prev_hash, entries[0].hash);
    assert_eq!(entries[2].prev_hash, entries[1].hash);
    assert_eq!(
        entries[1].hash,
        compute_hash(&entries[0].hash, &entries[1].record)
    );
}

#[test]
fn rewind_entries_are_supported() {
    let dir = TempDir::new().unwrap();
    let mut j = Journal::open(dir.path()).unwrap();
    let mut r = record("rewind to turn 1");
    r.kind = EntryKind::Rewind;

    j.append(r).unwrap();

    assert_eq!(j.entries().unwrap()[0].record.kind, EntryKind::Rewind);
}

#[cfg(unix)]
#[test]
fn the_signing_key_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    Journal::open(dir.path()).unwrap();
    let mode = std::fs::metadata(dir.path().join("signing.key"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn a_truncated_key_file_is_rejected() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("signing.key"), [1u8; 5]).unwrap();
    assert!(matches!(
        Journal::open(dir.path()),
        Err(JournalError::InvalidKey(_))
    ));
}

// --- Review Focus ---

#[test]
fn reopening_continues_the_chain_with_the_same_key() {
    let dir = TempDir::new().unwrap();
    let key_before = {
        let mut j = Journal::open(dir.path()).unwrap();
        j.append(record("before restart")).unwrap();
        j.verifying_key_hex()
    };

    let mut j = Journal::open(dir.path()).unwrap();
    let second = j.append(record("after restart")).unwrap();

    assert_eq!(j.verifying_key_hex(), key_before);
    assert_eq!(second.prev_hash, j.entries().unwrap()[0].hash);
}

#[test]
fn unicode_and_multiline_prompts_round_trip() {
    let dir = TempDir::new().unwrap();
    let mut j = Journal::open(dir.path()).unwrap();
    let r = record("borrá el build 🦀\nline two\t\"quoted\"\0nul");

    j.append(r.clone()).unwrap();

    assert_eq!(j.entries().unwrap()[0].record, r);
}
