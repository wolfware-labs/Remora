use std::fs;
use std::path::Path;

use ed25519_dalek::{Signer, SigningKey};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::{EntryKind, EntryRecord, GENESIS_HASH, JournalError, compute_hash, key};

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
    prev_hash     TEXT    NOT NULL,
    hash          TEXT    NOT NULL,
    signature     TEXT    NOT NULL
);";

const SELECT_ALL: &str = "SELECT seq, session_id, kind, agent, started_at_ms, ended_at_ms, snapshot_id,
    prompt, commands, changes, prev_hash, hash, signature FROM entries ORDER BY seq";

/// A stored journal entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub seq: i64,
    pub record: EntryRecord,
    pub prev_hash: String,
    pub hash: String,
    /// Hex-encoded ed25519 signature over `hash`.
    pub signature: String,
}

pub struct Journal {
    conn: Connection,
    key: SigningKey,
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
            prev_hash: row.get(10)?,
            hash: row.get(11)?,
            signature: row.get(12)?,
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
        })
    }
}

impl Journal {
    /// Open (or create) the journal in `dir`: `journal.db` plus `signing.key`.
    pub fn open(dir: &Path) -> Result<Self, JournalError> {
        fs::create_dir_all(dir).map_err(|source| JournalError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let key = key::load_or_create(&dir.join("signing.key"))?;
        let conn = Connection::open(dir.join("journal.db"))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn, key })
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
        let signature = hex::encode(self.key.sign(hash.as_bytes()).to_bytes());
        tx.execute(
            "INSERT INTO entries (session_id, kind, agent, started_at_ms, ended_at_ms, snapshot_id,
                prompt, commands, changes, prev_hash, hash, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
        let mut stmt = self.conn.prepare(SELECT_ALL)?;
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
}
