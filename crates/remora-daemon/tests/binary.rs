#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use remora_ipc::{Client, ClientError, Request, Response};

const REMORAD: &str = env!("CARGO_BIN_EXE_remorad");

fn connect_within(data: &std::path::Path, limit: Duration) -> Client {
    let deadline = Instant::now() + limit;
    loop {
        match Client::connect(data) {
            Ok(client) => return client,
            Err(ClientError::DaemonNotRunning) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("the daemon did not come up: {e}"),
        }
    }
}

#[test]
fn remorad_runs_until_asked_to_shut_down() {
    let data = tempfile::TempDir::new().unwrap();
    let mut daemon = Command::new(REMORAD)
        .args(["run", "--data-dir"])
        .arg(data.path())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut client = connect_within(data.path(), Duration::from_secs(10));

    let second = Command::new(REMORAD)
        .args(["run", "--data-dir"])
        .arg(data.path())
        .output()
        .unwrap();
    assert!(second.status.success(), "a second daemon exits cleanly");
    assert!(String::from_utf8_lossy(&second.stderr).contains("already running"));

    assert_eq!(client.call(Request::Shutdown).unwrap(), Response::ShuttingDown);
    assert!(daemon.wait().unwrap().success());
    let log = std::fs::read_to_string(data.path().join("remorad.log")).unwrap();
    assert!(log.contains("listening on"), "{log}");
}

#[test]
fn an_unknown_command_prints_usage() {
    let out = Command::new(REMORAD).arg("frobnicate").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: remorad"));
}
