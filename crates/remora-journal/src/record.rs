use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Turn,
    Rewind,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::Turn => "turn",
            EntryKind::Rewind => "rewind",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "turn" => Some(EntryKind::Turn),
            "rewind" => Some(EntryKind::Rewind),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRecord {
    pub command: String,
    pub exit_code: Option<i32>,
    /// The checkpoint snapshot taken just before the command ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    /// When the command started (milliseconds since the Unix epoch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<i64>,
    /// The agent's id for this tool call, pairing the command with its result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// The checkpoint finished after the agent stopped waiting for it, so the command may
    /// already have started: it may include the command's effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub late: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub kind: FileChangeKind,
}

/// The content of one journal entry: what gets hashed and signed.
///
/// The JSON serialization of this struct is the canonical form used for hashing.
/// NEVER reorder, rename or remove fields: old journals would stop verifying.
/// Add new fields only at the end, as `Option` with
/// `#[serde(default, skip_serializing_if = "Option::is_none")]`, so an old entry (where
/// the field is absent) serializes, and therefore hashes, exactly as it did before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRecord {
    pub session_id: String,
    pub kind: EntryKind,
    pub agent: String,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub snapshot_id: Option<String>,
    pub prompt: Option<String>,
    pub commands: Vec<CommandRecord>,
    pub changes: Vec<FileChange>,
    /// The turn never saw an end event (for example, the user interrupted the agent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted: Option<bool>,
    /// For `Rewind` entries: the snapshot that was restored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewind_target: Option<String>,
    /// For `Rewind` entries: the restore did not fully succeed; `changes` lists what
    /// actually changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete: Option<bool>,
}
