use std::fs;
use std::io::{self, Write};
use std::path::Path;

use ed25519_dalek::SigningKey;

use crate::JournalError;

/// Load the 32-byte ed25519 seed at `path`, creating it (owner-only permissions) if missing.
pub(crate) fn load_or_create(path: &Path) -> Result<SigningKey, JournalError> {
    let io_err = |source| JournalError::Io {
        path: path.to_path_buf(),
        source,
    };
    match fs::read(path) {
        Ok(bytes) => {
            let seed: [u8; 32] = bytes
                .try_into()
                .map_err(|_| JournalError::InvalidKey(path.to_path_buf()))?;
            Ok(SigningKey::from_bytes(&seed))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).map_err(|e| JournalError::Random(e.to_string()))?;
            let mut file = private_file(path).map_err(io_err)?;
            file.write_all(&seed).map_err(io_err)?;
            file.sync_all().map_err(io_err)?;
            Ok(SigningKey::from_bytes(&seed))
        }
        Err(e) => Err(io_err(e)),
    }
}

#[cfg(unix)]
fn private_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().write(true).create_new(true).open(path)
}
