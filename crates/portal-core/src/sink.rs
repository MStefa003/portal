//! Writing a transfer to disk (receiver side).
//!
//! The [`Sink`] is where untrusted data meets the filesystem, so it is strict:
//!
//! 1. The manifest is validated and every target path is a [`SafeRelPath`].
//! 2. Before anything is written, existing files are checked: conflicts are
//!    refused unless overwriting was allowed, and existing symbolic links or
//!    file/folder mismatches are always refused.
//! 3. Every chunk is hashed before it is written; a mismatch is an error.
//! 4. Files are assembled in `<dest>/.portal-partial/<id>/`. When the last
//!    chunk of a file arrives, the whole file is hashed again and compared to
//!    the manifest, and only then atomically renamed into place.
//!
//! A corrupted transfer therefore never produces a file at its final path.
//!
//! An interrupted transfer leaves its partial folder behind. Receiving the
//! same manifest again (same transfer id) picks it up: every chunk already in
//! a partial file is re-hashed and kept if it matches. Files that already
//! exist at their target with the right content are skipped, not conflicts.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::chunk::MAX_CHUNK_SIZE;
use crate::hash::Hash;
use crate::manifest::{Entry, EntryKind, FileMeta, Manifest, ManifestError};
use crate::path::{PARTIAL_DIR_NAME, SafeRelPath};

/// What to do when a file already exists at a target path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverwritePolicy {
    /// Refuse to start the transfer.
    Never,
    /// Replace existing files.
    Always,
}

/// Why receiving failed.
#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    /// The manifest is invalid.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// A filesystem operation failed.
    #[error("could not write `{}`: {source}", path.display())]
    Io {
        /// The path being accessed.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The destination exists but is not a folder.
    #[error("the destination `{}` is not a folder", .0.display())]
    DestinationNotAFolder(PathBuf),
    /// Files already exist and overwriting was not allowed.
    #[error("{}", conflicts_message(.0))]
    Conflicts(Vec<SafeRelPath>),
    /// A folder exists where a file is expected, or the other way round.
    #[error("`{0}` already exists as a different kind (file vs. folder)")]
    TypeConflict(SafeRelPath),
    /// A symbolic link exists at a target path.
    #[error("refusing to write through the symbolic link at `{0}`")]
    Symlink(SafeRelPath),
    /// A chunk is larger than the protocol allows.
    #[error("received a chunk of {0} bytes, larger than the {MAX_CHUNK_SIZE}-byte limit")]
    ChunkTooLarge(usize),
    /// A chunk's content does not match its hash.
    #[error("received a corrupted chunk ({0})")]
    CorruptChunk(Hash),
    /// A chunk was not requested (unknown or already received).
    #[error("received an unexpected chunk ({0})")]
    UnexpectedChunk(Hash),
    /// A fully assembled file does not match its hash.
    #[error("`{0}` is corrupted: its content does not match the sender's")]
    CorruptFile(SafeRelPath),
    /// `finish` was called before all chunks arrived.
    #[error("the transfer is incomplete: {missing_chunks} chunks are missing")]
    Incomplete {
        /// How many distinct chunks are still missing.
        missing_chunks: usize,
    },
}

fn conflicts_message(paths: &[SafeRelPath]) -> String {
    match paths {
        [one] => format!("`{one}` already exists"),
        [first, rest @ ..] => format!("`{first}` and {} other file(s) already exist", rest.len()),
        [] => "files already exist".into(),
    }
}

/// Statistics about a completed transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Number of files written.
    pub files: u64,
    /// Total bytes of the written files.
    pub bytes: u64,
}

struct Slot {
    entry: usize,
    offset: u64,
}

struct Wanted {
    len: u32,
    slots: Vec<Slot>,
}

struct FileProgress {
    missing_slots: usize,
    created: bool,
}

/// Receives chunks for one manifest and assembles them under a destination folder.
pub struct Sink {
    dest: PathBuf,
    partial_dir: PathBuf,
    manifest: Manifest,
    overwrite: OverwritePolicy,
    /// Distinct chunk hashes in manifest order, for a stable request order.
    order: Vec<Hash>,
    wanted: HashMap<Hash, Wanted>,
    progress: HashMap<usize, FileProgress>,
    /// Files already at their target with the right content.
    present: HashSet<usize>,
    /// The most recently written partial file, kept open because chunks are
    /// usually requested in file order.
    open: Option<(usize, File)>,
}

