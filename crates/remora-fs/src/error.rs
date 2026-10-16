//! The errors of `remora-fs` that `std::io::ErrorKind` cannot express. The daemon
//! maps them to the error kinds of spec 10.12.

use std::fmt;
use std::io;

#[derive(Debug)]
pub enum Error {
    /// A system call failed.
    Io(io::Error),
    /// The name is a symlink, which is never followed.
    IsSymlink,
    /// The name exists but is not a directory.
    NotADirectory,
    /// The entry changed between the walk's `stat` and the open: another type or
    /// another file (identity).
    Changed,
    /// Another process holds the entry open without sharing delete access
    /// (Windows), so it cannot be renamed or deleted now (`in_use`).
    InUse,
    /// A component of a parent chain is not a real directory. Holds the key prefix
    /// up to and including that component (`unsafe_parent`).
    UnsafeParent(Vec<u8>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::IsSymlink => f.write_str("is a symlink"),
            Self::NotADirectory => f.write_str("not a directory"),
            Self::Changed => f.write_str("changed while being opened"),
            Self::InUse => f.write_str("in use by another process"),
            Self::UnsafeParent(prefix) => {
                write!(f, "unsafe parent: {}", String::from_utf8_lossy(prefix))
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(e) => e,
            Error::NotADirectory => io::ErrorKind::NotADirectory.into(),
            Error::InUse => io::Error::new(io::ErrorKind::ResourceBusy, Error::InUse),
            other => io::Error::other(other),
        }
    }
}
