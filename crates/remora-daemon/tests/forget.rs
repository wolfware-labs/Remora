#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use common::TestDaemon;
use remora_engine::project_id;
use remora_ipc::{ClientError, ErrorKind, Request, Response};

#[test]
fn forget_closes_the_open_project_and_removes_its_data() {
    let d = TestDaemon::start();
    let mut client = d.client();
    d.write("a.txt", "a");
    d.begin(&mut client, "s1", Some("edit"));
    d.end(&mut client, "s1");
    let dir = d
        .data
        .path()
        .join("projects")
        .join(project_id(&d.root().canonicalize().unwrap()));
    assert!(dir.exists());

    let answer = client
        .call(Request::Forget {
            root: d.root(),
            force: false,
        })
        .unwrap();

    let Response::Forgotten { data_dir, blob_home } = answer else {
        panic!("unexpected {answer:?}");
    };
    assert!(!data_dir.exists() && !dir.exists());
    assert_eq!(blob_home, None);
    assert!(d.points(&mut client).is_empty(), "a later request starts afresh");
}

#[test]
fn forget_is_refused_while_a_turn_is_open() {
    let d = TestDaemon::start();
    let mut client = d.client();
    d.begin(&mut client, "s1", Some("working"));

    let refused = client.call(Request::Forget {
        root: d.root(),
        force: false,
    });

    assert!(
        matches!(&refused, Err(ClientError::Daemon(e)) if matches!(e.kind, ErrorKind::TurnInProgress { .. })),
        "{refused:?}"
    );
    assert_eq!(
        d.points(&mut client).len(),
        1,
        "nothing removed, the project still works"
    );
}

fn forgotten_daemon() -> (TestDaemon, std::path::PathBuf) {
    let d = TestDaemon::start();
    let mut client = d.client();
    d.write("a.txt", "a");
    d.begin(&mut client, "s1", Some("edit"));
    d.end(&mut client, "s1");
    let dir = d
        .data
        .path()
        .join("projects")
        .join(project_id(&d.root().canonicalize().unwrap()));
    client
        .call(Request::Forget {
            root: d.root(),
            force: false,
        })
        .unwrap();
    assert!(!dir.exists());
    (d, dir)
}

#[test]
fn the_sweep_does_not_bring_a_forgotten_project_back() {
    let (d, dir) = forgotten_daemon();

    d.daemon.sweep();

    assert!(!dir.exists());
}

#[test]
fn the_daily_prune_does_not_bring_a_forgotten_project_back() {
    let (d, dir) = forgotten_daemon();

    d.daemon.prune_if_due();

    assert!(!dir.exists());
}
