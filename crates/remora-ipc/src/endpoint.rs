use std::io;
use std::path::{Path, PathBuf};

use interprocess::local_socket::Name;

/// Where the daemon for a data directory listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// macOS, Linux: a socket file inside the (owner-only) data directory.
    Socket(PathBuf),
    /// Windows: `\\.\pipe\remora-<first 16 hex of sha256(data directory)>`.
    Pipe(String),
}

/// The endpoint of the daemon that owns `data_dir`. Pass the canonical data directory
/// (as `Engine::data_dir` returns it) so that clients and daemon agree.
pub fn endpoint(data_dir: &Path) -> Endpoint {
    if cfg!(windows) {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(data_dir.to_string_lossy().as_bytes());
        Endpoint::Pipe(format!(r"\\.\pipe\remora-{}", &hex::encode(digest)[..16]))
    } else {
        Endpoint::Socket(data_dir.join("remorad.sock"))
    }
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
