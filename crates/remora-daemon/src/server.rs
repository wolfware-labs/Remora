//! The daemon: single-instance lock, listener, connections, workers and the janitor.

use std::collections::HashMap;
use std::fs::{self, File, TryLockError};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use interprocess::local_socket::traits::{Listener as _, Stream as _};
use interprocess::local_socket::{Listener, ListenerOptions, Stream};
use remora_engine::{Engine, EngineError, SessionRef};
use remora_ipc::auth::{self, Secret};
use remora_ipc::{
    Endpoint, ErrorBody, ErrorKind, FrameError, MAX_MESSAGE_BYTES, Outcome, PROTOCOL_VERSION, Request,
    RequestEnvelope, Response, ResponseEnvelope, endpoint, read_frame, write_frame,
};

use crate::clock::{Clock, SystemClock};
use crate::handler;
use crate::worker::{self, Context, Job, Reply, WorkerHandle};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How the daemon runs. [`DaemonConfig::new`] gives the production defaults.
#[derive(Clone)]
pub struct DaemonConfig {
    pub data_dir: PathBuf,
    pub clock: Arc<dyn Clock>,
    /// Turns with no event for this long are closed as interrupted (default 6 hours: a session
    /// that exits closes its own turn with `end_session`, so this only catches crashes and
    /// must not close a turn waiting on a long permission prompt).
    pub stale_turn_after: Duration,
    /// How often stale turns are looked for (default 1 minute).
    pub sweep_every: Duration,
    /// Connections with no request for this long are closed (default 30 seconds). Not
    /// enforced on Windows, whose named pipes have no read timeouts.
    pub idle_connection_after: Duration,
}

impl DaemonConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            clock: Arc::new(SystemClock),
            stale_turn_after: Duration::from_secs(6 * 60 * 60),
            sweep_every: Duration::from_secs(60),
            idle_connection_after: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("remorad is already running for {0}")]
    AlreadyRunning(PathBuf),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error("cannot listen on {endpoint}: {source}")]
    Listen {
        endpoint: String,
        #[source]
        source: io::Error,
    },
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Starts the daemon.
pub struct Daemon;

impl Daemon {
    /// Open the data directory, take the single-instance lock, listen, and start serving
    /// on background threads. Returns once the daemon accepts connections.
    pub fn start(config: DaemonConfig) -> Result<DaemonHandle, DaemonError> {
        let engine = Engine::open(&config.data_dir)?;
        let data_dir = engine.data_dir().to_path_buf();
        let instance = instance_lock(&data_dir)?;
        // A fresh secret for every run: a client must prove it can read this data directory.
        let secret = Secret::generate().map_err(|source| DaemonError::Io {
            path: data_dir.clone(),
            source,
        })?;
        secret.store(&data_dir).map_err(|source| DaemonError::Io {
            path: data_dir.join(auth::SECRET_FILE),
            source,
        })?;
        let endpoint = endpoint(&data_dir);
        let listener = listen(&endpoint)?;
        let shared = Arc::new(Shared {
            ctx: Arc::new(Context {
                engine,
                clock: config.clock,
                stale_after_ms: i64::try_from(config.stale_turn_after.as_millis()).unwrap_or(i64::MAX),
            }),
            data_dir,
            endpoint,
            idle_connection_after: config.idle_connection_after,
            workers: Mutex::new(Some(Workers::default())),
            worker_threads: Mutex::new(Vec::new()),
            stopping: Mutex::new(false),
            wake: Condvar::new(),
            threads: Mutex::new(Vec::new()),
            waker: Mutex::new(None),
            instance: Mutex::new(Some(instance)),
            secret,
            in_flight: Mutex::new(0),
            answered: Condvar::new(),
        });
        let accept = {
            let shared = Arc::clone(&shared);
            spawn_named("remora-accept", move || accept_loop(&listener, &shared))
        };
        let janitor = {
            let shared = Arc::clone(&shared);
            spawn_named("remora-janitor", move || {
                janitor_loop(&shared, config.sweep_every)
            })
        };
        let (Some(accept), Some(janitor)) = (accept, janitor) else {
            return Err(DaemonError::Io {
                path: shared.data_dir.clone(),
                source: io::Error::other("cannot start the daemon's threads"),
            });
        };
        lock(&shared.threads).extend([accept, janitor]);
        log::info!("remorad {VERSION} listening on {}", shared.endpoint);
        Ok(DaemonHandle { shared })
    }
}

