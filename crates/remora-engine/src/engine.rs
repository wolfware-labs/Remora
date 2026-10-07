use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use remora_journal::{SigningKey, load_or_create_key};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::io_at;
use crate::{EngineError, Project, fsutil};

/// The per-OS default data directory: Linux `$XDG_DATA_HOME/remora` or
/// `~/.local/share/remora`, macOS `~/Library/Application Support/remora`,
/// Windows `%LOCALAPPDATA%\remora`.
pub fn default_data_dir() -> Result<PathBuf, EngineError> {
    dirs::data_local_dir()
        .map(|d| d.join("remora"))
        .ok_or(EngineError::NoDataDir)
}

/// The project id: the first 16 hex characters of sha256(canonical root).
pub fn project_id(canonical_root: &Path) -> String {
    let digest = Sha256::digest(canonical_root.to_string_lossy().as_bytes());
    hex::encode(digest)[..16].to_owned()
}

#[derive(Debug, Serialize, Deserialize)]
struct ProjectFile {
    root: String,
}

/// Owns the data directory and the machine's signing key.
pub struct Engine {
    data_dir: PathBuf,
    key: SigningKey,
}

impl Engine {
    /// Open (or create) the data directory: owner-only, with the machine signing key.
    pub fn open(data_dir: &Path) -> Result<Self, EngineError> {
        fsutil::private_dir(data_dir).map_err(io_at(data_dir))?;
        let key = load_or_create_key(&data_dir.join("signing.key"))?;
        let projects = data_dir.join("projects");
        fsutil::private_dir(&projects).map_err(io_at(&projects))?;
        let data_dir = data_dir.canonicalize().map_err(io_at(data_dir))?;
        Ok(Self { data_dir, key })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Hex-encoded public key of this machine.
    pub fn verifying_key_hex(&self) -> String {
        hex::encode(self.key.verifying_key().to_bytes())
    }

    /// Open (or create) the data for the project rooted at `root`.
    pub fn project(&self, root: &Path) -> Result<Project, EngineError> {
        let root = self.resolve_root(root)?;
        // A project that contains the data directory would snapshot (and rewind) Remora's
        // own journal, sessions and key; one inside it would rewind other projects' data.
        if root.starts_with(&self.data_dir) || self.data_dir.starts_with(&root) {
            return Err(EngineError::RootOverlapsDataDir {
                root,
                data_dir: self.data_dir.clone(),
            });
        }
        let id = project_id(&root);
        let dir = self.data_dir.join("projects").join(&id);
        fsutil::private_dir(&dir).map_err(io_at(&dir))?;

        let file = dir.join("project.json");
        let recorded = match fs::read(&file) {
            Ok(bytes) => Some(serde_json::from_slice::<ProjectFile>(&bytes).map_err(|e| {
                EngineError::CorruptState {
                    path: file.clone(),
                    detail: e.to_string(),
                }
            })?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_at(&file)(e)),
        };
        let root_str = root.to_string_lossy().into_owned();
        match recorded {
            Some(p) if p.root != root_str => {
                return Err(EngineError::ProjectMismatch {
                    id,
                    recorded_root: p.root,
                });
            }
            Some(_) => {}
            None => {
                let json =
                    serde_json::to_vec(&ProjectFile { root: root_str }).expect("project file serializes");
                fsutil::write_private_atomic(&file, &json).map_err(io_at(&file))?;
            }
        }
        Project::open(id, root, dir, self.key.clone())
    }

    /// The canonical project root. A folder that no longer exists (the agent deleted it)
    /// is recreated, empty, when Remora already has a project there, so its history stays
    /// reachable; an unknown missing folder is an error and nothing is created.
    fn resolve_root(&self, root: &Path) -> Result<PathBuf, EngineError> {
        let missing = match root.canonicalize() {
            Ok(root) => return Ok(root),
            Err(e) if e.kind() == io::ErrorKind::NotFound => e,
            Err(e) => return Err(io_at(root)(e)),
        };
        // The folder itself, or the target of a symlink to it that now dangles.
        let link_target = fs::read_link(root)
            .ok()
            .and_then(|target| root.parent().map(|parent| parent.join(target)));
        let recorded = [Some(root.to_path_buf()), link_target]
            .into_iter()
            .flatten()
            .filter_map(|path| canonical_prefix(&path))
            .find(|path| self.has_project_at(path));
        match recorded {
            Some(recorded) => {
                fs::create_dir_all(&recorded).map_err(io_at(&recorded))?;
                recorded.canonicalize().map_err(io_at(&recorded))
            }
            None => Err(io_at(root)(missing)),
        }
    }

    /// Whether a project is recorded with exactly this canonical root.
    fn has_project_at(&self, root: &Path) -> bool {
        let file = self
            .data_dir
            .join("projects")
            .join(project_id(root))
            .join("project.json");
        fs::read(file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ProjectFile>(&bytes).ok())
            .is_some_and(|recorded| Path::new(&recorded.root) == root)
    }
}

/// `path` with its deepest existing ancestor canonicalized and the missing rest appended:
/// what its canonical form was before the missing part was deleted.
fn canonical_prefix(path: &Path) -> Option<PathBuf> {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(base) = existing.canonicalize() {
            return Some(missing.iter().rev().fold(base, |path, part| path.join(part)));
        }
        missing.push(existing.file_name()?);
        existing = existing.parent()?;
    }
}
