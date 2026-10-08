//! Start a daemon process that outlives its starter and holds none of its handles.
//!
//! On Windows, `std::process::Command` always lets the child inherit every inheritable handle
//! of its parent. A hook's stdout is such a handle: a daemon started straight from the hook
//! would keep the agent's pipe open, and the agent would wait for it to close until the daemon
//! exits. So on Windows the daemon is started through PowerShell's `Start-Process`, which
//! creates it without inheriting handles; PowerShell itself exits right after.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

/// Start `exe args…` in `cwd`, detached, with no console window and no inherited handles.
pub fn spawn_detached(exe: &Path, args: &[&str], cwd: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let script = start_process_script(&exe.to_string_lossy(), args, &cwd.to_string_lossy());
        let status = Command::new(powershell())
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("Start-Process failed: {status}")))
        }
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::process::CommandExt;
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
/// A PowerShell single-quoted string: nothing inside is expanded.
fn ps_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
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
}
