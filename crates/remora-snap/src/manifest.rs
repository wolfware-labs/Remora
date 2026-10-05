use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::SnapError;

/// Content address of a manifest: 64 lowercase hex characters (SHA-256).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SnapshotId(String);

impl SnapshotId {
    /// Parse an id. Rejects anything that is not 64 lowercase hex characters,
    /// so an id can never be used for path traversal in the store.
    pub fn parse(s: &str) -> Result<Self, SnapError> {
        let valid = s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if valid {
            Ok(Self(s.to_owned()))
        } else {
            Err(SnapError::InvalidId(s.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One path in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Node {
    Dir {
        mode: u32,
    },
    /// `stored == false` means the file was too large: recorded by hash, not restorable.
    File {
        hash: String,
        size: u64,
        mode: u32,
        stored: bool,
    },
    Symlink {
        target: String,
    },
    /// Existed but could not be read. Restore leaves it (and everything under it) alone.
    Opaque,
}

impl Node {
    /// True when restore must leave this path untouched.
    pub fn is_protected_gap(&self) -> bool {
        matches!(self, Node::Opaque | Node::File { stored: false, .. })
    }
}

/// Every path under a root, keyed by `/`-separated relative path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub nodes: BTreeMap<String, Node>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

impl Manifest {
    /// Changes needed to go from `self` (older) to `newer`, sorted by path.
    pub fn diff(&self, newer: &Manifest) -> Vec<Change> {
        let mut changes = Vec::new();
        for (path, old) in &self.nodes {
            match newer.nodes.get(path) {
                None => changes.push(Change {
                    path: path.clone(),
                    kind: ChangeKind::Deleted,
                }),
                Some(new) if new != old => changes.push(Change {
                    path: path.clone(),
                    kind: ChangeKind::Modified,
                }),
                Some(_) => {}
            }
        }
        for path in newer.nodes.keys() {
            if !self.nodes.contains_key(path) {
                changes.push(Change {
                    path: path.clone(),
                    kind: ChangeKind::Added,
                });
            }
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        changes
    }

    /// True if `rel` or any of its ancestors is a protected gap (see [`Node::is_protected_gap`]).
    pub fn protects_gap(&self, rel: &str) -> bool {
        let mut prefixes = rel.match_indices('/').map(|(i, _)| &rel[..i]).chain([rel]);
        prefixes.any(|p| self.nodes.get(p).is_some_and(Node::is_protected_gap))
    }
}
