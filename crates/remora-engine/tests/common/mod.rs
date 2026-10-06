//! Helpers shared by the engine test suites.
#![allow(dead_code, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use remora_engine::{Engine, Project, SessionRef};
use tempfile::TempDir;

/// A data directory and a project directory, both deleted on drop.
pub struct Fixture {
    pub data: TempDir,
    pub project: TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        Self {
            data: TempDir::new().unwrap(),
            project: TempDir::new().unwrap(),
        }
    }

    pub fn root(&self) -> &Path {
        self.project.path()
    }

    pub fn engine(&self) -> Engine {
        Engine::open(self.data.path()).unwrap()
    }

    pub fn open(&self) -> Project {
        self.engine().project(self.root()).unwrap()
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

pub fn agent() -> SessionRef {
    SessionRef::new("session-1", "claude-code")
}

/// Every file under `root` with its contents (directories as `None`).
pub fn tree(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(Result::unwrap)
        .map(|e| {
            let rel = e
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let contents = e.file_type().is_file().then(|| fs::read(e.path()).unwrap());
            (rel, contents)
        })
        .collect()
}
