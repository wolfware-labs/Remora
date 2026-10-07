use std::path::PathBuf;

use remora_engine::{RestorePoint, Status};
use remora_journal::CommandRecord;
use remora_snap::SnapshotId;
use serde::{Deserialize, Serialize};

/// The protocol version. A daemon refuses envelopes and `hello`s with another version.
pub const PROTOCOL_VERSION: u32 = 1;

/// The largest message, newline excluded, either side accepts.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// What a client sends: `{"v":1,"id":7,"req":{...}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub v: u32,
    /// Chosen by the client; echoed in the response.
    pub id: u64,
    pub req: Request,
}

/// What the daemon answers: `{"v":1,"id":7,"ok":{...}}` or `{"v":1,"id":7,"err":{...}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub v: u32,
    /// The request's id, or 0 if the request could not be read.
    pub id: u64,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(Response),
    Err(ErrorBody),
}

/// Project requests carry the project folder (`root`) as the agent sees it; the daemon
/// resolves symlinks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Exchange versions. Answered with [`Response::Hello`] or a `version_mismatch` error.
    Hello {
        client_version: String,
        protocol: u32,
    },
    BeginTurn {
        root: PathBuf,
        session: String,
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    BeforeCommand {
        root: PathBuf,
        session: String,
        agent: String,
        command: String,
    },
    AfterCommand {
        root: PathBuf,
        session: String,
        agent: String,
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    EndTurn {
        root: PathBuf,
        session: String,
        agent: String,
    },
    RestorePoints {
        root: PathBuf,
    },
    /// `point` is a restore point exactly as `restore_points` returned it.
    Rewind {
        root: PathBuf,
        point: RestorePoint,
        #[serde(default)]
        force: bool,
    },
    Status {
        root: PathBuf,
    },
    Verify {
        root: PathBuf,
    },
    /// Stop the daemon after the requests already running finish.
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        daemon_version: String,
        protocol: u32,
    },
    /// `begin_turn` and `before_command`: the snapshot just taken.
    Snapshot {
        snapshot: SnapshotId,
    },
    /// `after_command`.
    Recorded,
    /// `end_turn`: the journal entry's sequence number, or `None` if no turn was open.
    TurnEnded {
        seq: Option<i64>,
    },
    /// Newest first.
    RestorePoints {
        points: Vec<RestorePoint>,
    },
    Rewound(Rewound),
    Status {
        status: Status,
    },
    Verified {
        journal: JournalCheck,
    },
    ShuttingDown,
}

/// The outcome of a rewind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rewound {
    /// The state just before the rewind; rewinding to it undoes the rewind.
    pub safety_snapshot: SnapshotId,
    pub written: usize,
    pub deleted: usize,
    /// Paths left untouched because the snapshot could not protect them.
    pub skipped: Vec<String>,
    pub failed: Vec<FailedPath>,
    /// Commands that ran after the restore point: their effects outside the project are
    /// not undone.
    pub commands_after: Vec<CommandRecord>,
    /// The journal entry recording the rewind.
    pub seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedPath {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JournalCheck {
    Intact {
        entries: usize,
    },
    /// `reason` is one of `unparseable`, `chain_broken`, `hash_mismatch`, `bad_signature`.
    Broken {
        seq: i64,
        reason: String,
    },
}

/// An error answer: `{"kind":"turn_in_progress","session":"…","message":"…"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct ErrorBody {
    #[serde(flatten)]
    pub kind: ErrorKind,
    /// For people: what went wrong and what to do.
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ErrorKind {
    /// The request could not be read: too large, not UTF-8, not JSON, or invalid fields.
    InvalidRequest,
    /// The client speaks another protocol version.
    VersionMismatch,
    TurnInProgress {
        session: String,
    },
    UnknownRestorePoint,
    RootOverlapsDataDir,
    /// The rewind stopped part-way; rewinding to `safety_snapshot` undoes it.
    RewindIncomplete {
        safety_snapshot: String,
    },
    LockTimeout,
    InvalidSessionId,
    Internal,
}

impl ErrorBody {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
