//! The protocol between Remora clients (hooks, CLI) and the `remorad` daemon.
//!
//! One JSON object per line, one request and one response, each at most
//! [`MAX_MESSAGE_BYTES`]:
//!
//! ```text
//! → {"v":1,"id":7,"req":{"type":"before_command","root":"/home/me/proj","session":"4f1c","agent":"claude-code","command":"rm -rf build"}}
//! ← {"v":1,"id":7,"ok":{"type":"snapshot","snapshot":"9ad3…"}}
//! ← {"v":1,"id":7,"err":{"kind":"turn_in_progress","session":"4f1c","message":"…"}}
//! ```
//!
//! The daemon listens on a socket file inside the data directory (macOS, Linux) or on a
//! named pipe derived from it (Windows): see [`endpoint`]. A socket whose path would exceed
//! [`MAX_SOCKET_PATH`] lives in the owner-only runtime folder instead. Every connection
//! starts with the mutual handshake of the [`auth`] module.

pub mod auth;
mod client;
mod endpoint;
mod framing;
mod protocol;

pub use client::{Client, ClientError, DEFAULT_TIMEOUT, stop_daemon};
#[cfg(unix)]
pub use endpoint::socket_dir_is_private;
pub use endpoint::{Endpoint, MAX_SOCKET_PATH, endpoint};
pub use framing::{FrameError, read_frame, write_frame};
pub use protocol::{
    DEFAULT_POINTS, ErrorBody, ErrorKind, FailedPath, JournalCheck, MAX_COMMANDS_AFTER, MAX_MESSAGE_BYTES,
    MAX_POINTS, MAX_PREVIEW_PATHS, Outcome, PROTOCOL_VERSION, Preview, Request, RequestEnvelope, Response,
    ResponseEnvelope, Rewound,
};
