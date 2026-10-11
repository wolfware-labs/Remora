//! `remora hook claude-code`: forward one Claude Code hook event to the daemon.
//!
//! It never blocks or changes what the agent does. Every failure is logged to
//! `<data_dir>/hook.log`; when it means a session is not being recorded, Claude Code shows a
//! warning once per session and reason. The process always exits 0.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use remora_daemon::{LOG_ROTATE_BYTES, LogFile};
use remora_ipc::{Client, ClientError, ErrorKind, Request, Response};

use crate::claude::{self, AGENT, HookEvent};
use crate::root::{self, Root};
use crate::upgrade;

/// The most hook input read from stdin.
pub const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;
/// How long a snapshotting hook waits for its answer.
/// Claude Code waits 30 s for these hooks (the timeout `remora init` writes). The whole hook
/// (replacing an older daemon, up to two daemon starts, and the call) is held to
/// `2 * start_wait + 24 s`, 28 s with the 2 s `remora hook` uses, which leaves Claude Code
/// headroom. Giving up earlier would let the command run before its restore point exists.
pub const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(24);
/// How long any other hook waits for its answer.
pub const QUICK_TIMEOUT: Duration = Duration::from_secs(2);
/// What a session's marker holds when its turn started where nothing is recorded.
const NOT_RECORDED: &str = "-";
/// Session and warning markers older than this are removed.
const MARKER_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Where a hook runs.
#[derive(Debug, Clone)]
pub struct HookEnv {
    pub data_dir: PathBuf,
    /// The `remora` executable, started as `remora daemon` when no daemon answers.
    /// `None` never starts one.
    pub remora: Option<PathBuf>,
    /// How long to wait for a daemon that was just started.
    pub start_wait: Duration,
    /// How long replacing an older daemon may take, apart from `start_wait`: when it runs
    /// out, the hook goes on with whatever daemon it can reach or start.
    pub replace_wait: Duration,
    /// This `remora`'s version: an older daemon is replaced (see `upgrade`).
    pub version: String,
}

