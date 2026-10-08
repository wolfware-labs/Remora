#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::time::Duration;

use common::{START_MS, TestDaemon, config};
use remora_daemon::{Daemon, DaemonError};
use remora_ipc::{Client, ClientError, ErrorKind, Request, Response};

#[test]
fn a_turn_idle_for_over_6_hours_is_closed_as_interrupted() {
    let d = TestDaemon::start();
    let mut client = d.client();
    d.begin(&mut client, "s1", Some("interrupted by the user"));

    d.clock.advance(Duration::from_secs(5 * 60 * 60 + 59 * 60));
    d.daemon.sweep();
    assert_eq!(
        d.status(&mut client).open_sessions,
        ["s1"],
        "5 h 59 min is not stale yet: a permission prompt may be waiting"
    );

    d.clock.advance(Duration::from_secs(2 * 60));
    d.daemon.sweep();
    let status = d.status(&mut client);
    assert!(status.open_sessions.is_empty());
    assert_eq!(status.journal_entries, 1);
}

#[test]
fn turns_left_open_before_a_restart_are_closed_when_their_project_is_used() {
    let d = TestDaemon::start();
    let mut client = d.client();
    d.begin(&mut client, "s1", None);
    drop(client);
    d.stop();

    d.clock.advance(Duration::from_secs(7 * 60 * 60));
    let restarted = Daemon::start(config(d.data.path(), &d.clock)).unwrap();
    let mut client = Client::connect(d.data.path()).unwrap();
    let status = d.status(&mut client);
    restarted.shutdown();
    restarted.wait();

    assert!(status.open_sessions.is_empty());
    assert_eq!(status.journal_entries, 1);
}

#[test]
fn event_times_never_go_backwards() {
    let d = TestDaemon::start();
    let mut client = d.client();
    d.begin(&mut client, "s1", None);
    d.clock.set(START_MS - 60_000); // the clock jumped back a minute
    d.before(&mut client, "s1", "ls");
    d.before(&mut client, "s1", "ls -la");

    let mut times: Vec<i64> = d.points(&mut client).iter().map(|p| p.at_ms).collect();
    times.reverse(); // oldest first

    assert_eq!(times.len(), 3);
    assert!(times[0] >= START_MS);
    assert!(times.windows(2).all(|w| w[0] < w[1]), "{times:?}");
}

#[test]
fn a_client_with_another_protocol_version_is_told_what_to_do() {
    let d = TestDaemon::start();
    let mut client = d.client();

    let refused = client.call(Request::Hello {
        client_version: "9.0.0".into(),
        protocol: 2,
    });

    let Err(ClientError::Daemon(error)) = refused else {
        panic!("expected version_mismatch, got {refused:?}");
    };
    assert_eq!(error.kind, ErrorKind::VersionMismatch);
    assert!(error.message.contains("remorad install"), "{}", error.message);
}

#[test]
fn a_second_daemon_on_the_same_data_dir_is_refused() {
    let d = TestDaemon::start();

    let second = Daemon::start(config(d.data.path(), &d.clock));

    assert!(matches!(second, Err(DaemonError::AlreadyRunning(_))));
    assert!(
        Client::connect(d.data.path()).is_ok(),
        "the first daemon is unaffected"
    );
}

