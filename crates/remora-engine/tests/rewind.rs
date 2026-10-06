#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use common::{Fixture, agent, tree};
use remora_engine::{EngineError, PointKind, REWIND_SESSION};
use remora_journal::{EntryKind, VerifyReport};

#[test]
fn rewinding_to_the_checkpoint_before_rm_rf_brings_everything_back() {
    let fx = Fixture::new();
    fx.write("src/main.rs", "fn main() {}");
    fx.write("README.md", "hello");
    let before = tree(fx.root());
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("clean the repo"), 1_000).unwrap();
    p.before_command(&agent(), "rm -rf src README.md", 1_100).unwrap();
    std::fs::remove_dir_all(fx.root().join("src")).unwrap();
    std::fs::remove_file(fx.root().join("README.md")).unwrap();
    p.end_turn(&agent(), 2_000).unwrap();

    let point = p
        .restore_points()
        .unwrap()
        .into_iter()
        .find(|pt| pt.kind == PointKind::BeforeCommand)
        .unwrap();
    let report = p.rewind(&point, 3_000, false).unwrap();

    assert_eq!(tree(fx.root()), before);
    assert!(report.restore.failed.is_empty());
    assert_eq!(report.commands_after.len(), 1);
    assert_eq!(report.commands_after[0].command, "rm -rf src README.md");
}

#[test]
fn rewinding_to_a_turn_start_undoes_the_whole_turn_and_lists_its_commands() {
    let fx = Fixture::new();
    fx.write("a.txt", "v1");
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("edit a"), 1_000).unwrap();
    p.before_command(&agent(), "sed -i s/v1/v2/ a.txt", 1_100)
        .unwrap();
    fx.write("a.txt", "v2");
    p.before_command(&agent(), "git push", 1_200).unwrap();
    p.end_turn(&agent(), 2_000).unwrap();

    let start = p
        .restore_points()
        .unwrap()
        .into_iter()
        .find(|pt| pt.kind == PointKind::TurnStart)
        .unwrap();
    let report = p.rewind(&start, 3_000, false).unwrap();

    assert_eq!(fx.read("a.txt"), "v1");
    let commands: Vec<_> = report.commands_after.iter().map(|c| c.command.as_str()).collect();
    assert_eq!(commands, ["sed -i s/v1/v2/ a.txt", "git push"]);
}

#[test]
fn a_rewind_is_journaled_and_can_itself_be_undone() {
    let fx = Fixture::new();
    fx.write("a.txt", "v1");
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("edit"), 1_000).unwrap();
    fx.write("a.txt", "v2");
    p.end_turn(&agent(), 2_000).unwrap();
    let start = p
        .restore_points()
        .unwrap()
        .into_iter()
        .find(|pt| pt.kind == PointKind::TurnStart)
        .unwrap();

    let report = p.rewind(&start, 3_000, false).unwrap();
    assert_eq!(fx.read("a.txt"), "v1");

    let entry = &report.entry.record;
    assert_eq!(entry.kind, EntryKind::Rewind);
    assert_eq!(entry.session_id, REWIND_SESSION);
    assert_eq!(entry.rewind_target.as_deref(), Some(start.snapshot.as_str()));
    assert_eq!(
        entry.snapshot_id.as_deref(),
        Some(report.safety_snapshot.as_str())
    );
    assert_eq!(p.journal().verify().unwrap(), VerifyReport::Intact { entries: 2 });

    let undo = p
        .restore_points()
        .unwrap()
        .into_iter()
        .find(|pt| pt.kind == PointKind::BeforeRewind)
        .unwrap();
    p.rewind(&undo, 4_000, false).unwrap();
    assert_eq!(fx.read("a.txt"), "v2");
}

#[test]
fn rewind_is_refused_while_a_turn_is_open_unless_forced() {
    let fx = Fixture::new();
    fx.write("a.txt", "v1");
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("edit"), 1_000).unwrap();
    fx.write("a.txt", "v2");
    let start = p.restore_points().unwrap()[0].clone();
    assert!(start.open);

    let refused = p.rewind(&start, 2_000, false);
    assert!(
        matches!(refused, Err(EngineError::TurnInProgress { ref session }) if session == "session-1"),
        "{refused:?}"
    );
    assert_eq!(fx.read("a.txt"), "v2");

    p.rewind(&start, 2_000, true).unwrap();
    assert_eq!(fx.read("a.txt"), "v1");
}

