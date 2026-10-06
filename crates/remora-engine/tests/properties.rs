//! Property: in any messy session, every restore point can be restored exactly.
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::fs;
use std::path::Path;

use common::{Fixture, agent, tree};
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    Write(String, Vec<u8>),
    Remove(String),
}

#[derive(Debug, Clone)]
struct Turn {
    edits_before: Vec<Op>,
    /// Each command is followed by its effects.
    commands: Vec<Vec<Op>>,
    /// False: the turn never ends (interrupted by the next turn, or still open).
    ends: bool,
}

fn path() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(vec!["a", "b", "c.txt"]), 1..=2).prop_map(|p| p.join("/"))
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (path(), prop::collection::vec(any::<u8>(), 0..16)).prop_map(|(p, b)| Op::Write(p, b)),
        1 => path().prop_map(Op::Remove),
    ]
}

fn turn() -> impl Strategy<Value = Turn> {
    (
        prop::collection::vec(op(), 0..3),
        prop::collection::vec(prop::collection::vec(op(), 0..3), 0..3),
        any::<bool>(),
    )
        .prop_map(|(edits_before, commands, ends)| Turn {
            edits_before,
            commands,
            ends,
        })
}

fn apply(root: &Path, op: &Op) {
    match op {
        Op::Write(p, bytes) => {
            let path = root.join(p);
            let _ = fs::create_dir_all(path.parent().unwrap());
            let _ = fs::write(path, bytes);
        }
        Op::Remove(p) => {
            let path = root.join(p);
            if let Ok(meta) = fs::symlink_metadata(&path) {
                let _ = if meta.is_dir() {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                };
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn every_restore_point_restores_its_exact_tree(
        turns in prop::collection::vec(turn(), 1..5),
        pick in any::<prop::sample::Index>(),
    ) {
        let fx = Fixture::new();
        let mut p = fx.open();
        let mut t = 0i64;
        // The tree as it was at each restore point, in the order the points were taken.
        let mut expected = Vec::new();

        for turn in &turns {
            t += 10;
            expected.push(tree(fx.root()));
            p.begin_turn(&agent(), Some("prompt"), t).unwrap();
            for op in &turn.edits_before {
                apply(fx.root(), op);
            }
            for (i, effects) in turn.commands.iter().enumerate() {
                expected.push(tree(fx.root()));
                p.before_command(&agent(), &format!("command {i}"), t + 1 + i as i64).unwrap();
                for op in effects {
                    apply(fx.root(), op);
                }
            }
            if turn.ends {
                p.end_turn(&agent(), t + 5).unwrap();
            }
        }

        let mut points = p.restore_points().unwrap();
        points.reverse(); // oldest first, matching `expected`
        prop_assert_eq!(points.len(), expected.len());
        let i = pick.index(points.len());

        let report = p.rewind(&points[i], t + 100, true).unwrap();

        prop_assert!(report.restore.failed.is_empty(), "{:?}", report.restore.failed);
        prop_assert_eq!(tree(fx.root()), expected[i].clone());
    }
}
