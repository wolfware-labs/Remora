//! `remora`: record and rewind what AI coding agents do to a git repository.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use remora_cli::commands::{self, CliError, Ctx, EXIT_ERROR};
use remora_cli::hook;

#[derive(Parser)]
#[command(
    name = "remora",
    version,
    about = "Record and rewind what AI coding agents do to your repository"
)]
struct Cli {
    /// Remora's data directory [default: the per-user data directory]
    #[arg(long, env = "REMORA_DATA_DIR", global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Record Claude Code sessions: install its hooks (user-wide) and the login service
    Init {
        /// Do not start the daemon at login (hooks still start it when needed)
        #[arg(long)]
        no_service: bool,
    },
    /// Remove Remora's Claude Code hooks
    Uninit,
    /// Start the daemon at login, or stop doing so
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Handle one hook event (run by the agent, not by people)
    #[command(hide = true)]
    Hook { agent: Agent },
    /// Run the daemon in the foreground (how a hook starts it)
    #[command(hide = true)]
    Daemon,
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Start remorad at login, and now
    Install,
    /// Stop starting remorad at login, and stop it
    Uninstall,
    /// Whether the daemon is running
    Status,
}

#[derive(Clone, Copy, ValueEnum)]
enum Agent {
    ClaudeCode,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // A hook must never fail the agent, not even with bad arguments: exit code 2 would
        // block the tool call.
        Err(e) if std::env::args().any(|arg| arg == "hook") => {
            let _ = e.print();
            return ExitCode::SUCCESS;
        }
        Err(e) => e.exit(),
    };
    // A hook always exits 0, even if it panics.
    if let Command::Hook {
        agent: Agent::ClaudeCode,
    } = cli.command
    {
        let data_dir = cli.data_dir;
        let _ = std::panic::catch_unwind(move || run_hook(data_dir));
        return ExitCode::SUCCESS;
    }
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("remora: {}", e.message);
            ExitCode::from(e.code)
        }
    }
}

fn data_dir(explicit: Option<PathBuf>) -> Result<PathBuf, CliError> {
    match explicit {
        Some(dir) => Ok(dir),
        None => remora_engine::default_data_dir().map_err(|e| CliError::new(EXIT_ERROR, e.to_string())),
    }
}

fn run_hook(explicit: Option<PathBuf>) {
    let Ok(data_dir) = data_dir(explicit) else { return };
    let mut input = Vec::new();
    let _ = std::io::stdin()
        .take(hook::MAX_INPUT_BYTES as u64)
        .read_to_end(&mut input);
    let env = hook::HookEnv {
        data_dir,
        remora: std::env::current_exe().ok(),
        start_wait: Duration::from_secs(2),
    };
    if let Some(output) = hook::run(&input, &env) {
        let mut stdout = std::io::stdout();
        let _ = writeln!(stdout, "{output}");
    }
}

fn run(cli: Cli) -> Result<(), CliError> {
    let ctx = Ctx {
        data_dir: data_dir(cli.data_dir)?,
    };
    match cli.command {
        Command::Init { no_service } => commands::setup::init(&ctx, no_service),
        Command::Uninit => commands::setup::uninit(),
        Command::Service { action } => match action {
            ServiceAction::Install => {
                println!("{}", commands::service::install()?);
                Ok(())
            }
            ServiceAction::Uninstall => commands::service::uninstall(&ctx),
            ServiceAction::Status => commands::service::show(&ctx),
        },
        Command::Hook { .. } => Ok(()),
        Command::Daemon => match remora_daemon::run_in_foreground(&ctx.data_dir) {
            Ok(remora_daemon::Ran::Stopped) => Ok(()),
            Ok(remora_daemon::Ran::AlreadyRunning(dir)) => {
                eprintln!("a daemon is already running for {}", dir.display());
                Ok(())
            }
            Err(e) => Err(CliError::new(EXIT_ERROR, e.to_string())),
        },
    }
}
