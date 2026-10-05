//! Filesystem snapshots and restore for Remora.
//!
//! Every backend implements [`Snapshotter`]. The plain-copy backend
//! ([`CopySnapshotter`]) works on any filesystem; copy-on-write backends
//! (APFS, reflink, ReFS) arrive in a later plan behind the same trait.

mod error;
mod manifest;

pub use error::SnapError;
pub use manifest::{Change, ChangeKind, Manifest, Node, SnapshotId};
