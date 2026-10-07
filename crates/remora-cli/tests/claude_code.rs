//! The real Claude Code with Remora's hooks. It calls the model, so it only runs when
//! `REMORA_TEST_CLAUDE=1` is set (and `claude` is installed and signed in).
#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use std::path::Path;
use std::process::Command;

use common::{REMORA, Setup, stderr, stdout};

#[test]
fn a_real_claude_code_session_is_recorded_and_rewound() {
    if std::env::var("REMORA_TEST_CLAUDE").as_deref() != Ok("1") {
        return;
    }
    let s = Setup::new();
    s.write("src/main.rs", "fn main() {}");
    // Remora's hooks for this run only: the user's own settings are left alone.
    let mut settings = serde_json::json!({});
    remora_cli::init::install(&mut settings, Path::new(REMORA)).unwrap();
    let settings_dir = tempfile::TempDir::new().unwrap();
    let settings_file = settings_dir.path().join("settings.json");
    std::fs::write(&settings_file, settings.to_string()).unwrap();

    let claude = Command::new("claude")
        .args([
            "-p",
            "This is a disposable test repository. Use the Bash tool to run exactly this command, then stop: printf replaced > src/main.rs",
        ])
        .arg("--settings")
        .arg(&settings_file)
        .args(["--allowedTools", "Bash", "--model", "haiku"])
        .env("REMORA_DATA_DIR", s.data.path())
        .current_dir(s.repo.path())
        .output()
        .unwrap();
    assert!(claude.status.success(), "{}", stderr(&claude));
    assert_eq!(
        s.read("src/main.rs"),
        "replaced",
        "Claude did not run the command: {}",
        stdout(&claude)
    );

    let log = s.remora(&["log", "--json"]);
    let points: serde_json::Value = serde_json::from_slice(&log.stdout).unwrap();
    let number = points
        .as_array()
        .unwrap()
        .iter()
        .find(|p| {
            p["point"]["kind"] == "before_command"
                && p["point"]["label"]
                    .as_str()
                    .unwrap_or("")
                    .contains("printf replaced")
        })
        .map(|p| p["number"].as_u64().unwrap())
        .unwrap_or_else(|| panic!("no checkpoint before the command: {points}"));

    let rewind = s.remora(&["rewind", &number.to_string(), "--yes"]);

    assert!(rewind.status.success(), "{}", stderr(&rewind));
    assert_eq!(s.read("src/main.rs"), "fn main() {}");
}
