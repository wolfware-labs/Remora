//! What each job does to a project, and how engine errors become wire errors.

use remora_engine::{EngineError, RewindReport, short_label};
use remora_ipc::{
    DEFAULT_POINTS, ErrorBody, ErrorKind, FailedPath, JournalCheck, MAX_COMMANDS_AFTER, MAX_POINTS,
    MAX_PREVIEW_PATHS, Preview, Response, Rewound,
};
use remora_journal::{BreakReason, CommandRecord, VerifyReport};

use crate::worker::{Job, Reply, State};

pub(crate) fn execute(state: &mut State, job: Job) -> Reply {
    let ctx = state.context();
    // Forgetting never opens the project: its files must not be in use when they go.
    if let Job::Forget { force } = job {
        state.close();
        return ctx
            .engine
            .forget(state.root(), force)
            .map(|forgotten| Response::Forgotten {
                data_dir: forgotten.data_dir,
                blob_home: forgotten.blob_home,
            })
            .map_err(engine_error);
    }
    let open = state.open().map_err(engine_error)?;
    let result = match job {
        Job::BeginTurn {
            session,
            prompt,
            deadline_ms,
        } => {
            let at = open.at_ms();
            open.project
                .begin_turn(&session, prompt.as_deref(), at)
                .map(|snapshot| {
                    // The agent stopped waiting before the start snapshot was ready: the turn
                    // may already have changed files.
                    if deadline_ms.is_some_and(|deadline| open.now_ms() > deadline) {
                        log::warn!(
                            "session {}: a turn's start snapshot finished after the agent's deadline, marked late",
                            session.id
                        );
                        if let Err(e) = open.project.mark_turn_start_late(&session) {
                            log::warn!("session {}: could not mark the turn start late: {e}", session.id);
                        }
                    }
                    Response::Snapshot {
                        snapshot,
                        notice: open.project.take_notice(),
                    }
                })
        }
        Job::BeforeCommand {
            session,
            command,
            tool_use_id,
            deadline_ms,
        } => {
            let at = open.at_ms();
            open.project
                .before_command(&session, &command, tool_use_id.as_deref(), at)
                .map(|snapshot| {
                    // The agent stopped waiting before the checkpoint was ready: the command
                    // may already have started.
                    if deadline_ms.is_some_and(|deadline| open.now_ms() > deadline) {
                        log::warn!(
                            "session {}: a checkpoint finished after the agent's deadline, marked late",
                            session.id
                        );
                        if let Err(e) = open.project.mark_late(&session, tool_use_id.as_deref()) {
                            log::warn!("session {}: could not mark the checkpoint late: {e}", session.id);
                        }
                    }
                    Response::Snapshot {
                        snapshot,
                        notice: open.project.take_notice(),
                    }
                })
        }
        Job::AfterCommand {
            session,
            command,
            tool_use_id,
            exit_code,
        } => {
            let at = open.at_ms();
            open.project
                .after_command(&session, &command, tool_use_id.as_deref(), exit_code, at)
                .map(|()| Response::Recorded)
        }
        Job::EndSession { session } => {
            let at = open.at_ms();
            open.project
                .end_session(&session, at)
                .map(|entry| Response::TurnEnded {
                    seq: entry.map(|e| e.seq),
                })
        }
        Job::Preview { point } => open.project.preview(&point).map(preview),
        Job::RestoreSnapshot { snapshot, force } => {
            let at = open.at_ms();
            open.project.restore_snapshot(&snapshot, at, force).map(rewound)
        }
        Job::EndTurn { session } => {
            let at = open.at_ms();
            open.project
                .end_turn(&session, at)
                .map(|entry| Response::TurnEnded {
                    seq: entry.map(|e| e.seq),
                })
        }
        Job::RestorePoints { skip, limit } => open.project.restore_points().map(|points| {
            let total = points.len();
            let limit = limit.unwrap_or(DEFAULT_POINTS).min(MAX_POINTS);
            let points = points.into_iter().skip(skip).take(limit).collect();
            Response::RestorePoints { points, total }
        }),
        Job::Rewind { point, force } => {
            let at = open.at_ms();
            open.project.rewind(&point, at, force).map(rewound)
        }
        Job::Status => open.project.status().map(|mut status| {
            status.retention = Some(ctx.config.retention);
            status.config_problem.clone_from(&ctx.config.problem);
            Response::Status { status }
        }),
        Job::Forget { .. } => unreachable!("handled before the project is opened"),
        Job::Prune { dry_run } => {
            let now = open.now_ms();
            open.project
                .prune(&ctx.config.retention, now, dry_run)
                .map(|report| Response::Pruned { report })
        }
        Job::Verify => open
            .project
            .journal()
            .verify()
            .map(|report| Response::Verified {
                journal: check(report),
            })
            .map_err(EngineError::from),
        Job::Sweep => open.sweep().map(|()| Response::Recorded),
    };
    result.map_err(engine_error)
}

