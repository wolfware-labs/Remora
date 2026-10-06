use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::SigningKey;

use crate::{JournalError, private};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Load the machine's 32-byte ed25519 seed at `path`, creating it if missing.
///
/// Creation is atomic: the seed is written to an owner-only temp file and hard-linked into
/// place, so a crash never leaves a half-written key and concurrent callers all end up
/// with the same key (a caller that loses the race reads the winner's key). An existing
/// key with loose permissions is tightened to owner-only.
pub fn load_or_create_key(path: &Path) -> Result<SigningKey, JournalError> {
    let io_err = |source| JournalError::Io {
        path: path.to_path_buf(),
        source,
    };
    match read_key(path) {
        Err(JournalError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {}
        other => return other,
    }

    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| JournalError::Random(e.to_string()))?;
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("key");
    let tmp = path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let written = (|| {
        let mut file = private::create_new_file(&tmp)?;
        file.write_all(&seed)?;
        file.sync_all()
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(io_err(e));
    }
    let linked = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(SigningKey::from_bytes(&seed)),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => read_key(path),
        Err(e) => Err(io_err(e)),
    }
}

fn read_key(path: &Path) -> Result<SigningKey, JournalError> {
    let bytes = fs::read(path).map_err(|source| JournalError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let seed: [u8; 32] = bytes
        .try_into()
        .map_err(|_| JournalError::InvalidKey(path.to_path_buf()))?;
    private::tighten_file(path).map_err(|source| JournalError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(SigningKey::from_bytes(&seed))
}
