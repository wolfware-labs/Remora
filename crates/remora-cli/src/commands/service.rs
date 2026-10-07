use std::path::PathBuf;
use std::time::Duration;

use remora_daemon::service;

use super::{CliError, Ctx, EXIT_ERROR};

/// `remorad`, next to this `remora` or on `PATH`.
pub fn find_remorad() -> Option<PathBuf> {
    let name = format!("remorad{}", std::env::consts::EXE_SUFFIX);
    let sibling = std::env::current_exe().ok()?.with_file_name(&name);
    if sibling.is_file() {
        return Some(sibling);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&name))
        .find(|candidate| candidate.is_file())
}

/// `remora service install`: start `remorad` at login, and now.
pub fn install(ctx: &Ctx) -> Result<String, CliError> {
    let remorad = find_remorad().ok_or_else(|| {
        CliError::new(
            EXIT_ERROR,
            "remorad was not found next to remora or on PATH; install it alongside remora",
        )
    })?;
    // A daemon a hook started (or an older version) holds the instance lock: the service's
    // own daemon would exit with "already running". Stop it first, on every OS.
    remora_ipc::stop_daemon(&ctx.data_dir, Duration::from_secs(10))?;
    service::install(&remorad).map_err(|e| CliError::new(EXIT_ERROR, e.to_string()))
}

/// `remora service uninstall`: stop starting `remorad` at login, and stop it.
pub fn uninstall(ctx: &Ctx) -> Result<(), CliError> {
    println!(
        "{}",
        service::uninstall().map_err(|e| CliError::new(EXIT_ERROR, e.to_string()))?
    );
    if remora_ipc::stop_daemon(&ctx.data_dir, Duration::from_secs(10))? {
        println!("stopped the running daemon");
    }
    Ok(())
}

/// `remora service status`: is a daemon answering?
pub fn show(ctx: &Ctx) -> Result<(), CliError> {
    match ctx.daemon_version() {
        Some(version) => println!("daemon {version} running for {}", ctx.data_dir.display()),
        None => println!("daemon not running for {}", ctx.data_dir.display()),
    }
    Ok(())
}
