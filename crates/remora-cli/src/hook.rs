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
use remora_ipc::{Client, ClientError, ErrorKind, Request};

use crate::claude::{self, AGENT, HookEvent};
use crate::root::{self, Root};

/// The most hook input read from stdin.
pub const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;
/// How long a snapshotting hook waits for its answer.
/// Claude Code waits 30 s for these hooks (the timeout `remora init` writes); with up to two
/// daemon starts of 2 s each, 24 s keeps the whole hook inside it. Giving up earlier would let
/// the command run before its restore point exists.
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
    let request = to_request(event, root)?;
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
    let answer = connect(env, timeout, deadline).and_then(|client| {
        let left = deadline.saturating_duration_since(Instant::now());
        client
            .with_timeout(timeout.min(left).max(Duration::from_millis(10)))
            .call(request)
    });
    match answer {
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
    }
}

fn system_message(text: &str) -> String {
    serde_json::json!({ "systemMessage": text }).to_string()
}

/// Connect, starting a daemon if none answers (and one more if the first is not up within
/// `start_wait`). Never waits past `deadline`.
fn connect(env: &HookEnv, timeout: Duration, deadline: Instant) -> Result<Client, ClientError> {
    let wait = |limit: Duration| {
        let left = deadline.saturating_duration_since(Instant::now());
        limit.min(left).max(Duration::from_millis(10))
    };
    match Client::connect_with_timeout(&env.data_dir, wait(timeout)) {
        Err(ClientError::DaemonNotRunning) => {}
        connected => return connected,
    }
    let Some(remora) = &env.remora else {
        return Err(ClientError::DaemonNotRunning);
    };
    for _ in 0..2 {
        start_daemon(remora, &env.data_dir)?;
        let started_by = (Instant::now() + env.start_wait).min(deadline);
        loop {
            match Client::connect_with_timeout(&env.data_dir, wait(timeout)) {
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

/// Start `remora daemon` for `data_dir`, detached so it outlives this hook.
fn start_daemon(remora: &Path, data_dir: &Path) -> Result<(), ClientError> {
    // Not the agent's folder, which the agent may delete.
    let cwd = if data_dir.is_dir() {
        data_dir.to_path_buf()
    } else {
        std::env::temp_dir()
    };
    let data_dir = data_dir.to_string_lossy();
    remora_daemon::spawn_detached(remora, &["--data-dir", &data_dir, "daemon"], &cwd).map_err(ClientError::Io)
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
