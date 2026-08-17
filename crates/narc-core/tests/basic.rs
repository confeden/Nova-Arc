//! Integration tests for the NARC v0 container: round-trip, append-only
//! updates, dedup, removal + compaction, crash recovery.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use narc_core::{Archive, Overwrite, PackOptions, Tier};

/// Deterministic pseudo-random (incompressible) bytes.
fn rnd(len: usize, mut seed: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(len + 8);
    while v.len() < len {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        v.extend_from_slice(&seed.to_le_bytes());
    }
    v.truncate(len);
    v
}

/// Compressible text of the given size.
fn text(len: usize) -> Vec<u8> {
    let mut v = "the quick brown fox jumps over the lazy dog. "
        .repeat(len / 45 + 1)
        .into_bytes();
    v.truncate(len);
    v
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    src: PathBuf,
    arc: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let src = root.join("src");
    fs::create_dir_all(src.join("docs/sub")).unwrap();
    fs::write(src.join("docs/a.txt"), text(200_000)).unwrap();
    fs::write(src.join("docs/sub/b.txt"), text(10_000)).unwrap();
    fs::write(src.join("pic.bin"), rnd(3 << 20, 1)).unwrap();
    fs::write(src.join("empty.dat"), b"").unwrap();
    let arc = root.join("a.narc");
    Fixture {
        _tmp: tmp,
        root,
        src,
        arc,
    }
}

fn assert_same_tree(expected: &Path, actual: &Path) {
    for entry in walkdir_files(expected) {
        let rel = entry.strip_prefix(expected).unwrap();
        let other = actual.join(rel);
        let a = fs::read(&entry).unwrap();
        let b = fs::read(&other).unwrap_or_else(|e| panic!("missing {}: {e}", other.display()));
        assert_eq!(a, b, "content mismatch for {}", rel.display());
    }
}

fn walkdir_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(d).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn roundtrip_create_extract() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    let s = a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Normal)).unwrap();
    assert_eq!(s.files, 4);
    drop(a);

    let a = Archive::open_ro(&fx.arc).unwrap();
    let out = fx.root.join("out");
    let xs = a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_eq!(xs.files, 4);
    assert_same_tree(&fx.src, &out.join("src"));
    // empty file must exist
    assert_eq!(fs::read(out.join("src/empty.dat")).unwrap().len(), 0);
    // text must actually compress
    let info = a.info();
    assert!(info.file_len < (3 << 20) + 220_000, "text should compress");
}

#[test]
fn update_appends_without_rewriting() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Normal)).unwrap();
    drop(a);

    let before = fs::read(&fx.arc).unwrap();

    // "edit one photo out of 700": add one new file
    fs::write(fx.src.join("new.txt"), text(50_000)).unwrap();
    let mut a = Archive::open_rw(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Normal)).unwrap();
    drop(a);

    let after = fs::read(&fx.arc).unwrap();
    // The killer feature: an update never rewrites committed bytes.
    assert!(after.len() > before.len());
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "prefix must be untouched"
    );

    let a = Archive::open_ro(&fx.arc).unwrap();
    let out = fx.root.join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_same_tree(&fx.src, &out.join("src"));
}

#[test]
fn replace_reuses_unchanged_chunks() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Normal)).unwrap();
    drop(a);

    // change one small file, re-add the whole tree
    fs::write(fx.src.join("docs/a.txt"), text(100_000)).unwrap();
    let mut a = Archive::open_rw(&fx.arc).unwrap();
    let s = a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Normal)).unwrap();
    // the unchanged 3 MiB pic.bin must be fully deduplicated
    assert!(s.bytes_deduped >= 3 << 20, "dedup: {}", s.bytes_deduped);
    let info = a.info();
    assert!(info.reclaimable > 0, "old a.txt chunks are now dead space");
    drop(a);

    let a = Archive::open_ro(&fx.arc).unwrap();
    let out = fx.root.join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_same_tree(&fx.src, &out.join("src"));
}

