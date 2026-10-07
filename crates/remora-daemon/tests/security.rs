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
