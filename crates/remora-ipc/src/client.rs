use std::io::{self, BufReader};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use interprocess::ConnectWaitMode;
use interprocess::local_socket::{ConnectOptions, Stream};

use crate::auth::{self, Secret};
use crate::framing::{FrameError, read_frame, write_frame};
use crate::{
    ErrorBody, ErrorKind, Outcome, PROTOCOL_VERSION, Request, RequestEnvelope, Response, ResponseEnvelope,
    endpoint,
};

/// How long [`Client::call`] waits for an answer by default. Generous, because
/// `before_command` waits for a snapshot of the whole project.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("the Remora daemon is not running")]
    DaemonNotRunning,
    #[error("no answer from the Remora daemon within {0:?}")]
    Timeout(Duration),
    #[error("the connection to the Remora daemon was closed")]
    Disconnected,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("unexpected message from the Remora daemon: {0}")]
    Protocol(String),
    /// What answered on the daemon's endpoint could not prove it knows the secret: it is not
    /// this user's daemon. Nothing but the challenge was sent to it.
    #[error("something other than your Remora daemon answers on {endpoint}")]
    Impostor { endpoint: String },
    /// The folder holding the daemon's socket is someone else's or open to others: nothing
    /// is sent through it.
    #[error("unsafe socket directory {}: it must be yours and closed to other users", .0.display())]
    UnsafeSocketDir(std::path::PathBuf),
    /// The daemon answered with an error.
    #[error(transparent)]
    Daemon(ErrorBody),
}

impl From<FrameError> for ClientError {
    fn from(e: FrameError) -> Self {
        match e {
            FrameError::Io(e) => ClientError::Io(e),
            other => ClientError::Protocol(other.to_string()),
        }
    }
}

/// A connection to the daemon. Requests on one connection are answered in order.
#[derive(Debug)]
pub struct Client {
    /// `None` after a failed or timed-out exchange: the connection is then unusable.
    conn: Option<BufReader<Stream>>,
    next_id: u64,
    timeout: Duration,
    daemon_version: String,
}

impl Client {
    /// Connect to the daemon that owns `data_dir` and exchange versions, waiting up to [`DEFAULT_TIMEOUT`] for each answer.
    pub fn connect(data_dir: &Path) -> Result<Self, ClientError> {
        Self::connect_with_timeout(data_dir, DEFAULT_TIMEOUT)
    }

    /// Like [`connect`](Self::connect), with `timeout` for the `hello` exchange and every later call.
    pub fn connect_with_timeout(data_dir: &Path, timeout: Duration) -> Result<Self, ClientError> {
        let (client, protocol) = Self::open(data_dir, timeout)?;
        if protocol != PROTOCOL_VERSION {
            return Err(ClientError::Daemon(ErrorBody::new(
                ErrorKind::VersionMismatch,
                format!(
                    "the running daemon ({}) speaks protocol {protocol}, this remora ({}) speaks {PROTOCOL_VERSION}: run `remora service install` to replace it",
                    client.daemon_version,
                    env!("CARGO_PKG_VERSION")
                ),
            )));
        }
        Ok(client)
    }

    /// Connect and authenticate, whatever protocol the daemon speaks. Returns its protocol.
    fn open(data_dir: &Path, timeout: Duration) -> Result<(Self, u32), ClientError> {
        // No secret means no daemon has started for this data directory (or this user cannot
        // read it).
        let secret = Secret::load(data_dir).map_err(|_| ClientError::DaemonNotRunning)?;
        match Self::open_with(data_dir, timeout, &secret) {
            // The daemon may have restarted, with a new secret, since this one was read: one
            // more try with the new one.
            Err(ClientError::Impostor { .. })
                if Secret::load(data_dir).is_ok_and(|current| current != secret) =>
            {
                let current = Secret::load(data_dir).map_err(|_| ClientError::DaemonNotRunning)?;
                Self::open_with(data_dir, timeout, &current)
            }
            opened => opened,
        }
    }

