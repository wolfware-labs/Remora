#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use common::{Fixture, agent};

#[test]
fn status_reports_open_sessions_and_counts() {
    let fx = Fixture::new();
    let mut p = fx.open();
    p.begin_turn(&agent(), None, 1_000).unwrap();
    p.before_command(&agent(), "ls", None, 1_100).unwrap();

    let status = p.status().unwrap();

    assert_eq!(status.open_sessions, ["session-1"]);
    assert_eq!(status.journal_entries, 0);
    assert_eq!(status.restore_points, 2);
    assert_eq!(status.root, fx.root().canonicalize().unwrap());
}

#[cfg(unix)]
#[test]
fn status_lists_what_the_last_snapshot_could_not_protect() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write("secret.txt", "s3cret");
    let path = fx.root().join("secret.txt");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&path).is_ok() {
        return; // running as root: permissions are not enforced
    }
    let mut p = fx.open();

    p.begin_turn(&agent(), None, 1_000).unwrap();

    assert_eq!(
        p.status().unwrap().last_unprotected,
        [path.canonicalize().unwrap()]
    );
}