impl Sink {
    /// Prepares to receive `manifest` into the folder `dest` (created if missing).
    ///
    /// Fails before writing anything if the manifest is invalid or the
    /// destination has conflicts that `overwrite` does not allow.
    pub fn new(
        dest: &Path,
        manifest: Manifest,
        overwrite: OverwritePolicy,
    ) -> Result<Self, SinkError> {
        manifest.validate()?;
        match fs::metadata(dest) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(SinkError::DestinationNotAFolder(dest.to_path_buf())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(dest).map_err(io_err(dest))?;
            }
            Err(e) => return Err(io_err(dest)(e)),
        }

        let mut present = HashSet::new();
        let mut conflicts = Vec::new();
        for (index, entry) in manifest.entries.iter().enumerate() {
            match check_target(dest, entry)? {
                Target::Missing => {}
                Target::Identical => {
                    present.insert(index);
                }
                Target::Different => conflicts.push(entry.path.clone()),
            }
        }
        if !conflicts.is_empty() && overwrite == OverwritePolicy::Never {
            return Err(SinkError::Conflicts(conflicts));
        }

        let id = manifest.transfer_id()?;
        let partial_dir = dest.join(PARTIAL_DIR_NAME).join(id.to_hex_prefix(16));
        fs::create_dir_all(&partial_dir).map_err(io_err(&partial_dir))?;

        let mut order = Vec::new();
        let mut wanted: HashMap<Hash, Wanted> = HashMap::new();
        let mut progress = HashMap::new();
        for (index, _, file) in manifest.files() {
            if present.contains(&index) {
                continue;
            }
            let mut offset = 0u64;
            for chunk in &file.chunks {
                let w = wanted.entry(chunk.hash).or_insert_with(|| {
                    order.push(chunk.hash);
                    Wanted {
                        len: chunk.len,
                        slots: Vec::new(),
                    }
                });
                w.slots.push(Slot {
                    entry: index,
                    offset,
                });
                offset += u64::from(chunk.len);
            }
            progress.insert(
                index,
                FileProgress {
                    missing_slots: file.chunks.len(),
                    created: false,
                },
            );
        }

