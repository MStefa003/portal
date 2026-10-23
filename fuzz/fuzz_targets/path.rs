//! Paths from manifests: the receiver writes files there.
#![no_main]

use std::path::{Component, Path};

use libfuzzer_sys::fuzz_target;
use portal_core::SafeRelPath;

fuzz_target!(|text: &str| {
    let Ok(path) = SafeRelPath::new(text) else {
        return;
    };
    let dest = Path::new("dest");
    let target = path.to_path(dest);
    assert!(target.starts_with(dest));
    assert!(
        target.components().all(|c| matches!(c, Component::Normal(_))),
        "{target:?} has a component that could escape"
    );
    assert_eq!(SafeRelPath::new(path.as_str()).as_ref(), Ok(&path));
    let _ = (path.collision_key(), path.file_name(), path.parent());
});
