use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params};

use crate::{EntryKind, EntryRecord, GENESIS_HASH, JournalError, compute_hash, private};

/// The on-disk format version, stored in SQLite's `user_version`.
pub const FORMAT_VERSION: i64 = 1;

/// Signatures cover this prefix followed by the entry hash, so a signature made for
/// another purpose (or another format version) can never verify as a journal entry.
const SIGNING_DOMAIN: &[u8] = b"remora-journal-v1\n";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS entries (
    seq           INTEGER PRIMARY KEY,
    session_id    TEXT    NOT NULL,
    kind          TEXT    NOT NULL,
    agent         TEXT    NOT NULL,
    started_at_ms INTEGER NOT NULL,
    ended_at_ms   INTEGER NOT NULL,
    snapshot_id   TEXT,
    prompt        TEXT,
    commands      TEXT    NOT NULL,
    changes       TEXT    NOT NULL,
    interrupted   INTEGER,
    rewind_target TEXT,
    incomplete    INTEGER,
    prev_hash     TEXT    NOT NULL,
    hash          TEXT    NOT NULL,
    signature     TEXT    NOT NULL
);";

const SELECT_ALL: &str = "SELECT seq, session_id, kind, agent, started_at_ms, ended_at_ms, snapshot_id,
    prompt, commands, changes, interrupted, rewind_target, incomplete, prev_hash, hash, signature
    FROM entries ORDER BY seq";

/// A stored journal entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub seq: i64,
    pub record: EntryRecord,
    pub prev_hash: String,
    pub hash: String,
    /// Hex-encoded ed25519 signature over the signing domain plus `hash`.
    pub signature: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakReason {
    /// A column could not be decoded back into a record.
    Unparseable,
    /// `prev_hash` does not match the previous entry's hash (entry deleted or reordered).
    ChainBroken,
    /// The content does not match the stored hash (entry edited).
    HashMismatch,
    /// The signature does not match the hash (hash forged without the key).
    BadSignature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyReport {
    Intact {
        entries: usize,
    },
    /// The first entry that fails verification.
    Broken {
        seq: i64,
        reason: BreakReason,
    },
}

/// A journal open for appending. Holds the machine's signing key.
pub struct Journal {
    conn: Connection,
    key: SigningKey,
}

/// A journal opened read-only, for inspection and verification without the private key.
pub struct ReadOnlyJournal {
    conn: Connection,
}

/// One row as stored, before decoding.
struct RawRow {
    seq: i64,
    session_id: String,
    kind: String,
    agent: String,
    started_at_ms: i64,
    ended_at_ms: i64,
    snapshot_id: Option<String>,
    prompt: Option<String>,
    commands: String,
    changes: String,
    interrupted: Option<bool>,
    rewind_target: Option<String>,
    incomplete: Option<bool>,
    prev_hash: String,
    hash: String,
    signature: String,
}

impl RawRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            seq: row.get(0)?,
            session_id: row.get(1)?,
            kind: row.get(2)?,
            agent: row.get(3)?,
            started_at_ms: row.get(4)?,
            ended_at_ms: row.get(5)?,
            snapshot_id: row.get(6)?,
            prompt: row.get(7)?,
            commands: row.get(8)?,
            changes: row.get(9)?,
            interrupted: row.get(10)?,
            rewind_target: row.get(11)?,
            incomplete: row.get(12)?,
            prev_hash: row.get(13)?,
            hash: row.get(14)?,
            signature: row.get(15)?,
        })
    }

    fn record(&self) -> Result<EntryRecord, String> {
        Ok(EntryRecord {
            session_id: self.session_id.clone(),
            kind: EntryKind::parse(&self.kind).ok_or_else(|| format!("unknown kind {:?}", self.kind))?,
            agent: self.agent.clone(),
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
            snapshot_id: self.snapshot_id.clone(),
            prompt: self.prompt.clone(),
            commands: serde_json::from_str(&self.commands).map_err(|e| e.to_string())?,
            changes: serde_json::from_str(&self.changes).map_err(|e| e.to_string())?,
            interrupted: self.interrupted,
            rewind_target: self.rewind_target.clone(),
            incomplete: self.incomplete,
        })
    }
}

fn signing_message(hash: &str) -> Vec<u8> {
    [SIGNING_DOMAIN, hash.as_bytes()].concat()
}

fn check_version(conn: &Connection) -> Result<(), JournalError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == FORMAT_VERSION {
        Ok(())
    } else {
        Err(JournalError::UnsupportedVersion(version))
    }
}

