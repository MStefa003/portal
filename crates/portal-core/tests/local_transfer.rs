//! End-to-end transfers between two folders in one process, with the manifest
//! going through its byte encoding exactly as it will over the network.
#![allow(clippy::unwrap_used)] // test helpers, not just #[test] fns

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use portal_core::path::PARTIAL_DIR_NAME;
use portal_core::{
    Manifest, OverwritePolicy, ScanOptions, Sink, SinkError, Source, SourceError, Summary, scan,
};

/// Deterministic pseudo-random bytes (xorshift64*).
fn random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        out.extend_from_slice(&state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Scans `src`, sends the manifest "over the wire" and moves every chunk.
fn transfer(src: &Path, dest: &Path, overwrite: OverwritePolicy) -> Result<Summary, SinkError> {
    let scan = scan(src, &ScanOptions::default()).unwrap();
    let wire = scan.manifest.to_bytes().unwrap();
    let received = Manifest::from_bytes(&wire).unwrap();

    let source = Source::new(&scan.roots, &scan.manifest);
    let mut sink = Sink::new(dest, received, overwrite)?;
    for hash in sink.missing_chunks() {
        let data = source.read_chunk(&hash).unwrap();
        sink.write_chunk(&hash, &data)?;
    }
    sink.finish()
}

/// Snapshot of a folder: relative path -> `None` for dirs, `Some(bytes)` for files.
fn snapshot(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                out.insert(rel, None);
                walk(root, &path, out);
            } else {
                out.insert(rel, Some(fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn make_project(root: &Path) {
    fs::create_dir_all(root.join("src/nested/deeper")).unwrap();
    fs::create_dir_all(root.join("empty_dir")).unwrap();
    fs::write(
        root.join("README.md"),
        "# Project\r\nWindows line endings stay intact.\r\n",
    )
    .unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.join("src/nested/empty.txt"), "").unwrap();
    fs::write(
        root.join("src/nested/deeper/big.bin"),
        random_bytes(7, 3_500_000),
    )
    .unwrap();
    fs::write(root.join("ünïcødé 文件.txt"), "unicode name").unwrap();
}

#[test]
fn transfers_a_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src_side/project");
    let dest = tmp.path().join("dest_side");
    make_project(&src);

    let summary = transfer(&src, &dest, OverwritePolicy::Never).unwrap();
    assert_eq!(summary.files, 5);
    assert_eq!(snapshot(&src), snapshot(&dest.join("project")));
    assert!(
        !dest.join(PARTIAL_DIR_NAME).exists(),
        "partial dir must be cleaned up"
    );
}

#[test]
fn transfers_a_single_file() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("photo.raw");
    let data = random_bytes(1, 2_000_000);
    fs::write(&src, &data).unwrap();
    let dest = tmp.path().join("out");

    transfer(&src, &dest, OverwritePolicy::Never).unwrap();
    assert_eq!(fs::read(dest.join("photo.raw")).unwrap(), data);
}

#[test]
fn transfers_an_empty_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("nothing");
    fs::create_dir(&src).unwrap();
    let dest = tmp.path().join("out");
    let summary = transfer(&src, &dest, OverwritePolicy::Never).unwrap();
    assert_eq!(summary.files, 0);
    assert!(dest.join("nothing").is_dir());
}

#[test]
fn preserves_modification_times() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("proj");
    fs::create_dir(&src).unwrap();
    let file = src.join("old.txt");
    fs::write(&file, "old").unwrap();
    let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000);
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();

    let dest = tmp.path().join("out");
    transfer(&src, &dest, OverwritePolicy::Never).unwrap();
    let got = fs::metadata(dest.join("proj/old.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(got, mtime);
}

#[test]
fn identical_content_is_sent_once() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("proj");
    fs::create_dir(&src).unwrap();
    let data = random_bytes(3, 1_500_000);
    fs::write(src.join("a.bin"), &data).unwrap();
    fs::write(src.join("copy-of-a.bin"), &data).unwrap();

    let scan = scan(&src, &ScanOptions::default()).unwrap();
    let sink = Sink::new(
        &tmp.path().join("out"),
        scan.manifest.clone(),
        OverwritePolicy::Never,
    )
    .unwrap();
    assert_eq!(sink.missing_bytes(), data.len() as u64);

    transfer(&src, &tmp.path().join("out2"), OverwritePolicy::Never).unwrap();
    assert_eq!(
        fs::read(tmp.path().join("out2/proj/copy-of-a.bin")).unwrap(),
        data
    );
}

