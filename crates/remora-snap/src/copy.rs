//! The snapshot store: a content-addressed blob store plus JSON manifests.
//!
//! Store layout:
//! ```text
//! <store>/blobs/<first 2 hex>/<remaining 62 hex>   file contents, deduplicated by SHA-256
//! <store>/blobs/tmp/                               files being stored (leftovers removed when opened)
//! <store>/manifests/<snapshot id>.json             one manifest per snapshot
//! <store>/statcache.json                           metadata → hash cache (see `statcache`)
//! ```
//!
//! Files go into and come out of the blob store by cloning (copy-on-write: APFS, btrfs,
//! XFS, ReFS) where the filesystem can, else by copying, one file at a time.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

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
    /// `<store>/blobs`, or a folder on the project's own volume (see `with_blobs`).
    blobs: PathBuf,
    opts: SnapOptions,
}

impl CopySnapshotter {
    /// Open (or create) a store at `store`, with its blobs in `<store>/blobs`.
    pub fn new(store: impl Into<PathBuf>, opts: SnapOptions) -> Result<Self, SnapError> {
        let store = store.into();
        let blobs = store.join("blobs");
        Self::with_blobs(store, blobs, opts)
    }

    /// Open (or create) a store at `store` whose blobs live in `blobs`: a folder on the
    /// project's own volume, so its files can be cloned into it.
    pub fn with_blobs(
        store: impl Into<PathBuf>,
        blobs: impl Into<PathBuf>,
        opts: SnapOptions,
    ) -> Result<Self, SnapError> {
        let (store, blobs) = (store.into(), blobs.into());
        for dir in [&blobs, &store.join("manifests")] {
            platform::create_private_dir_all(dir).map_err(io_at(dir))?;
        }
        // A store created before permissions were enforced (or loosened since) is tightened.
        for dir in [store.clone(), blobs.clone(), store.join("manifests")] {
            platform::tighten_dir(&dir).map_err(io_at(&dir))?;
        }
        let store = store.canonicalize().map_err(io_at(&store))?;
        let blobs = blobs.canonicalize().map_err(io_at(&blobs))?;
        let tmp = blobs.join("tmp");
        platform::create_private_dir_all(&tmp).map_err(io_at(&tmp))?;
        remove_stale_temps(&tmp, STALE_TEMP);
        Ok(Self { store, blobs, opts })
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.blobs.join(&hash[..2]).join(&hash[2..])
    }

    /// Where this store keeps its blobs.
    pub fn blobs_dir(&self) -> &Path {
        &self.blobs
    }

    fn manifest_path(&self, id: &SnapshotId) -> PathBuf {
        self.store.join("manifests").join(format!("{id}.json"))
    }

    fn statcache_path(&self) -> PathBuf {
        self.store.join("statcache.json")
    }

    /// Store the file at `abs` as a blob: clone it (or copy it) into `blobs/tmp`, hash that
    /// copy, which cannot change under us, and move it to its place.
    fn store_file(&self, abs: &Path) -> Result<Stored, StoreError> {
        // An unreadable or vanished file is the project's problem, not the store's.
        File::open(abs).map_err(StoreError::Source)?;
        let tmp = self.blobs.join("tmp").join(temp_name("blob"));
        let result = (|| {
            let (tmp, cloned) = clone_or_copy(abs, &tmp).map_err(|e| match e {
                CopyError::Read(e) => StoreError::Source(e),
                CopyError::Write(e) => StoreError::Store(io_at(&tmp)(e)),
            })?;
            let (hash, size) = hash_and_size(&tmp).map_err(|e| StoreError::Store(io_at(&tmp)(e)))?;
            let path = self.blob_path(&hash);
            // A blob whose size is wrong (say, truncated by a crash) is replaced.
            if fs::metadata(&path).is_ok_and(|m| m.len() == size) {
                let _ = fs::remove_file(&tmp);
            } else {
                let dir = path.parent().expect("blob path has a parent");
                platform::create_private_dir_all(dir).map_err(|e| StoreError::Store(io_at(dir)(e)))?;
                fs::rename(&tmp, &path).map_err(|e| StoreError::Store(io_at(&path)(e)))?;
            }
            Ok(Stored { hash, size, cloned })
        })();
        if result.is_err() {
            remove_temps(&tmp);
        }
        result
    }