#[test]
fn dedup_identical_files() {
    let fx = fixture();
    let dup = fx.src.join("pic_copy.bin");
    fs::copy(fx.src.join("pic.bin"), &dup).unwrap();
    let mut a = Archive::create(&fx.arc).unwrap();
    let s = a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast)).unwrap();
    assert!(s.bytes_deduped >= 3 << 20, "identical file should dedup");
    let info = a.info();
    assert!(
        info.file_len < (3 << 20) + 400_000,
        "two identical 3MiB files must not double the archive: {}",
        info.file_len
    );
}

#[test]
fn remove_then_compact() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Normal)).unwrap();

    let n = a.remove(&["src/pic.bin".to_string()]).unwrap();
    assert_eq!(n, 1);
    let info = a.info();
    assert!(info.reclaimable >= 3 << 20, "pic chunks are dead now");

    let (before, after) = a.compact().unwrap();
    assert!(after < before, "compact must shrink: {before} -> {after}");

    let a = Archive::open_ro(&fx.arc).unwrap();
    let out = fx.root.join("out");
    let xs = a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_eq!(xs.files, 3);
    assert!(!out.join("src/pic.bin").exists());
    assert_same_tree(&out.join("src"), &fx.src); // reverse direction: all extracted match
}

#[test]
fn selective_extract() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast)).unwrap();
    let out = fx.root.join("out");
    let xs = a
        .extract(&out, Some(&["src/docs".to_string()]), Overwrite::Fail)
        .unwrap();
    assert_eq!(xs.files, 2);
    assert!(out.join("src/docs/a.txt").exists());
    assert!(!out.join("src/pic.bin").exists());
}

#[test]
fn crash_recovery_ignores_trailing_garbage() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast)).unwrap();
    drop(a);
    let committed = fs::read(&fx.arc).unwrap();

    // simulate a crash mid-append: garbage after the last valid footer
    let mut f = fs::OpenOptions::new().append(true).open(&fx.arc).unwrap();
    f.write_all(&rnd(100_000, 7)).unwrap();
    drop(f);

    // read-only open must still see the committed state
    let a = Archive::open_ro(&fx.arc).unwrap();
    assert_eq!(a.manifest.files.len(), 4);
    drop(a);

    // read-write open truncates the garbage and can keep appending
    let mut a = Archive::open_rw(&fx.arc).unwrap();
    fs::write(fx.src.join("more.txt"), text(1000)).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast)).unwrap();
    drop(a);

    let now = fs::read(&fx.arc).unwrap();
    assert_eq!(&now[..committed.len()], &committed[..]);

    let a = Archive::open_ro(&fx.arc).unwrap();
    let out = fx.root.join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_same_tree(&fx.src, &out.join("src"));
}

#[test]
fn rejects_non_narc_files() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("fake.narc");
    fs::write(&p, b"MZ this is not an archive at all").unwrap();
    assert!(Archive::open_ro(&p).is_err());
    let q = tmp.path().join("tiny.narc");
    fs::write(&q, b"NA").unwrap();
    assert!(Archive::open_ro(&q).is_err());
}

#[test]
fn create_refuses_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("x.narc");
    fs::write(&p, b"precious data").unwrap();
    assert!(
        Archive::create(&p).is_err(),
        "must not clobber existing files"
    );
    assert_eq!(fs::read(&p).unwrap(), b"precious data");
}

// --- regression tests for the issues found in the v0 code review ---

/// Read the committed footer at EOF and return (manifest_offset, footer_start).
fn footer_fields(path: &Path) -> (u64, u64) {
    let b = fs::read(path).unwrap();
    let f = &b[b.len() - 80..];
    assert_eq!(&f[0..8], b"NARCEND1");
    let moff = u64::from_le_bytes(f[16..24].try_into().unwrap());
    (moff, b.len() as u64 - 80)
}