    fn open_with(data_dir: &Path, timeout: Duration, secret: &Secret) -> Result<(Self, u32), ClientError> {
        let (stream, endpoint) = connect_stream(data_dir, timeout)?;
        let mut client = Self {
            conn: Some(BufReader::new(stream)),
            next_id: 1,
            timeout,
            daemon_version: String::new(),
        };
        let protocol = client.authenticate(secret, &endpoint)?;
        Ok((client, protocol))
    }

    /// The handshake: the daemon proves it knows the secret before anything else is sent,
    /// then the client proves it does. Returns the daemon's protocol.
    fn authenticate(&mut self, secret: &Secret, endpoint: &str) -> Result<u32, ClientError> {
        let nonce = auth::nonce()?;
        let hello = Request::Hello {
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol: PROTOCOL_VERSION,
            nonce: nonce.clone(),
        };
        let (protocol, server_nonce) = match self.call(hello)? {
            Response::Hello {
                daemon_version,
                protocol,
                nonce: server_nonce,
                proof,
            } => {
                if !secret.verify_server(&nonce, &proof) {
                    self.conn = None;
                    return Err(ClientError::Impostor {
                        endpoint: endpoint.to_owned(),
                    });
                }
                self.daemon_version = daemon_version;
                (protocol, server_nonce)
            }
            other => {
                return Err(ClientError::Protocol(format!(
                    "unexpected answer to hello: {other:?}"
                )));
            }
        };
        match self.call(Request::Prove {
            proof: secret.client_proof(&server_nonce),
        })? {
            Response::Authenticated => Ok(protocol),
            other => Err(ClientError::Protocol(format!(
                "unexpected answer to prove: {other:?}"
            ))),
        }
    }

    /// How long each [`call`](Self::call) waits for its answer.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The daemon's version, from the `hello` exchange.
    pub fn daemon_version(&self) -> &str {
        &self.daemon_version
    }

    /// Ask the daemon behind this connection (and no other) to stop, and wait up to `timeout`
    /// for it to be gone, or replaced by a daemon of another version. Unlike [`stop_daemon`]
    /// this cannot stop a daemon that took the old one's place after the version was read.
    pub fn stop(self, data_dir: &Path, timeout: Duration) -> Result<(), ClientError> {
        let deadline = Instant::now() + timeout;
        let stopping = self.daemon_version.clone();
        // The daemon stops once it has read the request, whether or not the answer arrives.
        let mut connection = self.with_timeout(timeout);
        let _ = connection.call(Request::Shutdown);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match Client::connect_with_timeout(data_dir, left.max(Duration::from_millis(10))) {
                Ok(other) if other.daemon_version != stopping => return Ok(()),
                Err(ClientError::DaemonNotRunning) if instance_lock_is_free(data_dir) => {
                    return Ok(());
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(ClientError::Timeout(timeout));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Send `request` and wait for its answer. An error answer is
    /// [`ClientError::Daemon`].
    pub fn call(&mut self, request: Request) -> Result<Response, ClientError> {
        let mut conn = self.conn.take().ok_or(ClientError::Disconnected)?;
        let id = self.next_id;
        self.next_id += 1;
        let envelope = RequestEnvelope {
            v: PROTOCOL_VERSION,
            id,
            req: request,
        };
        // Named pipes have no read timeouts, so the exchange runs on its own thread and
        // the wait is bounded here. After a timeout the connection is abandoned.
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = exchange(&mut conn, &envelope);
            let _ = tx.send((conn, result));
        });
        let (conn, result) = match rx.recv_timeout(self.timeout) {
            Ok(done) => done,
            Err(RecvTimeoutError::Timeout) => return Err(ClientError::Timeout(self.timeout)),
            Err(RecvTimeoutError::Disconnected) => return Err(ClientError::Disconnected),
        };
        let response = result?;
        if response.v != PROTOCOL_VERSION || response.id != id {
            // Out of step with the daemon: the connection is not reused.
            return Err(ClientError::Protocol(format!(
                "answer v{} #{} to request v{PROTOCOL_VERSION} #{id}",
                response.v, response.id
            )));
        }
        self.conn = Some(conn);
        match response.outcome {
            Outcome::Ok(response) => Ok(response),
            Outcome::Err(error) => Err(ClientError::Daemon(error)),
        }
    }
}

fn exchange(
    conn: &mut BufReader<Stream>,
    envelope: &RequestEnvelope,
) -> Result<ResponseEnvelope, ClientError> {
    write_frame(conn.get_mut(), envelope)?;
    let line = read_frame(conn)?.ok_or(ClientError::Disconnected)?;
    serde_json::from_str(&line).map_err(|e| ClientError::Protocol(e.to_string()))
}

/// Connect to the endpoint of the daemon that owns `data_dir`, waiting at most `timeout`.
/// Returns the stream and the endpoint, for messages.
fn connect_stream(data_dir: &Path, timeout: Duration) -> Result<(Stream, String), ClientError> {
    let data_dir = data_dir
        .canonicalize()
        .map_err(|_| ClientError::DaemonNotRunning)?;
    let endpoint = endpoint(&data_dir);
    #[cfg(unix)]
    if let crate::Endpoint::Socket(path) = &endpoint
        && let Some(dir) = path.parent().filter(|dir| *dir != data_dir)
    {
        match crate::socket_dir_is_private(dir, &data_dir) {
            Ok(true) => {}
            Ok(false) => return Err(ClientError::UnsafeSocketDir(dir.to_path_buf())),
            // No folder, no daemon.
            Err(_) => return Err(ClientError::DaemonNotRunning),
        }
    }
    let stream = ConnectOptions::new()
        .name(endpoint.name()?)
        .wait_mode(ConnectWaitMode::Timeout(timeout))
        .connect_sync()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => ClientError::DaemonNotRunning,
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ClientError::Timeout(timeout),
            _ => ClientError::Io(e),
        })?;
    Ok((stream, endpoint.to_string()))
}

