use std::path::PathBuf;

use remora_engine::{RestorePoint, Status};
use remora_journal::CommandRecord;
use remora_snap::SnapshotId;
use serde::{Deserialize, Serialize};

/// The protocol version. A daemon refuses envelopes and `hello`s with another version.
pub const PROTOCOL_VERSION: u32 = 1;

/// The largest message, newline excluded, either side accepts.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Restore points per `restore_points` answer when the request sets no `limit`.
pub const DEFAULT_POINTS: usize = 50;
/// The most restore points one answer carries, whatever the request's `limit`.
pub const MAX_POINTS: usize = 100;
/// The most commands a `rewound` answer lists; the rest are only counted.
pub const MAX_COMMANDS_AFTER: usize = 100;
/// The most paths each list of a `preview` answer carries; the rest are only counted.
pub const MAX_PREVIEW_PATHS: usize = 200;

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
    /// The first message on a connection: versions, and the client's challenge (`nonce`) the
    /// daemon must answer to prove it knows the secret (see [`crate::auth`]).
    Hello {
        client_version: String,
        protocol: u32,
        nonce: String,
    },
    /// The client's answer to the daemon's challenge. Until it is accepted, every other
    /// request is refused with `unauthenticated`.
    Prove {
        proof: String,
    },
    BeginTurn {
        root: PathBuf,
        session: String,
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    /// `tool_use_id`, when the agent has one, pairs the command with its `after_command`.
    /// `deadline_ms` (milliseconds since the epoch) is when the agent stops waiting: a
    /// checkpoint finished later is marked `late`.
    BeforeCommand {
        root: PathBuf,
        session: String,
        agent: String,
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deadline_ms: Option<i64>,
    },
    AfterCommand {
        root: PathBuf,
        session: String,
        agent: String,
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    EndTurn {
        root: PathBuf,
        session: String,
        agent: String,
    },
    /// The agent's session ended: its open turn, if any, is closed as interrupted.
    EndSession {
        root: PathBuf,
        session: String,
        agent: String,
    },
    /// Newest first: skip the `skip` newest points, then return up to `limit` (default
    /// [`DEFAULT_POINTS`], at most [`MAX_POINTS`]).
    RestorePoints {
        root: PathBuf,
        #[serde(default)]
        skip: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    /// `point` is a restore point exactly as `restore_points` returned it.
    Rewind {
        root: PathBuf,
        point: RestorePoint,
        #[serde(default)]
        force: bool,
    },
    /// What rewinding to `point` would change. Changes nothing.
    Preview {
        root: PathBuf,
        point: RestorePoint,
    },
    /// Restore a snapshot by id, such as the `safety_snapshot` of a `rewind_incomplete` error.
    RestoreSnapshot {
        root: PathBuf,
        snapshot: SnapshotId,
        #[serde(default)]
        force: bool,
    },
    Status {
        root: PathBuf,
    },
    Verify {
        root: PathBuf,
    },
    /// Stop the daemon after the requests already running finish. Every daemon version
    /// understands `{"v":1,"id":0,"req":{"type":"shutdown"}}`, so an upgrade can always stop
    /// the daemon it replaces.
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// The daemon's versions, its answer to the client's challenge (`proof`) and its own
    /// challenge (`nonce`).
    Hello {
        daemon_version: String,
        protocol: u32,
        nonce: String,
        proof: String,
    },
    /// The client's proof was accepted.
    Authenticated,
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
    /// One page, newest first; `total` counts every point of the project.
    RestorePoints {
        points: Vec<RestorePoint>,
        total: usize,
    },
    Rewound(Rewound),
    Preview(Preview),
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
    /// Commands that ran after the restore point, oldest first: their effects outside the
    /// project are not undone. At most [`MAX_COMMANDS_AFTER`], each cut like a label.
    pub commands_after: Vec<CommandRecord>,
    /// Commands after the point that are not listed.
    pub commands_omitted: usize,
    /// The journal entry recording the rewind.
    pub seq: i64,
}

/// What a rewind would change. Each list holds at most [`MAX_PREVIEW_PATHS`] paths; the
/// `*_total` counts include the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    /// Absent now: brought back.
    pub restore: Vec<String>,
    pub restore_total: usize,
    /// Created after the point: removed.
    pub delete: Vec<String>,
    pub delete_total: usize,
    /// Changed since the point: reverted.
    pub revert: Vec<String>,
    pub revert_total: usize,
    /// Commands after the point, oldest first, as in [`Rewound`].
    pub commands_after: Vec<CommandRecord>,
    pub commands_omitted: usize,
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
    /// The connection has not proved it knows the secret.
    Unauthenticated,
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