/// Controls a running daemon. Clones control the same daemon.
#[derive(Clone)]
pub struct DaemonHandle {
    shared: Arc<Shared>,
}

impl DaemonHandle {
    /// The canonical data directory.
    pub fn data_dir(&self) -> &Path {
        &self.shared.data_dir
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.shared.endpoint
    }

    /// Stop accepting connections. Requests already running finish; [`wait`](Self::wait)
    /// returns when they have.
    pub fn shutdown(&self) {
        self.shared.shutdown();
    }

    /// Close stale turns in every open project now (the janitor does this periodically).
    pub fn sweep(&self) {
        self.shared.sweep();
    }

    /// Block until the daemon has shut down: no listener, no workers, lock released.
    /// Idle connections are not waited for; on Windows each keeps its pipe instance (and so
    /// the pipe name) until the client closes it or this process exits.
    pub fn wait(&self) {
        for thread in std::mem::take(&mut *lock(&self.shared.threads)) {
            let _ = thread.join();
        }
        drop(lock(&self.shared.waker).take());
        // Without handles no new jobs arrive; each worker finishes the jobs it has, then stops.
        drop(lock(&self.shared.workers).take());
        for thread in std::mem::take(&mut *lock(&self.shared.worker_threads)) {
            let _ = thread.join();
        }
        // The workers are done; let their answers reach the clients before returning.
        let in_flight = lock(&self.shared.in_flight);
        drop(
            self.shared
                .answered
                .wait_while(in_flight, |n| *n > 0)
                .unwrap_or_else(PoisonError::into_inner),
        );
        drop(lock(&self.shared.instance).take());
        log::info!("remorad stopped");
    }
}

#[derive(Default)]
struct Workers {
    /// By canonical project root.
    by_root: HashMap<PathBuf, WorkerHandle>,
    /// The root as clients spelled it, to its canonical form: a project folder the agent
    /// deleted can no longer be canonicalized, but its worker must still be found.
    aliases: HashMap<PathBuf, PathBuf>,
}

struct Shared {
    /// The secret clients prove they know (see `remora_ipc::auth`).
    secret: Secret,
    ctx: Arc<Context>,
    data_dir: PathBuf,
    endpoint: Endpoint,
    idle_connection_after: Duration,
    /// `None` once shutting down.
    workers: Mutex<Option<Workers>>,
    worker_threads: Mutex<Vec<JoinHandle<()>>>,
    stopping: Mutex<bool>,
    wake: Condvar,
    /// The accept loop and the janitor.
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// The connection that woke the accept loop at shutdown, open until the loop has ended.
    waker: Mutex<Option<Stream>>,
    instance: Mutex<Option<File>>,
    /// Requests read but not yet answered; `wait` returns only once this is zero.
    in_flight: Mutex<usize>,
    answered: Condvar,
}

impl Shared {
    fn is_stopping(&self) -> bool {
        *lock(&self.stopping)
    }

    fn shutdown(&self) {
        {
            let mut stopping = lock(&self.stopping);
            if *stopping {
                return;
            }
            *stopping = true;
        }
        self.wake.notify_all();
        log::info!("remorad shutting down");
        // Unblock the accept loop, which then sees `stopping`. The connection stays open until
        // `wait` has joined that loop: a Windows pipe listener skips clients that are already
        // gone and would keep waiting.
        if let Ok(name) = self.endpoint.name()
            && let Ok(stream) = Stream::connect(name)
        {
            *lock(&self.waker) = Some(stream);
        }
    }

