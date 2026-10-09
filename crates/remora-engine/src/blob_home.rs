//! Where a project's blobs live. On the data directory's volume they stay in the project's
//! store. A project on another volume gets `<anchor>/.remora-store/<project id>/blobs` on its
//! own volume, so its files can be cloned instead of copied (clones never cross volumes).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::EngineError;
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
}

/// The anchor for a project on `volume`: the highest of `ancestors` (the project's parent
/// first, then upward) still on that volume and owned by `owner`. Never the project root
/// itself, so the store is never inside the project.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn pick_anchor(ancestors: &[Ancestor], volume: u64, owner: u32) -> Option<PathBuf> {
    ancestors
        .iter()
        .take_while(|a| a.volume == volume)
        .filter(|a| a.owner == owner)
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
    let store_dir = blobs
        .ancestors()
        .nth(2)
        .filter(|dir| dir.file_name().is_some_and(|name| name == STORE_DIR))
        .ok_or_else(|| unavailable(format!("it is not inside a {STORE_DIR} folder")))?;
    let owner = owner_of(data_dir).map_err(|e| unavailable(e.to_string()))?;
    check_private(store_dir, owner).map_err(|e| unavailable(e.to_string()))
}

/// Create (or reuse) `<anchor>/.remora-store/<id>/blobs`, refusing a `.remora-store` that
/// is a symlink, someone else's, or open to others. An existing one is never loosened or
/// tightened, only checked.
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
            })
        })
        .collect();
    Ok(pick_anchor(&ancestors, volume, data.uid()).map(|anchor| (anchor, data.uid())))
}

#[cfg(not(unix))]
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

#[cfg(not(unix))]
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
}
