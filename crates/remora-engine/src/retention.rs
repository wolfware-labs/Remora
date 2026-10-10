//! Retention: which restore points the store keeps, and pruning the others.
//!
//! A pruned point's manifest is deleted and its snapshot id recorded in `pruned.json`;
//! then every blob no remaining manifest needs is deleted (mark and sweep). The journal is
//! never touched: it stays complete and verifiable, and lists pruned points as such.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use remora_snap::SnapshotId;

use crate::error::io_at;
use crate::{EngineError, fsutil};

/// The file listing pruned snapshots in a project's data folder.
pub const PRUNED_FILE: &str = "pruned.json";

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const GB: u64 = 1_000_000_000;

/// How long restore points are kept, and how large a project's store may grow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retention {
    /// Restore points younger than this are always kept.
    pub days: u64,
    /// Per project: beyond this, the oldest points go until the blob folder fits.
    pub max_store_gb: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            days: 30,
            max_store_gb: 10,
        }
    }
}

impl Retention {
    fn cutoff_ms(&self, now_ms: i64) -> i64 {
        let days = i64::try_from(self.days).unwrap_or(i64::MAX);
        now_ms.saturating_sub(days.saturating_mul(DAY_MS))
    }

    fn max_store_bytes(&self) -> u64 {
        self.max_store_gb.saturating_mul(GB)
    }
}

/// Why a snapshot was pruned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PruneReason {
    /// Its newest restore point is older than the retention period.
    Age,
    /// The store was over its size cap and it was the oldest left.
    Size,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrunedSnapshot {
    pub snapshot: SnapshotId,
    /// When its newest restore point was taken.
    pub at_ms: i64,
    pub reason: PruneReason,
}

/// What a prune did, or (`dry_run`) would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReport {
    pub dry_run: bool,
    pub pruned: Vec<PrunedSnapshot>,
    pub blobs_removed: usize,
    pub bytes_freed: u64,
    /// The blob folder's size afterwards.
    pub store_bytes: u64,
    /// Snapshots whose manifest could not be read: kept, and no blob was removed, since
    /// they might need any of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<SnapshotId>,
}

/// Snapshot id → when it was pruned (milliseconds since the Unix epoch).
pub(crate) type Pruned = BTreeMap<String, i64>;

pub(crate) fn load_pruned(path: &Path) -> Result<Pruned, EngineError> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| EngineError::CorruptState {
            path: path.to_path_buf(),
            detail: e.to_string(),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Pruned::new()),
        Err(e) => Err(io_at(path)(e)),
    }
}

pub(crate) fn save_pruned(path: &Path, pruned: &Pruned) -> Result<(), EngineError> {
    let json = serde_json::to_vec(pruned).expect("the pruned list serializes");
    fsutil::write_private_atomic(path, &json).map_err(io_at(path))
}

/// The store as a prune sees it.
pub(crate) struct Store {
    /// Each readable snapshot with when it was last used (its newest restore point, else
    /// when its manifest was written) and the blobs it needs.
    pub snapshots: BTreeMap<SnapshotId, (i64, BTreeSet<String>)>,
    /// The readable snapshots no restore point uses.
    pub unused: BTreeSet<SnapshotId>,
    pub unreadable: Vec<SnapshotId>,
    /// Every blob with its size.
    pub blobs: BTreeMap<String, u64>,
}

/// The decision: which snapshots go, and which blobs then go with them.
pub(crate) struct Plan {
    pub pruned: Vec<PrunedSnapshot>,
    pub doomed_blobs: Vec<String>,
    pub bytes_freed: u64,
    pub store_bytes: u64,
}

/// Choose what to prune: snapshots last used before the cutoff, then the oldest left while
/// the blobs still needed exceed the cap. `protected` snapshots are never chosen, and the
/// cap never takes a snapshot no point uses that is younger than a day (the safety snapshot
/// of a rewind whose journal write failed: the user was handed its id). With an
/// unreadable manifest no blob can be proven unneeded: nothing is swept, and the size rule
/// (which could only free blobs) is skipped.
pub(crate) fn plan(store: &Store, protected: &BTreeSet<SnapshotId>, policy: &Retention, now_ms: i64) -> Plan {
    let mut candidates: Vec<(&SnapshotId, i64)> = store
        .snapshots
        .iter()
        .filter(|(id, _)| !protected.contains(*id))
        .map(|(id, (at, _))| (id, *at))
        .collect();
    candidates.sort_by_key(|(id, at)| (*at, id.as_str().to_owned()));

    // How many remaining snapshots need each blob.
    let mut needed: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, blobs) in store.snapshots.values() {
        for hash in blobs {
            *needed.entry(hash.as_str()).or_default() += 1;
        }
    }
    let total: u64 = store.blobs.values().sum();
    let sweep = store.unreadable.is_empty();
    let kept_bytes = |needed: &BTreeMap<&str, usize>| -> u64 {
        if !sweep {
            return total;
        }
        store
            .blobs
            .iter()
            .filter(|(hash, _)| needed.get(hash.as_str()).is_some_and(|n| *n > 0))
            .map(|(_, size)| size)
            .sum()
    };
    let cutoff = policy.cutoff_ms(now_ms);
    let mut pruned = Vec::new();
    let mut prune = |id: &SnapshotId, at_ms: i64, reason, needed: &mut BTreeMap<&str, usize>| {
        if let Some((_, blobs)) = store.snapshots.get(id) {
            for hash in blobs {
                if let Some(n) = needed.get_mut(hash.as_str()) {
                    *n -= 1;
                }
            }
        }
        pruned.push(PrunedSnapshot {
            snapshot: id.clone(),
            at_ms,
            reason,
        });
    };
    let mut left = candidates.into_iter().peekable();
    while let Some((id, at)) = left.next_if(|(_, at)| *at < cutoff) {
        prune(id, at, PruneReason::Age, &mut needed);
    }
    let mut store_bytes = kept_bytes(&needed);
    if sweep {
        let young_unused =
            |id: &SnapshotId, at: i64| store.unused.contains(id) && now_ms.saturating_sub(at) < DAY_MS;
        let mut left = left.filter(|(id, at)| !young_unused(id, *at));
        while store_bytes > policy.max_store_bytes() {
            let Some((id, at)) = left.next() else { break };
            prune(id, at, PruneReason::Size, &mut needed);
            store_bytes = kept_bytes(&needed);
        }
    }
    let doomed_blobs: Vec<String> = if sweep {
        store
            .blobs
            .keys()
            .filter(|hash| !needed.get(hash.as_str()).is_some_and(|n| *n > 0))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    Plan {
        pruned,
        doomed_blobs,
        bytes_freed: total - store_bytes,
        store_bytes,
    }
}

/// `YYYY-MM-DD` (UTC) for milliseconds since the Unix epoch.
pub fn utc_date(ms: i64) -> String {
    // Howard Hinnant's civil-from-days.
    let z = ms.div_euclid(DAY_MS) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_dates() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400_000), "2000-02-29");
        assert_eq!(utc_date(1_791_590_400_000), "2026-10-10");
        assert_eq!(utc_date(-DAY_MS), "1969-12-31");
    }
}
