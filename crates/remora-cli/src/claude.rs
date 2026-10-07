//! Claude Code hook payloads (JSON on stdin) turned into [`HookEvent`]s. Unknown fields are
//! ignored, so new Claude Code versions keep working.

use std::path::PathBuf;

use serde::Deserialize;

/// The agent name Remora records for Claude Code.
pub const AGENT: &str = "claude-code";
/// Prompts longer than this are cut before they are sent to the daemon.
pub const MAX_PROMPT_BYTES: usize = 4 * 1024;
/// Commands longer than this are cut before they are sent to the daemon.
pub const MAX_COMMAND_BYTES: usize = 16 * 1024;

/// What a hook call asks Remora to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookEvent {
    /// `UserPromptSubmit`: a turn starts.
    PromptSubmitted {
        session: String,
        cwd: PathBuf,
        prompt: String,
    },
    /// `PreToolUse` for Bash: snapshot before the command runs.
    BeforeBash {
        session: String,
        cwd: PathBuf,
        command: String,
        tool_use_id: Option<String>,
    },
    /// `PostToolUse` / `PostToolUseFailure` for Bash.
    AfterBash {
        session: String,
        cwd: PathBuf,
        command: String,
        tool_use_id: Option<String>,
        exit_code: Option<i32>,
    },
    /// `Stop` / `StopFailure`: the turn ended.
    TurnEnded { session: String, cwd: PathBuf },
    /// `SessionEnd`: the session is over; an open turn is closed as interrupted.
    SessionEnded { session: String, cwd: PathBuf },
    /// Any other event or tool: nothing to record.
    Ignored,
}

#[derive(Debug, thiserror::Error)]
#[error("unreadable Claude Code hook payload: {0}")]
pub struct ParseError(#[from] serde_json::Error);

#[derive(Deserialize)]
struct Payload {
    hook_event_name: String,
    session_id: String,
    cwd: PathBuf,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_input: Option<ToolInput>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    is_interrupt: Option<bool>,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Deserialize)]
struct ToolInput {
    #[serde(default)]
    command: Option<String>,
}

/// Parse one hook payload.
pub fn parse(json: &str) -> Result<HookEvent, ParseError> {
    let p: Payload = serde_json::from_str(json)?;
    // Without a session there is nothing to record a turn under.
    if p.session_id.is_empty() {
        return Ok(HookEvent::Ignored);
    }
    let session = p.session_id.clone();
    let cwd = p.cwd.clone();
    let bash_command = || -> Option<String> {
        if p.tool_name.as_deref() != Some("Bash") {
            return None;
        }
        let command = p.tool_input.as_ref()?.command.clone()?;
        // A subagent's commands are labelled with its type.
        let command = match (&p.agent_id, &p.agent_type) {
            (Some(_), Some(agent_type)) => format!("[{agent_type}] {command}"),
            _ => command,
        };
        Some(cut(&command, MAX_COMMAND_BYTES))
    };
    Ok(match p.hook_event_name.as_str() {
        "UserPromptSubmit" => HookEvent::PromptSubmitted {
            session,
            cwd,
            prompt: cut(p.prompt.as_deref().unwrap_or_default(), MAX_PROMPT_BYTES),
        },
        "PreToolUse" => match bash_command() {
            Some(command) => HookEvent::BeforeBash {
                session,
                cwd,
                command,
                tool_use_id: p.tool_use_id.clone(),
            },
            None => HookEvent::Ignored,
        },
        "PostToolUse" | "PostToolUseFailure" => match bash_command() {
            Some(command) => HookEvent::AfterBash {
                session,
                cwd,
                command,
                tool_use_id: p.tool_use_id.clone(),
                exit_code: if p.hook_event_name == "PostToolUse" {
                    Some(0)
                } else if p.is_interrupt == Some(true) {
                    None
                } else {
                    p.error.as_deref().and_then(exit_code_of)
                },
            },
            None => HookEvent::Ignored,
        },
        "Stop" | "StopFailure" => HookEvent::TurnEnded { session, cwd },
        "SessionEnd" => HookEvent::SessionEnded { session, cwd },
        _ => HookEvent::Ignored,
    })
}

/// `N` from a Bash failure message starting `Exit code N`.
fn exit_code_of(error: &str) -> Option<i32> {
    let rest = error.strip_prefix("Exit code ")?;
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

/// `text` cut to at most `max` bytes at a character boundary, with `…` when cut.
pub fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_read_from_failure_messages() {
        assert_eq!(exit_code_of("Exit code 1\nError: Cannot find module"), Some(1));
        assert_eq!(exit_code_of("Exit code 127"), Some(127));
        assert_eq!(exit_code_of("Command timed out"), None);
    }

    #[test]
    fn long_text_is_cut_at_a_character_boundary() {
        assert_eq!(cut("short", 10), "short");
        assert_eq!(cut("ééé", 3), "é…", "never splits a character");
    }
}
