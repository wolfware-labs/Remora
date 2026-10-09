#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use common::{REMORA, Setup, after_bash, before_bash, prompt, stderr, stdout, stop};

/// A recorded session: a prompt, then `rm -rf src` with its checkpoint, then the end.
fn recorded_rm_rf(s: &Setup) {
    let cwd = s.repo.path().to_path_buf();
    s.write("src/main.rs", "fn main() {}");
    assert!(s.hook(&prompt(&cwd, "clean up")).status.success());
    assert!(
        s.hook(&before_bash(&cwd, "rm -rf src", "toolu_1"))
            .status
            .success()
    );
    std::fs::remove_dir_all(cwd.join("src")).unwrap();
    assert!(
        s.hook(&after_bash(&cwd, "rm -rf src", "toolu_1"))
            .status
            .success()
    );
    assert!(s.hook(&stop(&cwd)).status.success());
}

#[test]
fn what_rm_rf_deleted_comes_back_with_remora_rewind() {
    let s = Setup::new();
    recorded_rm_rf(&s);

    let log = s.remora(&["log"]);
    assert!(log.status.success(), "{}", stderr(&log));
    let listing = stdout(&log);
    assert!(
        listing.contains("#1") && listing.contains("before command") && listing.contains("rm -rf src"),
        "{listing}"
    );
    assert!(
        listing.contains("#2") && listing.contains("clean up"),
        "{listing}"
    );

    let rewind = s.remora(&["rewind", "#1", "--yes"]);

    assert!(rewind.status.success(), "{}", stderr(&rewind));
    let out = stdout(&rewind);
    assert!(out.contains("Rewind to #1"), "the preview comes first: {out}");
    assert!(out.contains("restore"), "{out}");
    assert!(
        out.contains("Undo this rewind: remora rewind --snapshot"),
        "{out}"
    );
    assert_eq!(s.read("src/main.rs"), "fn main() {}");
    assert!(s.remora(&["verify"]).status.success());
}

#[test]
fn a_rewind_can_be_undone_with_the_snapshot_it_prints() {
    let s = Setup::new();
    recorded_rm_rf(&s);
    let out = stdout(&s.remora(&["rewind", "1", "--yes"]));
    let snapshot = out
        .lines()
        .find_map(|l| l.strip_prefix("Undo this rewind: remora rewind --snapshot "))
        .unwrap();

    let undo = s.remora(&["rewind", "--snapshot", snapshot, "--yes"]);

    assert!(undo.status.success(), "{}", stderr(&undo));
    assert!(!s.repo.path().join("src").exists(), "back to after the rm -rf");
}

#[test]
fn rewinding_off_a_terminal_needs_yes() {
    let s = Setup::new();
    recorded_rm_rf(&s);

    let refused = s.remora(&["rewind", "1"]);

    assert_eq!(refused.status.code(), Some(2));
    assert!(stderr(&refused).contains("--yes"));
    assert!(!s.repo.path().join("src").exists(), "nothing changed");
}

#[test]
fn a_rewind_while_the_agent_works_is_refused_and_says_why() {
    let s = Setup::new();
    let cwd = s.repo.path().to_path_buf();
    s.hook(&prompt(&cwd, "long task"));

    let refused = s.remora(&["rewind", "1", "--yes"]);

    assert_eq!(refused.status.code(), Some(3), "{}", stderr(&refused));
    let message = stderr(&refused);
    assert!(
        message.contains("0f8c2b1e") && message.contains("--force") && message.contains("Esc"),
        "{message}"
    );
    assert!(
        !stdout(&refused).contains("Rewind to"),
        "refused before the preview"
    );
    assert!(s.remora(&["rewind", "1", "--yes", "--force"]).status.success());
}

#[test]
fn a_point_number_past_the_end_is_a_usage_error() {
    let s = Setup::new();
    recorded_rm_rf(&s);

    let out = s.remora(&["rewind", "#9", "--yes"]);

    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("there is no #9"), "{}", stderr(&out));
}

#[test]
fn status_shows_the_project_and_open_sessions() {
    let s = Setup::new();
    let cwd = s.repo.path().to_path_buf();
    s.hook(&prompt(&cwd, "long task"));
    s.hook(&before_bash(&cwd, "cargo build", "toolu_1"));

    let out = s.remora(&["status"]);

    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("daemon") && text.contains("running"), "{text}");
    assert!(
        text.contains("0f8c2b1e") && text.contains("cargo build"),
        "{text}"
    );
    let json: serde_json::Value = serde_json::from_slice(&s.remora(&["status", "--json"]).stdout).unwrap();
    assert_eq!(json["restore_points"], 2);
}

#[test]
fn commands_without_a_daemon_say_so_with_exit_code_4() {
    let s = Setup::new();

    let out = s.remora(&["status"]);

    assert_eq!(out.status.code(), Some(4));
    assert!(stdout(&out).contains("daemon not running"));
}

#[test]
fn outside_a_git_repository_a_root_is_required() {
    let s = Setup::new();
    let outside = tempfile::TempDir::new().unwrap();
    let out = Command::new(REMORA)
        .args(["log"])
        .env("REMORA_DATA_DIR", s.data.path())
        .current_dir(outside.path())
        .output()
        .unwrap();
    if outside.path().ancestors().any(|a| a.join(".git").exists()) {
        return; // the temporary directory itself sits inside a repository
    }
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--root"));
}

