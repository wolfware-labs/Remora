//! Where a project's blobs live: with Remora's data on the same volume, else in a private
//! `.remora-store` folder on the project's own volume.
#![cfg(unix)]
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use common::{agent, test_dir};
use remora_engine::{Engine, EngineError, project_id};
use tempfile::TempDir;

/// The data directory always in the system temp folder; the project on the filesystem under
/// test, which CI mounts as another volume.
fn split() -> (TempDir, TempDir) {
    (TempDir::new().unwrap(), test_dir())
}

#[test]
fn a_project_on_another_volume_keeps_its_blobs_on_that_volume() {
    let (data, project) = split();
    fs::write(project.path().join("a.txt"), "a").unwrap();
    let engine = Engine::open(data.path()).unwrap();
    let root = project.path().canonicalize().unwrap();

    let mut p = engine.project(&root).unwrap();
    p.begin_turn(&agent(), None, 1_000).unwrap();
    let status = p.status().unwrap();

    let same_volume = fs::metadata(data.path()).unwrap().dev() == fs::metadata(&root).unwrap().dev();
    if same_volume {
        assert!(
            status.blob_store.starts_with(engine.data_dir()),
            "{}",
            status.blob_store.display()
        );
        return;
    }
    // On another volume: under the highest folder of that volume the user owns, which is
    // the mount point CI hands over to the runner user.
    let mount = PathBuf::from(std::env::var_os("REMORA_TEST_FS").unwrap());
    let store_dir = mount.canonicalize().unwrap().join(".remora-store");
    assert_eq!(status.blob_store, store_dir.join(project_id(&root)).join("blobs"));
    assert_eq!(
        fs::metadata(&store_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    if let Ok(expect) = std::env::var("REMORA_EXPECT_CLONES") {
        assert_eq!(status.copy_on_write, Some(expect == "1"));
    }
}

/// A project whose data records `blobs` as its blob folder, as `Engine::project` writes it.
fn record_blobs(data: &Path, root: &Path, blobs: &Path) {
    let dir = data.join("projects").join(project_id(root));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("project.json"),
        serde_json::json!({"root": root, "blobs": blobs}).to_string(),
    )
    .unwrap();
}

/// `<parent>/.remora-store/<id>/blobs`, private.
fn blob_folder(parent: &Path, root: &Path) -> PathBuf {
    let store_dir = parent.join(".remora-store");
    let blobs = store_dir.join(project_id(root)).join("blobs");
    fs::create_dir_all(&blobs).unwrap();
    fs::set_permissions(&store_dir, fs::Permissions::from_mode(0o700)).unwrap();
    blobs
}

#[test]
fn a_recorded_blob_folder_is_used_and_never_silently_replaced() {
    let data = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    let root = project.path().canonicalize().unwrap();
    let blobs = blob_folder(&elsewhere.path().canonicalize().unwrap(), &root);
    Engine::open(data.path()).unwrap();
    record_blobs(data.path(), &root, &blobs);
    let engine = Engine::open(data.path()).unwrap();

    assert_eq!(engine.project(&root).unwrap().status().unwrap().blob_store, blobs);

    fs::remove_dir_all(blobs.parent().unwrap()).unwrap(); // say, its volume is not mounted
    match engine.project(&root) {
        Err(EngineError::BlobStoreUnavailable { path, .. }) => assert_eq!(path, blobs),
        other => panic!("expected an unavailable blob store, got {other:?}"),
    }
}

#[test]
fn a_blob_folder_open_to_other_users_is_refused() {
    let data = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    let root = project.path().canonicalize().unwrap();
    let blobs = blob_folder(&elsewhere.path().canonicalize().unwrap(), &root);
    let store_dir = blobs.parent().unwrap().parent().unwrap();
    fs::set_permissions(store_dir, fs::Permissions::from_mode(0o755)).unwrap();
    Engine::open(data.path()).unwrap();
    record_blobs(data.path(), &root, &blobs);

    let engine = Engine::open(data.path()).unwrap();

    assert!(matches!(
        engine.project(&root),
        Err(EngineError::BlobStoreUnavailable { .. })
    ));
}
