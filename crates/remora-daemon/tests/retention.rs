#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::time::Duration;

use common::{START_MS, TestDaemon};
use remora_engine::Retention;
use remora_ipc::{ClientError, ErrorKind, Request};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

#[test]
fn status_reports_the_retention_in_effect() {
    let d = TestDaemon::start();
    let status = d.status(&mut d.client());
    assert_eq!(status.retention, Some(Retention::default()));
    assert_eq!(status.config_problem, None);

    let d = TestDaemon::start_with_config(Some("[retention]\ndays = 7\nmax_store_gb = 2\n"));
    let status = d.status(&mut d.client());
    assert_eq!(
        status.retention,
        Some(Retention {
            days: 7,
            max_store_gb: 2
        })
    );
}

#[test]
fn an_invalid_config_falls_back_to_the_defaults_and_says_why() {
    let d = TestDaemon::start_with_config(Some("[retention]\ndayz = 7\n"));
    let status = d.status(&mut d.client());
    assert_eq!(status.retention, Some(Retention::default()));
    let problem = status.config_problem.unwrap();
    assert!(
        problem.contains("config.toml") && problem.contains("dayz"),
        "{problem}"
    );
}

/// A turn now that changes `a.txt` to `version`.
fn turn(d: &TestDaemon, version: &str) {
    let mut client = d.client();
    d.write("a.txt", version);
    d.begin(&mut client, "s1", Some(version));
    d.end(&mut client, "s1");
}

#[test]
fn prune_applies_the_policy_and_a_pruned_point_is_refused() {
    let d = TestDaemon::start();
    let mut client = d.client();
    turn(&d, "old");
    d.clock.advance(40 * DAY);
    turn(&d, "new");

    let dry = d.prune(&mut client, true);
    assert_eq!(dry.pruned.len(), 1);
    assert!(d.points(&mut client).iter().all(|p| p.pruned_at_ms.is_none()));

    let report = d.prune(&mut client, false);
    assert_eq!(report.pruned.len(), 1);
    let points = d.points(&mut client);
    assert_eq!(points[1].pruned_at_ms, Some(START_MS + 40 * 86_400_000));
    let refused = client.call(Request::Rewind {
        root: d.root(),
        point: points[1].clone(),
        force: false,
    });
    assert!(
        matches!(&refused, Err(ClientError::Daemon(e)) if matches!(e.kind, ErrorKind::Pruned { .. })),
        "{refused:?}"
    );
    assert_eq!(d.read("a.txt"), "new");
}

#[test]
fn the_janitor_prunes_every_project_once_a_day() {
    let d = TestDaemon::start();
    turn(&d, "old");
    // A recorded project whose folder is gone is left alone, not recreated.
    let gone = tempfile::TempDir::new().unwrap();
    let gone_root = gone.path().canonicalize().unwrap();
    d.client()
        .call(Request::Status {
            root: gone_root.clone(),
        })
        .unwrap();
    drop(gone);
    d.clock.advance(40 * DAY);
    turn(&d, "new");

    d.daemon.prune_if_due();

    let mut client = d.client();
    assert!(d.points(&mut client)[1].pruned_at_ms.is_some());
    assert!(!gone_root.exists());
    assert_eq!(last_prune(&d), START_MS + 40 * 86_400_000);

    // Not again until a day has passed.
    d.clock.advance(DAY / 2);
    d.daemon.prune_if_due();
    assert_eq!(last_prune(&d), START_MS + 40 * 86_400_000);
    d.clock.advance(DAY / 2);
    d.daemon.prune_if_due();
    assert_eq!(last_prune(&d), START_MS + 41 * 86_400_000);
}

fn last_prune(d: &TestDaemon) -> i64 {
    let file = std::fs::read(d.data.path().join("janitor.json")).unwrap();
    serde_json::from_slice::<serde_json::Value>(&file).unwrap()["last_prune_ms"]
        .as_i64()
        .unwrap()
}
