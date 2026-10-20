//! Manifests: the complete description of what is being transferred.
//!
//! A manifest lists every directory and file under one top-level name, with
//! each file described by its size, metadata, whole-file hash and the list of
//! its chunks. The receiver uses it to decide which chunks it still needs and
//! to verify everything it writes.
//!
//! Invariants (enforced by [`Manifest::validate`], which runs on every
//! manifest decoded from bytes):
//! - every path is a [`SafeRelPath`] and the first entry is the single root,
//! - each entry's parent directory appears earlier in the list,
//! - no two paths collide, even case-insensitively,
//! - each file's chunk lengths are in range and sum to its size.

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::chunk::{ChunkRef, MAX_CHUNK_SIZE};
use crate::hash::Hash;
use crate::path::SafeRelPath;

/// Version of the manifest format.
pub const MANIFEST_VERSION: u16 = 1;
/// Maximum number of entries (files plus directories) in one manifest.
pub const MAX_ENTRIES: usize = 1_000_000;
/// Maximum encoded size of a manifest received from the network.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024 * 1024;

/// A complete description of a file or folder being transferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Format version; see [`MANIFEST_VERSION`].
    pub version: u16,
    /// All entries, root first, parents before children.
    pub entries: Vec<Entry>,
}

/// One directory or file in a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Path relative to the destination, starting with the root name.
    pub path: SafeRelPath,
    /// Whether this is a directory or a file.
    pub kind: EntryKind,
}

/// The kind of an [`Entry`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    /// A directory. Listed explicitly so empty directories are transferred.
    Dir,
    /// A regular file.
    File(FileMeta),
}

/// Content and metadata of a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    /// Size in bytes.
    pub size: u64,
    /// Last modification time, if known.
    pub mtime: Option<Timestamp>,
    /// Whether any execute bit was set (Unix only; always `false` from Windows).
    pub executable: bool,
    /// BLAKE3 hash of the whole content.
    pub hash: Hash,
    /// The content's chunks, in order.
    pub chunks: Vec<ChunkRef>,
}

/// A point in time relative to the Unix epoch, independent of the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timestamp {
    /// Whole seconds since 1970-01-01 UTC (negative before).
    pub secs: i64,
    /// Nanoseconds to add to `secs`, below 1,000,000,000.
    pub nanos: u32,
}

const NANOS_PER_SEC: u32 = 1_000_000_000;

impl Timestamp {
    /// Converts from a [`SystemTime`]. `None` if out of range.
    pub fn from_system_time(t: SystemTime) -> Option<Self> {
        match t.duration_since(UNIX_EPOCH) {
            Ok(d) => Some(Self {
                secs: i64::try_from(d.as_secs()).ok()?,
                nanos: d.subsec_nanos(),
            }),
            Err(before) => {
                let d = before.duration();
                let secs = i64::try_from(d.as_secs()).ok()?;
                Some(if d.subsec_nanos() == 0 {
                    Self {
                        secs: -secs,
                        nanos: 0,
                    }
                } else {
                    Self {
                        secs: -secs - 1,
                        nanos: NANOS_PER_SEC - d.subsec_nanos(),
                    }
                })
            }
        }
    }

    /// Converts to a [`SystemTime`]. `None` if invalid or out of range for this OS.
    pub fn to_system_time(self) -> Option<SystemTime> {
        if self.nanos >= NANOS_PER_SEC {
            return None;
        }
        let whole = Duration::from_secs(self.secs.unsigned_abs());
        let base = if self.secs >= 0 {
            UNIX_EPOCH.checked_add(whole)?
        } else {
            UNIX_EPOCH.checked_sub(whole)?
        };
        base.checked_add(Duration::from_nanos(u64::from(self.nanos)))
    }
}

