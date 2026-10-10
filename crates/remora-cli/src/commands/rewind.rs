use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use remora_engine::RestorePoint;
use remora_ipc::{Client, ClientError, ErrorKind, Preview, Request, Response, Rewound};
use remora_snap::SnapshotId;

use super::{
    CliError, Ctx, EXIT_ERROR, EXIT_REFUSED, EXIT_USAGE, ago, now_ms, one_line, open_sessions, point_kind,
    points_page, short_session, unexpected,
};

/// What to rewind to.
#[derive(Debug, Clone)]
pub enum Target {
    /// `#N` (or `N`) as `remora log` numbers points.
    Number(usize),
    /// A raw snapshot id, such as the safety snapshot of an earlier rewind.
    Snapshot(String),
}

/// Parse `#3` or `3`.
pub fn parse_number(text: &str) -> Result<usize, String> {
    text.trim_start_matches('#')
        .parse::<usize>()
        .ok()
        .filter(|n| *n >= 1)
        .ok_or_else(|| format!("`{text}` is not a restore point number like #3"))
}

/// `remora rewind`: preview, confirm, rewind.
pub fn run(ctx: &Ctx, root: &Path, target: &Target, yes: bool, force: bool) -> Result<(), CliError> {
    let interactive = std::io::stdin().is_terminal();
    if !yes && !interactive {
        return Err(CliError::new(
            EXIT_USAGE,
            "not on a terminal, so there is no one to confirm: pass --yes",
        ));
    }
    let mut client = ctx.client()?;
    // Refuse before showing a preview the user cannot act on. After a failed rollback the
    // project answers only a restore by snapshot id, the way out: let that one through (the
    // daemon still refuses it while a turn is open).
    if !force && agent_works(&mut client, root, matches!(target, Target::Snapshot(_)))? {
        return Err(busy(&mut client, root));
    }
    let answer = match target {
        Target::Number(n) => {
            let (page, total) = points_page(&mut client, root, n - 1, 1)?;
            let Some(point) = page.into_iter().next() else {
                return Err(CliError::new(
                    EXIT_USAGE,
                    format!("there is no #{n}: this project has {total} restore points (see `remora log`)"),
                ));
            };
            let preview = match client.call(Request::Preview {
                root: root.to_path_buf(),
                point: point.clone(),
            }) {
                Ok(Response::Preview(preview)) => preview,
                Ok(other) => return Err(unexpected(&other)),
                Err(e) => return Err(refusal(&mut client, root, e)),
            };
            print_preview(*n, &point, &preview);
            if !yes && !confirm()? {
                println!("nothing changed");
                return Ok(());
            }
            client.call(Request::Rewind {
                root: root.to_path_buf(),
                point,
                force,
            })
        }
        Target::Snapshot(id) => {
            let snapshot = SnapshotId::parse(id)
                .map_err(|_| CliError::new(EXIT_USAGE, format!("`{id}` is not a snapshot id")))?;
            println!("Restore snapshot {snapshot}");
            if !yes && !confirm()? {
                println!("nothing changed");
                return Ok(());
            }
            client.call(Request::RestoreSnapshot {
                root: root.to_path_buf(),
                snapshot,
                force,
            })
        }
    };
    match answer {
        Ok(Response::Rewound(rewound)) => {
            print_rewound(&rewound);
            if rewound.failed.is_empty() {
                Ok(())
            } else {
                // Only part of the project was restored: say so in the exit code too.
                Err(CliError::new(
                    EXIT_ERROR,
                    format!(
                        "{} paths could not be restored (listed above); your files from before the rewind are kept: `remora rewind --snapshot {}` brings them back",
                        rewound.failed.len(),
                        rewound.safety_snapshot
                    ),
                ))
            }
        }
        Ok(other) => Err(unexpected(&other)),
        Err(e) => Err(refusal(&mut client, root, e)),
    }
}