#[test]
fn init_and_uninit_manage_the_claude_code_hooks() {
    let s = Setup::new();
    let config = tempfile::TempDir::new().unwrap();
    let run = |args: &[&str]| {
        Command::new(REMORA)
            .args(args)
            .env("REMORA_DATA_DIR", s.data.path())
            .env("CLAUDE_CONFIG_DIR", config.path())
            .output()
            .unwrap()
    };

    let init = run(&["init", "--no-service"]);

    assert!(init.status.success(), "{}", stderr(&init));
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config.path().join("settings.json")).unwrap()).unwrap();
    let command = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(command.ends_with("hook claude-code"), "{command}");
    assert!(stdout(&run(&["init", "--no-service"])).contains("already installed"));

    assert!(run(&["uninit"]).status.success());
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config.path().join("settings.json")).unwrap()).unwrap();
    assert_eq!(settings, serde_json::json!({}));
}

#[test]
fn a_hook_that_snapshots_nothing_is_fast() {
    let s = Setup::new();
    let cwd = s.repo.path().to_path_buf();
    s.hook(&prompt(&cwd, "warm up")); // starts the daemon
    let payload = after_bash(&cwd, "true", "toolu_x");

    let mut times: Vec<Duration> = (0..20)
        .map(|_| {
            let started = Instant::now();
            assert!(s.hook(&payload).status.success());
            started.elapsed()
        })
        .collect();
    times.sort();

    assert!(times[10] < Duration::from_millis(50), "median {:?}", times[10]);
}

#[test]
fn hooks_always_exit_0() {
    let s = Setup::new();
    let out = s.remora_with_input(&["hook", "claude-code"], Some("garbage"));
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn a_rewind_that_restores_only_part_of_the_project_fails_and_says_how_to_go_back() {
    use std::os::unix::fs::PermissionsExt;
    let s = Setup::new();
    let cwd = s.repo.path().to_path_buf();
    s.write("locked/a.txt", "v1");
    s.write("free.txt", "v1");
    s.hook(&prompt(&cwd, "edit both"));
    s.write("locked/a.txt", "v2");
    s.write("free.txt", "v2");
    s.hook(&stop(&cwd));
    let locked = cwd.join("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(locked.join("probe"), "").is_ok() {
        return; // running as root: permissions are not enforced
    }

    let out = s.remora(&["rewind", "1", "--yes"]);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("could not restore locked/a.txt"),
        "{}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("remora rewind --snapshot"),
        "{}",
        stderr(&out)
    );
    assert_eq!(s.read("free.txt"), "v1", "what could be restored was");
}

#[test]
fn a_relative_or_subfolder_root_means_its_repository() {
    let s = Setup::new();
    recorded_rm_rf(&s);
    std::fs::create_dir_all(s.repo.path().join("docs")).unwrap();

    let from_dot = stdout(&s.remora(&["log", "--root", "."]));
    let from_sub = stdout(&s.remora(&["log", "--root", "docs"]));

    assert!(from_dot.contains("rm -rf src"), "{from_dot}");
    assert_eq!(from_sub, from_dot);
}

#[test]
fn a_late_checkpoint_is_labelled_in_log_and_its_preview_warns() {
    let s = Setup::new();
    let cwd = s.repo.path().to_path_buf();
    s.write("src/main.rs", "fn main() {}");
    assert!(s.hook(&prompt(&cwd, "build")).status.success());
    // A hook that had already given up when the checkpoint finished.
    let mut client = remora_ipc::Client::connect(s.data.path()).unwrap();
    client
        .call(remora_ipc::Request::BeforeCommand {
            root: s.root(),
            session: common::SESSION.into(),
            agent: "claude-code".into(),
            command: "make".into(),
            tool_use_id: Some("toolu_1".into()),
            deadline_ms: Some(0),
        })
        .unwrap();

    let listing = stdout(&s.remora(&["log"]));
    assert!(listing.contains("before command (late)"), "{listing}");
    let rewind = s.remora(&["rewind", "#1", "--yes", "--force"]);
    let out = stdout(&rewind);
    assert!(out.contains("taken after the command may have started"), "{out}");
}

#[cfg(unix)]
#[test]
fn a_socket_folder_open_to_others_is_reported_as_unsafe() {
    use std::os::unix::fs::PermissionsExt;
    // A data directory long enough that its socket goes to the runtime folder, and a
    // runtime folder other users could write in.
    let parent = tempfile::TempDir::new().unwrap();
    let data = parent.path().join("d".repeat(110));
    std::fs::create_dir(&data).unwrap();
    // Create a secret so the client progresses to the socket directory check.
    // The secret is 32 bytes in hex (64 characters).
    std::fs::write(
        data.join("remorad.secret"),
        b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    )
    .unwrap();
    let runtime = tempfile::TempDir::new().unwrap();
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let repo = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(repo.path().join(".git")).unwrap();

    let out = std::process::Command::new(REMORA)
        .args(["status", "--data-dir"])
        .arg(&data)
        .arg("--root")
        .arg(repo.path())
        .env("XDG_RUNTIME_DIR", runtime.path())
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("unsafe socket directory"),
        "{}",
        stderr(&out)
    );
}
