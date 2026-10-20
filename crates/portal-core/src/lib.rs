//! Core library for Portal.
//!
//! This crate holds everything about *what* gets transferred, independent of
//! *how* bytes move: content-defined chunking, manifests, hashing,
//! path sanitization, the transfer state machine and resume logic.
//!
//! It deliberately contains no networking and no CLI code, so it can be
//! tested by "transferring" between two directories in a single process.
#![forbid(unsafe_code)]
