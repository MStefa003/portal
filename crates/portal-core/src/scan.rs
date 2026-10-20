//! Building a [`Manifest`] from a file or folder on disk (sender side).

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::chunk::chunk_reader;
use crate::manifest::{
    Entry, EntryKind, FileMeta, MANIFEST_VERSION, Manifest, ManifestError, Timestamp,
};
use crate::path::{PARTIAL_DIR_NAME, PathError, SafeRelPath};

/// Options for [`scan`].
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Skip files excluded by `.gitignore`, `.ignore` and git exclude rules.
    pub respect_gitignore: bool,
}

/// The result of a scan: the manifest plus everything that was left out.
#[derive(Debug)]
pub struct Scan {
    /// The manifest describing what will be sent.
    pub manifest: Manifest,
    /// Entries that could not be included, with the reason.
    pub skipped: Vec<Skipped>,
}

/// An entry left out of the manifest.
#[derive(Debug, Clone)]
pub struct Skipped {
    /// The path on disk, when known.
    pub path: Option<PathBuf>,
    /// Human-readable reason.
    pub reason: String,
}

/// Why a scan failed as a whole.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// Reading the file or folder to send failed.
    #[error("could not read `{}`: {source}", path.display())]
    Io {
        /// The path being read.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The path has no final name (e.g. a filesystem root such as `C:\` or `/`).
    #[error("`{}` has no name; pick a file or folder inside it", .0.display())]
    NoName(PathBuf),
    /// The top-level name is not portable.
    #[error("cannot send `{}`: {source}", path.display())]
    InvalidName {
        /// The path being sent.
        path: PathBuf,
        /// What is wrong with its name.
        source: PathError,
    },
    /// The path is neither a regular file nor a directory.
    #[error("`{}` is not a regular file or folder", .0.display())]
    Unsupported(PathBuf),
    /// The resulting manifest is invalid (e.g. too many entries).
    #[error(transparent)]
    Manifest(#[from] ManifestError),
}

/// Scans `root` (a file or folder) and builds its manifest, chunking every file.
///
/// Entries that cannot be sent portably (symbolic links, special files, names
/// that are invalid on some OS, names that collide when case is ignored,
/// unreadable files) are left out and reported in [`Scan::skipped`].
pub fn scan(root: &Path, options: &ScanOptions) -> Result<Scan, ScanError> {
    let io_err = |source| ScanError::Io {
        path: root.to_path_buf(),
        source,
    };
    let meta = fs::metadata(root).map_err(io_err)?;
    // Canonicalize only to learn the real name of e.g. `.` or `..`.
    let canonical = fs::canonicalize(root).map_err(io_err)?;
    let name = canonical
        .file_name()
        .ok_or_else(|| ScanError::NoName(root.to_path_buf()))?;
    let root_path = SafeRelPath::from_relative_path(Path::new(name)).map_err(|source| {
        ScanError::InvalidName {
            path: root.to_path_buf(),
            source,
        }
    })?;

    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    if meta.is_file() {
        let file = file_meta(root, &meta).map_err(io_err)?;
        entries.push(Entry {
            path: root_path,
            kind: EntryKind::File(file),
        });
    } else if meta.is_dir() {
        entries.push(Entry {
            path: root_path.clone(),
            kind: EntryKind::Dir,
        });
        walk(root, &root_path, options, &mut entries, &mut skipped);
    } else {
        return Err(ScanError::Unsupported(root.to_path_buf()));
    }

    let manifest = Manifest {
        version: MANIFEST_VERSION,
        entries,
    };
    manifest.validate()?;
    Ok(Scan { manifest, skipped })
}

fn walk(
    root: &Path,
    root_path: &SafeRelPath,
    options: &ScanOptions,
    entries: &mut Vec<Entry>,
    skipped: &mut Vec<Skipped>,
) {
    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(false)
        .hidden(false)
        .follow_links(false)
        // Sorted, depth-first: every directory is yielded before its contents,
        // which is the parent-before-child order the manifest requires.
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|e| e.depth() == 0 || e.file_name() != OsStr::new(PARTIAL_DIR_NAME));
    if options.respect_gitignore {
        builder
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .ignore(true)
            .parents(true)
            .require_git(false);
    }

    let mut collision_keys = HashSet::from([root_path.collision_key()]);
    let mut skipped_dirs: Vec<PathBuf> = Vec::new();

    for result in builder.build() {
        let entry = match result {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(%err, "skipping unreadable entry");
                skipped.push(Skipped {
                    path: None,
                    reason: err.to_string(),
                });
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        let path = entry.path();
        // Everything below a skipped directory is skipped with it.
        if skipped_dirs.iter().any(|d| path.starts_with(d)) {
            continue;
        }
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            skip(skipped, path, "symbolic links are not sent".into());
            continue;
        }
        let rel_path = match path
            .strip_prefix(root)
            .map_err(|_| PathError::NotRelative)
            .and_then(|rel| root_path.join_path(rel))
        {
            Ok(p) => p,
            Err(e) => {
                if file_type.is_dir() {
                    skipped_dirs.push(path.to_path_buf());
                }
                skip(skipped, path, format!("name not portable: {e}"));
                continue;
            }
        };
        if !collision_keys.insert(rel_path.collision_key()) {
            if file_type.is_dir() {
                skipped_dirs.push(path.to_path_buf());
            }
            skip(
                skipped,
                path,
                "another entry has the same name when letter case is ignored".into(),
            );
            continue;
        }

        if file_type.is_dir() {
            entries.push(Entry {
                path: rel_path,
                kind: EntryKind::Dir,
            });
        } else if file_type.is_file() {
            match entry
                .metadata()
                .map_err(io::Error::other)
                .and_then(|meta| file_meta(path, &meta))
            {
                Ok(file) => entries.push(Entry {
                    path: rel_path,
                    kind: EntryKind::File(file),
                }),
                Err(e) => skip(skipped, path, format!("could not read: {e}")),
            }
        } else {
            skip(skipped, path, "not a regular file or folder".into());
        }
    }
}

