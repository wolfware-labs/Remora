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
//! It holds a `.gitignore` with `*`, so a git repository around it does not list it.

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

/// The owner a security descriptor's SDDL names (`O:<sid>`), if any.
#[cfg_attr(not(windows), allow(dead_code))]
fn sddl_owner(sddl: &str) -> Option<&str> {
    let rest = sddl.trim().strip_prefix("O:")?;
    let end = ["G:", "D:", "S:"]
        .iter()
        .filter_map(|mark| rest.find(mark))
        .min()
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Whether a `.remora-store-<SID>` that no recorded project uses may be adopted: a crash
/// after Remora made and secured it, before the first project was recorded, leaves it owned
/// by the user, open to the user and SYSTEM alone, holding at most its `.gitignore`.
/// Anything else may be another user's doing and stays refused.
#[cfg_attr(not(windows), allow(dead_code))]
fn adoptable(sddl: &str, sid: &str, entries: &[String]) -> bool {
    let built_in_administrator = sid.ends_with("-500");
    let owned =
        sddl_owner(sddl).is_some_and(|owner| owner == sid || (built_in_administrator && owner == "LA"));
    owned && only_user_and_system(sddl, sid) && entries.iter().all(|entry| entry == ".gitignore")
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

/// Whether a folder owned by `uid` with `mode` can be trusted to stay in place for `owner`
/// (like ssh's StrictModes): no other user may rename what is inside it, and it belongs to
/// `owner` or to root.
#[cfg_attr(not(unix), allow(dead_code))]
fn safe_ancestor(mode: u32, uid: u32, owner: u32) -> bool {
    !open_to_others(mode) && (uid == owner || uid == 0)
}

impl Ancestor {
    #[cfg_attr(not(unix), allow(dead_code))]
    fn is_safe(&self, owner: u32) -> bool {
        safe_ancestor(self.mode, self.owner, owner)
    }
}

/// The anchor for a project on `volume`: the highest of `ancestors` (the project's parent
/// first, then upward to `/`, across volumes) still on that volume and owned by `owner`.
/// Never the project root itself, so the store is never inside the project. Every ancestor
/// above the anchor must be safe (see `safe_ancestor`): otherwise another user could rename
/// the anchor away and put their own folder in its place. The search stops looking upward at
/// the first unsafe ancestor.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn pick_anchor(ancestors: &[Ancestor], volume: u64, owner: u32) -> Option<PathBuf> {
    // Only ancestors from `trusted` upward have a safe chain above them.
    let trusted = ancestors
        .iter()
        .rposition(|a| !a.is_safe(owner))
        .map_or(0, |unsafe_at| unsafe_at + 1);
    ancestors
        .iter()
        .enumerate()
        .take_while(|(_, a)| a.volume == volume)
        .filter(|(i, a)| *i >= trusted && a.owner == owner && !open_to_others(a.mode))
        .last()
        .map(|(_, a)| a.path.clone())
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
    // project.json records the folder as a string. The rest of the path is ASCII.
    if anchor.to_str().is_none() {
        log::warn!(
            "the blob store folder on the volume of {} is not valid UTF-8, the blobs stay in the data directory",
            root.display()
        );
        return None;
    }
    match prepare(&anchor, id, owner, data_dir) {
        Ok(blobs) => Some(blobs),
        Err(e) => {
            log::warn!(
                "cannot use a private folder in {} for the blobs of {}, they stay in the data directory: {e}",
                anchor.display(),
                root.display()
            );
            None
        }
    }
}

/// Check a recorded blob folder before use: it must exist (a missing one means its volume is
/// not mounted, and switching to another would lose the blobs already there) and its private
/// folder (`.remora-store` on Unix, this user's `.remora-store-<SID>` on Windows) must still
/// be private to the user.
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

/// Unix: `.remora-store`, owned by the data directory's owner, closed to others, in a chain of
/// folders up to `/` that other users may not change (see `safe_ancestor`).
#[cfg(not(windows))]
fn recheck_private(private: &Path, _blobs: &Path, data_dir: &Path) -> Result<(), String> {
    if private.file_name() != Some(std::ffi::OsStr::new(STORE_DIR)) {
        return Err(format!("it is not inside a {STORE_DIR} folder"));
    }
    let owner = owner_of(data_dir).map_err(|e| e.to_string())?;
    check_private(private, owner).map_err(|e| e.to_string())?;
    // Another user able to rename `.remora-store`, or a folder above it, could redirect the blobs.
    #[cfg(unix)]
    for dir in private.ancestors().skip(1) {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(dir).map_err(|e| e.to_string())?;
        if !safe_ancestor(meta.mode(), meta.uid(), owner) {
            return Err(format!("{} can be changed by other users", dir.display()));
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
    let id_dir = blobs.parent().unwrap_or(blobs);
    for dir in [private, id_dir, blobs] {
        check_private(dir, 0).map_err(|e| e.to_string())?;
    }
    run_icacls(&icacls_steps(private, &sid))
        .and_then(|()| verify_acl(private, &sid))
        .map_err(|e| e.to_string())?;
    // The project's own folders: the user's, with nothing but what they inherit from the
    // private folder (a folder made again after it vanished must not keep someone else's entries).
    for dir in [id_dir, blobs] {
        let [owner, ..] = icacls_steps(dir, &sid);
        run_icacls(&[owner])
            .and_then(|()| verify_acl(dir, &sid))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Create (or reuse) `<anchor>/.remora-store/<id>/blobs`, refusing a `.remora-store` that
/// is a symlink, someone else's, or open to others. An existing one is never loosened or
/// tightened, only checked.
#[cfg(not(windows))]
fn prepare(anchor: &Path, id: &str, owner: u32, _data_dir: &Path) -> io::Result<PathBuf> {
    let store_dir = anchor.join(STORE_DIR);
    let created = match create_private(&store_dir) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(e),
    };
    let blobs = store_dir.join(id).join("blobs");
    let result = (|| {
        check_private(&store_dir, owner)?;
        ensure_gitignore(&store_dir, created)?;
        fsutil::private_dir(&blobs)
    })();
    match result {
        Ok(()) => Ok(blobs),
        Err(e) => {
            if created {
                remove_created(&store_dir, id);
            }
            Err(e)
        }
    }
}

/// Put a `.gitignore` with `*` in the private folder, unless it has one: a git repository
/// around it would list it otherwise. For a folder made by an earlier version it is best effort.
fn ensure_gitignore(private: &Path, required: bool) -> io::Result<()> {
    let file = private.join(".gitignore");
    if fs::symlink_metadata(&file).is_ok() {
        return Ok(());
    }
    let written = crate::fsutil::write_private_atomic(&file, b"*\n");
    if required { written } else { Ok(()) }
}

/// Remove the private folder this call made, after a later step failed, so a stray one does
/// not refuse every later project. Only what Remora made and only if empty: nothing else goes.
fn remove_created(private: &Path, id: &str) {
    let id_dir = private.join(id);
    let _ = fs::remove_dir(id_dir.join("blobs"));
    let _ = fs::remove_dir(&id_dir);
    let _ = fs::remove_file(private.join(".gitignore"));
    let _ = fs::remove_dir(private);
}

/// Create `<drive>:\.remora-store-<SID>` (or reuse it when Remora made it for this user
/// earlier), make it the user's alone, then create `<id>\blobs` in it.
#[cfg(windows)]
fn prepare(anchor: &Path, id: &str, _owner: u32, data_dir: &Path) -> io::Result<PathBuf> {
    let sid = crate::user_sid().ok_or_else(|| io::Error::other("cannot read the user's SID with whoami"))?;
    // At the drive's root, not inside a shared folder: other users may not rename what is
    // in the root unless they own it.
    let private = anchor.join(private_name(&sid));
    let created = match fs::create_dir(&private) {
        Ok(()) => true,
        // Someone else's folder could hold their entries, junctions or open handles.
        Err(e)
            if e.kind() == io::ErrorKind::AlreadyExists
                && !recorded_under(data_dir, &private)
                && !left_by_a_crash(&private, &sid) =>
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} exists but was not made by Remora for you: delete it to let Remora use this drive",
                    private.display()
                ),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(e),
    };
    let id_dir = private.join(id);
    let blobs = id_dir.join("blobs");
    let result = (|| {
        check_private(&private, 0)?;
        run_icacls(&icacls_steps(&private, &sid))?;
        verify_acl(&private, &sid)?;
        ensure_gitignore(&private, created)?;
        for dir in [&id_dir, &blobs] {
            match fs::create_dir(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            check_private(dir, 0)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(blobs),
        Err(e) => {
            if created {
                remove_created(&private, id);
            }
            Err(e)
        }
    }
}

/// Whether `private`, unknown to every recorded project, is one a crash left (see `adoptable`).
#[cfg(windows)]
fn left_by_a_crash(private: &Path, sid: &str) -> bool {
    if check_private(private, 0).is_err() {
        return false;
    }
    let Ok(entries) = fs::read_dir(private) else {
        return false;
    };
    let entries: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    match full_sddl(private) {
        Ok(sddl) => adoptable(&sddl, sid, &entries),
        Err(e) => {
            log::warn!("cannot read the owner of {}: {e}", private.display());
            false
        }
    }
}

/// `dir`'s owner and access list (the owner is what `icacls /save` leaves out) as SDDL, read
/// through .NET from Windows PowerShell. No PowerShell module is loaded, and the
/// `PSModulePath` of a PowerShell 7 parent is dropped: Windows PowerShell cannot load the
/// modules it points to.
#[cfg(windows)]
fn full_sddl(dir: &Path) -> io::Result<String> {
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let path = without_verbatim_prefix(dir).to_string_lossy().replace('\'', "''");
    let out = std::process::Command::new(
        Path::new(&system_root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
    )
    .args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &format!(
            "[System.IO.Directory]::GetAccessControl('{path}').GetSecurityDescriptorSddlForm('Owner, Access')"
        ),
    ])
    .env_remove("PSModulePath")
    .stdin(std::process::Stdio::null())
    .output()?;
    let sddl = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() || sddl.is_empty() {
        return Err(io::Error::other(format!(
            "reading the security descriptor failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(sddl)
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
    let ancestors = ancestors_of(root);
    Ok(pick_anchor(&ancestors, volume, data.uid()).map(|anchor| (anchor, data.uid())))
}

/// Why a project on another volume than `data_dir` has no blob folder on its own volume
/// (so its files are copied, not cloned), when a folder above it is the reason: `None` on
/// the data directory's volume or when a blob folder can be chosen there.
#[cfg(unix)]
pub(crate) fn plain_copy_reason(root: &Path, data_dir: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let volume = fs::metadata(root).ok()?.dev();
    let data = fs::metadata(data_dir).ok()?;
    if data.dev() == volume {
        return None;
    }
    why_not_anchored(&ancestors_of(root), volume, data.uid())
}

#[cfg(not(unix))]
pub(crate) fn plain_copy_reason(_root: &Path, _data_dir: &Path) -> Option<String> {
    None
}

/// For `pick_anchor` finding nothing: the folder that stops it, in words.
#[cfg_attr(not(unix), allow(dead_code))]
fn why_not_anchored(ancestors: &[Ancestor], volume: u64, owner: u32) -> Option<String> {
    if pick_anchor(ancestors, volume, owner).is_some() {
        return None;
    }
    let reason = match ancestors.iter().find(|a| !a.is_safe(owner)) {
        Some(a) if open_to_others(a.mode) => format!(
            "{} is writable by its group or others, so Remora keeps no store under it",
            a.path.display()
        ),
        Some(a) => format!(
            "{} belongs to another user, so Remora keeps no store under it",
            a.path.display()
        ),
        None => "no folder above the project on its volume belongs to you".to_owned(),
    };
    Some(reason)
}

/// The folders above `root`, its parent first, all the way up to `/`. One that cannot be
/// read counts as unsafe and ends the list.
#[cfg(unix)]
fn ancestors_of(root: &Path) -> Vec<Ancestor> {
    use std::os::unix::fs::MetadataExt;
    let mut ancestors = Vec::new();
    for path in root.ancestors().skip(1) {
        match fs::metadata(path) {
            Ok(m) => ancestors.push(Ancestor {
                path: path.to_path_buf(),
                volume: m.dev(),
                owner: m.uid(),
                mode: m.mode(),
            }),
            Err(_) => {
                ancestors.push(Ancestor {
                    path: path.to_path_buf(),
                    volume: u64::MAX,
                    owner: u32::MAX,
                    mode: 0o777,
                });
                break;
            }
        }
    }
    ancestors
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
        let top = at("/", 1, 0);
        // The open folder itself is skipped, and being above `mine` it makes `mine` unsafe too.
        assert_eq!(
            pick_anchor(&[mine.clone(), open.clone(), top.clone()], 2, 1000),
            None
        );
        assert_eq!(
            pick_anchor(&[mine, sticky, top], 2, 1000),
            Some(PathBuf::from("/mnt/work")),
            "the sticky bit keeps others from renaming"
        );
    }

    #[test]
    fn why_files_are_copied_names_the_folder_that_stops_the_store() {
        let shared = Ancestor {
            mode: 0o775,
            ..at("/mnt/shared", 2, 1000)
        };
        let mine = at("/mnt/shared/me", 2, 1000);
        let top = at("/", 1, 0);
        let reason = why_not_anchored(&[mine.clone(), shared, top.clone()], 2, 1000).unwrap();
        assert!(
            reason.contains("/mnt/shared is writable by its group or others"),
            "{reason}"
        );

        let theirs = at("/mnt/team", 2, 1001);
        let reason = why_not_anchored(&[mine.clone(), theirs, top.clone()], 2, 1000).unwrap();
        assert!(reason.contains("/mnt/team belongs to another user"), "{reason}");

        let roots = at("/mnt/data", 2, 0);
        let reason = why_not_anchored(&[roots, top.clone()], 2, 1000).unwrap();
        assert!(reason.contains("no folder above the project"), "{reason}");

        assert_eq!(why_not_anchored(&[mine, at("/mnt", 2, 0), top], 2, 1000), None);
    }

    #[test]
    fn every_ancestor_above_the_anchor_must_be_safe() {
        // /a is open to others, so they can rename /a/b away.
        let open_above = [
            at("/a/b", 2, 1000),
            Ancestor {
                mode: 0o777,
                ..at("/a", 2, 1000)
            },
            at("/", 1, 0),
        ];
        assert_eq!(pick_anchor(&open_above, 2, 1000), None);
        // Owned by another, non-root user.
        let foreign = [at("/a/b", 2, 1000), at("/a", 2, 1001), at("/", 1, 0)];
        assert_eq!(pick_anchor(&foreign, 2, 1000), None);
        // An unsafe ancestor far above a mount still counts.
        let far = [
            at("/srv/data", 2, 1000),
            at("/srv", 1, 0),
            Ancestor {
                mode: 0o775,
                ..at("/", 1, 0)
            },
        ];
        assert_eq!(pick_anchor(&far, 2, 1000), None);
        // A root-owned 0755 chain above an owned mount is fine.
        let mount = [
            at("/mnt/work/me", 2, 1000),
            at("/mnt/work", 2, 1000),
            at("/mnt", 1, 0),
            at("/", 1, 0),
        ];
        assert_eq!(pick_anchor(&mount, 2, 1000), Some(PathBuf::from("/mnt/work")));
        // A sticky 1777 ancestor above (like /tmp) is fine too.
        let sticky_above = [
            at("/tmp/mnt", 2, 1000),
            Ancestor {
                mode: 0o1777,
                ..at("/tmp", 1, 0)
            },
            at("/", 1, 0),
        ];
        assert_eq!(
            pick_anchor(&sticky_above, 2, 1000),
            Some(PathBuf::from("/tmp/mnt"))
        );
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::unwrap_used)]
    fn a_new_store_folder_ignores_itself_in_git() {
        let anchor = tempfile::TempDir::new().unwrap();
        let owner = owner_of(anchor.path()).unwrap();
        let blobs = prepare(anchor.path(), "0123456789abcdef", owner, anchor.path()).unwrap();
        let store_dir = anchor.path().join(STORE_DIR);
        assert_eq!(fs::read_to_string(store_dir.join(".gitignore")).unwrap(), "*\n");
        assert!(blobs.is_dir());
        // A second project in the same folder keeps it, and the folder still checks out.
        prepare(anchor.path(), "fedcba9876543210", owner, anchor.path()).unwrap();
        check_private(&store_dir, owner).unwrap();
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
    fn only_a_store_folder_a_crash_left_for_the_user_is_adopted() {
        let sid = "S-1-5-21-1-2-3-1013";
        let private = format!("O:{sid}G:{sid}D:PAI(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)");
        let gitignore = vec![".gitignore".to_owned()];
        assert_eq!(sddl_owner(&private), Some(sid));
        assert!(adoptable(&private, sid, &gitignore));
        assert!(adoptable(&private, sid, &[]), "a crash before the .gitignore");
        assert!(
            !adoptable(
                &private,
                sid,
                &[".gitignore".to_owned(), "0123456789abcdef".to_owned()]
            ),
            "something in it"
        );
        let theirs = private.replacen(&format!("O:{sid}"), "O:S-1-5-21-1-2-3-1014", 1);
        assert!(!adoptable(&theirs, sid, &gitignore), "someone else's");
        let admins = private.replacen(&format!("O:{sid}"), "O:BA", 1);
        assert!(!adoptable(&admins, sid, &gitignore), "owned by Administrators");
        let open = format!("{private}(A;OICI;FA;;;WD)");
        assert!(!adoptable(&open, sid, &gitignore), "open to Everyone");
        assert!(!adoptable("D:PAI(A;OICI;FA;;;SY)", sid, &gitignore), "no owner");
        let admin = "S-1-5-21-1-2-3-500";
        assert!(adoptable(
            "O:LAG:LAD:PAI(A;OICI;FA;;;LA)(A;OICI;FA;;;SY)",
            admin,
            &gitignore
        ));
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
