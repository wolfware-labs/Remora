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
//! named pipe derived from it (Windows): see [`endpoint`].

mod client;
mod endpoint;
mod framing;
mod protocol;

pub use client::{Client, ClientError, DEFAULT_TIMEOUT};
pub use endpoint::{Endpoint, endpoint};
pub use framing::{FrameError, read_frame, write_frame};
pub use protocol::{
    ErrorBody, ErrorKind, FailedPath, JournalCheck, MAX_MESSAGE_BYTES, Outcome, PROTOCOL_VERSION, Request,
    RequestEnvelope, Response, ResponseEnvelope, Rewound,
};
