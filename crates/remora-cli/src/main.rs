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

#[derive(clap::Args)]
struct Project {
    /// The project folder [default: the git repository around the current directory]
    #[arg(long)]
    root: Option<PathBuf>,
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
    /// What is recorded for this project, and whether the daemon runs
    Status {
        #[command(flatten)]
        project: Project,
        #[arg(long)]
        json: bool,
    },
    /// Restore points, #1 newest
    Log {
        #[command(flatten)]
        project: Project,
        /// How many points to list
        #[arg(short = 'n', long, default_value_t = 20)]
        count: usize,
        /// List every point
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Rewind the project to a restore point (shows what would change first)
    Rewind {
        #[command(flatten)]
        project: Project,
        /// The restore point, as `remora log` numbers it (#3 or 3)
        #[arg(value_parser = commands::rewind::parse_number, required_unless_present = "snapshot")]
        point: Option<usize>,
        /// Restore a snapshot by id instead, such as the one a rewind prints to undo it
        #[arg(long, conflicts_with = "point")]
        snapshot: Option<String>,
        /// Do not ask for confirmation
        #[arg(long, short = 'y')]
        yes: bool,
        /// Rewind even though an agent is still working in the project
        #[arg(long)]
        force: bool,
    },
    /// Check the signed journal for tampering
    Verify {
        #[command(flatten)]
        project: Project,
        #[arg(long)]
        json: bool,
    },
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
    let root = |project: &Project| commands::project_root(project.root.as_deref());
    match cli.command {
        Command::Init { no_service } => commands::setup::init(&ctx, no_service),
        Command::Uninit => commands::setup::uninit(),
        Command::Status { project, json } => commands::status::run(&ctx, &root(&project)?, json),
        Command::Log {
            project,
            count,
            all,
            json,
        } => commands::log::run(&ctx, &root(&project)?, count, all, json),
        Command::Rewind {
            project,
            point,
            snapshot,
            yes,
            force,
        } => {
            let target = match (point, snapshot) {
                (_, Some(id)) => commands::rewind::Target::Snapshot(id),
                (Some(n), None) => commands::rewind::Target::Number(n),
                (None, None) => unreachable!("clap requires a point or --snapshot"),
            };
            commands::rewind::run(&ctx, &root(&project)?, &target, yes, force)
        }
        Command::Verify { project, json } => commands::verify::run(&ctx, &root(&project)?, json),
        Command::Service { action } => match action {
            ServiceAction::Install => {
                println!("{}", commands::service::install(&ctx)?);
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
