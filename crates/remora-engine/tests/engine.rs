#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;

use common::Fixture;
use remora_engine::{Engine, EngineError, SessionRef, default_data_dir, project_id};

#[test]
fn opening_creates_the_key_and_the_project_data() {
    let fx = Fixture::new();

    let project = fx.open();

    let dir = fx.data.path().join("projects").join(project.id());
    assert_eq!(fs::read(fx.data.path().join("signing.key")).unwrap().len(), 32);
    for name in ["project.json", "store", "journal.db", "sessions"] {
        assert!(dir.join(name).exists(), "{name}");
    }
    assert_eq!(project.id(), project_id(&fx.root().canonicalize().unwrap()));
}

#[test]
fn the_machine_key_is_shared_by_every_project() {
    let fx = Fixture::new();
    let other = tempfile::TempDir::new().unwrap();
    let engine = fx.engine();

    let key_before = engine.verifying_key_hex();
    engine.project(fx.root()).unwrap();
    engine.project(other.path()).unwrap();

    assert_eq!(
        Engine::open(fx.data.path()).unwrap().verifying_key_hex(),
        key_before
    );
}

#[test]
fn data_recorded_for_another_root_is_refused() {
    let fx = Fixture::new();
    let project = fx.open();
    let file = fx
        .data
        .path()
        .join("projects")
        .join(project.id())
        .join("project.json");
    fs::write(&file, r#"{"root":"/somewhere/else"}"#).unwrap();

    let result = fx.engine().project(fx.root());

    assert!(
        matches!(result, Err(EngineError::ProjectMismatch { .. })),
        "{result:?}"
    );
}

#[test]
fn session_ids_that_are_not_plain_names_are_rejected() {
    let fx = Fixture::new();
    let mut project = fx.open();

    for bad in ["../escape", "", "a/b", &"x".repeat(129)] {
        let result = project.begin_turn(&SessionRef::new(bad, "claude-code"), None, 1);
        assert!(matches!(result, Err(EngineError::InvalidSessionId(_))), "{bad:?}");
    }
}

#[test]
fn the_default_data_dir_is_named_remora() {
    if let Ok(dir) = default_data_dir() {
        assert_eq!(dir.file_name().unwrap(), "remora");
    }
}

#[cfg(unix)]
#[test]
fn the_data_directory_is_owner_only_even_if_it_was_loose() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let project = fx.open();
    let projects = fx.data.path().join("projects");
    let dir = projects.join(project.id());
    for d in [fx.data.path(), &projects, &dir] {
        fs::set_permissions(d, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fx.open();

    for d in [fx.data.path(), &projects, &dir] {
        assert_eq!(
            fs::metadata(d).unwrap().permissions().mode() & 0o777,
            0o700,
            "{}",
            d.display()
        );
    }
}

// --- Review Focus ---

#[test]
fn a_project_path_with_spaces_and_unicode_works() {
    let fx = Fixture::new();
    let root = fx.root().join("mi proyecto ñandú 🦀");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.txt"), "a").unwrap();
    let engine = fx.engine();

    let mut project = engine.project(&root).unwrap();
    project.begin_turn(&common::agent(), Some("hola"), 1).unwrap();
    project.end_turn(&common::agent(), 2).unwrap();

    assert_eq!(
        engine.project(&root).unwrap().id(),
        project.id(),
        "the same folder maps to the same data"
    );
}

#[test]
fn uuid_session_ids_are_accepted() {
    let fx = Fixture::new();
    let mut project = fx.open();
    let session = SessionRef::new("4f1c2b9e-0d6a-4a8e-9a51-7c3e2d1b0f99", "claude-code");

    project.begin_turn(&session, None, 1).unwrap();
    assert!(project.end_turn(&session, 2).unwrap().is_some());
}

#[test]
fn a_project_folder_that_contains_the_data_dir_is_refused() {
    // An agent run from $HOME: the default data dir (~/.local/share/remora) is inside it.
    let home = tempfile::TempDir::new().unwrap();
    let engine = Engine::open(&home.path().join(".local/share/remora")).unwrap();

    let result = engine.project(home.path());

    assert!(
        matches!(result, Err(EngineError::RootOverlapsDataDir { .. })),
        "{result:?}"
    );
}

#[test]
fn a_project_folder_inside_the_data_dir_is_refused() {
    let fx = Fixture::new();
    let engine = fx.engine();
    let inside = fx.data.path().join("projects");

    let result = engine.project(&inside);

    assert!(
        matches!(result, Err(EngineError::RootOverlapsDataDir { .. })),
        "{result:?}"
    );
}
