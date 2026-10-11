//! Which path `remora init` and `remora service install` record for Remora's binaries.
//!
//! Package managers keep the path people reach through `PATH` stable across upgrades
//! (`/opt/homebrew/bin/remora`, `~/.cargo/bin/remora`, winget's `Links\remora.exe`), while
//! the file it leads to moves with every version (`/opt/homebrew/Cellar/remora/0.1.0/...`).
//! Recording the `PATH` entry keeps hooks and the login service working after an upgrade.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

/// The entry of `path` (a `PATH`-style list) for the executable `name` that is the same file
/// as `target`, as written there: links are not resolved. Relative and missing folders are
/// skipped. `None` when no entry leads to `target`.
pub fn on_path(target: &Path, name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    let target = target.canonicalize().ok()?;
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(&file))
        .find(|candidate| candidate.canonicalize().is_ok_and(|c| c == target))
}

/// The first entry of `path` for the executable `name`, whatever file it is.
pub fn first_on_path(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(&file))
        .find(|candidate| candidate.is_file())
}

/// The path to record for the binary `name` at `exe`: its entry on this process's `PATH`, and
/// whether it was found there; else (a build run by its full path) `exe` itself, resolved.
pub fn stable(exe: &Path, name: &str) -> io::Result<(PathBuf, bool)> {
    match on_path(exe, name, std::env::var_os("PATH").as_deref()) {
        Some(entry) => Ok((entry, true)),
        None => Ok((exe.canonicalize()?, false)),
    }
}

/// What is wrong with a recorded command path, if anything: it no longer exists, or the
/// `name` people run (the first on `path`) is another file. `None` when it is fine.
pub fn problem(recorded: &Path, name: &str, path: Option<&OsStr>) -> Option<String> {
    let Ok(resolved) = recorded.canonicalize() else {
        return Some(format!("{} no longer exists", recorded.display()));
    };
    let current = first_on_path(name, path)?;
    match current.canonicalize() {
        Ok(c) if c == resolved => None,
        _ => Some(format!(
            "{} is not the {name} on PATH ({})",
            recorded.display(),
            current.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let file = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&file, "binary").unwrap();
        file
    }

    fn path_of(dirs: &[&Path]) -> std::ffi::OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn the_path_entry_is_found_only_for_the_same_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        let remora = exe(&bin, "remora");
        let other = exe(&tmp.path().join("other"), "remora");
        let missing = tmp.path().join("missing");

        let path = path_of(&[Path::new("relative"), &missing, &bin]);
        assert_eq!(
            on_path(&remora, "remora", Some(&path)),
            Some(bin.join(remora.file_name().unwrap()))
        );
        assert_eq!(
            on_path(&other, "remora", Some(&path)),
            None,
            "another file of that name"
        );
        assert_eq!(on_path(&remora, "remorad", Some(&path)), None);
        assert_eq!(on_path(&remora, "remora", None), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_on_path_is_recorded_as_the_link() {
        // Homebrew: /opt/homebrew/bin/remora -> ../Cellar/remora/0.1.0/bin/remora.
        let tmp = tempfile::TempDir::new().unwrap();
        let real = exe(&tmp.path().join("Cellar/remora/0.1.0/bin"), "remora");
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&real, bin.join("remora")).unwrap();

        let path = path_of(&[&bin]);
        assert_eq!(on_path(&real, "remora", Some(&path)), Some(bin.join("remora")));
        assert_eq!(problem(&bin.join("remora"), "remora", Some(&path)), None);

        // An upgrade moved the real file: the recorded link still works, the old target does not.
        let newer = exe(&tmp.path().join("Cellar/remora/0.2.0/bin"), "remora");
        std::fs::remove_file(bin.join("remora")).unwrap();
        std::os::unix::fs::symlink(&newer, bin.join("remora")).unwrap();
        std::fs::remove_dir_all(tmp.path().join("Cellar/remora/0.1.0")).unwrap();
        assert_eq!(problem(&bin.join("remora"), "remora", Some(&path)), None);
        let stale = problem(&real, "remora", Some(&path)).unwrap();
        assert!(stale.contains("no longer exists"), "{stale}");
    }

    #[test]
    fn a_recorded_binary_other_than_the_one_on_path_is_reported() {
        let tmp = tempfile::TempDir::new().unwrap();
        let old = exe(&tmp.path().join("old"), "remora");
        let bin = tmp.path().join("bin");
        exe(&bin, "remora");
        let path = path_of(&[&bin]);

        let problem = problem(&old, "remora", Some(&path)).unwrap();
        assert!(problem.contains("is not the remora on PATH"), "{problem}");
        assert_eq!(
            super::problem(&old, "remora", None),
            None,
            "nothing on PATH to compare with"
        );
    }
}
