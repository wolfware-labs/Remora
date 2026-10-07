#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use common::TestDaemon;
use remora_engine::PointKind;
use remora_ipc::{JournalCheck, Request, Response};

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
    let Response::RestorePoints { points } = client
        .call(Request::RestorePoints { root: link.clone() })
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
