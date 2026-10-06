//! Signed, hash-chained session journal for Remora.
//!
//! Every entry stores the hash of the entry before it, and the entry's own hash is
//! signed with a per-machine ed25519 key. [`Journal::verify`] reports the first entry
//! that was edited, removed from the middle of the chain, reordered, or forged.
//!
//! Limits:
//! - Removing the *newest* entries (or the whole table) is not detectable: the shorter
//!   chain is still valid. Detecting it needs an anchor kept off the machine, such as the
//!   latest hash uploaded by the paid tier.
//! - `verify` trusts the key stored next to the journal. Someone who can replace both
//!   the database and the key can rewrite history; off-machine signed copies close this.

mod chain;
mod error;
mod journal;
mod key;
mod private;
mod record;

pub use chain::{GENESIS_HASH, compute_hash};
pub use error::JournalError;
pub use journal::{BreakReason, Entry, Journal, VerifyReport};
pub use record::{CommandRecord, EntryKind, EntryRecord, FileChange, FileChangeKind};
