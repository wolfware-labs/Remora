use std::io::{self, BufRead, Read, Write};

use serde::Serialize;

use crate::MAX_MESSAGE_BYTES;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("message larger than {MAX_MESSAGE_BYTES} bytes")]
    TooLarge,
    #[error("message is not UTF-8")]
    NotUtf8,
    #[error("connection closed in the middle of a message")]
    Truncated,
}

/// Write `message` as one JSON line.
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, message: &T) -> Result<(), FrameError> {
    let mut line = serde_json::to_vec(message).map_err(io::Error::from)?;
    if line.len() > MAX_MESSAGE_BYTES {
        return Err(FrameError::TooLarge);
    }
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()?;
    Ok(())
}

/// Read one line, without its newline. `Ok(None)` when the peer closed the connection
/// between messages. Never buffers more than [`MAX_MESSAGE_BYTES`] + 1 bytes.
pub fn read_frame<R: BufRead>(r: &mut R) -> Result<Option<String>, FrameError> {
    let mut line = Vec::new();
    let limit = MAX_MESSAGE_BYTES as u64 + 1;
    r.take(limit).read_until(b'\n', &mut line)?;
    match line.last() {
        None => Ok(None),
        Some(b'\n') => {
            line.pop();
            String::from_utf8(line).map(Some).map_err(|_| FrameError::NotUtf8)
        }
        Some(_) if line.len() as u64 == limit => Err(FrameError::TooLarge),
        Some(_) => Err(FrameError::Truncated),
    }
}
