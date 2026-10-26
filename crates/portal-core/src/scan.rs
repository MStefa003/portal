//! Building a [`Manifest`] from files and folders on disk (sender side).

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use ignore::WalkBuilder;

use crate::chunk::chunk_reader_with;
use crate::manifest::{
    Entry, EntryKind, FileMeta, MAX_ENTRIES, Manifest, ManifestError, Timestamp,
};
use crate::path::{PARTIAL_DIR_NAME, PathError, SafeRelPath};

/// Files read at the same time while scanning. More stops helping once the
/// disk, not hashing, is the limit.
const MAX_SCAN_THREADS: usize = 8;

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
    /// The scanned paths on disk, one per top-level entry of the manifest
    /// and in the same order (for [`crate::Source::new`]).
    pub roots: Vec<PathBuf>,
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
    /// Two of the paths to send have the same name (ignoring letter case).
    #[error("`{}` has the same name as another item being sent; rename one or send them separately", .0.display())]
    DuplicateName(PathBuf),
    /// No paths were given.
    #[error("nothing to send")]
    Nothing,
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
    scan_many(&[root.to_path_buf()], options)
}

/// Like [`scan`], for several files and folders sent together: each becomes
/// a top-level entry of one manifest. Their names must differ.
pub fn scan_many(roots: &[PathBuf], options: &ScanOptions) -> Result<Scan, ScanError> {
    plan(roots, options)?.read(|_| {})
}

/// The first, quick part of [`scan_many`]: finds every file and folder and
/// their sizes without reading any file. [`Plan::read`] does the rest.
pub fn plan(roots: &[PathBuf], options: &ScanOptions) -> Result<Plan, ScanError> {
    if roots.is_empty() {
        return Err(ScanError::Nothing);
    }
    let mut planned = Vec::new();
    let mut skipped = Vec::new();
    let mut collision_keys = HashSet::new();
    for root in roots {
        scan_root(
            root,
            options,
            &mut planned,
            &mut skipped,
            &mut collision_keys,
        )?;
    }
    if planned.len() > MAX_ENTRIES {
        return Err(ManifestError::TooManyEntries.into());
    }
    Ok(Plan {
        planned,
        roots: roots.to_vec(),
        skipped,
    })
}

/// The files and folders to send, found by [`plan`]; their files are not
/// read yet. The numbers are as found on disk: files that turn out to be
/// unreadable are left out of the final [`Scan`].
#[derive(Debug)]
pub struct Plan {
    planned: Vec<Planned>,
    roots: Vec<PathBuf>,
    skipped: Vec<Skipped>,
}

impl Plan {
    /// The name of the first top-level item.
    pub fn name(&self) -> &str {
        self.planned
            .first()
            .map_or("", |p| p.rel().first_component())
    }

    /// Number of top-level items.
    pub fn root_count(&self) -> usize {
        self.roots.len()
    }

    /// Whether this is a single file rather than a folder or several items.
    pub fn is_single_file(&self) -> bool {
        matches!(self.planned[..], [Planned::File { .. }])
    }

    /// Number of files.
    pub fn file_count(&self) -> u64 {
        self.files().count() as u64
    }

    /// Sum of the file sizes.
    pub fn total_bytes(&self) -> u64 {
        self.files().map(Metadata::len).sum()
    }

    /// Entries left out so far.
    pub fn skipped(&self) -> &[Skipped] {
        &self.skipped
    }

    fn files(&self) -> impl Iterator<Item = &Metadata> {
        self.planned.iter().filter_map(|p| match p {
            Planned::File { meta, .. } => Some(meta),
            Planned::Dir(_) => None,
        })
    }

    /// Reads and chunks every file, calling `progress` with the number of
    /// bytes read as it goes (from several threads), and builds the manifest.
    /// [`Scan::skipped`] includes the entries of [`Plan::skipped`].
    pub fn read(self, progress: impl Fn(u64) + Sync) -> Result<Scan, ScanError> {
        let Self {
            planned,
            roots,
            mut skipped,
        } = self;
        let entries = read_files(planned, &mut skipped, &progress)?;
        let manifest = Manifest {
            version: Manifest::version_for(roots.len()),
            entries,
        };
        manifest.validate()?;
        Ok(Scan {
            manifest,
            roots,
            skipped,
        })
    }
}

/// An entry found while walking, before the content of its file is read.
#[derive(Debug)]
enum Planned {
    Dir(SafeRelPath),
    File {
        rel: SafeRelPath,
        path: PathBuf,
        meta: Metadata,
        /// Given directly rather than found in a folder: failing to read it
        /// fails the scan instead of skipping it.
        root: bool,
    },
}

impl Planned {
    fn rel(&self) -> &SafeRelPath {
        match self {
            Self::Dir(rel) | Self::File { rel, .. } => rel,
        }
    }
}

fn scan_root(
    root: &Path,
    options: &ScanOptions,
    entries: &mut Vec<Planned>,
    skipped: &mut Vec<Skipped>,
    collision_keys: &mut HashSet<String>,
) -> Result<(), ScanError> {
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
    if !collision_keys.insert(root_path.collision_key()) {
        return Err(ScanError::DuplicateName(root.to_path_buf()));
    }

    if meta.is_file() {
        entries.push(Planned::File {
            rel: root_path,
            path: root.to_path_buf(),
            meta,
            root: true,
        });
    } else if meta.is_dir() {
        entries.push(Planned::Dir(root_path.clone()));
        walk(root, &root_path, options, entries, skipped, collision_keys);
    } else {
        return Err(ScanError::Unsupported(root.to_path_buf()));
    }
    Ok(())
}

