//! Descriptor-relative system calls on Unix, through `rustix`.

use crate::Error;
use crate::nofollow::{Entry, FileKind, Metadata, Timestamp};
use crate::sys::Dir;
use crate::sys::name::check_name;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat, Timespec, Timestamps};
use rustix::io::Errno;
use std::ffi::OsStr;
use std::fs::File;
use std::io;

/// `openat(parent, name, O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`. A symlink is
/// `IsSymlink`, anything else that is not a directory `NotADirectory`.
pub(crate) fn open_dir(parent: &Dir, name: &OsStr) -> Result<Dir, Error> {
    check_name(name)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match rustix::fs::openat(parent, name, flags, Mode::empty()) {
        Ok(fd) => Ok(Dir::from_fd(fd)),
        // Linux reports a symlink as ENOTDIR here, macOS as ELOOP; ask the entry.
        Err(Errno::NOTDIR | Errno::LOOP) => match stat_at(parent, name)?.kind {
            FileKind::Symlink => Err(Error::IsSymlink),
            _ => Err(Error::NotADirectory),
        },
        Err(e) => Err(io::Error::from(e).into()),
    }
}

/// `fstatat(parent, name, AT_SYMLINK_NOFOLLOW)`.
pub(crate) fn stat_at(parent: &Dir, name: &OsStr) -> io::Result<Metadata> {
    check_name(name)?;
    let st = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    Ok(metadata(&st))
}

/// `fstat` of an open file.
pub(crate) fn fstat(file: &File) -> io::Result<Metadata> {
    Ok(metadata(&rustix::fs::fstat(file)?))
}

/// The entries of `dir` except `.` and `..`, read through a separate open file
/// description so `dir` keeps its own offset.
pub(crate) fn entries(dir: &Dir) -> io::Result<Vec<Entry>> {
    let mut reader = rustix::fs::Dir::read_from(dir)?;
    let mut out = Vec::new();
    while let Some(entry) = reader.read() {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        out.push(Entry {
            name: name.to_vec(),
            kind_hint: kind_hint(entry.file_type()),
            metadata: None,
        });
    }
    Ok(out)
}

/// `readlinkat(parent, name)` into a buffer of `max + 1` bytes: the raw target, or
/// `InvalidData` when it is longer than `max`.
pub(crate) fn read_link_at(parent: &Dir, name: &OsStr, max: usize) -> io::Result<Vec<u8>> {
    check_name(name)?;
    let mut buf = vec![0; max + 1];
    let len = rustix::fs::readlinkat_raw(parent, name, &mut buf[..])?;
    if len > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("symlink target longer than {max} bytes"),
        ));
    }
    buf.truncate(len);
    Ok(buf)
}

/// `openat(parent, name, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)`. The
/// non-blocking flag keeps an open of a FIFO from waiting for a writer; a symlink
/// is `IsSymlink`. Call [`finish_read`] once the file is accepted.
pub(crate) fn open_read(parent: &Dir, name: &OsStr) -> Result<File, Error> {
    check_name(name)?;
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    match rustix::fs::openat(parent, name, flags, Mode::empty()) {
        Ok(fd) => Ok(File::from(fd)),
        Err(Errno::LOOP) => Err(Error::IsSymlink),
        Err(e) => Err(io::Error::from(e).into()),
    }
}

/// Clears `O_NONBLOCK` on a file from [`open_read`], for blocking reads.
pub(crate) fn finish_read(file: &File) -> io::Result<()> {
    let flags = rustix::fs::fcntl_getfl(file)?;
    Ok(rustix::fs::fcntl_setfl(file, flags - OFlags::NONBLOCK)?)
}

/// `unlinkat(parent, name, 0 or AT_REMOVEDIR)`. A symlink is removed itself.
pub(crate) fn unlink_at(parent: &Dir, name: &OsStr, is_dir: bool) -> Result<(), Error> {
    check_name(name)?;
    let flags = if is_dir {
        AtFlags::REMOVEDIR
    } else {
        AtFlags::empty()
    };
    rustix::fs::unlinkat(parent, name, flags).map_err(io::Error::from)?;
    Ok(())
}

/// `renameat(from_dir, from, to_dir, to)`, replacing a file at `to`. A symlink is
/// renamed itself.
pub(crate) fn rename_at(
    from_dir: &Dir,
    from: &OsStr,
    to_dir: &Dir,
    to: &OsStr,
) -> Result<(), Error> {
    check_name(from)?;
    check_name(to)?;
    rustix::fs::renameat(from_dir, from, to_dir, to).map_err(io::Error::from)?;
    Ok(())
}

/// `utimensat(parent, name, times, AT_SYMLINK_NOFOLLOW)`: a symlink's own times.
pub(crate) fn set_times_at(
    parent: &Dir,
    name: &OsStr,
    accessed: Timestamp,
    modified: Timestamp,
) -> io::Result<()> {
    check_name(name)?;
    let times = Timestamps {
        last_access: timespec(accessed),
        last_modification: timespec(modified),
    };
    Ok(rustix::fs::utimensat(
        parent,
        name,
        &times,
        AtFlags::SYMLINK_NOFOLLOW,
    )?)
}

fn timespec(t: Timestamp) -> Timespec {
    Timespec {
        tv_sec: t.secs,
        tv_nsec: t.nanos.into(),
    }
}