    fn sweep(&self) {
        let workers: Vec<WorkerHandle> = match &*lock(&self.workers) {
            Some(workers) => workers.by_root.values().cloned().collect(),
            None => return,
        };
        for worker in workers {
            if let Err(e) = worker.run(Job::Sweep) {
                log::warn!("closing idle turns failed: {e}");
            }
        }
    }

    fn handle(&self, request: Request) -> Reply {
        match request {
            // The handshake is handled per connection, in `serve`.
            Request::Hello { .. } | Request::Prove { .. } => Err(ErrorBody::new(
                ErrorKind::InvalidRequest,
                "hello and prove are only valid at the start of a connection",
            )),
            Request::Shutdown => Ok(Response::ShuttingDown),
            Request::BeginTurn {
                root,
                session,
                agent,
                prompt,
            } => self.run(
                &root,
                Job::BeginTurn {
                    session: SessionRef::new(session, agent),
                    prompt,
                },
            ),
            Request::BeforeCommand {
                root,
                session,
                agent,
                command,
                tool_use_id,
                deadline_ms,
            } => self.run(
                &root,
                Job::BeforeCommand {
                    session: SessionRef::new(session, agent),
                    command,
                    tool_use_id,
                    deadline_ms,
                },
            ),
            Request::AfterCommand {
                root,
                session,
                agent,
                command,
                tool_use_id,
                exit_code,
            } => self.run(
                &root,
                Job::AfterCommand {
                    session: SessionRef::new(session, agent),
                    command,
                    tool_use_id,
                    exit_code,
                },
            ),
            Request::EndSession { root, session, agent } => self.run(
                &root,
                Job::EndSession {
                    session: SessionRef::new(session, agent),
                },
            ),
            Request::Preview { root, point } => self.run(&root, Job::Preview { point }),
            Request::RestoreSnapshot {
                root,
                snapshot,
                force,
            } => self.run(&root, Job::RestoreSnapshot { snapshot, force }),
            Request::EndTurn { root, session, agent } => self.run(
                &root,
                Job::EndTurn {
                    session: SessionRef::new(session, agent),
                },
            ),
            Request::RestorePoints { root, skip, limit } => {
                self.run(&root, Job::RestorePoints { skip, limit })
            }
            Request::Rewind { root, point, force } => self.run(&root, Job::Rewind { point, force }),
            Request::Status { root } => self.run(&root, Job::Status),
            Request::Verify { root } => self.run(&root, Job::Verify),
        }
    }

    fn run(&self, root: &Path, job: Job) -> Reply {
        self.worker(root)?.run(job)
    }

    /// The worker for the project at `root`, started on first use.
    fn worker(&self, root: &Path) -> Result<WorkerHandle, ErrorBody> {
        if !root.is_absolute() {
            return Err(ErrorBody::new(
                ErrorKind::InvalidRequest,
                format!(
                    "the project folder must be an absolute path, not {}",
                    root.display()
                ),
            ));
        }
        let mut guard = lock(&self.workers);
        let Some(workers) = guard.as_mut() else {
            return Err(ErrorBody::new(ErrorKind::Internal, "remorad is shutting down"));
        };
        let key = match root.canonicalize() {
            Ok(canonical) => {
                workers.aliases.insert(root.to_path_buf(), canonical.clone());
                canonical
            }
            Err(_) => workers
                .aliases
                .get(root)
                .cloned()
                .unwrap_or_else(|| root.to_path_buf()),
        };
        if let Some(worker) = workers.by_root.get(&key) {
            return Ok(worker.clone());
        }
        let (worker, thread) =
            worker::spawn(key.clone(), Arc::clone(&self.ctx), handler::execute).map_err(|e| {
                ErrorBody::new(ErrorKind::Internal, format!("cannot start a project worker: {e}"))
            })?;
        lock(&self.worker_threads).push(thread);
        workers.by_root.insert(key, worker.clone());
        Ok(worker)
    }
}

