//! The client's side of the handshake, against a stand-in for the daemon.
#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::io::BufReader;
use std::path::Path;
use std::thread::JoinHandle;

use interprocess::local_socket::ListenerOptions;
use interprocess::local_socket::traits::Listener as _;
use remora_ipc::auth::Secret;
use remora_ipc::{
    Client, ClientError, ErrorKind, Outcome, Request, RequestEnvelope, Response, ResponseEnvelope, endpoint,
    read_frame, write_frame,
};

/// Listen on the endpoint of `data` and answer one connection's hello with `proof_with`'s
/// proof and `protocol`; returns every request line the stand-in received.
fn stand_in(data: &Path, proof_with: Secret, protocol: u32) -> JoinHandle<Vec<String>> {
    let endpoint = endpoint(&data.canonicalize().unwrap());
    let listener = ListenerOptions::new()
        .name(endpoint.name().unwrap())
        .create_sync()
        .unwrap();
    std::thread::spawn(move || {
        let mut conn = BufReader::new(listener.accept().unwrap());
        let mut received = Vec::new();
        while let Ok(Some(line)) = read_frame(&mut conn) {
            let request: RequestEnvelope = serde_json::from_str(&line).unwrap();
            received.push(line);
            let response = match request.req {
                Request::Hello { nonce, .. } => Response::Hello {
                    daemon_version: "9.9.9".into(),
                    protocol,
                    nonce: "beef".into(),
                    proof: proof_with.server_proof(&nonce),
                },
                Request::Prove { .. } => Response::Authenticated,
                _ => Response::Recorded,
            };
            let answer = ResponseEnvelope {
                v: 1,
                id: request.id,
                outcome: Outcome::Ok(response),
            };
            if write_frame(conn.get_mut(), &answer).is_err() {
                break;
            }
        }
        received
    })
}

fn client_side_secret(data: &Path) -> Secret {
    let secret = Secret::generate().unwrap();
    secret.store(data).unwrap();
    secret
}

#[test]
fn a_server_that_cannot_prove_the_secret_is_an_impostor_and_learns_nothing() {
    let data = tempfile::TempDir::new().unwrap();
    client_side_secret(data.path());
    let impostor = stand_in(data.path(), Secret::generate().unwrap(), 1);

    let result = Client::connect(data.path());

    let Err(ClientError::Impostor { endpoint: named }) = &result else {
        panic!("expected an impostor, got {result:?}");
    };
    let expected = endpoint(&data.path().canonicalize().unwrap()).to_string();
    assert_eq!(named, &expected);
    assert!(
        result.unwrap_err().to_string().contains(&expected),
        "the message names it"
    );
    let received = impostor.join().unwrap();
    assert_eq!(received.len(), 1, "only the hello: {received:?}");
    assert!(!received[0].contains(&Secret::load(data.path()).unwrap().client_proof("beef")));
}

#[test]
fn a_daemon_speaking_another_protocol_is_reported_with_what_to_do() {
    let data = tempfile::TempDir::new().unwrap();
    let secret = client_side_secret(data.path());
    let daemon = stand_in(data.path(), secret, 2);

    let result = Client::connect(data.path());

    let Err(ClientError::Daemon(error)) = result else {
        panic!("expected version_mismatch, got {result:?}");
    };
    assert_eq!(error.kind, ErrorKind::VersionMismatch);
    assert!(
        error.message.contains("remora service install"),
        "{}",
        error.message
    );
    drop(daemon);
}

#[test]
fn without_a_secret_file_no_connection_is_attempted() {
    let data = tempfile::TempDir::new().unwrap();
    // Something listens, but no daemon has written a secret.
    let endpoint = endpoint(&data.path().canonicalize().unwrap());
    let _listener = ListenerOptions::new()
        .name(endpoint.name().unwrap())
        .create_sync()
        .unwrap();

    assert!(matches!(
        Client::connect(data.path()),
        Err(ClientError::DaemonNotRunning)
    ));
}

#[test]
fn a_daemon_that_restarted_with_a_new_secret_is_reached_on_a_second_try() {
    let data = tempfile::TempDir::new().unwrap();
    client_side_secret(data.path()); // what the client reads first
    let restarted = Secret::generate().unwrap();
    let endpoint = endpoint(&data.path().canonicalize().unwrap());
    let listener = ListenerOptions::new()
        .name(endpoint.name().unwrap())
        .create_sync()
        .unwrap();
    let dir = data.path().to_path_buf();
    let daemon = std::thread::spawn(move || {
        // The client has read the old secret and says hello: only now does the restarted
        // daemon's secret replace it. Every connection is answered with the new one.
        for _ in 0..2 {
            let mut conn = BufReader::new(listener.accept().unwrap());
            while let Ok(Some(line)) = read_frame(&mut conn) {
                let request: RequestEnvelope = serde_json::from_str(&line).unwrap();
                let response = match request.req {
                    Request::Hello { nonce, .. } => {
                        restarted.store(&dir).unwrap();
                        Response::Hello {
                            daemon_version: "9.9.9".into(),
                            protocol: 1,
                            nonce: "beef".into(),
                            proof: restarted.server_proof(&nonce),
                        }
                    }
                    Request::Prove { .. } => Response::Authenticated,
                    _ => Response::Recorded,
                };
                let answer = ResponseEnvelope {
                    v: 1,
                    id: request.id,
                    outcome: Outcome::Ok(response),
                };
                if write_frame(conn.get_mut(), &answer).is_err() {
                    break;
                }
            }
        }
    });

    let result = Client::connect(data.path());

    assert!(result.is_ok(), "{result:?}");
    drop(result);
    daemon.join().unwrap();
}

#[test]
fn a_daemon_from_before_authentication_is_stopped_with_a_bare_shutdown() {
    let data = tempfile::TempDir::new().unwrap();
    // No secret file: an older daemon. It stops at a bare shutdown.
    let endpoint = endpoint(&data.path().canonicalize().unwrap());
    let listener = ListenerOptions::new()
        .name(endpoint.name().unwrap())
        .create_sync()
        .unwrap();
    let old = std::thread::spawn(move || {
        let mut conn = BufReader::new(listener.accept().unwrap());
        let line = read_frame(&mut conn).unwrap().unwrap();
        drop(listener);
        line
    });

    let stopped = remora_ipc::stop_daemon(data.path(), std::time::Duration::from_secs(10)).unwrap();

    assert!(stopped);
    let request: RequestEnvelope = serde_json::from_str(&old.join().unwrap()).unwrap();
    assert_eq!((request.id, request.req), (0, Request::Shutdown));
}

#[cfg(unix)]
#[test]
fn only_a_private_folder_may_hold_the_socket() {
    use std::os::unix::fs::PermissionsExt;
    let data = tempfile::TempDir::new().unwrap();
    let runtime = tempfile::TempDir::new().unwrap();
    let dir = runtime.path().join("remora-1000");
    std::fs::create_dir(&dir).unwrap();

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(remora_ipc::socket_dir_is_private(&dir, data.path()).unwrap());

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        !remora_ipc::socket_dir_is_private(&dir, data.path()).unwrap(),
        "open to others"
    );

    let link = runtime.path().join("link");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    assert!(
        !remora_ipc::socket_dir_is_private(&link, data.path()).unwrap(),
        "a symlink"
    );
}
