use std::io;
use std::path::PathBuf;

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
}