impl Journal {
    /// Open (or create) the journal database at `path`, signing new entries with `key`.
    ///
    /// The file is created owner-only; an existing file with loose permissions is tightened.
    /// A database written by another format version is refused.
    pub fn open(path: &Path, key: SigningKey) -> Result<Self, JournalError> {
        let io_err = |source| JournalError::Io {
            path: path.to_path_buf(),
            source,
        };
        private::ensure_file(path).map_err(io_err)?;
        private::tighten_file(path).map_err(io_err)?;
        let mut conn = Connection::open(path)?;
        // Create the table and stamp the version in one IMMEDIATE transaction: a crash (or a
        // second process opening the same new journal) can never see a table without a version.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let has_table: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'entries')",
            [],
            |r| r.get(0),
        )?;
        if version == 0 && !has_table {
            tx.execute_batch(SCHEMA)?;
            tx.execute_batch(&format!("PRAGMA user_version = {FORMAT_VERSION}"))?;
        }
        tx.commit()?;
        check_version(&conn)?;
        Ok(Self { conn, key })
    }

    /// Open an existing journal read-only. Never creates or modifies anything.
    pub fn open_read_only(path: &Path) -> Result<ReadOnlyJournal, JournalError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags)?;
        check_version(&conn)?;
        Ok(ReadOnlyJournal { conn })
    }

    /// Hex-encoded public key that verifies this journal's signatures.
    pub fn verifying_key_hex(&self) -> String {
        hex::encode(self.key.verifying_key().to_bytes())
    }

    /// Append a record, chaining it to the last entry and signing it.
    pub fn append(&mut self, record: EntryRecord) -> Result<Entry, JournalError> {
        // IMMEDIATE takes the write lock up front, so two writers can't chain to the same entry.
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prev_hash: String = tx
            .query_row("SELECT hash FROM entries ORDER BY seq DESC LIMIT 1", [], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or_else(|| GENESIS_HASH.to_owned());
        let hash = compute_hash(&prev_hash, &record);
        let signature = hex::encode(self.key.sign(&signing_message(&hash)).to_bytes());
        tx.execute(
            "INSERT INTO entries (session_id, kind, agent, started_at_ms, ended_at_ms, snapshot_id,
                prompt, commands, changes, interrupted, rewind_target, incomplete, prev_hash, hash, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                record.session_id,
                record.kind.as_str(),
                record.agent,
                record.started_at_ms,
                record.ended_at_ms,
                record.snapshot_id,
                record.prompt,
                serde_json::to_string(&record.commands).expect("commands always serialize"),
                serde_json::to_string(&record.changes).expect("changes always serialize"),
                record.interrupted,
                record.rewind_target,
                record.incomplete,
                prev_hash,
                hash,
                signature,
            ],
        )?;
        let seq = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Entry {
            seq,
            record,
            prev_hash,
            hash,
            signature,
        })
    }

    /// All entries in order.
    pub fn entries(&self) -> Result<Vec<Entry>, JournalError> {
        read_entries(&self.conn)
    }

    /// Verify the chain with this journal's own key. See [`ReadOnlyJournal::verify_with`].
    pub fn verify(&self) -> Result<VerifyReport, JournalError> {
        verify_chain(&self.conn, &self.key.verifying_key())
    }
}

impl ReadOnlyJournal {
    /// All entries in order.
    pub fn entries(&self) -> Result<Vec<Entry>, JournalError> {
        read_entries(&self.conn)
    }

    /// Walk the chain from the start and report the first broken entry, if any, checking
    /// signatures against `key`. Never modifies the journal: a broken chain is evidence.
    pub fn verify_with(&self, key: &VerifyingKey) -> Result<VerifyReport, JournalError> {
        verify_chain(&self.conn, key)
    }
}

fn read_entries(conn: &Connection) -> Result<Vec<Entry>, JournalError> {
    let mut stmt = conn.prepare(SELECT_ALL)?;
    let rows = stmt.query_map([], RawRow::from_row)?;
    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        let record = row
            .record()
            .map_err(|detail| JournalError::Corrupt { seq: row.seq, detail })?;
        out.push(Entry {
            seq: row.seq,
            record,
            prev_hash: row.prev_hash,
            hash: row.hash,
            signature: row.signature,
        });
    }
    Ok(out)
}

fn verify_chain(conn: &Connection, verifying_key: &VerifyingKey) -> Result<VerifyReport, JournalError> {
    let mut stmt = conn.prepare(SELECT_ALL)?;
    // `seq` is the rowid, so it is always an integer; the other columns may hold any SQLite type.
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, RawRow::from_row(r))))?;
    let mut expected_prev = GENESIS_HASH.to_owned();
    let mut count = 0;

    for row in rows {
        let (seq, parsed) = row?;
        // A column holding the wrong type (say text in an integer column) is tampering too.
        let Ok(row) = parsed else {
            return Ok(VerifyReport::Broken {
                seq,
                reason: BreakReason::Unparseable,
            });
        };
        let broken = |reason| Ok(VerifyReport::Broken { seq: row.seq, reason });
        let Ok(record) = row.record() else {
            return broken(BreakReason::Unparseable);
        };
        if row.prev_hash != expected_prev {
            return broken(BreakReason::ChainBroken);
        }
        if compute_hash(&row.prev_hash, &record) != row.hash {
            return broken(BreakReason::HashMismatch);
        }
        let signature_ok = hex::decode(&row.signature)
            .ok()
            .and_then(|bytes| <[u8; 64]>::try_from(bytes).ok())
            .is_some_and(|bytes| {
                verifying_key
                    .verify_strict(&signing_message(&row.hash), &Signature::from_bytes(&bytes))
                    .is_ok()
            });
        if !signature_ok {
            return broken(BreakReason::BadSignature);
        }
        expected_prev = row.hash;
        count += 1;
    }
    Ok(VerifyReport::Intact { entries: count })
}