#[test]
fn a_stopped_daemon_releases_its_data_dir() {
    let d = TestDaemon::start();
    let idle = d.client(); // an idle connection must not keep the daemon alive
    d.stop();
    // A Windows pipe lasts while a connection to it is open (`remorad` exiting closes them all).
    drop(idle);

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match Client::connect_with_timeout(d.data.path(), Duration::from_millis(500)) {
            Err(ClientError::DaemonNotRunning) => break,
            other => assert!(std::time::Instant::now() < deadline, "still answering: {other:?}"),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let again = Daemon::start(config(d.data.path(), &d.clock)).unwrap();
    again.shutdown();
    again.wait();
}

#[test]
fn shutdown_lets_the_running_request_finish() {
    let d = TestDaemon::start();
    let mut client = d.client();
    d.begin(&mut client, "s1", None);
    let status = d.status(&mut client);
    let project_dir = d
        .data
        .path()
        .join("projects")
        .join(remora_engine::project_id(&status.root));

    // Another process holds the project, so the next request waits for it.
    let holder = std::fs::OpenOptions::new()
        .write(true)
        .open(project_dir.join("lock"))
        .unwrap();
    holder.lock().unwrap();
    let root = d.root();
    let slow = std::thread::spawn(move || {
        client.call(Request::BeforeCommand {
            root,
            session: "s1".into(),
            agent: "claude-code".into(),
            command: "make".into(),
            tool_use_id: None,
        })
    });
    std::thread::sleep(Duration::from_millis(200)); // let it reach the worker
    let mut other = d.client();
    assert_eq!(other.call(Request::Shutdown).unwrap(), Response::ShuttingDown);
    let daemon = d.daemon.clone();
    let waiting = std::thread::spawn(move || daemon.wait());
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !waiting.is_finished(),
        "wait() returned while a request was still running"
    );
    holder.unlock().unwrap();

    waiting.join().unwrap();
    assert!(
        matches!(slow.join().unwrap(), Ok(Response::Snapshot { .. })),
        "finished, not cut off"
    );
    assert!(matches!(
        Client::connect(d.data.path()),
        Err(ClientError::DaemonNotRunning)
    ));
}

#[test]
fn stop_daemon_stops_a_daemon_without_a_hello() {
    let d = TestDaemon::start();
    // As in `remorad run`, a thread waits on the daemon: that is what releases its lock.
    let daemon = d.daemon.clone();
    let waiting = std::thread::spawn(move || daemon.wait());

    assert!(remora_ipc::stop_daemon(d.data.path(), Duration::from_secs(10)).unwrap());

    waiting.join().unwrap();
    assert!(matches!(
        Client::connect(d.data.path()),
        Err(ClientError::DaemonNotRunning)
    ));
    assert!(
        !remora_ipc::stop_daemon(d.data.path(), Duration::from_secs(1)).unwrap(),
        "nothing left to stop"
    );
}

#[test]
fn stop_daemon_returns_only_once_a_replacement_can_start() {
    let d = TestDaemon::start();
    let mut client = d.client();
    d.begin(&mut client, "s1", None);
    let status = d.status(&mut client);
    let lock_file = d
        .data
        .path()
        .join("projects")
        .join(remora_engine::project_id(&status.root))
        .join("lock");
    // A request in flight keeps the old daemon (and its instance lock) alive a while.
    let holder = std::fs::OpenOptions::new().write(true).open(lock_file).unwrap();
    holder.lock().unwrap();
    let root = d.root();
    let slow = std::thread::spawn(move || {
        client.call(Request::BeforeCommand {
            root,
            session: "s1".into(),
            agent: "claude-code".into(),
            command: "make".into(),
            tool_use_id: None,
        })
    });
    std::thread::sleep(Duration::from_millis(200));
    // As in `remorad run`, a thread waits on the daemon: that is what releases its lock.
    let daemon = d.daemon.clone();
    let waiting = std::thread::spawn(move || daemon.wait());
    let data = d.data.path().to_path_buf();
    let stopping = std::thread::spawn(move || remora_ipc::stop_daemon(&data, Duration::from_secs(10)));
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !stopping.is_finished(),
        "returned while the old daemon still held its lock"
    );
    holder.unlock().unwrap();

    assert!(stopping.join().unwrap().unwrap());
    let replacement = Daemon::start(config(d.data.path(), &d.clock));
    waiting.join().unwrap();
    assert!(replacement.is_ok(), "{:?}", replacement.err());
    let replacement = replacement.unwrap();
    replacement.shutdown();
    replacement.wait();
    assert!(slow.join().unwrap().is_ok());
}
