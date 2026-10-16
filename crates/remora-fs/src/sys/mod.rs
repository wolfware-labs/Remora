//! System calls behind safe functions, and the only place in the workspace where
//! `unsafe` is allowed (spec 3.5, D-06).
//!
//! Rules for every submodule:
//!
//! 1. Built on `rustix` (Unix) and `windows-sys` (Windows). `libc` only for the macOS
//!    calls spec 3.5 lists.
//! 2. Every `unsafe` block carries a `// SAFETY:` comment stating the invariant it
//!    relies on; clippy denies one without it.
//! 3. Every function has its own unit tests on each OS where it compiles.
//! 4. No raw pointer, raw descriptor or `HANDLE` crosses the module boundary: the API
//!    takes and returns plain Rust types.

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!("remora-fs supports Linux, macOS and Windows only");

mod dir;
pub mod random;

pub use dir::Dir;

#[cfg(test)]
pub(crate) mod test_dir {
    use std::path::{Path, PathBuf};

    /// A fresh directory under the system temporary directory, removed on drop.
    pub(crate) struct TestDir(PathBuf);

    impl TestDir {
        pub(crate) fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("remora-fs-{}", super::random::hex(8).unwrap()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