#[test]
fn a_point_that_does_not_match_the_timeline_is_unknown() {
    let fx = Fixture::new();
    let mut p = fx.open();
    p.begin_turn(&agent(), None, 1_000).unwrap();
    p.end_turn(&agent(), 2_000).unwrap();
    let mut point = p.restore_points().unwrap()[0].clone();
    point.label = Some("never happened".into());

    assert!(matches!(
        p.rewind(&point, 3_000, false),
        Err(EngineError::UnknownRestorePoint(_))
    ));
}

#[test]
fn restore_points_are_listed_newest_first_with_labels() {
    let fx = Fixture::new();
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("first prompt"), 1_000).unwrap();
    p.before_command(&agent(), "ls", 1_100).unwrap();
    p.end_turn(&agent(), 2_000).unwrap();
    p.begin_turn(&agent(), Some("second prompt"), 3_000).unwrap();

    let labels: Vec<_> = p
        .restore_points()
        .unwrap()
        .into_iter()
        .map(|pt| (pt.kind, pt.label.unwrap_or_default(), pt.open))
        .collect();

    assert_eq!(
        labels,
        [
            (PointKind::TurnStart, "second prompt".to_string(), true),
            (PointKind::BeforeCommand, "ls".to_string(), false),
            (PointKind::TurnStart, "first prompt".to_string(), false),
        ]
    );
}

// --- Review Focus ---

#[test]
fn the_agent_deleting_the_whole_project_is_recorded_and_undone() {
    let fx = Fixture::new();
    fx.write("src/lib.rs", "pub fn x() {}");
    let before = tree(fx.root());
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("start over"), 1_000).unwrap();
    std::fs::remove_dir_all(fx.root()).unwrap(); // the agent ran `rm -rf .` from the parent

    let entry = p.end_turn(&agent(), 2_000).unwrap().unwrap();
    assert!(
        entry
            .record
            .changes
            .iter()
            .all(|c| c.kind == remora_journal::FileChangeKind::Deleted)
    );

    let start = p
        .restore_points()
        .unwrap()
        .into_iter()
        .find(|pt| pt.kind == PointKind::TurnStart)
        .unwrap();
    std::fs::remove_dir_all(fx.root()).unwrap(); // and again, right before the rewind
    p.rewind(&start, 3_000, false).unwrap();
    assert_eq!(tree(fx.root()), before);
}

#[test]
fn rewinding_twice_in_a_row_is_safe_and_both_are_journaled() {
    let fx = Fixture::new();
    fx.write("a.txt", "v1");
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("edit"), 1_000).unwrap();
    fx.write("a.txt", "v2");
    p.end_turn(&agent(), 2_000).unwrap();
    let start = p
        .restore_points()
        .unwrap()
        .into_iter()
        .find(|pt| pt.kind == PointKind::TurnStart)
        .unwrap();

    p.rewind(&start, 3_000, false).unwrap();
    let start_again = p
        .restore_points()
        .unwrap()
        .into_iter()
        .find(|pt| pt.kind == PointKind::TurnStart)
        .unwrap();
    let second = p.rewind(&start_again, 4_000, false).unwrap();

    assert_eq!(fx.read("a.txt"), "v1");
    assert_eq!((second.restore.written, second.restore.deleted), (0, 0));
    assert_eq!(p.journal().verify().unwrap(), VerifyReport::Intact { entries: 3 });
}

#[test]
fn a_point_listed_while_its_turn_was_open_still_works_after_the_turn_ends() {
    let fx = Fixture::new();
    fx.write("a.txt", "v1");
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("edit"), 1_000).unwrap();
    let listed_while_open = p.restore_points().unwrap()[0].clone();
    fx.write("a.txt", "v2");
    p.end_turn(&agent(), 2_000).unwrap();

    p.rewind(&listed_while_open, 3_000, false).unwrap();

    assert_eq!(fx.read("a.txt"), "v1");
}

#[test]
fn commands_after_include_other_sessions_that_ran_later() {
    let fx = Fixture::new();
    let mut p = fx.open();
    let a = remora_engine::SessionRef::new("session-a", "claude-code");
    let b = remora_engine::SessionRef::new("session-b", "codex");
    p.begin_turn(&a, Some("long task"), 1_000).unwrap(); // still open below
    p.begin_turn(&b, Some("ship it"), 2_000).unwrap();
    p.before_command(&b, "git push", 2_100).unwrap();
    p.end_turn(&b, 3_000).unwrap();

    let a_start = p
        .restore_points()
        .unwrap()
        .into_iter()
        .find(|pt| pt.kind == PointKind::TurnStart && pt.session_id == "session-a")
        .unwrap();
    let report = p.rewind(&a_start, 4_000, true).unwrap();

    let commands: Vec<_> = report.commands_after.iter().map(|c| c.command.as_str()).collect();
    assert_eq!(commands, ["git push"], "session B pushed after session A started");
}

