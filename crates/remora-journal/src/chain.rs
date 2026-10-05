use sha2::{Digest, Sha256};

use crate::EntryRecord;

/// `prev_hash` of the first entry.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// SHA-256 over `prev_hash`, a newline, and the record's canonical JSON. Lowercase hex.
pub fn compute_hash(prev_hash: &str, record: &EntryRecord) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(b"\n");
    hasher.update(serde_json::to_vec(record).expect("records always serialize"));
    hex::encode(hasher.finalize())
}