        let mut sink = Self {
            dest: dest.to_path_buf(),
            partial_dir,
            manifest,
            overwrite,
            order,
            wanted,
            progress,
            present,
            open: None,
        };
        sink.resume()?;
        Ok(sink)
    }

    /// Keeps every chunk an earlier attempt already wrote to a partial file.
    fn resume(&mut self) -> Result<(), SinkError> {
        let files: Vec<(usize, FileMeta)> = self
            .manifest
            .files()
            .filter(|(index, _, file)| !self.present.contains(index) && !file.chunks.is_empty())
            .map(|(index, _, file)| (index, file.clone()))
            .collect();
        let mut done = Vec::new();
        let mut buf = Vec::new();
        for (entry, file) in files {
            let path = self.partial_path(entry);
            let mut partial = match File::open(&path) {
                Ok(f) => BufReader::new(f),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_err(&path)(e)),
            };
            let mut offset = 0u64;
            for chunk in &file.chunks {
                buf.resize(chunk.len as usize, 0);
                // A short file just means the rest was never written.
                if partial.read_exact(&mut buf).is_err() {
                    break;
                }
                if Hash::of(&buf) == chunk.hash {
                    done.extend(self.fill(&chunk.hash, &buf, Some((entry, offset)))?);
                }
                offset += u64::from(chunk.len);
            }
            if let Some(p) = self.progress.get_mut(&entry) {
                // Its length was already set by the earlier attempt.
                p.created = true;
            }
        }
        // Only now: Windows cannot rename partial files that are still open.
        for entry in done {
            self.finish_file(entry)?;
        }
        Ok(())
    }

    /// The manifest being received.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Distinct chunks still needed, in manifest order.
    pub fn missing_chunks(&self) -> Vec<Hash> {
        self.order
            .iter()
            .filter(|h| self.wanted.contains_key(h))
            .copied()
            .collect()
    }

    /// Bytes still to be received over the wire (each distinct chunk once).
    pub fn missing_bytes(&self) -> u64 {
        self.wanted.values().map(|w| u64::from(w.len)).sum()
    }

    /// Whether every chunk has been received.
    pub fn is_complete(&self) -> bool {
        self.wanted.is_empty()
    }

    /// Verifies a received chunk and writes it everywhere it occurs.
    ///
    /// Files whose last chunk this was are verified and moved into place.
    pub fn write_chunk(&mut self, hash: &Hash, data: &[u8]) -> Result<(), SinkError> {
        if data.len() > MAX_CHUNK_SIZE as usize {
            return Err(SinkError::ChunkTooLarge(data.len()));
        }
        if Hash::of(data) != *hash {
            return Err(SinkError::CorruptChunk(*hash));
        }
        if !self.wanted.contains_key(hash) {
            return Err(SinkError::UnexpectedChunk(*hash));
        }
        for entry in self.fill(hash, data, None)? {
            self.finish_file(entry)?;
        }
        Ok(())
    }

    /// Writes a verified chunk to every slot that still needs it, except
    /// `on_disk` (an `(entry, offset)` that already holds it), and returns the
    /// files this completed.
    fn fill(
        &mut self,
        hash: &Hash,
        data: &[u8],
        on_disk: Option<(usize, u64)>,
    ) -> Result<Vec<usize>, SinkError> {
        let Some(wanted) = self.wanted.remove(hash) else {
            return Ok(Vec::new());
        };
        let mut done = Vec::new();
        for slot in &wanted.slots {
            if on_disk != Some((slot.entry, slot.offset)) {
                self.write_at(slot.entry, slot.offset, data)?;
            }
            if let Some(p) = self.progress.get_mut(&slot.entry) {
                p.missing_slots -= 1;
                if p.missing_slots == 0 {
                    done.push(slot.entry);
                }
            }
        }
        Ok(done)
    }

    /// Creates directories and empty files, then removes the partial folder.
    pub fn finish(mut self) -> Result<Summary, SinkError> {
        if !self.wanted.is_empty() {
            return Err(SinkError::Incomplete {
                missing_chunks: self.wanted.len(),
            });
        }
        self.open = None;

        let mut empty_files = Vec::new();
        for (index, entry) in self.manifest.entries.iter().enumerate() {
            match &entry.kind {
                EntryKind::Dir => {
                    let path = entry.path.to_path(&self.dest);
                    fs::create_dir_all(&path).map_err(io_err(&path))?;
                }
                EntryKind::File(f) if f.chunks.is_empty() && !self.present.contains(&index) => {
                    empty_files.push(index);
                }
                EntryKind::File(_) => {}
            }
        }
        for index in empty_files {
            let partial = self.partial_path(index);
            File::create(&partial).map_err(io_err(&partial))?;
            self.finish_file(index)?;
        }

        fs::remove_dir_all(&self.partial_dir).map_err(io_err(&self.partial_dir))?;
        // Fails harmlessly if another transfer into this folder is still in progress.
        let _ = fs::remove_dir(self.dest.join(PARTIAL_DIR_NAME));

        Ok(Summary {
            files: self.manifest.file_count(),
            bytes: self.manifest.total_bytes(),
        })
    }

    fn partial_path(&self, entry: usize) -> PathBuf {
        self.partial_dir.join(format!("{entry}.part"))
    }

    fn file_meta(&self, entry: usize) -> Option<(&SafeRelPath, &FileMeta)> {
        let e = self.manifest.entries.get(entry)?;
        match &e.kind {
            EntryKind::File(f) => Some((&e.path, f)),
            EntryKind::Dir => None,
        }
    }

    fn write_at(&mut self, entry: usize, offset: u64, data: &[u8]) -> Result<(), SinkError> {
        let path = self.partial_path(entry);
        let file = self.open_partial(entry, &path)?;
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.write_all(data))
            .map_err(io_err(&path))
    }

    fn open_partial(&mut self, entry: usize, path: &Path) -> Result<&mut File, SinkError> {
        let file = match self.open.take() {
            Some((open_entry, file)) if open_entry == entry => file,
            _ => {
                let file = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(path)
                    .map_err(io_err(path))?;
                let size = self.file_meta(entry).map_or(0, |(_, f)| f.size);
                if let Some(p) = self.progress.get_mut(&entry)
                    && !p.created
                {
                    file.set_len(size).map_err(io_err(path))?;
                    p.created = true;
                }
                file
            }
        };
        Ok(&mut self.open.insert((entry, file)).1)
    }

    /// Verifies a fully written partial file and moves it to its final path.
    fn finish_file(&mut self, entry: usize) -> Result<(), SinkError> {
        // Windows cannot rename a file that is still open.
        if matches!(self.open, Some((open_entry, _)) if open_entry == entry) {
            self.open = None;
        }
        let partial = self.partial_path(entry);
        let Some((rel, meta)) = self.file_meta(entry) else {
            return Ok(());
        };
        let (rel, meta) = (rel.clone(), meta.clone());

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&partial)
            .map_err(io_err(&partial))?;
        if hash_file(&file).map_err(io_err(&partial))? != meta.hash {
            return Err(SinkError::CorruptFile(rel));
        }
        if let Some(mtime) = meta.mtime.and_then(|t| t.to_system_time()) {
            file.set_modified(mtime).map_err(io_err(&partial))?;
        }
        set_executable(&file, meta.executable).map_err(io_err(&partial))?;
        drop(file);

        let target = rel.to_path(&self.dest);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        // Re-check right before the rename: the destination may have changed
        // since `new` looked at it.
        match fs::symlink_metadata(&target) {
            Ok(m) if m.file_type().is_symlink() => return Err(SinkError::Symlink(rel)),
            Ok(m) if m.is_dir() => return Err(SinkError::TypeConflict(rel)),
            Ok(_) if self.overwrite == OverwritePolicy::Never => {
                return Err(SinkError::Conflicts(vec![rel]));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&target)(e)),
        }
        // Atomic on the same volume; replaces an existing file on all platforms.
        fs::rename(&partial, &target).map_err(io_err(&target))
    }
}