    /// Make `abs` hold blob `hash` with `mode`, unless it already does.
    fn restore_file(&self, abs: &Path, hash: &str, mode: u32) -> io::Result<Restored> {
        let unchanged =
            fs::symlink_metadata(abs).is_ok_and(|m| m.is_file()) && hash_file(abs).is_ok_and(|h| h == hash);
        if unchanged {
            platform::set_mode(abs, mode)?;
            return Ok(Restored::Unchanged);
        }
        let blob = self.blob_path(hash);
        let tmp = abs.with_file_name(temp_name(
            abs.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        ));
        let result = (|| {
            let (tmp, cloned) = clone_or_copy(&blob, &tmp).map_err(|e| {
                let e = io::Error::from(e);
                io::Error::new(e.kind(), format!("reading blob {hash}: {e}"))
            })?;
            if hash_file(&tmp)? != hash {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("blob {hash} is corrupt (its contents do not match its hash)"),
                ));
            }
            platform::set_mode(&tmp, mode)?;
            fs::rename(&tmp, abs)?;
            Ok(Restored::Written { cloned })
        })();
        if result.is_err() {
            remove_temps(&tmp);
        }
        result
    }
}

/// A file put into the blob store.
struct Stored {
    hash: String,
    size: u64,
    cloned: bool,
}

/// Why a file could not be stored.
enum StoreError {
    /// Reading the project's file failed: the file is left unprotected.
    Source(io::Error),
    /// Writing the store failed: the snapshot fails.
    Store(SnapError),
}

/// What restoring one file did.
enum Restored {
    Unchanged,
    Written { cloned: bool },
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
        let mut cloned_files = 0;

