//! Owner-only filesystem helpers: the journal and the key hold secrets.

use std::fs;
use std::io;
use std::path::Path;

/// Create a new file (failing if anything exists at `path`), owner-only (0600) on Unix.
#[cfg(unix)]
pub(crate) fn create_new_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
pub(crate) fn create_new_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().write(true).create_new(true).open(path)
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

/// Remove group/other permissions from an existing file (Unix); no-op elsewhere.
#[cfg(unix)]
pub(crate) fn tighten_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn tighten_file(_path: &Path) -> io::Result<()> {
    Ok(())
}
