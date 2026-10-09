//! Where a project's blobs live. On the data directory's volume they stay in the project's
//! store. A project on another volume gets a blob folder on its own volume, so its files can
//! be cloned instead of copied (clones never cross volumes):
//!
//! ```text
//! Unix     <anchor>/.remora-store/<project id>/blobs        <anchor>: see `pick_anchor`
//! Windows  <drive>:\.remora-store\<user SID>\<project id>\blobs
//! ```
//!
//! The private folder (`.remora-store` on Unix, `<user SID>` on Windows) is owner-only:
//! mode 0700 on Unix, an `icacls` grant to the user and SYSTEM alone on Windows.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::EngineError;
#[cfg(not(windows))]
use crate::fsutil;

/// The folder created in the anchor.
pub(crate) const STORE_DIR: &str = ".remora-store";

/// One ancestor of a project root: its path, volume and owner (Unix; on Windows the anchor
/// is the drive's root).
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ancestor {
    pub path: PathBuf,
    pub volume: u64,
    pub owner: u32,
    /// Permission bits (Unix).
    pub mode: u32,
}

/// Whether a folder with `mode` lets other users rename what is inside it: writable by
/// group or others, without the sticky bit that would stop them.
#[cfg_attr(not(unix), allow(dead_code))]
fn open_to_others(mode: u32) -> bool {
    mode & 0o022 != 0 && mode & 0o1000 == 0
}

/// The anchor for a project on `volume`: the highest of `ancestors` (the project's parent
/// first, then upward) still on that volume and owned by `owner`. Never the project root
/// itself, so the store is never inside the project. A folder other users may write in
/// without the sticky bit is skipped: they could replace `.remora-store` in it.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn pick_anchor(ancestors: &[Ancestor], volume: u64, owner: u32) -> Option<PathBuf> {
    ancestors
        .iter()
        .take_while(|a| a.volume == volume)
        .filter(|a| a.owner == owner && !open_to_others(a.mode))
        .last()
        .map(|a| a.path.clone())
}

/// The blob folder for a new project rooted at `root` (canonical): `None` when the project
/// is on the data directory's volume or no folder on its own volume can be used (the reason
/// is logged); the store in the data directory is used then.
pub(crate) fn choose(root: &Path, data_dir: &Path, id: &str) -> Option<PathBuf> {
    let (anchor, owner) = match anchor(root, data_dir) {
        Ok(found) => found?,
        Err(e) => {
            log::warn!(
                "cannot look for a blob store on the volume of {}: {e}",
                root.display()
            );
            return None;
        }
    };
    match prepare(&anchor, id, owner) {
        Ok(blobs) => Some(blobs),
        Err(e) => {
            log::warn!(
                "cannot use {} for the blobs of {}, they stay in the data directory: {e}",
                anchor.join(STORE_DIR).display(),
                root.display()
            );
            None
        }
    }
}

/// Check a recorded blob folder before use: it must exist (a missing one means its volume is
/// not mounted, and switching to another would lose the blobs already there) and its
/// `.remora-store` must still be private to the data directory's owner.
pub(crate) fn check(blobs: &Path, data_dir: &Path) -> Result<(), EngineError> {
    let unavailable = |reason: String| EngineError::BlobStoreUnavailable {
        path: blobs.to_path_buf(),
        reason,
    };
    if !blobs.is_dir() {
        return Err(unavailable(
            "it does not exist (is its volume mounted?)".to_owned(),
        ));
    }
    // `<private>/<id>/blobs`, with `<private>` inside `.remora-store` on Windows.
    let private = blobs
        .ancestors()
        .nth(2)
        .filter(|dir| {
            let store_dir = if cfg!(windows) { dir.parent() } else { Some(*dir) };
            store_dir
                .and_then(Path::file_name)
                .is_some_and(|name| name == STORE_DIR)
        })
        .ok_or_else(|| unavailable(format!("it is not inside a {STORE_DIR} folder")))?;
    let owner = owner_of(data_dir).map_err(|e| unavailable(e.to_string()))?;
    check_private(private, owner).map_err(|e| unavailable(e.to_string()))?;
    // Another user able to rename `.remora-store` in its anchor could redirect the blobs.
    #[cfg(unix)]
    if let Some(anchor) = private.parent() {
        use std::os::unix::fs::MetadataExt;
        let mode = fs::metadata(anchor)
            .map_err(|e| unavailable(e.to_string()))?
            .mode();
        if open_to_others(mode) {
            return Err(unavailable(format!(
                "{} can be changed by other users",
                anchor.display()
            )));
        }
    }
    Ok(())
}

