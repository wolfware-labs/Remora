//! One thread per project: it owns the project's `Project` and runs its jobs strictly in
//! order, so events from overlapping sessions never interleave. Projects run in parallel.

use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

use remora_engine::{Engine, EngineError, Project, RestorePoint, SessionRef};
use remora_ipc::{ErrorBody, ErrorKind, Response};

use crate::clock::{Clock, Monotonic};

/// Work for a project worker.
#[derive(Debug)]
pub(crate) enum Job {
    BeginTurn {
        session: SessionRef,
        prompt: Option<String>,
    },
    BeforeCommand {
        session: SessionRef,
        command: String,
    },
    AfterCommand {
        session: SessionRef,
        command: String,
        exit_code: Option<i32>,
    },
    EndTurn {
        session: SessionRef,
    },
    RestorePoints,
    Rewind {
        point: RestorePoint,
        force: bool,
    },
    Status,
    Verify,
    /// Close turns that have been idle for too long (see [`Open::sweep`]).
    Sweep,
}

pub(crate) type Reply = Result<Response, ErrorBody>;

/// Runs one job against a project. The daemon uses [`crate::handler::execute`].
pub(crate) type Executor = fn(&mut State, Job) -> Reply;

/// What every worker shares.
pub(crate) struct Context {
    pub engine: Engine,
    pub clock: Arc<dyn Clock>,
    /// Turns with no event for this long are closed as interrupted.
    pub stale_after_ms: i64,
}

/// A worker's state: the project, opened on first use and reopened after a panic.
pub(crate) struct State {
    root: PathBuf,
    ctx: Arc<Context>,
    open: Option<Open>,
}

/// An open project and its clock.
pub(crate) struct Open {
    pub project: Project,
    times: Monotonic,
    clock: Arc<dyn Clock>,
    stale_after_ms: i64,
}

impl Open {
    /// The time for the next event: never earlier than any event already recorded.
    pub(crate) fn at_ms(&mut self) -> i64 {
        self.times.next(self.clock.now_ms())
    }

    /// Close, as interrupted, the turns with no event for the stale period (an interrupted
    /// agent never sends its end event).
    pub(crate) fn sweep(&mut self) -> Result<(), EngineError> {
        let at = self.at_ms();
        let closed = self.project.close_idle_turns(at - self.stale_after_ms, at)?;
        for entry in &closed {
            log::info!(
                "closed idle turn of session {} in {} as interrupted",
                entry.record.session_id,
                self.project.root().display()
            );
        }
        Ok(())
    }
}

impl State {
    /// The open project, opening it first if needed. Opening closes turns that went stale
    /// while the daemon was not running.
    pub(crate) fn open(&mut self) -> Result<&mut Open, EngineError> {
        let open = match self.open.take() {
            Some(open) => open,
            None => {
                let project = self.ctx.engine.project(&self.root)?;
                let mut open = Open {
                    times: Monotonic::starting_after(latest_at_ms(&project)?),
                    project,
                    clock: Arc::clone(&self.ctx.clock),
                    stale_after_ms: self.ctx.stale_after_ms,
                };
                open.sweep()?;
                open
            }
        };
        Ok(self.open.insert(open))
    }
}

/// The latest time recorded anywhere in the project, so times stay increasing across
/// daemon restarts even if the clock went backwards in between.
fn latest_at_ms(project: &Project) -> Result<i64, EngineError> {
    let journaled = project
        .journal()
        .entries()?
        .into_iter()
        .map(|e| e.record.ended_at_ms);
    let points = project.restore_points()?.into_iter().map(|p| p.at_ms);
    Ok(journaled.chain(points).max().unwrap_or(0))
}

struct Task {
    job: Job,
    reply: mpsc::Sender<Reply>,
}

/// Sends jobs to one project's worker. The worker stops once every handle is dropped and
/// the jobs already sent are done.
#[derive(Clone)]
pub(crate) struct WorkerHandle {
    tx: mpsc::Sender<Task>,
}

impl WorkerHandle {
    /// Run `job` on the worker and wait for its answer.
    pub(crate) fn run(&self, job: Job) -> Reply {
        let (reply, answer) = mpsc::channel();
        if self.tx.send(Task { job, reply }).is_err() {
            return Err(ErrorBody::new(
                ErrorKind::Internal,
                "the project worker has stopped",
            ));
        }
        answer.recv().unwrap_or_else(|_| {
            Err(ErrorBody::new(
                ErrorKind::Internal,
                "the project worker has stopped",
            ))
        })
    }
}

/// Start the worker for the project at `root`.
pub(crate) fn spawn(
    root: PathBuf,
    ctx: Arc<Context>,
    execute: Executor,
) -> std::io::Result<(WorkerHandle, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel::<Task>();
    let thread = thread::Builder::new()
        .name("remora-project".into())
        .spawn(move || {
            let mut state = State {
                root,
                ctx,
                open: None,
            };
            for task in rx {
                let reply = match panic::catch_unwind(AssertUnwindSafe(|| execute(&mut state, task.job))) {
                    Ok(reply) => reply,
                    Err(_) => {
                        // The project may be half-updated in memory: drop it, reopen on next use.
                        log::error!(
                            "a request for {} panicked; reopening the project",
                            state.root.display()
                        );
                        state.open = None;
                        Err(ErrorBody::new(
                            ErrorKind::Internal,
                            "internal error; the request did not complete",
                        ))
                    }
                };
                let _ = task.reply.send(reply);
            }
        })?;
    Ok((WorkerHandle { tx }, thread))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::clock::ManualClock;

    fn context() -> (tempfile::TempDir, Arc<Context>) {
        let data = tempfile::TempDir::new().unwrap();
        let ctx = Context {
            engine: Engine::open(data.path()).unwrap(),
            clock: Arc::new(ManualClock::new(1_000)),
            stale_after_ms: 60_000,
        };
        (data, Arc::new(ctx))
    }

    /// Panics on `Verify`, otherwise reports whether the project was already open.
    fn flaky(state: &mut State, job: Job) -> Reply {
        let was_open = state.open.is_some();
        state
            .open()
            .map_err(|e| ErrorBody::new(ErrorKind::Internal, e.to_string()))?;
        match job {
            Job::Verify => panic!("simulated bug"),
            _ => Ok(Response::TurnEnded {
                seq: Some(i64::from(was_open)),
            }),
        }
    }

    #[test]
    fn a_panicking_request_gets_an_internal_error_and_the_worker_recovers() {
        let (_data, ctx) = context();
        let project = tempfile::TempDir::new().unwrap();
        let (worker, thread) = spawn(project.path().to_path_buf(), ctx, flaky).unwrap();

        assert_eq!(worker.run(Job::Status), Ok(Response::TurnEnded { seq: Some(0) }));
        let failed = worker.run(Job::Verify).unwrap_err();
        assert_eq!(failed.kind, ErrorKind::Internal);
        assert_eq!(
            worker.run(Job::Status),
            Ok(Response::TurnEnded { seq: Some(0) }),
            "the project was reopened after the panic"
        );

        drop(worker);
        thread.join().unwrap();
    }
}
