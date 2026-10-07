//! A temporary git repository, a temporary data directory, and Claude Code payloads.
#![allow(dead_code, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

pub const SESSION: &str = "0f8c2b1e-7d4a-4c1b-9e2f-5a6b7c8d9e0f";
pub const REMORA: &str = env!("CARGO_BIN_EXE_remora");

/// A git repository (a `.git` folder is all Remora looks for) and a data directory.
pub struct Setup {
    pub repo: TempDir,
    pub data: TempDir,
}

impl Setup {
    pub fn new() -> Self {
        let repo = TempDir::new().unwrap();
        fs::create_dir(repo.path().join(".git")).unwrap();
        Self {
            repo,
            data: TempDir::new().unwrap(),
        }
    }

    pub fn root(&self) -> PathBuf {
        self.repo.path().canonicalize().unwrap()
    }

    pub fn write(&self, rel: &str, contents: &str) {
        let path = self.repo.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    pub fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.repo.path().join(rel)).unwrap()
    }

    /// Run the real `remora` binary against this data directory.
    pub fn remora(&self, args: &[&str]) -> Output {
        self.remora_with_input(args, None)
    }

    pub fn remora_with_input(&self, args: &[&str], input: Option<&str>) -> Output {
        use std::io::Write;
        let mut child = Command::new(REMORA)
            .args(args)
            .env("REMORA_DATA_DIR", self.data.path())
            .current_dir(self.repo.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        if let Some(input) = input {
            stdin.write_all(input.as_bytes()).unwrap();
        }
        drop(stdin);
        child.wait_with_output().unwrap()
    }

    /// Send one Claude Code event through the real `remora hook claude-code`.
    pub fn hook(&self, payload: &Value) -> Output {
        self.remora_with_input(&["hook", "claude-code"], Some(&payload.to_string()))
    }

    /// Stop whatever daemon a hook started for this data directory.
    pub fn stop_daemon(&self) {
        let _ = remora_ipc::stop_daemon(self.data.path(), Duration::from_secs(10));
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.stop_daemon();
    }
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A Claude Code payload for `event` in `cwd`, with `extra` fields merged in.
pub fn payload(event: &str, cwd: &Path, extra: Value) -> Value {
    let mut payload = json!({
        "session_id": SESSION,
        "transcript_path": "/home/me/.claude/projects/x/transcript.jsonl",
        "cwd": cwd,
        "permission_mode": "default",
        "hook_event_name": event,
    });
    if let (Some(payload), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        payload.extend(extra.clone());
    }
    payload
}

pub fn prompt(cwd: &Path, text: &str) -> Value {
    payload("UserPromptSubmit", cwd, json!({ "prompt": text }))
}

pub fn before_bash(cwd: &Path, command: &str, id: &str) -> Value {
    payload(
        "PreToolUse",
        cwd,
        json!({ "tool_name": "Bash", "tool_input": { "command": command }, "tool_use_id": id }),
    )
}

pub fn after_bash(cwd: &Path, command: &str, id: &str) -> Value {
    payload(
        "PostToolUse",
        cwd,
        json!({ "tool_name": "Bash", "tool_input": { "command": command }, "tool_use_id": id,
                "tool_response": { "stdout": "", "stderr": "", "interrupted": false } }),
    )
}

pub fn stop(cwd: &Path) -> Value {
    payload("Stop", cwd, json!({ "stop_hook_active": false }))
}
