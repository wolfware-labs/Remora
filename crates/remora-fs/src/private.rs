//! Owner-only creation of files and directories (spec 5.1): every entry Remora
//! creates is private to the user from the moment it exists, never created and then
//! restricted (D-44). No other code creates entries in `<data>`, a store or a blob
//! home.

use crate::sys::{self, Dir};
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Component, Path};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Creates the directory `name` in `parent`, exclusively, and opens it.
///
/// Unix: mode `0o700`. Windows: owner the current user, protected DACL granting full
/// control to the user and `SYSTEM`, inherited by everything created inside. An
/// existing name fails with `AlreadyExists` and is left untouched.
pub fn create_dir_private(parent: &Dir, name: &OsStr) -> io::Result<Dir> {
    let _lock = creation_lock();
    sys::create_dir(parent, name)
}

/// Creates the file `name` in `parent`, exclusively, open for reading and writing.
///
/// Unix: mode `0o600`, whatever the parent. Windows: owner the current user and the
/// DACL inherited from `parent` (spec 5.1 rule 2), so the file is owner-only when
/// `parent` is: in `<data>`, a store or a blob home every directory comes from
/// [`create_dir_private`], and #62 checks the ones that already exist.
///
/// An existing name, a symlink included, fails with `AlreadyExists`; a symlink's
/// target is never opened.
pub fn create_private_file(parent: &Dir, name: &OsStr) -> io::Result<File> {
    let _lock = creation_lock();
    sys::create_file(parent, name)
}

/// Opens the directory at the absolute `path`, first creating each missing
/// component owner-only, so a missing `~/.local/share` is created private as well.
///
/// Existing leading components are followed; an existing last component must be a
/// directory, not a symlink.
pub fn create_dir_all_private(path: &Path) -> io::Result<Dir> {
    if !path.is_absolute()
        || path.components().any(|c| {
            // A verbatim (`\\?\`) Windows path keeps `.` and `..` as plain names.
            matches!(c, Component::CurDir | Component::ParentDir)
                || c.as_os_str() == "."
                || c.as_os_str() == ".."
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not an absolute, normalized path: {}", path.display()),
        ));
    }
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let (Some(name), Some(parent)) = (existing.file_name(), existing.parent()) else {
                    return Err(e);
                };
                missing.push(name);
                existing = parent;
            }
            Err(e) => return Err(e),
        }
    }
    if missing.is_empty() {
        return Dir::open(path);
    }
    let mut dir = Dir::open(&std::fs::canonicalize(existing)?)?;
    for name in missing.into_iter().rev() {
        dir = match create_dir_private(&dir, name) {
            // Another process created it in between: open what is there, relative to
            // the parent and without following a link.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                crate::nofollow::open_dir(&dir, name)?
            }
            result => result?,
        };
    }
    Ok(dir)
}

/// Runs `f` with the process umask set to `mask`, then restores the previous umask,
/// also when `f` panics (spec 5.1 rule 5).
///
/// A Unix socket takes its mode from the umask at `bind`. The umask is process-wide,
/// so this holds the lock that the creation functions take: nothing is created
/// through this module while the umask is changed.
#[cfg(unix)]
pub fn with_umask<T>(mask: u32, f: impl FnOnce() -> T) -> T {
    let _lock = creation_lock();
    let _umask = sys::ScopedUmask::set(mask);
    f()
}

static CREATION: Mutex<()> = Mutex::new(());