/// At most `max` commands, oldest first, each cut like a label; and how many were left out.
fn bounded_commands(commands: Vec<CommandRecord>) -> (Vec<CommandRecord>, usize) {
    let omitted = commands.len().saturating_sub(MAX_COMMANDS_AFTER);
    let kept = commands
        .into_iter()
        .take(MAX_COMMANDS_AFTER)
        .map(|mut c| {
            c.command = short_label(&c.command);
            c
        })
        .collect();
    (kept, omitted)
}

fn preview(preview: remora_engine::Preview) -> Response {
    let capped = |paths: Vec<String>| {
        let total = paths.len();
        (
            paths.into_iter().take(MAX_PREVIEW_PATHS).collect::<Vec<_>>(),
            total,
        )
    };
    let (restore, restore_total) = capped(preview.restore);
    let (delete, delete_total) = capped(preview.delete);
    let (revert, revert_total) = capped(preview.revert);
    let (commands_after, commands_omitted) = bounded_commands(preview.commands_after);
    Response::Preview(Preview {
        restore,
        restore_total,
        delete,
        delete_total,
        revert,
        revert_total,
        commands_after,
        commands_omitted,
    })
}

fn rewound(report: RewindReport) -> Response {
    let (commands_after, commands_omitted) = bounded_commands(report.commands_after);
    Response::Rewound(Rewound {
        safety_snapshot: report.safety_snapshot,
        written: report.restore.written,
        deleted: report.restore.deleted,
        skipped: report.restore.skipped,
        failed: report
            .restore
            .failed
            .into_iter()
            .map(|(path, error)| FailedPath { path, error })
            .collect(),
        commands_after,
        commands_omitted,
        seq: report.entry.seq,
    })
}

fn check(report: VerifyReport) -> JournalCheck {
    match report {
        VerifyReport::Intact { entries } => JournalCheck::Intact { entries },
        VerifyReport::Broken { seq, reason } => JournalCheck::Broken {
            seq,
            reason: match reason {
                BreakReason::Unparseable => "unparseable",
                BreakReason::ChainBroken => "chain_broken",
                BreakReason::HashMismatch => "hash_mismatch",
                BreakReason::BadSignature => "bad_signature",
            }
            .to_owned(),
        },
    }
}

/// The wire form of an engine error. The message is the engine's own, written for people.
pub(crate) fn engine_error(e: EngineError) -> ErrorBody {
    let kind = match &e {
        EngineError::TurnInProgress { session } => ErrorKind::TurnInProgress {
            session: session.clone(),
        },
        EngineError::UnknownRestorePoint(_) => ErrorKind::UnknownRestorePoint,
        EngineError::Pruned { pruned_at_ms, .. } => ErrorKind::Pruned {
            pruned_at_ms: *pruned_at_ms,
        },
        EngineError::RootOverlapsDataDir { .. } => ErrorKind::RootOverlapsDataDir,
        EngineError::RewindIncomplete { safety_snapshot, .. } => ErrorKind::RewindIncomplete {
            safety_snapshot: safety_snapshot.clone(),
        },
        EngineError::LockTimeout => ErrorKind::LockTimeout,
        EngineError::InvalidSessionId(_) => ErrorKind::InvalidSessionId,
        _ => ErrorKind::Internal,
    };
    ErrorBody::new(kind, e.to_string())
}
