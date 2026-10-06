use remora_journal::{
    CommandRecord, EntryKind, EntryRecord, FileChange, FileChangeKind, GENESIS_HASH, compute_hash,
};

fn sample() -> EntryRecord {
    EntryRecord {
        session_id: "s1".into(),
        kind: EntryKind::Turn,
        agent: "claude-code".into(),
        started_at_ms: 1_000,
        ended_at_ms: 2_000,
        snapshot_id: Some("ab".repeat(32)),
        prompt: Some("delete the build dir".into()),
        commands: vec![CommandRecord {
            command: "rm -rf build".into(),
            exit_code: Some(0),
            snapshot_id: None,
        }],
        changes: vec![FileChange {
            path: "build/out.o".into(),
            kind: FileChangeKind::Deleted,
        }],
        interrupted: None,
        rewind_target: None,
    }
}

/// Pins the canonical format. If this fails, you changed `EntryRecord`'s
/// serialization and every existing journal would stop verifying.
#[test]
fn canonical_hash_is_stable() {
    assert_eq!(
        compute_hash(GENESIS_HASH, &sample()),
        "fbb01f4e6ddd1413f93d806ff8ea3cef9d9736e01cafa6fadbc7502dfd2e3b11"
    );
}

#[test]
fn hash_depends_on_prev_hash_and_content() {
    let a = compute_hash(GENESIS_HASH, &sample());
    let b = compute_hash(&"1".repeat(64), &sample());
    let mut changed = sample();
    changed.prompt = Some("something else".into());
    let c = compute_hash(GENESIS_HASH, &changed);
    assert_ne!(a, b);
    assert_ne!(a, c);
}

/// The optional fields added after the format was pinned must change the hash when set,
/// and (see `canonical_hash_is_stable`) leave it untouched when absent.
#[test]
fn optional_fields_change_the_hash_only_when_present() {
    let base = compute_hash(GENESIS_HASH, &sample());
    let mut interrupted = sample();
    interrupted.interrupted = Some(true);
    let mut checkpointed = sample();
    checkpointed.commands[0].snapshot_id = Some("cd".repeat(32));
    let mut rewind = sample();
    rewind.rewind_target = Some("ef".repeat(32));

    for changed in [interrupted, checkpointed, rewind] {
        assert_ne!(compute_hash(GENESIS_HASH, &changed), base);
    }
}
