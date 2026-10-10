#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::{SESSION, Setup, after_bash, before_bash, payload, prompt, stop};
use remora_cli::hook::{HookEnv, run};
use remora_daemon::{Daemon, DaemonConfig, DaemonHandle};
use remora_ipc::{Client, Request, Response};
use serde_json::json;

/// Run hooks in this process against an in-process daemon.
struct Hooks {
    setup: Setup,
    daemon: Option<DaemonHandle>,
}

impl Hooks {
    fn with_daemon() -> Self {
        let setup = Setup::new();
        let daemon = Daemon::start(DaemonConfig::new(setup.data.path())).unwrap();
        Self {
            setup,
            daemon: Some(daemon),
        }
    }

    fn without_daemon() -> Self {
        Self {
            setup: Setup::new(),
            daemon: None,
        }
    }

    fn env(&self, remora: Option<PathBuf>) -> HookEnv {
        HookEnv {
            data_dir: self.setup.data.path().to_path_buf(),
            remora,
            start_wait: Duration::from_secs(5),
        }
    }

    fn send(&self, payload: &serde_json::Value) -> Option<String> {
        run(payload.to_string().as_bytes(), &self.env(None))
    }

    fn client(&self) -> Client {
        Client::connect(self.setup.data.path()).unwrap()
    }

    fn journal_entries(&self) -> usize {
        match self
            .client()
            .call(Request::Status {
                root: self.setup.root(),
            })
            .unwrap()
        {
            Response::Status { status } => status.journal_entries,
            other => panic!("unexpected {other:?}"),
        }
    }
}

impl Drop for Hooks {
    fn drop(&mut self) {
        if let Some(daemon) = &self.daemon {
            daemon.shutdown();
            daemon.wait();
        }
    }
}