/// Handle one hook call. Returns what to print on stdout: a `systemMessage` for Claude
/// Code, or nothing.
pub fn run(input: &[u8], env: &HookEnv) -> Option<String> {
    let log = LogFile::open_named(&env.data_dir, "hook.log", LOG_ROTATE_BYTES).ok();
    let note = |message: &str| {
        if let Some(log) = &log {
            let _ = log.write_entry("WARN", message);
        }
    };
    let input = &input[..input.len().min(MAX_INPUT_BYTES)];
    let event = match claude::parse(&String::from_utf8_lossy(input)) {
        Ok(event) => event,
        Err(e) => {
            note(&e.to_string());
            return None;
        }
    };
    let (session, cwd) = match &event {
        HookEvent::Ignored => return None,
        HookEvent::PromptSubmitted { session, cwd, .. }
        | HookEvent::BeforeBash { session, cwd, .. }
        | HookEvent::AfterBash { session, cwd, .. }
        | HookEvent::TurnEnded { session, cwd }
        | HookEvent::SessionEnded { session, cwd } => (session.clone(), cwd.clone()),
    };
    let sessions = Markers::new(&env.data_dir, "hook-sessions");
    let root = match &event {
        // A turn records into the repository it started in, even if the agent later
        // changes directory or deletes the repository.
        HookEvent::PromptSubmitted { .. } => match root::resolve(&cwd) {
            Root::Repo(root) => {
                sessions.write(&session, &root.to_string_lossy());
                root
            }
            // Remember this too: a later `cd` into another repository must not start
            // recording a turn that began where the user records nothing.
            Root::Ignored(_) | Root::NotInGit => {
                sessions.write(&session, NOT_RECORDED);
                return None;
            }
        },
        _ => match sessions.read(&session).as_deref() {
            Some(NOT_RECORDED) => return None,
            Some(root) if Path::new(root).is_absolute() => PathBuf::from(root),
            _ => match root::resolve(&cwd) {
                Root::Repo(root) => root,
                Root::Ignored(_) | Root::NotInGit => return None,
            },
        },
    };
    let snapshots = matches!(
        event,
        HookEvent::PromptSubmitted { .. } | HookEvent::BeforeBash { .. }
    );
    let session_ends = matches!(event, HookEvent::SessionEnded { .. });
    if session_ends {
        sessions.remove(&session);
    }
    // Starting a daemon when a session ends only makes Claude Code wait on exit.
    let env = &HookEnv {
        remora: if session_ends { None } else { env.remora.clone() },
        ..env.clone()
    };
    let timeout = if snapshots {
        SNAPSHOT_TIMEOUT
    } else {
        QUICK_TIMEOUT
    };
    // One budget for the whole hook: connecting, starting a daemon (twice at most), and
    // the call itself.
    let deadline = Instant::now() + 2 * env.start_wait + timeout;
    let request = to_request(event, root)?;
    let call = |client: Client| {
        let left = deadline.saturating_duration_since(Instant::now());
        let wait = timeout.min(left).max(Duration::from_millis(10));
        // The daemon marks the checkpoint late if it is ready only after this hook stopped waiting.
        client
            .with_timeout(wait)
            .call(with_deadline(request.clone(), epoch_ms(SystemTime::now() + wait)))
    };
    let answer = connect(env, timeout, deadline, &note).and_then(|(client, stopping)| {
        match call(client) {
            // The daemon this hook asked to stop answered the fallback, then stopped: it did
            // not run the request, which goes to the daemon after it.
            Err(e) if stopping && stopped(&e) => {
                note(&format!(
                    "the daemon stopped under this hook ({e}); trying the next one"
                ));
                connect_after_stop(env, timeout, deadline).and_then(call)
            }
            answer => answer,
        }
    });
    match answer {
        // Something the user should hear once per session (an interrupted rewind that was
        // rolled back).
        Ok(Response::Snapshot {
            notice: Some(notice), ..
        }) => {
            note(&format!("session {session}: {notice}"));
            Markers::new(&env.data_dir, "hook-warnings")
                .first(&format!("{session}.notice"))
                .then(|| system_message(&format!("Remora: {notice}")))
        }
        Ok(_) => None,
        Err(e) => {
            note(&format!("session {session}: {e}"));
            // Claude Code discards what a SessionEnd hook prints: log it, keep the
            // once-per-session warning for a hook whose message is seen.
            if session_ends {
                return None;
            }
            let (reason, detail) = describe(&e);
            Markers::new(&env.data_dir, "hook-warnings")
                .first(&format!("{session}.{reason}"))
                .then(|| {
                    system_message(&format!(
                        "Remora is not recording this session: {detail}. Run `remora status`."
                    ))
                })
        }
    }
}

/// The daemon request for `event`; `None` for events Remora does not record.
fn to_request(event: HookEvent, root: PathBuf) -> Option<Request> {
    let agent = AGENT.to_owned();
    Some(match event {
        HookEvent::PromptSubmitted { session, prompt, .. } => Request::BeginTurn {
            root,
            session,
            agent,
            prompt: Some(prompt).filter(|p| !p.is_empty()),
            deadline_ms: None,
        },
        HookEvent::BeforeBash {
            session,
            command,
            tool_use_id,
            ..
        } => Request::BeforeCommand {
            root,
            session,
            agent,
            command,
            tool_use_id,
            deadline_ms: None,
        },
        HookEvent::AfterBash {
            session,
            command,
            tool_use_id,
            exit_code,
            ..
        } => Request::AfterCommand {
            root,
            session,
            agent,
            command,
            tool_use_id,
            exit_code,
        },
        HookEvent::TurnEnded { session, .. } => Request::EndTurn { root, session, agent },
        HookEvent::SessionEnded { session, .. } => Request::EndSession { root, session, agent },
        HookEvent::Ignored => return None,
    })
}

/// Milliseconds since the Unix epoch, as the daemon's clock counts them.
fn epoch_ms(at: SystemTime) -> i64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// `request` with the time this hook stops waiting for it, for requests that take one:
/// a snapshot the daemon finishes later may already contain the agent's changes, so the
/// daemon marks it late.
fn with_deadline(mut request: Request, at_ms: i64) -> Request {
    if let Request::BeginTurn { deadline_ms, .. } | Request::BeforeCommand { deadline_ms, .. } = &mut request
    {
        *deadline_ms = Some(at_ms);
    }
    request
}

