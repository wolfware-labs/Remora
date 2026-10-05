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
use crate::{Manifest, Node, SnapError, SnapOptions, Snapshot, SnapshotId, Snapshotter};

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