#[test]
fn refuses_to_overwrite_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("proj");
    make_project(&src);
    let dest = tmp.path().join("out");
    fs::create_dir_all(dest.join("proj/src")).unwrap();
    fs::write(dest.join("proj/src/main.rs"), "precious").unwrap();

    let err = transfer(&src, &dest, OverwritePolicy::Never).unwrap_err();
    assert!(
        matches!(&err, SinkError::Conflicts(c) if c.len() == 1),
        "{err}"
    );
    assert_eq!(
        fs::read_to_string(dest.join("proj/src/main.rs")).unwrap(),
        "precious"
    );
    assert!(
        !dest.join("proj/README.md").exists(),
        "nothing written on refusal"
    );

    transfer(&src, &dest, OverwritePolicy::Always).unwrap();
    assert_eq!(snapshot(&src), snapshot(&dest.join("proj")));
}

#[test]
fn refuses_file_where_folder_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("proj");
    make_project(&src);
    let dest = tmp.path().join("out");
    fs::create_dir_all(dest.join("proj/README.md")).unwrap();
    let err = transfer(&src, &dest, OverwritePolicy::Always).unwrap_err();
    assert!(matches!(err, SinkError::TypeConflict(_)), "{err}");
}

#[test]
fn rejects_corrupted_chunks_and_never_finalizes() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("proj");
    make_project(&src);
    let dest = tmp.path().join("out");

    let scan = scan(&src, &ScanOptions::default()).unwrap();
    let source = Source::new(&scan.roots, &scan.manifest);
    let mut sink = Sink::new(&dest, scan.manifest.clone(), OverwritePolicy::Never).unwrap();
    let (_, _, big) = scan
        .manifest
        .files()
        .find(|(_, e, _)| e.path.file_name() == "big.bin")
        .unwrap();
    let victim = big.chunks[big.chunks.len() / 2].hash;

    for hash in sink.missing_chunks() {
        if hash != victim {
            sink.write_chunk(&hash, &source.read_chunk(&hash).unwrap())
                .unwrap();
        }
    }
    let mut data = source.read_chunk(&victim).unwrap();
    data[0] ^= 0x01; // flip one bit
    assert!(matches!(
        sink.write_chunk(&victim, &data),
        Err(SinkError::CorruptChunk(_))
    ));
    assert!(!sink.is_complete());
    assert!(matches!(
        sink.finish(),
        Err(SinkError::Incomplete { missing_chunks: 1 })
    ));
    assert!(
        !dest.join("proj/src/nested/deeper/big.bin").exists(),
        "a file with a corrupted chunk must never appear at its final path"
    );
}

#[test]
fn rejects_unrequested_and_oversized_chunks() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("f.txt");
    fs::write(&src, "content").unwrap();
    let scan = scan(&src, &ScanOptions::default()).unwrap();
    let mut sink = Sink::new(
        &tmp.path().join("out"),
        scan.manifest,
        OverwritePolicy::Never,
    )
    .unwrap();

    let stray = b"not part of this transfer";
    assert!(matches!(
        sink.write_chunk(&portal_core::Hash::of(stray), stray),
        Err(SinkError::UnexpectedChunk(_))
    ));
    let huge = vec![0u8; portal_core::chunk::MAX_CHUNK_SIZE as usize + 1];
    assert!(matches!(
        sink.write_chunk(&portal_core::Hash::of(&huge), &huge),
        Err(SinkError::ChunkTooLarge(_))
    ));
}

#[test]
fn detects_source_changed_after_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("f.txt");
    fs::write(&src, "original content").unwrap();
    let scan = scan(&src, &ScanOptions::default()).unwrap();
    let source = Source::new(&scan.roots, &scan.manifest);
    fs::write(&src, "modified content").unwrap();

    let hash = scan.manifest.files().next().unwrap().2.chunks[0].hash;
    assert!(matches!(
        source.read_chunk(&hash),
        Err(SourceError::Changed(_))
    ));
}

