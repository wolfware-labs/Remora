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
/// Add new fields only at the end, with `#[serde(default)]`.
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
}
