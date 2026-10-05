//! Signed, hash-chained session journal for Remora.
//!
//! Every entry stores the hash of the entry before it, and the entry's own hash is
//! signed with a per-machine ed25519 key. Editing, deleting or forging an entry is
//! detected by [`Journal::verify`].
//!
//! Limitation: `verify` trusts the key stored next to the journal. Someone who can
//! replace both the database and the key can rewrite history; the paid tier closes
//! this gap by uploading signed entries off the machine.

mod chain;
mod record;

pub use chain::{GENESIS_HASH, compute_hash};
pub use record::{CommandRecord, EntryKind, EntryRecord, FileChange, FileChangeKind};
