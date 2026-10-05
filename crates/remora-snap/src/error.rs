use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum SnapError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid snapshot id {0:?}")]
    InvalidId(String),
    #[error("snapshot {0} not found")]
    NotFound(String),
    #[error("corrupt manifest for snapshot {id}: {source}")]
    CorruptManifest {
        id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid manifest for snapshot {id}: {reason}")]
    InvalidManifest { id: String, reason: String },
    #[error("blob {0} is missing from the store")]
    MissingBlob(String),
}

/// Attach a path to an `io::Error`: `fs::read(p).map_err(io_at(p))`.
pub(crate) fn io_at(path: &Path) -> impl FnOnce(io::Error) -> SnapError + '_ {
    move |source| SnapError::Io {
        path: path.to_path_buf(),
        source,
    }
}
