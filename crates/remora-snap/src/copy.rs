//! Plain-copy backend: a content-addressed blob store plus JSON manifests.
//!
//! Store layout:
//! ```text
//! <store>/blobs/<first 2 hex>/<remaining 62 hex>   file contents, deduplicated by SHA-256
//! <store>/manifests/<snapshot id>.json             one manifest per snapshot
//! <store>/statcache.json                           metadata → hash cache (see `statcache`)
//! ```

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::error::io_at;
use crate::manifest::is_sha256_hex;
use crate::scan::{Kind, scan};
use crate::statcache::{CachedStat, StatCache, is_racy};
use crate::{
    Manifest, Node, RecordedOptions, RestoreReport, SnapError, SnapOptions, Snapshot, SnapshotId,
    Snapshotter, platform,
};

#[derive(Debug, Clone)]
pub struct CopySnapshotter {
    store: PathBuf,
    opts: SnapOptions,
}

impl CopySnapshotter {
    /// Open (or create) a store at `store`.
    pub fn new(store: impl Into<PathBuf>, opts: SnapOptions) -> Result<Self, SnapError> {
        let store = store.into();
        for sub in ["blobs", "manifests"] {
            let dir = store.join(sub);
            platform::create_private_dir_all(&dir).map_err(io_at(&dir))?;
        }
        // A store created before permissions were enforced (or loosened since) is tightened.
        for dir in [store.clone(), store.join("blobs"), store.join("manifests")] {
            platform::tighten_dir(&dir).map_err(io_at(&dir))?;
        }
        let store = store.canonicalize().map_err(io_at(&store))?;
        Ok(Self { store, opts })
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.store.join("blobs").join(&hash[..2]).join(&hash[2..])
    }

    fn manifest_path(&self, id: &SnapshotId) -> PathBuf {
        self.store.join("manifests").join(format!("{id}.json"))
    }

    fn statcache_path(&self) -> PathBuf {
        self.store.join("statcache.json")
    }

    fn put_blob(&self, bytes: &[u8]) -> Result<String, SnapError> {
        let hash = hash_bytes(bytes);
        let path = self.blob_path(&hash);
        // A blob whose size is wrong (say, truncated by a crash) is rewritten.
        let intact = fs::metadata(&path).is_ok_and(|m| m.len() == bytes.len() as u64);
        if !intact {
            let dir = path.parent().expect("blob path has a parent");
            platform::create_private_dir_all(dir).map_err(io_at(dir))?;
            write_atomic(&path, bytes).map_err(io_at(&path))?;
        }
        Ok(hash)
    }

    /// Returns `Ok(true)` if the file was (re)written, `Ok(false)` if it already matched.
    fn restore_file(&self, abs: &Path, hash: &str, mode: u32) -> io::Result<bool> {
        let unchanged =
            fs::symlink_metadata(abs).is_ok_and(|m| m.is_file()) && hash_file(abs).is_ok_and(|h| h == hash);
        if !unchanged {
            let bytes = fs::read(self.blob_path(hash))
                .map_err(|e| io::Error::new(e.kind(), format!("reading blob {hash}: {e}")))?;
            if hash_bytes(&bytes) != hash {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("blob {hash} is corrupt (its contents do not match its hash)"),
                ));
            }
            write_atomic(abs, &bytes)?;
        }
        platform::set_mode(abs, mode)?;
        Ok(!unchanged)
    }
}

