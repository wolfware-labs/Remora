#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::io::{BufRead, Write};

use common::TestDaemon;
use remora_ipc::{ErrorKind, MAX_MESSAGE_BYTES, Outcome, ResponseEnvelope};

fn send(
    conn: &mut std::io::BufReader<interprocess::local_socket::Stream>,
    bytes: &[u8],
) -> Option<ResponseEnvelope> {
    if conn.get_mut().write_all(bytes).is_err() {
        return None; // the daemon already closed the connection
    }
    let mut line = String::new();
    match conn.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(serde_json::from_str(&line).unwrap()),
    }
}

fn error_kind(response: &ResponseEnvelope) -> &ErrorKind {
    match &response.outcome {
        Outcome::Err(e) => &e.kind,
        Outcome::Ok(ok) => panic!("expected an error, got {ok:?}"),
    }
}

#[test]
fn a_request_that_is_not_json_is_answered_then_the_connection_closed() {
    let d = TestDaemon::start();
    let mut conn = d.raw();

    let response = send(&mut conn, b"hello daemon\n").unwrap();

    assert_eq!(error_kind(&response), &ErrorKind::InvalidRequest);
    assert_eq!(response.id, 0);
    assert!(send(&mut conn, b"{}\n").is_none(), "closed");
}

#[test]
fn a_malformed_request_keeps_its_id_in_the_answer() {
    let d = TestDaemon::start();
    let mut conn = d.raw();

    let response = send(
        &mut conn,
        b"{\"v\":1,\"id\":42,\"req\":{\"type\":\"format_disk\"}}\n",
    )
    .unwrap();

    assert_eq!(
        (response.id, error_kind(&response)),
        (42, &ErrorKind::InvalidRequest)
    );
}

#[test]
fn an_oversized_request_is_refused() {
    let d = TestDaemon::start();
    let mut conn = d.raw();
    let huge = vec![b'x'; MAX_MESSAGE_BYTES + 1];

    let response = send(&mut conn, &huge).unwrap();

    assert_eq!(error_kind(&response), &ErrorKind::InvalidRequest);
}

#[test]
fn a_request_with_another_envelope_version_is_refused() {
    let d = TestDaemon::start();
    let mut conn = d.raw();

    let response = send(&mut conn, b"{\"v\":2,\"id\":5,\"req\":{\"type\":\"shutdown\"}}\n").unwrap();

    assert_eq!(
        (response.id, error_kind(&response)),
        (5, &ErrorKind::VersionMismatch)
    );
    assert!(
        d.client().daemon_version().starts_with("0."),
        "and the daemon did not shut down"
    );
}

#[test]
fn a_relative_project_folder_is_refused() {
    let d = TestDaemon::start();
    let mut conn = d.raw();

    let response = send(
        &mut conn,
        b"{\"v\":1,\"id\":1,\"req\":{\"type\":\"status\",\"root\":\"proj\"}}\n",
    )
    .unwrap();

    assert_eq!(error_kind(&response), &ErrorKind::InvalidRequest);
}

#[test]
fn a_shutdown_whose_answer_cannot_be_delivered_still_stops_the_daemon() {
    let d = TestDaemon::start();
    let mut conn = d.raw();
    conn.get_mut()
        .write_all(b"{\"v\":1,\"id\":1,\"req\":{\"type\":\"shutdown\"}}\n")
        .unwrap();
    drop(conn); // gone before the answer arrives

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while remora_ipc::Client::connect(d.data.path()).is_ok() {
        assert!(std::time::Instant::now() < deadline, "the daemon kept running");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
