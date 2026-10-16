//! Validation of the single-component names that relative calls take.

use std::ffi::OsStr;
use std::io;

/// Checks that `name` is one path component that names the same entry on every OS:
/// not empty, not `.` or `..`, and free of separators and NUL. On Windows it also
/// refuses what the Win32 layer cannot open: control characters, `<>:"|?*`, a
/// trailing dot or space, and device names such as `CON` or `NUL.txt`.
pub(crate) fn check_name(name: &OsStr) -> io::Result<()> {
    let bytes = name.as_encoded_bytes();
    let bad_byte = |b: u8| b == b'/' || b == 0 || (cfg!(windows) && is_bad_on_windows(b));
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.iter().any(|&b| bad_byte(b)) {
        return Err(invalid(name));
    }
    if cfg!(windows) && (bytes.ends_with(b".") || bytes.ends_with(b" ") || is_device(bytes)) {
        return Err(invalid(name));
    }
    Ok(())
}

fn is_bad_on_windows(b: u8) -> bool {
    b < 0x20 || b"\\<>:\"|?*".contains(&b)
}

/// `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9` and `LPT1`-`LPT9`, in any case and with
/// any extension.
fn is_device(bytes: &[u8]) -> bool {
    let stem = bytes.split(|&b| b == b'.').next().unwrap_or(bytes);
    let stem = stem.to_ascii_uppercase();
    match stem.as_slice() {
        b"CON" | b"PRN" | b"AUX" | b"NUL" => true,
        [b'C', b'O', b'M', d] | [b'L', b'P', b'T', d] => (b'1'..=b'9').contains(d),
        _ => false,
    }
}

fn invalid(name: &OsStr) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidFilename,
        format!("not a single path component: {}", name.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_names() {
        for name in ["projects", ".remora", "a.b", "blob-01"] {
            check_name(OsStr::new(name)).unwrap();
        }
    }

    #[test]
    fn refuses_names_that_are_not_one_component() {
        for name in ["", ".", "..", "a/b", "/a", "a\0b"] {
            let err = check_name(OsStr::new(name)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidFilename, "{name:?}");
        }
    }

    #[test]
    fn windows_rules_apply_only_on_windows() {
        for name in [
            "a\\b", "a:b", "a*", "trail.", "trail ", "CON", "nul.txt", "com1", "LPT9.log",
        ] {
            assert_eq!(
                check_name(OsStr::new(name)).is_err(),
                cfg!(windows),
                "{name:?}"
            );
        }
        for name in ["CONSOLE", "com0", "lpt10"] {
            check_name(OsStr::new(name)).unwrap();
        }
    }
}