fn walk(
    root: &Path,
    root_path: &SafeRelPath,
    options: &ScanOptions,
    entries: &mut Vec<Planned>,
    skipped: &mut Vec<Skipped>,
    collision_keys: &mut HashSet<String>,
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
            entries.push(Planned::Dir(rel_path));
        } else if file_type.is_file() {
            match entry.metadata() {
                Ok(meta) => entries.push(Planned::File {
                    rel: rel_path,
                    path: path.to_path_buf(),
                    meta,
                    root: false,
                }),
                Err(e) => skip(skipped, path, format!("could not read: {e}")),
            }
        } else {
            skip(skipped, path, "not a regular file or folder".into());
        }
    }
}

/// Chunks the planned files on several threads (most of a scan is reading
/// and hashing) and returns the entries in their original order, leaving out
/// files in folders that could not be read.
fn read_files(
    planned: Vec<Planned>,
    skipped: &mut Vec<Skipped>,
    progress: &(impl Fn(u64) + Sync),
) -> Result<Vec<Entry>, ScanError> {
    let files: Vec<usize> = planned
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, Planned::File { .. }))
        .map(|(i, _)| i)
        .collect();
    let results: Vec<OnceLock<io::Result<FileMeta>>> =
        planned.iter().map(|_| OnceLock::new()).collect();
    let threads = thread::available_parallelism()
        .map_or(1, usize::from)
        .min(MAX_SCAN_THREADS)
        .min(files.len());
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some(&i) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if let Planned::File { path, meta, .. } = &planned[i] {
                        let _ = results[i].set(file_meta(path, meta, progress));
                    }
                }
            });
        }
    });

    let mut entries = Vec::with_capacity(planned.len());
    for (planned, result) in planned.into_iter().zip(results) {
        match planned {
            Planned::Dir(path) => entries.push(Entry {
                path,
                kind: EntryKind::Dir,
            }),
            Planned::File {
                rel, path, root, ..
            } => match result.into_inner() {
                Some(Ok(file)) => entries.push(Entry {
                    path: rel,
                    kind: EntryKind::File(file),
                }),
                Some(Err(source)) if root => return Err(ScanError::Io { path, source }),
                Some(Err(e)) => skip(skipped, &path, format!("could not read: {e}")),
                None => skip(skipped, &path, "could not read".into()),
            },
        }
    }
    Ok(entries)
}

fn skip(skipped: &mut Vec<Skipped>, path: &Path, reason: String) {
    tracing::warn!(path = %path.display(), %reason, "skipping");
    skipped.push(Skipped {
        path: Some(path.to_path_buf()),
        reason,
    });
}

fn file_meta(path: &Path, meta: &Metadata, progress: impl Fn(u64)) -> io::Result<FileMeta> {
    let chunked = chunk_reader_with(File::open(path)?, progress)?;
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
    fn scans_several_items_into_one_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("photos");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("a.jpg"), "aa").unwrap();
        let file = tmp.path().join("notes.txt");
        fs::write(&file, "n").unwrap();

        let scan = scan_many(&[file.clone(), folder.clone()], &ScanOptions::default()).unwrap();
        assert_eq!(paths(&scan), ["notes.txt", "photos", "photos/a.jpg"]);
        assert_eq!(scan.roots, [file, folder]);
        assert_eq!(
            scan.manifest.version,
            crate::manifest::MANIFEST_VERSION_MULTI
        );
        assert_eq!(scan.manifest.root_count(), 2);

        let one = scan_many(&[tmp.path().join("notes.txt")], &ScanOptions::default()).unwrap();
        assert_eq!(one.manifest.version, crate::manifest::MANIFEST_VERSION);
    }

    #[test]
    fn items_with_the_same_name_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["x", "y"] {
            fs::create_dir(tmp.path().join(dir)).unwrap();
            fs::write(tmp.path().join(dir).join("Same.txt"), dir).unwrap();
        }
        let err = scan_many(
            &[tmp.path().join("x/Same.txt"), tmp.path().join("y/Same.txt")],
            &ScanOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ScanError::DuplicateName(_)));
        assert!(matches!(
            scan_many(&[], &ScanOptions::default()),
            Err(ScanError::Nothing)
        ));
    }

    #[test]
    fn plan_counts_before_reading() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(root.join("b")).unwrap();
        fs::write(root.join("a.txt"), "hello").unwrap();
        fs::write(root.join("b/x.txt"), "x").unwrap();

        let plan = plan(&[root], &ScanOptions::default()).unwrap();
        assert_eq!(plan.name(), "proj");
        assert_eq!((plan.file_count(), plan.total_bytes()), (2, 6));
        assert!(!plan.is_single_file());
        let read = std::sync::atomic::AtomicU64::new(0);
        let scan = plan
            .read(|n| {
                read.fetch_add(n, Ordering::Relaxed);
            })
            .unwrap();
        assert_eq!(read.into_inner(), 6);
        assert_eq!(scan.manifest.total_bytes(), 6);
    }

    #[test]
    fn missing_path_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = scan(&tmp.path().join("nope"), &ScanOptions::default()).unwrap_err();
        assert!(matches!(err, ScanError::Io { .. }));
    }
}
