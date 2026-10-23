//! Serving chunks from disk (sender side).

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::hash::Hash;
use crate::manifest::Manifest;

/// Why a chunk could not be served.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The peer asked for a chunk that is not in the manifest.
    #[error("the receiver asked for unknown chunk {0}")]
    UnknownChunk(Hash),
    /// The file no longer matches what was scanned.
    #[error("`{}` changed while it was being sent", .0.display())]
    Changed(PathBuf),
    /// Reading the file failed.
    #[error("could not read `{}`: {source}", path.display())]
    Io {
        /// The file being read.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
}

/// A file kept open between [`Source::read_chunk_with`] calls; one per reader.
#[derive(Debug, Default)]
pub struct OpenFile(Option<(usize, File)>);

struct Location {
    file: usize,
    offset: u64,
    len: u32,
}

/// Reads chunks of a scanned file or folder by hash.
///
/// Each distinct chunk is mapped to one place it occurs, so a chunk that
/// appears in several files (or several times in one) is only ever sent once.
pub struct Source {
    files: Vec<PathBuf>,
    locations: HashMap<Hash, Location>,
}

impl Source {
    /// Indexes `manifest`, which must have been scanned from `roots` (see
    /// [`crate::Scan::roots`]): one path on disk per top-level entry.
    pub fn new(roots: &[PathBuf], manifest: &Manifest) -> Self {
        let on_disk: HashMap<&str, &Path> = manifest
            .roots()
            .map(|e| e.path.as_str())
            .zip(roots.iter().map(PathBuf::as_path))
            .collect();
        let mut files = Vec::new();
        let mut locations = HashMap::new();
        for (_, entry, meta) in manifest.files() {
            // The first component is the name of one of the roots.
            let Some(root) = on_disk.get(entry.path.first_component()) else {
                continue;
            };
            let mut path = root.to_path_buf();
            path.extend(entry.path.components().skip(1));
            let file = files.len();
            files.push(path);
            let mut offset = 0u64;
            for chunk in &meta.chunks {
                locations.entry(chunk.hash).or_insert(Location {
                    file,
                    offset,
                    len: chunk.len,
                });
                offset += u64::from(chunk.len);
            }
        }
        Self { files, locations }
    }

    /// Number of files, for numbering them with [`Source::file_of`].
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// The number (below [`Source::file_count`]) of the file a chunk is read from.
    pub fn file_of(&self, hash: &Hash) -> Option<usize> {
        self.locations.get(hash).map(|loc| loc.file)
    }

    /// Reads one chunk and re-checks its hash, so a file that was modified
    /// after scanning is reported instead of sending wrong data.
    pub fn read_chunk(&self, hash: &Hash) -> Result<Vec<u8>, SourceError> {
        self.read_chunk_with(hash, &mut OpenFile::default())
    }

    /// Like [`Source::read_chunk`], keeping the file open in `open` for the
    /// next chunk: chunks are mostly requested in file order, and opening a
    /// file per chunk is slow (on Windows especially).
    pub fn read_chunk_with(
        &self,
        hash: &Hash,
        open: &mut OpenFile,
    ) -> Result<Vec<u8>, SourceError> {
        let loc = self
            .locations
            .get(hash)
            .ok_or(SourceError::UnknownChunk(*hash))?;
        let path = &self.files[loc.file];
        let io_err = |source| SourceError::Io {
            path: path.clone(),
            source,
        };
        let file = match open.0.take() {
            Some((index, file)) if index == loc.file => file,
            _ => File::open(path).map_err(io_err)?,
        };
        let file = &mut open.0.insert((loc.file, file)).1;
        file.seek(SeekFrom::Start(loc.offset)).map_err(io_err)?;
        let mut buf = vec![0; loc.len as usize];
        match file.read_exact(&mut buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(SourceError::Changed(path.clone()));
            }
            Err(e) => return Err(io_err(e)),
        }
        if Hash::of(&buf) != *hash {
            return Err(SourceError::Changed(path.clone()));
        }
        Ok(buf)
    }
}
