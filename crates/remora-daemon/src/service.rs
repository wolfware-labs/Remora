//! Starting `remorad` at login, without administrator rights: a launchd agent (macOS), a
//! systemd user unit (Linux), or a logon entry under `HKCU\...\Run` (Windows).
//!
//! The file and command contents come from pure functions, tested on every OS. `install`
//! and `uninstall` apply them to the current user's session.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The launchd agent's label (macOS).
pub const LAUNCHD_LABEL: &str = "dev.wolfware.remora";
/// The systemd user unit (Linux).
pub const SYSTEMD_UNIT: &str = "remorad.service";
/// The per-user logon key and value (Windows).
pub const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
pub const RUN_VALUE: &str = "Remora";

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("cannot find the home directory")]
    NoHome,
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("`{command}` failed: {detail}")]
    Command { command: String, detail: String },
    #[error(
        "systemd user services are not available here (common in WSL and containers); \
         start the daemon yourself with `remorad run`"
    )]
    NoSystemdUserSession,
    #[error("starting remorad at login is not supported on this platform; run `remorad run`")]
    Unsupported,
}

/// The launchd agent: started at login, restarted if it crashes (not after a clean exit).
pub fn launchd_plist(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#,
        xml_escape(&exe.to_string_lossy())
    )
}

/// The systemd user unit: started with the user session, restarted if it crashes.
pub fn systemd_unit(exe: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Remora flight recorder for AI coding agents\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={} run\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        systemd_quote(&exe.to_string_lossy())
    )
}

