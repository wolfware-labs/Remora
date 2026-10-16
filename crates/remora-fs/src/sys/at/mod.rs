//! Descriptor- and handle-relative system calls: each takes an open directory and
//! a single name, so no path string is resolved (spec 5.5). One module per OS with
//! the same functions; `remora_fs::nofollow` holds the rules on top of them.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::*;
