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
