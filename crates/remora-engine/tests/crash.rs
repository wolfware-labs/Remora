//! A rewind cut short (a crash, a hard kill, a power loss) is rolled back when the project
//! next opens. The tests leave the project as such a rewind would: the intent record
//! written and the tree partly changed.
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;
use std::path::PathBuf;

use common::{Fixture, agent, tree};
use remora_engine::{
    EngineError, PointKind, REWIND_FILE, ROLLED_BACK_LABEL, ROLLED_BACK_NOTICE, RestorePoint, project_id,
};
use remora_journal::{EntryKind, VerifyReport};

/// A project at state B with a restore point at state A (the target) and one at B (what
/// the safety snapshot of a rewind from B would hold).
fn two_states(fx: &Fixture) -> (RestorePoint, RestorePoint) {
    fx.write("a.txt", "a1");
    fx.write("b.txt", "b1");
    fx.write("dir/c.txt", "c1");
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("first"), 1_000).unwrap();
    fx.write("a.txt", "a2");
    fs::remove_file(fx.root().join("b.txt")).unwrap();
    fx.write("dir/c.txt", "c2");
    fx.write("new.txt", "new");
    p.end_turn(&agent(), 2_000).unwrap();
    p.begin_turn(&agent(), Some("second"), 3_000).unwrap();
    p.end_turn(&agent(), 4_000).unwrap();
    let points = p.restore_points().unwrap();
    let starts: Vec<_> = points
        .into_iter()
        .filter(|pt| pt.kind == PointKind::TurnStart)
        .collect();
    // Newest first.
    (starts[1].clone(), starts[0].clone())
}

fn project_dir(fx: &Fixture) -> PathBuf {
    let root = fx.root().canonicalize().unwrap();
    fx.data.path().join("projects").join(project_id(&root))
}

fn intent_path(fx: &Fixture) -> PathBuf {
    project_dir(fx).join(REWIND_FILE)
}

fn write_intent(fx: &Fixture, safety: &RestorePoint, target: &RestorePoint) {
    let intent = serde_json::json!({
        "safety": safety.snapshot.to_string(),
        "target": target.snapshot.to_string(),
        "started_at_ms": 5_000,
    });
    fs::write(intent_path(fx), intent.to_string()).unwrap();
}

/// The changes a rewind from B to A makes, in order; a crash stops after some of them.
fn rewind_steps(fx: &Fixture) -> Vec<Box<dyn Fn() + '_>> {
    vec![
        Box::new(|| fx.write("a.txt", "a1")),
        Box::new(|| fx.write("b.txt", "b1")),
        Box::new(|| fx.write("dir/c.txt", "c1")),
        Box::new(|| fs::remove_file(fx.root().join("new.txt")).unwrap()),
    ]
}

#[test]
fn a_rewind_cut_short_anywhere_is_rolled_back_when_the_project_opens() {
    let steps = rewind_steps(&Fixture::new()).len();
    for done in 0..=steps {
        let fx = Fixture::new();
        let (target, safety) = two_states(&fx);
        let before = tree(fx.root());
        write_intent(&fx, &safety, &target);
        for step in rewind_steps(&fx).iter().take(done) {
            step();
        }

        let mut p = fx.open();

        assert_eq!(tree(fx.root()), before, "cut after {done} changes");
        assert!(!intent_path(&fx).exists(), "the record is removed");
        let last = p.journal().entries().unwrap().pop().unwrap().record;
        assert_eq!(last.kind, EntryKind::Rewind);
        assert_eq!(last.interrupted, Some(true));
        assert_eq!(last.snapshot_id, Some(safety.snapshot.to_string()));
        assert_eq!(last.rewind_target, Some(target.snapshot.to_string()));
        assert_eq!(last.started_at_ms, 5_000);
        assert!(matches!(
            p.journal().verify().unwrap(),
            VerifyReport::Intact { .. }
        ));
        assert_eq!(p.take_notice().as_deref(), Some(ROLLED_BACK_NOTICE));
        assert_eq!(p.take_notice(), None, "told once");
    }
}

#[test]
fn a_rolled_back_rewind_is_listed_and_the_project_is_usable_afterwards() {
    let fx = Fixture::new();
    let (target, safety) = two_states(&fx);
    write_intent(&fx, &safety, &target);
    fx.write("a.txt", "a1");

    let mut p = fx.open();
    let newest = p.restore_points().unwrap().remove(0);
    assert_eq!(newest.kind, PointKind::BeforeRewind);
    assert_eq!(newest.label.as_deref(), Some(ROLLED_BACK_LABEL));

    // The same rewind, this time uninterrupted.
    let target = p
        .restore_points()
        .unwrap()
        .into_iter()
        .find(|pt| pt.snapshot == target.snapshot)
        .unwrap();
    p.rewind(&target, 6_000, false).unwrap();
    assert_eq!(fx.read("a.txt"), "a1");
    assert_eq!(fx.read("b.txt"), "b1");
    assert!(!fx.exists("new.txt"));
    assert!(!intent_path(&fx).exists());
}

#[test]
fn a_finished_rewind_leaves_no_intent_record() {
    let fx = Fixture::new();
    let (target, _) = two_states(&fx);
    let mut p = fx.open();
    p.rewind(&target, 6_000, false).unwrap();
    assert!(!intent_path(&fx).exists());
    drop(p);
    assert_eq!(fx.open().take_notice(), None);
}

#[test]
fn a_rollback_that_fails_keeps_the_record_and_names_the_safety_snapshot() {
    use sha2::{Digest, Sha256};
    let fx = Fixture::new();
    let (target, safety) = two_states(&fx);
    write_intent(&fx, &safety, &target);
    fx.write("a.txt", "a1");
    // Lose the blob the rollback needs ("a2" is only in the safety snapshot).
    let hash = hex::encode(Sha256::digest(b"a2"));
    let blob = project_dir(&fx)
        .join("store/blobs")
        .join(&hash[..2])
        .join(&hash[2..]);
    let saved = fs::read(&blob).unwrap();
    fs::remove_file(&blob).unwrap();

    for _ in 0..2 {
        let result = fx.engine().project(fx.root());
        let Err(EngineError::RewindIncomplete { safety_snapshot, .. }) = result else {
            panic!(
                "expected RewindIncomplete, got {:?}",
                result.map(|p| p.id().to_owned())
            );
        };
        assert_eq!(safety_snapshot, safety.snapshot.to_string());
        assert!(intent_path(&fx).exists(), "the record stays");
    }

    // Once the store is whole again, the next open finishes the rollback.
    fs::write(&blob, saved).unwrap();
    let mut p = fx.open();
    assert_eq!(fx.read("a.txt"), "a2");
    assert!(!intent_path(&fx).exists());
    assert!(p.take_notice().is_some());
}

#[test]
fn an_unreadable_intent_record_fails_the_open() {
    let fx = Fixture::new();
    two_states(&fx);
    fs::write(intent_path(&fx), "{ not json").unwrap();
    let result = fx.engine().project(fx.root());
    assert!(
        matches!(result, Err(EngineError::CorruptState { .. })),
        "{:?}",
        result.map(|p| p.id().to_owned())
    );
}