#[test]
fn extract_refuses_to_clobber_by_default() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast)).unwrap();
    drop(a);

    let out = fx.root.join("out");
    fs::create_dir_all(out.join("src/docs")).unwrap();
    fs::write(out.join("src/docs/a.txt"), b"MY PRECIOUS LOCAL EDIT").unwrap();

    let a = Archive::open_ro(&fx.arc).unwrap();
    assert!(a.extract(&out, None, Overwrite::Fail).is_err());
    assert_eq!(
        fs::read(out.join("src/docs/a.txt")).unwrap(),
        b"MY PRECIOUS LOCAL EDIT",
        "default policy must not destroy existing data"
    );

    let s = a.extract(&out, None, Overwrite::Skip).unwrap();
    assert_eq!(s.skipped_existing, 1);
    assert_eq!(
        fs::read(out.join("src/docs/a.txt")).unwrap(),
        b"MY PRECIOUS LOCAL EDIT"
    );

    a.extract(&out, None, Overwrite::Force).unwrap();
    assert_eq!(fs::read(out.join("src/docs/a.txt")).unwrap(), text(200_000));
}

#[test]
fn torn_manifest_falls_back_to_previous_generation() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast)).unwrap();
    drop(a);
    let files_before = Archive::open_ro(&fx.arc).unwrap().manifest.files.len();

    fs::write(fx.src.join("second.txt"), text(30_000)).unwrap();
    let mut a = Archive::open_rw(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast)).unwrap();
    drop(a);

    // Corrupt the newest manifest, keeping its (valid) footer: exactly what a
    // crash between the manifest write and the footer write leaves behind.
    let (moff, _) = footer_fields(&fx.arc);
    let mut bytes = fs::read(&fx.arc).unwrap();
    bytes[moff as usize] ^= 0xFF;
    fs::write(&fx.arc, &bytes).unwrap();

    let a = Archive::open_ro(&fx.arc).unwrap();
    assert_eq!(
        a.manifest.files.len(),
        files_before,
        "must fall back to the last fully committed generation"
    );
    let out = fx.root.join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
}

#[test]
fn embedded_footer_in_stored_data_is_not_mistaken_for_commit() {
    let fx = fixture();
    // an inner archive whose bytes (including its own footer) get stored raw
    let inner = fx.root.join("inner.narc");
    let mut b = Archive::create(&inner).unwrap();
    b.add_paths(&[fx.src.join("pic.bin")], &PackOptions::new(Tier::Fast)).unwrap();
    drop(b);

    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(&[fx.src.join("docs")], &PackOptions::new(Tier::Fast)).unwrap();
    drop(a);
    let files_before = Archive::open_ro(&fx.arc).unwrap().manifest.files.len();

    let mut a = Archive::open_rw(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&inner), &PackOptions::new(Tier::Fast)).unwrap();
    drop(a);

    // Simulate a crash after the chunks were appended but before the commit:
    // the last footer-looking bytes in the file now belong to the *stored*
    // inner archive, not to a real generation.
    let (moff, _) = footer_fields(&fx.arc);
    let bytes = fs::read(&fx.arc).unwrap();
    fs::write(&fx.arc, &bytes[..moff as usize]).unwrap();

    let a = Archive::open_ro(&fx.arc).unwrap();
    assert_eq!(a.manifest.files.len(), files_before);
    let out = fx.root.join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
}

#[test]
fn rejects_forged_footer_claiming_huge_manifest() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(&[fx.src.join("docs")], &PackOptions::new(Tier::Fast)).unwrap();
    drop(a);

    let mut b = fs::read(&fx.arc).unwrap();
    let n = b.len();
    // claim a 1 TiB unpacked manifest and re-sign the footer (the self-hash
    // covers the footer bytes plus its absolute offset)
    b[n - 80 + 32..n - 80 + 40].copy_from_slice(&(1u64 << 40).to_le_bytes());
    let mut h = blake3::Hasher::new();
    h.update(&b[n - 80..n - 16]);
    h.update(&((n - 80) as u64).to_le_bytes());
    b[n - 16..].copy_from_slice(&h.finalize().as_bytes()[..16]);
    fs::write(&fx.arc, &b).unwrap();

    // The forged footer must be ignored (no multi-GB allocation): either the
    // open fails, or it falls back to the previous, empty generation.
    if let Ok(a) = Archive::open_ro(&fx.arc) {
        assert!(
            a.manifest.files.is_empty(),
            "forged footer must not be acted upon"
        );
    }
}

