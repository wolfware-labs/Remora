//! Running a daemon in the foreground until it is told to stop, as `remorad run` does.

use std::path::{Path, PathBuf};

use crate::{Daemon, DaemonConfig, DaemonError, install_logger};

/// How a foreground run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ran {
    /// The daemon ran and was stopped.
    Stopped,
    /// Another daemon already owns the data directory; nothing was started.
    AlreadyRunning(PathBuf),
}

/// Run a daemon for `data_dir` until it is asked to stop: a `shutdown` request, SIGTERM,
/// SIGINT or Ctrl-C. Logs to `<data_dir>/remorad.log`. Installs the process's logger and
/// signal handler, so call it at most once per process.
pub fn run_in_foreground(data_dir: &Path) -> Result<Ran, DaemonError> {
    let io_error = |source| DaemonError::Io {
        path: data_dir.to_path_buf(),
        source,
    };
    install_logger(data_dir).map_err(io_error)?;
    let daemon = match Daemon::start(DaemonConfig::new(data_dir)) {
        Ok(daemon) => daemon,
        Err(DaemonError::AlreadyRunning(dir)) => return Ok(Ran::AlreadyRunning(dir)),
        Err(e) => {
            log::error!("cannot start: {e}");
            return Err(e);
        }
    };
    let stopper = daemon.clone();
    ctrlc::set_handler(move || stopper.shutdown()).map_err(|e| io_error(std::io::Error::other(e)))?;
    daemon.wait();
    Ok(Ran::Stopped)
}
