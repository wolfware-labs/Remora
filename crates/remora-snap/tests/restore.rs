#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;

use common::{EXCLUDED, Fixture, tree_state};
use remora_snap::{CopySnapshotter, SnapError, SnapOptions, SnapshotId, Snapshotter};

#[test]
fn restores_a_deleted_file() {
    let fx = Fixture::new();
    fx.write("src/main.rs", "fn main() {}");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();

    fs::remove_file(fx.root().join("src/main.rs")).unwrap();
    let report = snap.restore(fx.root(), &s.id).unwrap();

    assert_eq!(fx.read("src/main.rs"), "fn main() {}");
    assert_eq!(report.written, 1);
    assert!(report.failed.is_empty());
}

#[test]
fn removes_files_created_after_the_snapshot() {
    let fx = Fixture::new();
    fx.write("keep.txt", "keep");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();

    fx.write("new/dir/junk.txt", "junk");
    let report = snap.restore(fx.root(), &s.id).unwrap();

    assert!(!fx.exists("new"));
    assert!(fx.exists("keep.txt"));
    assert_eq!(report.deleted, 3, "junk.txt, new/dir, new");
    // Each removal is flushed with its parent; a removed folder is not flushed itself.
    assert!(report.failed.is_empty(), "{:?}", report.failed);
}

#[test]
fn reverts_a_modified_file() {
    let fx = Fixture::new();
    fx.write("a.txt", "original");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();

    fx.write("a.txt", "changed by agent");
    snap.restore(fx.root(), &s.id).unwrap();

    assert_eq!(fx.read("a.txt"), "original");
}

#[test]
fn handles_a_file_replaced_by_a_directory() {
    let fx = Fixture::new();
    fx.write("thing", "i am a file");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();

    fs::remove_file(fx.root().join("thing")).unwrap();
    fx.write("thing/inner.txt", "now a dir");
    snap.restore(fx.root(), &s.id).unwrap();

    assert_eq!(fx.read("thing"), "i am a file");
}

#[cfg(unix)]
#[test]
fn restores_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write("run.sh", "#!/bin/sh");
    let path = fx.root().join("run.sh");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();

    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    snap.restore(fx.root(), &s.id).unwrap();

    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o755);
}

#[cfg(unix)]
#[test]
fn restores_a_deleted_symlink_without_following_it() {
    let fx = Fixture::new();
    fx.write("real.txt", "real");
    std::os::unix::fs::symlink("real.txt", fx.root().join("link")).unwrap();
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();

    fs::remove_file(fx.root().join("link")).unwrap();
    snap.restore(fx.root(), &s.id).unwrap();

    assert_eq!(
        fs::read_link(fx.root().join("link")).unwrap().to_str(),
        Some("real.txt")
    );
}

#[cfg(unix)]
#[test]
fn restores_a_symlink_that_was_re_pointed() {
    let fx = Fixture::new();
    fx.write("v1.txt", "1");
    fx.write("v2.txt", "2");
    let link = fx.root().join("current");
    std::os::unix::fs::symlink("v1.txt", &link).unwrap();
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();

    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("v2.txt", &link).unwrap();
    snap.restore(fx.root(), &s.id).unwrap();

    assert_eq!(fs::read_link(&link).unwrap().to_str(), Some("v1.txt"));
}

#[test]
fn leaves_excluded_directories_untouched() {
    let fx = Fixture::new();
    fx.write("node_modules/pkg/index.js", "v1");
    fx.write("app.js", "app");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();

    fx.write("node_modules/pkg/index.js", "v2");
    fx.write("node_modules/new/index.js", "new");
    snap.restore(fx.root(), &s.id).unwrap();

    assert_eq!(fx.read("node_modules/pkg/index.js"), "v2");
    assert!(fx.exists("node_modules/new/index.js"));
}

#[test]
fn oversized_files_are_left_alone() {
    let fx = Fixture::new();
    fx.write("big.bin", "0123456789-more-than-ten-bytes");
    let snap = fx.snapshotter_with(SnapOptions {
        max_file_bytes: 10,
        ..SnapOptions::default()
    });
    let s = snap.snapshot(fx.root()).unwrap();

    fx.write("big.bin", "changed");
    let report = snap.restore(fx.root(), &s.id).unwrap();

    assert_eq!(fx.read("big.bin"), "changed");
    assert_eq!(report.skipped, vec!["big.bin".to_string()]);
}

#[cfg(unix)]
#[test]
fn unreadable_files_are_left_alone() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write("secret.txt", "s3cret");
    let path = fx.root().join("secret.txt");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&path).is_ok() {
        return; // running as root: permissions are not enforced
    }
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();

    let report = snap.restore(fx.root(), &s.id).unwrap();

    assert!(fx.exists("secret.txt"));
    assert_eq!(report.skipped, vec!["secret.txt".to_string()]);
}

#[test]
fn restore_never_deletes_a_store_inside_the_root() {
    let fx = Fixture::new();
    fx.write("a.txt", "a");
    let snap = CopySnapshotter::new(fx.root().join("store-here"), SnapOptions::default()).unwrap();
    let s = snap.snapshot(fx.root()).unwrap();

    snap.restore(fx.root(), &s.id).unwrap();

    assert!(fx.exists("store-here/manifests"));
}

#[test]
fn restoring_an_unknown_snapshot_is_not_found() {
    let fx = Fixture::new();
    let id = SnapshotId::parse(&"0".repeat(64)).unwrap();
    assert!(matches!(
        fx.snapshotter().restore(fx.root(), &id),
        Err(SnapError::NotFound(_))
    ));
}

// --- Review Focus ---

#[test]
fn restores_after_the_whole_project_dir_was_deleted() {
    let fx = Fixture::new();
    fx.write("src/lib.rs", "pub fn x() {}");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();
    let before = tree_state(fx.root(), EXCLUDED);

    fs::remove_dir_all(fx.root()).unwrap(); // the agent ran `rm -rf .`
    let report = snap.restore(fx.root(), &s.id).unwrap();

    assert_eq!(tree_state(fx.root(), EXCLUDED), before);
    // The folders it created are flushed with their parents.
    assert!(report.failed.is_empty(), "{:?}", report.failed);
}

#[test]
fn restoring_twice_is_a_no_op_the_second_time() {
    let fx = Fixture::new();
    fx.write("a.txt", "a");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();
    fx.write("a.txt", "b");
    fx.write("c.txt", "c");

    snap.restore(fx.root(), &s.id).unwrap();
    let second = snap.restore(fx.root(), &s.id).unwrap();

    assert_eq!((second.written, second.deleted), (0, 0));
    assert!(second.failed.is_empty());
}

#[test]
fn unicode_and_space_names_round_trip() {
    let fx = Fixture::new();
    fx.write("dír con espacios/ñandú 🦀.txt", "hola");
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();
    let before = tree_state(fx.root(), EXCLUDED);

    fs::remove_dir_all(fx.root().join("dír con espacios")).unwrap();
    snap.restore(fx.root(), &s.id).unwrap();

    assert_eq!(tree_state(fx.root(), EXCLUDED), before);
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_names_survive_restore() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let fx = Fixture::new();
    let weird = fx.root().join(OsStr::from_bytes(b"bad-\xff-name"));
    fs::write(&weird, "x").unwrap();
    let snap = fx.snapshotter();
    let s = snap.snapshot(fx.root()).unwrap();

    snap.restore(fx.root(), &s.id).unwrap();

    assert!(weird.exists(), "restore must not delete paths it cannot name");
}
