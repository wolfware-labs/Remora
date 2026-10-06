use std::fs::{File, TryLockError};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::error::io_at;
use crate::{EngineError, fsutil};

/// How long to wait for another process holding the project lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// An exclusive, cross-process lock on one project. Released when dropped.
pub(crate) struct ProjectLock {
    _file: File,
}

impl ProjectLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self, EngineError> {
        let file = fsutil::open_lock_file(path).map_err(io_at(path))?;
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(TryLockError::WouldBlock) => return Err(EngineError::LockTimeout),
                Err(TryLockError::Error(e)) => return Err(io_at(path)(e)),
            }
        }
    }
}