/// The command stored under the Windows logon key.
pub fn windows_run_command(exe: &Path) -> String {
    format!("\"{}\" run", exe.display())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A double-quoted systemd argument: `\` and `"` escaped, `%` and `$` doubled so systemd
/// expands neither specifiers nor variables.
fn systemd_quote(s: &str) -> String {
    let mut quoted = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => quoted.push_str(r"\\"),
            '"' => quoted.push_str("\\\""),
            '%' => quoted.push_str("%%"),
            '$' => quoted.push_str("$$"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// Run a command; its stderr becomes the error.
#[allow(dead_code)] // unused on platforms without a service manager
fn run(program: &str, args: &[&str]) -> Result<String, ServiceError> {
    let command = format!("{program} {}", args.join(" "));
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| ServiceError::Command {
            command: command.clone(),
            detail: e.to_string(),
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(ServiceError::Command {
            command,
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

#[allow(dead_code)] // unused on platforms without a service manager
fn write_file(path: &Path, contents: &str) -> Result<(), ServiceError> {
    let io_err = |source| ServiceError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    std::fs::write(path, contents).map_err(io_err)
}

#[allow(dead_code)] // unused on platforms without a service manager
fn remove_file(path: &Path) -> Result<(), ServiceError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ServiceError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Register `exe run` to start at login, and start it now. Returns what was done.
pub fn install(exe: &Path) -> Result<String, ServiceError> {
    platform::install(exe)
}

/// Undo [`install`]. Stopping a daemon started some other way is up to the caller.
pub fn uninstall() -> Result<String, ServiceError> {
    platform::uninstall()
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    fn plist_path() -> Result<PathBuf, ServiceError> {
        let home = dirs::home_dir().ok_or(ServiceError::NoHome)?;
        Ok(home
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist")))
    }

    fn gui_domain() -> Result<String, ServiceError> {
        Ok(format!("gui/{}", run("id", &["-u"])?))
    }

    /// Unload the agent if it is loaded, and wait until launchd has let it go: `bootout`
    /// returns before the job is gone, and an immediate `bootstrap` then fails.
    fn unload(domain: &str) -> Result<(), ServiceError> {
        let target = format!("{domain}/{LAUNCHD_LABEL}");
        let loaded = || run("launchctl", &["print", &target]).is_ok();
        if !loaded() {
            return Ok(());
        }
        let bootout = run("launchctl", &["bootout", &target]);
        for _ in 0..50 {
            if !loaded() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err(match bootout {
            Err(e) => e,
            Ok(_) => ServiceError::Command {
                command: format!("launchctl bootout {target}"),
                detail: "the previous agent was still loaded after 5 seconds".to_owned(),
            },
        })
    }

    pub(super) fn install(exe: &Path) -> Result<String, ServiceError> {
        let plist = plist_path()?;
        write_file(&plist, &launchd_plist(exe))?;
        let domain = gui_domain()?;
        // Replace a previous registration, if any.
        unload(&domain)?;
        run("launchctl", &["bootstrap", &domain, &plist.to_string_lossy()])?;
        Ok(format!("installed launchd agent {}", plist.display()))
    }

    pub(super) fn uninstall() -> Result<String, ServiceError> {
        let plist = plist_path()?;
        if !plist.exists() {
            return Ok(format!("no launchd agent installed at {}", plist.display()));
        }
        unload(&gui_domain()?)?;
        remove_file(&plist)?;
        Ok(format!("removed launchd agent {}", plist.display()))
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    fn unit_path() -> Result<PathBuf, ServiceError> {
        let config = dirs::config_dir().ok_or(ServiceError::NoHome)?;
        Ok(config.join("systemd/user").join(SYSTEMD_UNIT))
    }

    pub(super) fn install(exe: &Path) -> Result<String, ServiceError> {
        run("systemctl", &["--user", "show-environment"]).map_err(|_| ServiceError::NoSystemdUserSession)?;
        let unit = unit_path()?;
        write_file(&unit, &systemd_unit(exe))?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", SYSTEMD_UNIT])?;
        // `restart` also starts it, and replaces a daemon from an earlier install.
        run("systemctl", &["--user", "restart", SYSTEMD_UNIT])?;
        Ok(format!("installed systemd user unit {}", unit.display()))
    }

    pub(super) fn uninstall() -> Result<String, ServiceError> {
        let unit = unit_path()?;
        if !unit.exists() {
            return Ok(format!("no systemd user unit installed at {}", unit.display()));
        }
        let _ = run("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT]);
        remove_file(&unit)?;
        let _ = run("systemctl", &["--user", "daemon-reload"]);
        Ok(format!("removed systemd user unit {}", unit.display()))
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    pub(super) fn install(exe: &Path) -> Result<String, ServiceError> {
        let command = windows_run_command(exe);
        run(
            "reg",
            &[
                "add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &command, "/f",
            ],
        )?;
        // Replace a daemon from an earlier install: it would keep the endpoint.
        let data_dir = remora_engine::default_data_dir().map_err(|_| ServiceError::NoHome)?;
        remora_ipc::stop_daemon(&data_dir, std::time::Duration::from_secs(10)).map_err(|e| {
            ServiceError::Command {
                command: "stop the running remorad".to_owned(),
                detail: e.to_string(),
            }
        })?;
        start_detached(exe)?;
        Ok(format!(
            r"added logon entry {RUN_KEY}\{RUN_VALUE} and started remorad"
        ))
    }

    pub(super) fn uninstall() -> Result<String, ServiceError> {
        // Nothing to remove is fine; failing to remove an entry that exists is an error.
        if run("reg", &["query", RUN_KEY, "/v", RUN_VALUE]).is_err() {
            return Ok(format!(r"no logon entry {RUN_KEY}\{RUN_VALUE} to remove"));
        }
        run("reg", &["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])?;
        Ok(format!(r"removed logon entry {RUN_KEY}\{RUN_VALUE}"))
    }

    /// Start `exe run` with no console window, outliving this process.
    fn start_detached(exe: &Path) -> Result<(), ServiceError> {
        crate::spawn_detached(exe, &["run"], &std::env::temp_dir()).map_err(|source| ServiceError::Io {
            path: exe.to_path_buf(),
            source,
        })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod platform {
    use super::*;

    pub(super) fn install(_exe: &Path) -> Result<String, ServiceError> {
        Err(ServiceError::Unsupported)
    }

    pub(super) fn uninstall() -> Result<String, ServiceError> {
        Err(ServiceError::Unsupported)
    }
}
