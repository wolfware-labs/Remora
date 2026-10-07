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
/// root.
pub fn resolve(dir: &Path) -> Root {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
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

        assert_eq!(resolve(&gone), Root::Repo(repo.path().to_path_buf()));
    }
}
