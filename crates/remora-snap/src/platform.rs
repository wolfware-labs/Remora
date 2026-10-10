//! The only module with OS-specific code paths for permissions and symlinks.

use std::fs::{self, Metadata};
use std::io;
use std::path::Path;

#[cfg(unix)]
pub(crate) fn mode_of(meta: &Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
pub(crate) fn mode_of(meta: &Metadata) -> u32 {
    match (meta.is_dir(), meta.permissions().readonly()) {
        (_, true) => 0o444,
        (true, false) => 0o755,
        (false, false) => 0o644,
    }
}

#[cfg(unix)]
pub(crate) fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
pub(crate) fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(mode & 0o200 == 0);
    fs::set_permissions(path, perms)
}

#[cfg(unix)]
pub(crate) fn create_symlink(target: &str, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
pub(crate) fn create_symlink(target: &str, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn create_symlink(_target: &str, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "symlinks are not supported on this platform",
    ))
}

/// Create `path` and any missing parents, owner-only (0700) on Unix.
#[cfg(unix)]
pub(crate) fn create_private_dir_all(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
}

#[cfg(not(unix))]
pub(crate) fn create_private_dir_all(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

/// Create a new file, failing if anything (including a symlink) already exists at `path`.
/// Owner-only (0600) on Unix.
#[cfg(unix)]
pub(crate) fn create_private_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
pub(crate) fn create_private_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().write(true).create_new(true).open(path)
}

/// Make an existing file writable by its owner alone: 0600 on Unix, not read-only on Windows.
/// A clone keeps its source's permissions.
#[cfg(unix)]
pub(crate) fn set_private(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub(crate) fn set_private(path: &Path) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    #[allow(clippy::permissions_set_readonly_false)] // Windows: only clears the read-only attribute
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)
}

/// Make a rename in `path`'s folder durable: on Unix the folder itself is flushed, or a power
/// loss could forget the rename. Windows journals rename metadata (NTFS, ReFS) and has no safe
/// way to flush a folder.
pub(crate) fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Modification time in nanoseconds since the Unix epoch, if the platform reports one.
pub(crate) fn mtime_ns(meta: &Metadata) -> Option<u64> {
    let since_epoch = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(since_epoch.as_nanos()).ok()
}

#[cfg(unix)]
pub(crate) fn inode(meta: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
pub(crate) fn inode(_meta: &Metadata) -> u64 {
    0
}

/// Inode change time in nanoseconds. Unlike mtime it cannot be set by tools such as
/// `touch`, `cp -p` or `tar`, so it catches an edit that restored an old mtime.
#[cfg(unix)]
pub(crate) fn ctime_ns(meta: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let nanos = i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec());
    u64::try_from(nanos).unwrap_or(0)
}

#[cfg(not(unix))]
pub(crate) fn ctime_ns(_meta: &Metadata) -> u64 {
    0
}

/// Remove group/other permissions from an existing directory (Unix); no-op elsewhere.
#[cfg(unix)]
pub(crate) fn tighten_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn tighten_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn create_private_file_never_follows_a_planted_symlink() {
        let dir = tempfile::TempDir::new().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, "untouched").unwrap();
        let planted = dir.path().join("planted");
        std::os::unix::fs::symlink(&victim, &planted).unwrap();

        let err = create_private_file(&planted).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched");
    }
}

/// Remove every extended attribute of `path` (macOS; elsewhere a no-op: there clones and
/// copies carry none Remora needs to clear).
#[cfg(target_os = "macos")]
pub(crate) fn clear_xattrs(path: &Path) -> io::Result<()> {
    for name in xattr::list(path)? {
        match xattr::remove(path, &name) {
            Ok(()) => {}
            // Gone in between (some are maintained by the system).
            Err(e) if e.raw_os_error() == Some(93) => {} // ENOATTR
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn clear_xattrs(_path: &Path) -> io::Result<()> {
    Ok(())
}
