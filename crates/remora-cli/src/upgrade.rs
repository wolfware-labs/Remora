//! After an upgrade the previous `remorad` keeps running. The first newer `remora` (a hook or
//! a command) that reaches it stops it and starts its own version, once. Open turns survive:
//! they are on disk.

use std::ffi::OsStr;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use remora_ipc::{Client, ClientError};

/// This `remora`'s version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whether a daemon reporting `daemon` is an older release than `ours`. Versions compare by
/// their `major.minor.patch`; one that does not parse is never older (it is left alone).
pub fn is_older(daemon: &str, ours: &str) -> bool {
    fn parse(version: &str) -> Option<(u64, u64, u64)> {
        let core = version.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
        let version = (parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }
    matches!((parse(daemon), parse(ours)), (Some(daemon), Some(ours)) if daemon < ours)
}

/// Start `remora daemon` for `data_dir`, detached so it outlives this process, taking at
/// most `limit` to do so.
pub fn start_daemon(remora: &Path, data_dir: &Path, limit: Duration) -> Result<(), ClientError> {
    // Not the agent's folder, which the agent may delete.
    let cwd = if data_dir.is_dir() {
        data_dir.to_path_buf()
    } else {
        std::env::temp_dir()
    };
    let args = [
        OsStr::new("--data-dir"),
        data_dir.as_os_str(),
        OsStr::new("daemon"),
    ];
    remora_daemon::spawn_detached(remora, &args, &cwd, limit).map_err(ClientError::Io)
}

/// `client`, unless its daemon is older than `ours`: then that very daemon (never one that
/// replaced it meanwhile) is asked to stop and this `remora` starts its own, and the client
/// of the new one is returned. A daemon that does not stop, or a new one that does not answer
/// within `wait`, leaves the error; the stop gets at most half of `wait`. Without that time
/// the daemon is not asked to stop at all: it goes on running, and the result is an error.
pub fn replace_older(
    client: Client,
    data_dir: &Path,
    remora: &Path,
    ours: &str,
    wait: Duration,
) -> Result<Client, ClientError> {
    if !is_older(client.daemon_version(), ours) {
        return Ok(client);
    }
    if (wait / 2).is_zero() {
        return Err(ClientError::Timeout(wait));
    }
    let deadline = Instant::now() + wait;
    client.stop(data_dir, wait / 2)?;
    let mut started = false;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match Client::connect_with_timeout(data_dir, left.max(Duration::from_millis(10))) {
            Err(ClientError::DaemonNotRunning) if !left.is_zero() => {
                // Another remora may have started one by now; only start when none answers.
                if !started {
                    started = true;
                    start_daemon(remora, data_dir, left)?;
                } else {
                    thread::sleep(Duration::from_millis(50));
                }
            }
            connected => return connected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_older;

    #[test]
    fn only_an_older_release_is_older() {
        assert!(is_older("0.1.0", "0.2.0"));
        assert!(is_older("0.9.3", "0.10.0"), "numbers, not text");
        assert!(is_older("1.4.2", "2.0.0"));
        assert!(is_older("0.0.0", "0.1.0"), "a development build");
        assert!(!is_older("0.2.0", "0.2.0"));
        assert!(!is_older("0.3.0", "0.2.0"), "a newer daemon is left alone");
        assert!(!is_older("0.2.0-rc.1", "0.2.0"), "same release");
        assert!(!is_older("", "0.2.0"));
        assert!(!is_older("0.2", "0.3.0"));
        assert!(!is_older("0.2.0.1", "0.3.0"));
        assert!(!is_older("0.1.0", "unknown"));
    }
}
