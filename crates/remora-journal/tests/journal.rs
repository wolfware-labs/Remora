#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use common::{db, key, open, raw_db, record};
use remora_journal::{EntryKind, GENESIS_HASH, Journal, JournalError, compute_hash};
use tempfile::TempDir;

#[test]
fn appended_entries_round_trip() {
    let dir = TempDir::new().unwrap();
    let mut j = open(dir.path());
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
    let mut j = open(dir.path());
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
fn rewind_entries_and_new_fields_round_trip() {
    let dir = TempDir::new().unwrap();
    let mut j = open(dir.path());
    let mut turn = record("interrupted turn");
    turn.interrupted = Some(true);
    turn.commands[0].snapshot_id = Some("cd".repeat(32));
    let mut rewind = record("rewind");
    rewind.kind = EntryKind::Rewind;
    rewind.rewind_target = Some("ef".repeat(32));

    j.append(turn.clone()).unwrap();
    j.append(rewind.clone()).unwrap();

    let entries = j.entries().unwrap();
    assert_eq!(entries[0].record, turn);
    assert_eq!(entries[1].record, rewind);
}

#[test]
fn reopening_continues_the_chain() {
    let dir = TempDir::new().unwrap();
    open(dir.path()).append(record("before restart")).unwrap();

    let mut j = open(dir.path());
    let second = j.append(record("after restart")).unwrap();

    assert_eq!(second.prev_hash, j.entries().unwrap()[0].hash);
    assert_eq!(
        j.verifying_key_hex(),
        hex::encode(key().verifying_key().to_bytes())
    );
}

#[test]
fn unicode_and_multiline_prompts_round_trip() {
    let dir = TempDir::new().unwrap();
    let mut j = open(dir.path());
    let r = record("borrá el build 🦀\nline two\t\"quoted\"\0nul");

    j.append(r.clone()).unwrap();

    assert_eq!(j.entries().unwrap()[0].record, r);
}

#[test]
fn a_journal_from_another_format_version_is_refused() {
    let dir = TempDir::new().unwrap();
    drop(open(dir.path()));
    raw_db(dir.path())
        .execute_batch("PRAGMA user_version = 2")
        .unwrap();

    assert!(matches!(
        Journal::open(&db(dir.path()), key()),
        Err(JournalError::UnsupportedVersion(2))
    ));
    assert!(matches!(
        Journal::open_read_only(&db(dir.path())),
        Err(JournalError::UnsupportedVersion(2))
    ));
}

#[test]
fn read_only_open_never_creates_a_missing_journal() {
    let dir = TempDir::new().unwrap();

    assert!(Journal::open_read_only(&db(dir.path())).is_err());
    assert!(!db(dir.path()).exists());
}
