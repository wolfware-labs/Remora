//! `remorad`, the per-user Remora daemon. It owns the engine: hooks and the CLI talk to it
//! through [`remora_ipc`], and it is the only place that reads the clock.
//!
//! ```text
//! listener thread   accepts connections (socket file / named pipe)
//! connection thread one per client, one request at a time
//! project worker    one thread per project, runs its requests strictly in order
//! janitor           closes turns idle for 6 hours as interrupted
//! ```

mod clock;
mod detach;
mod foreground;
mod handler;
mod logfile;
mod server;
pub mod service;
mod worker;

pub use clock::{Clock, ManualClock, SystemClock};
pub use detach::spawn_detached;
pub use foreground::{Ran, run_in_foreground};
pub use logfile::{LOG_ROTATE_BYTES, LogFile, install_logger};
pub use server::{Daemon, DaemonConfig, DaemonError, DaemonHandle};
