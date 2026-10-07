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

#[cfg(unix)]
#[test]
fn a_daemon_that_never_answers_times_out_while_connecting() {
    let data = tempfile::TempDir::new().unwrap();
    let Endpoint::Socket(path) = endpoint(&data.path().canonicalize().unwrap()) else {
        panic!("Unix endpoints are socket files");
    };
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let silent = std::thread::spawn(move || listener.accept().map(|(conn, _)| conn));

    let started = std::time::Instant::now();
    let result = Client::connect_with_timeout(data.path(), std::time::Duration::from_millis(200));

    assert!(matches!(result, Err(ClientError::Timeout(_))), "{result:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    drop(silent.join().unwrap());
}
