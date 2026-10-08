use std::io;
use std::path::{Path, PathBuf};

use interprocess::local_socket::Name;

/// The longest socket path kept inside the data directory. The OS limit is 104 bytes on
/// macOS and 108 on Linux; a longer path goes to the runtime directory instead.
pub const MAX_SOCKET_PATH: usize = 100;

/// Where the daemon for a data directory listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// macOS, Linux: a socket file inside the (owner-only) data directory, or, when that path
    /// would be too long, in the owner-only runtime directory.
    Socket(PathBuf),
    /// Windows: `\\.\pipe\remora-<first 16 hex of sha256(data directory)>`.
    Pipe(String),
}

/// The endpoint of the daemon that owns `data_dir`. Pass the canonical data directory
/// (as `Engine::data_dir` returns it) so that clients and daemon agree.
pub fn endpoint(data_dir: &Path) -> Endpoint {
    if cfg!(windows) {
        Endpoint::Pipe(format!(r"\\.\pipe\remora-{}", short_hash(data_dir)))
    } else {
        Endpoint::Socket(socket_path(data_dir, || runtime_dir(data_dir)))
    }
}

/// `<data_dir>/remorad.sock` when it fits in [`MAX_SOCKET_PATH`] bytes, else
/// `<runtime>/remora-<first 16 hex of sha256(data directory)>.sock`.
pub fn socket_path(data_dir: &Path, runtime: impl FnOnce() -> PathBuf) -> PathBuf {
    let inside = data_dir.join("remorad.sock");
    if inside.as_os_str().len() <= MAX_SOCKET_PATH {
        inside
    } else {
        runtime().join(format!("remora-{}.sock", short_hash(data_dir)))
    }
}

/// Where sockets of long data directories go: `$XDG_RUNTIME_DIR` when set, else
/// `<temp dir>/remora-<uid of the data directory's owner>`. The daemon creates the latter
/// owner-only and refuses either if another user owns it or others may open it.
pub fn runtime_dir(data_dir: &Path) -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if Path::new(&dir).is_absolute() => PathBuf::from(dir),
        _ => std::env::temp_dir().join(format!("remora-{}", owner_uid(data_dir))),
    }
}

#[cfg(unix)]
fn owner_uid(data_dir: &Path) -> u32 {
    use std::os::unix::fs::MetadataExt;
    // No data directory, no daemon: any name will do, nothing listens on it.
    std::fs::metadata(data_dir).map_or(u32::MAX, |m| m.uid())
}

#[cfg(not(unix))]
fn owner_uid(_data_dir: &Path) -> u32 {
    0
}

fn short_hash(data_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(data_dir.to_string_lossy().as_bytes());
    hex::encode(digest)[..16].to_owned()
}

impl Endpoint {
    /// The name `interprocess` binds or connects to. Never the Linux abstract namespace.
    pub fn name(&self) -> io::Result<Name<'_>> {
        use interprocess::local_socket::{GenericFilePath, ToFsName};
        match self {
            Endpoint::Socket(path) => path.as_path().to_fs_name::<GenericFilePath>(),
            Endpoint::Pipe(pipe) => pipe.as_str().to_fs_name::<GenericFilePath>(),
        }
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Endpoint::Socket(path) => write!(f, "{}", path.display()),
            Endpoint::Pipe(pipe) => f.write_str(pipe),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_data_dir_keeps_its_socket_inside() {
        let data = Path::new("/home/me/.local/share/remora");
        let path = socket_path(data, || panic!("the runtime directory is not needed"));
        assert_eq!(path, data.join("remorad.sock"));
    }

    #[test]
    fn a_long_data_dir_puts_its_socket_in_the_runtime_dir() {
        let data = PathBuf::from(format!("/home/me/{}/remora", "a".repeat(90)));
        let path = socket_path(&data, || PathBuf::from("/run/user/1000"));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(path.parent(), Some(Path::new("/run/user/1000")));
        assert!(
            name.starts_with("remora-") && name.ends_with(".sock") && name.len() == 28,
            "{name}"
        );
        let other = socket_path(Path::new(&format!("{}x", data.display())), || {
            "/run/user/1000".into()
        });
        assert_ne!(path, other, "each data directory gets its own socket");
    }

    #[test]
    fn the_limit_is_on_the_whole_path() {
        let fits = PathBuf::from(format!(
            "/{}",
            "d".repeat(MAX_SOCKET_PATH - "/".len() - "/remorad.sock".len())
        ));
        assert_eq!(fits.join("remorad.sock").as_os_str().len(), MAX_SOCKET_PATH);
        assert_eq!(socket_path(&fits, || "/run".into()), fits.join("remorad.sock"));
        let over = PathBuf::from(format!("{}d", fits.display()));
        assert!(socket_path(&over, || "/run".into()).starts_with("/run"));
    }
}