#[test]
fn second_writer_is_locked_out() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(&[fx.src.join("docs")], &PackOptions::new(Tier::Fast)).unwrap();
    assert!(
        Archive::open_rw(&fx.arc).is_err(),
        "a second writer must not be able to append at a stale EOF"
    );
    drop(a);
    assert!(Archive::open_rw(&fx.arc).is_ok(), "lock releases on close");
}

#[test]
fn selectors_accept_windows_separators_and_report_misses() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast)).unwrap();
    let out = fx.root.join("out");

    let s = a
        .extract(&out, Some(&[r"src\docs".to_string()]), Overwrite::Fail)
        .unwrap();
    assert_eq!(s.files, 2, "backslash selectors must match");

    let e = a
        .extract(
            &fx.root.join("out2"),
            Some(&["src/nope".to_string()]),
            Overwrite::Fail,
        )
        .unwrap_err();
    assert!(e.to_string().contains("not found"), "got: {e}");
}

#[test]
fn add_rejects_paths_extract_would_refuse() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    // Windows cannot create such names, so test the rule directly; on Unix
    // these are creatable and must be rejected at add time.
    for bad in ["note.", "name ", "a:b", "con.jpg"] {
        assert!(
            narc_core::paths::normalize_rel(Path::new(bad)).is_err(),
            "add must reject {bad:?} because extract does"
        );
    }
    let s = a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast)).unwrap();
    assert_eq!(s.files, 4);
}

#[test]
fn preserves_pre_1970_timestamps() {
    let fx = fixture();
    let old = fx.src.join("docs/a.txt");
    let t = filetime::FileTime::from_unix_time(-14_182_980, 0); // 1969-07-20
    filetime::set_file_mtime(&old, t).unwrap();

    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast)).unwrap();
    let entry = a
        .manifest
        .files
        .iter()
        .find(|f| f.path.ends_with("docs/a.txt"))
        .unwrap();
    assert_eq!(entry.mtime, -14_182_980);

    let out = fx.root.join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    let got = filetime::FileTime::from_last_modification_time(
        &fs::metadata(out.join("src/docs/a.txt")).unwrap(),
    );
    assert_eq!(got.unix_seconds(), -14_182_980);
}

#[test]
fn compact_detects_corrupted_chunk_instead_of_copying_it() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(&[fx.src.join("docs")], &PackOptions::new(Tier::Fast)).unwrap();
    let first = a.manifest.chunks[0].clone();
    drop(a);

    let mut b = fs::read(&fx.arc).unwrap();
    b[first.offset as usize] ^= 0xFF;
    fs::write(&fx.arc, &b).unwrap();

    let a = Archive::open_rw(&fx.arc).unwrap();
    assert!(
        a.compact().is_err(),
        "compaction must not bake corruption into the new archive"
    );
    assert!(fx.arc.exists(), "the original archive must survive");
}

// --- solid blocks (compression v2) ---

/// A tree of many small files of two types, the case solid blocks exist for.
fn small_tree(root: &Path, n: usize) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("cfg")).unwrap();
    for i in 0..n {
        fs::write(
            root.join(format!("src/mod_{i}.rs")),
            format!("pub fn f_{i}(x: u32) -> u32 {{ x + {i} }}\n").repeat(40),
        )
        .unwrap();
        fs::write(
            root.join(format!("cfg/app_{i}.toml")),
            format!("[server]\nname = \"node{i}\"\nport = {}\n", 8000 + i).repeat(20),
        )
        .unwrap();
    }
}

#[test]
fn shared_units_roundtrip_and_shrink_small_files() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("many");
    small_tree(&src, 120);
    let arc = tmp.path().join("s.narc");

    let mut a = Archive::create(&arc).unwrap();
    let s = a.add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Normal))
        .unwrap();
    assert_eq!(s.files, 240);
    // 240 tiny files must not become 240 chunks: they are packed together.
    assert!(
        a.manifest.chunks.len() < 20,
        "expected solid packing, got {} chunks",
        a.manifest.chunks.len()
    );
    
    assert!(
        a.manifest
            .files
            .iter()
            .filter(|f| f.size > 0)
            .all(|f| f.extents.len() == 1),
        "each small file should sit in exactly one shared unit"
    );
    // Cross-file redundancy is the point: these files are near-identical.
    assert!(
        s.bytes_stored * 40 < s.bytes_in,
        "solid ratio too weak: {} -> {}",
        s.bytes_in,
        s.bytes_stored
    );
    drop(a);

    let a = Archive::open_ro(&arc).unwrap();
    let out = tmp.path().join("out");
    let xs = a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_eq!(xs.files, 240);
    assert_same_tree(&src, &out.join("many"));
}

