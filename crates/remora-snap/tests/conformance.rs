//! Property test: snapshot, apply random changes, restore, and the tree must match exactly.
//! Every snapshot backend must pass this suite unchanged.
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;
use std::path::Path;

use common::{EXCLUDED, Fixture, tree_state};
use proptest::prelude::*;
use remora_snap::Snapshotter;

#[derive(Debug, Clone)]
enum Op {
    Write(String, Vec<u8>),
    Mkdir(String),
    Remove(String),
    Rename(String, String),
    Symlink(String, String),
    Chmod(String, u32),
}

fn path() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(vec!["a", "b", "c.txt", "d e"]), 1..=3)
        .prop_map(|parts| parts.join("/"))
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (path(), prop::collection::vec(any::<u8>(), 0..64)).prop_map(|(p, b)| Op::Write(p, b)),
        1 => path().prop_map(Op::Mkdir),
        2 => path().prop_map(Op::Remove),
        1 => (path(), path()).prop_map(|(a, b)| Op::Rename(a, b)),
        1 => (path(), prop::sample::select(vec!["a", "c.txt", "missing"]))
            .prop_map(|(p, t)| Op::Symlink(p, t.to_owned())),
        1 => (path(), prop::sample::select(vec![0o644u32, 0o600, 0o755, 0o444]))
            .prop_map(|(p, m)| Op::Chmod(p, m)),
    ]
}

/// Apply an operation, ignoring failures (e.g. writing under a path that is a file).
fn apply(root: &Path, op: &Op) {
    let at = |p: &str| root.join(p);
    let parent = |p: &str| {
        let _ = fs::create_dir_all(at(p).parent().unwrap());
    };
    match op {
        Op::Write(p, bytes) => {
            parent(p);
            let _ = fs::write(at(p), bytes);
        }
        Op::Mkdir(p) => {
            let _ = fs::create_dir_all(at(p));
        }
        Op::Remove(p) => {
            if let Ok(meta) = fs::symlink_metadata(at(p)) {
                let _ = if meta.is_dir() {
                    fs::remove_dir_all(at(p))
                } else {
                    fs::remove_file(at(p))
                };
            }
        }
        Op::Rename(from, to) => {
            parent(to);
            let _ = fs::rename(at(from), at(to));
        }
        Op::Symlink(p, target) => {
            parent(p);
            // Replace an existing file or symlink, so targets also get re-pointed.
            if fs::symlink_metadata(at(p)).is_ok_and(|m| !m.is_dir()) {
                let _ = fs::remove_file(at(p));
            }
            #[cfg(unix)]
            let _ = std::os::unix::fs::symlink(target, at(p));
            #[cfg(not(unix))]
            let _ = target;
        }
        Op::Chmod(p, mode) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if fs::symlink_metadata(at(p)).is_ok_and(|m| m.is_file()) {
                    let _ = fs::set_permissions(at(p), fs::Permissions::from_mode(*mode));
                }
            }
            #[cfg(not(unix))]
            let _ = (p, mode);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn restore_returns_the_exact_snapshotted_tree(
        before in prop::collection::vec(op(), 0..12),
        after in prop::collection::vec(op(), 1..12),
    ) {
        let fx = Fixture::new();
        let snap = fx.snapshotter();
        for op in &before {
            apply(fx.root(), op);
        }
        let expected = tree_state(fx.root(), EXCLUDED);
        let s = snap.snapshot(fx.root()).unwrap();

        for op in &after {
            apply(fx.root(), op);
        }
        let report = snap.restore(fx.root(), &s.id).unwrap();

        prop_assert!(report.failed.is_empty(), "restore failures: {:?}", report.failed);
        prop_assert_eq!(tree_state(fx.root(), EXCLUDED), expected);
    }

    #[test]
    fn manifest_diff_is_empty_exactly_when_trees_match(
        before in prop::collection::vec(op(), 0..8),
        after in prop::collection::vec(op(), 0..8),
    ) {
        let fx = Fixture::new();
        let snap = fx.snapshotter();
        for op in &before {
            apply(fx.root(), op);
        }
        let first_state = tree_state(fx.root(), EXCLUDED);
        let first = snap.manifest(&snap.snapshot(fx.root()).unwrap().id).unwrap();
        for op in &after {
            apply(fx.root(), op);
        }
        let second_state = tree_state(fx.root(), EXCLUDED);
        let second = snap.manifest(&snap.snapshot(fx.root()).unwrap().id).unwrap();

        prop_assert_eq!(first.diff(&second).is_empty(), first_state == second_state);
    }
}
