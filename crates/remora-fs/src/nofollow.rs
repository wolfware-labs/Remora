//! Symlink-safe open, walk, unlink and rename (spec 5.5), the single owner of that
//! capability (spec 4). Every call works relative to an open directory and never
//! follows a symlink: a link is reported as a link, and a parent that stopped being
//! a real directory stops the operation.

use crate::Error;
use crate::sys::{Dir, at};
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStrExt;

/// The longest symlink target Remora reads (spec 7.11).
pub const MAX_LINK_TARGET: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    /// A FIFO, socket or device.
    Other,
}

/// Seconds and nanoseconds since the Unix epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp {
    pub secs: i64,
    pub nanos: u32,
}

/// What `stat_at` reports about an entry, without following a symlink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub kind: FileKind,
    /// Permission bits, including setuid, setgid and sticky.
    pub mode: u32,
    pub size: u64,
    pub modified: Timestamp,
    /// The inode change time.
    pub changed: Timestamp,
    /// The filesystem, so a walk can refuse to cross into another one.
    pub dev: u64,
    pub ino: u64,
}

/// One directory entry. The kind is a hint from the listing and can be stale or
/// missing; `stat_at` is authoritative.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The raw name; nothing here requires UTF-8.
    pub name: Vec<u8>,
    pub kind_hint: Option<FileKind>,
}

/// Opens the subdirectory `name` of `parent`. A symlink is `IsSymlink`, anything
/// else that is not a directory `NotADirectory`.
pub fn open_dir(parent: &Dir, name: &OsStr) -> Result<Dir, Error> {
    match at::open_dir(parent, name) {
        Ok(dir) => Ok(dir),
        // Linux reports a symlink as ENOTDIR here, macOS as ELOOP; ask the entry.
        Err(e) if is_not_dir_or_loop(&e) => Err(classify(parent, name, e)),
        Err(e) => Err(e.into()),
    }
}

/// The entries of `dir`, unsorted, without `.` and `..`.
pub fn entries(dir: &Dir) -> Result<Vec<Entry>, Error> {
    Ok(at::entries(dir)?)
}

/// The metadata of `name` in `parent`; a symlink is described, not followed.
pub fn stat_at(parent: &Dir, name: &OsStr) -> Result<Metadata, Error> {
    Ok(at::stat_at(parent, name)?)
}

/// The raw target of the symlink `name`, at most [`MAX_LINK_TARGET`] bytes.
pub fn read_link_at(parent: &Dir, name: &OsStr) -> Result<Vec<u8>, Error> {
    let target = at::read_link_at(parent, name)?;
    if target.len() > MAX_LINK_TARGET {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("symlink target longer than {MAX_LINK_TARGET} bytes"),
        )
        .into());
    }
    Ok(target)
}

/// Opens the regular file `name` for reading, if it is still the file `expected`
/// describes: a regular file with the same `dev` and `ino`. Anything else, a FIFO
/// included, is `Changed`; a symlink is `IsSymlink`. Never blocks on the open.
pub fn open_file_read(parent: &Dir, name: &OsStr, expected: &Metadata) -> Result<File, Error> {
    let file = match at::open_read_nonblocking(parent, name) {
        Ok(file) => file,
        Err(e) if e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) => {
            return Err(Error::IsSymlink);
        }
        Err(e) => return Err(e.into()),
    };
    let actual = at::fstat(&file)?;
    if actual.kind != FileKind::File || (actual.dev, actual.ino) != (expected.dev, expected.ino) {
        return Err(Error::Changed);
    }
    at::set_blocking(&file)?;
    Ok(file)
}

/// Removes `name` from `parent`: the entry itself, a symlink included. `is_dir`
/// removes an empty directory.
pub fn unlink_at(parent: &Dir, name: &OsStr, is_dir: bool) -> Result<(), Error> {
    Ok(at::unlink_at(parent, name, is_dir)?)
}

/// Renames `from` in `from_dir` to `to` in `to_dir`, replacing a file there. A
/// symlink is renamed itself.
pub fn rename_at(from_dir: &Dir, from: &OsStr, to_dir: &Dir, to: &OsStr) -> Result<(), Error> {
    Ok(at::rename_at(from_dir, from, to_dir, to)?)
}

/// Sets the access and modification times of `name` itself, a symlink's own times
/// included (spec 5.5 rule 7).
pub fn set_times_at(
    parent: &Dir,
    name: &OsStr,
    accessed: Timestamp,
    modified: Timestamp,
) -> Result<(), Error> {
    Ok(at::set_times_at(parent, name, accessed, modified)?)
}