#[allow(
    clippy::unnecessary_cast,
    reason = "the field types differ between Linux and macOS"
)]
fn metadata(st: &Stat) -> Metadata {
    let kind = match FileType::from_raw_mode(st.st_mode as _) {
        FileType::RegularFile => FileKind::File,
        FileType::Directory => FileKind::Dir,
        FileType::Symlink => FileKind::Symlink,
        _ => FileKind::Other,
    };
    let stamp = |secs: i64, nanos: i64| Timestamp {
        secs,
        nanos: u32::try_from(nanos).unwrap_or(0),
    };
    Metadata {
        kind,
        mode: (st.st_mode as u32) & 0o7777,
        size: st.st_size as u64,
        modified: stamp(st.st_mtime as i64, st.st_mtime_nsec as i64),
        changed: stamp(st.st_ctime as i64, st.st_ctime_nsec as i64),
        dev: st.st_dev as u64,
        ino: u128::from(st.st_ino as u64),
    }
}

fn kind_hint(file_type: FileType) -> Option<FileKind> {
    match file_type {
        FileType::RegularFile => Some(FileKind::File),
        FileType::Directory => Some(FileKind::Dir),
        FileType::Symlink => Some(FileKind::Symlink),
        FileType::Unknown => None,
        _ => Some(FileKind::Other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::test_dir::TestDir;
    use std::io::Read;

    fn name(s: &str) -> &OsStr {
        OsStr::new(s)
    }

    fn setup() -> (TestDir, Dir) {
        let tmp = TestDir::new();
        tmp.mkdir_all("sub");
        std::fs::write(tmp.path().join("file"), b"hello").unwrap();
        std::os::unix::fs::symlink("file", tmp.path().join("link")).unwrap();
        let dir = Dir::open(tmp.path()).unwrap();
        (tmp, dir)
    }

    #[test]
    fn open_dir_opens_only_real_directories() {
        let (_tmp, dir) = setup();
        open_dir(&dir, name("sub")).unwrap();
        assert!(open_dir(&dir, name("link")).is_err());
        assert!(open_dir(&dir, name("file")).is_err());
        assert!(open_dir(&dir, name("../x")).is_err());
    }

    #[test]
    fn stat_at_does_not_follow() {
        let (_tmp, dir) = setup();
        let file = stat_at(&dir, name("file")).unwrap();
        assert_eq!((file.kind, file.size), (FileKind::File, 5));
        assert_eq!(stat_at(&dir, name("link")).unwrap().kind, FileKind::Symlink);
        assert_eq!(stat_at(&dir, name("sub")).unwrap().kind, FileKind::Dir);
    }

    #[test]
    fn fstat_matches_stat_at() {
        let (_tmp, dir) = setup();
        let file = open_read(&dir, name("file")).unwrap();
        let (a, b) = (fstat(&file).unwrap(), stat_at(&dir, name("file")).unwrap());
        assert_eq!((a.dev, a.ino, a.kind), (b.dev, b.ino, b.kind));
    }

    #[test]
    fn entries_lists_names_without_dots() {
        let (_tmp, dir) = setup();
        let mut names: Vec<Vec<u8>> = entries(&dir).unwrap().into_iter().map(|e| e.name).collect();
        names.sort();
        assert_eq!(names, [b"file".to_vec(), b"link".to_vec(), b"sub".to_vec()]);
        // A second listing of the same handle sees everything again.
        assert_eq!(entries(&dir).unwrap().len(), 3);
    }

    #[test]
    fn read_link_at_returns_target() {
        let (_tmp, dir) = setup();
        assert_eq!(read_link_at(&dir, name("link"), 4).unwrap(), b"file");
        let err = read_link_at(&dir, name("link"), 3).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn open_read_nonblocking_refuses_symlink_and_reads_after_finish_read() {
        let (_tmp, dir) = setup();
        assert!(open_read(&dir, name("link")).is_err());
        let mut file = open_read(&dir, name("file")).unwrap();
        finish_read(&file).unwrap();
        assert!(
            !rustix::fs::fcntl_getfl(&file)
                .unwrap()
                .contains(OFlags::NONBLOCK)
        );
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        assert_eq!(text, "hello");
    }

    #[test]
    fn unlink_at_removes_links_files_and_directories() {
        let (tmp, dir) = setup();
        unlink_at(&dir, name("link"), false).unwrap();
        assert!(tmp.path().join("file").exists());
        unlink_at(&dir, name("file"), false).unwrap();
        unlink_at(&dir, name("sub"), true).unwrap();
        assert!(entries(&dir).unwrap().is_empty());
    }

    #[test]
    fn rename_at_moves_the_link_itself() {
        let (tmp, dir) = setup();
        let sub = open_dir(&dir, name("sub")).unwrap();
        rename_at(&dir, name("link"), &sub, name("moved")).unwrap();
        assert_eq!(
            std::fs::read_link(tmp.path().join("sub/moved"))
                .unwrap()
                .as_os_str(),
            "file"
        );
        assert!(tmp.path().join("file").is_file());
    }

    #[test]
    fn set_times_at_changes_the_link_not_the_target() {
        let (_tmp, dir) = setup();
        let target_before = stat_at(&dir, name("file")).unwrap().modified;
        let t = Timestamp {
            secs: 1_000_000_000,
            nanos: 0,
        };
        set_times_at(&dir, name("link"), t, t).unwrap();
        assert_eq!(stat_at(&dir, name("link")).unwrap().modified, t);
        assert_eq!(stat_at(&dir, name("file")).unwrap().modified, target_before);
    }
}
