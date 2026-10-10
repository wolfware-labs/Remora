#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use common::{db, key, open, raw_db, record};
use remora_journal::{
    EntryKind, FORMAT_VERSION, GENESIS_HASH, Journal, JournalError, VerifyReport, compute_hash,
};
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
    turn.commands[0].at_ms = Some(1_500);
    let mut rewind = record("rewind");
    rewind.kind = EntryKind::Rewind;
    rewind.rewind_target = Some("ef".repeat(32));
    rewind.incomplete = Some(true);
    let mut late = record("late start");
    late.late_start = Some(true);

    j.append(turn.clone()).unwrap();
    j.append(rewind.clone()).unwrap();
    j.append(late.clone()).unwrap();

    let entries = j.entries().unwrap();
    assert_eq!(entries[0].record, turn);
    assert_eq!(entries[1].record, rewind);
    assert_eq!(entries[2].record, late);
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
        .execute_batch("PRAGMA user_version = 3")
        .unwrap();

    assert!(matches!(
        Journal::open(&db(dir.path()), key()),
        Err(JournalError::UnsupportedVersion(3))
    ));
    assert!(matches!(
        Journal::open_read_only(&db(dir.path())),
        Err(JournalError::UnsupportedVersion(3))
    ));
}

/// A format 1 journal as Remora 0.1 wrote it: the same table without `late_start`.
fn v1_journal(dir: &std::path::Path) -> Vec<remora_journal::Entry> {
    let mut j = open(dir);
    j.append(record("first")).unwrap();
    let mut second = record("second");
    second.interrupted = Some(true);
    j.append(second).unwrap();
    let entries = j.entries().unwrap();
    drop(j);
    raw_db(dir)
        .execute_batch("ALTER TABLE entries DROP COLUMN late_start; PRAGMA user_version = 1;")
        .unwrap();
    entries
}

fn version(dir: &std::path::Path) -> i64 {
    raw_db(dir)
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_format_1_journal_reads_as_it_is_when_opened_read_only() {
    let dir = TempDir::new().unwrap();
    let written = v1_journal(dir.path());

    let j = Journal::open_read_only(&db(dir.path())).unwrap();

    assert_eq!(j.entries().unwrap(), written);
    let report = j.verify_with(&key().verifying_key()).unwrap();
    assert_eq!(report, VerifyReport::Intact { entries: 2 });
    assert_eq!(version(dir.path()), 1, "a read-only open changes nothing");
}

#[test]
fn a_format_1_journal_is_migrated_when_opened_for_writing_and_keeps_its_hashes() {
    let dir = TempDir::new().unwrap();
    let written = v1_journal(dir.path());

    let mut j = open(dir.path());

    assert_eq!(version(dir.path()), FORMAT_VERSION);
    assert_eq!(
        j.entries().unwrap(),
        written,
        "same records, hashes and signatures"
    );
    let mut late = record("third");
    late.late_start = Some(true);
    j.append(late.clone()).unwrap();
    let entries = j.entries().unwrap();
    assert_eq!(entries[2].record, late);
    assert_eq!(entries[2].prev_hash, written[1].hash);
    assert_eq!(j.verify().unwrap(), VerifyReport::Intact { entries: 3 });
    let read_only = Journal::open_read_only(&db(dir.path())).unwrap();
    assert_eq!(read_only.entries().unwrap(), entries);
}

#[test]
fn a_migration_that_fails_leaves_the_format_1_journal_untouched() {
    let dir = TempDir::new().unwrap();
    v1_journal(dir.path());
    // A column the migration cannot add (it already exists under that name).
    raw_db(dir.path())
        .execute_batch("ALTER TABLE entries ADD COLUMN late_start TEXT; PRAGMA user_version = 1;")
        .unwrap();

    let result = Journal::open(&db(dir.path()), key());

    assert!(
        matches!(result, Err(JournalError::Sqlite(_))),
        "{:?}",
        result.err()
    );
    assert_eq!(version(dir.path()), 1);
}

#[test]
fn read_only_open_never_creates_a_missing_journal() {
    let dir = TempDir::new().unwrap();

    assert!(Journal::open_read_only(&db(dir.path())).is_err());
    assert!(!db(dir.path()).exists());
}

#[test]
fn a_journal_without_a_version_stamp_is_refused() {
    // A table with user_version 0: a pre-versioning journal, or one damaged mid-creation.
    let dir = TempDir::new().unwrap();
    drop(open(dir.path()));
    raw_db(dir.path())
        .execute_batch("PRAGMA user_version = 0")
        .unwrap();

    assert!(matches!(
        Journal::open(&db(dir.path()), key()),
        Err(JournalError::UnsupportedVersion(0))
    ));
}

#[test]
fn concurrent_first_opens_of_a_new_journal_all_succeed() {
    for _ in 0..20 {
        let dir = TempDir::new().unwrap();
        let results: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| s.spawn(|| Journal::open(&db(dir.path()), key()).map(drop)))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for r in results {
            assert!(r.is_ok(), "{r:?}");
        }
    }
}