/// Whether a turn is open in the project. For a restore by snapshot id, a project that
/// answers only that request (its rollback failed) counts as idle.
fn agent_works(client: &mut Client, root: &Path, by_snapshot: bool) -> Result<bool, CliError> {
    match client.call(Request::Status {
        root: root.to_path_buf(),
    }) {
        Ok(Response::Status { status }) => Ok(!status.open_sessions.is_empty()),
        Ok(other) => Err(unexpected(&other)),
        Err(ClientError::Daemon(body))
            if by_snapshot && matches!(body.kind, ErrorKind::RewindIncomplete { .. }) =>
        {
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

/// The refusal while a turn is open: which sessions, and what to do. Claude Code sends no
/// end event when the user interrupts it, so an interrupted agent still looks busy.
pub(super) fn busy(client: &mut Client, root: &Path) -> CliError {
    let mut message = String::from("an agent is still working in this project:\n");
    let now = now_ms();
    let points = points_page(client, root, 0, remora_ipc::MAX_POINTS)
        .map(|p| p.0)
        .unwrap_or_default();
    for (id, started, last, label) in open_sessions(&points) {
        let label = label.as_deref().map(|l| one_line(l, 50)).unwrap_or_default();
        message.push_str(&format!(
            "  session {}  started {}  ·  last event {}  {label}\n",
            short_session(&id),
            ago(now, started),
            ago(now, last)
        ));
    }
    message
        .push_str("stop the agent first; if you interrupted it (Esc), it has already stopped: pass --force");
    CliError::new(EXIT_REFUSED, message)
}

/// Turn a refused or failed rewind into a message that says what to do.
fn refusal(client: &mut Client, root: &Path, e: ClientError) -> CliError {
    let ClientError::Daemon(body) = &e else {
        return e.into();
    };
    match &body.kind {
        ErrorKind::TurnInProgress { .. } => busy(client, root),
        ErrorKind::RewindIncomplete { safety_snapshot } => {
            CliError::new(EXIT_ERROR, incomplete(&body.message, safety_snapshot))
        }
        ErrorKind::UnknownRestorePoint => CliError::new(
            EXIT_ERROR,
            format!(
                "{} (new events may have arrived: run `remora log` again)",
                body.message
            ),
        ),
        _ => e.into(),
    }
}

/// A rewind that did not finish, and how to get the files from before it back: once, as
/// the message of a failed rollback already says it.
fn incomplete(message: &str, safety: &str) -> String {
    let hint = format!("remora rewind --snapshot {safety}");
    if message.contains(&hint) {
        message.to_owned()
    } else {
        format!("{message}\nyour files from before the rewind are kept: `{hint}` brings them back")
    }
}

fn print_preview(n: usize, point: &RestorePoint, preview: &Preview) {
    let label = point
        .label
        .as_deref()
        .map(|l| format!(" `{}`", one_line(l, 60)))
        .unwrap_or_default();
    println!(
        "Rewind to #{n}  ·  {}{label}  ·  {}",
        point_kind(point),
        ago(now_ms(), point.at_ms)
    );
    if point.late {
        println!(
            "  this point was taken after the command may have started: it may already include its effects"
        );
    }
    let line = |verb: &str, paths: &[String], total: usize, note: &str| {
        if total > 0 {
            let shown: Vec<_> = paths.iter().take(3).map(String::as_str).collect();
            let more = if total > shown.len() { ", …" } else { "" };
            println!("  {verb:<8} {total:>5} paths  ({}{more}){note}", shown.join(", "));
        }
    };
    line("restore", &preview.restore, preview.restore_total, "");
    line(
        "delete",
        &preview.delete,
        preview.delete_total,
        "  created after this point",
    );
    line("revert", &preview.revert, preview.revert_total, "");
    if preview.restore_total + preview.delete_total + preview.revert_total == 0 {
        println!("  no file changes: the project already matches this point");
    }
    print_commands(&preview.commands_after, preview.commands_omitted);
}

fn print_commands(commands: &[remora_journal::CommandRecord], omitted: usize) {
    if commands.is_empty() {
        return;
    }
    println!("Can't undo — review these:");
    let now = now_ms();
    for command in commands {
        let when = command.at_ms.map(|at| ago(now, at)).unwrap_or_default();
        println!("  {when:<12} {}", one_line(&command.command, 70));
    }
    if omitted > 0 {
        println!("  … and {omitted} more");
    }
}

fn print_rewound(rewound: &Rewound) {
    println!(
        "Rewound: {} written, {} deleted.",
        rewound.written, rewound.deleted
    );
    for failed in &rewound.failed {
        println!("  could not restore {}: {}", failed.path, failed.error);
    }
    if !rewound.skipped.is_empty() {
        println!(
            "  left as they are (never protected): {}",
            rewound.skipped.join(", ")
        );
    }
    print_commands(&rewound.commands_after, rewound.commands_omitted);
    println!(
        "Undo this rewind: remora rewind --snapshot {}",
        rewound.safety_snapshot
    );
}

pub(super) fn confirm() -> Result<bool, CliError> {
    print!("Proceed? [y/N] ");
    std::io::stdout()
        .flush()
        .map_err(|e| CliError::new(EXIT_ERROR, e.to_string()))?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| CliError::new(EXIT_ERROR, e.to_string()))?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}

#[cfg(test)]
mod tests {
    use super::incomplete;

    #[test]
    fn the_way_back_is_named_once() {
        let hint = "remora rewind --snapshot 1a2b";
        let cut_short = incomplete(
            "the rewind did not finish (disk full); kept as snapshot 1a2b",
            "1a2b",
        );
        assert_eq!(cut_short.matches(hint).count(), 1, "{cut_short}");
        let rolling_back = incomplete(
            "rolling back an interrupted rewind failed (blob missing); the state before that rewind is kept as snapshot 1a2b: `remora rewind --snapshot 1a2b` restores it",
            "1a2b",
        );
        assert_eq!(rolling_back.matches(hint).count(), 1, "{rolling_back}");
    }
}