#[test]
fn a_session_is_recorded_and_its_commands_paired_by_tool_use_id() {
    let h = Hooks::with_daemon();
    let cwd = h.setup.repo.path().to_path_buf();

    assert_eq!(h.send(&prompt(&cwd, "build it")), None);
    assert_eq!(h.send(&before_bash(&cwd, "make", "toolu_1")), None);
    let failed = payload(
        "PostToolUseFailure",
        &cwd,
        json!({ "tool_name": "Bash", "tool_input": { "command": "make" }, "tool_use_id": "toolu_1",
                "error": "Exit code 2\nmake: *** No targets", "is_interrupt": false }),
    );
    assert_eq!(h.send(&failed), None);
    assert_eq!(h.send(&stop(&cwd)), None);

    let points = match h
        .client()
        .call(Request::RestorePoints {
            root: h.setup.root(),
            skip: 0,
            limit: None,
        })
        .unwrap()
    {
        Response::RestorePoints { points, .. } => points,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(points.len(), 2);
    assert_eq!(h.journal_entries(), 1);
}

#[test]
fn a_rewind_rolled_back_at_start_is_told_once_per_session() {
    let mut h = Hooks::with_daemon();
    let cwd = h.setup.repo.path().to_path_buf();
    h.setup.write("a.txt", "kept");
    h.send(&prompt(&cwd, "edit"));
    h.send(&stop(&cwd));
    let point = match h
        .client()
        .call(Request::RestorePoints {
            root: h.setup.root(),
            skip: 0,
            limit: None,
        })
        .unwrap()
    {
        Response::RestorePoints { mut points, .. } => points.remove(0),
        other => panic!("unexpected {other:?}"),
    };
    // The daemon died in the middle of a rewind: the record is there, the tree half changed.
    let daemon = h.daemon.take().unwrap();
    daemon.shutdown();
    daemon.wait();
    let dir = h
        .setup
        .data
        .path()
        .join("projects")
        .join(remora_engine::project_id(&h.setup.root()));
    let intent = json!({ "safety": point.snapshot, "target": point.snapshot, "started_at_ms": 1 });
    std::fs::write(dir.join(remora_engine::REWIND_FILE), intent.to_string()).unwrap();
    h.setup.write("a.txt", "half rewound");
    h.daemon = Some(Daemon::start(DaemonConfig::new(h.setup.data.path())).unwrap());

    let first = h.send(&prompt(&cwd, "next")).unwrap();
    let second = h.send(&before_bash(&cwd, "ls", "toolu_1"));

    let message: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(
        message["systemMessage"].as_str().unwrap(),
        format!("Remora: {}", remora_engine::ROLLED_BACK_NOTICE)
    );
    assert_eq!(second, None, "once per session");
    assert_eq!(h.setup.read("a.txt"), "kept");
}

#[test]
fn a_subfolder_records_into_its_repository() {
    let h = Hooks::with_daemon();
    h.setup.write("src/lib.rs", "");
    let sub = h.setup.repo.path().join("src");

    h.send(&prompt(&sub, "edit"));
    h.send(&stop(&sub));

    assert_eq!(h.journal_entries(), 1);
}

#[test]
fn nothing_is_recorded_outside_git_or_in_an_ignored_repository() {
    let h = Hooks::without_daemon();
    let outside = tempfile::TempDir::new().unwrap();
    std::fs::write(h.setup.repo.path().join(".remora-ignore"), "").unwrap();

    // No daemon and no way to start one: any attempt to record would warn.
    assert_eq!(h.send(&prompt(outside.path(), "hi")), None);
    assert_eq!(h.send(&prompt(h.setup.repo.path(), "hi")), None);
}

#[test]
fn a_session_whose_daemon_cannot_start_is_warned_about_once() {
    let h = Hooks::without_daemon();
    let cwd = h.setup.repo.path().to_path_buf();

    let first = h.send(&prompt(&cwd, "hi")).unwrap();
    let second = h.send(&before_bash(&cwd, "ls", "toolu_1"));

    let message: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert!(
        message["systemMessage"]
            .as_str()
            .unwrap()
            .starts_with("Remora is not recording this session")
    );
    assert_eq!(second, None, "once per session and reason");
    let log = std::fs::read_to_string(h.setup.data.path().join("hook.log")).unwrap();
    assert!(log.contains(SESSION), "{log}");
}

#[test]
fn a_hook_starts_the_daemon_when_none_is_running() {
    let h = Hooks::without_daemon();
    let cwd = h.setup.repo.path().to_path_buf();
    let env = h.env(Some(PathBuf::from(common::REMORA)));

    let answer = run(prompt(&cwd, "hi").to_string().as_bytes(), &env);

    assert_eq!(answer, None, "recorded, no warning");
    assert_eq!(h.journal_entries(), 0);
    let status = h.client().call(Request::Status { root: h.setup.root() }).unwrap();
    assert!(
        matches!(&status, Response::Status { status } if status.open_sessions == [SESSION]),
        "{status:?}"
    );
}

#[test]
fn a_turn_still_ends_after_the_agent_deleted_the_repository() {
    let h = Hooks::with_daemon();
    let cwd = h.setup.repo.path().to_path_buf();
    h.setup.write("notes.txt", "keep");
    h.send(&prompt(&cwd, "clean everything"));
    h.send(&before_bash(&cwd, "rm -rf .", "toolu_1"));

    std::fs::remove_dir_all(&cwd).unwrap();
    h.send(&after_bash(&cwd, "rm -rf .", "toolu_1"));
    h.send(&stop(&cwd));

    assert_eq!(h.journal_entries(), 1, "the turn was journaled in its repository");
}

#[test]
fn unreadable_input_is_logged_and_ignored() {
    let h = Hooks::without_daemon();

    assert_eq!(run(b"this is not json", &h.env(None)), None);
    assert_eq!(run(&[0xff, 0xfe, 0x00], &h.env(None)), None);

    let log = std::fs::read_to_string(h.setup.data.path().join("hook.log")).unwrap();
    assert!(log.contains("unreadable Claude Code hook payload"), "{log}");
}

#[test]
fn a_session_end_closes_an_unfinished_turn() {
    let h = Hooks::with_daemon();
    let cwd = h.setup.repo.path().to_path_buf();
    h.send(&prompt(&cwd, "long task"));

    h.send(&payload(
        "SessionEnd",
        &cwd,
        json!({ "reason": "prompt_input_exit" }),
    ));

    assert_eq!(h.journal_entries(), 1);
}

#[test]
fn a_session_started_outside_git_stays_unrecorded_after_moving_into_a_repository() {
    let h = Hooks::without_daemon();
    let outside = tempfile::TempDir::new().unwrap();
    if outside.path().ancestors().any(|a| a.join(".git").exists()) {
        return; // the temporary directory itself sits inside a repository
    }
    assert_eq!(h.send(&prompt(outside.path(), "hi")), None);

    // The agent then works in a repository: still nothing recorded, so no warning either.
    let repo = h.setup.repo.path().to_path_buf();
    assert_eq!(h.send(&before_bash(&repo, "ls", "toolu_1")), None);
    assert_eq!(h.send(&stop(&repo)), None);
}

#[test]
fn a_failed_session_end_shows_nothing_and_keeps_the_warning_for_later() {
    let h = Hooks::without_daemon();
    let cwd = h.setup.repo.path().to_path_buf();

    let end = payload("SessionEnd", &cwd, json!({ "reason": "other" }));
    assert_eq!(h.send(&end), None, "Claude Code discards SessionEnd output");

    assert!(
        h.send(&prompt(&cwd, "hi")).is_some(),
        "the warning is still shown once"
    );
}

#[test]
fn a_hook_with_bad_arguments_still_exits_0() {
    let s = Setup::new();
    let out = s.remora_with_input(&["hook", "some-other-agent"], Some("{}"));
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn a_session_end_never_starts_a_daemon() {
    let h = Hooks::without_daemon();
    let cwd = h.setup.repo.path().to_path_buf();
    let env = h.env(Some(PathBuf::from(common::REMORA)));

    let end = payload("SessionEnd", &cwd, json!({ "reason": "other" }));
    assert_eq!(run(end.to_string().as_bytes(), &env), None);

    assert!(matches!(
        Client::connect(h.setup.data.path()),
        Err(remora_ipc::ClientError::DaemonNotRunning)
    ));
}
