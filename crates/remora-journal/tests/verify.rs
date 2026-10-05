#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::path::Path;

use proptest::prelude::*;
use remora_journal::{
    BreakReason, CommandRecord, EntryKind, EntryRecord, FileChange, FileChangeKind, Journal, VerifyReport,
    compute_hash,
};
use rusqlite::{Connection, params};
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

/// Journal with entries "one", "two", "three" (seq 1, 2, 3).
fn journal_with_three(dir: &Path) -> Journal {
    let mut j = Journal::open(dir).unwrap();
    for p in ["one", "two", "three"] {
        j.append(record(p)).unwrap();
    }
    j
}

/// A second connection that bypasses the Journal API, like an attacker editing the file.
fn raw_db(dir: &Path) -> Connection {
    Connection::open(dir.join("journal.db")).unwrap()
}

#[test]
fn an_untouched_journal_verifies() {
    let dir = TempDir::new().unwrap();
    let j = journal_with_three(dir.path());
    assert_eq!(j.verify().unwrap(), VerifyReport::Intact { entries: 3 });
}

#[test]
fn an_empty_journal_verifies() {
    let dir = TempDir::new().unwrap();
    let j = Journal::open(dir.path()).unwrap();
    assert_eq!(j.verify().unwrap(), VerifyReport::Intact { entries: 0 });
}

#[test]
fn editing_an_entry_is_a_hash_mismatch() {
    let dir = TempDir::new().unwrap();
    let j = journal_with_three(dir.path());
    raw_db(dir.path())
        .execute("UPDATE entries SET prompt = 'innocent' WHERE seq = 2", [])
        .unwrap();

    assert_eq!(
        j.verify().unwrap(),
        VerifyReport::Broken {
            seq: 2,
            reason: BreakReason::HashMismatch
        }
    );
}

#[test]
fn deleting_an_entry_breaks_the_chain() {
    let dir = TempDir::new().unwrap();
    let j = journal_with_three(dir.path());
    raw_db(dir.path())
        .execute("DELETE FROM entries WHERE seq = 2", [])
        .unwrap();

    assert_eq!(
        j.verify().unwrap(),
        VerifyReport::Broken {
            seq: 3,
            reason: BreakReason::ChainBroken
        }
    );
}

#[test]
fn a_forged_hash_without_the_key_is_a_bad_signature() {
    let dir = TempDir::new().unwrap();
    let j = journal_with_three(dir.path());
    let entries = j.entries().unwrap();
    let forged_hash = compute_hash(&entries[2].prev_hash, &record("innocent"));
    raw_db(dir.path())
        .execute(
            "UPDATE entries SET prompt = 'innocent', hash = ?1 WHERE seq = 3",
            params![forged_hash],
        )
        .unwrap();

    assert_eq!(
        j.verify().unwrap(),
        VerifyReport::Broken {
            seq: 3,
            reason: BreakReason::BadSignature
        }
    );
}

#[test]
fn garbage_in_a_column_is_unparseable() {
    let dir = TempDir::new().unwrap();
    let j = journal_with_three(dir.path());
    raw_db(dir.path())
        .execute("UPDATE entries SET kind = 'bogus' WHERE seq = 1", [])
        .unwrap();

    assert_eq!(
        j.verify().unwrap(),
        VerifyReport::Broken {
            seq: 1,
            reason: BreakReason::Unparseable
        }
    );
}

// --- Review Focus ---

#[test]
fn a_reopened_journal_still_verifies() {
    let dir = TempDir::new().unwrap();
    drop(journal_with_three(dir.path()));

    let mut j = Journal::open(dir.path()).unwrap();
    j.append(record("after restart")).unwrap();

    assert_eq!(j.verify().unwrap(), VerifyReport::Intact { entries: 4 });
}

#[test]
fn unicode_and_multiline_prompts_verify() {
    let dir = TempDir::new().unwrap();
    let mut j = Journal::open(dir.path()).unwrap();
    j.append(record("borrá el build 🦀\nline two\t\"quoted\"\0nul"))
        .unwrap();
    assert_eq!(j.verify().unwrap(), VerifyReport::Intact { entries: 1 });
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn tampering_with_any_entry_is_detected_at_that_entry(
        prompts in prop::collection::vec(".{0,40}", 1..8),
        pick in any::<prop::sample::Index>(),
    ) {
        let dir = TempDir::new().unwrap();
        let mut j = Journal::open(dir.path()).unwrap();
        for p in &prompts {
            j.append(record(p)).unwrap();
        }
        prop_assert_eq!(j.verify().unwrap(), VerifyReport::Intact { entries: prompts.len() });

        let seq = pick.index(prompts.len()) as i64 + 1;
        raw_db(dir.path())
            .execute("UPDATE entries SET prompt = prompt || '!' WHERE seq = ?1", params![seq])
            .unwrap();
        prop_assert_eq!(j.verify().unwrap(), VerifyReport::Broken { seq, reason: BreakReason::HashMismatch });
    }
}
