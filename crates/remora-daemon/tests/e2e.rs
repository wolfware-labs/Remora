#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use common::{START_MS, TestDaemon};
use remora_engine::PointKind;
use remora_ipc::{ClientError, ErrorKind, JournalCheck, Request, Response};

#[test]
fn what_rm_rf_deleted_comes_back_with_a_rewind() {
    let d = TestDaemon::start();
    d.write("src/main.rs", "fn main() {}");
    d.write("build/out.o", "object");
    let mut client = d.client();

    d.begin(&mut client, "s1", Some("clean the build"));
    d.before(&mut client, "s1", "rm -rf src build");
    std::fs::remove_dir_all(d.project.path().join("src")).unwrap();
    std::fs::remove_dir_all(d.project.path().join("build")).unwrap();
    client
        .call(Request::AfterCommand {
            root: d.root(),
            session: "s1".into(),
            agent: "claude-code".into(),
            command: "rm -rf src build".into(),
            tool_use_id: None,
            exit_code: Some(0),
        })
        .unwrap();
    assert_eq!(d.end(&mut client, "s1"), Response::TurnEnded { seq: Some(1) });

    let points = d.points(&mut client);
    let before_rm = points
        .iter()
        .find(|p| p.kind == PointKind::BeforeCommand)
        .unwrap()
        .clone();
    assert_eq!(before_rm.label.as_deref(), Some("rm -rf src build"));
    let Response::Rewound(rewound) = client
        .call(Request::Rewind {
            root: d.root(),
            point: before_rm,
            force: false,
        })
        .unwrap()
    else {
        panic!("expected a rewind report");
    };

    assert_eq!(d.read("src/main.rs"), "fn main() {}");
    assert_eq!(d.read("build/out.o"), "object");
    assert_eq!(rewound.written, 2);
    assert!(rewound.failed.is_empty());
    assert_eq!(
        client.call(Request::Verify { root: d.root() }).unwrap(),
        Response::Verified {
            journal: JournalCheck::Intact { entries: 2 }
        }
    );
}

#[test]
fn a_deleted_project_folder_is_still_recorded_and_restored() {
    let d = TestDaemon::start();
    d.write("notes.txt", "keep me");
    let mut client = d.client();
    d.begin(&mut client, "s1", None);

    std::fs::remove_dir_all(d.project.path()).unwrap();
    d.end(&mut client, "s1");
    let start = d
        .points(&mut client)
        .into_iter()
        .find(|p| p.kind == PointKind::TurnStart)
        .unwrap();
    client
        .call(Request::Rewind {
            root: d.root(),
            point: start,
            force: false,
        })
        .unwrap();

    assert_eq!(d.read("notes.txt"), "keep me");
}

#[test]
fn engine_refusals_reach_the_client_with_their_kind() {
    use remora_ipc::{ClientError, ErrorKind};
    let d = TestDaemon::start();
    let mut client = d.client();
    d.begin(&mut client, "s1", None);
    let start = d.points(&mut client).pop().unwrap();

    let refused = client.call(Request::Rewind {
        root: d.root(),
        point: start,
        force: false,
    });

    let Err(ClientError::Daemon(error)) = refused else {
        panic!("expected a refusal, got {refused:?}");
    };
    assert_eq!(error.kind, ErrorKind::TurnInProgress { session: "s1".into() });
    assert!(error.message.contains("s1"));

    let bad_session = client.call(Request::BeginTurn {
        root: d.root(),
        session: "../escape".into(),
        agent: "claude-code".into(),
        prompt: None,
        deadline_ms: None,
    });
    assert!(matches!(bad_session, Err(ClientError::Daemon(e)) if e.kind == ErrorKind::InvalidSessionId));
}

#[test]
fn a_project_containing_the_data_dir_is_refused() {
    use remora_ipc::{ClientError, ErrorKind};
    let d = TestDaemon::start();
    let mut client = d.client();

    let refused = client.call(Request::Status {
        root: d.data.path().to_path_buf(),
    });

    assert!(matches!(refused, Err(ClientError::Daemon(e)) if e.kind == ErrorKind::RootOverlapsDataDir));
}