impl Snapshotter for CopySnapshotter {
    fn snapshot(&self, root: &Path) -> Result<Snapshot, SnapError> {
        let root = root.canonicalize().map_err(io_at(root))?;
        let started_ns = now_ns();
        let scanned = scan(&root, &self.opts, &self.store)?;
        let mut manifest = Manifest {
            options: Some(RecordedOptions {
                exclude_dirs: self.opts.exclude_dirs.clone(),
                max_file_bytes: self.opts.max_file_bytes,
            }),
            ..Manifest::default()
        };
        let mut unprotected = scanned.non_utf8;
        // Look up in the cache as it was. The new cache starts from it with this root's
        // entries dropped and is refilled from this snapshot, so deleted files disappear.
        let old_cache = StatCache::load(&self.statcache_path());
        let mut cache = old_cache.clone();
        cache.clear_under(&format!(
            "{}{}",
            root.to_string_lossy(),
            std::path::MAIN_SEPARATOR
        ));
        let mut hashed_files = 0;

        for (rel, entry) in scanned.entries {
            let node = match entry.kind {
                Kind::Dir { mode } => Node::Dir { mode },
                Kind::Symlink { target } => Node::Symlink { target },
                Kind::Unreadable => {
                    unprotected.push(entry.abs);
                    Node::Opaque
                }
                Kind::File { size, mode, .. } if size > self.opts.max_file_bytes => {
                    match hash_file(&entry.abs) {
                        Ok(hash) => {
                            unprotected.push(entry.abs);
                            Node::File {
                                hash,
                                size,
                                mode,
                                stored: false,
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                        Err(_) => {
                            unprotected.push(entry.abs);
                            Node::Opaque
                        }
                    }
                }
                Kind::File {
                    size,
                    mode,
                    mtime_ns,
                    ino,
                } => {
                    let key = entry.abs.to_string_lossy().into_owned();
                    let cached = mtime_ns
                        .and_then(|m| old_cache.lookup(&key, size, m, ino, started_ns))
                        // A damaged cache must never panic `blob_path` or reach a manifest.
                        .filter(|hash| is_sha256_hex(hash))
                        .filter(|hash| fs::metadata(self.blob_path(hash)).is_ok_and(|b| b.len() == size))
                        .map(str::to_owned);
                    if let Some(hash) = cached {
                        let mtime_ns = mtime_ns.expect("a cache hit implies an mtime");
                        cache.insert(
                            key,
                            CachedStat {
                                size,
                                mtime_ns,
                                ino,
                                hash: hash.clone(),
                            },
                        );
                        Node::File {
                            hash,
                            size,
                            mode,
                            stored: true,
                        }
                    } else {
                        match fs::read(&entry.abs) {
                            Ok(bytes) => {
                                hashed_files += 1;
                                let hash = self.put_blob(&bytes)?;
                                // Only cache what was read if the file did not change since the scan, and
                                // never while it is racy (see `statcache`).
                                if let Some(mtime_ns) = mtime_ns
                                    && bytes.len() as u64 == size
                                    && !is_racy(mtime_ns, started_ns)
                                {
                                    cache.insert(
                                        key,
                                        CachedStat {
                                            size,
                                            mtime_ns,
                                            ino,
                                            hash: hash.clone(),
                                        },
                                    );
                                }
                                Node::File {
                                    hash,
                                    size: bytes.len() as u64,
                                    mode,
                                    stored: true,
                                }
                            }
                            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                            Err(_) => {
                                unprotected.push(entry.abs);
                                Node::Opaque
                            }
                        }
                    }
                }
            };
            manifest.nodes.insert(rel, node);
        }

        let json = serde_json::to_vec(&manifest).expect("manifest always serializes");
        let id = SnapshotId::parse(&hash_bytes(&json))?;
        let path = self.manifest_path(&id);
        // A manifest whose bytes differ from what they must be (say, truncated) is rewritten.
        if fs::read(&path).ok().as_deref() != Some(json.as_slice()) {
            write_atomic(&path, &json).map_err(io_at(&path))?;
        }
        let cache_path = self.statcache_path();
        write_atomic(&cache_path, &cache.to_json()).map_err(io_at(&cache_path))?;
        Ok(Snapshot {
            id,
            unprotected,
            hashed_files,
        })
    }

    fn manifest(&self, id: &SnapshotId) -> Result<Manifest, SnapError> {
        let path = self.manifest_path(id);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(SnapError::NotFound(id.to_string())),
            Err(e) => return Err(io_at(&path)(e)),
        };
        let invalid = |reason: String| SnapError::InvalidManifest {
            id: id.to_string(),
            reason,
        };
        if hash_bytes(&bytes) != id.as_str() {
            return Err(invalid("contents do not match the snapshot id".into()));
        }
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|source| SnapError::CorruptManifest {
                id: id.to_string(),
                source,
            })?;
        manifest.validate().map_err(invalid)?;
        Ok(manifest)
    }

