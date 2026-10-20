//! Safe relative paths.
//!
//! Every path that comes from the network passes through [`SafeRelPath`]
//! before it can touch the filesystem. The rules are the strictest common
//! denominator of Windows, macOS and Linux: a manifest built on one OS can
//! always be written on another, and no path can escape the destination.
//!
//! A path is `/`-separated UTF-8 and every component must:
//! - be non-empty and not `.` or `..` (rules out absolute paths and traversal),
//! - contain no control characters and none of `< > : " / \ | ? *`
//!   (rules out drive letters `C:`, UNC `\\server`, `\\?\` prefixes and
//!   NTFS alternate data streams `file:stream`),
//! - not end in a space or dot (Windows silently strips those),
//! - not be a reserved Windows device name such as `CON` or `nul.txt`.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Maximum length of a whole path, in bytes.
pub const MAX_PATH_LEN: usize = 4096;
/// Maximum length of one path component, in bytes.
pub const MAX_COMPONENT_LEN: usize = 255;
/// Maximum number of components in a path.
pub const MAX_DEPTH: usize = 128;
/// Directory inside the destination where Portal keeps in-progress files.
/// It cannot be used as the top-level name of a transfer.
pub const PARTIAL_DIR_NAME: &str = ".portal-partial";

const FORBIDDEN_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Device names Windows reserves in every directory, with or without an extension.
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// Why a path was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// The path is the empty string.
    #[error("the path is empty")]
    Empty,
    /// The path exceeds [`MAX_PATH_LEN`].
    #[error("the path is longer than {MAX_PATH_LEN} bytes")]
    TooLong,
    /// The path has more than [`MAX_DEPTH`] components.
    #[error("the path is nested deeper than {MAX_DEPTH} levels")]
    TooDeep,
    /// A component is empty: a leading `/` (absolute path) or a doubled `//`.
    #[error("the path is absolute or contains an empty component")]
    EmptyComponent,
    /// A component is `.` or `..`.
    #[error("the path contains `.` or `..`")]
    DotComponent,
    /// A component exceeds [`MAX_COMPONENT_LEN`].
    #[error("a name in the path is longer than {MAX_COMPONENT_LEN} bytes")]
    ComponentTooLong,
    /// A component contains a character that is not allowed on every platform.
    #[error("the path contains the character {0:?}, which is not allowed")]
    ForbiddenChar(char),
    /// A component ends in a space or dot, which Windows cannot store.
    #[error("the name {0:?} ends with a space or dot")]
    TrailingDotOrSpace(String),
    /// A component is a reserved Windows device name.
    #[error("the name {0:?} is reserved on Windows")]
    ReservedName(String),
    /// The top-level component is [`PARTIAL_DIR_NAME`].
    #[error("the name {PARTIAL_DIR_NAME:?} is reserved for Portal's own use")]
    ReservedByPortal,
    /// The path is not valid UTF-8.
    #[error("the path is not valid UTF-8")]
    NotUtf8,
    /// A filesystem path contained a root, prefix, `.` or `..` component.
    #[error("the path is not a plain relative path")]
    NotRelative,
}

/// A validated, portable, relative path such as `project/src/main.rs`.
///
/// Can only be constructed through validation, including when deserialized,
/// so holding one is proof that it is safe to join onto a destination.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SafeRelPath(String);

impl SafeRelPath {
    /// Validates a `/`-separated path.
    pub fn new(path: &str) -> Result<Self, PathError> {
        if path.is_empty() {
            return Err(PathError::Empty);
        }
        if path.len() > MAX_PATH_LEN {
            return Err(PathError::TooLong);
        }
        for (depth, component) in path.split('/').enumerate() {
            if depth >= MAX_DEPTH {
                return Err(PathError::TooDeep);
            }
            validate_component(component)?;
            if depth == 0 && component.eq_ignore_ascii_case(PARTIAL_DIR_NAME) {
                return Err(PathError::ReservedByPortal);
            }
        }
        Ok(Self(path.to_owned()))
    }

