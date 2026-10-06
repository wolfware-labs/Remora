//! The journal holds prompts and commands, which can contain secrets: the database is owner-only.
#![cfg(unix)]
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::{db, open, record};
use tempfile::TempDir;

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_new_database_is_owner_only() {
    let dir = TempDir::new().unwrap();

    open(dir.path()).append(record("export API_KEY=secret")).unwrap();

    assert_eq!(mode(&db(dir.path())), 0o600);
}

#[test]
fn a_loose_existing_database_is_tightened() {
    let dir = TempDir::new().unwrap();
    drop(open(dir.path()));
    fs::set_permissions(db(dir.path()), fs::Permissions::from_mode(0o644)).unwrap();

    drop(open(dir.path()));

    assert_eq!(mode(&db(dir.path())), 0o600);
}

#[test]
fn reopening_keeps_existing_entries() {
    let dir = TempDir::new().unwrap();
    open(dir.path()).append(record("one")).unwrap();

    assert_eq!(
        open(dir.path()).entries().unwrap().len(),
        1,
        "opening must never truncate journal.db"
    );
}
