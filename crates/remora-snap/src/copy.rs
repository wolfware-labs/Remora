//! Plain-copy backend: a content-addressed blob store plus JSON manifests.
//!
//! Store layout:
//! ```text
//! <store>/blobs/<first 2 hex>/<remaining 62 hex>   file contents, deduplicated by SHA-256
//! <store>/manifests/<snapshot id>.json             one manifest per snapshot
//! ```

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::io_at;
use crate::scan::{Kind, scan};
use crate::{
    Manifest, Node, RestoreReport, SnapError, SnapOptions, Snapshot, SnapshotId, Snapshotter, platform,
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
            fs::create_dir_all(&dir).map_err(io_at(&dir))?;
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

    fn put_blob(&self, bytes: &[u8]) -> Result<String, SnapError> {
        let hash = hash_bytes(bytes);
        let path = self.blob_path(&hash);
        if !path.exists() {
            let dir = path.parent().expect("blob path has a parent");
            fs::create_dir_all(dir).map_err(io_at(dir))?;
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
            write_atomic(abs, &bytes)?;
        }
        platform::set_mode(abs, mode)?;
        Ok(!unchanged)
    }
}

impl Snapshotter for CopySnapshotter {
    fn snapshot(&self, root: &Path) -> Result<Snapshot, SnapError> {
        let root = root.canonicalize().map_err(io_at(root))?;
        let scanned = scan(&root, &self.opts, &self.store)?;
        let mut manifest = Manifest::default();
        let mut unprotected = scanned.non_utf8;

        for (rel, entry) in scanned.entries {
            let node = match entry.kind {
                Kind::Dir { mode } => Node::Dir { mode },
                Kind::Symlink { target } => Node::Symlink { target },
                Kind::Unreadable => {
                    unprotected.push(entry.abs);
                    Node::Opaque
                }
                Kind::File { size, mode } if size > self.opts.max_file_bytes => match hash_file(&entry.abs) {
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
                },
                Kind::File { mode, .. } => match fs::read(&entry.abs) {
                    Ok(bytes) => {
                        let hash = self.put_blob(&bytes)?;
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
                },
            };
            manifest.nodes.insert(rel, node);
        }

        let json = serde_json::to_vec(&manifest).expect("manifest always serializes");
        let id = SnapshotId::parse(&hash_bytes(&json))?;
        let path = self.manifest_path(&id);
        if !path.exists() {
            write_atomic(&path, &json).map_err(io_at(&path))?;
        }
        Ok(Snapshot { id, unprotected })
    }

    fn manifest(&self, id: &SnapshotId) -> Result<Manifest, SnapError> {
        let path = self.manifest_path(id);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(SnapError::NotFound(id.to_string())),
            Err(e) => return Err(io_at(&path)(e)),
        };
        serde_json::from_slice(&bytes).map_err(|source| SnapError::CorruptManifest {
            id: id.to_string(),
            source,
        })
    }

    fn restore(&self, root: &Path, id: &SnapshotId) -> Result<RestoreReport, SnapError> {
        let manifest = self.manifest(id)?;
        fs::create_dir_all(root).map_err(io_at(root))?;
        let root = root.canonicalize().map_err(io_at(root))?;
        let current = scan(&root, &self.opts, &self.store)?;
        let mut report = RestoreReport::default();

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
                Err(e) => report.failed.push((rel.clone(), e.to_string())),
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
            let abs = root.join(rel);
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
                && let Err(e) = platform::set_mode(&root.join(rel), *mode)
            {
                report.failed.push((rel.clone(), e.to_string()));
            }
        }

        Ok(report)
    }
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

/// Write via a temp file in the same directory, then rename over the target.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = path.with_file_name(format!(".{name}.remora-tmp"));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
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
