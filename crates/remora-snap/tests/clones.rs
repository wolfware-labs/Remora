//! Copy-on-write: on a filesystem that clones (`REMORA_EXPECT_CLONES=1`), every stored and
//! restored file is cloned; on one that does not (`=0`), every file is copied.
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use common::{Fixture, check_restore_clones, check_snapshot_clones};
use remora_snap::Snapshotter;

#[test]
fn stored_files_are_cloned_where_the_filesystem_can() {
    let fx = Fixture::new();
    fx.write("a.txt", "alpha");
    fx.write("src/b.rs", "fn b() {}");
    fx.write("src/deep/c.bin", &"c".repeat(64 * 1024));

    let s = fx.snapshotter().snapshot(fx.root()).unwrap();

    assert_eq!(s.hashed_files, 3);
    check_snapshot_clones(&s);
}

#[test]
fn restored_files_are_cloned_where_the_filesystem_can() {
    let fx = Fixture::new();
    fx.write("a.txt", "alpha");
    fx.write("src/b.rs", "fn b() {}");
    fx.write("keep.txt", "unchanged");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();
    std::fs::remove_file(fx.root().join("a.txt")).unwrap();
    fx.write("src/b.rs", "changed");

    let report = snap.restore(fx.root(), &s.id).unwrap();

    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(
        (fx.read("a.txt").as_str(), fx.read("src/b.rs").as_str()),
        ("alpha", "fn b() {}")
    );
    assert_eq!(report.written, 2, "the unchanged file is not rewritten");
    check_restore_clones(&report, 2);
}

#[test]
fn files_with_spaces_and_unicode_in_their_path_are_cloned_too() {
    let fx = Fixture::new();
    fx.write("mi proyecto ñandú 🦀/a.txt", "a");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();
    check_snapshot_clones(&s);
    std::fs::remove_file(fx.root().join("mi proyecto ñandú 🦀/a.txt")).unwrap();

    let report = snap.restore(fx.root(), &s.id).unwrap();

    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(fx.read("mi proyecto ñandú 🦀/a.txt"), "a");
    check_restore_clones(&report, 1);
}

#[test]
fn a_stored_blob_is_fresh_even_when_its_file_is_old() {
    // A clone keeps its source's times on some filesystems (APFS); a blob that looked old
    // could be taken for a crash's leftover while it is still being stored.
    let fx = Fixture::new();
    fx.write("old.txt", "written long ago");
    let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
    std::fs::File::options()
        .write(true)
        .open(fx.root().join("old.txt"))
        .unwrap()
        .set_modified(two_hours_ago)
        .unwrap();
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();
    let blobs: Vec<_> = walkdir::WalkDir::new(fx.store.path().join("blobs"))
        .into_iter()
        .map(Result::unwrap)
        .filter(|e| e.file_type().is_file())
        .collect();
    assert_eq!(blobs.len(), 1);
    let age = blobs[0]
        .metadata()
        .unwrap()
        .modified()
        .unwrap()
        .elapsed()
        .unwrap_or_default();
    assert!(
        age < std::time::Duration::from_secs(60 * 60),
        "blob looks {age:?} old"
    );
    check_snapshot_clones(&s);
}

#[cfg(unix)]
#[test]
fn a_read_only_file_is_stored_and_restored() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write("ro.txt", "read only");
    std::fs::set_permissions(fx.root().join("ro.txt"), std::fs::Permissions::from_mode(0o444)).unwrap();
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();
    std::fs::remove_file(fx.root().join("ro.txt")).unwrap();
    let report = snap.restore(fx.root(), &s.id).unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(fx.read("ro.txt"), "read only");
    let mode = std::fs::metadata(fx.root().join("ro.txt"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o444, "the file's own mode comes back");
    check_restore_clones(&report, 1);
}
