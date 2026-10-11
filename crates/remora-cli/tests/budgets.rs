//! Latency budgets (spec: a hook's p99 under 50 ms). Measured in release builds, through the
//! hook's own entry point against an in-process daemon, so process starts and debug code do
//! not blur them:
//!
//! ```text
//! cargo test --release -p remora-cli --test budgets -- --ignored
//! ```
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::time::{Duration, Instant};

use common::{Setup, after_bash, before_bash, prompt};
use remora_cli::hook::{HookEnv, run};
use remora_daemon::{Daemon, DaemonConfig};

const CALLS: usize = 200;
const BUDGET: Duration = Duration::from_millis(50);

/// The 99th percentile of `CALLS` runs of `call`.
fn p99(mut call: impl FnMut(usize)) -> Duration {
    let mut times: Vec<Duration> = (0..CALLS)
        .map(|i| {
            let started = Instant::now();
            call(i);
            started.elapsed()
        })
        .collect();
    times.sort();
    times[CALLS * 99 / 100 - 1]
}

fn with_daemon(files: usize, measure: impl FnOnce(&Setup, &HookEnv)) {
    let s = Setup::new();
    // Written an hour ago, like the files of a project nobody touched: a file changed within
    // the last two seconds is always re-read (racy timestamps), which is not this budget.
    let an_hour_ago = std::time::SystemTime::now() - Duration::from_secs(3_600);
    for i in 0..files {
        let rel = format!("d{}/f{i}.txt", i / 100);
        s.write(&rel, &format!("file {i} ").repeat(400));
        std::fs::File::options()
            .write(true)
            .open(s.repo.path().join(rel))
            .and_then(|file| file.set_modified(an_hour_ago))
            .unwrap();
    }
    let daemon = Daemon::start(DaemonConfig::new(s.data.path())).unwrap();
    let env = HookEnv {
        data_dir: s.data.path().to_path_buf(),
        remora: None,
        start_wait: Duration::from_secs(5),
        replace_wait: Duration::from_secs(2),
        version: remora_cli::upgrade::VERSION.to_owned(),
    };
    // The first snapshot of the project is not what the budget is about.
    assert_eq!(
        run(prompt(s.repo.path(), "warm up").to_string().as_bytes(), &env),
        None
    );
    measure(&s, &env);
    daemon.shutdown();
    daemon.wait();
}

#[test]
#[ignore = "a release-build budget: run with --release -- --ignored"]
fn a_hook_that_snapshots_nothing_stays_within_budget() {
    with_daemon(10, |s, env| {
        let payload = after_bash(s.repo.path(), "true", "toolu_1").to_string();
        let p99 = p99(|_| assert_eq!(run(payload.as_bytes(), env), None));
        assert!(p99 < BUDGET, "p99 {p99:?} over {BUDGET:?}");
    });
}

#[test]
#[ignore = "a release-build budget: run with --release -- --ignored"]
fn a_checkpoint_of_an_unchanged_project_stays_within_budget() {
    with_daemon(1_000, |s, env| {
        let p99 = p99(|i| {
            let payload = before_bash(s.repo.path(), "true", &format!("toolu_{i}")).to_string();
            assert_eq!(run(payload.as_bytes(), env), None);
        });
        assert!(p99 < BUDGET, "p99 {p99:?} over {BUDGET:?}");
    });
}