#[cfg(unix)]
#[test]
fn a_deleted_project_reached_through_a_symlink_is_still_found() {
    let d = TestDaemon::start();
    let real = d.project.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("notes.txt"), "keep me").unwrap();
    let link = d.project.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut client = d.client();
    let session = |root: &std::path::Path| (root.to_path_buf(), "s1".to_owned(), "claude-code".to_owned());
    let (root, s, agent) = session(&link);
    client
        .call(Request::BeginTurn {
            root,
            session: s,
            agent,
            prompt: None,
            deadline_ms: None,
        })
        .unwrap();

    std::fs::remove_dir_all(&real).unwrap(); // the link now dangles
    let (root, s, agent) = session(&link);
    let ended = client.call(Request::EndTurn {
        root,
        session: s,
        agent,
    });

    assert!(
        matches!(ended, Ok(Response::TurnEnded { seq: Some(1) })),
        "{ended:?}"
    );
    let Response::RestorePoints { points, .. } = client
        .call(Request::RestorePoints {
            root: link.clone(),
            skip: 0,
            limit: None,
        })
        .unwrap()
    else {
        panic!("expected restore points");
    };
    let start = points
        .into_iter()
        .find(|p| p.kind == PointKind::TurnStart)
        .unwrap();
    client
        .call(Request::Rewind {
            root: link.clone(),
            point: start,
            force: false,
        })
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(real.join("notes.txt")).unwrap(),
        "keep me"
    );
}

#[test]
fn restore_points_stay_listable_after_very_long_commands() {
    let d = TestDaemon::start();
    let mut client = d.client();
    d.begin(&mut client, "s1", Some(&"p".repeat(200_000)));
    for i in 0..12 {
        d.before(&mut client, "s1", &format!("{i} {}", "x".repeat(100_000)));
    }

    let points = d.points(&mut client);

    assert_eq!(points.len(), 13, "every point fits in one answer");
    assert!(points.iter().all(|p| {
        p.label
            .as_ref()
            .is_none_or(|l| l.len() <= remora_engine::MAX_LABEL_BYTES + 3)
    }));
    let oldest_command = points
        .iter()
        .find(|p| {
            p.kind == PointKind::BeforeCommand && p.label.as_deref().is_some_and(|l| l.starts_with("0 "))
        })
        .unwrap()
        .clone();
    d.end(&mut client, "s1");
    client
        .call(Request::Rewind {
            root: d.root(),
            point: oldest_command,
            force: false,
        })
        .unwrap();
}

#[test]
fn restore_points_come_in_pages_newest_first() {
    let d = TestDaemon::start();
    let mut client = d.client();
    d.begin(&mut client, "s1", None);
    for i in 0..5 {
        d.before(&mut client, "s1", &format!("step {i}"));
    }
    let page = |client: &mut remora_ipc::Client, skip, limit| match client
        .call(Request::RestorePoints {
            root: d.root(),
            skip,
            limit,
        })
        .unwrap()
    {
        Response::RestorePoints { points, total } => {
            (points.into_iter().map(|p| p.label).collect::<Vec<_>>(), total)
        }
        other => panic!("unexpected {other:?}"),
    };

    let (first, total) = page(&mut client, 0, Some(2));
    let (second, _) = page(&mut client, 2, Some(2));

    assert_eq!(total, 6);
    assert_eq!(first, [Some("step 4".to_owned()), Some("step 3".to_owned())]);
    assert_eq!(second, [Some("step 2".to_owned()), Some("step 1".to_owned())]);
}

