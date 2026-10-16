//! Exclusive creation relative to an open directory, owner-only from the moment the
//! entry exists (spec 5.1). `remora_fs::private` is the only caller.

use super::Dir;
use super::name::check_name;
use std::ffi::OsStr;
use std::fs::File;
use std::io;

/// Creates the directory `name` in `parent` and opens it. Unix: mode `0o700`.
/// Windows: owner the current user, protected DACL for the user and `SYSTEM`.
pub(crate) fn create_dir(parent: &Dir, name: &OsStr) -> io::Result<Dir> {
    check_name(name)?;
    imp::create_dir(parent, name)
}

/// Creates the file `name` in `parent`, opened for reading and writing. Unix: mode
/// `0o600`. Windows: owner the current user, DACL inherited from `parent`.
pub(crate) fn create_file(parent: &Dir, name: &OsStr) -> io::Result<File> {
    check_name(name)?;
    imp::create_file(parent, name)
}

#[cfg(unix)]
mod imp {
    use super::Dir;
    use rustix::fs::{Mode, OFlags, mkdirat, openat};
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io;

    pub(super) fn create_dir(parent: &Dir, name: &OsStr) -> io::Result<Dir> {
        mkdirat(parent, name, Mode::RWXU)?;
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        Ok(Dir::from_fd(openat(parent, name, flags, Mode::empty())?))
    }

    pub(super) fn create_file(parent: &Dir, name: &OsStr) -> io::Result<File> {
        // O_EXCL with O_CREAT fails on any existing name, a symlink included.
        let flags =
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        Ok(File::from(openat(
            parent,
            name,
            flags,
            Mode::RUSR | Mode::WUSR,
        )?))
    }
}

#[cfg(windows)]
mod imp {
    use super::Dir;
    use crate::sys::nt::{Kind, create};
    use crate::sys::security::Descriptor;
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io;

    pub(super) fn create_dir(parent: &Dir, name: &OsStr) -> io::Result<Dir> {
        let descriptor = Descriptor::owner_only_dir()?;
        Ok(Dir::from_handle(create(
            parent,
            name,
            Kind::Dir,
            &descriptor,
        )?))
    }

    pub(super) fn create_file(parent: &Dir, name: &OsStr) -> io::Result<File> {
        let descriptor = Descriptor::owner_inheriting()?;
        Ok(File::from(create(parent, name, Kind::File, &descriptor)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::test_dir::TestDir;
    use std::io::{Read, Seek, Write};

    #[test]
    fn creates_a_directory_and_a_writable_file() {
        let tmp = TestDir::new();
        let parent = Dir::open(tmp.path()).unwrap();
        let dir = create_dir(&parent, OsStr::new("d")).unwrap();
        let mut file = create_file(&dir, OsStr::new("f")).unwrap();
        file.write_all(b"data").unwrap();
        file.rewind().unwrap();
        let mut back = String::new();
        file.read_to_string(&mut back).unwrap();
        assert_eq!(back, "data");
        assert!(tmp.path().join("d").join("f").is_file());
    }

    #[test]
    fn existing_names_and_bad_names_fail() {
        let tmp = TestDir::new();
        let parent = Dir::open(tmp.path()).unwrap();
        create_dir(&parent, OsStr::new("d")).unwrap();
        create_file(&parent, OsStr::new("f")).unwrap();
        for existing in ["d", "f"] {
            let kind = |r: io::Result<()>| r.unwrap_err().kind();
            assert_eq!(
                kind(create_dir(&parent, OsStr::new(existing)).map(drop)),
                io::ErrorKind::AlreadyExists
            );
            assert_eq!(
                kind(create_file(&parent, OsStr::new(existing)).map(drop)),
                io::ErrorKind::AlreadyExists
            );
        }
        let err = create_file(&parent, OsStr::new("a/b")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidFilename);
    }
}
