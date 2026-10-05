//! The only module with OS-specific code paths for permissions and symlinks.

use std::fs::Metadata;

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
