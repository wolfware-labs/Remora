//! The stat cache must skip unchanged files without ever missing a real change.
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use common::Fixture;
use remora_snap::Snapshotter;
use sha2::{Digest, Sha256};

fn set_mtime(path: &Path, when: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

fn minutes_ago(n: u64) -> SystemTime {
    SystemTime::now() - Duration::from_secs(60 * n)
}

#[test]
fn unchanged_files_are_not_read_again() {
    let fx = Fixture::new();
    fx.write("a.txt", "a");
    fx.write("b.txt", "b");
    for f in ["a.txt", "b.txt"] {
        set_mtime(&fx.root().join(f), minutes_ago(5));
    }
    let snap = fx.snapshotter();

    let first = snap.snapshot(fx.root()).unwrap();
    let second = snap.snapshot(fx.root()).unwrap();

    assert_eq!(first.hashed_files, 2);
    assert_eq!(second.hashed_files, 0);
    assert_eq!(first.id, second.id);
}

#[test]
fn an_edit_that_keeps_size_and_a_recent_mtime_is_still_caught() {
    let fx = Fixture::new();
    let path = fx.root().join("a.txt");
    fx.write("a.txt", "aaaa");
    let snap = fx.snapshotter();
    let first = snap.snapshot(fx.root()).unwrap();
    let mtime = fs::metadata(&path).unwrap().modified().unwrap();

    // Same size, same mtime: invisible to metadata. The 2-second window must catch it.
    fx.write("a.txt", "bbbb");
    set_mtime(&path, mtime);
    let second = snap.snapshot(fx.root()).unwrap();

    assert_ne!(first.id, second.id);
    assert_eq!(second.hashed_files, 1);
}

#[test]
fn an_old_file_edited_with_a_new_mtime_is_read_again() {
    let fx = Fixture::new();
    let path = fx.root().join("a.txt");
    fx.write("a.txt", "aaaa");
    set_mtime(&path, minutes_ago(10));
    let snap = fx.snapshotter();
    let first = snap.snapshot(fx.root()).unwrap();

    fx.write("a.txt", "bbbb");
    set_mtime(&path, minutes_ago(5));
    let second = snap.snapshot(fx.root()).unwrap();

    assert_ne!(first.id, second.id);
    assert_eq!(second.hashed_files, 1);
}

#[test]
fn a_cache_hit_is_not_trusted_when_its_blob_is_missing() {
    let fx = Fixture::new();
    fx.write("a.txt", "hello");
    set_mtime(&fx.root().join("a.txt"), minutes_ago(5));
    let snap = fx.snapshotter();
    snap.snapshot(fx.root()).unwrap();
    let hash = hex::encode(Sha256::digest(b"hello"));
    let blob = fx.store.path().join("blobs").join(&hash[..2]).join(&hash[2..]);
    fs::remove_file(&blob).unwrap();

    let second = snap.snapshot(fx.root()).unwrap();

    assert_eq!(second.hashed_files, 1);
    assert!(blob.exists(), "the blob must be stored again");
}

#[test]
fn deleted_files_drop_out_of_the_cache() {
    let fx = Fixture::new();
    fx.write("a.txt", "a");
    set_mtime(&fx.root().join("a.txt"), minutes_ago(5));
    let snap = fx.snapshotter();
    snap.snapshot(fx.root()).unwrap();

    fs::remove_file(fx.root().join("a.txt")).unwrap();
    snap.snapshot(fx.root()).unwrap();

    let cache = fs::read_to_string(fx.store.path().join("statcache.json")).unwrap();
    assert!(!cache.contains("a.txt"), "{cache}");
}

#[test]
fn a_file_still_racy_when_read_is_never_trusted_later() {
    // Snapshot 1 reads the file within its racy window; the file is then rewritten in the
    // same timestamp tick (same size, same mtime). Long after, snapshot 2 must not trust
    // the hash snapshot 1 read.
    let fx = Fixture::new();
    let path = fx.root().join("a.txt");
    fx.write("a.txt", "aaaa");
    let snap = fx.snapshotter();
    let first = snap.snapshot(fx.root()).unwrap();
    let mtime = fs::metadata(&path).unwrap().modified().unwrap();
    fx.write("a.txt", "bbbb");
    set_mtime(&path, mtime);

    std::thread::sleep(Duration::from_millis(2_100));
    let second = snap.snapshot(fx.root()).unwrap();

    assert_ne!(first.id, second.id, "snapshot 2 recorded the stale content");
    assert_eq!(second.hashed_files, 1);
}

#[test]
fn a_damaged_hash_in_the_cache_is_ignored() {
    let fx = Fixture::new();
    fx.write("a.txt", "a");
    set_mtime(&fx.root().join("a.txt"), minutes_ago(5));
    let snap = fx.snapshotter();
    snap.snapshot(fx.root()).unwrap();
    let cache_path = fx.store.path().join("statcache.json");
    let hash = hex::encode(Sha256::digest(b"a"));
    let damaged = fs::read_to_string(&cache_path).unwrap().replace(&hash, "a");
    fs::write(&cache_path, damaged).unwrap();

    let second = snap.snapshot(fx.root()).unwrap();

    assert_eq!(
        second.hashed_files, 1,
        "the damaged entry must be ignored and the file read again"
    );
    assert!(snap.manifest(&second.id).is_ok());
}