        for (rel, entry) in scanned.entries {
            let node = match entry.kind {
                Kind::Dir { mode } => Node::Dir { mode },
                Kind::Symlink { target } => Node::Symlink { target },
                Kind::Unreadable => {
                    unprotected.push(entry.abs);
                    Node::Opaque
                }
                Kind::File {
                    size,
                    mode,
                    mtime_ns,
                    ino,
                    ctime_ns,
                } if size > self.opts.max_file_bytes => {
                    // Too large to store: recorded by hash only. The hash goes through the stat
                    // cache like any other file's, so an unchanged large file is not re-read on
                    // every snapshot.
                    let key = entry.abs.to_string_lossy().into_owned();
                    let cached = mtime_ns
                        .and_then(|m| old_cache.lookup(&key, size, m, ino, ctime_ns, started_ns))
                        .filter(|hash| is_sha256_hex(hash))
                        .map(str::to_owned);
                    let hashed = match cached {
                        Some(hash) => Ok((hash, true)),
                        None => {
                            hashed_files += 1;
                            // Cache it only if the file did not change while it was hashed.
                            hash_file(&entry.abs).map(|hash| {
                                let unchanged = fs::metadata(&entry.abs).is_ok_and(|meta| {
                                    meta.len() == size && platform::mtime_ns(&meta) == mtime_ns
                                });
                                (hash, unchanged)
                            })
                        }
                    };
                    match hashed {
                        Ok((hash, cacheable)) => {
                            if let Some(mtime_ns) = mtime_ns
                                && cacheable
                                && !is_racy(mtime_ns, started_ns)
                            {
                                cache.insert(
                                    key,
                                    CachedStat {
                                        size,
                                        mtime_ns,
                                        ino,
                                        ctime_ns,
                                        hash: hash.clone(),
                                    },
                                );
                            }
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
                    ctime_ns,
                } => {
                    let key = entry.abs.to_string_lossy().into_owned();
                    let cached = mtime_ns
                        .and_then(|m| old_cache.lookup(&key, size, m, ino, ctime_ns, started_ns))
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
                                ctime_ns,
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
                        match self.store_file(&entry.abs) {
                            Ok(Stored {
                                hash,
                                size: stored_size,
                                cloned,
                            }) => {
                                hashed_files += 1;
                                cloned_files += usize::from(cloned);
                                // Only cache what was stored if the file did not change since the scan,
                                // and never while it is racy (see `statcache`).
                                if let Some(mtime_ns) = mtime_ns
                                    && stored_size == size
                                    && !is_racy(mtime_ns, started_ns)
                                {
                                    cache.insert(
                                        key,
                                        CachedStat {
                                            size,
                                            mtime_ns,
                                            ino,
                                            ctime_ns,
                                            hash: hash.clone(),
                                        },
                                    );
                                }
                                Node::File {
                                    hash,
                                    size: stored_size,
                                    mode,
                                    stored: true,
                                }
                            }
                            Err(StoreError::Store(e)) => return Err(e),
                            Err(StoreError::Source(e)) if e.kind() == io::ErrorKind::NotFound => continue,
                            Err(StoreError::Source(_)) => {
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
            cloned_files,
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
                Node::Dir { .. } => fs::create_dir_all(&abs).map(|()| Restored::Unchanged),
                Node::File { hash, mode, .. } => self.restore_file(&abs, hash, *mode),
                Node::Symlink { target } => restore_symlink(&abs, target).map(|written| {
                    if written {
                        Restored::Written { cloned: false }
                    } else {
                        Restored::Unchanged
                    }
                }),
                Node::Opaque => Ok(Restored::Unchanged),
            };
            match result {
                Ok(Restored::Written { cloned }) => {
                    report.written += 1;
                    report.cloned += usize::from(cloned);
                }
                Ok(Restored::Unchanged) => {}
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
    let tmp = path.with_file_name(temp_name(
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
    ));
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

/// How old a file in `blobs/tmp` must be to count as left behind by a crash. Another
/// snapshotter may be storing into the same store right now; none takes this long per file.
const STALE_TEMP: Duration = Duration::from_secs(60 * 60);

/// Remove the files in `dir` last modified more than `age` ago. Best effort: a store that
/// keeps some leftovers still works.
fn remove_stale_temps(dir: &Path, age: Duration) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|modified| modified.elapsed().is_ok_and(|elapsed| elapsed > age));
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// A temp file name for `name`, unique per process and call.
fn temp_name(name: &str) -> String {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(".{name}.{}.{n}.remora-tmp", std::process::id())
}

/// Why `clone_or_copy` failed: reading the source, or writing the copy.
enum CopyError {
    Read(io::Error),
    Write(io::Error),
}

impl From<CopyError> for io::Error {
    fn from(e: CopyError) -> Self {
        match e {
            CopyError::Read(e) | CopyError::Write(e) => e,
        }
    }
}

/// Clone `from` to the new file `to` (copy-on-write) where the filesystem can, else copy it
/// to `to` with `.copy` appended: Windows may still hold the file of a failed clone, pending
/// deletion, under the first name. Returns the file written and whether it was cloned. Like
/// every temp file here, both names are created exclusively, so a planted symlink makes this
/// fail instead of being followed. The file written is made writable by its owner, stamped with
/// the current time (a clone keeps its source's, which would make it look like a leftover in
/// `blobs/tmp`) and flushed.
fn clone_or_copy(from: &Path, to: &Path) -> Result<(PathBuf, bool), CopyError> {
    let (written, cloned) = match reflink_copy::reflink(from, to) {
        Ok(()) => (to.to_path_buf(), true),
        // Not ours to remove, and copying would fail the same way.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(CopyError::Write(e)),
        Err(_) => {
            // No clones here (another filesystem, or one without them): copy instead.
            let _ = fs::remove_file(to);
            let copy = copy_name(to);
            let mut source = File::open(from).map_err(CopyError::Read)?;
            let mut target = platform::create_private_file(&copy).map_err(CopyError::Write)?;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = source.read(&mut buf).map_err(CopyError::Read)?;
                if n == 0 {
                    break;
                }
                target.write_all(&buf[..n]).map_err(CopyError::Write)?;
            }
            (copy, false)
        }
    };
    platform::set_private(&written).map_err(CopyError::Write)?;
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&written)
        .map_err(CopyError::Write)?;
    file.set_modified(std::time::SystemTime::now())
        .map_err(CopyError::Write)?;
    file.sync_all().map_err(CopyError::Write)?;
    Ok((written, cloned))
}

/// Where `clone_or_copy` copies when it cannot clone to `to`.
fn copy_name(to: &Path) -> PathBuf {
    let mut name = to.as_os_str().to_owned();
    name.push(".copy");
    PathBuf::from(name)
}

/// Remove whatever `clone_or_copy(_, tmp)` may have left.
fn remove_temps(tmp: &Path) {
    let _ = fs::remove_file(tmp);
    let _ = fs::remove_file(copy_name(tmp));
}

/// The SHA-256 of a file and how many bytes it has.
fn hash_and_size(path: &Path) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((hex::encode(hasher.finalize()), size))
}

fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hash_file(path: &Path) -> io::Result<String> {
    hash_and_size(path).map(|(hash, _)| hash)
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
