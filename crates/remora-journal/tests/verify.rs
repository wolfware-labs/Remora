#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use common::{db, journal_with_three, key, raw_db, record};
use ed25519_dalek::Signer;
use proptest::prelude::*;
use remora_journal::{BreakReason, Journal, SigningKey, VerifyReport, compute_hash};
use rusqlite::params;
use tempfile::TempDir;

#[test]
fn an_untouched_journal_verifies() {
    let dir = TempDir::new().unwrap();
    let j = journal_with_three(dir.path());
    assert_eq!(j.verify().unwrap(), VerifyReport::Intact { entries: 3 });
}

#[test]
fn an_empty_journal_verifies() {
    let dir = TempDir::new().unwrap();
    let j = common::open(dir.path());
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

#[test]
fn a_column_holding_the_wrong_type_is_unparseable() {
    for (column, value) in [("started_at_ms", "'abc'"), ("prompt", "X'00'"), ("kind", "5")] {
        let dir = TempDir::new().unwrap();
        let j = journal_with_three(dir.path());
        raw_db(dir.path())
            .execute(
                &format!("UPDATE entries SET {column} = {value} WHERE seq = 2"),
                [],
            )
            .unwrap();

        assert_eq!(
            j.verify().unwrap(),
            VerifyReport::Broken {
                seq: 2,
                reason: BreakReason::Unparseable
            },
            "{column} = {value}"
        );
    }
}

#[test]
fn a_malformed_signature_is_a_bad_signature() {
    for value in ["'not hex'", "substr(signature, 1, 10)"] {
        let dir = TempDir::new().unwrap();
        let j = journal_with_three(dir.path());
        raw_db(dir.path())
            .execute(
                &format!("UPDATE entries SET signature = {value} WHERE seq = 2"),
                [],
            )
            .unwrap();

        assert_eq!(
            j.verify().unwrap(),
            VerifyReport::Broken {
                seq: 2,
                reason: BreakReason::BadSignature
            },
            "signature = {value}"
        );
    }
}

#[test]
fn a_read_only_journal_verifies_against_a_public_key() {
    let dir = TempDir::new().unwrap();
    drop(journal_with_three(dir.path()));

    let reader = Journal::open_read_only(&db(dir.path())).unwrap();

    assert_eq!(reader.entries().unwrap().len(), 3);
    assert_eq!(
        reader.verify_with(&key().verifying_key()).unwrap(),
        VerifyReport::Intact { entries: 3 }
    );
    let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
    assert_eq!(
        reader.verify_with(&other).unwrap(),
        VerifyReport::Broken {
            seq: 1,
            reason: BreakReason::BadSignature
        }
    );
}

#[test]
fn a_signature_without_the_domain_prefix_is_rejected() {
    let dir = TempDir::new().unwrap();
    let j = journal_with_three(dir.path());
    let hash = j.entries().unwrap()[1].hash.clone();
    // Sign the bare hash, as a pre-versioning format would have: same key, wrong message.
    let bare = hex::encode(key().sign(hash.as_bytes()).to_bytes());
    raw_db(dir.path())
        .execute("UPDATE entries SET signature = ?1 WHERE seq = 2", params![bare])
        .unwrap();

    assert_eq!(
        j.verify().unwrap(),
        VerifyReport::Broken {
            seq: 2,
            reason: BreakReason::BadSignature
        }
    );
}

// --- Review Focus ---

#[test]
fn a_reopened_journal_still_verifies() {
    let dir = TempDir::new().unwrap();
    drop(journal_with_three(dir.path()));

    let mut j = common::open(dir.path());
    j.append(record("after restart")).unwrap();

    assert_eq!(j.verify().unwrap(), VerifyReport::Intact { entries: 4 });
}

#[test]
fn unicode_and_multiline_prompts_verify() {
    let dir = TempDir::new().unwrap();
    let mut j = common::open(dir.path());
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
        let mut j = common::open(dir.path());
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
