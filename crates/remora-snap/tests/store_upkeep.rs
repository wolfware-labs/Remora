//! The store records how each snapshot was taken and repairs entries it finds damaged.
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;

use common::Fixture;
use remora_snap::{CopySnapshotter, RecordedOptions, SnapOptions, Snapshotter};
use sha2::{Digest, Sha256};

#[test]
fn the_manifest_records_the_options_it_was_taken_with() {
    let fx = Fixture::new();
    fx.write("a.txt", "a");
    let opts = SnapOptions {
        max_file_bytes: 1234,
        ..SnapOptions::default()
    };
    let snap = fx.snapshotter_with(opts.clone());

    let m = snap.manifest(&snap.snapshot(fx.root()).unwrap().id).unwrap();

    assert_eq!(
        m.options,
        Some(RecordedOptions {
            exclude_dirs: opts.exclude_dirs,
            max_file_bytes: 1234
        })
    );
}

#[test]
fn restore_leaves_alone_a_dir_excluded_when_the_snapshot_was_taken() {
    let fx = Fixture::new();
    fx.write("src/main.rs", "fn main() {}");
    fx.write("vendor/lib/big.c", "vendored");
    let mut with_vendor = SnapOptions::default();
    with_vendor.exclude_dirs.push("vendor".into());
    let s = CopySnapshotter::new(fx.store.path(), with_vendor)
        .unwrap()
        .snapshot(fx.root())
        .unwrap();

    // Restore with today's options, which no longer exclude vendor/.
    let report = fx.snapshotter().restore(fx.root(), &s.id).unwrap();

    assert_eq!(
        fx.read("vendor/lib/big.c"),
        "vendored",
        "a dir the snapshot never saw must survive"
    );
    assert!(report.failed.is_empty());
}

#[test]
fn a_truncated_blob_is_rewritten_by_the_next_snapshot() {
    let fx = Fixture::new();
    fx.write("a.txt", "hello world");
    let snap = fx.snapshotter();
    snap.snapshot(fx.root()).unwrap();
    let hash = hex::encode(Sha256::digest(b"hello world"));
    let blob = fx.store.path().join("blobs").join(&hash[..2]).join(&hash[2..]);
    fs::write(&blob, "hello").unwrap();

    let s = snap.snapshot(fx.root()).unwrap();
    fx.write("a.txt", "changed");
    snap.restore(fx.root(), &s.id).unwrap();

    assert_eq!(fs::read(&blob).unwrap(), b"hello world");
    assert_eq!(fx.read("a.txt"), "hello world");
}

#[test]
fn a_damaged_manifest_is_rewritten_by_the_next_snapshot_of_the_same_tree() {
    let fx = Fixture::new();
    fx.write("a.txt", "a");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();
    let path = fx.store.path().join("manifests").join(format!("{}.json", s.id));
    fs::write(&path, b"{\"nodes\":").unwrap();
    assert!(snap.manifest(&s.id).is_err());

    let again = snap.snapshot(fx.root()).unwrap();

    assert_eq!(again.id, s.id);
    assert!(snap.manifest(&s.id).is_ok());
}

#[cfg(unix)]
#[test]
fn opening_a_store_tightens_loose_directory_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let store = fx.store.path().join("store");
    drop(CopySnapshotter::new(&store, SnapOptions::default()).unwrap());
    for dir in [store.clone(), store.join("blobs"), store.join("manifests")] {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    }

    drop(CopySnapshotter::new(&store, SnapOptions::default()).unwrap());

    for dir in [store.clone(), store.join("blobs"), store.join("manifests")] {
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700,
            "{}",
            dir.display()
        );
    }
}

#[test]
fn opening_a_store_removes_temp_files_a_crash_left_but_not_fresh_ones() {
    let fx = Fixture::new();
    fx.snapshotter();
    let tmp = fx.store.path().join("blobs").join("tmp");
    let old = tmp.join(".blob.1.1.remora-tmp");
    let fresh = tmp.join(".blob.1.2.remora-tmp");
    fs::write(&old, "half a file").unwrap();
    fs::write(&fresh, "being stored right now").unwrap();
    let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(two_hours_ago)
        .unwrap();

    fx.snapshotter();

    assert!(!old.exists(), "a leftover from a crash is removed");
    assert!(fresh.exists(), "another snapshotter may still be storing it");
}

#[test]
fn a_snapshot_leaves_no_temp_files_behind() {
    let fx = Fixture::new();
    fx.write("a.txt", "a");
    fx.write("b.txt", "a"); // the same contents: stored once, the second copy discarded

    fx.snapshotter().snapshot(fx.root()).unwrap();

    let tmp = fx.store.path().join("blobs").join("tmp");
    assert_eq!(fs::read_dir(tmp).unwrap().count(), 0);
}
