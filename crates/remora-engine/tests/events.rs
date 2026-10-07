#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::thread;

use common::{Fixture, agent};
use remora_engine::SessionRef;
use remora_journal::{EntryKind, FileChange, FileChangeKind, VerifyReport};

#[test]
fn a_turn_is_journaled_with_its_commands_and_changes() {
    let fx = Fixture::new();
    fx.write("old.txt", "old");
    let mut p = fx.open();

    let pre = p.begin_turn(&agent(), Some("tidy up"), 1_000).unwrap();
    let checkpoint = p.before_command(&agent(), "rm old.txt", 1_100).unwrap();
    std::fs::remove_file(fx.root().join("old.txt")).unwrap();
    p.after_command(&agent(), "rm old.txt", Some(0), 1_200).unwrap();
    fx.write("new.txt", "new");
    let entry = p.end_turn(&agent(), 2_000).unwrap().unwrap();

    let r = &entry.record;
    assert_eq!(r.kind, EntryKind::Turn);
    assert_eq!(
        (r.agent.as_str(), r.prompt.as_deref()),
        ("claude-code", Some("tidy up"))
    );
    assert_eq!((r.started_at_ms, r.ended_at_ms), (1_000, 2_000));
    assert_eq!(r.snapshot_id.as_deref(), Some(pre.as_str()));
    assert_eq!(r.commands.len(), 1);
    assert_eq!(r.commands[0].exit_code, Some(0));
    assert_eq!(r.commands[0].snapshot_id.as_deref(), Some(checkpoint.as_str()));
    assert_eq!(
        r.changes,
        vec![
            FileChange {
                path: "new.txt".into(),
                kind: FileChangeKind::Added
            },
            FileChange {
                path: "old.txt".into(),
                kind: FileChangeKind::Deleted
            },
        ]
    );
    assert_eq!(r.interrupted, None);
    assert_eq!(p.journal().verify().unwrap(), VerifyReport::Intact { entries: 1 });
}

#[test]
fn a_new_turn_closes_an_unfinished_one_as_interrupted() {
    let fx = Fixture::new();
    let mut p = fx.open();

    p.begin_turn(&agent(), Some("first"), 1_000).unwrap();
    p.begin_turn(&agent(), Some("second"), 2_000).unwrap();
    p.end_turn(&agent(), 3_000).unwrap();

    let entries = p.journal().entries().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].record.prompt.as_deref(), Some("first"));
    assert_eq!(entries[0].record.interrupted, Some(true));
    assert_eq!(entries[0].record.ended_at_ms, 2_000);
    assert_eq!(entries[1].record.interrupted, None);
}

#[test]
fn a_command_without_a_turn_opens_an_implicit_one() {
    let fx = Fixture::new();
    let mut p = fx.open();

    p.before_command(&agent(), "make", 1_000).unwrap();
    let entry = p.end_turn(&agent(), 2_000).unwrap().unwrap();

    assert_eq!(entry.record.prompt, None);
    assert_eq!(entry.record.commands[0].command, "make");
    assert_eq!(entry.record.commands[0].exit_code, None);
}

#[test]
fn ending_a_turn_that_never_started_does_nothing() {
    let fx = Fixture::new();
    let mut p = fx.open();

    assert!(p.end_turn(&agent(), 1_000).unwrap().is_none());
    p.after_command(&agent(), "ls", Some(0), 1_100).unwrap();
    assert!(p.journal().entries().unwrap().is_empty());
}

#[test]
fn an_open_turn_survives_a_restart() {
    let fx = Fixture::new();
    {
        let mut p = fx.open();
        p.begin_turn(&agent(), Some("long task"), 1_000).unwrap();
        p.before_command(&agent(), "cargo build", 1_100).unwrap();
    } // the daemon "restarts" here

    let mut p = fx.open();
    p.after_command(&agent(), "cargo build", Some(101), 1_900)
        .unwrap();
    let entry = p.end_turn(&agent(), 2_000).unwrap().unwrap();

    assert_eq!(entry.record.prompt.as_deref(), Some("long task"));
    assert_eq!(entry.record.commands[0].exit_code, Some(101));
}

#[test]
fn concurrent_sessions_on_one_project_are_serialized() {
    let fx = Fixture::new();
    drop(fx.open());

    thread::scope(|s| {
        for n in 0..4 {
            let fx = &fx;
            s.spawn(move || {
                let session = SessionRef::new(format!("session-{n}"), "claude-code");
                let mut p = fx.open();
                for turn in 0..3 {
                    let t = (n * 100 + turn) as i64;
                    p.begin_turn(&session, Some("work"), t).unwrap();
                    fx.write(&format!("s{n}-t{turn}.txt"), "x");
                    p.end_turn(&session, t + 1).unwrap();
                }
            });
        }
    });

    let p = fx.open();
    assert_eq!(
        p.journal().verify().unwrap(),
        VerifyReport::Intact { entries: 12 }
    );
    let sessions = fx.data.path().join("projects").join(p.id()).join("sessions");
    assert_eq!(
        std::fs::read_dir(sessions).unwrap().count(),
        0,
        "every turn was closed"
    );
}

// --- Review Focus ---

#[test]
fn a_corrupt_session_file_is_set_aside_instead_of_blocking_the_session() {
    let fx = Fixture::new();
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("first"), 1_000).unwrap();
    let sessions = fx.data.path().join("projects").join(p.id()).join("sessions");
    std::fs::write(sessions.join("session-1.json"), b"{ not json").unwrap();

    p.begin_turn(&agent(), Some("second"), 2_000).unwrap();
    let entry = p.end_turn(&agent(), 3_000).unwrap().unwrap();

    assert_eq!(entry.record.prompt.as_deref(), Some("second"));
    assert!(
        sessions.join("session-1.json.corrupt").exists(),
        "the damaged state is kept as evidence"
    );
}

#[test]
fn a_session_file_with_a_bad_snapshot_id_does_not_break_the_project() {
    let fx = Fixture::new();
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("first"), 1_000).unwrap();
    p.end_turn(&agent(), 1_500).unwrap();
    let sessions = fx.data.path().join("projects").join(p.id()).join("sessions");
    let other = remora_engine::SessionRef::new("other", "codex");
    p.begin_turn(&other, None, 2_000).unwrap();
    let path = sessions.join("other.json");
    let damaged = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\"pre_snapshot\":\"", "\"pre_snapshot\":\"zz");
    std::fs::write(&path, damaged).unwrap();

    let points = p.restore_points().unwrap();

    assert_eq!(points.len(), 1, "only the healthy turn's point remains");
    assert!(p.status().is_ok());
    assert!(sessions.join("other.json.corrupt").exists());
    p.rewind(&points[0], 3_000, false).unwrap();
}

#[test]
fn a_second_damaged_session_file_keeps_the_first_as_evidence() {
    let fx = Fixture::new();
    let mut p = fx.open();
    let sessions = fx.data.path().join("projects").join(p.id()).join("sessions");
    for (i, at) in [(1, 1_000), (2, 2_000)] {
        p.begin_turn(&agent(), None, at).unwrap();
        std::fs::write(sessions.join("session-1.json"), format!("damaged {i}")).unwrap();
        p.status().unwrap(); // reading the state sets the damaged file aside
    }

    let first = std::fs::read_to_string(sessions.join("session-1.json.corrupt")).unwrap();
    let second = std::fs::read_to_string(sessions.join("session-1.json.corrupt.1")).unwrap();
    assert_eq!((first.as_str(), second.as_str()), ("damaged 1", "damaged 2"));
}
