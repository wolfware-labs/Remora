//! The store holds copies of project files, secrets included: it must be owner-only,
//! and concurrent writers must never corrupt a content-addressed blob.
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;
use std::path::Path;
use std::thread;

use common::Fixture;
use remora_snap::{CopySnapshotter, SnapOptions, Snapshotter};
use sha2::{Digest, Sha256};

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .map(Result::unwrap)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect()
}

#[cfg(unix)]
#[test]
fn store_directories_are_owner_only() {
    let fx = Fixture::new();
    fx.write("a.txt", "a");
    let store = fx.store.path().join("fresh-store");
    let snap = CopySnapshotter::new(&store, SnapOptions::default()).unwrap();

    snap.snapshot(fx.root()).unwrap();

    for dir in [store.join("blobs"), store.join("manifests")] {
        assert_eq!(mode(&dir), 0o700, "{}", dir.display());
    }
    for entry in fs::read_dir(store.join("blobs")).unwrap() {
        assert_eq!(mode(&entry.unwrap().path()), 0o700);
    }
}

#[cfg(unix)]
#[test]
fn blobs_and_manifests_are_owner_only() {
    let fx = Fixture::new();
    fx.write(".env", "API_KEY=secret");
    let snap = fx.snapshotter();

    snap.snapshot(fx.root()).unwrap();

    let files = files_under(fx.store.path());
    assert_eq!(files.len(), 2, "one blob and one manifest");
    for file in files {
        assert_eq!(mode(&file), 0o600, "{}", file.display());
    }
}

#[test]
fn concurrent_snapshots_leave_only_valid_blobs_and_no_temp_files() {
    let fx = Fixture::new();
    for i in 0..20 {
        fx.write(&format!("file-{i}.txt"), &format!("contents {i}").repeat(500));
    }

    thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| fx.snapshotter().snapshot(fx.root()).unwrap());
        }
    });

    let blobs = files_under(&fx.store.path().join("blobs"));
    assert_eq!(blobs.len(), 20);
    for blob in &blobs {
        let name: String = blob
            .iter()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|c| c.to_str().unwrap())
            .collect();
        assert_eq!(
            hex::encode(Sha256::digest(fs::read(blob).unwrap())),
            name,
            "corrupt blob"
        );
    }
    let leftovers: Vec<_> = files_under(fx.store.path())
        .into_iter()
        .filter(|p| p.to_str().unwrap().ends_with(".remora-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
