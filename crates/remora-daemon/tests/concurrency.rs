#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::time::{Duration, Instant};

use common::TestDaemon;
use remora_ipc::{JournalCheck, Request, Response};

#[test]
fn overlapping_sessions_on_one_project_are_recorded_in_order() {
    let d = TestDaemon::start();
    std::thread::scope(|scope| {
        for session in ["a", "b", "c"] {
            let d = &d;
            scope.spawn(move || {
                let mut client = d.client();
                for i in 0..5 {
                    d.begin(&mut client, session, None);
                    d.write(&format!("{session}.txt"), &i.to_string());
                    d.before(&mut client, session, &format!("echo {i}"));
                    assert!(matches!(
                        d.end(&mut client, session),
                        Response::TurnEnded { seq: Some(_) }
                    ));
                }
            });
        }
    });

    let mut client = d.client();
    assert_eq!(
        client.call(Request::Verify { root: d.root() }).unwrap(),
        Response::Verified {
            journal: JournalCheck::Intact { entries: 15 }
        }
    );
    let mut times: Vec<i64> = d.points(&mut client).iter().map(|p| p.at_ms).collect();
    let count = times.len();
    times.dedup();
    assert_eq!(times.len(), count, "every event got its own time");
}

#[test]
fn a_busy_project_does_not_hold_up_another_one() {
    let d = TestDaemon::start();
    let other = TestDaemon::start(); // only for its project folder
    let mut client = d.client();
    let status = d.status(&mut client);
    let lock_file = d
        .data
        .path()
        .join("projects")
        .join(remora_engine::project_id(&status.root))
        .join("lock");
    let holder = std::fs::OpenOptions::new().write(true).open(lock_file).unwrap();
    holder.lock().unwrap();

    let root = d.root();
    let blocked = std::thread::spawn(move || client.call(Request::Status { root }));
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    let mut second = d.client();
    let answer = second.call(Request::Status { root: other.root() });
    let waited = started.elapsed();
    holder.unlock().unwrap();

    assert!(answer.is_ok());
    assert!(
        waited < Duration::from_secs(5),
        "waited {waited:?} behind the busy project"
    );
    assert!(blocked.join().unwrap().is_ok());
}
