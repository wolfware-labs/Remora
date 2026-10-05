//! Walks a directory tree without following symlinks.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use walkdir::{DirEntry, WalkDir};

use crate::error::io_at;
use crate::{SnapError, SnapOptions, platform};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    Dir {
        mode: u32,
    },
    File {
        size: u64,
        mode: u32,
    },
    Symlink {
        target: String,
    },
    /// Could not be read, or is not a regular file/dir/symlink (socket, FIFO, device).
    Unreadable,
}

#[derive(Debug)]
pub(crate) struct ScanEntry {
    pub abs: PathBuf,
    pub kind: Kind,
}

#[derive(Debug, Default)]
pub(crate) struct ScanResult {
    /// Keyed by `/`-separated path relative to the root.
    pub entries: BTreeMap<String, ScanEntry>,
    /// Paths skipped because a component is not valid UTF-8.
    pub non_utf8: Vec<PathBuf>,
}

/// Scan `root`, skipping excluded directory names and anything under `skip_under`.
pub(crate) fn scan(root: &Path, opts: &SnapOptions, skip_under: &Path) -> Result<ScanResult, SnapError> {
    let mut entries = BTreeMap::new();
    let mut non_utf8 = Vec::new();

    let walker = WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.path().starts_with(skip_under) {
                return false;
            }
            let Some(name) = e.file_name().to_str() else {
                non_utf8.push(e.path().to_path_buf());
                return false;
            };
            !(e.file_type().is_dir() && opts.exclude_dirs.iter().any(|x| x == name))
        });

    for item in walker {
        match item {
            Ok(entry) => {
                if let Some(rel) = rel_key(root, entry.path()) {
                    let kind = classify(&entry);
                    entries.insert(
                        rel,
                        ScanEntry {
                            abs: entry.path().to_path_buf(),
                            kind,
                        },
                    );
                }
            }
            Err(err) => {
                let Some(path) = err.path().map(Path::to_path_buf) else {
                    return Err(io_at(root)(io::Error::other(err.to_string())));
                };
                if err
                    .io_error()
                    .is_some_and(|e| e.kind() == io::ErrorKind::NotFound)
                {
                    continue; // vanished mid-scan
                }
                if path == root {
                    return Err(io_at(root)(io::Error::other(err.to_string())));
                }
                if let Some(rel) = rel_key(root, &path) {
                    entries.insert(
                        rel,
                        ScanEntry {
                            abs: path,
                            kind: Kind::Unreadable,
                        },
                    );
                }
            }
        }
    }

    Ok(ScanResult { entries, non_utf8 })
}

/// `/`-joined relative path, or `None` if a component is not UTF-8.
pub(crate) fn rel_key(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

fn classify(entry: &DirEntry) -> Kind {
    let ft = entry.file_type();
    if ft.is_symlink() {
        return match fs::read_link(entry.path()) {
            Ok(target) => match target.to_str() {
                Some(t) => Kind::Symlink { target: t.to_owned() },
                None => Kind::Unreadable,
            },
            Err(_) => Kind::Unreadable,
        };
    }
    match entry.metadata() {
        Ok(meta) if ft.is_dir() => Kind::Dir {
            mode: platform::mode_of(&meta),
        },
        Ok(meta) if ft.is_file() => Kind::File {
            size: meta.len(),
            mode: platform::mode_of(&meta),
        },
        _ => Kind::Unreadable,
    }
}
