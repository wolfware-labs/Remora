//! Start a daemon process that outlives its starter and holds none of its handles.
//!
//! On Windows, `std::process::Command` always lets the child inherit every inheritable handle
//! of its parent. A hook's stdout is such a handle: a daemon started straight from the hook
//! would keep the agent's pipe open, and the agent would wait for it to close until the daemon
//! exits. So on Windows the daemon is started through PowerShell's `Start-Process`, which
//! creates it without inheriting handles; PowerShell itself exits right after.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Start `exe args…` in `cwd`, detached, with no console window and no inherited handles,
/// taking at most `limit` (on Windows, PowerShell is stopped past it).
pub fn spawn_detached(exe: &Path, args: &[&OsStr], cwd: &Path, limit: Duration) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // PowerShell takes text: an argument that is not valid Unicode cannot reach it intact
        // on Windows anyway.
        let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let script = start_process_script(&exe.to_string_lossy(), &args, &cwd.to_string_lossy());
        let mut powershell = Command::new(powershell())
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        match wait_at_most(&mut powershell, limit)? {
            Some(status) if status.success() => Ok(()),
            Some(status) => Err(io::Error::other(format!("Start-Process failed: {status}"))),
            None => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("Start-Process did not finish within {limit:?}"),
            )),
        }
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::process::CommandExt;
        let _ = limit; // spawning returns at once
        Command::new(exe)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Its own process group: the starter ending must not stop the daemon.
            .process_group(0)
            .spawn()
            .map(drop)
    }
}

/// Wait for `child` to exit, at most `limit`; past it, kill it and return `None`.
#[cfg_attr(not(windows), allow(dead_code))]
fn wait_at_most(child: &mut Child, limit: Duration) -> io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Windows PowerShell by its full path, so a `powershell.exe` earlier on `PATH` is not used.
#[cfg(windows)]
fn powershell() -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
}

/// The PowerShell command starting `exe args…` hidden, in `cwd`.
#[cfg_attr(not(windows), allow(dead_code))]
fn start_process_script(exe: &str, args: &[&str], cwd: &str) -> String {
    let command_line: Vec<String> = args.iter().map(|a| quote_arg(a)).collect();
    format!(
        "Start-Process -FilePath {} -ArgumentList {} -WorkingDirectory {} -WindowStyle Hidden",
        ps_literal(exe),
        ps_literal(&command_line.join(" ")),
        ps_literal(cwd)
    )
}

#[cfg_attr(not(windows), allow(dead_code))]
/// A PowerShell single-quoted string: nothing inside is expanded. PowerShell also reads the
/// curly single quotes as quotes, so each of them is doubled too.
fn ps_literal(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        out.push(c);
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg_attr(not(windows), allow(dead_code))]
/// One argument quoted for a Windows command line, as the C runtime splits it again.
fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Backslashes before a quote are doubled, and the quote escaped.
                out.extend(std::iter::repeat_n('\\', 2 * backslashes + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    // Backslashes before the closing quote are doubled too.
    out.extend(std::iter::repeat_n('\\', 2 * backslashes));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_survive_the_windows_command_line() {
        assert_eq!(quote_arg("run"), "run");
        assert_eq!(quote_arg(r"C:\data"), r"C:\data");
        assert_eq!(quote_arg(r"C:\My Data\"), r#""C:\My Data\\""#);
        assert_eq!(quote_arg(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote_arg(""), r#""""#);
    }

    #[test]
    fn the_start_process_command_quotes_for_powershell() {
        let script = start_process_script(
            r"C:\Users\o'neil\remora.exe",
            &["--data-dir", r"C:\Users\o'neil\AppData\Local\remora", "daemon"],
            r"C:\Users\o'neil\AppData\Local\remora",
        );
        assert_eq!(
            script,
            r"Start-Process -FilePath 'C:\Users\o''neil\remora.exe' -ArgumentList '--data-dir C:\Users\o''neil\AppData\Local\remora daemon' -WorkingDirectory 'C:\Users\o''neil\AppData\Local\remora' -WindowStyle Hidden"
        );
        let spaced = start_process_script(r"C:\Program Files\remorad.exe", &["run", r"C:\a b\"], r"C:\");
        assert!(spaced.contains(r#"-ArgumentList 'run "C:\a b\\"'"#), "{spaced}");
    }

    #[test]
    fn curly_single_quotes_are_doubled_for_powershell() {
        assert_eq!(ps_literal("C:\\o\u{2019}neil"), "'C:\\o\u{2019}\u{2019}neil'");
    }

    #[cfg(unix)]
    #[test]
    fn a_child_past_its_limit_is_stopped() {
        let mut sleeper = Command::new("sleep").arg("30").spawn().unwrap();
        let started = Instant::now();

        let status = wait_at_most(&mut sleeper, Duration::from_millis(200)).unwrap();

        assert_eq!(status, None);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(sleeper.try_wait().unwrap().is_some(), "killed and reaped");
    }

    #[cfg(unix)]
    #[test]
    fn a_child_within_its_limit_reports_how_it_exited() {
        let mut quick = Command::new("true").spawn().unwrap();
        let status = wait_at_most(&mut quick, Duration::from_secs(10)).unwrap();
        assert!(status.is_some_and(|s| s.success()));
    }

    #[cfg(unix)]
    #[test]
    fn arguments_that_are_not_unicode_reach_the_child_intact() {
        use std::os::unix::ffi::OsStrExt;
        let out = tempfile::TempDir::new().unwrap();
        let received = out.path().join("received");
        // Some filesystems (APFS) refuse such names, so the child writes the bytes it got.
        let arg = OsStr::from_bytes(b"data-\xff-dir");
        let args = [
            OsStr::new("-c"),
            OsStr::new("printf %s \"$0\" > \"$1\""),
            arg,
            received.as_os_str(),
        ];

        spawn_detached(Path::new("/bin/sh"), &args, out.path(), Duration::from_secs(5)).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::read(&received).map_or(true, |bytes| bytes.len() < arg.len()) {
            assert!(
                Instant::now() < deadline,
                "the child never wrote {}",
                received.display()
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(std::fs::read(&received).unwrap(), arg.as_bytes());
    }
}
