use std::time::{Duration, Instant};

use super::{CliError, Ctx, EXIT_ERROR, service};
use crate::init;

/// `remora init`: install the Claude Code hooks (user-wide) and, unless `no_service`, the
/// login service; then check both.
pub fn init(ctx: &Ctx, no_service: bool) -> Result<(), CliError> {
    let error = |e: init::InitError| CliError::new(EXIT_ERROR, e.to_string());
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|e| CliError::new(EXIT_ERROR, e.to_string()))?;
    let settings = init::settings_path().map_err(error)?;
    let changed = init::update_file(&settings, |value| init::install(value, &exe)).map_err(error)?;
    println!(
        "✓ Claude Code hooks {} in {}",
        if changed { "installed" } else { "already installed" },
        settings.display()
    );
    if no_service {
        println!("· login service skipped (--no-service); hooks start the daemon when needed");
    } else {
        match service::install(ctx) {
            Ok(done) => println!("✓ {done}"),
            Err(e) => println!(
                "✗ login service not installed: {}\n  hooks still start the daemon when needed",
                e.message
            ),
        }
    }
    // The service starts the daemon in the background: give it a moment.
    let deadline = Instant::now() + Duration::from_secs(if no_service { 0 } else { 5 });
    let version = loop {
        match ctx.daemon_version() {
            Some(version) => break Some(version),
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            None => break None,
        }
    };
    match version {
        Some(version) => println!("✓ daemon {version} answering"),
        None => println!("· daemon not running yet; the first Claude Code prompt starts it"),
    }
    println!(
        "Remora now records new Claude Code sessions in git repositories (restart sessions already running). Try `remora status` in one."
    );
    Ok(())
}

/// `remora uninit`: remove Remora's Claude Code hooks.
pub fn uninit() -> Result<(), CliError> {
    let error = |e: init::InitError| CliError::new(EXIT_ERROR, e.to_string());
    let settings = init::settings_path().map_err(error)?;
    if init::update_file(&settings, |value| Ok(init::uninstall(value))).map_err(error)? {
        println!("✓ removed Remora's hooks from {}", settings.display());
    } else {
        println!("· no Remora hooks in {}", settings.display());
    }
    println!("The daemon keeps running; `remora service uninstall` stops it.");
    Ok(())
}
