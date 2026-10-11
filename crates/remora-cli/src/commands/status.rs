use std::path::{Path, PathBuf};

use remora_ipc::{Request, Response};

use crate::{init, install_path};

use super::{
    CliError, Ctx, EXIT_NO_DAEMON, ago, now_ms, one_line, open_sessions, points_page, short_session,
    unexpected,
};

/// What Remora installed outside its data folder: which binary it runs, and what is wrong
/// with it if anything is.
struct Installed {
    exe: Option<PathBuf>,
    problem: Option<String>,
}

impl Installed {
    /// `record` is what was found, or why it could not be read.
    fn check(record: Result<Option<PathBuf>, String>, name: &str) -> Self {
        match record {
            Ok(exe) => {
                let problem = exe
                    .as_deref()
                    .and_then(|exe| install_path::problem(exe, name, std::env::var_os("PATH").as_deref()));
                Self { exe, problem }
            }
            Err(problem) => Self {
                exe: None,
                problem: Some(problem),
            },
        }
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({ "command": self.exe, "problem": self.problem })
    }

    fn print(&self, label: &str, missing: &str) {
        match (&self.exe, &self.problem) {
            (Some(exe), _) => println!("{label}  {}", exe.display()),
            (None, None) => println!("{label}  {missing}"),
            (None, Some(_)) => {}
        }
        if let Some(problem) = &self.problem {
            println!("⚠ {}: {problem}: run `remora init` to repair", label.trim_end());
        }
    }
}

/// The `remora` the hooks in Claude Code's settings run; only a missing file means none.
fn hooks_record() -> Result<Option<PathBuf>, String> {
    let path = init::settings_path().map_err(|e| e.to_string())?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let settings =
        serde_json::from_str(&text).map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;
    Ok(init::installed_exe(&settings))
}

/// The hooks in Claude Code's settings and the login service.
fn installed() -> (Installed, Installed) {
    let service = remora_daemon::service::installed_exe().map_err(|e| e.to_string());
    (
        Installed::check(hooks_record(), "remora"),
        Installed::check(service, "remorad"),
    )
}

fn print_installed((hooks, service): &(Installed, Installed)) {
    hooks.print("hooks   ", "not installed (run `remora init`)");
    service.print("service ", "not installed (hooks start the daemon when needed)");
}

/// `remora status`: the daemon, what is installed, and what is recorded for this project.
pub fn run(ctx: &Ctx, root: &Path, json: bool) -> Result<(), CliError> {
    let installed = installed();
    let mut client = match ctx.client() {
        Ok(client) => client,
        Err(e) if e.code == EXIT_NO_DAEMON => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "daemon": null, "root": root,
                        "hooks": installed.0.json(), "service": installed.1.json() })
                );
            } else {
                println!("remora {}  ·  daemon not running", env!("CARGO_PKG_VERSION"));
                println!("  start it with `remora service install` (or `remorad run`)");
                print_installed(&installed);
            }
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    let status = match client.call(Request::Status {
        root: root.to_path_buf(),
    })? {
        Response::Status { status } => status,
        other => return Err(unexpected(&other)),
    };
    let (points, _) = points_page(&mut client, root, 0, remora_ipc::MAX_POINTS)?;
    let sessions = open_sessions(&points);
    let last_snapshot = points.first().map(|p| p.at_ms);
    if json {
        let sessions: Vec<_> = sessions
            .iter()
            .map(|(id, started, last, label)| {
                serde_json::json!({ "session": id, "started_at_ms": started, "last_event_ms": last, "last_label": label })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "daemon": client.daemon_version(),
                "root": status.root,
                "project_id": remora_engine::project_id(&status.root),
                "open_sessions": sessions,
                "journal_entries": status.journal_entries,
                "restore_points": status.restore_points,
                "unprotected": status.last_unprotected,
                "blob_store": status.blob_store,
                "copy_on_write": status.copy_on_write,
                "last_snapshot_ms": last_snapshot,
                "retention": status.retention,
                "config_problem": status.config_problem,
                "plain_copies_because": status.plain_copies_because,
                "hooks": installed.0.json(),
                "service": installed.1.json(),
            })
        );
        return Ok(());
    }
    let now = now_ms();
    println!(
        "remora {}  ·  daemon {} (running)",
        env!("CARGO_PKG_VERSION"),
        client.daemon_version()
    );
    if client.daemon_version() != env!("CARGO_PKG_VERSION") {
        println!(
            "⚠ the running daemon is version {}, not this remora's: update remora, or restart the daemon",
            client.daemon_version()
        );
    }
    print_installed(&installed);
    println!(
        "project  {}  (id {})",
        status.root.display(),
        remora_engine::project_id(&status.root)
    );
    if sessions.is_empty() {
        println!("open sessions  none");
    } else {
        println!("open sessions");
        for (id, started, last, label) in &sessions {
            let label = label.as_deref().map(|l| one_line(l, 50)).unwrap_or_default();
            println!(
                "  {}  started {}  ·  last event {}  {label}",
                short_session(id),
                ago(now, *started),
                ago(now, *last)
            );
        }
    }
    let last = last_snapshot.map_or_else(|| "never".to_owned(), |at| ago(now, at));
    println!(
        "journal  {} entries  ·  {} restore points  ·  last snapshot {last}",
        status.journal_entries, status.restore_points
    );
    let mode = match status.copy_on_write {
        Some(true) => "copy-on-write",
        Some(false) => "plain copy",
        None => "not known until the next snapshot",
    };
    println!("store  {}  ({mode})", status.blob_store.display());
    if let Some(reason) = &status.plain_copies_because {
        println!("  files are copied, not cloned: {reason}");
    }
    if let Some(retention) = status.retention {
        println!(
            "retention  {} days  ·  {} GB per project  (`remora prune` applies it now)",
            retention.days, retention.max_store_gb
        );
    }
    if let Some(problem) = &status.config_problem {
        println!("⚠ config invalid, the defaults apply: {problem}");
    }
    if !status.last_unprotected.is_empty() {
        let names: Vec<_> = status
            .last_unprotected
            .iter()
            .take(5)
            .map(|p| p.display().to_string())
            .collect();
        println!(
            "not protected  {} paths ({})",
            status.last_unprotected.len(),
            names.join(", ")
        );
    }
    Ok(())
}
