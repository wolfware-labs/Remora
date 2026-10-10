//! On Windows, a project on another drive keeps its blobs in
//! `<drive>:\.remora-store-<user SID>\<project id>\blobs`, granted to the user and SYSTEM alone.
#![cfg(windows)]
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;
use std::path::{Component, Path};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};

use common::{agent, test_dir};
use remora_engine::{Engine, EngineError, project_id, user_sid};
use tempfile::TempDir;

fn drive(path: &Path) -> Option<Component<'_>> {
    path.components().next()
}

/// The folder's access list as `icacls /save` writes it (UTF-16LE), for failure messages.
fn saved_sddl(dir: &Path) -> String {
    let plain = dir.to_string_lossy().trim_start_matches(r"\\?\").to_owned();
    let dump = std::env::temp_dir().join(format!("remora-test-acl-{}.txt", std::process::id()));
    let out = Command::new("icacls")
        .arg(&plain)
        .arg("/save")
        .arg(&dump)
        .arg("/q")
        .output();
    let bytes = fs::read(&dump);
    let _ = fs::remove_file(&dump);
    match (out, bytes) {
        (Ok(_), Ok(bytes)) => {
            let (pairs, _) = bytes.as_chunks::<2>();
            let units: Vec<u16> = pairs.iter().map(|p| u16::from_le_bytes(*p)).collect();
            String::from_utf16_lossy(&units)
        }
        (out, bytes) => format!("(could not save the access list: {out:?}, {bytes:?})"),
    }
}

/// Every test here works on the same folder at the drive's root: one at a time.
static DRIVE_ROOT: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> MutexGuard<'static, ()> {
    DRIVE_ROOT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `path` without the `\\?\` prefix, as `icacls` takes it.
fn plain(path: &Path) -> String {
    path.to_string_lossy().trim_start_matches(r"\\?\").to_owned()
}

fn icacls(args: &[&str]) {
    let status = Command::new("icacls").args(args).status().unwrap();
    assert!(status.success(), "icacls {args:?}: {status}");
}

/// Removes the folder when dropped.
struct RemoveOnDrop(std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_project_on_another_drive_keeps_its_blobs_on_that_drive() {
    let _one = one_at_a_time();
    // The data directory on the system drive; the project on the drive under test (CI's
    // Dev Drive).
    let data = TempDir::new().unwrap();
    let project = test_dir();
    fs::write(project.path().join("a.txt"), "a").unwrap();
    let engine = Engine::open(data.path()).unwrap();
    let root = project.path().canonicalize().unwrap();

    let sid = user_sid().expect("whoami reports the user's SID");
    let private = root
        .ancestors()
        .last()
        .unwrap()
        .join(format!(".remora-store-{sid}"));
    // On another drive the test makes a folder at that drive's root, which outlives the
    // temporary directories: remove it at the end (also when an assertion fails; declared
    // before the project so its files are closed first), so a second run starts clean.
    // Never remove a folder that was already there: it may be the user's real store.
    let existed = private.exists();
    let _cleanup =
        (drive(engine.data_dir()) != drive(&root) && !existed).then(|| RemoveOnDrop(private.clone()));

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
    let expected = private.join(project_id(&root)).join("blobs");
    if status.blob_store != expected {
        panic!(
            "the blobs are at {} instead of {}; the store folder's access list is:\n{}",
            status.blob_store.display(),
            expected.display(),
            saved_sddl(&private)
        );
    }
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

#[test]
fn a_store_folder_left_by_a_crash_is_adopted_and_anything_else_refused() {
    let _one = one_at_a_time();
    let project = test_dir();
    let root = project.path().canonicalize().unwrap();
    let first = TempDir::new().unwrap();
    if drive(Engine::open(first.path()).unwrap().data_dir()) == drive(&root) {
        eprintln!("skipped: the data directory and the project are on the same drive");
        return;
    }
    let sid = user_sid().unwrap();
    let private = root
        .ancestors()
        .last()
        .unwrap()
        .join(format!(".remora-store-{sid}"));
    if private.exists() {
        eprintln!("{} exists (a real store?): skipped", private.display());
        return;
    }
    let _cleanup = RemoveOnDrop(private.clone());
    // As a crash right after Remora made and secured it leaves it.
    fs::create_dir(&private).unwrap();
    icacls(&[&plain(&private), "/setowner", &format!("*{sid}"), "/q"]);
    icacls(&[
        &plain(&private),
        "/inheritance:r",
        "/grant:r",
        &format!("*{sid}:(OI)(CI)F"),
        "*S-1-5-18:(OI)(CI)F",
        "/q",
    ]);
    // A new folder can carry explicit entries for these (the Dev Drive's root gives them).
    icacls(&[
        &plain(&private),
        "/remove:g",
        "*S-1-5-32-544",
        "*S-1-3-0",
        "*S-1-5-32-545",
        "*S-1-1-0",
        "*S-1-5-11",
        "/q",
    ]);
    fs::write(private.join(".gitignore"), "*\n").unwrap();
    fs::write(private.join("stray.txt"), "not Remora's").unwrap();

    // Holding something else: refused, the blobs stay in the data directory.
    let engine = Engine::open(first.path()).unwrap();
    let status = engine.project(&root).unwrap().status().unwrap();
    assert!(
        status.blob_store.starts_with(engine.data_dir()),
        "{}",
        status.blob_store.display()
    );

    // Holding only its .gitignore: adopted.
    fs::remove_file(private.join("stray.txt")).unwrap();
    let second = TempDir::new().unwrap();
    let engine = Engine::open(second.path()).unwrap();
    let status = engine.project(&root).unwrap().status().unwrap();
    let expected = private.join(project_id(&root)).join("blobs");
    if status.blob_store != expected {
        let sddl = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!(
                "[System.IO.Directory]::GetAccessControl('{}').GetSecurityDescriptorSddlForm('Owner, Access')",
                plain(&private)
            ))
            .env_remove("PSModulePath")
            .output()
            .map(|o| {
                format!(
                    "{} / {}",
                    String::from_utf8_lossy(&o.stdout).trim(),
                    String::from_utf8_lossy(&o.stderr).trim()
                )
            });
        let entries: Vec<_> = fs::read_dir(&private)
            .map(|d| d.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        panic!(
            "not adopted: the blobs are at {}; its security descriptor is {sddl:?}; it holds {entries:?}",
            status.blob_store.display()
        );
    }
}

#[test]
fn a_project_folder_in_the_store_opened_to_others_is_refused() {
    let _one = one_at_a_time();
    let data = TempDir::new().unwrap();
    let project = test_dir();
    let root = project.path().canonicalize().unwrap();
    let engine = Engine::open(data.path()).unwrap();
    if drive(engine.data_dir()) == drive(&root) {
        eprintln!("skipped: the data directory and the project are on the same drive");
        return;
    }
    let sid = user_sid().unwrap();
    let private = root
        .ancestors()
        .last()
        .unwrap()
        .join(format!(".remora-store-{sid}"));
    if private.exists() {
        eprintln!("{} exists (a real store?): skipped", private.display());
        return;
    }
    let _cleanup = RemoveOnDrop(private.clone());
    let blobs = engine.project(&root).unwrap().status().unwrap().blob_store;
    assert!(blobs.starts_with(&private));

    // Someone gave Everyone access to the project's blobs folder.
    icacls(&[&plain(&blobs), "/grant", "*S-1-1-0:(OI)(CI)R", "/q"]);

    let refused = engine.project(&root);
    assert!(
        matches!(&refused, Err(EngineError::BlobStoreUnavailable { .. })),
        "{:?}",
        refused.map(|p| p.id().to_owned())
    );
}