fn version_mismatch(detail: &str) -> ErrorBody {
    ErrorBody::new(
        ErrorKind::VersionMismatch,
        format!(
            "{detail}, this daemon ({VERSION}) speaks protocol {PROTOCOL_VERSION}: \
             run `remorad install` again or restart the daemon"
        ),
    )
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic elsewhere never leaves these structures half-updated.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn spawn_named(name: &str, f: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(f)
        .inspect_err(|e| log::error!("cannot start thread {name}: {e}"))
        .ok()
}

/// Only one daemon per data directory: an exclusive lock on `remorad.lock`, held while
/// the daemon runs and released by the OS if it dies.
fn instance_lock(data_dir: &Path) -> Result<File, DaemonError> {
    let path = data_dir.join("remorad.lock");
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(|source| DaemonError::Io {
        path: path.clone(),
        source,
    })?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(DaemonError::AlreadyRunning(data_dir.to_path_buf())),
        Err(TryLockError::Error(source)) => Err(DaemonError::Io { path, source }),
    }
}

fn listen(endpoint: &Endpoint) -> Result<Listener, DaemonError> {
    let failed = |source| DaemonError::Listen {
        endpoint: endpoint.to_string(),
        source,
    };
    if let Endpoint::Socket(path) = endpoint {
        // The instance lock is held, so a socket file here was left by a dead daemon.
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(failed(e)),
        }
    }
    let options = ListenerOptions::new().name(endpoint.name().map_err(failed)?);
    #[cfg(windows)]
    let options = {
        use interprocess::os::windows::local_socket::ListenerOptionsExt;
        options.security_descriptor(owner_only().map_err(failed)?)
    };
    let listener = options.create_sync().map_err(failed)?;
    #[cfg(unix)]
    if let Endpoint::Socket(path) = endpoint {
        // Inside the owner-only data directory already; the file mode is a second fence.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(failed)?;
    }
    Ok(listener)
}

/// A protected DACL granting access to the pipe's owner (the user running the daemon) and
/// SYSTEM only. Without it, named pipes are readable by everyone.
#[cfg(windows)]
fn owner_only() -> io::Result<interprocess::os::windows::security_descriptor::SecurityDescriptor> {
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    let sddl = widestring::U16CString::from_str("D:P(A;;GA;;;OW)(A;;GA;;;SY)")
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    SecurityDescriptor::deserialize(&sddl)
}

