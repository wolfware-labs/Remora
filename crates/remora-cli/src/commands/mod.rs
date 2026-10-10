//! The `remora` commands people run. Each prints its result and returns a [`CliError`]
//! carrying the exit code on failure.

pub mod forget;
pub mod log;
pub mod prune;
pub mod rewind;
pub mod service;
pub mod setup;
pub mod status;
pub mod verify;

use std::path::{Path, PathBuf};
use std::time::Duration;

use remora_engine::{PointKind, RestorePoint};
use remora_ipc::{Client, ClientError, ErrorKind, Request, Response};

use crate::root::{self, Root};

/// Exit codes.
pub const EXIT_ERROR: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_REFUSED: u8 = 3;
pub const EXIT_NO_DAEMON: u8 = 4;

/// A failed command: what to tell the user, and the exit code.
#[derive(Debug)]
pub struct CliError {
    pub code: u8,
    pub message: String,
}

impl CliError {
    pub fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<ClientError> for CliError {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::DaemonNotRunning => CliError::new(
                EXIT_NO_DAEMON,
                "the Remora daemon is not running: run `remora service install` (or `remorad run`)",
            ),
            ClientError::Impostor { endpoint } => CliError::new(
                EXIT_NO_DAEMON,
                format!(
                    "something other than your Remora daemon answers on {endpoint}: stop it, then run `remora service install`"
                ),
            ),
            e @ ClientError::UnsafeSocketDir(_) => CliError::new(EXIT_NO_DAEMON, e.to_string()),
            ClientError::Daemon(body) if matches!(body.kind, ErrorKind::TurnInProgress { .. }) => {
                CliError::new(EXIT_REFUSED, body.message)
            }
            other => CliError::new(EXIT_ERROR, other.to_string()),
        }
    }
}

/// Settings every command shares.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub data_dir: PathBuf,
}

impl Ctx {
    pub fn client(&self) -> Result<Client, CliError> {
        Ok(Client::connect_with_timeout(
            &self.data_dir,
            Duration::from_secs(120),
        )?)
    }

    /// The version of the daemon answering for this data directory, if one does.
    pub fn daemon_version(&self) -> Option<String> {
        self.client()
            .ok()
            .map(|client| client.daemon_version().to_owned())
    }
}

/// The project a command works on: `--root`, else the git repository around the current
/// directory.
pub fn project_root(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    let cwd = std::env::current_dir().map_err(|e| CliError::new(EXIT_ERROR, e.to_string()))?;
    if let Some(root) = explicit {
        // `--root .` or a subfolder means its repository, like the hooks see it.
        let absolute = cwd.join(root);
        return Ok(match root::resolve(&absolute) {
            Root::Repo(root) | Root::Ignored(root) => root,
            Root::NotInGit => absolute,
        });
    }
    match root::resolve(&cwd) {
        Root::Repo(root) | Root::Ignored(root) => Ok(root),
        Root::NotInGit => Err(CliError::new(
            EXIT_USAGE,
            "not inside a git repository: run this in a project, or pass --root <dir>",
        )),
    }
}

/// One page of restore points, newest first.
pub fn points_page(
    client: &mut Client,
    root: &Path,
    skip: usize,
    limit: usize,
) -> Result<(Vec<RestorePoint>, usize), CliError> {
    match client.call(Request::RestorePoints {
        root: root.to_path_buf(),
        skip,
        limit: Some(limit),
    })? {
        Response::RestorePoints { points, total } => Ok((points, total)),
        other => Err(unexpected(&other)),
    }
}

pub fn unexpected(response: &Response) -> CliError {
    CliError::new(
        EXIT_ERROR,
        format!("unexpected answer from the daemon: {response:?}"),
    )
}

/// Milliseconds since the Unix epoch, now.
pub fn now_ms() -> i64 {
    use remora_daemon::Clock;
    remora_daemon::SystemClock.now_ms()
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(now_ms: i64, at_ms: i64) -> String {
    let secs = (now_ms - at_ms).max(0) / 1_000;
    match secs {
        0..60 => "just now".to_owned(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

pub fn kind_name(kind: PointKind) -> &'static str {
    match kind {
        PointKind::TurnStart => "turn",
        PointKind::BeforeCommand => "before command",
        PointKind::BeforeRewind => "before rewind",
    }
}

/// A point's kind for people, with "(late)" when its checkpoint finished after the agent
/// stopped waiting and "(pruned)" when retention removed its files.
pub fn point_kind(point: &RestorePoint) -> String {
    let mut kind = kind_name(point.kind).to_owned();
    if point.late {
        kind.push_str(" (late)");
    }
    if point.pruned_at_ms.is_some() {
        kind.push_str(" (pruned)");
    }
    kind
}

/// A label on one line, at most `max` characters.
pub fn one_line(label: &str, max: usize) -> String {
    let flat: String = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// The first characters of a session id.
pub fn short_session(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Open sessions as the restore points show them: id, start, latest event and label.
pub fn open_sessions(points: &[RestorePoint]) -> Vec<(String, i64, i64, Option<String>)> {
    let mut sessions: Vec<(String, i64, i64, Option<String>)> = Vec::new();
    // Points are newest first: the first one seen per session is its latest event.
    for point in points.iter().filter(|p| p.open) {
        match sessions.iter_mut().find(|s| s.0 == point.session_id) {
            Some(session) => session.1 = session.1.min(point.at_ms),
            None => sessions.push((
                point.session_id.clone(),
                point.at_ms,
                point.at_ms,
                point.label.clone(),
            )),
        }
    }
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_kind_says_late_and_pruned() {
        let mut point = RestorePoint {
            position: 0,
            snapshot: remora_snap::SnapshotId::parse(&"a".repeat(64)).unwrap(),
            kind: PointKind::BeforeCommand,
            session_id: "s1".into(),
            at_ms: 0,
            label: None,
            open: false,
            late: false,
            pruned_at_ms: None,
        };
        assert_eq!(point_kind(&point), "before command");
        point.late = true;
        point.pruned_at_ms = Some(1);
        assert_eq!(point_kind(&point), "before command (late) (pruned)");
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(ago(100_000, 99_000), "just now");
        assert_eq!(ago(10 * 60_000, 0), "10 min ago");
        assert_eq!(ago(3 * 3_600_000, 0), "3 h ago");
        assert_eq!(ago(2 * 86_400_000, 0), "2 days ago");
        assert_eq!(
            ago(0, 5_000),
            "just now",
            "a clock behind the record is not negative"
        );
    }

    #[test]
    fn labels_fit_on_one_line() {
        assert_eq!(one_line("rm -rf\n  build", 40), "rm -rf build");
        assert_eq!(one_line("abcdef", 4), "abc…");
    }
}
