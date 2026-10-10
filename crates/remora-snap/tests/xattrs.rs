//! A blob holds a file's contents alone: extended attributes a clone would carry
//! (quarantine flags, Finder tags) are cleared.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used)] // tests may panic on failure

use remora_snap::{CopySnapshotter, Node, SnapOptions, Snapshotter};

#[test]
fn a_stored_blob_has_no_extended_attributes() {
    let project = tempfile::TempDir::new().unwrap();
    let store = tempfile::TempDir::new().unwrap();
    let file = project.path().join("download.zip");
    std::fs::write(&file, "contents").unwrap();
    xattr::set(&file, "com.apple.quarantine", b"0081;00000000;Safari;").unwrap();
    xattr::set(&file, "user.remora-test", b"tag").unwrap();
    let snap = CopySnapshotter::new(store.path(), SnapOptions::default()).unwrap();

    let id = snap.snapshot(project.path()).unwrap().id;

    let Some(Node::File { hash, .. }) = snap.manifest(&id).unwrap().nodes.get("download.zip").cloned() else {
        panic!("not stored");
    };
    let blob = snap.blobs_dir().join(&hash[..2]).join(&hash[2..]);
    assert_eq!(xattr::list(&blob).unwrap().count(), 0);
    assert_eq!(
        xattr::list(&file).unwrap().count(),
        2,
        "the project's file keeps its own"
    );
}