/// Why a manifest is invalid or could not be (de)serialized.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The peer uses a manifest format we do not understand.
    #[error("unsupported manifest version {found} (this build understands {MANIFEST_VERSION})")]
    UnsupportedVersion {
        /// The version found in the manifest.
        found: u16,
    },
    /// The manifest has no entries.
    #[error("the manifest is empty")]
    Empty,
    /// The manifest has more than [`MAX_ENTRIES`] entries.
    #[error("the manifest has more than {MAX_ENTRIES} entries")]
    TooManyEntries,
    /// The encoded manifest is larger than [`MAX_MANIFEST_BYTES`].
    #[error("the manifest is larger than {MAX_MANIFEST_BYTES} bytes")]
    TooLarge,
    /// The manifest has more than one top-level entry.
    #[error("the manifest has more than one top-level entry (`{0}`)")]
    MultipleRoots(SafeRelPath),
    /// Two entries have the same path, possibly differing only in case.
    #[error("two entries collide at `{0}` (names must differ by more than letter case)")]
    Duplicate(SafeRelPath),
    /// An entry's parent directory is not listed before it.
    #[error("`{0}` has no parent directory listed before it")]
    MissingParent(SafeRelPath),
    /// A chunk is empty or larger than [`MAX_CHUNK_SIZE`].
    #[error("`{0}` has a chunk with an invalid size")]
    InvalidChunkSize(SafeRelPath),
    /// A file's chunk lengths do not add up to its size.
    #[error("`{0}`: chunk sizes do not add up to the file size")]
    SizeMismatch(SafeRelPath),
    /// A timestamp has an out-of-range nanosecond field.
    #[error("`{0}` has an invalid modification time")]
    InvalidTimestamp(SafeRelPath),
    /// Encoding or decoding failed.
    #[error("the manifest could not be encoded or decoded: {0}")]
    Codec(#[from] postcard::Error),
}

impl Manifest {
    /// The top-level name (folder or file name) of the transfer.
    pub fn name(&self) -> &str {
        self.entries
            .first()
            .map_or("", |e| e.path.first_component())
    }

    /// Whether the transfer is a single file rather than a folder.
    pub fn is_single_file(&self) -> bool {
        matches!(
            self.entries.first(),
            Some(Entry {
                kind: EntryKind::File(_),
                ..
            })
        )
    }

    /// Iterates over files as `(entry index, entry, file metadata)`.
    pub fn files(&self) -> impl Iterator<Item = (usize, &Entry, &FileMeta)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match &e.kind {
                EntryKind::File(f) => Some((i, e, f)),
                EntryKind::Dir => None,
            })
    }

    /// Number of files.
    pub fn file_count(&self) -> u64 {
        self.files().count() as u64
    }

    /// Sum of all file sizes.
    pub fn total_bytes(&self) -> u64 {
        self.files().map(|(_, _, f)| f.size).sum()
    }

    /// Canonical binary encoding (postcard).
    pub fn to_bytes(&self) -> Result<Vec<u8>, ManifestError> {
        Ok(postcard::to_allocvec(self)?)
    }

    /// Decodes and validates a manifest received from an untrusted peer.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ManifestError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge);
        }
        let manifest: Manifest = postcard::from_bytes(bytes)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Identifies this exact transfer: the BLAKE3 hash of the canonical encoding.
    ///
    /// Re-running a send of unchanged data yields the same id, which is how
    /// the receiver finds its resume state.
    pub fn transfer_id(&self) -> Result<Hash, ManifestError> {
        Ok(Hash::of(&self.to_bytes()?))
    }

    /// Checks all structural invariants (see the module docs).
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.version != MANIFEST_VERSION {
            return Err(ManifestError::UnsupportedVersion {
                found: self.version,
            });
        }
        if self.entries.is_empty() {
            return Err(ManifestError::Empty);
        }
        if self.entries.len() > MAX_ENTRIES {
            return Err(ManifestError::TooManyEntries);
        }

        let mut seen = HashSet::with_capacity(self.entries.len());
        let mut dirs: HashSet<&str> = HashSet::new();
        for (i, entry) in self.entries.iter().enumerate() {
            let path = &entry.path;
            if !seen.insert(path.collision_key()) {
                return Err(ManifestError::Duplicate(path.clone()));
            }
            match path.parent() {
                None if i == 0 => {}
                None => return Err(ManifestError::MultipleRoots(path.clone())),
                Some(parent) if dirs.contains(parent.as_str()) => {}
                Some(_) => return Err(ManifestError::MissingParent(path.clone())),
            }
            match &entry.kind {
                EntryKind::Dir => {
                    dirs.insert(path.as_str());
                }
                EntryKind::File(file) => validate_file(path, file)?,
            }
        }
        Ok(())
    }
}

