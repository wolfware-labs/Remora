use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("signing key at {0} is not a 32-byte ed25519 key")]
    InvalidKey(PathBuf),
    #[error("could not generate a signing key: {0}")]
    Random(String),
    #[error("entry {seq} is unreadable: {detail}")]
    Corrupt { seq: i64, detail: String },
}
