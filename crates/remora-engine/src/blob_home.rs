//! Where a project's blobs live. On the data directory's volume they stay in the project's
//! store. A project on another volume gets a blob folder on its own volume, so its files can
//! be cloned instead of copied (clones never cross volumes):
//!
//! ```text
//! Unix     <anchor>/.remora-store/<project id>/blobs        <anchor>: see `pick_anchor`
//! Windows  <drive>:\.remora-store-<user SID>\<project id>\blobs
//! ```
//!
//! The private folder (`.remora-store` on Unix, `.remora-store-<user SID>` at the drive's root on Windows) is owner-only:
//! mode 0700 on Unix; on Windows owned by the user, with an `icacls` access list of the user and SYSTEM alone, re-applied at every open.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::EngineError;
#[cfg(not(windows))]
use crate::fsutil;

/// The folder created in the anchor.
pub(crate) const STORE_DIR: &str = ".remora-store";

/// The private folder at a drive's root on Windows: `.remora-store-<user SID>`.
#[cfg_attr(not(windows), allow(dead_code))]
fn private_name(sid: &str) -> String {
    format!("{STORE_DIR}-{sid}")
}

/// Whether every entry of a security descriptor's access list (SDDL, as `icacls /save`
/// writes it) is for `sid` or SYSTEM. An empty list counts as not private. SDDL writes the
/// built-in Administrator (RID 500) as the alias `LA`, so that trustee counts as `sid` when
/// `sid` ends in `-500`.
#[cfg_attr(not(windows), allow(dead_code))]
fn only_user_and_system(sddl: &str, sid: &str) -> bool {
    let Some(dacl) = sddl.split_once("D:").map(|(_, rest)| rest) else {
        return false;
    };
    // A SACL, if any, follows the DACL.
    let dacl = dacl.split_once("S:").map_or(dacl, |(d, _)| d);
    let trustees: Vec<&str> = dacl
        .split('(')
        .skip(1)
        .filter_map(|ace| ace.split(')').next())
        .map(|ace| ace.rsplit(';').next().unwrap_or(""))
        .collect();
    let built_in_administrator = sid.ends_with("-500");
    !trustees.is_empty()
        && trustees
            .iter()
            .all(|t| *t == sid || *t == "SY" || *t == "S-1-5-18" || (built_in_administrator && *t == "LA"))
}