/// Re-opens the parent chain of `key` (a `/`-separated path relative to `root`)
/// one component at a time and returns the parent directory and the last name.
/// A component that is not a real directory is `UnsafeParent` with the key prefix
/// up to it.
pub fn reopen_parent<'k>(root: &Dir, key: &'k [u8]) -> Result<(Dir, &'k OsStr), Error> {
    let components: Vec<&[u8]> = key.split(|&b| b == b'/').collect();
    if components
        .iter()
        .any(|c| c.is_empty() || *c == b"." || *c == b"..")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a normalized key: {}", String::from_utf8_lossy(key)),
        )
        .into());
    }
    let (last, parents) = components.split_last().ok_or(Error::NotADirectory)?;
    let mut dir: Option<Dir> = None;
    let mut prefix_len = 0;
    for component in parents {
        prefix_len += component.len() + usize::from(prefix_len > 0);
        let parent = dir.as_ref().unwrap_or(root);
        dir = Some(match open_dir(parent, OsStr::from_bytes(component)) {
            Ok(next) => next,
            Err(Error::IsSymlink | Error::NotADirectory) => {
                return Err(Error::UnsafeParent(key[..prefix_len].to_vec()));
            }
            Err(e) => return Err(e),
        });
    }
    let dir = match dir {
        Some(dir) => dir,
        None => root.try_clone()?,
    };
    Ok((dir, OsStr::from_bytes(last)))
}

/// Deletes `name` in `parent` and everything below it, depth-first through
/// descriptors. A symlink anywhere in the tree is unlinked, never followed.
pub fn remove_tree(parent: &Dir, name: &OsStr) -> Result<(), Error> {
    let top = match open_dir(parent, name) {
        Ok(dir) => dir,
        Err(Error::IsSymlink | Error::NotADirectory) => return unlink_at(parent, name, false),
        Err(e) => return Err(e),
    };
    // Each level holds its directory and its name in the level above; the stack
    // keeps a deep tree from exhausting the thread stack.
    let mut stack: Vec<(Dir, Vec<u8>)> = vec![(top, name.as_bytes().to_vec())];
    while let Some((dir, _)) = stack.last() {
        let mut subdir = None;
        for entry in at::entries(dir)? {
            let child = OsStr::from_bytes(&entry.name);
            match open_dir(dir, child) {
                Ok(child_dir) => {
                    subdir = Some((child_dir, entry.name));
                    break;
                }
                Err(Error::IsSymlink | Error::NotADirectory) => unlink_at(dir, child, false)?,
                Err(e) => return Err(e),
            }
        }
        match subdir {
            Some(next) => stack.push(next),
            None => {
                let (_, done) = stack.pop().ok_or(Error::NotADirectory)?;
                let above = stack.last().map_or(parent, |(dir, _)| dir);
                unlink_at(above, OsStr::from_bytes(&done), true)?;
            }
        }
    }
    Ok(())
}

fn is_not_dir_or_loop(e: &io::Error) -> bool {
    use rustix::io::Errno;
    let code = e.raw_os_error();
    code == Some(Errno::NOTDIR.raw_os_error()) || code == Some(Errno::LOOP.raw_os_error())
}

