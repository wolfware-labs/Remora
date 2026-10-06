//! Signed, hash-chained session journal for Remora.
//!
//! Every entry stores the hash of the entry before it, and the entry's own hash is
//! signed with the machine's ed25519 key (see [`load_or_create_key`]).
//! [`Journal::verify`] and [`ReadOnlyJournal::verify_with`] report the first entry that
//! was edited, removed from the middle of the chain, reordered, or forged.
//!
//! Limits:
//! - Removing the *newest* entries (or the whole table) is not detectable: the shorter
//!   chain is still valid. Detecting it needs an anchor kept off the machine, such as the
//!   latest hash uploaded by the paid tier.
//! - Verification is only as trustworthy as the key it checks against. Someone who can
//!   replace both the database and the machine key can rewrite history; checking against
//!   a public key pinned elsewhere ([`ReadOnlyJournal::verify_with`]) closes this.

mod chain;
mod error;
mod journal;
mod key;
mod private;
mod record;

pub use chain::{GENESIS_HASH, compute_hash};
pub use ed25519_dalek::{SigningKey, VerifyingKey};
pub use error::JournalError;
pub use journal::{BreakReason, Entry, FORMAT_VERSION, Journal, ReadOnlyJournal, VerifyReport};
pub use key::load_or_create_key;
pub use record::{CommandRecord, EntryKind, EntryRecord, FileChange, FileChangeKind};