/// A short reason key (for the once-per-session marker) and a sentence for people.
fn describe(error: &ClientError) -> (&'static str, String) {
    match error {
        ClientError::DaemonNotRunning | ClientError::Io(_) | ClientError::Disconnected => (
            "daemon",
            "the Remora daemon is not running and could not be started".to_owned(),
        ),
        ClientError::Timeout(_) => (
            "timeout",
            "the Remora daemon did not answer in time (a first snapshot of a large project can be slow), so this restore point may be missing or late"
                .to_owned(),
        ),
        ClientError::Daemon(body) if body.kind == ErrorKind::VersionMismatch => (
            "version",
            "this remora and the running daemon are different versions (run `remora service install`)"
                .to_owned(),
        ),
        ClientError::Daemon(body) => ("daemon-error", body.message.clone()),
        ClientError::Protocol(detail) => ("protocol", detail.clone()),
        ClientError::Impostor { endpoint } => (
            "impostor",
            format!("something other than your Remora daemon answers on {endpoint}"),
        ),
        ClientError::UnsafeSocketDir(dir) => (
            "unsafe-socket-dir",
            format!(
                "the folder of the daemon's socket, {}, is not yours alone",
                dir.display()
            ),
        ),
    }
}

fn system_message(text: &str) -> String {
    serde_json::json!({ "systemMessage": text }).to_string()
}

/// Connect, starting a daemon if none answers (and one more if the first is not up within
/// `start_wait`). Replacing an older daemon spends from the same `deadline`, at most
/// `replace_wait`. Never waits past `deadline`. Also returns whether the client's daemon may
/// be one this hook asked to stop (a replacement that failed).
fn connect(
    env: &HookEnv,
    timeout: Duration,
    deadline: Instant,
    note: &dyn Fn(&str),
) -> Result<(Client, bool), ClientError> {
    match (
        Client::connect_with_timeout(&env.data_dir, bounded(timeout, deadline)),
        &env.remora,
    ) {
        (Err(ClientError::DaemonNotRunning), _) => start(env, timeout, deadline).map(|c| (c, false)),
        // After an upgrade: this version replaces the daemon, once. If that fails, the older
        // daemon may be stopping: whatever answers once it is gone serves this call.
        (Ok(client), Some(remora)) => match upgrade::replace_older(
            client,
            &env.data_dir,
            remora,
            &env.version,
            // Exactly this much, even none: no floor.
            env.replace_wait
                .min(deadline.saturating_duration_since(Instant::now())),
        ) {
            Ok(client) => Ok((client, false)),
            Err(e) => {
                note(&format!("replacing the older daemon failed: {e}"));
                connect_after_stop(env, timeout, deadline).map(|c| (c, true))
            }
        },
        (connected, _) => connected.map(|c| (c, false)),
    }
}

/// Connect after asking a daemon to stop: to one that answers, or, once no daemon holds the
/// data folder any more, to one this hook starts. A daemon that is stopping answers no new
/// connection but holds the folder until it is gone. Never waits past `deadline`.
fn connect_after_stop(env: &HookEnv, timeout: Duration, deadline: Instant) -> Result<Client, ClientError> {
    loop {
        match Client::connect_with_timeout(&env.data_dir, bounded(timeout, deadline)) {
            Err(ClientError::DaemonNotRunning) if remora_ipc::instance_lock_is_free(&env.data_dir) => {
                return start(env, timeout, deadline);
            }
            Err(ClientError::DaemonNotRunning) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            connected => return connected,
        }
    }
}

/// Start a daemon and connect to it (one more start if the first is not up within
/// `start_wait`). Never waits past `deadline`.
fn start(env: &HookEnv, timeout: Duration, deadline: Instant) -> Result<Client, ClientError> {
    let Some(remora) = &env.remora else {
        return Err(ClientError::DaemonNotRunning);
    };
    for _ in 0..2 {
        if Instant::now() >= deadline {
            break;
        }
        upgrade::start_daemon(
            remora,
            &env.data_dir,
            deadline.saturating_duration_since(Instant::now()),
        )?;
        let started_by = (Instant::now() + env.start_wait).min(deadline);
        loop {
            match Client::connect_with_timeout(&env.data_dir, bounded(timeout, deadline)) {
                Err(ClientError::DaemonNotRunning) if Instant::now() < started_by => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(ClientError::DaemonNotRunning) => break,
                connected => return connected,
            }
        }
    }
    Err(ClientError::DaemonNotRunning)
}