#[test]
fn a_rewind_far_back_lists_a_bounded_number_of_commands() {
    let d = TestDaemon::start();
    let mut client = d.client();
    d.begin(&mut client, "s1", None);
    let start = d.points(&mut client).pop().unwrap();
    for i in 0..(remora_ipc::MAX_COMMANDS_AFTER + 5) {
        d.before(&mut client, "s1", &format!("echo {i}"));
    }
    d.end(&mut client, "s1");
    let Response::RestorePoints { points, total } = client
        .call(Request::RestorePoints {
            root: d.root(),
            skip: 0,
            limit: Some(10_000),
        })
        .unwrap()
    else {
        panic!("expected restore points");
    };
    assert_eq!(total, remora_ipc::MAX_COMMANDS_AFTER + 6);
    assert_eq!(
        points.len(),
        remora_ipc::MAX_POINTS,
        "a large limit is capped, not refused"
    );

    let Response::Rewound(rewound) = client
        .call(Request::Rewind {
            root: d.root(),
            point: start,
            force: false,
        })
        .unwrap()
    else {
        panic!("expected a rewind report");
    };

    assert_eq!(rewound.commands_after.len(), remora_ipc::MAX_COMMANDS_AFTER);
    assert_eq!(rewound.commands_omitted, 5);
    assert_eq!(rewound.commands_after[0].command, "echo 0", "oldest first");
}

#[test]
fn a_deleted_project_folder_is_reachable_after_a_restart() {
    let d = TestDaemon::start();
    d.write("notes.txt", "keep me");
    let mut client = d.client();
    d.begin(&mut client, "s1", None);
    d.end(&mut client, "s1");
    drop(client);
    d.stop();
    std::fs::remove_dir_all(d.project.path()).unwrap();

    let restarted = remora_daemon::Daemon::start(common::config(d.data.path(), &d.clock)).unwrap();
    let mut client = remora_ipc::Client::connect(d.data.path()).unwrap();
    let start = d
        .points(&mut client)
        .into_iter()
        .find(|p| p.kind == PointKind::TurnStart)
        .unwrap();
    client
        .call(Request::Rewind {
            root: d.root(),
            point: start,
            force: false,
        })
        .unwrap();
    restarted.shutdown();
    restarted.wait();

    assert_eq!(d.read("notes.txt"), "keep me");
}

#[test]
fn a_turn_whose_snapshot_was_lost_does_not_lock_the_project() {
    let d = TestDaemon::start();
    d.write("a.txt", "v1");
    let mut client = d.client();
    let Response::Snapshot { snapshot, .. } = d.begin(&mut client, "s1", None) else {
        panic!("expected a snapshot");
    };
    d.write("a.txt", "v2");
    drop(client);
    d.stop();
    let status_root = d.project.path().canonicalize().unwrap();
    let store = d
        .data
        .path()
        .join("projects")
        .join(remora_engine::project_id(&status_root))
        .join("store");
    std::fs::remove_file(store.join("manifests").join(format!("{snapshot}.json"))).unwrap();
    d.clock.advance(std::time::Duration::from_secs(7 * 60 * 60));

    let restarted = remora_daemon::Daemon::start(common::config(d.data.path(), &d.clock)).unwrap();
    let mut client = remora_ipc::Client::connect(d.data.path()).unwrap();
    let status = d.status(&mut client);
    let begun = client.call(Request::BeginTurn {
        root: d.root(),
        session: "s2".into(),
        agent: "claude-code".into(),
        prompt: None,
        deadline_ms: None,
    });
    restarted.shutdown();
    restarted.wait();

    assert!(status.open_sessions.is_empty(), "the stale turn was closed");
    assert_eq!(status.journal_entries, 1);
    assert!(matches!(begun, Ok(Response::Snapshot { .. })), "{begun:?}");
}

