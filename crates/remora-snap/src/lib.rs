//! Filesystem snapshots and restore for Remora.
//!
//! Every backend implements [`Snapshotter`]. The plain-copy backend
//! ([`CopySnapshotter`]) works on any filesystem; copy-on-write backends
//! (APFS, reflink, ReFS) arrive in a later plan behind the same trait.

mod copy;
mod error;
mod manifest;
mod platform;
mod scan;
mod statcache;

use std::path::{Path, PathBuf};

pub use copy::CopySnapshotter;
pub use error::SnapError;
pub use manifest::{Change, ChangeKind, Manifest, Node, RecordedOptions, SnapshotId};

/// Options shared by every backend.
#[derive(Debug, Clone)]
pub struct SnapOptions {
    /// Directory names skipped at any depth (never snapshotted, never touched by restore).
    pub exclude_dirs: Vec<String>,
    /// Files larger than this are recorded by hash only and cannot be restored.
    pub max_file_bytes: u64,
}

impl Default for SnapOptions {
    fn default() -> Self {
        Self {
            exclude_dirs: ["node_modules", "target", ".venv", ".remora"]
                .into_iter()
                .map(String::from)
                .collect(),
            max_file_bytes: 100 * 1024 * 1024,
        }
    }
}

/// Result of taking a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub id: SnapshotId,
    /// Paths that exist but are not protected (unreadable, too large, non-UTF-8 names).
    pub unprotected: Vec<PathBuf>,
    /// How many files had to be read and hashed (the rest were unchanged per the stat cache).
    pub hashed_files: usize,
    /// How many of the hashed files were stored by cloning (copy-on-write) instead of copying.
    pub cloned_files: usize,
}

/// Result of a restore.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// Files and symlinks written.
    pub written: usize,
    /// How many of the written files were restored by cloning (copy-on-write) instead of copying.
    pub cloned: usize,
    /// Paths removed because they did not exist in the snapshot.
    pub deleted: usize,
    /// Paths left untouched because the snapshot could not protect them.
    pub skipped: Vec<String>,
    /// Paths that could not be restored, with the error message.
    pub failed: Vec<(String, String)>,
}

/// A snapshot backend.
pub trait Snapshotter {
    /// Capture the current state of `root`.
    fn snapshot(&self, root: &Path) -> Result<Snapshot, SnapError>;
    /// Make `root` match snapshot `id` exactly (excluded directories aside).
    fn restore(&self, root: &Path, id: &SnapshotId) -> Result<RestoreReport, SnapError>;
    /// Load the manifest of snapshot `id`.
    fn manifest(&self, id: &SnapshotId) -> Result<Manifest, SnapError>;
}