    fn restore(&self, root: &Path, id: &SnapshotId) -> Result<RestoreReport, SnapError> {
        let manifest = self.manifest(id)?;

        // 0. Every blob restore needs must exist before anything is deleted.
        for (rel, node) in &manifest.nodes {
            if let Node::File {
                hash, stored: true, ..
            } = node
                && !manifest.protects_gap(rel)
                && !self.blob_path(hash).is_file()
            {
                return Err(SnapError::MissingBlob(hash.clone()));
            }
        }

        fs::create_dir_all(root).map_err(io_at(root))?;
        let root = root.canonicalize().map_err(io_at(root))?;
        // Leave alone everything excluded now OR when the snapshot was taken: the snapshot
        // knows nothing about the contents of directories it excluded.
        let mut opts = self.opts.clone();
        for dir in manifest.options.iter().flat_map(|o| &o.exclude_dirs) {
            if !opts.exclude_dirs.contains(dir) {
                opts.exclude_dirs.push(dir.clone());
            }
        }
        let current = scan(&root, &opts, &self.store)?;
        let mut report = RestoreReport::default();
        // Paths whose removal failed. They (and everything under them) are never written
        // or chmod-ed: a surviving symlink would otherwise be followed outside the root.
        let mut blocked: Vec<String> = Vec::new();

        // 1. Remove what the snapshot lacks or has as a different type. Reverse order: children first.
        for (rel, entry) in current.entries.iter().rev() {
            if manifest.protects_gap(rel) {
                continue;
            }
            let keep = manifest
                .nodes
                .get(rel)
                .is_some_and(|node| same_type(node, &entry.kind));
            if keep {
                continue;
            }
            match remove_path(&entry.abs) {
                Ok(()) => report.deleted += 1,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    report.failed.push((rel.clone(), e.to_string()));
                    blocked.push(rel.clone());
                }
            }
        }
        // 2. Recreate in path order, so parents exist before children.
        for (rel, node) in &manifest.nodes {
            if manifest.protects_gap(rel) {
                if node.is_protected_gap() {
                    report.skipped.push(rel.clone());
                }
                continue;
            }
            if is_under_any(rel, &blocked) {
                continue;
            }
            // The scan never looks inside excluded directories or the store, so phase 1 cannot
            // have cleaned them up: never write there (a symlink could lead outside the root).
            if is_excluded(&opts, rel, node) || root.join(rel).starts_with(&self.store) {
                report.skipped.push(rel.clone());
                continue;
            }
            let abs = root.join(rel);
            // Never write through anything that is not a real directory: a surviving symlink
            // (or one a case-insensitive filesystem treats as the same name) leads outside the root.
            if let Err(e) = check_real_dirs(&root, rel, node) {
                report.failed.push((rel.clone(), e.to_string()));
                blocked.push(rel.clone());
                continue;
            }
            let result = match node {
                Node::Dir { .. } => fs::create_dir_all(&abs).map(|()| false),
                Node::File { hash, mode, .. } => self.restore_file(&abs, hash, *mode),
                Node::Symlink { target } => restore_symlink(&abs, target),
                Node::Opaque => Ok(false),
            };
            match result {
                Ok(true) => report.written += 1,
                Ok(false) => {}
                Err(e) => report.failed.push((rel.clone(), e.to_string())),
            }
        }

