#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::thread;

use remora_journal::{JournalError, load_or_create_key};
use tempfile::TempDir;

#[test]
fn a_key_is_created_once_and_reloaded() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("signing.key");

    let first = load_or_create_key(&path).unwrap();
    let second = load_or_create_key(&path).unwrap();

    assert_eq!(first.to_bytes(), second.to_bytes());
    assert_eq!(std::fs::read(&path).unwrap().len(), 32);
}

#[test]
fn a_truncated_key_file_is_rejected() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("signing.key");
    std::fs::write(&path, [1u8; 5]).unwrap();

    assert!(matches!(
        load_or_create_key(&path),
        Err(JournalError::InvalidKey(_))
    ));
}

#[test]
fn concurrent_creators_all_get_the_same_key_and_leave_no_temp_files() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("signing.key");

    let keys: Vec<[u8; 32]> = thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| s.spawn(|| load_or_create_key(&path).unwrap().to_bytes()))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    assert!(
        keys.windows(2).all(|w| w[0] == w[1]),
        "every caller must end with the winner's key"
    );
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["signing.key"], "temp files must be cleaned up");
}

#[cfg(unix)]
#[test]
fn the_key_is_owner_only_and_loose_permissions_are_tightened() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("signing.key");
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

    load_or_create_key(&path).unwrap();
    assert_eq!(mode(&path), 0o600);

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    load_or_create_key(&path).unwrap();
    assert_eq!(mode(&path), 0o600);
}