/// Create (or reuse) `<anchor>/.remora-store/<id>/blobs`, refusing a `.remora-store` that
/// is a symlink, someone else's, or open to others. An existing one is never loosened or
/// tightened, only checked.
#[cfg(not(windows))]
fn prepare(anchor: &Path, id: &str, owner: u32) -> io::Result<PathBuf> {
    let store_dir = anchor.join(STORE_DIR);
    match create_private(&store_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    check_private(&store_dir, owner)?;
    let blobs = store_dir.join(id).join("blobs");
    fsutil::private_dir(&blobs)?;
    Ok(blobs)
}

/// Create (or reuse) `<drive>:\.remora-store\<SID>\<id>\blobs`, with the `<SID>` folder
/// granted to the user and SYSTEM alone.
#[cfg(windows)]
fn prepare(anchor: &Path, id: &str, _owner: u32) -> io::Result<PathBuf> {
    let sid = crate::user_sid().ok_or_else(|| io::Error::other("cannot read the user's SID with whoami"))?;
    let store_dir = anchor.join(STORE_DIR);
    let private = store_dir.join(&sid);
    for dir in [&store_dir, &private] {
        match fs::create_dir(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    check_private(&private, 0)?;
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    for args in icacls_steps(&private, &sid) {
        let status = std::process::Command::new(Path::new(&system_root).join(r"System32\icacls.exe"))
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!("icacls failed: {status}")));
        }
    }
    let blobs = private.join(id).join("blobs");
    fs::create_dir_all(&blobs)?;
    Ok(blobs)
}

/// The `icacls` runs making `dir` (and what is created in it) the user's and SYSTEM's alone:
/// inherited entries removed and full control granted to the two SIDs, then the entries a
/// folder may get when an elevated process creates it (Administrators, CREATOR OWNER)
/// removed.
#[cfg_attr(not(windows), allow(dead_code))]
fn icacls_steps(dir: &Path, sid: &str) -> [Vec<OsString>; 2] {
    let dir = without_verbatim_prefix(dir).into_os_string();
    [
        vec![
            dir.clone(),
            "/inheritance:r".into(),
            "/grant:r".into(),
            format!("*{sid}:(OI)(CI)F").into(),
            "*S-1-5-18:(OI)(CI)F".into(),
            "/q".into(),
        ],
        vec![
            dir,
            "/remove:g".into(),
            "*S-1-5-32-544".into(),
            "*S-1-3-0".into(),
            "/q".into(),
        ],
    ]
}

/// A verbatim `\\?\R:\x` as `R:\x`: tools such as `icacls` do not take verbatim paths.
#[cfg_attr(not(windows), allow(dead_code))]
fn without_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// The anchor for `root` and the owner it must have, if `root` is on another volume than
/// `data_dir`.
#[cfg(unix)]
fn anchor(root: &Path, data_dir: &Path) -> io::Result<Option<(PathBuf, u32)>> {
    use std::os::unix::fs::MetadataExt;
    let volume = fs::metadata(root)?.dev();
    let data = fs::metadata(data_dir)?;
    if data.dev() == volume {
        return Ok(None);
    }
    let ancestors: Vec<Ancestor> = root
        .ancestors()
        .skip(1)
        .map_while(|path| {
            fs::metadata(path).ok().map(|m| Ancestor {
                path: path.to_path_buf(),
                volume: m.dev(),
                owner: m.uid(),
                mode: m.mode(),
            })
        })
        .collect();
    Ok(pick_anchor(&ancestors, volume, data.uid()).map(|anchor| (anchor, data.uid())))
}

/// On Windows, a project on another drive than `data_dir` is anchored at its drive's root.
/// A project at a drive's root itself, or on a network share, keeps the data directory's
/// store.
#[cfg(windows)]
fn anchor(root: &Path, data_dir: &Path) -> io::Result<Option<(PathBuf, u32)>> {
    let (Some(drive), Some(data_drive)) = (drive_of(root), drive_of(data_dir)) else {
        return Ok(None);
    };
    if drive == data_drive {
        return Ok(None);
    }
    let anchor = root.ancestors().last().unwrap_or(root).to_path_buf();
    Ok((anchor != root).then_some((anchor, 0)))
}

/// The drive letter of an absolute Windows path, upper-cased.
#[cfg(windows)]
fn drive_of(path: &Path) -> Option<u8> {
    use std::path::{Component, Prefix};
    match path.components().next()? {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::VerbatimDisk(d) | Prefix::Disk(d) => Some(d.to_ascii_uppercase()),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(not(any(unix, windows)))]
fn anchor(_root: &Path, _data_dir: &Path) -> io::Result<Option<(PathBuf, u32)>> {
    Ok(None)
}

#[cfg(unix)]
fn owner_of(path: &Path) -> io::Result<u32> {
    use std::os::unix::fs::MetadataExt;
    Ok(fs::metadata(path)?.uid())
}

#[cfg(not(unix))]
fn owner_of(_path: &Path) -> io::Result<u32> {
    Ok(0)
}

#[cfg(unix)]
fn create_private(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(dir)
}

#[cfg(not(any(unix, windows)))]
fn create_private(dir: &Path) -> io::Result<()> {
    fs::create_dir(dir)
}

/// A real folder (not a symlink), owned by `owner`, closed to everyone else.
#[cfg(unix)]
fn check_private(dir: &Path, owner: u32) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != owner || meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} must be your own folder, closed to other users", dir.display()),
        ));
    }
    Ok(())
}

