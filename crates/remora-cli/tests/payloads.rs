#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::path::PathBuf;

use remora_cli::claude::{HookEvent, MAX_COMMAND_BYTES, MAX_PROMPT_BYTES, parse};

const SESSION: &str = "0f8c2b1e-7d4a-4c1b-9e2f-5a6b7c8d9e0f";

fn fixture(name: &str) -> HookEvent {
    let path = format!("{}/tests/fixtures/claude/{name}.json", env!("CARGO_MANIFEST_DIR"));
    parse(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn cwd() -> PathBuf {
    PathBuf::from("/home/me/proj")
}

#[test]
fn a_prompt_starts_a_turn() {
    assert_eq!(
        fixture("user_prompt_submit"),
        HookEvent::PromptSubmitted {
            session: SESSION.into(),
            cwd: cwd(),
            prompt: "Clean up the build directory".into(),
        }
    );
}

#[test]
fn a_bash_call_is_snapshotted_before_and_paired_after() {
    assert_eq!(
        fixture("pre_tool_use_bash"),
        HookEvent::BeforeBash {
            session: SESSION.into(),
            cwd: cwd(),
            command: "rm -rf build".into(),
            tool_use_id: Some("toolu_01ABC123".into()),
        }
    );
    assert_eq!(
        fixture("post_tool_use_bash"),
        HookEvent::AfterBash {
            session: SESSION.into(),
            cwd: cwd(),
            command: "rm -rf build".into(),
            tool_use_id: Some("toolu_01ABC123".into()),
            exit_code: Some(0),
        }
    );
}

#[test]
fn a_failed_bash_call_keeps_its_exit_code() {
    let HookEvent::AfterBash { exit_code, .. } = fixture("post_tool_use_failure_bash") else {
        panic!("expected AfterBash");
    };
    assert_eq!(exit_code, Some(1));
    let HookEvent::AfterBash { exit_code, .. } = fixture("post_tool_use_failure_interrupt") else {
        panic!("expected AfterBash");
    };
    assert_eq!(exit_code, None, "an interrupted command has no exit code");
}

#[test]
fn turns_and_sessions_end() {
    let ended = HookEvent::TurnEnded {
        session: SESSION.into(),
        cwd: cwd(),
    };
    assert_eq!(fixture("stop"), ended);
    assert_eq!(fixture("stop_failure"), ended);
    assert_eq!(
        fixture("session_end"),
        HookEvent::SessionEnded {
            session: SESSION.into(),
            cwd: cwd(),
        }
    );
}

#[test]
fn a_subagent_command_is_labelled_with_its_type() {
    let HookEvent::BeforeBash { command, .. } = fixture("subagent_pre_tool_use_bash") else {
        panic!("expected BeforeBash");
    };
    assert_eq!(command, "[Explore] find . -name '*.rs'");
}

#[test]
fn other_tools_and_events_are_ignored() {
    assert_eq!(fixture("pre_tool_use_write"), HookEvent::Ignored);
    assert_eq!(
        fixture("notification"),
        HookEvent::Ignored,
        "unknown fields are fine"
    );
}

#[test]
fn huge_prompts_and_commands_are_cut() {
    let prompt = serde_json::json!({
        "hook_event_name": "UserPromptSubmit", "session_id": "s", "cwd": "/p",
        "prompt": "x".repeat(MAX_PROMPT_BYTES * 10),
    });
    let HookEvent::PromptSubmitted { prompt, .. } = parse(&prompt.to_string()).unwrap() else {
        panic!("expected PromptSubmitted");
    };
    assert!(prompt.len() <= MAX_PROMPT_BYTES + "…".len());

    let command = serde_json::json!({
        "hook_event_name": "PreToolUse", "session_id": "s", "cwd": "/p", "tool_name": "Bash",
        "tool_input": {"command": "y".repeat(MAX_COMMAND_BYTES * 10)}, "tool_use_id": "t",
    });
    let HookEvent::BeforeBash { command, .. } = parse(&command.to_string()).unwrap() else {
        panic!("expected BeforeBash");
    };
    assert!(command.len() <= MAX_COMMAND_BYTES + "…".len());
}

#[test]
fn a_payload_that_is_not_json_is_an_error() {
    assert!(parse("not json").is_err());
    assert!(parse(r#"{"no_event_name": true}"#).is_err());
}

#[test]
fn a_payload_without_session_or_folder_is_an_error_and_an_empty_session_is_ignored() {
    assert!(parse(r#"{"hook_event_name": "Stop", "cwd": "/p"}"#).is_err());
    assert!(parse(r#"{"hook_event_name": "Stop", "session_id": "s"}"#).is_err());
    assert_eq!(
        parse(r#"{"hook_event_name": "Stop", "session_id": "", "cwd": "/p"}"#).unwrap(),
        HookEvent::Ignored
    );
}
