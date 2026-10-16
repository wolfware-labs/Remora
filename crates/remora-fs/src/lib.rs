//! The filesystem and OS safety module: atomic writes, owner-only creation, fsync
//! points, symlink-safe open and walk, locks, clones and SQLite connection setup.

// The only module allowed to contain `unsafe` (spec 3.5, D-06).
#[allow(unsafe_code)]
pub mod sys;
