//! The process umask (Unix).

use rustix::fs::{Mode, RawMode};
use rustix::process::umask;

/// Sets the process umask and restores the previous value on drop.
///
/// The umask is process-wide: callers hold the creation lock of
/// `remora_fs::private` for the whole lifetime of the value.
pub(crate) struct ScopedUmask {
    previous: Mode,
}

impl ScopedUmask {
    /// Sets the umask to the permission bits of `mask`.
    pub(crate) fn set(mask: u32) -> Self {
        let bits = RawMode::try_from(mask & 0o777).unwrap_or(0o777);
        Self {
            previous: umask(Mode::from_bits_truncate(bits)),
        }
    }

    /// The umask that was in effect before `set`.
    #[cfg(test)]
    #[allow(clippy::useless_conversion, reason = "the mode is u16 on macOS")]
    pub(crate) fn previous(&self) -> u32 {
        u32::from(self.previous.bits())
    }
}

impl Drop for ScopedUmask {
    fn drop(&mut self) {
        umask(self.previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_the_previous_mask() {
        let _lock = crate::private::creation_lock();
        let outer = ScopedUmask::set(0o022);
        {
            let inner = ScopedUmask::set(0o177);
            assert_eq!(inner.previous(), 0o022);
        }
        let probe = ScopedUmask::set(0o077);
        assert_eq!(probe.previous(), 0o022);
        drop(probe);
        drop(outer);
    }
}
