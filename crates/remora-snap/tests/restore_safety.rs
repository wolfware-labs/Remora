//! Restore must never trust the store blindly, and must never write through a path it
//! failed to remove.
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;

use common::Fixture;
use remora_snap::{CopySnapshotter, Manifest, Node, SnapError, SnapOptions, SnapshotId, Snapshotter};
use sha2::{Digest, Sha256};

/// Store a hand-written manifest the way the store does, and return its id.
fn plant_manifest(fx: &Fixture, manifest: &Manifest) -> SnapshotId {
    let json = serde_json::to_vec(manifest).unwrap();
    let id = hex::encode(Sha256::digest(&json));
    fs::create_dir_all(fx.store.path().join("manifests")).unwrap();
    fs::write(fx.store.path().join("manifests").join(format!("{id}.json")), json).unwrap();
    SnapshotId::parse(&id).unwrap()
}

fn blob_path(fx: &Fixture, contents: &[u8]) -> std::path::PathBuf {
    let hash = hex::encode(Sha256::digest(contents));
    fx.store.path().join("blobs").join(&hash[..2]).join(&hash[2..])
}

#[test]
fn a_manifest_key_escaping_the_root_is_rejected() {
    // The project lives inside its own parent dir, so a regression writes there, not into /tmp.
    let outer = tempfile::TempDir::new().unwrap();
    let project = outer.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("a.txt"), "a").unwrap();
    let store = tempfile::TempDir::new().unwrap();
    let snap = CopySnapshotter::new(store.path(), SnapOptions::default()).unwrap();
    let real = snap.manifest(&snap.snapshot(&project).unwrap().id).unwrap();
    let mut evil = Manifest::default();
    evil.nodes
        .insert("../escape.txt".into(), real.nodes["a.txt"].clone());
    let json = serde_json::to_vec(&evil).unwrap();
    let id = hex::encode(Sha256::digest(&json));
    fs::write(store.path().join("manifests").join(format!("{id}.json")), json).unwrap();

    let result = snap.restore(&project, &SnapshotId::parse(&id).unwrap());

    assert!(
        matches!(result, Err(SnapError::InvalidManifest { .. })),
        "{result:?}"
    );
    assert!(
        !outer.path().join("escape.txt").exists(),
        "restore wrote outside the root"
    );
}

#[test]
fn a_malformed_hash_is_rejected_instead_of_panicking() {
    let fx = Fixture::new();
    let mut evil = Manifest::default();
    evil.nodes.insert(
        "a.txt".into(),
        Node::File {
            hash: "ab".into(),
            size: 1,
            mode: 0o644,
            stored: true,
        },
    );
    let id = plant_manifest(&fx, &evil);

    let result = fx.snapshotter().restore(fx.root(), &id);

    assert!(
        matches!(result, Err(SnapError::InvalidManifest { .. })),
        "{result:?}"
    );
}

#[test]
fn a_manifest_that_does_not_match_its_id_is_rejected() {
    let fx = Fixture::new();
    fx.write("a.txt", "a");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();
    let path = fx.store.path().join("manifests").join(format!("{}.json", s.id));
    let mut json = fs::read(&path).unwrap();
    json.push(b' ');
    fs::write(&path, json).unwrap();

    assert!(matches!(
        snap.manifest(&s.id),
        Err(SnapError::InvalidManifest { .. })
    ));
}

#[test]
fn a_missing_blob_aborts_before_anything_changes() {
    let fx = Fixture::new();
    fx.write("a.txt", "original a");
    fx.write("b.txt", "original b");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();
    fx.write("a.txt", "changed a");
    fx.write("c.txt", "new c");
    fs::remove_file(blob_path(&fx, b"original b")).unwrap();

    let result = snap.restore(fx.root(), &s.id);

    assert!(matches!(result, Err(SnapError::MissingBlob(_))), "{result:?}");
    assert_eq!(fx.read("a.txt"), "changed a");
    assert_eq!(fx.read("c.txt"), "new c");
}

#[test]
fn a_corrupt_blob_is_reported_and_never_restored() {
    let fx = Fixture::new();
    fx.write("a.txt", "good");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();
    fs::write(blob_path(&fx, b"good"), "evil").unwrap();
    fx.write("a.txt", "changed");

    let report = snap.restore(fx.root(), &s.id).unwrap();

    assert_eq!(fx.read("a.txt"), "changed");
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0, "a.txt");
}

#[cfg(unix)]
#[test]
fn a_symlink_that_cannot_be_removed_is_never_written_through() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write("p/x/f.txt", "inside x");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();

    // Replace p/x with a symlink to a directory outside the project, then make p read-only
    // so restore cannot remove the symlink.
    let victim = tempfile::TempDir::new().unwrap();
    let p = fx.root().join("p");
    fs::remove_dir_all(p.join("x")).unwrap();
    std::os::unix::fs::symlink(victim.path(), p.join("x")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(p.join("probe"), "").is_ok() {
        return; // running as root: permissions are not enforced
    }

    let report = snap.restore(fx.root(), &s.id).unwrap();

    assert!(
        !victim.path().join("f.txt").exists(),
        "restore wrote through the symlink"
    );
    assert!(report.failed.iter().any(|(rel, _)| rel == "p/x"));
}
