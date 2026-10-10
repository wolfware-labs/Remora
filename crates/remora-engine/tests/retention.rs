#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;
use std::path::PathBuf;

use common::{Fixture, agent};
use remora_engine::{EngineError, PRUNED_FILE, PointKind, Project, PruneReason, Retention, project_id};
use remora_journal::VerifyReport;
use sha2::{Digest, Sha256};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// One turn per version of `a.txt`, a day apart, starting on day 1; `shared.txt` never
/// changes. Returns the open project.
fn history(fx: &Fixture, versions: &[&str]) -> Project {
    fx.write("shared.txt", "the same in every point");
    let mut p = fx.open();
    for (day, version) in versions.iter().enumerate() {
        let at = (i64::try_from(day).unwrap() + 1) * DAY_MS;
        fx.write("a.txt", version);
        p.begin_turn(&agent(), Some(version), at).unwrap();
        p.end_turn(&agent(), at + 1_000).unwrap();
    }
    p
}

fn store(fx: &Fixture) -> PathBuf {
    let root = fx.root().canonicalize().unwrap();
    fx.data
        .path()
        .join("projects")
        .join(project_id(&root))
        .join("store")
}

fn blob(fx: &Fixture, contents: &str) -> PathBuf {
    let hash = hex::encode(Sha256::digest(contents.as_bytes()));
    store(fx).join("blobs").join(&hash[..2]).join(&hash[2..])
}

fn manifests(fx: &Fixture) -> usize {
    fs::read_dir(store(fx).join("manifests")).unwrap().count()
}

fn keep_days(days: u64) -> Retention {
    Retention {
        days,
        max_store_gb: 1_000,
    }
}

#[test]
fn points_older_than_the_retention_period_are_pruned_and_the_journal_is_untouched() {
    let fx = Fixture::new();
    let mut p = history(&fx, &["v1", "v2", "v3"]);
    let entries = p.journal().entries().unwrap();
    // Day 33: v1 (day 1) and v2 (day 2) are older than 30 days, v3 (day 3) is not.
    let now = 33 * DAY_MS;

    let report = p.prune(&keep_days(30), now, false).unwrap();

    let reasons: Vec<_> = report.pruned.iter().map(|s| s.reason).collect();
    assert_eq!(reasons, [PruneReason::Age, PruneReason::Age]);
    assert!(!blob(&fx, "v1").exists() && !blob(&fx, "v2").exists());
    assert!(blob(&fx, "v3").exists());
    assert!(
        blob(&fx, "the same in every point").exists(),
        "still needed by v3"
    );
    assert_eq!(p.journal().entries().unwrap(), entries);
    assert!(matches!(
        p.journal().verify().unwrap(),
        VerifyReport::Intact { .. }
    ));
    let pruned: Vec<_> = p
        .restore_points()
        .unwrap()
        .iter()
        .map(|pt| pt.pruned_at_ms)
        .collect();
    assert_eq!(pruned, [None, Some(now), Some(now)], "newest first");
}

#[test]
fn a_pruned_point_is_refused_with_its_prune_date() {
    let fx = Fixture::new();
    let mut p = history(&fx, &["v1", "v2"]);
    p.prune(&keep_days(30), 40 * DAY_MS, false).unwrap();
    let old = p.restore_points().unwrap().pop().unwrap();

    let refused = p.rewind(&old, 41 * DAY_MS, false);
    let Err(EngineError::Pruned { pruned_at_ms, .. }) = &refused else {
        panic!("expected Pruned, got {refused:?}");
    };
    assert_eq!(*pruned_at_ms, 40 * DAY_MS);
    assert!(refused.unwrap_err().to_string().contains("pruned on 1970-02-10"));
    assert!(matches!(p.preview(&old), Err(EngineError::Pruned { .. })));
    assert!(matches!(
        p.restore_snapshot(&old.snapshot, 41 * DAY_MS, false),
        Err(EngineError::Pruned { .. })
    ));
    assert_eq!(fx.read("a.txt"), "v2", "nothing changed");
}

#[test]
fn the_newest_point_open_turns_and_the_last_rewind_are_never_pruned() {
    let fx = Fixture::new();
    let mut p = history(&fx, &["v1", "v2", "v3"]);
    let v1 = p.restore_points().unwrap().pop().unwrap();
    p.rewind(&v1, 4 * DAY_MS, false).unwrap();
    fx.write("a.txt", "v4");
    p.begin_turn(&agent(), Some("open"), 5 * DAY_MS).unwrap();

    // Everything is old and the cap is zero: only the protected points stay.
    let report = p
        .prune(
            &Retention {
                days: 1,
                max_store_gb: 0,
            },
            100 * DAY_MS,
            false,
        )
        .unwrap();

    let points = p.restore_points().unwrap();
    let kept: Vec<_> = points
        .iter()
        .filter(|pt| pt.pruned_at_ms.is_none())
        .map(|pt| pt.kind)
        .collect();
    // v3's turn start is the same tree as the state before the rewind: kept with it.
    assert_eq!(
        kept,
        [
            PointKind::TurnStart,
            PointKind::BeforeRewind,
            PointKind::TurnStart
        ],
        "{report:?}"
    );
    assert!(points[0].open);
    p.end_turn(&agent(), 101 * DAY_MS).unwrap();
    let undo = p.restore_points().unwrap()[1].clone();
    assert_eq!(undo.kind, PointKind::BeforeRewind);
    p.rewind(&undo, 102 * DAY_MS, false).unwrap();
    assert_eq!(fx.read("a.txt"), "v3", "the rewind can still be undone");
}

