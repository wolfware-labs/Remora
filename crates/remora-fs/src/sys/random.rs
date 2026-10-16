//! The OS random source: nonces, request ids, `turn_uid`, temporary and staging names
//! (spec 4). A failure is returned, never replaced by a user-space generator.

use std::io;

/// Fills `buf` from the OS random source.
///
/// Linux uses `getrandom(2)` without flags, which blocks only until the kernel pool is
/// initialized at boot. macOS uses `getentropy(3)`. Windows uses `ProcessPrng`.
pub fn fill(buf: &mut [u8]) -> io::Result<()> {
    imp::fill(buf)
}

/// `n` random bytes as `2n` lowercase hex characters.
pub fn hex(n: usize) -> io::Result<String> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = vec![0; n];
    fill(&mut bytes)?;
    let mut out = String::with_capacity(2 * n);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    Ok(out)
}

#[cfg(target_os = "linux")]
mod imp {
    use rustix::io::Errno;
    use rustix::rand::{GetRandomFlags, getrandom};
    use std::io;

    pub(super) fn fill(mut buf: &mut [u8]) -> io::Result<()> {
        // getrandom may return fewer bytes than asked (above 256 bytes, or when a
        // signal arrives), and fails with EINTR if a signal arrives before any byte.
        while !buf.is_empty() {
            match getrandom(&mut *buf, GetRandomFlags::empty()) {
                Ok(n) => buf = &mut buf[n..],
                Err(Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::io;

    /// The most `getentropy` returns in one call.
    const MAX_CALL: usize = 256;

    pub(super) fn fill(buf: &mut [u8]) -> io::Result<()> {
        for chunk in buf.chunks_mut(MAX_CALL) {
            // SAFETY: `chunk` is a valid, writable region of `chunk.len()` bytes, and
            // `chunk.len()` is at most 256, the limit of one call.
            let rc = unsafe { libc::getentropy(chunk.as_mut_ptr().cast(), chunk.len()) };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use windows_sys::Win32::Security::Cryptography::ProcessPrng;

    pub(super) fn fill(buf: &mut [u8]) -> io::Result<()> {
        // SAFETY: `buf` is a valid, writable region of `buf.len()` bytes.
        let ok = unsafe { ProcessPrng(buf.as_mut_ptr(), buf.len()) };
        // Documented to always succeed; checked so a failure can never pass as random.
        if ok == 0 {
            return Err(io::Error::other("ProcessPrng failed"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_accepts_every_size() {
        for n in [0, 1, 8, 32, 256, 257, 1000, 4096] {
            let mut buf = vec![0; n];
            fill(&mut buf).unwrap();
        }
    }

    #[test]
    fn two_draws_differ() {
        // A false failure needs 32 equal bytes: probability 2^-256.
        for n in [32, 256, 4096] {
            let (mut a, mut b) = (vec![0; n], vec![0; n]);
            fill(&mut a).unwrap();
            fill(&mut b).unwrap();
            assert_ne!(a, b, "{n} bytes");
        }
    }

    #[test]
    fn large_draw_is_not_one_value() {
        let mut buf = vec![0; 4096];
        fill(&mut buf).unwrap();
        assert!(buf.iter().any(|&b| b != buf[0]));
    }

    #[test]
    fn hex_has_two_lowercase_digits_per_byte() {
        for n in [0, 8, 16, 32] {
            let s = hex(n).unwrap();
            assert_eq!(s.len(), 2 * n);
            assert!(
                s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')),
                "{s}"
            );
        }
    }
}