/// Whether a project recorded in `data_dir` keeps its blobs under `private`: then Remora made
/// that folder for this user earlier, and it may be used again.
#[cfg_attr(not(windows), allow(dead_code))]
fn recorded_under(data_dir: &Path, private: &Path) -> bool {
    let Ok(projects) = fs::read_dir(data_dir.join("projects")) else {
        return false;
    };
    projects.flatten().any(|project| {
        fs::read(project.path().join("project.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|json| json.get("blobs")?.as_str().map(PathBuf::from))
            .is_some_and(|blobs| blobs.starts_with(private))
    })
}

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
    match prepare(&anchor, id, owner, data_dir) {
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
    // `<private>/<id>/blobs`.
    let private = blobs
        .ancestors()
        .nth(2)
        .ok_or_else(|| unavailable("it is not inside a private store folder".to_owned()))?;
    recheck_private(private, blobs, data_dir).map_err(unavailable)
}

/// Unix: `.remora-store`, owned by the data directory's owner, closed to others, in an
/// anchor other users may not write in.
#[cfg(not(windows))]
fn recheck_private(private: &Path, _blobs: &Path, data_dir: &Path) -> Result<(), String> {
    if private.file_name() != Some(std::ffi::OsStr::new(STORE_DIR)) {
        return Err(format!("it is not inside a {STORE_DIR} folder"));
    }
    let owner = owner_of(data_dir).map_err(|e| e.to_string())?;
    check_private(private, owner).map_err(|e| e.to_string())?;
    // Another user able to rename `.remora-store` in its anchor could redirect the blobs.
    #[cfg(unix)]
    if let Some(anchor) = private.parent() {
        use std::os::unix::fs::MetadataExt;
        let mode = fs::metadata(anchor).map_err(|e| e.to_string())?.mode();
        if open_to_others(mode) {
            return Err(format!("{} can be changed by other users", anchor.display()));
        }
    }
    Ok(())
}

/// Windows: this user's `.remora-store-<SID>`, a real folder, its access list re-applied
/// (whatever another user may have changed does not survive).
#[cfg(windows)]
fn recheck_private(private: &Path, blobs: &Path, _data_dir: &Path) -> Result<(), String> {
    let sid = crate::user_sid().ok_or_else(|| "cannot read the user's SID with whoami".to_owned())?;
    let name = private_name(&sid);
    if private.file_name() != Some(std::ffi::OsStr::new(&name)) {
        return Err(format!("it is not inside your {name} folder"));
    }
    for dir in [private, blobs.parent().unwrap_or(blobs), blobs] {
        check_private(dir, 0).map_err(|e| e.to_string())?;
    }
    run_icacls(&icacls_steps(private, &sid))
        .and_then(|()| verify_acl(private, &sid))
        .map_err(|e| e.to_string())
}

/// Create (or reuse) `<anchor>/.remora-store/<id>/blobs`, refusing a `.remora-store` that
/// is a symlink, someone else's, or open to others. An existing one is never loosened or
/// tightened, only checked.
#[cfg(not(windows))]
fn prepare(anchor: &Path, id: &str, owner: u32, _data_dir: &Path) -> io::Result<PathBuf> {
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

/// Create `<drive>:\.remora-store-<SID>` (or reuse it when Remora made it for this user
/// earlier), make it the user's alone, then create `<id>\blobs` in it.
#[cfg(windows)]
fn prepare(anchor: &Path, id: &str, _owner: u32, data_dir: &Path) -> io::Result<PathBuf> {
    let sid = crate::user_sid().ok_or_else(|| io::Error::other("cannot read the user's SID with whoami"))?;
    // At the drive's root, not inside a shared folder: other users may not rename what is
    // in the root unless they own it.
    let private = anchor.join(private_name(&sid));
    match fs::create_dir(&private) {
        Ok(()) => {}
        // Someone else's folder could hold their entries, junctions or open handles.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && !recorded_under(data_dir, &private) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} exists but was not made by Remora for you: delete it to let Remora use this drive",
                    private.display()
                ),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    check_private(&private, 0)?;
    run_icacls(&icacls_steps(&private, &sid))?;
    verify_acl(&private, &sid)?;
    let id_dir = private.join(id);
    let blobs = id_dir.join("blobs");
    for dir in [&id_dir, &blobs] {
        match fs::create_dir(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        check_private(dir, 0)?;
    }
    Ok(blobs)
}

/// Run each `icacls` step, failing on the first that fails.
#[cfg(windows)]
fn run_icacls(steps: &[Vec<OsString>]) -> io::Result<()> {
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    for args in steps {
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
    Ok(())
}

/// Read `dir`'s access list back in SID form (not localized names) and refuse it unless
/// only the user and SYSTEM are on it.
#[cfg(windows)]
fn verify_acl(dir: &Path, sid: &str) -> io::Result<()> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dump = std::env::temp_dir().join(format!("remora-acl-{}-{n}.txt", std::process::id()));
    let saved = run_icacls(&[vec![
        without_verbatim_prefix(dir).into_os_string(),
        "/save".into(),
        dump.clone().into_os_string(),
        "/q".into(),
    ]])
    .and_then(|()| fs::read(&dump));
    let _ = fs::remove_file(&dump);
    let bytes = saved?;
    // UTF-16LE: the folder's name on the first line, its SDDL on the second.
    let (pairs, _) = bytes.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|p| u16::from_le_bytes(*p)).collect();
    let text = String::from_utf16_lossy(&units);
    let sddl = text.trim_start_matches('\u{feff}').lines().nth(1).unwrap_or("");
    if only_user_and_system(sddl, sid) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is open to other users: {sddl}", dir.display()),
        ))
    }
}

