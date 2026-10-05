//! Helpers shared by the snapshot test suites.
#![allow(dead_code, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use remora_snap::{CopySnapshotter, SnapOptions};
use tempfile::TempDir;

/// Observable state of one path, compared before and after restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Dir { mode: u32 },
    File { bytes: Vec<u8>, mode: u32 },
    Symlink { target: String },
}

/// A project dir and a separate store dir, both deleted on drop.
pub struct Fixture {
    pub project: TempDir,
    pub store: TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        Self {
            project: TempDir::new().unwrap(),
            store: TempDir::new().unwrap(),
        }
    }

    pub fn root(&self) -> &Path {
        self.project.path()
    }

    pub fn snapshotter(&self) -> CopySnapshotter {
        self.snapshotter_with(SnapOptions::default())
    }

    pub fn snapshotter_with(&self, opts: SnapOptions) -> CopySnapshotter {
        CopySnapshotter::new(self.store.path(), opts).unwrap()
    }

    pub fn write(&self, rel: &str, contents: &str) {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    pub fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root().join(rel)).unwrap()
    }

    pub fn exists(&self, rel: &str) -> bool {
        fs::symlink_metadata(self.root().join(rel)).is_ok()
    }
}

/// Every path under `root` except excluded dir names, without following symlinks.
pub fn tree_state(root: &Path, excluded: &[&str]) -> BTreeMap<String, State> {
    let mut out = BTreeMap::new();
    let walker = walkdir::WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !(e.file_type().is_dir() && excluded.iter().any(|x| e.file_name() == *x)));
    for entry in walker {
        let entry = entry.unwrap();
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "/");
        let meta = entry.path().symlink_metadata().unwrap();
        let state = if meta.file_type().is_symlink() {
            let target = fs::read_link(entry.path()).unwrap();
            State::Symlink {
                target: target.to_str().unwrap().to_owned(),
            }
        } else if meta.is_dir() {
            State::Dir { mode: mode(&meta) }
        } else {
            State::File {
                bytes: fs::read(entry.path()).unwrap(),
                mode: mode(&meta),
            }
        };
        out.insert(rel, state);
    }
    out
}

#[cfg(unix)]
fn mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn mode(meta: &fs::Metadata) -> u32 {
    u32::from(meta.permissions().readonly())
}

/// Default exclusions, for `tree_state`.
pub const EXCLUDED: &[&str] = &["node_modules", "target", ".venv", ".remora"];