    /// Converts a relative filesystem path (e.g. from `strip_prefix`).
    pub fn from_relative_path(path: &Path) -> Result<Self, PathError> {
        Self::new(&to_slash(path)?)
    }

    /// Appends a relative filesystem path. An empty `rel` returns `self`.
    pub fn join_path(&self, rel: &Path) -> Result<Self, PathError> {
        let rel = to_slash(rel)?;
        if rel.is_empty() {
            return Ok(self.clone());
        }
        Self::new(&format!("{}/{}", self.0, rel))
    }

    /// The path as a `/`-separated string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The components of the path, in order.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// The first component: the name of the file or folder being transferred.
    pub fn first_component(&self) -> &str {
        self.0.split('/').next().unwrap_or(&self.0)
    }

    /// The last component.
    pub fn file_name(&self) -> &str {
        self.0.rsplit_once('/').map_or(&self.0, |(_, name)| name)
    }

    /// The parent path, or `None` for a single-component path.
    pub fn parent(&self) -> Option<SafeRelPath> {
        // A prefix ending at a separator consists of valid components, so it is valid too.
        self.0
            .rsplit_once('/')
            .map(|(parent, _)| SafeRelPath(parent.to_owned()))
    }

    /// Joins this path onto `root`, component by component.
    pub fn to_path(&self, root: &Path) -> PathBuf {
        let mut path = root.to_path_buf();
        path.extend(self.components());
        path
    }

    /// Key under which two paths collide on a case-insensitive filesystem
    /// (Windows, default macOS).
    pub fn collision_key(&self) -> String {
        self.0.to_lowercase()
    }
}

impl fmt::Display for SafeRelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for SafeRelPath {
    type Error = PathError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(&s)
    }
}

impl From<SafeRelPath> for String {
    fn from(p: SafeRelPath) -> Self {
        p.0
    }
}

fn validate_component(c: &str) -> Result<(), PathError> {
    if c.is_empty() {
        return Err(PathError::EmptyComponent);
    }
    if c == "." || c == ".." {
        return Err(PathError::DotComponent);
    }
    if c.len() > MAX_COMPONENT_LEN {
        return Err(PathError::ComponentTooLong);
    }
    if let Some(ch) = c
        .chars()
        .find(|ch| ch.is_control() || FORBIDDEN_CHARS.contains(ch))
    {
        return Err(PathError::ForbiddenChar(ch));
    }
    if c.ends_with(' ') || c.ends_with('.') {
        return Err(PathError::TrailingDotOrSpace(c.to_owned()));
    }
    // Windows ignores the extension and trailing spaces: `nul.txt` and `CON .log` are devices.
    let stem = c.split('.').next().unwrap_or(c).trim_end_matches(' ');
    if RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return Err(PathError::ReservedName(c.to_owned()));
    }
    Ok(())
}