/// A real folder, not a symlink or junction (its access is set by `icacls`).
#[cfg(not(unix))]
fn check_private(dir: &Path, _owner: u32) -> io::Result<()> {
    if !fs::symlink_metadata(dir)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} must be a folder", dir.display()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(path: &str, volume: u64, owner: u32) -> Ancestor {
        Ancestor {
            path: PathBuf::from(path),
            volume,
            owner,
            mode: 0o755,
        }
    }

    #[test]
    fn the_anchor_is_the_highest_own_folder_on_the_project_volume() {
        // /mnt/work is a btrfs mount (volume 2) the user owns; / is volume 1.
        let ancestors = [
            at("/mnt/work/me/src", 2, 1000),
            at("/mnt/work/me", 2, 1000),
            at("/mnt/work", 2, 1000),
            at("/mnt", 1, 0),
            at("/", 1, 0),
        ];
        assert_eq!(pick_anchor(&ancestors, 2, 1000), Some(PathBuf::from("/mnt/work")));
    }

    #[test]
    fn folders_of_other_users_are_skipped_and_other_volumes_end_the_search() {
        let root_owned_mount = [
            at("/mnt/work/me", 2, 1000),
            at("/mnt/work", 2, 0),
            at("/mnt", 1, 1000),
        ];
        assert_eq!(
            pick_anchor(&root_owned_mount, 2, 1000),
            Some(PathBuf::from("/mnt/work/me"))
        );
        let none_owned = [at("/srv/shared", 2, 0), at("/srv", 1, 1000)];
        assert_eq!(pick_anchor(&none_owned, 2, 1000), None);
        assert_eq!(pick_anchor(&[], 2, 1000), None, "a project at the volume's root");
        // Nested mounts: the project's volume shows up again above another one, which ends
        // the search before it.
        let nested = [
            at("/mnt/x/me", 2, 1000),
            at("/mnt/x", 1, 1000),
            at("/mnt", 2, 1000),
        ];
        assert_eq!(pick_anchor(&nested, 2, 1000), Some(PathBuf::from("/mnt/x/me")));
    }

    #[test]
    fn a_folder_others_may_write_in_is_never_the_anchor() {
        let open = Ancestor {
            mode: 0o777,
            ..at("/mnt/work", 2, 1000)
        };
        let sticky = Ancestor {
            mode: 0o1777,
            ..at("/mnt/work", 2, 1000)
        };
        let mine = at("/mnt/work/me", 2, 1000);
        assert_eq!(
            pick_anchor(&[mine.clone(), open], 2, 1000),
            Some(PathBuf::from("/mnt/work/me"))
        );
        assert_eq!(
            pick_anchor(&[mine, sticky], 2, 1000),
            Some(PathBuf::from("/mnt/work"))
        );
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::unwrap_used)]
    fn an_open_or_symlinked_store_folder_is_refused_when_first_chosen() {
        use std::os::unix::fs::PermissionsExt;
        let anchor = tempfile::TempDir::new().unwrap();
        let owner = owner_of(anchor.path()).unwrap();
        let store_dir = anchor.path().join(STORE_DIR);
        fs::create_dir(&store_dir).unwrap();
        fs::set_permissions(&store_dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            prepare(anchor.path(), "0123456789abcdef", owner).is_err(),
            "open to others"
        );

        let elsewhere = tempfile::TempDir::new().unwrap();
        fs::remove_dir(&store_dir).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), &store_dir).unwrap();
        assert!(
            prepare(anchor.path(), "0123456789abcdef", owner).is_err(),
            "a symlink"
        );
    }

    #[test]
    fn icacls_grants_the_user_and_system_alone() {
        let dir = Path::new(r"\\?\R:\.remora-store\S-1-5-21-1-2-3-1013");
        let steps: Vec<Vec<String>> = icacls_steps(dir, "S-1-5-21-1-2-3-1013")
            .into_iter()
            .map(|args| args.into_iter().map(|a| a.into_string().unwrap()).collect())
            .collect();
        assert_eq!(
            steps,
            [
                vec![
                    r"R:\.remora-store\S-1-5-21-1-2-3-1013",
                    "/inheritance:r",
                    "/grant:r",
                    "*S-1-5-21-1-2-3-1013:(OI)(CI)F",
                    "*S-1-5-18:(OI)(CI)F",
                    "/q",
                ],
                vec![
                    r"R:\.remora-store\S-1-5-21-1-2-3-1013",
                    "/remove:g",
                    "*S-1-5-32-544",
                    "*S-1-3-0",
                    "/q",
                ],
            ]
        );
        assert_eq!(
            without_verbatim_prefix(Path::new(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\?\UNC\server\share"),
            "network paths are left alone"
        );
    }
}