#[test]
fn a_preview_then_a_rewind_and_its_undo_by_snapshot_id() {
    let d = TestDaemon::start();
    d.write("a.txt", "v1");
    let mut client = d.client();
    d.begin(&mut client, "s1", Some("edit"));
    d.write("a.txt", "v2");
    d.write("new.txt", "created");
    d.end(&mut client, "s1");
    let start = d
        .points(&mut client)
        .into_iter()
        .find(|p| p.kind == PointKind::TurnStart)
        .unwrap();

    let Response::Preview(preview) = client
        .call(Request::Preview {
            root: d.root(),
            point: start.clone(),
        })
        .unwrap()
    else {
        panic!("expected a preview");
    };
    assert_eq!(
        (preview.delete, preview.revert),
        (vec!["new.txt".to_owned()], vec!["a.txt".to_owned()])
    );
    assert_eq!(d.read("a.txt"), "v2", "a preview changes nothing");

    let Response::Rewound(rewound) = client
        .call(Request::Rewind {
            root: d.root(),
            point: start,
            force: false,
        })
        .unwrap()
    else {
        panic!("expected a rewind");
    };
    assert_eq!(d.read("a.txt"), "v1");
    client
        .call(Request::RestoreSnapshot {
            root: d.root(),
            snapshot: rewound.safety_snapshot,
            force: false,
        })
        .unwrap();
    assert_eq!(d.read("a.txt"), "v2", "the rewind was undone");
}

#[test]
fn ending_a_session_closes_its_turn() {
    let d = TestDaemon::start();
    let mut client = d.client();
    d.begin(&mut client, "s1", None);

    let ended = client
        .call(Request::EndSession {
            root: d.root(),
            session: "s1".into(),
            agent: "claude-code".into(),
        })
        .unwrap();

    assert_eq!(ended, Response::TurnEnded { seq: Some(1) });
    assert!(d.status(&mut client).open_sessions.is_empty());
}

#[test]
fn after_a_failed_rollback_only_a_restore_by_snapshot_id_is_answered() {
    let d = TestDaemon::start();
    d.write("a.txt", "v1");
    let mut client = d.client();
    d.begin(&mut client, "s1", Some("first"));
    d.write("a.txt", "v2");
    d.end(&mut client, "s1");
    d.begin(&mut client, "s1", Some("second"));
    d.end(&mut client, "s1");
    let starts: Vec<_> = d
        .points(&mut client)
        .into_iter()
        .filter(|p| p.kind == PointKind::TurnStart)
        .map(|p| p.snapshot)
        .collect();
    let (safety, target) = (starts[0].clone(), starts[1].clone());
    drop(client);
    d.stop();
    // A rewind from v2 to v1 cut short after its only change, and the blob of "v2", which
    // only the safety snapshot holds, lost: the rollback cannot finish.
    let dir = d.data.path().join("projects").join(remora_engine::project_id(
        &d.project.path().canonicalize().unwrap(),
    ));
    let intent = dir.join(remora_engine::REWIND_FILE);
    let record = format!(r#"{{"safety":"{safety}","target":"{target}","started_at_ms":{START_MS}}}"#);
    std::fs::write(&intent, record).unwrap();
    d.write("a.txt", "v1");
    for shard in std::fs::read_dir(dir.join("store").join("blobs")).unwrap() {
        for blob in std::fs::read_dir(shard.unwrap().path()).unwrap() {
            let blob = blob.unwrap().path();
            if std::fs::read(&blob).unwrap() == b"v2" {
                std::fs::remove_file(blob).unwrap();
            }
        }
    }

    let restarted = remora_daemon::Daemon::start(common::config(d.data.path(), &d.clock)).unwrap();
    let mut client = remora_ipc::Client::connect(d.data.path()).unwrap();
    let refused = client.call(Request::Status { root: d.root() });
    let restored = client.call(Request::RestoreSnapshot {
        root: d.root(),
        snapshot: target,
        force: false,
    });
    let status = client.call(Request::Status { root: d.root() });
    restarted.shutdown();
    restarted.wait();

    let rewind_incomplete = ErrorKind::RewindIncomplete {
        safety_snapshot: safety.to_string(),
    };
    assert!(
        matches!(&refused, Err(ClientError::Daemon(body)) if body.kind == rewind_incomplete),
        "{refused:?}"
    );
    assert!(matches!(restored, Ok(Response::Rewound(_))), "{restored:?}");
    assert_eq!(d.read("a.txt"), "v1");
    assert!(!intent.exists(), "the record is gone");
    let Ok(Response::Status { status }) = status else {
        panic!("the project does not open: {status:?}");
    };
    assert_eq!(status.journal_entries, 3, "two turns and the restore");
}
