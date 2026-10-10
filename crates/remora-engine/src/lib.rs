//! The Remora engine: turns agent events into snapshots, signed journal entries and rewinds.
//!
//! It has no networking and no clock. Callers pass timestamps (`at_ms`), so every
//! behaviour is deterministic; the daemon is the only place that reads the time.
//!
//! Data layout under the data directory (everything owner-only):
//! ```text
//! signing.key                      the machine's ed25519 key
//! projects/<id>/project.json       { "root": "<canonical project root>", "blobs": "<folder>"? }
//! projects/<id>/store/             snapshot store (its blobs, unless "blobs" names another folder)
//! projects/<id>/journal.db         signed, hash-chained journal
//! projects/<id>/sessions/<id>.json open turns (survive restarts)
//! projects/<id>/rewind.json        the rewind in progress, if any (see `Project::open`)
//! projects/<id>/pruned.json        snapshots removed by retention, and when
//! projects/<id>/lock               serializes processes working on the project
//! blob-home.lock                   serializes choosing the blob folder of new projects
//! ```
//!
//! A project on another volume than the data directory keeps its blobs on its own volume,
//! in `<anchor>/.remora-store/<id>/blobs` (Unix) or `<drive>:\.remora-store-<user SID>\<id>\blobs`
//! (Windows), see `blob_home`, so its files can be cloned.

mod blob_home;
mod engine;
mod error;
mod fsutil;
mod lock;
mod project;
mod retention;
mod session;
mod winuser;

pub use engine::{Engine, Forgotten, default_data_dir, project_id};
pub use error::EngineError;
pub use project::{
    MAX_LABEL_BYTES, PointKind, Preview, Project, REMORA_AGENT, REWIND_FILE, REWIND_SESSION,
    ROLLED_BACK_LABEL, ROLLED_BACK_NOTICE, RestorePoint, RewindReport, Status, short_label,
};
pub use retention::{PRUNED_FILE, PruneReason, PruneReport, PrunedSnapshot, Retention, utc_date};
pub use session::SessionRef;
pub use winuser::user_sid;
