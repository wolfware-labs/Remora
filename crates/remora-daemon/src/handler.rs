//! What each job does to a project, and how engine errors become wire errors.

use remora_engine::{EngineError, RewindReport, short_label};
use remora_ipc::{
    DEFAULT_POINTS, ErrorBody, ErrorKind, FailedPath, JournalCheck, MAX_COMMANDS_AFTER, MAX_POINTS, Response,
    Rewound,
};
use remora_journal::{BreakReason, VerifyReport};

use crate::worker::{Job, Reply, State};

pub(crate) fn execute(state: &mut State, job: Job) -> Reply {
    let open = state.open().map_err(engine_error)?;
    let result = match job {
        Job::BeginTurn { session, prompt } => {
            let at = open.at_ms();
            open.project
                .begin_turn(&session, prompt.as_deref(), at)
                .map(|snapshot| Response::Snapshot { snapshot })
        }
        Job::BeforeCommand { session, command } => {
            let at = open.at_ms();
            open.project
                .before_command(&session, &command, None, at)
                .map(|snapshot| Response::Snapshot { snapshot })
        }
        Job::AfterCommand {
            session,
            command,
            exit_code,
        } => {
            let at = open.at_ms();
            open.project
                .after_command(&session, &command, None, exit_code, at)
                .map(|()| Response::Recorded)
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
        Job::Status => open.project.status().map(|status| Response::Status { status }),
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

fn rewound(report: RewindReport) -> Response {
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
        commands_omitted: report.commands_after.len().saturating_sub(MAX_COMMANDS_AFTER),
        commands_after: report
            .commands_after
            .into_iter()
            .take(MAX_COMMANDS_AFTER)
            .map(|mut c| {
                c.command = short_label(&c.command);
                c
            })
            .collect(),
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