/// Stop the daemon that owns `data_dir` and wait up
/// to `timeout` until it has stopped: its endpoint gone and its instance lock
/// (`remorad.lock`) released, so a replacement can start at once. `Ok(false)` if no daemon
/// was running.
///
/// A daemon from before authentication writes no `remorad.secret`: if one answers anyway, it
/// gets the bare `{"v":1,"id":0,"req":{"type":"shutdown"}}` such daemons accept, which
/// carries nothing secret.
pub fn stop_daemon(data_dir: &Path, timeout: Duration) -> Result<bool, ClientError> {
    let deadline = Instant::now() + timeout;
    match Client::open(data_dir, timeout) {
        Ok((mut client, _protocol)) => {
            // The daemon stops once it has read the request, whether or not the answer arrives.
            let _ = client.call(Request::Shutdown);
        }
        // A secret, but nothing listening: no daemon.
        Err(ClientError::DaemonNotRunning) if Secret::load(data_dir).is_ok() => return Ok(false),
        Err(ClientError::DaemonNotRunning) => match connect_stream(data_dir, timeout) {
            Ok((mut stream, _)) => {
                let shutdown = RequestEnvelope {
                    v: 1,
                    id: 0,
                    req: Request::Shutdown,
                };
                let _ = write_frame(&mut stream, &shutdown);
            }
            Err(ClientError::DaemonNotRunning) => return Ok(false),
            Err(e) => return Err(e),
        },
        Err(e) => return Err(e),
    }
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let gone = matches!(
            connect_stream(data_dir, left.max(Duration::from_millis(10))),
            Err(ClientError::DaemonNotRunning)
        );
        if gone && instance_lock_is_free(data_dir) {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Err(ClientError::Timeout(timeout));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Whether no daemon holds `<data_dir>/remorad.lock` (a missing file counts as free).
fn instance_lock_is_free(data_dir: &Path) -> bool {
    let path = data_dir.join("remorad.lock");
    match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
        // Locking and dropping at once: the lock is only probed.
        Ok(file) => file.try_lock().is_ok(),
        Err(e) => e.kind() == io::ErrorKind::NotFound,
    }
}
