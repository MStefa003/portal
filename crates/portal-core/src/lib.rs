//! Core library for Portal.
//!
//! This crate holds everything about *what* gets transferred, independent of
//! *how* bytes move: content-defined chunking, manifests, hashing, path
//! sanitization, and the two ends of a transfer:
//!
//! - the sender [`scan`]s a file or folder into a [`Manifest`] and serves
//!   chunks from a [`Source`];
//! - the receiver feeds chunks into a [`Sink`], which verifies and writes them.
//!
//! It contains no networking and no CLI code. The network layer only has to
//! carry the manifest one way and chunk requests/chunks back and forth, so the
//! whole pipeline can be tested in a single process between two folders.
#![forbid(unsafe_code)]

pub mod chunk;
pub mod hash;
pub mod manifest;
pub mod path;
pub mod scan;

pub use hash::Hash;
pub use manifest::Manifest;
pub use path::SafeRelPath;
pub use scan::{Scan, ScanOptions, scan};
