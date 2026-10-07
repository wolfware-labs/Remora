//! Which folder a session protects: the git repository (or linked worktree) containing it.

use std::path::{Path, PathBuf};

/// A repository root holding this file is not recorded.
pub const IGNORE_FILE: &str = ".remora-ignore";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Root {
    /// Record this repository root.
    Repo(PathBuf),
    /// Inside a repository that opted out with [`IGNORE_FILE`].
    Ignored(PathBuf),
    /// Not inside any git repository: nothing is recorded.
    NotInGit,
}

/// The root protecting `dir`: the nearest ancestor (or `dir` itself) holding a `.git`
/// directory or file. A `.git` file marks a linked worktree or submodule, which is its own
/// root. A relative `dir` is never in a repository: it would resolve against whatever
/// directory this process happens to run in.
pub fn resolve(dir: &Path) -> Root {
    if !dir.is_absolute() {
        return Root::NotInGit;
    }
    // A folder the agent deleted still resolves to its repository's canonical root.
    let Some(dir) = canonical_prefix(dir) else {
        return Root::NotInGit;
    };
    for candidate in dir.ancestors() {
        if candidate.join(".git").symlink_metadata().is_ok() {
            return if candidate.join(IGNORE_FILE).symlink_metadata().is_ok() {
                Root::Ignored(candidate.to_path_buf())
            } else {
                Root::Repo(candidate.to_path_buf())
            };
        }
    }
    Root::NotInGit
}

/// `path` with its deepest existing ancestor canonicalized and the missing rest appended.
fn canonical_prefix(path: &Path) -> Option<PathBuf> {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(base) = existing.canonicalize() {
            return Some(missing.iter().rev().fold(base, |path, part| path.join(part)));
        }
        missing.push(existing.file_name()?);
        existing = existing.parent()?;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::fs;

    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        dir
    }

    fn canonical(path: &Path) -> PathBuf {
        path.canonicalize().unwrap()
    }

    #[test]
    fn the_repository_root_protects_its_subfolders() {
        let repo = repo();
        let sub = repo.path().join("src/deep");
        fs::create_dir_all(&sub).unwrap();

        assert_eq!(resolve(&sub), Root::Repo(canonical(repo.path())));
        assert_eq!(resolve(repo.path()), Root::Repo(canonical(repo.path())));
    }

    #[test]
    fn a_linked_worktree_is_its_own_root() {
        let repo = repo();
        let worktree = repo.path().join("wt");
        fs::create_dir(&worktree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: ../.git/worktrees/wt\n").unwrap();

        assert_eq!(resolve(&worktree.join("")), Root::Repo(canonical(&worktree)));
    }

    #[test]
    fn a_folder_outside_git_is_not_recorded() {
        let dir = tempfile::TempDir::new().unwrap();
        // Temporary directories normally sit outside any repository; skip if not.
        if resolve(&dir.path().join("..")) != Root::NotInGit {
            return;
        }
        assert_eq!(resolve(dir.path()), Root::NotInGit);
    }

    #[test]
    fn a_repository_can_opt_out() {
        let repo = repo();
        fs::write(repo.path().join(IGNORE_FILE), "").unwrap();

        assert_eq!(resolve(repo.path()), Root::Ignored(canonical(repo.path())));
    }

    #[test]
    fn a_missing_folder_resolves_through_its_parents() {
        let repo = repo();
        let gone = repo.path().join("deleted/by/the/agent");

        assert_eq!(resolve(&gone), Root::Repo(canonical(repo.path())));
    }

    #[test]
    fn a_relative_or_empty_folder_is_not_in_git() {
        assert_eq!(resolve(Path::new("")), Root::NotInGit);
        assert_eq!(resolve(Path::new("some/relative/dir")), Root::NotInGit);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_folder_resolves_to_the_real_repository() {
        let repo = repo();
        let links = tempfile::TempDir::new().unwrap();
        let link = links.path().join("proj");
        std::os::unix::fs::symlink(repo.path(), &link).unwrap();

        assert_eq!(resolve(&link), Root::Repo(canonical(repo.path())));
    }
}