/// Lists files in `dest` that receiving `manifest` would overwrite.
///
/// Errors (instead of listing) for problems overwriting cannot fix: symbolic
/// links at target paths and file/folder mismatches.
/// Files that already exist with exactly the sender's content are not
/// conflicts: there is nothing to overwrite.
pub fn find_conflicts(dest: &Path, manifest: &Manifest) -> Result<Vec<SafeRelPath>, SinkError> {
    let mut conflicts = Vec::new();
    for entry in &manifest.entries {
        if check_target(dest, entry)? == Target::Different {
            conflicts.push(entry.path.clone());
        }
    }
    Ok(conflicts)
}

/// What is at an entry's target path.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// Nothing, or the folder a folder entry wants.
    Missing,
    /// A file with exactly the expected content.
    Identical,
    /// A file with other content.
    Different,
}

/// Looks at an entry's target path; errors for what overwriting cannot fix.
fn check_target(dest: &Path, entry: &Entry) -> Result<Target, SinkError> {
    let target = entry.path.to_path(dest);
    let meta = match fs::symlink_metadata(&target) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Target::Missing),
        Err(e) => return Err(io_err(&target)(e)),
    };
    if meta.file_type().is_symlink() {
        return Err(SinkError::Symlink(entry.path.clone()));
    }
    match (&entry.kind, meta.is_dir()) {
        (EntryKind::Dir, true) => Ok(Target::Missing),
        // Hash only when the size matches; that rules most files out cheaply.
        (EntryKind::File(f), false) if meta.len() == f.size => {
            let file = File::open(&target).map_err(io_err(&target))?;
            if hash_file(&file).map_err(io_err(&target))? == f.hash {
                Ok(Target::Identical)
            } else {
                Ok(Target::Different)
            }
        }
        (EntryKind::File(_), false) => Ok(Target::Different),
        _ => Err(SinkError::TypeConflict(entry.path.clone())),
    }
}

fn hash_file(file: &File) -> io::Result<Hash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file)?;
    Ok(hasher.finalize().into())
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> SinkError + '_ {
    move |source| SinkError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(unix)]
fn set_executable(file: &File, executable: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if executable {
        let mut perms = file.metadata()?.permissions();
        // Grant execute wherever read is granted, like `chmod +x` under a normal umask.
        let mode = perms.mode();
        perms.set_mode(mode | ((mode & 0o444) >> 2));
        file.set_permissions(perms)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_file: &File, _executable: bool) -> io::Result<()> {
    Ok(())
}
