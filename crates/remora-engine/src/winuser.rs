//! The Windows user's SID, read with `whoami` (Remora uses no `unsafe`, so no Windows API
//! calls). The daemon's named pipe and the blob stores on other drives grant access to it.

use std::path::Path;
use std::process::{Command, Stdio};

/// The SID of the user running this process, from `whoami`; `None` if it cannot be read
/// (always on other systems than Windows).
pub fn user_sid() -> Option<String> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let output = Command::new(Path::new(&root).join(r"System32\whoami.exe"))
        .args(["/user", "/fo", "csv", "/nh"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_whoami_sid(&String::from_utf8_lossy(&output.stdout))
}

/// The SID in `whoami /user /fo csv /nh` output (`"host\user","S-1-5-21-…"`): `S-1-`
/// followed by numbers, so nothing else can reach a security descriptor or a command line.
pub(crate) fn parse_whoami_sid(output: &str) -> Option<String> {
    let field = output.trim().rsplit(',').next()?.trim().trim_matches('"');
    let valid = field.strip_prefix("S-1-").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .split('-')
                .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    });
    valid.then(|| field.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_sid_is_read_from_whoami() {
        let out = "\"desktop-7q2\\o'neil\",\"S-1-5-21-3623811015-3361044348-30300820-1013\"\r\n";
        assert_eq!(
            parse_whoami_sid(out).as_deref(),
            Some("S-1-5-21-3623811015-3361044348-30300820-1013")
        );
        for bad in [
            "",
            "\"host\\me\"",
            "\"host\\me\",\"S-1-\"",
            "\"me\",\"S-1-5-x\"",
            "\"me\",\"S-1-5-\"",
            "\"me\",\"S-1-5-21)(A;;GA;;;WD\"",
        ] {
            assert_eq!(parse_whoami_sid(bad), None, "{bad}");
        }
    }
}