fn skip(skipped: &mut Vec<Skipped>, path: &Path, reason: String) {
    tracing::warn!(path = %path.display(), %reason, "skipping");
    skipped.push(Skipped {
        path: Some(path.to_path_buf()),
        reason,
    });
}

fn file_meta(path: &Path, meta: &Metadata) -> io::Result<FileMeta> {
    let chunked = chunk_reader(File::open(path)?)?;
    Ok(FileMeta {
        size: chunked.size,
        mtime: meta.modified().ok().and_then(Timestamp::from_system_time),
        executable: is_executable(meta),
        hash: chunked.hash,
        chunks: chunked.chunks,
    })
}

#[cfg(unix)]
fn is_executable(meta: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(scan: &Scan) -> Vec<&str> {
        scan.manifest
            .entries
            .iter()
            .map(|e| e.path.as_str())
            .collect()
    }

    #[test]
    fn scans_a_folder_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(root.join("b/empty")).unwrap();
        fs::write(root.join("b/x.txt"), "x").unwrap();
        fs::write(root.join("a.txt"), "hello").unwrap();
        fs::write(root.join(".hidden"), "").unwrap();

        let scan = scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(
            paths(&scan),
            [
                "proj",
                "proj/.hidden",
                "proj/a.txt",
                "proj/b",
                "proj/b/empty",
                "proj/b/x.txt"
            ]
        );
        assert!(scan.skipped.is_empty());
        assert_eq!(scan.manifest.total_bytes(), 6);
    }

    #[test]
    fn scans_a_single_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("notes.md");
        fs::write(&file, "# hi").unwrap();
        let scan = scan(&file, &ScanOptions::default()).unwrap();
        assert_eq!(paths(&scan), ["notes.md"]);
        assert!(scan.manifest.is_single_file());
    }

    #[test]
    fn dot_resolves_to_the_folder_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("named");
        fs::create_dir(&root).unwrap();
        let scan = scan(&root.join("."), &ScanOptions::default()).unwrap();
        assert_eq!(scan.manifest.name(), "named");
    }

    #[test]
    fn skips_partial_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(root.join(PARTIAL_DIR_NAME)).unwrap();
        fs::write(root.join(PARTIAL_DIR_NAME).join("junk"), "x").unwrap();
        let scan = scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(paths(&scan), ["proj"]);
    }

    #[test]
    fn gitignore_is_opt_in() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(root.join("target/out.bin"), "x").unwrap();
        fs::write(root.join("debug.log"), "x").unwrap();
        fs::write(root.join("main.rs"), "x").unwrap();

        let all = scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(all.manifest.entries.len(), 6);

        let filtered = scan(
            &root,
            &ScanOptions {
                respect_gitignore: true,
            },
        )
        .unwrap();
        assert_eq!(
            paths(&filtered),
            ["proj", "proj/.gitignore", "proj/main.rs"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn skips_symlinks_and_unportable_names() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(root.join("bad:dir")).unwrap();
        fs::write(root.join("bad:dir/inner"), "x").unwrap();
        fs::write(root.join("what?.txt"), "x").unwrap();
        fs::write(root.join("ok"), "x").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("link")).unwrap();

        let scan = scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(paths(&scan), ["proj", "proj/ok"]);
        assert_eq!(scan.skipped.len(), 3, "{:?}", scan.skipped);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn skips_case_collisions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("README"), "1").unwrap();
        fs::write(root.join("readme"), "2").unwrap();
        let scan = scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(scan.manifest.entries.len(), 2);
        assert_eq!(scan.skipped.len(), 1);
    }

    #[test]
    fn missing_path_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = scan(&tmp.path().join("nope"), &ScanOptions::default()).unwrap_err();
        assert!(matches!(err, ScanError::Io { .. }));
    }
}
