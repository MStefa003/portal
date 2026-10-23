//! Manifests come from the sender before the receiver agrees to anything.
#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use portal_core::Manifest;

fuzz_target!(|data: &[u8]| {
    let Ok(manifest) = Manifest::from_bytes(data) else {
        return;
    };
    // Whatever is accepted is valid, and survives a round trip.
    manifest.validate().expect("from_bytes accepted an invalid manifest");
    let bytes = manifest.to_bytes().expect("an accepted manifest encodes");
    assert_eq!(Manifest::from_bytes(&bytes).expect("and decodes again"), manifest);

    // Every path stays inside the destination folder.
    let dest = Path::new("dest");
    for entry in &manifest.entries {
        assert!(entry.path.to_path(dest).starts_with(dest));
    }
    let _ = (manifest.display_name(), manifest.file_count(), manifest.total_bytes());
});