fn accept_loop(listener: &Listener, shared: &Arc<Shared>) {
    loop {
        let accepted = listener.accept();
        if shared.is_stopping() {
            return;
        }
        match accepted {
            Ok(stream) => {
                let shared = Arc::clone(shared);
                spawn_named("remora-connection", move || serve(stream, &shared));
            }
            Err(e) => {
                log::warn!("accepting a connection failed: {e}");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn janitor_loop(shared: &Shared, every: Duration) {
    loop {
        let stopping = lock(&shared.stopping);
        let (stopping, _) = shared
            .wake
            .wait_timeout_while(stopping, every, |stopping| !*stopping)
            .unwrap_or_else(PoisonError::into_inner);
        if *stopping {
            return;
        }
        drop(stopping);
        shared.sweep();
    }
}

/// Answer one client's requests, in order, until it disconnects, goes idle or sends
/// something unreadable.
fn serve(stream: Stream, shared: &Shared) {
    // Named pipes do not support read timeouts; there, idle connections stay open.
    let _ = stream.set_recv_timeout(Some(shared.idle_connection_after));
    let mut conn = BufReader::new(stream);
    // The challenge this daemon sent in its hello, and whether the client answered it.
    let mut challenge: Option<String> = None;
    let mut authenticated = false;
    loop {
        let line = match read_frame(&mut conn) {
            Ok(Some(line)) => line,
            Ok(None) | Err(FrameError::Io(_)) => return,
            Err(e) => {
                let _ = answer(
                    &mut conn,
                    0,
                    Err(ErrorBody::new(ErrorKind::InvalidRequest, e.to_string())),
                );
                return;
            }
        };
        let envelope = match serde_json::from_str::<RequestEnvelope>(&line) {
            Ok(envelope) => envelope,
            Err(e) => {
                let error = ErrorBody::new(ErrorKind::InvalidRequest, format!("unreadable request: {e}"));
                let _ = answer(&mut conn, request_id(&line), Err(error));
                return;
            }
        };
        if envelope.v != PROTOCOL_VERSION {
            let error = version_mismatch(&format!("the request uses protocol {}", envelope.v));
            let _ = answer(&mut conn, envelope.id, Err(error));
            return;
        }
        match &envelope.req {
            Request::Hello { nonce, .. } => {
                let server_nonce = match auth::nonce() {
                    Ok(nonce) => nonce,
                    Err(e) => {
                        let error = ErrorBody::new(ErrorKind::Internal, format!("no randomness: {e}"));
                        let _ = answer(&mut conn, envelope.id, Err(error));
                        return;
                    }
                };
                let hello = Response::Hello {
                    daemon_version: VERSION.to_owned(),
                    protocol: PROTOCOL_VERSION,
                    nonce: server_nonce.clone(),
                    proof: shared.secret.server_proof(nonce),
                };
                challenge = Some(server_nonce);
                if answer(&mut conn, envelope.id, Ok(hello)).is_err() {
                    return;
                }
                continue;
            }
            Request::Prove { proof } => {
                let valid = challenge
                    .as_deref()
                    .is_some_and(|challenge| shared.secret.verify_client(challenge, proof));
                if !valid {
                    let error = ErrorBody::new(ErrorKind::Unauthenticated, "the proof does not match");
                    let _ = answer(&mut conn, envelope.id, Err(error));
                    log::warn!("refused a connection: the proof does not match");
                    return;
                }
                authenticated = true;
                if answer(&mut conn, envelope.id, Ok(Response::Authenticated)).is_err() {
                    return;
                }
                continue;
            }
            _ if !authenticated => {
                let error = ErrorBody::new(
                    ErrorKind::Unauthenticated,
                    "authenticate first: send hello, then prove",
                );
                let _ = answer(&mut conn, envelope.id, Err(error));
                log::warn!("refused a connection: a request before authenticating");
                return;
            }
            _ => {}
        }
        let shutdown = matches!(envelope.req, Request::Shutdown);
        let in_flight = InFlight::start(shared);
        let reply = shared.handle(envelope.req);
        let answered = answer(&mut conn, envelope.id, reply);
        drop(in_flight);
        // Stop even if the requester left without reading the answer.
        if shutdown {
            shared.shutdown();
            return;
        }
        if answered.is_err() {
            return;
        }
    }
}

/// Counts one request from the moment it is handled until its answer is written.
struct InFlight<'a>(&'a Shared);

impl<'a> InFlight<'a> {
    fn start(shared: &'a Shared) -> Self {
        *lock(&shared.in_flight) += 1;
        Self(shared)
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        *lock(&self.0.in_flight) -= 1;
        self.0.answered.notify_all();
    }
}

/// Send one answer; every error answer is logged.
fn answer(conn: &mut BufReader<Stream>, id: u64, reply: Reply) -> Result<(), FrameError> {
    if let Err(e) = &reply {
        log::warn!("request #{id} failed: {e}");
    }
    let outcome = match reply {
        Ok(response) => Outcome::Ok(response),
        Err(error) => Outcome::Err(error),
    };
    let envelope = |outcome| ResponseEnvelope {
        v: PROTOCOL_VERSION,
        id,
        outcome,
    };
    match write_frame(conn.get_mut(), &envelope(outcome)) {
        // Never close silently: say why there is no answer.
        Err(FrameError::TooLarge) => {
            log::error!("the answer to request #{id} is larger than {MAX_MESSAGE_BYTES} bytes");
            let error = ErrorBody::new(
                ErrorKind::Internal,
                format!("the answer is larger than the {MAX_MESSAGE_BYTES}-byte message limit"),
            );
            write_frame(conn.get_mut(), &envelope(Outcome::Err(error)))
        }
        written => written,
    }
}

/// The `id` of a request that could not be fully read, if it has one.
fn request_id(line: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| v.get("id").and_then(serde_json::Value::as_u64))
        .unwrap_or(0)
}
