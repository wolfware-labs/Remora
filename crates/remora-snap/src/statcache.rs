//! A cache of file content hashes keyed by file metadata, so a snapshot does not
//! re-read files that have not changed since the previous snapshot.
//!
//! Trust rules (a wrong cache hit would silently snapshot stale content):
//! - every one of size, mtime (nanoseconds) and inode must match;
//! - a file modified within [`RACY_WINDOW_NS`] of the snapshot start is always re-read,
//!   because a second write in the same timestamp tick leaves metadata unchanged
//!   ("racy timestamps", the same guard git uses), and such a file is never *recorded*
//!   either, or a later snapshot would trust a hash read while the file was still racy;
//! - the caller only trusts a hit whose blob is present in the store.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Files modified this recently (relative to the snapshot start) are always re-read.
pub(crate) const RACY_WINDOW_NS: u64 = 2_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CachedStat {
    pub size: u64,
    pub mtime_ns: u64,
    pub ino: u64,
    pub hash: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct StatCache {
    /// Keyed by absolute path.
    entries: BTreeMap<String, CachedStat>,
}

impl StatCache {
    /// Load the cache; a missing or unreadable cache is simply empty.
    pub(crate) fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub(crate) fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("cache always serializes")
    }

    /// The cached hash for `key`, if the metadata matches and the file is not racy.
    pub(crate) fn lookup(
        &self,
        key: &str,
        size: u64,
        mtime_ns: u64,
        ino: u64,
        started_ns: u64,
    ) -> Option<&str> {
        if is_racy(mtime_ns, started_ns) {
            return None;
        }
        self.entries
            .get(key)
            .filter(|c| c.size == size && c.mtime_ns == mtime_ns && c.ino == ino)
            .map(|c| c.hash.as_str())
    }

    pub(crate) fn insert(&mut self, key: String, stat: CachedStat) {
        self.entries.insert(key, stat);
    }

    /// Drop every entry under `prefix` (a snapshot root) before re-filling it, so files
    /// that no longer exist do not linger.
    pub(crate) fn clear_under(&mut self, prefix: &str) {
        self.entries.retain(|k, _| !k.starts_with(prefix));
    }
}

/// True if a file modified at `mtime_ns` may still be written again within the same
/// timestamp tick as a snapshot that started at `started_ns`.
pub(crate) fn is_racy(mtime_ns: u64, started_ns: u64) -> bool {
    mtime_ns.saturating_add(RACY_WINDOW_NS) >= started_ns
}
