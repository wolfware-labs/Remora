use std::io::{self, BufReader};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use interprocess::ConnectWaitMode;
use interprocess::local_socket::{ConnectOptions, Stream};

use crate::framing::{FrameError, read_frame, write_frame};
use crate::{
    ErrorBody, Outcome, PROTOCOL_VERSION, Request, RequestEnvelope, Response, ResponseEnvelope, endpoint,
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
        let stream = connect_stream(data_dir, timeout)?;
        let mut client = Self {
            conn: Some(BufReader::new(stream)),
            next_id: 1,
            timeout,
            daemon_version: String::new(),
        };
        let hello = Request::Hello {
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol: PROTOCOL_VERSION,
        };
        match client.call(hello)? {
            Response::Hello { daemon_version, .. } => client.daemon_version = daemon_version,
            other => {
                return Err(ClientError::Protocol(format!(
                    "unexpected answer to hello: {other:?}"
                )));
            }
        }
        Ok(client)
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
fn connect_stream(data_dir: &Path, timeout: Duration) -> Result<Stream, ClientError> {
    let data_dir = data_dir
        .canonicalize()
        .map_err(|_| ClientError::DaemonNotRunning)?;
    let endpoint = endpoint(&data_dir);
    ConnectOptions::new()
        .name(endpoint.name()?)
        .wait_mode(ConnectWaitMode::Timeout(timeout))
        .connect_sync()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => ClientError::DaemonNotRunning,
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ClientError::Timeout(timeout),
            _ => ClientError::Io(e),
        })
}

/// Stop the daemon that owns `data_dir`, whatever protocol version it speaks, and wait up
/// to `timeout` until it has stopped: its endpoint gone and its instance lock
/// (`remorad.lock`) released, so a replacement can start at once. `Ok(false)` if no daemon
/// was running.
///
/// No `hello` is exchanged: an older or newer daemon would refuse it. The daemon stops even
/// though its answer is not read.
pub fn stop_daemon(data_dir: &Path, timeout: Duration) -> Result<bool, ClientError> {
    let deadline = Instant::now() + timeout;
    let mut stream = match connect_stream(data_dir, timeout) {
        Ok(stream) => stream,
        Err(ClientError::DaemonNotRunning) => return Ok(false),
        Err(e) => return Err(e),
    };
    let shutdown = RequestEnvelope {
        v: PROTOCOL_VERSION,
        id: 0,
        req: Request::Shutdown,
    };
    write_frame(&mut stream, &shutdown)?;
    drop(stream);
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
