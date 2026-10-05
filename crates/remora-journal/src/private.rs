//! Owner-only filesystem helpers: the journal holds prompts and commands, which can contain secrets.

use std::fs;
use std::io;
use std::path::Path;

/// Create `dir` and any missing parents, owner-only (0700) on Unix.
#[cfg(unix)]
pub(crate) fn create_dir_all(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(not(unix))]
pub(crate) fn create_dir_all(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

/// Create `path` owner-only (0600 on Unix) if it does not exist yet; never truncates.
/// SQLite gives its journal/WAL side files the same permissions as the database file.
#[cfg(unix)]
pub(crate) fn ensure_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map(drop)
}

#[cfg(not(unix))]
pub(crate) fn ensure_file(path: &Path) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map(drop)
}
