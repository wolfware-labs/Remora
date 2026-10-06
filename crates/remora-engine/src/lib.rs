//! The Remora engine: turns agent events into snapshots, signed journal entries and rewinds.
//!
//! It has no networking and no clock. Callers pass timestamps (`at_ms`), so every
//! behaviour is deterministic; the daemon is the only place that reads the time.
//!
//! Data layout under the data directory (everything owner-only):
//! ```text
//! signing.key                      the machine's ed25519 key
//! projects/<id>/project.json       { "root": "<canonical project root>" }
//! projects/<id>/store/             snapshot store
//! projects/<id>/journal.db         signed, hash-chained journal
//! projects/<id>/sessions/<id>.json open turns (survive restarts)
//! projects/<id>/lock               serializes processes working on the project
//! ```

mod engine;
mod error;
mod fsutil;
mod lock;
mod project;
mod session;

pub use engine::{Engine, default_data_dir, project_id};
pub use error::EngineError;
pub use project::Project;
pub use session::SessionRef;
