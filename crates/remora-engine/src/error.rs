use std::path::PathBuf;

use remora_journal::JournalError;
use remora_snap::SnapError;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Snap(#[from] SnapError),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("project data {id} belongs to {recorded_root}, not this folder")]
    ProjectMismatch { id: String, recorded_root: String },
    #[error("a turn is still in progress in session {session}; stop the agent first")]
    TurnInProgress { session: String },
    #[error("Remora has nothing recorded for {}", .0.display())]
    UnknownProject(PathBuf),
    #[error("{0} is not a restore point of this project")]
    UnknownRestorePoint(String),
    #[error("this restore point was pruned on {}: its files are no longer stored", crate::utc_date(*pruned_at_ms))]
    Pruned { snapshot: String, pruned_at_ms: i64 },
    #[error("{}", incomplete_message(*rolling_back, safety_snapshot, source))]
    RewindIncomplete {
        safety_snapshot: String,
        /// True when it was the rollback of a rewind cut short, as the project opened, that
        /// did not finish: the project stays unusable until the safety snapshot is restored.
        rolling_back: bool,
        #[source]
        source: Box<EngineError>,
    },
    #[error(
        "{root} overlaps Remora's data directory {data_dir}; choose a project folder that neither contains it nor lies inside it"
    )]
    RootOverlapsDataDir { root: PathBuf, data_dir: PathBuf },
    #[error("another Remora process kept the project locked for too long")]
    LockTimeout,
    #[error("invalid session id {0:?}")]
    InvalidSessionId(String),
    #[error("state file {path} is unreadable: {detail}")]
    CorruptState { path: PathBuf, detail: String },
    #[error("no data directory: set one explicitly")]
    NoDataDir,
    #[error("blob store {path} is unavailable: {reason}")]
    BlobStoreUnavailable { path: PathBuf, reason: String },
}

fn incomplete_message(rolling_back: bool, safety: &str, source: &EngineError) -> String {
    if rolling_back {
        format!(
            "rolling back an interrupted rewind failed ({source}); the state before that rewind is kept as snapshot {safety}: `remora rewind --snapshot {safety}` restores it"
        )
    } else {
        format!("the rewind did not finish ({source}); the state before it was kept as snapshot {safety}")
    }
}

/// Attach a path to an `io::Error`: `fs::read(p).map_err(io_at(p))`.
pub(crate) fn io_at(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> EngineError + '_ {
    move |source| EngineError::Io {
        path: path.to_path_buf(),
        source,
    }
}
