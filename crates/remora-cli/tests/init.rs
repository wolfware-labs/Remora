#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::fs;
use std::path::Path;

use remora_cli::init::{InitError, install, uninstall, update_file};
use serde_json::{Value, json};

const EXE: &str = "/home/me/.cargo/bin/remora";

fn ours(timeout: u64) -> Value {
    json!({ "type": "command", "command": "\"/home/me/.cargo/bin/remora\" hook claude-code", "timeout": timeout })
}

fn installed(mut settings: Value) -> Value {
    install(&mut settings, Path::new(EXE)).unwrap();
    settings
}

#[test]
fn hooks_are_installed_into_empty_settings() {
    assert_eq!(
        installed(json!({})),
        json!({ "hooks": {
            "UserPromptSubmit": [{ "hooks": [ours(30)] }],
            "PreToolUse": [{ "matcher": "Bash", "hooks": [ours(30)] }],
            "PostToolUse": [{ "matcher": "Bash", "hooks": [ours(10)] }],
            "PostToolUseFailure": [{ "matcher": "Bash", "hooks": [ours(10)] }],
            "Stop": [{ "hooks": [ours(10)] }],
            "StopFailure": [{ "hooks": [ours(10)] }],
            "SessionEnd": [{ "hooks": [ours(5)] }],
        }})
    );
}

#[test]
fn the_users_own_settings_and_hooks_are_kept() {
    let theirs = json!({ "type": "command", "command": "prettier --write" });
    let settings = installed(json!({
        "model": "opus",
        "hooks": { "PostToolUse": [{ "matcher": "Write|Edit", "hooks": [theirs] }] }
    }));

    assert_eq!(settings["model"], "opus");
    assert_eq!(
        settings["hooks"]["PostToolUse"],
        json!([
            { "matcher": "Write|Edit", "hooks": [theirs] },
            { "matcher": "Bash", "hooks": [ours(10)] },
        ])
    );
}

#[test]
fn installing_twice_changes_nothing() {
    let once = installed(json!({}));
    let mut twice = once.clone();

    assert!(!install(&mut twice, Path::new(EXE)).unwrap());
    assert_eq!(twice, once);
}

#[test]
fn a_moved_remora_replaces_the_old_hooks() {
    let mut settings = installed(json!({}));

    install(&mut settings, Path::new("/opt/remora/bin/remora")).unwrap();

    let commands: Vec<_> = settings["hooks"]["Stop"][0]["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["command"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(commands, ["\"/opt/remora/bin/remora\" hook claude-code"]);
    assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 1);
}

#[test]
fn uninstall_removes_only_what_remora_added() {
    let theirs = json!({ "type": "command", "command": "notify-send done" });
    let original = json!({ "hooks": { "Stop": [{ "hooks": [theirs] }] }, "theme": "dark" });
    let mut settings = installed(original.clone());

    assert!(uninstall(&mut settings));

    assert_eq!(settings, original);
    assert!(!uninstall(&mut settings), "nothing left to remove");
}

#[test]
fn uninstall_drops_the_hooks_key_when_only_remora_used_it() {
    let mut settings = installed(json!({ "model": "sonnet" }));

    uninstall(&mut settings);

    assert_eq!(settings, json!({ "model": "sonnet" }));
}

#[test]
fn settings_with_an_unexpected_shape_are_refused() {
    let mut settings = json!({ "hooks": ["not", "an", "object"] });
    assert!(install(&mut settings, Path::new(EXE)).is_err());
}

fn write_settings(dir: &Path, text: &str) -> std::path::PathBuf {
    let path = dir.join("settings.json");
    fs::write(&path, text).unwrap();
    path
}

#[test]
fn the_settings_file_is_updated_with_a_backup_and_its_key_order_kept() {
    let dir = tempfile::TempDir::new().unwrap();
    let original = "{\n  \"permissions\": { \"allow\": [] },\n  \"env\": {},\n  \"model\": \"opus\"\n}\n";
    let path = write_settings(dir.path(), original);

    assert!(update_file(&path, |s| install(s, Path::new(EXE))).unwrap());

    let text = fs::read_to_string(&path).unwrap();
    let keys: Vec<_> = ["\"permissions\"", "\"env\"", "\"model\"", "\"hooks\""]
        .iter()
        .map(|k| text.find(k).unwrap())
        .collect();
    assert!(keys.windows(2).all(|w| w[0] < w[1]), "{text}");
    assert_eq!(
        fs::read_to_string(dir.path().join("settings.json.remora.bak")).unwrap(),
        original
    );
}

#[test]
fn a_missing_settings_file_is_created() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("new/.claude/settings.json");

    update_file(&path, |s| install(s, Path::new(EXE))).unwrap();

    let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written, installed(json!({})));
}

#[test]
fn invalid_json_is_refused_and_left_untouched() {
    let dir = tempfile::TempDir::new().unwrap();
    let broken = "{ \"model\": \"opus\", }";
    let path = write_settings(dir.path(), broken);

    let result = update_file(&path, |s| install(s, Path::new(EXE)));

    assert!(
        matches!(result, Err(InitError::InvalidJson { line: 1, .. })),
        "{result:?}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), broken);
}

#[cfg(unix)]
#[test]
fn a_symlinked_settings_file_is_updated_where_it_points() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::TempDir::new().unwrap();
    let dotfiles = dir.path().join("dotfiles");
    fs::create_dir(&dotfiles).unwrap();
    let real = write_settings(&dotfiles, "{}");
    fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
    let link = dir.path().join("settings.json");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    update_file(&link, |s| install(s, Path::new(EXE))).unwrap();

    assert!(
        fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
        "the link is kept"
    );
    let written: Value = serde_json::from_str(&fs::read_to_string(&real).unwrap()).unwrap();
    assert_eq!(written, installed(json!({})));
    assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn a_private_settings_file_stays_private_and_so_does_its_backup() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_settings(dir.path(), r#"{ "env": { "API_TOKEN": "secret" } }"#);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    // A world-readable leftover from an earlier run must not keep its mode.
    let backup = dir.path().join("settings.json.remora.bak");
    fs::write(&backup, "old").unwrap();
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o644)).unwrap();

    update_file(&path, |s| install(s, Path::new(EXE))).unwrap();

    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&backup), 0o600);
    assert!(fs::read_to_string(&backup).unwrap().contains("secret"));
}
