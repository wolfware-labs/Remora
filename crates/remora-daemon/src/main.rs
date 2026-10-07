//! `remorad`: the Remora daemon.

use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

use remora_daemon::{Daemon, DaemonConfig, DaemonError, install_logger, service};
use remora_engine::default_data_dir;
use remora_ipc::{Client, ClientError, Request};

const USAGE: &str = "usage: remorad <command>

commands:
  run [--data-dir DIR]  run the daemon in the foreground
  install               start remorad at login, and now
  uninstall             stop starting remorad at login, and stop it
  version               print the version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["run"] => run(None),
        ["run", "--data-dir", dir] => run(Some(PathBuf::from(dir))),
        ["install"] => install(),
        ["uninstall"] => uninstall(),
        ["version" | "--version"] => {
            println!("remorad {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("remorad: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(data_dir: Option<PathBuf>) -> Result<(), Box<dyn Error>> {
    let data_dir = match data_dir {
        Some(dir) => dir,
        None => default_data_dir()?,
    };
    install_logger(&data_dir)?;
    let daemon = match Daemon::start(DaemonConfig::new(data_dir)) {
        Ok(daemon) => daemon,
        // Not a failure: the service manager must not restart us in a loop.
        Err(DaemonError::AlreadyRunning(dir)) => {
            eprintln!("remorad is already running for {}", dir.display());
            return Ok(());
        }
        Err(e) => {
            log::error!("cannot start: {e}");
            return Err(e.into());
        }
    };
    let stopper = daemon.clone();
    ctrlc::set_handler(move || stopper.shutdown())?;
    daemon.wait();
    Ok(())
}

fn install() -> Result<(), Box<dyn Error>> {
    let exe = std::env::current_exe()?;
    println!("{}", service::install(&exe)?);
    Ok(())
}

fn uninstall() -> Result<(), Box<dyn Error>> {
    println!("{}", service::uninstall()?);
    // A daemon started by hand (or by the Windows logon entry) keeps running otherwise.
    match Client::connect(&default_data_dir()?) {
        Ok(mut client) => {
            client.call(Request::Shutdown)?;
            println!("stopped the running daemon");
        }
        Err(ClientError::DaemonNotRunning) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
