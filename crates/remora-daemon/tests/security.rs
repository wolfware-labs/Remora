#![cfg(unix)]
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::os::unix::fs::{FileTypeExt, PermissionsExt};

use common::TestDaemon;
use remora_ipc::{Endpoint, endpoint};

#[test]
fn the_socket_is_an_owner_only_file_in_the_owner_only_data_dir() {
    let d = TestDaemon::start();
    let Endpoint::Socket(path) = endpoint(d.daemon.data_dir()) else {
        panic!("Unix daemons listen on a socket file");
    };

    let meta = std::fs::symlink_metadata(&path).expect("on disk, not in the abstract namespace");

    assert!(meta.file_type().is_socket());
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    let dir = std::fs::metadata(d.daemon.data_dir()).unwrap();
    assert_eq!(dir.permissions().mode() & 0o777, 0o700);
}

#[test]
fn a_stale_socket_file_from_a_crashed_daemon_is_replaced() {
    let d = TestDaemon::start();
    let data = d.data.path().to_path_buf();
    let path = match endpoint(d.daemon.data_dir()) {
        Endpoint::Socket(path) => path,
        Endpoint::Pipe(_) => unreachable!(),
    };
    d.stop();
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap()); // what a crash leaves behind

    let again = remora_daemon::Daemon::start(common::config(&data, &d.clock)).unwrap();
    let connected = remora_ipc::Client::connect(&data);
    again.shutdown();
    again.wait();

    assert!(connected.is_ok());
}

#[test]
fn the_secret_file_is_owner_only() {
    let d = TestDaemon::start();

    let meta = std::fs::metadata(d.data.path().join(remora_ipc::auth::SECRET_FILE)).unwrap();

    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}

/// A data directory whose `remorad.sock` would be longer than the OS allows.
fn long_data_dir(parent: &std::path::Path) -> std::path::PathBuf {
    let dir = parent.join("d".repeat(110));
    std::fs::create_dir(&dir).unwrap();
    dir
}

#[test]
fn a_long_data_dir_listens_in_an_owner_only_runtime_dir() {
    let parent = tempfile::TempDir::new().unwrap();
    let data = long_data_dir(parent.path());
    let clock = remora_daemon::ManualClock::new(common::START_MS);
    let daemon = remora_daemon::Daemon::start(common::config(&data, &clock)).unwrap();

    let connected = remora_ipc::Client::connect(&data);
    let Endpoint::Socket(path) = endpoint(daemon.data_dir()) else {
        unreachable!()
    };
    daemon.shutdown();
    daemon.wait();

    assert!(connected.is_ok(), "{connected:?}");
    assert!(!path.starts_with(&data) && path.as_os_str().len() <= remora_ipc::MAX_SOCKET_PATH);
    let dir = std::fs::metadata(path.parent().unwrap()).unwrap();
    assert_eq!(dir.permissions().mode() & 0o077, 0, "closed to others");
}

#[test]
fn a_runtime_dir_open_to_others_is_refused() {
    let parent = tempfile::TempDir::new().unwrap();
    let data = long_data_dir(parent.path());
    let runtime = tempfile::TempDir::new().unwrap();
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut daemon = std::process::Command::new(env!("CARGO_BIN_EXE_remorad"))
        .args(["run", "--data-dir"])
        .arg(&data)
        .env("XDG_RUNTIME_DIR", runtime.path())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // A daemon that accepted the folder would run until stopped: give it a few seconds.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while daemon.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let refused = daemon.try_wait().unwrap();
    if refused.is_none() {
        daemon.kill().unwrap();
    }
    let out = daemon.wait_with_output().unwrap();
    assert!(
        refused.is_some_and(|status| !status.success()),
        "remorad listened"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unsafe socket directory"), "{stderr}");
}

#[test]
fn without_xdg_runtime_dir_an_owner_only_folder_is_made_in_temp() {
    use std::os::unix::fs::MetadataExt;
    let parent = tempfile::TempDir::new().unwrap();
    let data = long_data_dir(parent.path());
    let temp = tempfile::TempDir::new().unwrap();
    let mut daemon = std::process::Command::new(env!("CARGO_BIN_EXE_remorad"))
        .args(["run", "--data-dir"])
        .arg(&data)
        .env_remove("XDG_RUNTIME_DIR")
        .env("TMPDIR", temp.path())
        .spawn()
        .unwrap();

    let runtime = temp
        .path()
        .join(format!("remora-{}", std::fs::metadata(&data).unwrap().uid()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let socket = loop {
        let found = std::fs::read_dir(&runtime)
            .into_iter()
            .flatten()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().ends_with(".sock"));
        if let Some(entry) = found {
            break entry.path();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no socket in {}",
            runtime.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mode = std::fs::metadata(&runtime).unwrap().permissions().mode() & 0o777;
    daemon.kill().unwrap();
    daemon.wait().unwrap();

    assert_eq!(mode, 0o700);
    assert!(
        socket
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("remora-")
    );
}