/// The `icacls` runs making `dir` the user's and SYSTEM's alone: the user becomes its owner
/// (a folder made by an elevated process belongs to Administrators); inherited entries are
/// removed and full control granted to the two SIDs; the entries a folder can carry from its
/// drive or from an elevated creator (Administrators, CREATOR OWNER, Users, Everyone,
/// Authenticated Users) are removed. Nothing is recursive: only folders Remora made are ever
/// handled.
#[cfg_attr(not(windows), allow(dead_code))]
fn icacls_steps(dir: &Path, sid: &str) -> [Vec<OsString>; 3] {
    let dir = without_verbatim_prefix(dir).into_os_string();
    [
        vec![
            dir.clone(),
            "/setowner".into(),
            format!("*{sid}").into(),
            "/q".into(),
        ],
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
            "*S-1-5-32-545".into(),
            "*S-1-1-0".into(),
            "*S-1-5-11".into(),
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
#[allow(dead_code)]
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
            prepare(anchor.path(), "0123456789abcdef", owner, anchor.path()).is_err(),
            "open to others"
        );

        let elsewhere = tempfile::TempDir::new().unwrap();
        fs::remove_dir(&store_dir).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), &store_dir).unwrap();
        assert!(
            prepare(anchor.path(), "0123456789abcdef", owner, anchor.path()).is_err(),
            "a symlink"
        );
    }

    #[test]
    fn icacls_grants_the_user_and_system_alone() {
        let dir = Path::new(r"\\?\R:\.remora-store-S-1-5-21-1-2-3-1013");
        let steps: Vec<Vec<String>> = icacls_steps(dir, "S-1-5-21-1-2-3-1013")
            .into_iter()
            .map(|args| args.into_iter().map(|a| a.into_string().unwrap()).collect())
            .collect();
        assert_eq!(
            steps,
            [
                vec![
                    r"R:\.remora-store-S-1-5-21-1-2-3-1013",
                    "/setowner",
                    "*S-1-5-21-1-2-3-1013",
                    "/q",
                ],
                vec![
                    r"R:\.remora-store-S-1-5-21-1-2-3-1013",
                    "/inheritance:r",
                    "/grant:r",
                    "*S-1-5-21-1-2-3-1013:(OI)(CI)F",
                    "*S-1-5-18:(OI)(CI)F",
                    "/q",
                ],
                vec![
                    r"R:\.remora-store-S-1-5-21-1-2-3-1013",
                    "/remove:g",
                    "*S-1-5-32-544",
                    "*S-1-3-0",
                    "*S-1-5-32-545",
                    "*S-1-1-0",
                    "*S-1-5-11",
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

    #[test]
    fn only_the_user_and_system_may_be_on_the_store_folder() {
        let me = "S-1-5-21-1-2-3-1013";
        assert!(only_user_and_system(
            "O:S-1-5-21-1-2-3-1013D:PAI(A;OICI;FA;;;S-1-5-21-1-2-3-1013)(A;OICI;FA;;;SY)",
            me
        ));
        assert!(only_user_and_system(
            "D:P(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-21-1-2-3-1013)",
            me
        ));
        assert!(
            !only_user_and_system("D:PAI(A;OICI;FA;;;S-1-5-21-1-2-3-1013)(A;OICI;0x1200a9;;;BU)", me),
            "Users"
        );
        assert!(
            !only_user_and_system("D:PAI(A;OICI;FA;;;SY)(A;OICI;0x1200a9;;;WD)", me),
            "Everyone"
        );
        assert!(
            !only_user_and_system("D:PAI(A;OICI;FA;;;S-1-5-21-9-9-9-1001)", me),
            "another user"
        );
        assert!(!only_user_and_system("D:PAI", me), "an empty list");
        let admin = "S-1-5-21-1-2-3-500";
        assert!(
            only_user_and_system("O:LAD:PAI(A;OICI;FA;;;LA)(A;OICI;FA;;;SY)", admin),
            "the built-in Administrator's alias"
        );
        assert!(
            !only_user_and_system("D:PAI(A;OICI;FA;;;LA)(A;OICI;FA;;;SY)", me),
            "LA is not another RID's alias"
        );
        assert!(!only_user_and_system("garbage", me));
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn only_a_store_folder_a_recorded_project_uses_is_reused() {
        let data = tempfile::TempDir::new().unwrap();
        let private = PathBuf::from("/drive/.remora-store-S-1-5-21-1-2-3-1013");
        assert!(!recorded_under(data.path(), &private), "no projects at all");
        let project = data.path().join("projects").join("0123456789abcdef");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("project.json"), r#"{"root":"/p"}"#).unwrap();
        assert!(
            !recorded_under(data.path(), &private),
            "a project in the data directory's store"
        );
        fs::write(
            project.join("project.json"),
            serde_json::json!({"root": "/p", "blobs": private.join("0123456789abcdef").join("blobs")})
                .to_string(),
        )
        .unwrap();
        assert!(recorded_under(data.path(), &private));
        assert!(
            !recorded_under(data.path(), Path::new("/drive/.remora-store-S-1-5-21-9")),
            "another user's folder"
        );
    }
}