#[test]
fn over_the_size_cap_the_oldest_points_go_first() {
    let fx = Fixture::new();
    let big = |c: char| c.to_string().repeat(400_000);
    let mut p = history(&fx, &[&big('a'), &big('b'), &big('c'), &big('d')]);

    // Nothing is old enough, but the store is over a cap of zero: all but the newest go,
    // oldest first.
    let report = p
        .prune(
            &Retention {
                days: 3650,
                max_store_gb: 0,
            },
            5 * DAY_MS,
            false,
        )
        .unwrap();

    let order: Vec<_> = report.pruned.iter().map(|s| (s.at_ms, s.reason)).collect();
    assert_eq!(
        order,
        [
            (DAY_MS, PruneReason::Size),
            (2 * DAY_MS, PruneReason::Size),
            (3 * DAY_MS, PruneReason::Size)
        ]
    );
    assert!(report.bytes_freed >= 3 * 400_000);
    assert!(blob(&fx, &big('d')).exists());
    assert_eq!(fx.read("a.txt"), big('d'));
}

#[test]
fn a_dry_run_changes_nothing() {
    let fx = Fixture::new();
    let mut p = history(&fx, &["v1", "v2", "v3"]);
    let before = manifests(&fx);

    let report = p.prune(&keep_days(30), 40 * DAY_MS, true).unwrap();

    assert!(report.dry_run);
    assert_eq!(report.pruned.len(), 2);
    assert!(report.blobs_removed >= 2);
    assert_eq!(manifests(&fx), before);
    assert!(blob(&fx, "v1").exists());
    assert!(
        p.restore_points()
            .unwrap()
            .iter()
            .all(|pt| pt.pruned_at_ms.is_none())
    );
    let dir = store(&fx).parent().unwrap().to_path_buf();
    assert!(!dir.join(PRUNED_FILE).exists());
}

#[test]
fn an_unreadable_manifest_is_kept_and_no_blob_is_removed() {
    let fx = Fixture::new();
    let mut p = history(&fx, &["v1", "v2", "v3"]);
    let v2 = p.restore_points().unwrap()[1].snapshot.clone();
    fs::write(
        store(&fx).join("manifests").join(format!("{v2}.json")),
        "{ damaged",
    )
    .unwrap();

    let report = p.prune(&keep_days(30), 40 * DAY_MS, false).unwrap();

    assert_eq!(report.unreadable, [v2]);
    assert_eq!(report.pruned.len(), 1, "v1 still goes");
    assert_eq!(report.blobs_removed, 0);
    assert!(blob(&fx, "v1").exists(), "v2's manifest might need it");
}

#[test]
fn a_pruned_tree_stored_again_is_restorable_again() {
    let fx = Fixture::new();
    let mut p = history(&fx, &["v1", "v2"]);
    p.prune(&keep_days(30), 40 * DAY_MS, false).unwrap();
    assert!(p.restore_points().unwrap()[1].pruned_at_ms.is_some());

    // The project goes back to exactly the pruned tree, and a turn snapshots it.
    fx.write("a.txt", "v1");
    p.begin_turn(&agent(), Some("again"), 41 * DAY_MS).unwrap();
    p.end_turn(&agent(), 42 * DAY_MS).unwrap();
    fx.write("a.txt", "v5");

    let points = p.restore_points().unwrap();
    let old = points.last().unwrap();
    assert_eq!(old.pruned_at_ms, None);
    p.rewind(old, 43 * DAY_MS, false).unwrap();
    assert_eq!(fx.read("a.txt"), "v1");
    drop(p);
    assert!(
        fx.open()
            .restore_points()
            .unwrap()
            .last()
            .unwrap()
            .pruned_at_ms
            .is_none()
    );
}

#[test]
fn a_young_snapshot_no_point_uses_is_not_pruned_for_size() {
    // Such as the safety snapshot of a rewind whose journal write failed: its id was handed
    // to the user, who must still be able to restore it.
    let fx = Fixture::new();
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let now = i64::try_from(since_epoch.as_millis()).unwrap();
    fx.write("a.txt", "old");
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("edit"), now - 10 * DAY_MS).unwrap();
    // The turn's end state: a manifest written just now that no restore point uses.
    fx.write("a.txt", "unused");
    p.end_turn(&agent(), now - 10 * DAY_MS + 1_000).unwrap();
    fx.write("a.txt", "new");
    p.begin_turn(&agent(), Some("newest"), now - 9 * DAY_MS).unwrap();
    p.end_turn(&agent(), now - 9 * DAY_MS + 1_000).unwrap();
    assert_eq!(manifests(&fx), 3);

    // Nothing is old enough, but the store is over a cap of zero.
    let report = p
        .prune(
            &Retention {
                days: 3650,
                max_store_gb: 0,
            },
            now,
            false,
        )
        .unwrap();

    let pruned: Vec<_> = report.pruned.iter().map(|s| (s.at_ms, s.reason)).collect();
    assert_eq!(
        pruned,
        [(now - 10 * DAY_MS, PruneReason::Size)],
        "only the old point"
    );
    assert_eq!(manifests(&fx), 2);
    assert!(blob(&fx, "unused").exists());
    assert!(!blob(&fx, "old").exists());
}
