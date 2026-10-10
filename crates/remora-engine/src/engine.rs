use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use remora_journal::{SigningKey, load_or_create_key};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::io_at;
use crate::lock::ProjectLock;
use crate::{EngineError, Project, blob_home, fsutil, session};

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
    /// The project's blob folder on its own volume; absent for the data directory's store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blobs: Option<String>,
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
        let root_str = root.to_string_lossy().into_owned();
        let blobs = match read_project_file(&file)? {
            // Recorded once: blobs already stored must always be found again.
            Some(p) => recorded_blobs(p, &root_str, &id)?,
            None => {
                // Choosing a blob home creates a folder on the project's volume: two processes
                // opening new projects there at once must not both try, and a later one must
                // see what the first recorded.
                let _lock = ProjectLock::acquire(&self.data_dir.join("blob-home.lock"))?;
                match read_project_file(&file)? {
                    Some(p) => recorded_blobs(p, &root_str, &id)?,
                    None => {
                        let blobs = blob_home::choose(&root, &self.data_dir, &id);
                        let json = serde_json::to_vec(&ProjectFile {
                            root: root_str,
                            blobs: blobs.as_ref().map(|b| b.to_string_lossy().into_owned()),
                        })
                        .expect("project file serializes");
                        fsutil::write_private_atomic(&file, &json).map_err(io_at(&file))?;
                        blobs
                    }
                }
            }
        };
        if let Some(blobs) = &blobs {
            blob_home::check(blobs, &self.data_dir)?;
        }
        let plain_copies_because = match &blobs {
            None => blob_home::plain_copy_reason(&root, &self.data_dir),
            Some(_) => None,
        };
        let mut project = Project::open(id, root, dir, blobs, self.key.clone())?;
        project.plain_copies_because = plain_copies_because;
        Ok(project)
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

    /// Remove everything Remora holds for the project at `root`: its blob folder on the
    /// project's volume, then its data folder (journal, snapshots, sessions). Refused while
    /// a turn is open, unless `force`. The project must not be open in this process.
    pub fn forget(&self, root: &Path, force: bool) -> Result<Forgotten, EngineError> {
        // A deleted project folder is still forgotten by the path it had.
        let root = root
            .canonicalize()
            .ok()
            .or_else(|| canonical_prefix(root))
            .unwrap_or_else(|| root.to_path_buf());
        let id = project_id(&root);
        let dir = self.data_dir.join("projects").join(&id);
        let recorded = read_project_file(&dir.join("project.json"))?
            .ok_or_else(|| EngineError::UnknownProject(root.clone()))?;
        if Path::new(&recorded.root) != root {
            return Err(EngineError::ProjectMismatch {
                id,
                recorded_root: recorded.root,
            });
        }
        if !force {
            let _lock = ProjectLock::acquire(&dir.join("lock"))?;
            if let Some(open) = session::load_all(&dir.join("sessions"))?.first() {
                return Err(EngineError::TurnInProgress {
                    session: open.session_id.clone(),
                });
            }
        }
        // `<home>/<id>/blobs`: the folder named after the project goes, never more.
        let blob_home = recorded
            .blobs
            .map(PathBuf::from)
            .and_then(|blobs| blobs.parent().map(Path::to_path_buf))
            .filter(|home| home.file_name().is_some_and(|name| name == id.as_str()));
        // The data folder goes last: if anything fails, forgetting again finds the project.
        if let Some(home) = &blob_home {
            remove_all(home)?;
        }
        remove_all(&dir)?;
        Ok(Forgotten {
            data_dir: dir,
            blob_home,
        })
    }

    /// The roots of every project recorded in the data directory. Unreadable project
    /// files are skipped.
    pub fn project_roots(&self) -> Result<Vec<PathBuf>, EngineError> {
        let projects = self.data_dir.join("projects");
        let mut roots = Vec::new();
        for entry in fs::read_dir(&projects).map_err(io_at(&projects))? {
            let file = entry.map_err(io_at(&projects))?.path().join("project.json");
            if let Ok(Some(recorded)) = read_project_file(&file) {
                roots.push(PathBuf::from(recorded.root));
            }
        }
        roots.sort();
        Ok(roots)
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

/// What [`Engine::forget`] removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forgotten {
    pub data_dir: PathBuf,
    /// The project's blob folder on its own volume, if it had one.
    pub blob_home: Option<PathBuf>,
}

/// Remove a folder and everything in it (symlinks are removed, never followed); one
/// that is already gone is fine.
fn remove_all(dir: &Path) -> Result<(), EngineError> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_at(dir)(e)),
    }
}

/// The recorded project file, if there is one.
fn read_project_file(file: &Path) -> Result<Option<ProjectFile>, EngineError> {
    match fs::read(file) {
        Ok(bytes) => serde_json::from_slice::<ProjectFile>(&bytes)
            .map(Some)
            .map_err(|e| EngineError::CorruptState {
                path: file.to_path_buf(),
                detail: e.to_string(),
            }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_at(file)(e)),
    }
}

/// The blob folder a project file records, if it is for the project rooted at `root`.
fn recorded_blobs(recorded: ProjectFile, root: &str, id: &str) -> Result<Option<PathBuf>, EngineError> {
    if recorded.root == root {
        Ok(recorded.blobs.map(PathBuf::from))
    } else {
        Err(EngineError::ProjectMismatch {
            id: id.to_owned(),
            recorded_root: recorded.root,
        })
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
