#![allow(clippy::unwrap_used)] // tests may panic on failure

use remora_ipc::{Client, ClientError, Endpoint, endpoint};

#[test]
fn connecting_without_a_daemon_says_so() {
    let data = tempfile::TempDir::new().unwrap();
    assert!(matches!(
        Client::connect(data.path()),
        Err(ClientError::DaemonNotRunning)
    ));
}

#[test]
fn connecting_to_a_missing_data_dir_says_the_daemon_is_not_running() {
    let data = tempfile::TempDir::new().unwrap();
    let missing = data.path().join("nope");
    assert!(matches!(
        Client::connect(&missing),
        Err(ClientError::DaemonNotRunning)
    ));
}

#[cfg(unix)]
#[test]
fn a_stale_socket_file_means_the_daemon_is_not_running() {
    let data = tempfile::TempDir::new().unwrap();
    let Endpoint::Socket(path) = endpoint(&data.path().canonicalize().unwrap()) else {
        panic!("Unix endpoints are socket files");
    };
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap()); // leaves the file behind

    assert!(matches!(
        Client::connect(data.path()),
        Err(ClientError::DaemonNotRunning)
    ));
}

#[test]
fn endpoints_differ_per_data_dir() {
    let a = endpoint(std::path::Path::new("/data/a"));
    let b = endpoint(std::path::Path::new("/data/b"));
    assert_ne!(a, b);
    if cfg!(unix) {
        assert_eq!(a, Endpoint::Socket("/data/a/remorad.sock".into()));
    }
}