fn classify(parent: &Dir, name: &OsStr, original: io::Error) -> Error {
    match at::stat_at(parent, name) {
        Ok(meta) if meta.kind == FileKind::Symlink => Error::IsSymlink,
        Ok(_) => Error::NotADirectory,
        Err(_) => Error::Io(original),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::test_dir::TestDir;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    fn name(s: &str) -> &OsStr {
        OsStr::new(s)
    }

    /// Builds a tree under a fresh directory: entries ending in `/` are directories,
    /// `a -> b` is a symlink, anything else a file holding its own name.
    fn tree(spec: &[&str]) -> TestDir {
        let tmp = TestDir::new();
        for item in spec {
            if let Some(dir) = item.strip_suffix('/') {
                tmp.mkdir_all(dir);
            } else if let Some((link, target)) = item.split_once(" -> ") {
                symlink(target, tmp.path().join(link)).unwrap();
            } else {
                std::fs::write(tmp.path().join(item), item.as_bytes()).unwrap();
            }
        }
        tmp
    }

    /// Lists every key under `dir` through the API, without following links.
    fn walk(dir: &Dir, prefix: &str, out: &mut Vec<String>) {
        for entry in entries(dir).unwrap() {
            let child = String::from_utf8(entry.name).unwrap();
            let key = format!("{prefix}{child}");
            if stat_at(dir, name(&child)).unwrap().kind == FileKind::Dir {
                walk(
                    &open_dir(dir, name(&child)).unwrap(),
                    &format!("{key}/"),
                    out,
                );
            }
            out.push(key);
        }
    }

    #[test]
    fn walk_does_not_follow_dir_symlink() {
        let outside = tree(&["secret/", "secret/sentinel"]);
        let tmp = tree(&["src/", "src/main.rs"]);
        symlink(outside.path().join("secret"), tmp.path().join("src/escape")).unwrap();
        let root = Dir::open(tmp.path()).unwrap();
        let mut keys = Vec::new();
        walk(&root, "", &mut keys);
        keys.sort();
        assert_eq!(keys, ["src", "src/escape", "src/main.rs"]);
        let src = open_dir(&root, name("src")).unwrap();
        assert!(matches!(
            open_dir(&src, name("escape")),
            Err(Error::IsSymlink)
        ));
    }

    #[test]
    fn open_dir_classifies_links_and_files() {
        let tmp = tree(&["dir/", "file", "link -> dir", "dangling -> nowhere"]);
        let root = Dir::open(tmp.path()).unwrap();
        assert!(matches!(
            open_dir(&root, name("link")),
            Err(Error::IsSymlink)
        ));
        assert!(matches!(
            open_dir(&root, name("dangling")),
            Err(Error::IsSymlink)
        ));
        assert!(matches!(
            open_dir(&root, name("file")),
            Err(Error::NotADirectory)
        ));
        assert!(
            matches!(open_dir(&root, name("missing")), Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn swap_file_for_symlink_before_open() {
        let tmp = tree(&["file", "other"]);
        let root = Dir::open(tmp.path()).unwrap();
        let expected = stat_at(&root, name("file")).unwrap();
        std::fs::remove_file(tmp.path().join("file")).unwrap();
        symlink("other", tmp.path().join("file")).unwrap();
        let result = open_file_read(&root, name("file"), &expected);
        assert!(
            matches!(result, Err(Error::IsSymlink | Error::Changed)),
            "{result:?}"
        );
    }

    #[test]
    fn swap_file_for_other_file_before_open() {
        let tmp = tree(&["file", "other"]);
        let root = Dir::open(tmp.path()).unwrap();
        let expected = stat_at(&root, name("file")).unwrap();
        std::fs::rename(tmp.path().join("other"), tmp.path().join("file")).unwrap();
        assert!(matches!(
            open_file_read(&root, name("file"), &expected),
            Err(Error::Changed)
        ));
    }

    #[test]
    fn open_file_read_reads_an_unchanged_file() {
        use std::io::Read;
        let tmp = tree(&["file"]);
        let root = Dir::open(tmp.path()).unwrap();
        let expected = stat_at(&root, name("file")).unwrap();
        let mut text = String::new();
        open_file_read(&root, name("file"), &expected)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "file");
    }

    // rustix has no mkfifoat on macOS, and spec 3.5 keeps libc to a fixed list of
    // calls; the code under test is the same on both.
    #[cfg(target_os = "linux")]
    #[test]
    fn fifo_open_does_not_block() {
        let tmp = TestDir::new();
        let root = Dir::open(tmp.path()).unwrap();
        rustix::fs::mkfifoat(
            &root,
            "fifo",
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        let expected = stat_at(&root, name("fifo")).unwrap();
        assert_eq!(expected.kind, FileKind::Other);
        assert!(matches!(
            open_file_read(&root, name("fifo"), &expected),
            Err(Error::Changed)
        ));
    }

    #[test]
    fn reopen_parent_returns_the_parent_and_name() {
        let tmp = tree(&["src/lib/", "src/lib/mod.rs"]);
        let root = Dir::open(tmp.path()).unwrap();
        let (dir, last) = reopen_parent(&root, b"src/lib/mod.rs").unwrap();
        assert_eq!(last, "mod.rs");
        assert_eq!(stat_at(&dir, last).unwrap().kind, FileKind::File);
        let (dir, last) = reopen_parent(&root, b"top").unwrap();
        assert_eq!(last, "top");
        assert!(entries(&dir).unwrap().iter().any(|e| e.name == b"src"));
    }

    #[test]
    fn reopen_parent_refuses_symlink_component() {
        let outside = tree(&["lib/"]);
        let tmp = tree(&["src/"]);
        symlink(outside.path().join("lib"), tmp.path().join("src/lib")).unwrap();
        let root = Dir::open(tmp.path()).unwrap();
        match reopen_parent(&root, b"src/lib/mod.rs") {
            Err(Error::UnsafeParent(prefix)) => assert_eq!(prefix, b"src/lib"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reopen_parent_refuses_file_component() {
        let tmp = tree(&["src"]);
        let root = Dir::open(tmp.path()).unwrap();
        match reopen_parent(&root, b"src/mod.rs") {
            Err(Error::UnsafeParent(prefix)) => assert_eq!(prefix, b"src"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reopen_parent_refuses_unnormalized_keys() {
        let tmp = tree(&["src/"]);
        let root = Dir::open(tmp.path()).unwrap();
        for key in [&b"../x"[..], b"src/../x", b"./x", b"src//x", b"", b"/abs"] {
            assert!(
                matches!(reopen_parent(&root, key), Err(Error::Io(_))),
                "{key:?}"
            );
        }
    }

    #[test]
    fn rename_and_unlink_act_on_link() {
        let tmp = tree(&["file", "link -> file"]);
        let root = Dir::open(tmp.path()).unwrap();
        rename_at(&root, name("link"), &root, name("moved")).unwrap();
        assert_eq!(
            stat_at(&root, name("moved")).unwrap().kind,
            FileKind::Symlink
        );
        unlink_at(&root, name("moved"), false).unwrap();
        assert_eq!(std::fs::read(tmp.path().join("file")).unwrap(), b"file");
    }

    #[test]
    fn read_link_at_returns_raw_target() {
        let tmp = TestDir::new();
        #[cfg(target_os = "linux")]
        let target: &[u8] = b"caf\xff/target";
        #[cfg(not(target_os = "linux"))]
        let target: &[u8] = b"cafe/target";
        symlink(OsStr::from_bytes(target), tmp.path().join("link")).unwrap();
        let root = Dir::open(tmp.path()).unwrap();
        assert_eq!(read_link_at(&root, name("link")).unwrap(), target);
    }

    // APFS refuses names that are not UTF-8.
    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_names_listed_as_bytes() {
        let tmp = TestDir::new();
        let raw: &[u8] = b"caf\xff";
        std::fs::write(tmp.path().join(OsStr::from_bytes(raw)), b"").unwrap();
        let root = Dir::open(tmp.path()).unwrap();
        let listed = entries(&root).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, raw);
        assert_eq!(
            stat_at(&root, OsStr::from_bytes(raw)).unwrap().kind,
            FileKind::File
        );
    }

    #[test]
    fn remove_tree_does_not_follow_links() {
        let outside = tree(&["keep/", "keep/sentinel"]);
        let tmp = tree(&[
            "doomed/",
            "doomed/a/",
            "doomed/a/b/",
            "doomed/a/b/file",
            "doomed/top",
        ]);
        symlink(
            outside.path().join("keep"),
            tmp.path().join("doomed/a/escape"),
        )
        .unwrap();
        symlink(
            outside.path().join("keep"),
            tmp.path().join("doomed/a/b/escape"),
        )
        .unwrap();
        let root = Dir::open(tmp.path()).unwrap();
        remove_tree(&root, name("doomed")).unwrap();
        assert!(!tmp.path().join("doomed").exists());
        assert_eq!(
            std::fs::read(outside.path().join("keep/sentinel")).unwrap(),
            b"keep/sentinel"
        );
        // A symlink or a file as the top entry is removed itself.
        symlink(outside.path().join("keep"), tmp.path().join("top-link")).unwrap();
        remove_tree(&root, name("top-link")).unwrap();
        assert!(outside.path().join("keep").is_dir());
    }

    #[test]
    fn set_times_at_on_symlink_leaves_target() {
        let tmp = tree(&["file", "link -> file"]);
        let root = Dir::open(tmp.path()).unwrap();
        let before = stat_at(&root, name("file")).unwrap().modified;
        let t = Timestamp {
            secs: 1_234_567_890,
            nanos: 5,
        };
        set_times_at(&root, name("link"), t, t).unwrap();
        assert_eq!(stat_at(&root, name("link")).unwrap().modified, t);
        assert_eq!(stat_at(&root, name("file")).unwrap().modified, before);
    }

    /// On the CI `fs` jobs `REMORA_TEST_FS` is a separate mount.
    #[test]
    fn other_filesystem_dev_reported() {
        let Some(fs_root) = std::env::var_os("REMORA_TEST_FS") else {
            eprintln!("REMORA_TEST_FS is not set; skipping the cross-filesystem check");
            return;
        };
        let dev_of = |path: &Path| {
            let parent = Dir::open(path.parent().unwrap()).unwrap();
            stat_at(&parent, path.file_name().unwrap()).unwrap().dev
        };
        let on_test_fs = TestDir::new();
        assert!(on_test_fs.path().starts_with(&fs_root));
        let elsewhere = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        assert_ne!(dev_of(on_test_fs.path()), dev_of(&elsewhere));
    }
}
