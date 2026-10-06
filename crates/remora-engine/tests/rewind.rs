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
    point.position += 7;

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