/// Serializes creation with umask changes. The umask exists only on Unix; locking on
/// every OS keeps one code path. A panic while the lock was held leaves nothing to
/// repair, so a poisoned lock is taken over.
pub(crate) fn creation_lock() -> MutexGuard<'static, ()> {
    CREATION.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::test_dir::TestDir;

    fn name(s: &str) -> &OsStr {
        OsStr::new(s)
    }

    #[cfg(unix)]
    fn mode_of(file: &File) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        file.metadata().unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn dir_mode(dir: &Dir) -> u32 {
        use std::os::fd::AsFd;
        mode_of(&File::from(dir.as_fd().try_clone_to_owned().unwrap()))
    }

    #[cfg(unix)]
    #[test]
    fn dir_mode_is_0700_under_any_umask() {
        let tmp = TestDir::new();
        let parent = Dir::open(tmp.path()).unwrap();
        for (i, mask) in [0o000, 0o022].into_iter().enumerate() {
            let dir =
                with_umask(mask, || sys::create_dir(&parent, name(&format!("d{i}")))).unwrap();
            assert_eq!(dir_mode(&dir), 0o700, "umask {mask:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_mode_is_0600_under_any_umask() {
        let tmp = TestDir::new();
        let parent = Dir::open(tmp.path()).unwrap();
        for (i, mask) in [0o000, 0o022].into_iter().enumerate() {
            let file =
                with_umask(mask, || sys::create_file(&parent, name(&format!("f{i}")))).unwrap();
            assert_eq!(mode_of(&file), 0o600, "umask {mask:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn with_umask_excludes_creation() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, mpsc};
        use std::time::Duration;

        let tmp = TestDir::new();
        let parent = Dir::open(tmp.path()).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = mpsc::channel();
        let holder = {
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                with_umask(0o177, || {
                    started_tx.send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(200));
                    done.store(true, Ordering::SeqCst);
                });
            })
        };
        started_rx.recv().unwrap();
        let dir = create_dir_private(&parent, name("waited")).unwrap();
        assert!(
            done.load(Ordering::SeqCst),
            "creation ran while the umask was changed"
        );
        assert_eq!(dir_mode(&dir), 0o700);
        holder.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn with_umask_restores_on_panic() {
        let current = || {
            let _lock = creation_lock();
            sys::ScopedUmask::set(0o077).previous()
        };
        let before = current();
        let result = std::panic::catch_unwind(|| with_umask(0o177, || panic!("inside with_umask")));
        assert!(result.is_err());
        assert_eq!(current(), before);
    }

    #[test]
    fn existing_name_fails_already_exists() {
        let tmp = TestDir::new();
        let parent = Dir::open(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("file"), b"keep").unwrap();
        tmp.mkdir_all("dir/inner");
        for existing in ["file", "dir"] {
            let err = create_private_file(&parent, name(existing)).unwrap_err();
            assert_eq!(
                err.kind(),
                io::ErrorKind::AlreadyExists,
                "file over {existing}: {err}"
            );
            let err = create_dir_private(&parent, name(existing)).unwrap_err();
            assert_eq!(
                err.kind(),
                io::ErrorKind::AlreadyExists,
                "dir over {existing}: {err}"
            );
        }
        assert_eq!(std::fs::read(tmp.path().join("file")).unwrap(), b"keep");
        assert!(tmp.path().join("dir/inner").is_dir());
    }

    #[test]
    fn symlink_name_is_not_followed() {
        let tmp = TestDir::new();
        let outside = TestDir::new();
        let target = outside.path().join("target");
        std::fs::write(&target, b"outside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, tmp.path().join("link")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, tmp.path().join("link")).unwrap();
        let parent = Dir::open(tmp.path()).unwrap();
        let err = create_private_file(&parent, name("link")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
        assert_eq!(std::fs::read(&target).unwrap(), b"outside");
    }

    #[test]
    fn multi_component_names_are_refused() {
        let tmp = TestDir::new();
        let parent = Dir::open(tmp.path()).unwrap();
        for bad in ["a/b", "..", ""] {
            let err = create_dir_private(&parent, name(bad)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidFilename, "{bad:?}");
        }
        assert!(!tmp.path().join("a").exists());
    }

    #[test]
    fn create_dir_all_private_creates_missing_ancestors_owner_only() {
        let tmp = TestDir::new();
        let leaf = tmp.path().join("share").join("remora").join("data");
        let dir = create_dir_all_private(&leaf).unwrap();
        assert!(leaf.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(dir_mode(&dir), 0o700);
            for p in [tmp.path().join("share"), tmp.path().join("share/remora")] {
                let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o700, "{}", p.display());
            }
        }
        #[cfg(windows)]
        drop(dir);
        // Everything exists now: the call only opens.
        create_dir_all_private(&leaf).unwrap();
    }

    #[test]
    fn create_dir_all_private_refuses_relative_and_symlinked_paths() {
        let err = create_dir_all_private(Path::new("relative/path")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let tmp = TestDir::new();
        let dotted = tmp.path().join("a").join("..").join("b");
        let err = create_dir_all_private(&dotted).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        #[cfg(windows)]
        {
            let verbatim = format!(r"\\?\{}\a\..\b", tmp.path().display());
            let err = create_dir_all_private(Path::new(&verbatim)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
        drop(tmp);
        let tmp = TestDir::new();
        let real = tmp.mkdir_all("real");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, tmp.path().join("link")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real, tmp.path().join("link")).unwrap();
        assert!(create_dir_all_private(&tmp.path().join("link")).is_err());
        // An existing component that is a file stops the walk before anything is made.
        std::fs::write(tmp.path().join("file"), b"").unwrap();
        let err = create_dir_all_private(&tmp.path().join("file").join("child")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotADirectory, "{err}");
        // A symlink in a leading component is followed.
        create_dir_all_private(&tmp.path().join("link").join("child")).unwrap();
        assert!(real.join("child").is_dir());
    }

    #[cfg(windows)]
    mod windows {
        use super::*;
        use crate::sys::security::Sid;
        use crate::sys::security::test_support::read;
        use std::os::windows::io::AsHandle;
        use windows_sys::Win32::Security::{
            CONTAINER_INHERIT_ACE, INHERITED_ACE, OBJECT_INHERIT_ACE,
        };
        use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

        fn user() -> String {
            Sid::current_user().unwrap().to_string_form().unwrap()
        }

        fn sids(info: &crate::sys::security::test_support::Info) -> Vec<String> {
            let mut sids: Vec<String> = info.aces.iter().map(|a| a.sid.clone()).collect();
            sids.sort();
            sids
        }

        fn expected_sids() -> Vec<String> {
            let mut sids = vec![user(), "S-1-5-18".to_owned()];
            sids.sort();
            sids
        }

        #[test]
        fn windows_dir_dacl_is_protected_user_and_system() {
            let tmp = TestDir::new();
            let parent = Dir::open(tmp.path()).unwrap();
            let dir = create_dir_private(&parent, name("private")).unwrap();
            let info = read(dir.as_handle()).unwrap();
            assert!(info.protected);
            assert_eq!(sids(&info), expected_sids());
            for ace in &info.aces {
                assert_eq!(ace.kind, 0, "access-allowed ACE");
                assert_eq!(ace.mask, FILE_ALL_ACCESS);
                let inherit = (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8;
                assert_eq!(ace.flags & inherit, inherit, "{}", ace.sid);
            }
        }

        #[test]
        fn windows_file_inherits_owner_only_dacl() {
            let tmp = TestDir::new();
            let parent = Dir::open(tmp.path()).unwrap();
            let dir = create_dir_private(&parent, name("private")).unwrap();
            let file = create_private_file(&dir, name("secret")).unwrap();
            let info = read(file.as_handle()).unwrap();
            assert!(!info.protected);
            assert_eq!(sids(&info), expected_sids());
            for ace in &info.aces {
                assert_ne!(ace.flags & INHERITED_ACE as u8, 0, "{}", ace.sid);
            }
        }

        /// GitHub runners are elevated, where the default owner of a new object is
        /// `Administrators`; the explicit owner keeps it the user.
        #[test]
        fn elevated_owner_is_user_sid() {
            let tmp = TestDir::new();
            let parent = Dir::open(tmp.path()).unwrap();
            let dir = create_dir_private(&parent, name("private")).unwrap();
            let file = create_private_file(&dir, name("secret")).unwrap();
            assert_eq!(read(dir.as_handle()).unwrap().owner, user());
            assert_eq!(read(file.as_handle()).unwrap().owner, user());
        }
    }
}