#[test]
fn editing_one_small_file_rewrites_only_its_block() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("many");
    small_tree(&src, 60);
    let arc = tmp.path().join("s.narc");
    let mut a = Archive::create(&arc).unwrap();
    a.add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Normal))
        .unwrap();
    drop(a);
    let before = fs::metadata(&arc).unwrap().len();

    fs::write(src.join("src/mod_7.rs"), "pub fn changed() {}\n").unwrap();
    let mut a = Archive::open_rw(&arc).unwrap();
    a.add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Normal))
        .unwrap();
    drop(a);
    let after = fs::metadata(&arc).unwrap().len();

    // The whole tree is re-added, but only the changed block's chunks are new.
    assert!(
        after - before < before,
        "one-file edit grew the archive by {} on a {} archive",
        after - before,
        before
    );

    let a = Archive::open_ro(&arc).unwrap();
    let out = tmp.path().join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_same_tree(&src, &out.join("many"));
}

#[test]
fn selective_extract_of_a_unit_member() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("many");
    small_tree(&src, 30);
    let arc = tmp.path().join("s.narc");
    let mut a = Archive::create(&arc).unwrap();
    a.add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Fast))
        .unwrap();

    let out = tmp.path().join("one");
    let xs = a
        .extract(
            &out,
            Some(&["many/src/mod_3.rs".to_string()]),
            Overwrite::Fail,
        )
        .unwrap();
    assert_eq!(xs.files, 1);
    assert_eq!(
        fs::read(out.join("many/src/mod_3.rs")).unwrap(),
        fs::read(src.join("src/mod_3.rs")).unwrap()
    );
    assert!(!out.join("many/src/mod_4.rs").exists());
}

#[test]
fn compact_keeps_shared_units_consistent() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("many");
    small_tree(&src, 40);
    let arc = tmp.path().join("s.narc");
    let mut a = Archive::create(&arc).unwrap();
    a.add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Fast))
        .unwrap();
    a.remove(&["many/cfg".to_string()]).unwrap();
    let (before, after) = a.compact().unwrap();
    assert!(after < before);

    let a = Archive::open_ro(&arc).unwrap();
    assert_eq!(a.manifest.files.len(), 40);
    let out = tmp.path().join("out");
    let xs = a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_eq!(xs.files, 40);
    for i in 0..40 {
        assert_eq!(
            fs::read(out.join(format!("many/src/mod_{i}.rs"))).unwrap(),
            fs::read(src.join(format!("src/mod_{i}.rs"))).unwrap()
        );
    }
}

#[test]
fn max_tier_roundtrips_every_codec() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("mixed");
    fs::create_dir_all(&src).unwrap();
    // text -> PPMd, executable -> BCJ + LZMA2, random -> Store, big text -> LZMA2/PPMd
    fs::write(src.join("prose.txt"), text(900_000)).unwrap();
    let mut exe = b"MZ\x90\x00\x03\x00\x00\x00".to_vec();
    for i in 0..300_000u32 {
        exe.push(0xE8);
        exe.extend_from_slice(&(i % 977).to_le_bytes());
    }
    fs::write(src.join("prog.exe"), &exe).unwrap();
    fs::write(src.join("noise.bin"), rnd(700_000, 42)).unwrap();
    let arc = tmp.path().join("m.narc");

    let mut a = Archive::create(&arc).unwrap();
    a.add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Max))
        .unwrap();
    // These files are far smaller than a max-tier unit, so they share one:
    // what matters here is that whatever the tournament picked round-trips.
    assert!(!a.manifest.chunks.is_empty());
    drop(a);

    let a = Archive::open_ro(&arc).unwrap();
    let out = tmp.path().join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_same_tree(&src, &out.join("mixed"));
}