fn validate_file(path: &SafeRelPath, file: &FileMeta) -> Result<(), ManifestError> {
    let mut total = 0u64;
    for chunk in &file.chunks {
        if chunk.len == 0 || chunk.len > MAX_CHUNK_SIZE {
            return Err(ManifestError::InvalidChunkSize(path.clone()));
        }
        total += u64::from(chunk.len);
    }
    if total != file.size {
        return Err(ManifestError::SizeMismatch(path.clone()));
    }
    if file.mtime.is_some_and(|t| t.nanos >= NANOS_PER_SEC) {
        return Err(ManifestError::InvalidTimestamp(path.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> SafeRelPath {
        SafeRelPath::new(s).unwrap()
    }

    fn dir(s: &str) -> Entry {
        Entry {
            path: p(s),
            kind: EntryKind::Dir,
        }
    }

    fn file(s: &str, content: &[u8]) -> Entry {
        let chunked = crate::chunk::chunk_reader(content).unwrap();
        Entry {
            path: p(s),
            kind: EntryKind::File(FileMeta {
                size: chunked.size,
                mtime: Some(Timestamp {
                    secs: 1_700_000_000,
                    nanos: 5,
                }),
                executable: false,
                hash: chunked.hash,
                chunks: chunked.chunks,
            }),
        }
    }

    fn manifest(entries: Vec<Entry>) -> Manifest {
        Manifest {
            version: MANIFEST_VERSION,
            entries,
        }
    }

    fn sample() -> Manifest {
        manifest(vec![
            dir("proj"),
            file("proj/a.txt", b"hello"),
            dir("proj/sub"),
            file("proj/sub/b.bin", b""),
        ])
    }

    #[test]
    fn valid_manifest_roundtrips() {
        let m = sample();
        m.validate().unwrap();
        let decoded = Manifest::from_bytes(&m.to_bytes().unwrap()).unwrap();
        assert_eq!(decoded, m);
        assert_eq!(decoded.transfer_id().unwrap(), m.transfer_id().unwrap());
        assert_eq!(m.name(), "proj");
        assert_eq!(m.file_count(), 2);
        assert_eq!(m.total_bytes(), 5);
        assert!(!m.is_single_file());
    }

    #[test]
    fn single_file_manifest() {
        let m = manifest(vec![file("notes.txt", b"x")]);
        m.validate().unwrap();
        assert!(m.is_single_file());
    }

    #[test]
    fn transfer_id_changes_with_content() {
        let mut other = sample();
        other.entries[1] = file("proj/a.txt", b"HELLO");
        assert_ne!(
            sample().transfer_id().unwrap(),
            other.transfer_id().unwrap()
        );
    }

    #[test]
    fn rejects_wrong_version() {
        let mut m = sample();
        m.version = 99;
        assert!(matches!(
            m.validate(),
            Err(ManifestError::UnsupportedVersion { found: 99 })
        ));
    }

    #[test]
    fn rejects_empty() {
        assert!(matches!(
            manifest(vec![]).validate(),
            Err(ManifestError::Empty)
        ));
    }

    #[test]
    fn rejects_duplicates_and_case_collisions() {
        let m = manifest(vec![
            dir("proj"),
            file("proj/a", b"1"),
            file("proj/a", b"2"),
        ]);
        assert!(matches!(m.validate(), Err(ManifestError::Duplicate(_))));
        let m = manifest(vec![
            dir("proj"),
            file("proj/A.txt", b"1"),
            file("proj/a.TXT", b"2"),
        ]);
        assert!(matches!(m.validate(), Err(ManifestError::Duplicate(_))));
    }

    #[test]
    fn rejects_multiple_roots() {
        let m = manifest(vec![dir("a"), dir("b")]);
        assert!(matches!(m.validate(), Err(ManifestError::MultipleRoots(_))));
    }

    #[test]
    fn rejects_missing_or_late_parent() {
        let m = manifest(vec![dir("proj"), file("proj/sub/x", b"1")]);
        assert!(matches!(m.validate(), Err(ManifestError::MissingParent(_))));
        let m = manifest(vec![dir("proj"), file("proj/sub/x", b"1"), dir("proj/sub")]);
        assert!(matches!(m.validate(), Err(ManifestError::MissingParent(_))));
    }

    #[test]
    fn rejects_children_of_a_file() {
        let m = manifest(vec![file("proj", b"1"), file("proj/x", b"1")]);
        assert!(matches!(m.validate(), Err(ManifestError::MissingParent(_))));
    }

    #[test]
    fn rejects_bad_sizes() {
        let mut m = sample();
        if let EntryKind::File(f) = &mut m.entries[1].kind {
            f.size += 1;
        }
        assert!(matches!(m.validate(), Err(ManifestError::SizeMismatch(_))));

        let mut m = sample();
        if let EntryKind::File(f) = &mut m.entries[1].kind {
            f.chunks[0].len = MAX_CHUNK_SIZE + 1;
            f.size = u64::from(MAX_CHUNK_SIZE) + 1;
        }
        assert!(matches!(
            m.validate(),
            Err(ManifestError::InvalidChunkSize(_))
        ));
    }

    #[test]
    fn rejects_bad_timestamp() {
        let mut m = sample();
        if let EntryKind::File(f) = &mut m.entries[1].kind {
            f.mtime = Some(Timestamp {
                secs: 0,
                nanos: NANOS_PER_SEC,
            });
        }
        assert!(matches!(
            m.validate(),
            Err(ManifestError::InvalidTimestamp(_))
        ));
    }

    /// A malicious peer can put any string in the path field; decoding must refuse it.
    #[test]
    fn decoding_rejects_unsafe_paths() {
        #[derive(Serialize)]
        struct RawManifest {
            version: u16,
            entries: Vec<RawEntry>,
        }
        #[derive(Serialize)]
        struct RawEntry {
            path: String,
            kind: RawKind,
        }
        #[derive(Serialize)]
        enum RawKind {
            Dir,
        }
        for evil in ["../escape", "/etc", "C:\\Windows", "proj/../../x", "CON"] {
            let raw = RawManifest {
                version: MANIFEST_VERSION,
                entries: vec![RawEntry {
                    path: evil.into(),
                    kind: RawKind::Dir,
                }],
            };
            let bytes = postcard::to_allocvec(&raw).unwrap();
            assert!(
                matches!(Manifest::from_bytes(&bytes), Err(ManifestError::Codec(_))),
                "{evil}"
            );
        }
    }

    #[test]
    fn decoding_rejects_garbage_and_oversize() {
        assert!(Manifest::from_bytes(&[0xff; 64]).is_err());
        // Claims a huge entry count but has no data: must fail without allocating it.
        assert!(Manifest::from_bytes(&[1, 0xff, 0xff, 0xff, 0xff, 0x0f]).is_err());
        assert!(matches!(
            Manifest::from_bytes(&vec![0; MAX_MANIFEST_BYTES + 1]),
            Err(ManifestError::TooLarge)
        ));
    }

    #[test]
    fn timestamps_roundtrip() {
        for t in [
            UNIX_EPOCH,
            UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789),
            UNIX_EPOCH - Duration::new(5, 0),
            UNIX_EPOCH - Duration::new(5, 250),
        ] {
            let ts = Timestamp::from_system_time(t).unwrap();
            assert!(ts.nanos < NANOS_PER_SEC);
            assert_eq!(ts.to_system_time(), Some(t));
        }
        assert_eq!(
            Timestamp {
                secs: 0,
                nanos: NANOS_PER_SEC
            }
            .to_system_time(),
            None
        );
    }
}
