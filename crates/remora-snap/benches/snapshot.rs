//! Snapshot and restore speed on a generated project. Run with `cargo bench -p remora-snap`;
//! set `REMORA_TEST_FS` to measure on another filesystem.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use remora_snap::{CopySnapshotter, SnapOptions, Snapshotter};
use tempfile::TempDir;

fn temp_dir() -> TempDir {
    match std::env::var_os("REMORA_TEST_FS") {
        Some(dir) => TempDir::new_in(dir).expect("a temporary folder"),
        None => TempDir::new().expect("a temporary folder"),
    }
}

/// `files` files of about 4 KiB in folders of 100.
fn project(files: usize) -> TempDir {
    let dir = temp_dir();
    for i in 0..files {
        let folder = dir.path().join(format!("d{}", i / 100));
        fs::create_dir_all(&folder).expect("a folder");
        let file = folder.join(format!("f{i}.txt"));
        fs::write(&file, format!("file {i} ").repeat(400)).expect("a file");
        // Untouched for an hour: files changed in the last two seconds are always re-read.
        fs::File::options()
            .write(true)
            .open(&file)
            .and_then(|f| f.set_modified(SystemTime::now() - Duration::from_secs(3_600)))
            .expect("an old file");
    }
    dir
}

fn store() -> (TempDir, CopySnapshotter) {
    let dir = temp_dir();
    let snap = CopySnapshotter::new(dir.path(), SnapOptions::default()).expect("a store");
    (dir, snap)
}

/// Change one file in a hundred.
fn touch_one_percent(root: &Path, files: usize) {
    for i in (0..files).step_by(100) {
        let file = root.join(format!("d{}", i / 100)).join(format!("f{i}.txt"));
        fs::write(file, format!("changed {i} ").repeat(400)).expect("a file");
    }
}

fn snapshots(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot");
    group.sample_size(10);
    for files in [1_000, 10_000] {
        let root = project(files);
        group.bench_function(format!("first, {files} files"), |b| {
            b.iter_batched(
                store,
                |(_dir, snap)| snap.snapshot(root.path()).expect("a snapshot"),
                BatchSize::PerIteration,
            );
        });
        let (_dir, snap) = store();
        snap.snapshot(root.path()).expect("a snapshot");
        group.bench_function(format!("unchanged, {files} files"), |b| {
            b.iter(|| snap.snapshot(root.path()).expect("a snapshot"));
        });
        group.bench_function(format!("1% changed, {files} files"), |b| {
            b.iter_batched(
                || touch_one_percent(root.path(), files),
                |()| snap.snapshot(root.path()).expect("a snapshot"),
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

fn restores(c: &mut Criterion) {
    let mut group = c.benchmark_group("restore");
    group.sample_size(10);
    let files = 1_000;
    let root = project(files);
    let (_dir, snap) = store();
    let id = snap.snapshot(root.path()).expect("a snapshot").id;
    group.bench_function(format!("{files} files after all changed"), |b| {
        b.iter_batched(
            || {
                for i in 0..files {
                    let file = root
                        .path()
                        .join(format!("d{}", i / 100))
                        .join(format!("f{i}.txt"));
                    fs::write(file, "changed").expect("a file");
                }
            },
            |()| snap.restore(root.path(), &id).expect("a restore"),
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

criterion_group!(benches, snapshots, restores);
criterion_main!(benches);
