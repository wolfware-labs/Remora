//! On Windows, a project on another drive keeps its blobs in
//! `<drive>:\.remora-store-<user SID>\<project id>\blobs`, granted to the user and SYSTEM alone.
#![cfg(windows)]
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;
use std::path::{Component, Path};
use std::process::Command;

use common::{agent, test_dir};
use remora_engine::{Engine, project_id, user_sid};
use tempfile::TempDir;

fn drive(path: &Path) -> Option<Component<'_>> {
    path.components().next()
}

#[test]
fn a_project_on_another_drive_keeps_its_blobs_on_that_drive() {
    // The data directory on the system drive; the project on the drive under test (CI's
    // Dev Drive).
    let data = TempDir::new().unwrap();
    let project = test_dir();
    fs::write(project.path().join("a.txt"), "a").unwrap();
    let engine = Engine::open(data.path()).unwrap();
    let root = project.path().canonicalize().unwrap();

    let mut p = engine.project(&root).unwrap();
    p.begin_turn(&agent(), None, 1_000).unwrap();
    let status = p.status().unwrap();

    if drive(engine.data_dir()) == drive(&root) {
        assert!(
            status.blob_store.starts_with(engine.data_dir()),
            "{}",
            status.blob_store.display()
        );
        return;
    }
    let sid = user_sid().expect("whoami reports the user's SID");
    let private = root
        .ancestors()
        .last()
        .unwrap()
        .join(format!(".remora-store-{sid}"));
    assert_eq!(status.blob_store, private.join(project_id(&root)).join("blobs"));
    if let Ok(expect) = std::env::var("REMORA_EXPECT_CLONES") {
        assert_eq!(status.copy_on_write, Some(expect == "1"));
    }

    // Read the folder's access list back: the user and SYSTEM, nobody else.
    let plain = private.to_string_lossy().trim_start_matches(r"\\?\").to_owned();
    let out = Command::new("icacls").arg(&plain).output().unwrap();
    assert!(out.status.success(), "icacls should succeed");
    let text = String::from_utf8_lossy(&out.stdout);
    let entries: Vec<&str> = text.lines().filter(|l| l.contains(":(")).collect();
    assert_eq!(entries.len(), 2, "{text}");
    assert!(entries.iter().any(|e| e.contains("SYSTEM")), "{text}");
    if let Ok(username) = std::env::var("USERNAME") {
        assert!(text.contains(&username), "{text}");
    }
    assert!(!text.contains("Everyone"), "{text}");
}
