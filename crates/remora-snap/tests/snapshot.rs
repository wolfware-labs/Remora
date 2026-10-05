#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use common::Fixture;
use remora_snap::{CopySnapshotter, Node, SnapError, SnapOptions, SnapshotId, Snapshotter};

#[test]
fn the_manifest_lists_every_path() {
    let fx = Fixture::new();
    fx.write("src/main.rs", "fn main() {}");
    let snap = fx.snapshotter();

    let s = snap.snapshot(fx.root()).unwrap();
    let m = snap.manifest(&s.id).unwrap();

    assert_eq!(m.nodes.keys().collect::<Vec<_>>(), ["src", "src/main.rs"]);
    assert!(matches!(m.nodes["src"], Node::Dir { .. }));
    assert!(matches!(
        m.nodes["src/main.rs"],
        Node::File {
            size: 12,
            stored: true,
            ..
        }
    ));
    assert!(s.unprotected.is_empty());
}

#[test]
fn identical_trees_get_the_same_id_and_share_blobs() {
    let fx = Fixture::new();
    fx.write("a.txt", "same");
    fx.write("b.txt", "same");
    let snap = fx.snapshotter();

    let first = snap.snapshot(fx.root()).unwrap();
    let second = snap.snapshot(fx.root()).unwrap();

    assert_eq!(first.id, second.id);
    let blob_count = walkdir::WalkDir::new(fx.store.path().join("blobs"))
        .into_iter()
        .filter(|e| e.as_ref().unwrap().file_type().is_file())
        .count();
    assert_eq!(blob_count, 1, "a.txt and b.txt have identical contents");
}

#[test]
fn excluded_directories_are_not_snapshotted() {
    let fx = Fixture::new();
    fx.write("node_modules/pkg/index.js", "v1");
    fx.write("app.js", "app");
    let snap = fx.snapshotter();

    let s = snap.snapshot(fx.root()).unwrap();

    let keys: Vec<_> = snap.manifest(&s.id).unwrap().nodes.into_keys().collect();
    assert_eq!(keys, ["app.js"]);
}

#[test]
fn a_store_inside_the_root_is_not_snapshotted() {
    let fx = Fixture::new();
    fx.write("a.txt", "a");
    let snap = CopySnapshotter::new(fx.root().join("store-here"), SnapOptions::default()).unwrap();

    let s = snap.snapshot(fx.root()).unwrap();

    let keys: Vec<_> = snap.manifest(&s.id).unwrap().nodes.into_keys().collect();
    assert_eq!(keys, ["a.txt"]);
}

#[test]
fn oversized_files_are_recorded_by_hash_but_unprotected() {
    let fx = Fixture::new();
    fx.write("big.bin", "0123456789-more-than-ten-bytes");
    let snap = fx.snapshotter_with(SnapOptions {
        max_file_bytes: 10,
        ..SnapOptions::default()
    });

    let s = snap.snapshot(fx.root()).unwrap();

    assert_eq!(s.unprotected.len(), 1);
    let m = snap.manifest(&s.id).unwrap();
    assert!(matches!(m.nodes["big.bin"], Node::File { stored: false, .. }));
}

#[cfg(unix)]
#[test]
fn symlinks_are_recorded_not_followed() {
    let fx = Fixture::new();
    fx.write("real.txt", "real");
    std::os::unix::fs::symlink("real.txt", fx.root().join("link")).unwrap();
    let snap = fx.snapshotter();

    let s = snap.snapshot(fx.root()).unwrap();

    let m = snap.manifest(&s.id).unwrap();
    assert_eq!(
        m.nodes["link"],
        Node::Symlink {
            target: "real.txt".into()
        }
    );
}

#[cfg(unix)]
#[test]
fn unreadable_files_are_opaque_and_unprotected() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write("secret.txt", "s3cret");
    let path = fx.root().join("secret.txt");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&path).is_ok() {
        return; // running as root: permissions are not enforced
    }
    let snap = fx.snapshotter();

    let s = snap.snapshot(fx.root()).unwrap();

    assert_eq!(s.unprotected, vec![path.canonicalize().unwrap()]);
    assert_eq!(snap.manifest(&s.id).unwrap().nodes["secret.txt"], Node::Opaque);
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_names_are_unprotected() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let fx = Fixture::new();
    std::fs::write(fx.root().join(OsStr::from_bytes(b"bad-\xff-name")), "x").unwrap();
    fx.write("ok.txt", "ok");
    let snap = fx.snapshotter();

    let s = snap.snapshot(fx.root()).unwrap();

    assert_eq!(s.unprotected.len(), 1);
    let keys: Vec<_> = snap.manifest(&s.id).unwrap().nodes.into_keys().collect();
    assert_eq!(keys, ["ok.txt"]);
}

#[test]
fn loading_an_unknown_manifest_is_not_found() {
    let fx = Fixture::new();
    let id = SnapshotId::parse(&"0".repeat(64)).unwrap();
    assert!(matches!(
        fx.snapshotter().manifest(&id),
        Err(SnapError::NotFound(_))
    ));
}

#[test]
fn snapshotting_a_missing_root_is_an_io_error() {
    let fx = Fixture::new();
    let missing = fx.root().join("does-not-exist");
    assert!(matches!(
        fx.snapshotter().snapshot(&missing),
        Err(SnapError::Io { .. })
    ));
}
