#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Fixture, agent};
use remora_engine::{EngineError, project_id};

/// The name of Remora's private folder on a project's volume (on Windows it ends in the
/// user's SID).
#[cfg(not(windows))]
const STORE: &str = ".remora-store";
#[cfg(windows)]
const STORE: &str = ".remora-store-S-1-5-21-1-2-3-1001";

fn project_dir(fx: &Fixture) -> PathBuf {
    let root = fx.root().canonicalize().unwrap();
    // Canonical, as the engine reports it (`/private/var` on macOS, `\\?\` on Windows).
    let data = fx.data.path().canonicalize().unwrap();
    data.join("projects").join(project_id(&root))
}

/// Record `blobs` as the project's blob folder, as `Engine::project` does for a project on
/// another volume, and put a blob in it.
fn record_blobs(fx: &Fixture, blobs: &Path) {
    let root = fx.root().canonicalize().unwrap();
    fs::create_dir_all(blobs.join("ab")).unwrap();
    fs::write(blobs.join("ab").join("cd"), "blob").unwrap();
    fs::write(
        project_dir(fx).join("project.json"),
        serde_json::json!({"root": root, "blobs": blobs}).to_string(),
    )
    .unwrap();
}

#[test]
fn forgetting_removes_the_project_data_and_a_new_start_is_empty() {
    let fx = Fixture::new();
    fx.write("a.txt", "a");
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("edit"), 1_000).unwrap();
    p.end_turn(&agent(), 2_000).unwrap();
    drop(p);

    let forgotten = fx.engine().forget(fx.root(), false).unwrap();

    assert_eq!(forgotten.data_dir, project_dir(&fx));
    assert_eq!(forgotten.blob_home, None);
    assert!(!project_dir(&fx).exists());
    assert_eq!(fx.read("a.txt"), "a", "the project's own files are untouched");
    let p = fx.open();
    assert!(p.restore_points().unwrap().is_empty());
    assert!(p.journal().entries().unwrap().is_empty());
}

#[test]
fn forgetting_removes_the_projects_folder_in_its_blob_home_and_nothing_else() {
    let fx = Fixture::new();
    drop(fx.open());
    let volume = common::test_dir();
    let id = project_id(&fx.root().canonicalize().unwrap());
    let store = volume.path().join(STORE);
    let other = store.join("0123456789abcdef");
    fs::create_dir_all(&other).unwrap();
    record_blobs(&fx, &store.join(&id).join("blobs"));

    let forgotten = fx.engine().forget(fx.root(), false).unwrap();

    assert_eq!(forgotten.blob_home, Some(store.join(&id)));
    assert!(!store.join(&id).exists());
    assert!(other.exists(), "another project's blobs stay");
    assert!(!project_dir(&fx).exists());
}

#[test]
fn a_blob_folder_not_named_after_the_project_is_left_alone() {
    let fx = Fixture::new();
    drop(fx.open());
    let elsewhere = common::test_dir();
    let blobs = elsewhere.path().join("photos").join("blobs");
    record_blobs(&fx, &blobs);

    let forgotten = fx.engine().forget(fx.root(), false).unwrap();

    assert_eq!(forgotten.blob_home, None);
    assert!(blobs.join("ab").join("cd").exists());
    assert!(!project_dir(&fx).exists());
}

#[test]
fn a_blob_folder_outside_a_remora_store_is_left_alone() {
    let fx = Fixture::new();
    drop(fx.open());
    let elsewhere = common::test_dir();
    let id = project_id(&fx.root().canonicalize().unwrap());
    let blobs = elsewhere.path().join("photos").join(&id).join("blobs");
    record_blobs(&fx, &blobs);

    let forgotten = fx.engine().forget(fx.root(), false).unwrap();

    assert_eq!(forgotten.blob_home, None);
    assert!(blobs.join("ab").join("cd").exists());
    assert!(!project_dir(&fx).exists());
}

#[test]
fn a_relative_blob_folder_is_left_alone() {
    let fx = Fixture::new();
    drop(fx.open());
    let root = fx.root().canonicalize().unwrap();
    let id = project_id(&root);
    // Recorded only: nothing is created relative to the current folder.
    let blobs = Path::new(STORE).join(&id).join("blobs");
    fs::write(
        project_dir(&fx).join("project.json"),
        serde_json::json!({"root": root, "blobs": blobs}).to_string(),
    )
    .unwrap();

    let forgotten = fx.engine().forget(fx.root(), false).unwrap();

    assert_eq!(forgotten.blob_home, None);
    assert!(!project_dir(&fx).exists());
}

#[test]
fn forgetting_is_refused_while_a_turn_is_open_unless_forced() {
    let fx = Fixture::new();
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("working"), 1_000).unwrap();
    drop(p);

    let refused = fx.engine().forget(fx.root(), false);

    assert!(
        matches!(&refused, Err(EngineError::TurnInProgress { session }) if session == "session-1"),
        "{refused:?}"
    );
    assert!(project_dir(&fx).exists());
    fx.engine().forget(fx.root(), true).unwrap();
    assert!(!project_dir(&fx).exists());
}

#[test]
fn a_project_never_recorded_cannot_be_forgotten() {
    let fx = Fixture::new();
    let result = fx.engine().forget(fx.root(), false);
    assert!(
        matches!(result, Err(EngineError::UnknownProject(_))),
        "{result:?}"
    );
}

#[test]
fn a_deleted_project_folder_is_forgotten_by_its_path() {
    let fx = Fixture::new();
    let root = fx.root().canonicalize().unwrap().join("work");
    fs::create_dir(&root).unwrap();
    drop(fx.engine().project(&root).unwrap());
    fs::remove_dir(&root).unwrap();

    fx.engine().forget(&root, false).unwrap();

    let dir = fx.data.path().join("projects").join(project_id(&root));
    assert!(!dir.exists());
    assert!(!root.exists(), "not recreated");
}