#[test]
fn a_rewind_that_cannot_finish_is_journaled_and_can_be_undone() {
    use sha2::{Digest, Sha256};
    let fx = Fixture::new();
    fx.write("a.txt", "original");
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("edit"), 1_000).unwrap();
    fx.write("a.txt", "edited");
    p.end_turn(&agent(), 2_000).unwrap();
    let start = p
        .restore_points()
        .unwrap()
        .into_iter()
        .find(|pt| pt.kind == PointKind::TurnStart)
        .unwrap();
    // Lose the blob the restore needs.
    let hash = hex::encode(Sha256::digest(b"original"));
    let blob = fx
        .data
        .path()
        .join("projects")
        .join(p.id())
        .join("store/blobs")
        .join(&hash[..2])
        .join(&hash[2..]);
    std::fs::remove_file(blob).unwrap();

    let result = p.rewind(&start, 3_000, false);

    let Err(EngineError::RewindIncomplete { safety_snapshot, .. }) = result else {
        panic!("expected RewindIncomplete, got {result:?}");
    };
    assert_eq!(fx.read("a.txt"), "edited", "nothing was changed");
    let last = p.journal().entries().unwrap().pop().unwrap().record;
    assert_eq!(last.kind, EntryKind::Rewind);
    assert_eq!(last.incomplete, Some(true));
    assert_eq!(last.snapshot_id.as_deref(), Some(safety_snapshot.as_str()));
    let undo = p.restore_points().unwrap();
    assert!(
        undo.iter()
            .any(|pt| pt.kind == PointKind::BeforeRewind && pt.snapshot.as_str() == safety_snapshot)
    );
}

#[cfg(unix)]
#[test]
fn a_partly_failed_rewind_is_journaled_as_incomplete_with_what_really_changed() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write("locked/a.txt", "v1");
    fx.write("free.txt", "v1");
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("edit"), 1_000).unwrap();
    fx.write("locked/a.txt", "v2");
    fx.write("free.txt", "v2");
    p.end_turn(&agent(), 2_000).unwrap();
    let start = p
        .restore_points()
        .unwrap()
        .into_iter()
        .find(|pt| pt.kind == PointKind::TurnStart)
        .unwrap();
    let locked = fx.root().join("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(locked.join("probe"), "").is_ok() {
        return; // running as root: permissions are not enforced
    }

    let report = p.rewind(&start, 3_000, false).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(
        report
            .restore
            .failed
            .iter()
            .any(|(path, _)| path == "locked/a.txt")
    );
    assert_eq!(fx.read("free.txt"), "v1");
    let entry = &report.entry.record;
    assert_eq!(entry.incomplete, Some(true));
    let changed: Vec<_> = entry.changes.iter().map(|c| c.path.as_str()).collect();
    // free.txt was restored and the locked dir got its snapshotted permissions back; the file
    // inside it could not be written, so it must not be claimed as changed.
    assert_eq!(
        changed,
        ["free.txt", "locked"],
        "only what really changed is journaled"
    );
}

#[test]
fn a_point_still_works_after_earlier_events_shift_its_position() {
    let fx = Fixture::new();
    fx.write("a.txt", "v1");
    let mut p = fx.open();
    let a = remora_engine::SessionRef::new("session-a", "claude-code");
    let b = remora_engine::SessionRef::new("session-b", "codex");
    p.begin_turn(&a, Some("later task"), 2_000).unwrap();
    let listed = p.restore_points().unwrap()[0].clone();
    fx.write("a.txt", "v2");
    // A turn that started earlier is journaled afterwards: it sorts before session A's
    // point, so that point's position moves.
    p.begin_turn(&b, Some("earlier task"), 1_000).unwrap();
    p.end_turn(&b, 1_500).unwrap();
    assert_ne!(
        p.restore_points()
            .unwrap()
            .iter()
            .find(|pt| pt.session_id == "session-a")
            .unwrap()
            .position,
        listed.position
    );

    p.rewind(&listed, 3_000, true).unwrap();

    assert_eq!(fx.read("a.txt"), "v1");
}
