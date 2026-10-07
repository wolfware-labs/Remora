#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use common::{Fixture, agent};
use remora_engine::SessionRef;

#[test]
fn a_turn_idle_since_before_the_cutoff_is_closed_as_interrupted() {
    let fx = Fixture::new();
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("long task"), 1_000).unwrap();
    p.before_command(&agent(), "cargo build", 5_000).unwrap();

    assert!(
        p.close_idle_turns(5_000, 9_000).unwrap().is_empty(),
        "active at the cutoff"
    );
    let closed = p.close_idle_turns(5_001, 9_000).unwrap();

    assert_eq!(closed.len(), 1);
    let r = &closed[0].record;
    assert_eq!(r.interrupted, Some(true));
    assert_eq!((r.started_at_ms, r.ended_at_ms), (1_000, 9_000));
    assert!(p.status().unwrap().open_sessions.is_empty());
}

#[test]
fn an_exit_code_counts_as_activity() {
    let fx = Fixture::new();
    let mut p = fx.open();
    p.begin_turn(&agent(), None, 1_000).unwrap();
    p.before_command(&agent(), "cargo build", 1_100).unwrap();
    p.after_command(&agent(), "cargo build", Some(0), 8_000).unwrap();

    assert!(p.close_idle_turns(8_000, 9_000).unwrap().is_empty());
}

#[test]
fn only_idle_sessions_are_closed() {
    let fx = Fixture::new();
    let mut p = fx.open();
    let busy = SessionRef::new("busy", "codex");
    p.begin_turn(&agent(), None, 1_000).unwrap();
    p.begin_turn(&busy, None, 1_000).unwrap();
    p.before_command(&busy, "make", 7_000).unwrap();

    let closed = p.close_idle_turns(6_000, 9_000).unwrap();

    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].record.session_id, "session-1");
    assert_eq!(p.status().unwrap().open_sessions, ["busy"]);
}