fn to_slash(path: &Path) -> Result<String, PathError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => parts.push(name.to_str().ok_or(PathError::NotUtf8)?),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => return Err(PathError::NotRelative),
        }
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn err(p: &str) -> PathError {
        SafeRelPath::new(p).expect_err(p)
    }

    #[test]
    fn accepts_normal_paths() {
        for p in [
            "a",
            "project/src/main.rs",
            ".hidden",
            "a b/c d.txt",
            "ünïcødé/文件.txt",
            "CONSOLE",
            "COM10",
            "nul_file",
            "x..y",
        ] {
            assert_eq!(SafeRelPath::new(p).unwrap().as_str(), p);
        }
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(err(""), PathError::Empty);
    }

    #[test]
    fn rejects_traversal() {
        assert_eq!(err(".."), PathError::DotComponent);
        assert_eq!(err("../etc/passwd"), PathError::DotComponent);
        assert_eq!(err("a/../../b"), PathError::DotComponent);
        assert_eq!(err("a/./b"), PathError::DotComponent);
        assert_eq!(err("a/.."), PathError::DotComponent);
    }

    #[test]
    fn rejects_absolute_unix_paths() {
        assert_eq!(err("/etc/passwd"), PathError::EmptyComponent);
        assert_eq!(err("a//b"), PathError::EmptyComponent);
        assert_eq!(err("a/"), PathError::EmptyComponent);
    }

    #[test]
    fn rejects_windows_drive_letters() {
        assert_eq!(err("C:"), PathError::ForbiddenChar(':'));
        assert_eq!(err("C:/Windows/System32"), PathError::ForbiddenChar(':'));
        assert_eq!(err("C:file"), PathError::ForbiddenChar(':'));
        assert_eq!(err("c:\\x"), PathError::ForbiddenChar(':'));
    }

    #[test]
    fn rejects_backslashes_and_unc() {
        assert_eq!(err("a\\b"), PathError::ForbiddenChar('\\'));
        assert_eq!(err("..\\..\\x"), PathError::ForbiddenChar('\\'));
        assert_eq!(err("\\\\server\\share"), PathError::ForbiddenChar('\\'));
        assert_eq!(err("\\\\?\\C:\\x"), PathError::ForbiddenChar('\\'));
        assert_eq!(err("\\\\.\\PhysicalDrive0"), PathError::ForbiddenChar('\\'));
    }

    #[test]
    fn rejects_alternate_data_streams() {
        assert_eq!(err("file.txt:secret"), PathError::ForbiddenChar(':'));
        assert_eq!(err("file.txt::$DATA"), PathError::ForbiddenChar(':'));
    }

    #[test]
    fn rejects_other_forbidden_chars() {
        for c in [
            '<', '>', '"', '|', '?', '*', '\0', '\n', '\r', '\t', '\u{7f}', '\u{85}',
        ] {
            assert_eq!(err(&format!("a{c}b")), PathError::ForbiddenChar(c), "{c:?}");
        }
    }

    #[test]
    fn rejects_trailing_dot_or_space() {
        assert!(matches!(err("file."), PathError::TrailingDotOrSpace(_)));
        assert!(matches!(err("file "), PathError::TrailingDotOrSpace(_)));
        assert!(matches!(err("dir./x"), PathError::TrailingDotOrSpace(_)));
        assert!(matches!(err("..."), PathError::TrailingDotOrSpace(_)));
    }

    #[test]
    fn rejects_reserved_windows_names() {
        for p in [
            "CON",
            "con",
            "Nul",
            "aux.txt",
            "a/PRN.tar.gz",
            "COM1",
            "lpt9.log",
            "COM0",
            "COM¹",
            "lpt³.x",
            "CONIN$",
            "conout$.txt",
            "CON .txt",
        ] {
            assert!(matches!(err(p), PathError::ReservedName(_)), "{p}");
        }
    }

    #[test]
    fn rejects_portal_partial_at_top_level_only() {
        assert_eq!(err(".portal-partial"), PathError::ReservedByPortal);
        assert_eq!(err(".PORTAL-PARTIAL/x"), PathError::ReservedByPortal);
        assert!(SafeRelPath::new("a/.portal-partial").is_ok());
    }

    #[test]
    fn rejects_overlong_paths() {
        assert_eq!(err(&"a".repeat(256)), PathError::ComponentTooLong);
        assert!(SafeRelPath::new(&"a".repeat(255)).is_ok());
        let long = vec!["abcdefgh"; 600].join("/");
        assert_eq!(err(&long), PathError::TooLong);
        let deep = vec!["a"; MAX_DEPTH + 1].join("/");
        assert_eq!(err(&deep), PathError::TooDeep);
        assert!(SafeRelPath::new(&vec!["a"; MAX_DEPTH].join("/")).is_ok());
    }

    #[test]
    fn deserialization_validates() {
        let bytes = postcard::to_allocvec(&"../evil".to_string()).unwrap();
        assert!(postcard::from_bytes::<SafeRelPath>(&bytes).is_err());
        let bytes = postcard::to_allocvec(&"ok/path".to_string()).unwrap();
        let p: SafeRelPath = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(p.as_str(), "ok/path");
    }

    #[test]
    fn from_relative_path_rejects_non_relative() {
        assert_eq!(
            SafeRelPath::from_relative_path(Path::new("../x")),
            Err(PathError::NotRelative)
        );
        // `Path::components` keeps a leading `.`; we treat it as not plain relative.
        assert_eq!(
            SafeRelPath::from_relative_path(Path::new("./x")),
            Err(PathError::NotRelative)
        );
        let abs = if cfg!(windows) { "C:\\x" } else { "/x" };
        assert_eq!(
            SafeRelPath::from_relative_path(Path::new(abs)),
            Err(PathError::NotRelative)
        );
        let nested: PathBuf = ["a", "b", "c.txt"].iter().collect();
        assert_eq!(
            SafeRelPath::from_relative_path(&nested).unwrap().as_str(),
            "a/b/c.txt"
        );
    }

    #[test]
    fn helpers() {
        let p = SafeRelPath::new("proj/src/main.rs").unwrap();
        assert_eq!(p.first_component(), "proj");
        assert_eq!(p.file_name(), "main.rs");
        assert_eq!(p.parent().unwrap().as_str(), "proj/src");
        assert_eq!(SafeRelPath::new("proj").unwrap().parent(), None);
        assert_eq!(
            p.join_path(Path::new("")).unwrap(),
            p,
            "joining nothing is identity"
        );
        assert_eq!(
            SafeRelPath::new("Proj/A.txt").unwrap().collision_key(),
            SafeRelPath::new("proj/a.TXT").unwrap().collision_key()
        );
    }

    proptest! {
        /// Whatever string an attacker sends, an accepted path stays inside the root.
        #[test]
        fn accepted_paths_never_escape(s in "\\PC{0,64}") {
            if let Ok(p) = SafeRelPath::new(&s) {
                let root = Path::new("root");
                let joined = p.to_path(root);
                prop_assert!(joined.starts_with(root));
                prop_assert_eq!(joined.components().count(), p.components().count() + 1);
                for c in joined.components() {
                    prop_assert!(matches!(c, Component::Normal(_)));
                }
            }
        }

        /// Same, with inputs biased toward path-like syntax.
        #[test]
        fn path_like_inputs_never_escape(parts in prop::collection::vec(
            prop::sample::select(vec![
                "a", "..", ".", "", "C:", "\\", "\\\\srv", "nul", "x.", "b ", "/", "?", "ok.txt",
            ]),
            1..8,
        ), sep in prop::sample::select(vec!["/", "\\", ""])) {
            let s = parts.join(sep);
            if let Ok(p) = SafeRelPath::new(&s) {
                let joined = p.to_path(Path::new("root"));
                for c in joined.components() {
                    prop_assert!(matches!(c, Component::Normal(_)));
                }
            }
        }

        /// Ordinary names always validate and round-trip.
        #[test]
        fn simple_paths_roundtrip(parts in prop::collection::vec("[a-zA-Z0-9_-][a-zA-Z0-9_. -]{0,20}[a-zA-Z0-9_-]", 1..10)) {
            let s = parts.join("/");
            prop_assume!(!parts.iter().any(|c| {
                let stem = c.split('.').next().unwrap_or(c).trim_end_matches(' ');
                RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(stem))
            }));
            prop_assume!(!parts[0].eq_ignore_ascii_case(PARTIAL_DIR_NAME));
            let p = SafeRelPath::new(&s).unwrap();
            prop_assert_eq!(p.as_str(), s.as_str());
        }
    }
}