/// `limit`, but not past `deadline` (and never quite zero, which some waits read as forever).
fn bounded(limit: Duration, deadline: Instant) -> Duration {
    let left = deadline.saturating_duration_since(Instant::now());
    limit.min(left).max(Duration::from_millis(10))
}

/// Whether `error` means the daemon stopped before it ran the request: it closed or broke the
/// connection (a stopping daemon answers every request it read), or refused the request
/// because it is shutting down.
fn stopped(error: &ClientError) -> bool {
    use std::io::ErrorKind as Io;
    match error {
        ClientError::Disconnected => true,
        ClientError::Io(e) => matches!(
            e.kind(),
            Io::BrokenPipe | Io::ConnectionReset | Io::ConnectionAborted | Io::UnexpectedEof
        ),
        ClientError::Daemon(body) => {
            body.kind == ErrorKind::Internal && body.message == remora_daemon::SHUTTING_DOWN
        }
        _ => false,
    }
}

/// Small files under the data directory, one per session (and reason).
struct Markers {
    dir: PathBuf,
}

impl Markers {
    fn new(data_dir: &Path, name: &str) -> Self {
        Self {
            dir: data_dir.join(name),
        }
    }

    /// Session ids become file names: only plain ones are used.
    fn path(&self, key: &str) -> Option<PathBuf> {
        let plain = !key.is_empty()
            && key.len() <= 200
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.');
        (plain && !key.starts_with('.')).then(|| self.dir.join(key))
    }

    fn read(&self, key: &str) -> Option<String> {
        fs::read_to_string(self.path(key)?)
            .ok()
            .filter(|value| !value.is_empty())
    }

    fn write(&self, key: &str, value: &str) {
        if let Some(path) = self.path(key)
            && private_dir(&self.dir).is_ok()
        {
            let _ = fs::write(path, value);
            self.prune();
        }
    }

    fn remove(&self, key: &str) {
        if let Some(path) = self.path(key) {
            let _ = fs::remove_file(path);
        }
    }

    /// True the first time it is called for `key` (until the marker is pruned). False when
    /// the key cannot be remembered: warning on every event would be worse than not at all.
    fn first(&self, key: &str) -> bool {
        let Some(path) = self.path(key) else {
            return false;
        };
        if private_dir(&self.dir).is_err() {
            return false;
        }
        self.prune();
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .is_ok()
    }

    fn prune(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > MARKER_MAX_AGE);
            if old {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_deadline_sets_deadline_ms_on_snapshotting_requests_and_leaves_others_unchanged() {
        let path = PathBuf::from("/tmp");
        let before_cmd = Request::BeforeCommand {
            root: path.clone(),
            session: "s1".into(),
            agent: "test".into(),
            command: "make".into(),
            tool_use_id: None,
            deadline_ms: None,
        };
        let deadline = 1_000_000_000_i64;
        let updated = with_deadline(before_cmd.clone(), deadline);
        #[allow(clippy::unwrap_used)]
        if let Request::BeforeCommand { deadline_ms, .. } = updated {
            assert_eq!(deadline_ms, Some(deadline));
        } else {
            panic!("expected BeforeCommand");
        }

        let begin_turn = Request::BeginTurn {
            root: path.clone(),
            session: "s1".into(),
            agent: "test".into(),
            prompt: None,
            deadline_ms: None,
        };
        let Request::BeginTurn { deadline_ms, .. } = with_deadline(begin_turn, deadline) else {
            panic!("expected BeginTurn");
        };
        assert_eq!(deadline_ms, Some(deadline));

        let end_turn = Request::EndTurn {
            root: path,
            session: "s1".into(),
            agent: "test".into(),
        };
        let unchanged = with_deadline(end_turn, deadline);
        #[allow(clippy::unwrap_used)]
        if let Request::EndTurn { .. } = unchanged {
            // EndTurn should be unchanged
        } else {
            panic!("expected EndTurn");
        }
    }
}
