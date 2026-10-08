//! Mutual authentication between clients and the daemon.
//!
//! The daemon writes a fresh random secret to `<data_dir>/remorad.secret` (owner-only) every
//! time it starts. Each side proves it knows the secret with HMAC-SHA256 over a random nonce
//! the other side chose, so the secret never crosses the connection: a squatted endpoint
//! cannot answer the client's challenge, and a client that cannot read the data directory
//! cannot answer the daemon's.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// The secret's file in the data directory.
pub const SECRET_FILE: &str = "remorad.secret";

const SERVER_DOMAIN: &[u8] = b"remora-server\n";
const CLIENT_DOMAIN: &[u8] = b"remora-client\n";

/// The shared secret. Its `Debug` output never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret([u8; 32]);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// A random challenge, sent as hex.
pub fn nonce() -> io::Result<String> {
    Ok(hex::encode(random_bytes()?))
}

fn random_bytes() -> io::Result<[u8; 32]> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(bytes)
}

impl Secret {
    pub fn generate() -> io::Result<Self> {
        random_bytes().map(Self)
    }

    /// Read the secret the running daemon wrote.
    pub fn load(data_dir: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(data_dir.join(SECRET_FILE))?;
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(text.trim(), &mut bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Self(bytes))
    }

    /// Write the secret, owner-only from creation, replacing the previous one atomically.
    pub fn store(&self, data_dir: &Path) -> io::Result<()> {
        let path = data_dir.join(SECRET_FILE);
        let tmp = data_dir.join(format!(".{SECRET_FILE}.{}.tmp", std::process::id()));
        let _ = fs::remove_file(&tmp);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = options.open(&tmp)?;
            file.write_all(hex::encode(self.0).as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, &path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    fn mac(&self, domain: &[u8], nonce: &str) -> Hmac<Sha256> {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.0).expect("HMAC takes any key length");
        mac.update(domain);
        mac.update(nonce.as_bytes());
        mac
    }

    /// The daemon's answer to the client's challenge.
    pub fn server_proof(&self, client_nonce: &str) -> String {
        hex::encode(self.mac(SERVER_DOMAIN, client_nonce).finalize().into_bytes())
    }

    /// The client's answer to the daemon's challenge.
    pub fn client_proof(&self, server_nonce: &str) -> String {
        hex::encode(self.mac(CLIENT_DOMAIN, server_nonce).finalize().into_bytes())
    }

    /// Whether `proof` answers `client_nonce` with this secret (constant time).
    pub fn verify_server(&self, client_nonce: &str, proof: &str) -> bool {
        verify(self.mac(SERVER_DOMAIN, client_nonce), proof)
    }

    /// Whether `proof` answers `server_nonce` with this secret (constant time).
    pub fn verify_client(&self, server_nonce: &str, proof: &str) -> bool {
        verify(self.mac(CLIENT_DOMAIN, server_nonce), proof)
    }
}

fn verify(mac: Hmac<Sha256>, proof: &str) -> bool {
    hex::decode(proof).is_ok_and(|bytes| mac.verify_slice(&bytes).is_ok())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn proofs_verify_only_with_the_same_secret_and_nonce() {
        let secret = Secret::generate().unwrap();
        let other = Secret::generate().unwrap();
        let n = nonce().unwrap();

        assert!(secret.verify_server(&n, &secret.server_proof(&n)));
        assert!(!secret.verify_server(&n, &other.server_proof(&n)));
        assert!(!secret.verify_server(&nonce().unwrap(), &secret.server_proof(&n)));
        assert!(
            !secret.verify_client(&n, &secret.server_proof(&n)),
            "the two directions differ"
        );
        assert!(!secret.verify_server(&n, "not hex"));
    }

    #[test]
    fn the_secret_round_trips_through_its_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let secret = Secret::generate().unwrap();

        secret.store(dir.path()).unwrap();

        assert_eq!(Secret::load(dir.path()).unwrap(), secret);
        assert_eq!(format!("{secret:?}"), "Secret(..)");
    }

    #[cfg(unix)]
    #[test]
    fn the_secret_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        Secret::generate().unwrap().store(dir.path()).unwrap();

        let mode = fs::metadata(dir.path().join(SECRET_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