        // 3. Directory modes last, children first, so a read-only dir never blocks a write.
        for (rel, node) in manifest.nodes.iter().rev() {
            if let Node::Dir { mode } = node
                && !manifest.protects_gap(rel)
                && !is_under_any(rel, &blocked)
                && !is_excluded(&opts, rel, node)
                && !root.join(rel).starts_with(&self.store)
                && fs::symlink_metadata(root.join(rel)).is_ok_and(|m| m.is_dir())
                && let Err(e) = platform::set_mode(&root.join(rel), *mode)
            {
                report.failed.push((rel.clone(), e.to_string()));
            }
        }

        Ok(report)
    }
}

/// True if `rel` lies inside an excluded directory, or is one.
fn is_excluded(opts: &SnapOptions, rel: &str, node: &Node) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    let (last, parents) = parts.split_last().expect("split always yields one part");
    let excluded = |name: &str| opts.exclude_dirs.iter().any(|x| x == name);
    parents.iter().any(|p| excluded(p)) || (matches!(node, Node::Dir { .. }) && excluded(last))
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// True if `rel` is one of `prefixes` or lies under one of them.
fn is_under_any(rel: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|b| {
        rel == b
            || rel
                .strip_prefix(b.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Every ancestor of `rel` (and `rel` itself, for a directory node) must be a real
/// directory, not a symlink, before restore writes there.
fn check_real_dirs(root: &Path, rel: &str, node: &Node) -> io::Result<()> {
    let not_a_dir = |p: &str| {
        io::Error::other(format!(
            "{p:?} is not a real directory; refusing to write through it"
        ))
    };
    for (i, _) in rel.match_indices('/') {
        let ancestor = &rel[..i];
        if !fs::symlink_metadata(root.join(ancestor)).is_ok_and(|m| m.is_dir()) {
            return Err(not_a_dir(ancestor));
        }
    }
    if matches!(node, Node::Dir { .. })
        && let Ok(meta) = fs::symlink_metadata(root.join(rel))
        && !meta.is_dir()
    {
        return Err(not_a_dir(rel));
    }
    Ok(())
}

fn same_type(node: &Node, kind: &Kind) -> bool {
    matches!(
        (node, kind),
        (Node::Dir { .. }, Kind::Dir { .. })
            | (Node::File { .. }, Kind::File { .. })
            | (Node::Symlink { .. }, Kind::Symlink { .. })
    )
}

fn remove_path(abs: &Path) -> io::Result<()> {
    if fs::symlink_metadata(abs)?.is_dir() {
        fs::remove_dir_all(abs)
    } else {
        fs::remove_file(abs)
    }
}

fn restore_symlink(abs: &Path, target: &str) -> io::Result<bool> {
    if fs::read_link(abs).is_ok_and(|t| t.as_os_str() == target) {
        return Ok(false);
    }
    match fs::remove_file(abs) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    platform::create_symlink(target, abs)?;
    Ok(true)
}

/// Distinguishes temp files written concurrently by threads of this process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write via an owner-only temp file in the same directory, then rename over the target.
///
/// The temp name is unique per process and call, so concurrent writers never share a
/// temp file. It is created exclusively (`create_new`), so a symlink planted at that
/// path makes the write fail instead of being followed. The data is synced before the
/// rename, so a crash cannot leave a truncated file under the final name.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{name}.{}.{n}.remora-tmp", std::process::id()));
    let result = (|| {
        let mut file = platform::create_private_file(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn check_real_dirs_refuses_symlinked_ancestors_and_symlinked_dirs() {
        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        fs::create_dir(root.path().join("real")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        let file = Node::File {
            hash: "0".repeat(64),
            size: 0,
            mode: 0o644,
            stored: true,
        };
        let dir = Node::Dir { mode: 0o755 };

        assert!(check_real_dirs(root.path(), "real/x", &file).is_ok());
        assert!(check_real_dirs(root.path(), "missing", &dir).is_ok());
        assert!(check_real_dirs(root.path(), "link/x", &file).is_err());
        assert!(check_real_dirs(root.path(), "link", &dir).is_err());
        assert!(
            check_real_dirs(root.path(), "real/link-later/x", &file).is_err(),
            "missing ancestor"
        );
    }
}