#[cfg(unix)]
#[test]
fn refuses_to_write_through_existing_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("proj");
    make_project(&src);
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let dest = tmp.path().join("out");
    fs::create_dir(&dest).unwrap();
    std::os::unix::fs::symlink(&outside, dest.join("proj")).unwrap();

    let err = transfer(&src, &dest, OverwritePolicy::Always).unwrap_err();
    assert!(matches!(err, SinkError::Symlink(_)), "{err}");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn preserves_executable_bit() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("run.sh");
    fs::write(&src, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
    let dest = tmp.path().join("out");
    transfer(&src, &dest, OverwritePolicy::Never).unwrap();
    let mode = fs::metadata(dest.join("run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0);
}

#[test]
fn identical_files_are_not_conflicts() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("proj");
    make_project(&src);
    let dest = tmp.path().join("out");
    transfer(&src, &dest, OverwritePolicy::Never).unwrap();

    // Receiving the same folder again needs no data and no --overwrite.
    let scan = scan(&src, &ScanOptions::default()).unwrap();
    let sink = Sink::new(&dest, scan.manifest, OverwritePolicy::Never).unwrap();
    assert!(sink.is_complete());
    sink.finish().unwrap();
    assert_eq!(snapshot(&src), snapshot(&dest.join("proj")));
}

#[test]
fn resumes_an_interrupted_transfer() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("proj");
    make_project(&src);
    let dest = tmp.path().join("out");
    let scan = scan(&src, &ScanOptions::default()).unwrap();
    let source = Source::new(&scan.roots, &scan.manifest);

    // First attempt: half the chunks arrive, then the connection drops.
    let mut first = Sink::new(&dest, scan.manifest.clone(), OverwritePolicy::Never).unwrap();
    let all = first.missing_chunks();
    let half = all.len() / 2;
    assert!(half > 1, "the test project needs several chunks");
    for hash in &all[..half] {
        first
            .write_chunk(hash, &source.read_chunk(hash).unwrap())
            .unwrap();
    }
    drop(first);

    // Damage one byte of what was kept: that chunk must be fetched again.
    let id = scan.manifest.transfer_id().unwrap();
    let partial_dir = dest.join(PARTIAL_DIR_NAME).join(id.to_hex_prefix(16));
    let (big, _, big_meta) = scan
        .manifest
        .files()
        .find(|(_, e, _)| e.path.file_name() == "big.bin")
        .unwrap();
    assert!(all[..half].contains(&big_meta.chunks[0].hash));
    let part = partial_dir.join(format!("{big}.part"));
    let mut bytes = fs::read(&part).unwrap();
    bytes[0] ^= 0x01;
    fs::write(&part, bytes).unwrap();

    let mut second = Sink::new(&dest, scan.manifest.clone(), OverwritePolicy::Never).unwrap();
    let missing = second.missing_chunks();
    assert!(
        missing.len() < all.len(),
        "kept chunks are not requested again"
    );
    assert!(
        missing.len() > all.len() - half,
        "the damaged chunk is requested again"
    );
    for hash in &missing {
        second
            .write_chunk(hash, &source.read_chunk(hash).unwrap())
            .unwrap();
    }
    second.finish().unwrap();
    assert_eq!(snapshot(&src), snapshot(&dest.join("proj")));
    assert!(!dest.join(PARTIAL_DIR_NAME).exists());
}

/// Several files and folders from different places arrive side by side.
#[test]
fn several_items_arrive_side_by_side() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("one/photos");
    fs::create_dir_all(a.join("2026")).unwrap();
    fs::write(a.join("2026/x.jpg"), random_bytes(1, 300_000)).unwrap();
    let b = tmp.path().join("two/notes.txt");
    fs::create_dir_all(b.parent().unwrap()).unwrap();
    fs::write(&b, "notes").unwrap();
    let dest = tmp.path().join("out");

    let scan = portal_core::scan_many(&[a.clone(), b.clone()], &ScanOptions::default()).unwrap();
    let received = Manifest::from_bytes(&scan.manifest.to_bytes().unwrap()).unwrap();
    let source = Source::new(&scan.roots, &scan.manifest);
    let mut sink = Sink::new(&dest, received, OverwritePolicy::Never).unwrap();
    for hash in sink.missing_chunks() {
        sink.write_chunk(&hash, &source.read_chunk(&hash).unwrap())
            .unwrap();
    }
    let summary = sink.finish().unwrap();
    assert_eq!(summary.files, 2);
    assert_eq!(
        fs::read(dest.join("photos/2026/x.jpg")).unwrap(),
        fs::read(a.join("2026/x.jpg")).unwrap()
    );
    assert_eq!(fs::read_to_string(dest.join("notes.txt")).unwrap(), "notes");
}
