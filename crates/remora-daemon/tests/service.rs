#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::path::Path;

use remora_daemon::service::{launchd_plist, systemd_unit, windows_run_command};

#[test]
fn the_launchd_agent_runs_the_daemon_at_login_and_after_crashes() {
    let plist = launchd_plist(Path::new("/Users/me/.cargo/bin/remorad"));
    assert_eq!(
        plist,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>dev.wolfware.remora</string>
    <key>ProgramArguments</key>
    <array>
        <string>/Users/me/.cargo/bin/remorad</string>
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
"#
    );
}

#[test]
fn the_plist_escapes_xml_in_the_path() {
    let plist = launchd_plist(Path::new("/Users/R&D <team>/remorad"));
    assert!(plist.contains("<string>/Users/R&amp;D &lt;team&gt;/remorad</string>"));
}

#[test]
fn the_systemd_unit_runs_the_daemon_and_restarts_it_on_failure() {
    let unit = systemd_unit(Path::new("/home/me/.cargo/bin/remorad"));
    assert_eq!(
        unit,
        "[Unit]
Description=Remora flight recorder for AI coding agents

[Service]
Type=simple
ExecStart=\"/home/me/.cargo/bin/remorad\" run
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
"
    );
}

#[test]
fn the_systemd_unit_quotes_spaces_specifiers_and_variables() {
    let unit = systemd_unit(Path::new("/opt/my tools/100%/$HOME/re\"mo\\rad"));
    assert!(
        unit.contains(r#"ExecStart="/opt/my tools/100%%/$$HOME/re\"mo\\rad" run"#),
        "{unit}"
    );
}

#[test]
fn the_windows_logon_entry_quotes_the_executable() {
    let command = windows_run_command(Path::new(
        r"C:\Users\Me\AppData\Local\Programs\Remora\remorad.exe",
    ));
    assert_eq!(
        command,
        r#""C:\Users\Me\AppData\Local\Programs\Remora\remorad.exe" run"#
    );
}

/// Installs for real into the current user's session; opt in with `REMORA_TEST_SERVICE=1`.
#[test]
fn install_and_uninstall_for_real() {
    if std::env::var_os("REMORA_TEST_SERVICE").is_none() {
        return;
    }
    let exe = Path::new(env!("CARGO_BIN_EXE_remorad"));
    println!("{}", remora_daemon::service::install(exe).unwrap());
    println!("{}", remora_daemon::service::uninstall().unwrap());
}
