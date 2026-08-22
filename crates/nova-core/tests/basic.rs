//! Integration tests for the NOVA v0 container: round-trip, append-only
//! updates, dedup, removal + compaction, crash recovery.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use nova_core::{Archive, Overwrite, PackOptions, Tier};

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
    let arc = root.join("a.nva");
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
    let s = a
        .add_paths(
            std::slice::from_ref(&fx.src),
            &PackOptions::new(Tier::Normal),
        )
        .unwrap();
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
    a.add_paths(
        std::slice::from_ref(&fx.src),
        &PackOptions::new(Tier::Normal),
    )
    .unwrap();
    drop(a);

    let before = fs::read(&fx.arc).unwrap();

    // "edit one photo out of 700": add one new file
    fs::write(fx.src.join("new.txt"), text(50_000)).unwrap();
    let mut a = Archive::open_rw(&fx.arc).unwrap();
    a.add_paths(
        std::slice::from_ref(&fx.src),
        &PackOptions::new(Tier::Normal),
    )
    .unwrap();
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
    a.add_paths(
        std::slice::from_ref(&fx.src),
        &PackOptions::new(Tier::Normal),
    )
    .unwrap();
    drop(a);

    // change one small file, re-add the whole tree
    fs::write(fx.src.join("docs/a.txt"), text(100_000)).unwrap();
    let mut a = Archive::open_rw(&fx.arc).unwrap();
    let s = a
        .add_paths(
            std::slice::from_ref(&fx.src),
            &PackOptions::new(Tier::Normal),
        )
        .unwrap();
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
    let s = a
        .add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast))
        .unwrap();
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
    a.add_paths(
        std::slice::from_ref(&fx.src),
        &PackOptions::new(Tier::Normal),
    )
    .unwrap();

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
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast))
        .unwrap();
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
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast))
        .unwrap();
    drop(a);
    let committed = fs::read(&fx.arc).unwrap();

    // simulate a crash mid-append: garbage after the last valid footer
    let mut f = fs::OpenOptions::new().append(true).open(&fx.arc).unwrap();
    f.write_all(&rnd(100_000, 7)).unwrap();
    drop(f);

    // read-only open must still see the committed state
    let a = Archive::open_ro(&fx.arc).unwrap();
    // Folders are entries too now, so count what this test is about.
    assert_eq!(a.manifest.files.iter().filter(|f| !f.dir).count(), 4);
    drop(a);

    // read-write open truncates the garbage and can keep appending
    let mut a = Archive::open_rw(&fx.arc).unwrap();
    fs::write(fx.src.join("more.txt"), text(1000)).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast))
        .unwrap();
    drop(a);

    let now = fs::read(&fx.arc).unwrap();
    assert_eq!(&now[..committed.len()], &committed[..]);

    let a = Archive::open_ro(&fx.arc).unwrap();
    let out = fx.root.join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_same_tree(&fx.src, &out.join("src"));
}

#[test]
fn rejects_non_nova_files() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("fake.nva");
    fs::write(&p, b"MZ this is not an archive at all").unwrap();
    assert!(Archive::open_ro(&p).is_err());
    let q = tmp.path().join("tiny.nva");
    fs::write(&q, b"NA").unwrap();
    assert!(Archive::open_ro(&q).is_err());
}

#[test]
fn create_refuses_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("x.nva");
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
    assert_eq!(&f[0..8], b"NOVAEND1");
    let moff = u64::from_le_bytes(f[16..24].try_into().unwrap());
    (moff, b.len() as u64 - 80)
}

#[test]
fn extract_refuses_to_clobber_by_default() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast))
        .unwrap();
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
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast))
        .unwrap();
    drop(a);
    let files_before = Archive::open_ro(&fx.arc).unwrap().manifest.files.len();

    fs::write(fx.src.join("second.txt"), text(30_000)).unwrap();
    let mut a = Archive::open_rw(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast))
        .unwrap();
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
    let inner = fx.root.join("inner.nva");
    let mut b = Archive::create(&inner).unwrap();
    b.add_paths(&[fx.src.join("pic.bin")], &PackOptions::new(Tier::Fast))
        .unwrap();
    drop(b);

    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(&[fx.src.join("docs")], &PackOptions::new(Tier::Fast))
        .unwrap();
    drop(a);
    let files_before = Archive::open_ro(&fx.arc).unwrap().manifest.files.len();

    let mut a = Archive::open_rw(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&inner), &PackOptions::new(Tier::Fast))
        .unwrap();
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
    a.add_paths(&[fx.src.join("docs")], &PackOptions::new(Tier::Fast))
        .unwrap();
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
    a.add_paths(&[fx.src.join("docs")], &PackOptions::new(Tier::Fast))
        .unwrap();
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
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast))
        .unwrap();
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
            nova_core::paths::normalize_rel(Path::new(bad)).is_err(),
            "add must reject {bad:?} because extract does"
        );
    }
    let s = a
        .add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast))
        .unwrap();
    assert_eq!(s.files, 4);
}

#[test]
fn preserves_pre_1970_timestamps() {
    let fx = fixture();
    let old = fx.src.join("docs/a.txt");
    let t = filetime::FileTime::from_unix_time(-14_182_980, 0); // 1969-07-20
    filetime::set_file_mtime(&old, t).unwrap();

    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast))
        .unwrap();
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
    a.add_paths(&[fx.src.join("docs")], &PackOptions::new(Tier::Fast))
        .unwrap();
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
    let arc = tmp.path().join("s.nva");

    let mut a = Archive::create(&arc).unwrap();
    let s = a
        .add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Normal))
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
    let arc = tmp.path().join("s.nva");
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
    let arc = tmp.path().join("s.nva");
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
    let arc = tmp.path().join("s.nva");
    let mut a = Archive::create(&arc).unwrap();
    a.add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Fast))
        .unwrap();
    a.remove(&["many/cfg".to_string()]).unwrap();
    let (before, after) = a.compact().unwrap();
    assert!(after < before);

    let a = Archive::open_ro(&arc).unwrap();
    assert_eq!(a.manifest.files.iter().filter(|f| !f.dir).count(), 40);
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
    let arc = tmp.path().join("m.nva");

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

/// The progress contract, as a test rather than as a comment.
///
/// It exists because the old reading counted bytes the reader had taken off
/// disk: it hit 100% after 1.85 s of a 38.59 s max-tier pack and then went
/// silent, which in the GUI looked like a bar stuck at full with an unexplained
/// wait. Anything that reports work before it is done breaks one of these
/// assertions.
#[test]
fn progress_never_lies_and_ends_at_full() {
    use nova_core::{Phase, Progress};
    use std::sync::Mutex;

    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("tree");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a.txt"), text(400_000)).unwrap();
    fs::write(src.join("b.txt"), text(400_000)).unwrap();
    // Byte-identical to b.txt: its unit deduplicates, so no compression work
    // happens for it and the reading must still account for its bytes.
    fs::write(src.join("sub/b-copy.txt"), text(400_000)).unwrap();
    fs::write(src.join("empty.txt"), b"").unwrap();
    fs::write(src.join("noise.bin"), rnd(300_000, 7)).unwrap();

    let seen: Mutex<Vec<Progress>> = Mutex::new(Vec::new());
    let arc = tmp.path().join("p.nva");
    let mut a = Archive::create(&arc).unwrap();
    let stats = a
        .add_paths_with(
            std::slice::from_ref(&src),
            &PackOptions::new(Tier::Normal),
            Some(&|p: Progress| seen.lock().unwrap().push(p)),
        )
        .unwrap();
    let log = seen.into_inner().unwrap();

    assert!(!log.is_empty(), "no progress at all");
    let mut prev = Progress::default();
    for (i, p) in log.iter().enumerate() {
        assert!(
            p.bytes_done >= prev.bytes_done,
            "reading {i} went backwards: {} -> {}",
            prev.bytes_done,
            p.bytes_done
        );
        assert!(
            p.bytes_done <= p.bytes_total,
            "reading {i} exceeds the total"
        );
        assert!(p.bytes_read >= p.bytes_done, "read must lead done");
        prev = *p;
    }
    // Exactly one reading may claim completion, and it must be the last.
    let full: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, p)| p.bytes_total > 0 && p.bytes_done == p.bytes_total)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        full,
        vec![log.len() - 1],
        "100% must happen once, at the end"
    );
    let last = log.last().unwrap();
    assert_eq!(last.phase, Phase::Done);
    assert_eq!(last.files_done, last.files_total);
    assert_eq!(last.bytes_total, stats.bytes_in);
    assert!(
        log.iter().any(|p| p.phase == Phase::Commit),
        "the manifest write is invisible: {:?}",
        log.iter().map(|p| p.phase).collect::<Vec<_>>()
    );
    // Throttled: one event per file would be 5751 on a real tree.
    assert!(log.len() < 2100, "too many readings: {}", log.len());
}

/// Files kept by the overwrite policy are finished work and sit in the
/// denominator. Not counting them made a re-extraction into a populated
/// directory report 0% for its entire run — and the GUI always extracts with
/// this policy.
#[test]
fn skipped_files_still_reach_a_hundred_percent() {
    use nova_core::{Phase, Progress};
    use std::sync::Mutex;

    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("tree");
    fs::create_dir_all(&src).unwrap();
    for i in 0..8 {
        fs::write(src.join(format!("f{i}.txt")), text(50_000 + i * 1000)).unwrap();
    }
    let arc = tmp.path().join("s.nva");
    let mut a = Archive::create(&arc).unwrap();
    a.add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Fast))
        .unwrap();
    drop(a);

    let a = Archive::open_ro(&arc).unwrap();
    let out = tmp.path().join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();

    // Second pass: every file is already there, so every one is skipped.
    let seen: Mutex<Vec<Progress>> = Mutex::new(Vec::new());
    let stats = a
        .extract_reporting(
            &out,
            None,
            Overwrite::Skip,
            &PackOptions::new(Tier::Fast),
            Some(&|p: Progress| seen.lock().unwrap().push(p)),
        )
        .unwrap();
    assert_eq!(stats.skipped_existing, 8);
    let log = seen.into_inner().unwrap();
    let last = log.last().expect("no progress at all");
    assert_eq!(last.phase, Phase::Done);
    assert_eq!(last.bytes_done, last.bytes_total);
    assert!(last.bytes_total > 0);
}

/// Renaming and moving must be index-only. For an archive of family media this
/// is the whole point: a path lives in the manifest and the bytes live in
/// units, so reorganising folders must never re-read or re-compress anything.
#[test]
fn rename_moves_entries_without_touching_data() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("photos");
    fs::create_dir_all(src.join("2019")).unwrap();
    fs::write(src.join("2019/a.txt"), text(300_000)).unwrap();
    fs::write(src.join("2019/b.bin"), rnd(400_000, 3)).unwrap();
    fs::write(src.join("note.txt"), text(1000)).unwrap();
    let arc = tmp.path().join("r.nva");
    let mut a = Archive::create(&arc).unwrap();
    a.add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Normal))
        .unwrap();
    let units_before = a.manifest.chunks.len();
    let bytes_before: Vec<(String, u64)> = a
        .manifest
        .files
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();

    // Move a whole folder, then rename one file. The folder itself is an
    // entry as well, so moving it moves three things, not two.
    assert_eq!(a.rename("photos/2019", "photos/archive/2019").unwrap(), 3);
    assert_eq!(a.rename("photos/note.txt", "photos/readme.txt").unwrap(), 1);
    // Not one new unit: no data was re-read or re-compressed.
    assert_eq!(a.manifest.chunks.len(), units_before);
    let after: Vec<String> = a.manifest.files.iter().map(|f| f.path.clone()).collect();
    assert!(after.contains(&"photos/archive/2019/a.txt".to_string()));
    assert!(after.contains(&"photos/readme.txt".to_string()));
    // Sizes are untouched, so the entries still point at the same bytes.
    let mut sizes: Vec<u64> = a.manifest.files.iter().map(|f| f.size).collect();
    let mut want: Vec<u64> = bytes_before.iter().map(|(_, s)| *s).collect();
    sizes.sort_unstable();
    want.sort_unstable();
    assert_eq!(sizes, want);

    // Refusals: a destination that already exists, and a folder into itself.
    assert!(a
        .rename("photos/readme.txt", "photos/archive/2019/a.txt")
        .is_err());
    assert!(a.rename("photos/archive", "photos/archive/deeper").is_err());
    assert!(a.rename("photos/nope.txt", "photos/x.txt").is_err());
    drop(a);

    let a = Archive::open_ro(&arc).unwrap();
    let out = tmp.path().join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_eq!(
        fs::read(out.join("photos/archive/2019/a.txt")).unwrap(),
        text(300_000)
    );
    assert_eq!(
        fs::read(out.join("photos/archive/2019/b.bin")).unwrap(),
        rnd(400_000, 3)
    );
    assert_eq!(fs::read(out.join("photos/readme.txt")).unwrap(), text(1000));
}

/// Archives written by the build that existed before recompression landed must
/// stay readable forever.
///
/// The fixtures are real archives, committed as binaries, covering every codec
/// and filter the format could produce at the time: store, zstd, LZMA2, LZMA2
/// with the BCJ filter, and PPMd7 at order 16. They exist because the changes
/// recompression needs touch the two numbers a decoder derives its window and
/// its model pool from, and getting either wrong does not fail loudly — a
/// narrower LZMA2 window fails only when some match happens to reach past it,
/// and a wrong PPMd7 pool decodes into garbage of exactly the right length.
/// Only the chunk hash catches that, and only on the data that trips it.
#[test]
fn archives_from_before_recompression_still_extract() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let tmp = tempfile::tempdir().unwrap();
    let mut checked = 0;
    for name in ["legacy-max.nva", "legacy-normal.nva", "legacy-ppmd.nva"] {
        let a = Archive::open_ro(&dir.join(name))
            .unwrap_or_else(|e| panic!("cannot open {name}: {e:#}"));
        assert!(!a.manifest.files.is_empty(), "{name} is empty");
        let out = tmp.path().join(name);
        // extract verifies every unit against its blake3, so this is the check.
        let stats = a
            .extract(&out, None, Overwrite::Fail)
            .unwrap_or_else(|e| panic!("cannot extract {name}: {e:#}"));
        assert_eq!(stats.files, a.manifest.files.len());
        for f in &a.manifest.files {
            let p = out.join(f.path.replace('/', std::path::MAIN_SEPARATOR_STR));
            assert_eq!(
                fs::metadata(&p).unwrap().len(),
                f.size,
                "{name}: {}",
                f.path
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 3);
}

/// Deflate recompression: the archive must be smaller AND extract byte-exact.
///
/// This is the one transform that rebuilds data from a record rather than
/// undoing itself, so it is the one where a bug produces an archive that cannot
/// be read back. The packer round-trips every such unit before keeping it and
/// falls back to compressing the file the ordinary way, which is what the
/// zip-of-random-bytes case below exercises.
#[test]
fn deflate_containers_are_recompressed_and_still_extract() {
    use std::io::Write as _;

    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("containers");
    fs::create_dir_all(&src).unwrap();

    // A gzip member of compressible text, written by flate2's deflate — an
    // encoder preflate has to model rather than recognise.
    let body = text(400_000);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(9));
    gz.write_all(&body).unwrap();
    fs::write(src.join("text.gz"), gz.finish().unwrap()).unwrap();
    // A gzip member of noise: nothing to gain, so the packer must fall back
    // without damaging anything.
    let mut gz2 = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(9));
    gz2.write_all(&rnd(300_000, 5)).unwrap();
    fs::write(src.join("noise.gz"), gz2.finish().unwrap()).unwrap();

    let arc = tmp.path().join("d.nva");
    let mut a = Archive::create(&arc).unwrap();
    let stats = a
        .add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Max))
        .unwrap();
    // At least one unit used the recompression filter. 37 is the mixed
    // container (deflate + JPEG); 34 is its deflate-only predecessor, which
    // still decodes but is no longer written.
    assert!(
        a.manifest.chunks.iter().any(|c| c.filter == 40),
        "no unit was recompressed: {:?}",
        a.manifest
            .chunks
            .iter()
            .map(|c| (c.filter, c.codec, c.unpacked, c.filtered))
            .collect::<Vec<_>>()
    );
    // A recompressed unit records what the codec saw, which is not its length.
    for c in a.manifest.chunks.iter().filter(|c| c.filter == 40) {
        assert_ne!(c.filtered, 0, "a length-changing filter must record it");
        assert_ne!(c.filtered, c.unpacked);
        assert_eq!(c.coded_len(), c.filtered);
    }
    // The gzip of text must beat storing it; the whole point.
    assert!(stats.bytes_stored < stats.bytes_in);
    drop(a);

    let a = Archive::open_ro(&arc).unwrap();
    let out = tmp.path().join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_same_tree(&src, &out.join("containers"));
}

/// JPEG recompression: photographs are the bulk of a family archive and the one
/// case every archiver gives up on. Measured on real camera JPEGs, 7-Zip -mx9
/// takes 0.20% off them and nova takes 19.2%.
///
/// The stored payload here is the lepton form itself, not a codec's output —
/// lepton's bitstream is already entropy-coded, so LZMA2 and PPMd7 never beat
/// it. That makes this the test that a STORED unit can legitimately carry a
/// filter, which nothing before recompression could.
#[test]
fn jpeg_photos_are_recompressed_and_still_extract() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("photos");
    fs::create_dir_all(&src).unwrap();

    // A photo-like image: smooth gradients with detail, so the DCT coefficients
    // look like a real picture rather than like noise or like flat colour.
    let (w, h) = (900usize, 700usize);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let r = (x * 255 / w) as u8;
            let g = (y * 255 / h) as u8;
            let b = (((x * x + y * y) / 97) % 256) as u8;
            rgb.extend_from_slice(&[r, g, b.wrapping_add((x as u8) >> 3)]);
        }
    }
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 90)
        .encode(&rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    assert!(jpeg.len() > 64 * 1024, "too small to be given its own unit");
    fs::write(src.join("photo.jpg"), &jpeg).unwrap();
    // Not a JPEG at all despite the magic: the transform must refuse it and the
    // packer must fall back without damaging anything.
    let mut fake = vec![0xFF, 0xD8, 0xFF, 0xE0];
    fake.extend(rnd(200_000, 9));
    fs::write(src.join("broken.jpg"), &fake).unwrap();

    let arc = tmp.path().join("p.nva");
    let mut a = Archive::create(&arc).unwrap();
    let stats = a
        .add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Max))
        .unwrap();
    let recompressed: Vec<_> = a
        .manifest
        .chunks
        .iter()
        .filter(|c| c.filter == 35)
        .collect();
    assert_eq!(recompressed.len(), 1, "exactly the real photo");
    let c = recompressed[0];
    assert_eq!(c.unpacked, jpeg.len() as u64);
    assert!(
        c.filtered > 0 && c.filtered < c.unpacked,
        "lepton must shrink it"
    );
    assert_eq!(c.coded_len(), c.filtered);
    assert_eq!(c.packed, c.filtered, "the lepton form is stored as it is");
    assert!(stats.bytes_stored < stats.bytes_in);
    drop(a);

    let a = Archive::open_ro(&arc).unwrap();
    let out = tmp.path().join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_same_tree(&src, &out.join("photos"));
}

/// A .wav goes in as PCM, comes back byte for byte, and is stored as FLAC.
///
/// The two ways this can go wrong are both here. The transform must survive a
/// container with the awkwardness real files have — a metadata chunk with an
/// odd length and its pad byte, a chunk AFTER `data`, and a `RIFF` size that
/// disagrees with the file — because none of that is reconstructed from a
/// parse, only spliced back. And a RIFF that is not PCM must be refused
/// without damaging anything, since a refusal is a fallback and not an error.
#[test]
fn wav_audio_is_recompressed_and_still_extracts() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("audio");
    fs::create_dir_all(&src).unwrap();

    // Two channels of a slowly-moving waveform with dither: what a match finder
    // cannot use and a linear predictor can.
    let frames = 300_000usize;
    let mut pcm = Vec::with_capacity(frames * 4);
    let mut seed = 0x8E37_79B9_7F4A_7C15u64;
    for i in 0..frames {
        let t = i as f64;
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let d = (seed >> 48) as i16 % 32;
        let l = ((t * 0.0121).sin() * 11000.0) as i16;
        let r = ((t * 0.0093).cos() * 10500.0) as i16;
        pcm.extend_from_slice(&l.wrapping_add(d).to_le_bytes());
        pcm.extend_from_slice(&r.wrapping_sub(d).to_le_bytes());
    }

    let riff = |body: &[u8], tail: &[u8]| {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        // Deliberately not the real length: real encoders get this wrong.
        out.extend_from_slice(&((body.len() + 1) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(body);
        out.extend_from_slice(tail);
        out
    };
    let chunk = |id: &[u8; 4], body: &[u8], out: &mut Vec<u8>| {
        out.extend_from_slice(id);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
    };
    let fmt_pcm = |tag: u16| {
        let mut f = Vec::new();
        f.extend_from_slice(&tag.to_le_bytes());
        f.extend_from_slice(&2u16.to_le_bytes()); // channels
        f.extend_from_slice(&44100u32.to_le_bytes());
        f.extend_from_slice(&(44100u32 * 4).to_le_bytes());
        f.extend_from_slice(&4u16.to_le_bytes()); // block align
        f.extend_from_slice(&16u16.to_le_bytes()); // bits
        f
    };

    let mut body = Vec::new();
    chunk(b"fmt ", &fmt_pcm(1), &mut body);
    chunk(b"LIST", b"INFOINAModd length", &mut body);
    chunk(b"data", &pcm, &mut body);
    chunk(b"id3 ", b"after the audio", &mut body);
    let wav = riff(&body, b"loose bytes past the last chunk");
    fs::write(src.join("music.wav"), &wav).unwrap();

    // IEEE float, not integer PCM: same magic, must be refused and stored the
    // ordinary way.
    let mut fbody = Vec::new();
    chunk(b"fmt ", &fmt_pcm(3), &mut fbody);
    chunk(b"data", &pcm, &mut fbody);
    let float = riff(&fbody, b"");
    fs::write(src.join("float.wav"), &float).unwrap();

    let arc = tmp.path().join("a.nva");
    let mut a = Archive::create(&arc).unwrap();
    let stats = a
        .add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Max))
        .unwrap();
    let coded: Vec<_> = a
        .manifest
        .chunks
        .iter()
        .filter(|c| c.filter == 38)
        .collect();
    assert_eq!(coded.len(), 1, "exactly the integer-PCM file");
    let c = coded[0];
    assert_eq!(c.unpacked, wav.len() as u64);
    assert!(
        c.filtered > 0 && c.filtered < c.unpacked,
        "flac must shrink it: {} vs {}",
        c.filtered,
        c.unpacked
    );
    // Well under what the record-width filter reaches on the same samples.
    assert!(
        c.packed * 10 < c.unpacked * 7,
        "{} is not comfortably below 70% of {}",
        c.packed,
        c.unpacked
    );
    assert!(stats.bytes_stored < stats.bytes_in);
    drop(a);

    let a = Archive::open_ro(&arc).unwrap();
    let out = tmp.path().join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_same_tree(&src, &out.join("audio"));
}

/// A zip that STORES its photographs is still recompressed.
///
/// Method 0 means the zip writer decided deflate could not help — which is
/// exactly what it does with JPEG, and exactly the data lepton can halve. The
/// scanner used to skip every entry that was not method 8, so a backup of
/// photographs, an epub with illustrations and an apk full of stored assets
/// all came out byte for byte the size they went in.
#[test]
fn a_zip_of_stored_jpegs_is_recompressed() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("bak");
    fs::create_dir_all(&src).unwrap();

    // Photo-like: smooth gradients with detail, so the DCT coefficients look
    // like a picture rather than like noise.
    let (w, h) = (700usize, 520usize);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let r = (x * 255 / w) as u8;
            let g = (y * 255 / h) as u8;
            let b = (((x * x + y * y) / 83) % 256) as u8;
            rgb.extend_from_slice(&[r, g, b.wrapping_add((y as u8) >> 3)]);
        }
    }
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 92)
        .encode(&rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb)
        .unwrap();

    // A minimal zip with one STORED entry. Written by hand so the test does not
    // depend on what a zip library decides to do.
    let name = b"photo.jpg";
    let crc = crc32(&jpeg);
    let mut zip = Vec::new();
    let lho = 0usize;
    zip.extend_from_slice(b"PK");
    zip.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // version..time/date
    zip.extend_from_slice(&crc.to_le_bytes());
    zip.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
    zip.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
    zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(name);
    zip.extend_from_slice(&jpeg);
    let cd = zip.len();
    zip.extend_from_slice(b"PK");
    zip.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    zip.extend_from_slice(&crc.to_le_bytes());
    zip.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
    zip.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
    zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
    zip.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // extra..attrs
    zip.extend_from_slice(&(lho as u32).to_le_bytes());
    zip.extend_from_slice(name);
    let cd_len = zip.len() - cd;
    zip.extend_from_slice(b"PK");
    zip.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
    zip.extend_from_slice(&(cd_len as u32).to_le_bytes());
    zip.extend_from_slice(&(cd as u32).to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());

    assert!(zip.len() > 64 * 1024, "must be big enough for its own unit");
    fs::write(src.join("photos.zip"), &zip).unwrap();

    let arc = tmp.path().join("z.nva");
    let mut a = Archive::create(&arc).unwrap();
    a.add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Max))
        .unwrap();
    let c = a
        .manifest
        .chunks
        .iter()
        .find(|c| c.filter == 40)
        .expect("the container filter must reach a stored jpeg");
    assert!(
        c.packed * 10 < c.unpacked * 9,
        "a stored jpeg must shrink well past 90%: {} of {}",
        c.packed,
        c.unpacked
    );
    drop(a);

    let a = Archive::open_ro(&arc).unwrap();
    let out = tmp.path().join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_same_tree(&src, &out.join("bak"));
}

/// CRC-32 as zip defines it. Only needed so the fixture is a valid archive.
fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, e) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *e = c;
    }
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// A .wav too large for one unit is CUT, not given up on.
///
/// The solo cap is the reader's bound — `read_packed` refuses a chunk above
/// `MAX_STORED_CHUNK` — so it cannot simply be raised for big audio. FLAC
/// frames are independent, so the file is split into unit-sized runs of whole
/// frames instead. Every piece after the first is bare PCM with no `fmt `
/// chunk in it, which is the whole reason the format travels with the plan
/// rather than being re-parsed from the bytes.
///
/// The second file is the control: same magic, IEEE float instead of integer
/// PCM, so the split must be refused and the ordinary path taken.
#[test]
fn a_wav_past_the_solo_cap_is_split_and_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("audio");
    fs::create_dir_all(&src).unwrap();

    let pcm = |frames: usize, seed0: u64| {
        let mut v = Vec::with_capacity(frames * 4);
        let mut seed = seed0;
        for i in 0..frames {
            let t = i as f64;
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let d = (seed >> 48) as i16 % 28;
            v.extend_from_slice(
                &(((t * 0.0107).sin() * 10500.0) as i16)
                    .wrapping_add(d)
                    .to_le_bytes(),
            );
            v.extend_from_slice(
                &(((t * 0.0089).cos() * 9800.0) as i16)
                    .wrapping_sub(d)
                    .to_le_bytes(),
            );
        }
        v
    };
    let wav_of = |body: &[u8], tag: u16, tail: &[u8]| {
        let mut w = Vec::with_capacity(body.len() + 64);
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&((36 + body.len()) as u32).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&tag.to_le_bytes());
        w.extend_from_slice(&2u16.to_le_bytes());
        w.extend_from_slice(&44100u32.to_le_bytes());
        w.extend_from_slice(&(44100u32 * 4).to_le_bytes());
        w.extend_from_slice(&4u16.to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&(body.len() as u32).to_le_bytes());
        w.extend_from_slice(body);
        w.extend_from_slice(tail);
        w
    };

    // The fast tier's unit is 4 MiB, so this spans several of them, and the
    // trailing bytes have to survive on the last piece.
    let long = wav_of(
        &pcm(3_000_000, 0x51ED_2701_A3B4_C5D6),
        1,
        b"loose bytes past the chunks",
    );
    assert!(
        long.len() > 8 * 1024 * 1024,
        "must exceed the fast tier's cap"
    );
    fs::write(src.join("long.wav"), &long).unwrap();
    // Same size, IEEE float: the split must refuse it.
    let float = wav_of(&pcm(3_000_000, 0x1122_3344_5566_7788), 3, b"");
    fs::write(src.join("float.wav"), &float).unwrap();

    let arc = tmp.path().join("w.nva");
    let mut a = Archive::create(&arc).unwrap();
    a.add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Fast))
        .unwrap();
    let coded: Vec<_> = a
        .manifest
        .chunks
        .iter()
        .filter(|c| c.filter == 38)
        .collect();
    assert!(
        coded.len() >= 2,
        "the long file must be cut into several transformed pieces, got {}",
        coded.len()
    );
    let carried: u64 = coded.iter().map(|c| c.unpacked).sum();
    assert_eq!(
        carried,
        long.len() as u64,
        "every byte of the .wav must be inside a transformed piece"
    );
    assert!(
        coded
            .iter()
            .all(|c| c.filtered > 0 && c.filtered < c.unpacked),
        "flac must shrink each piece"
    );
    // No unit may exceed what a reader accepts, whatever path produced it.
    assert!(
        a.manifest
            .chunks
            .iter()
            .all(|c| c.unpacked <= 64 * 1024 * 1024),
        "a unit above MAX_STORED_CHUNK cannot be extracted"
    );
    drop(a);

    let a = Archive::open_ro(&arc).unwrap();
    let out = tmp.path().join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_same_tree(&src, &out.join("audio"));
}

/// One big file, extracted with the intra-file decode lanes.
///
/// Extraction parallelises across FILES, so an archive holding a single large
/// file used one thread however many cores the budget allowed. The lanes fix
/// that by decoding the file's own units concurrently, and this is the shape
/// that turns them on: one file, many extents, nothing else to spread over.
///
/// The trap it guards is not the threading but the GROUPING. Consecutive
/// extents of one file usually share a unit, and a first version split the work
/// per extent — which decoded the same unit once per lane and made enwik8
/// SLOWER, 4.8 s to 8.2 s. Grouping by distinct unit took it to 1.6 s.
#[test]
fn one_large_file_extracts_through_parallel_lanes() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("big");
    fs::create_dir_all(&src).unwrap();

    // Large enough to span several units at the fast tier (4 MiB each), and
    // compressible so the units are real rather than stored.
    let mut data = text(24 << 20);
    // Vary it so the chunker cuts in more than one place and the extents do not
    // all collapse into one deduplicated unit.
    for (i, b) in data.iter_mut().enumerate() {
        if i % 4096 == 0 {
            *b = (i / 4096 % 251) as u8;
        }
    }
    fs::write(src.join("one.bin"), &data).unwrap();

    let arc = tmp.path().join("big.nva");
    let mut a = Archive::create(&arc).unwrap();
    a.add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Fast))
        .unwrap();
    let entry = a
        .manifest
        .files
        .iter()
        .find(|f| f.path.ends_with("one.bin"))
        .expect("the file is in the archive");
    assert!(
        entry.extents.len() > 4,
        "needs several extents to exercise the lanes, got {}",
        entry.extents.len()
    );
    drop(a);

    let a = Archive::open_ro(&arc).unwrap();
    let out = tmp.path().join("out");
    let stats = a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_eq!(stats.files, 1);
    assert_eq!(fs::read(out.join("big/one.bin")).unwrap(), data);
}

/// PDF recompression. A text PDF is almost entirely `/FlateDecode` streams —
/// page content, the object streams holding the objects themselves, embedded
/// font programs — and every other archiver treats the whole file as finished
/// data. Measured on 7.27 MB of technical documentation: nova max 6,394,652 →
/// 4,658,233 B, and 7-Zip -mx9 reaches 5,590,496 B.
///
/// The trap this pins down is the wrapper. PDF's FlateDecode is RFC 1950, so a
/// stream carries two zlib header bytes and a four-byte adler32 that are not
/// part of the deflate stream. Handed those, preflate modelled 0 of 957 real
/// streams — the feature silently did nothing at all while the scanner reported
/// 72% coverage.
#[test]
fn pdf_flate_streams_are_recompressed_and_still_extract() {
    use std::io::Write as _;

    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("docs");
    fs::create_dir_all(&src).unwrap();

    // A PDF built the way a real producer builds one: numbered objects, each
    // stream's dictionary naming its filter and its length, zlib around the
    // deflate. The cross-reference table is left out on purpose — the scanner
    // is lexical and must not need it.
    let mut pdf = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut streams = 0;
    for i in 1..=40u32 {
        let mut body = text(3000 + (i as usize % 7) * 500);
        body.extend_from_slice(format!("\nBT /F{i} 12 Tf (page {i}) Tj ET\n").as_bytes());
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(9));
        z.write_all(&body).unwrap();
        let z = z.finish().unwrap();
        assert_eq!(z[0], 0x78, "flate2 writes a zlib wrapper");
        pdf.extend_from_slice(
            format!(
                "{i} 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
                z.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&z);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        streams += 1;
    }
    // An uncompressed object stream and a JPEG-filtered one: neither is deflate,
    // and both sit between streams that are.
    pdf.extend_from_slice(b"90 0 obj\n<< /Type /Metadata /Length 20 >>\nstream\n");
    pdf.extend_from_slice(b"<?xpacket begin?>\n\n\n");
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    let noise = rnd(120_000, 17);
    pdf.extend_from_slice(
        format!(
            "91 0 obj\n<< /Filter /DCTDecode /Length {} >>\nstream\n",
            noise.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&noise);
    pdf.extend_from_slice(b"\nendstream\nendobj\ntrailer\n<< /Size 92 >>\n%%EOF\n");
    assert!(
        pdf.len() as u64 >= 64 * 1024,
        "smaller than MIN_CONTAINER, so it would never get its own unit"
    );
    assert_eq!(
        nova_core::deflate::find_streams(&pdf).len(),
        streams,
        "the scanner must find every FlateDecode stream and nothing else"
    );
    fs::write(src.join("manual.pdf"), &pdf).unwrap();

    // Claims to be a PDF and is not. The transform must refuse it and the
    // packer must fall back without damaging anything.
    let mut fake = b"%PDF-1.4\n".to_vec();
    fake.extend(rnd(200_000, 23));
    fs::write(src.join("broken.pdf"), &fake).unwrap();

    let arc = tmp.path().join("pdf.nva");
    let mut a = Archive::create(&arc).unwrap();
    let stats = a
        .add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Max))
        .unwrap();
    let recompressed: Vec<_> = a
        .manifest
        .chunks
        .iter()
        .filter(|c| c.filter == 40)
        .collect();
    assert_eq!(recompressed.len(), 1, "exactly the real PDF");
    let c = recompressed[0];
    assert_eq!(c.unpacked, pdf.len() as u64);
    assert_eq!(c.coded_len(), c.filtered);
    assert!(stats.bytes_stored < stats.bytes_in);
    drop(a);

    let a = Archive::open_ro(&arc).unwrap();
    let out = tmp.path().join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_same_tree(&src, &out.join("docs"));
}

/// An MP3 goes through the plane split and comes back byte for byte.
///
/// The shape of a real file matters more than the audio here: an ID3v2 tag in
/// front, a frame chain with VBR and padding, an ID3v1 trailer behind. Every
/// one of those is a different arm of the parser, and the last two are what a
/// filter that "just parses frames" gets wrong — everything nova does not
/// recognise has to survive verbatim, in place.
///
/// The second file is the control: `.mp3` in the name, no frames in the bytes.
/// It must be refused without damaging anything, because a refusal is a
/// fallback and not an error.
#[test]
fn mp3_audio_is_plane_split_and_still_extracts() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("music");
    fs::create_dir_all(&src).unwrap();

    // ID3v2.4 with one real frame, then 512 bytes of tag body.
    let mut mp3 = b"ID3\x04\x00\x00\x00\x00\x04\x00".to_vec();
    mp3.extend_from_slice(b"TIT2\x00\x00\x00\x08\x00\x00");
    mp3.extend_from_slice(&[0u8; 502]);

    // 1200 frames of joint-stereo 44.1 kHz, walking the bitrate and toggling
    // the padding bit the way a VBR encoder does. Side info drifts slowly and
    // spectral data is noise — the split is what separates them.
    let mut seed = 0x51ED_270B_9DBC_D1F1u64;
    let mut spectral = 0usize;
    for i in 0..1200usize {
        let bitrate_idx = (9 + (i / 97) % 5) as u8;
        let pad = i % 3 == 0;
        mp3.extend_from_slice(&[0xFF, 0xFB, (bitrate_idx << 4) | (u8::from(pad) << 1), 0x40]);
        // 144 * kbps * 1000 / 44100 + pad, for the five indices used above.
        let kbps = [128u32, 160, 192, 224, 256][(bitrate_idx - 9) as usize];
        let len = (144 * kbps * 1000 / 44100) as usize + usize::from(pad);
        for c in 0..32usize {
            mp3.push(((i / 8) as u8).wrapping_add(c as u8));
        }
        for _ in 0..len - 4 - 32 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            mp3.push((seed >> 56) as u8);
            spectral += 1;
        }
    }
    mp3.extend_from_slice(b"TAG");
    mp3.extend_from_slice(&[0x20u8; 125]);
    fs::write(src.join("track.mp3"), &mp3).unwrap();

    // Named .mp3, but there is not a frame in it.
    let fake = rnd(300_000, 0x1234_5678);
    fs::write(src.join("notreally.mp3"), &fake).unwrap();

    let arc = tmp.path().join("a.nva");
    let mut a = Archive::create(&arc).unwrap();
    let stats = a
        .add_paths(std::slice::from_ref(&src), &PackOptions::new(Tier::Max))
        .unwrap();
    let coded: Vec<_> = a
        .manifest
        .chunks
        .iter()
        .filter(|c| c.filter == 39)
        .collect();
    assert_eq!(coded.len(), 1, "exactly the file that has frames");
    let c = coded[0];
    assert_eq!(c.unpacked, mp3.len() as u64);
    // A permutation, so the coded length is the original plus a segment table.
    assert!(
        c.filtered >= c.unpacked && c.filtered < c.unpacked + 1024,
        "{} is not the original {} plus a small table",
        c.filtered,
        c.unpacked
    );
    // The spectral plane is noise and cannot shrink, so it is a floor nothing
    // can go below. Everything else — 4 header bytes and 32 side-info bytes per
    // frame, plus the tags — is structure, and the whole point of the split is
    // that a codec can then see it. Interleaved it costs its full size; in a
    // plane it must nearly vanish, so a quarter of it is a generous bar that
    // an unfiltered pack (which lands at ~100% of the file) cannot clear.
    let structure = c.unpacked - spectral as u64;
    assert!(
        c.packed < spectral as u64 + structure / 4,
        "{} left too much of the {structure} structural bytes above the {spectral}-byte floor",
        c.packed
    );
    assert!(stats.bytes_stored < stats.bytes_in);
    drop(a);

    let a = Archive::open_ro(&arc).unwrap();
    let out = tmp.path().join("out");
    a.extract(&out, None, Overwrite::Fail).unwrap();
    assert_same_tree(&src, &out.join("music"));
}

/// A damaged manifest must not let the tool destroy the archive.
///
/// This is the worst failure an archiver can have and it was live: one flipped
/// bit inside the newest manifest made `Archive::open` fall back to generation
/// 1 — which `create` writes EMPTY — and the read-write path then truncated the
/// file to that footer. Measured on 12,583,583 B of real data: `list` printed
/// "0 file(s)" and exited 0, `info` printed "Reclaimable: 12.0 MiB (run 'nova
/// compact')", and `compact` then produced a 133-byte file while reporting
/// success. The tool recommended the command that destroyed the data.
///
/// The discriminator is that a footer's self-hash covers its own offset, so a
/// footer that verified and whose manifest then failed is a COMMITTED record —
/// unlike a crash, which leaves no valid footer in the tail at all, because the
/// commit order is manifest → fsync → footer → fsync. `crash_recovery_ignores_
/// trailing_garbage` is the other side of this test and both must pass.
#[test]
fn a_damaged_manifest_does_not_let_a_write_destroy_the_archive() {
    let fx = fixture();
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &PackOptions::new(Tier::Fast))
        .unwrap();
    drop(a);
    let intact = fs::read(&fx.arc).unwrap();
    assert!(intact.len() > 10_000, "need a body worth losing");

    // Flip one bit in the middle of the newest manifest, leaving its footer
    // valid — bit rot, not a crash.
    // `footer_fields` gives the manifest offset and the footer offset, so the
    // manifest is everything between them.
    let (moff, foff) = footer_fields(&fx.arc);
    let mut bytes = intact.clone();
    bytes[(moff + (foff - moff) / 2) as usize] ^= 0x01;
    fs::write(&fx.arc, &bytes).unwrap();

    // Read-only still opens, still reports the fallback generation, and now
    // says so instead of pretending the archive is empty.
    let ro = Archive::open_ro(&fx.arc).unwrap();
    let d = ro.damage.expect("damage must be reported, not swallowed");
    assert_eq!(d.opened_generation, 1);
    assert!(d.lost_generation > d.opened_generation, "{d:?}");
    assert!(
        d.stranded_bytes > 10_000,
        "the stranded bytes are the whole point: {d:?}"
    );
    drop(ro);

    // Every write path must refuse, and the file must be untouched afterwards.
    let err = match Archive::open_rw(&fx.arc) {
        Ok(_) => panic!("open_rw accepted a damaged archive"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("damaged"), "{err}");
    assert_eq!(
        fs::read(&fx.arc).unwrap().len(),
        bytes.len(),
        "open_rw truncated a damaged archive"
    );

    // And the bytes are all still there, so a recovery tool has something to
    // work with even though nova itself will not write to it.
    assert_eq!(fs::read(&fx.arc).unwrap(), bytes);
}

/// `test` says OK on a healthy archive, and it counts everything the files
/// actually reference — a check that passes because it checked nothing would be
/// worse than no check at all.
#[test]
fn test_verifies_a_healthy_archive() {
    let fx = fixture();
    let opts = PackOptions::new(Tier::Normal);
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &opts).unwrap();
    drop(a);

    let a = Archive::open_ro(&fx.arc).unwrap();
    let s = a.test(&opts, None).unwrap();
    assert!(s.bad.is_empty(), "healthy archive reported damage: {:?}", s.bad);
    assert_eq!(s.chunks_ok, s.chunks);
    assert!(s.chunks > 0, "nothing was checked");
    assert!(s.damaged.is_empty());
    // The fixture is 3 MiB of noise plus 210 KB of text; a check that only
    // looked at the manifest would not be able to report that.
    assert!(
        s.bytes_ok >= 3 << 20,
        "only {} bytes were read back",
        s.bytes_ok
    );
}

/// And it FAILS on a flipped byte, naming the files that byte took with it.
/// This is the whole reason the verb exists, so it is worth pinning: a
/// checksum that is computed but never compared passes every test but this one.
#[test]
fn test_finds_a_flipped_byte_and_names_the_files() {
    let fx = fixture();
    let opts = PackOptions::new(Tier::Normal);
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &opts).unwrap();
    drop(a);

    // Somewhere in the payload, past the header and well before the manifest.
    let mut bytes = fs::read(&fx.arc).unwrap();
    let at = bytes.len() / 3;
    bytes[at] ^= 0xFF;
    fs::write(&fx.arc, &bytes).unwrap();

    let a = Archive::open_ro(&fx.arc).unwrap();
    let s = a.test(&opts, None).unwrap();
    assert!(!s.bad.is_empty(), "a flipped payload byte went unnoticed");
    assert!(
        !s.damaged.is_empty(),
        "a bad block was found but no file was blamed for it"
    );
    // The rest of the archive is still readable, and saying so is the point of
    // not stopping at the first failure.
    assert!(
        s.chunks_ok + s.bad.len() == s.chunks,
        "{} ok + {} bad != {} chunks",
        s.chunks_ok,
        s.bad.len(),
        s.chunks
    );
}

/// An extracted tree must BE the tree that was packed, not merely contain the
/// same bytes: empty folders, read-only files and sub-second timestamps are
/// part of what someone stored, and losing them silently is the failure this
/// pins down. Symlinks, ADS and ACLs are still not preserved — that is stated
/// in the docs rather than tested here.
#[test]
fn metadata_survives_the_round_trip() {
    let fx = fixture();
    let empty = fx.src.join("docs/empty-folder");
    fs::create_dir_all(&empty).unwrap();
    let ro = fx.src.join("docs/readonly.txt");
    fs::write(&ro, b"do not touch").unwrap();

    // A timestamp with a fraction: NTFS keeps 100 ns, and every archiver that
    // rounds to the second changes every file it touches.
    let stamp = filetime::FileTime::from_unix_time(1_700_000_000, 123_456_700);
    filetime::set_file_mtime(&ro, stamp).unwrap();
    let mut perms = fs::metadata(&ro).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&ro, perms).unwrap();

    let opts = PackOptions::new(Tier::Normal);
    let mut a = Archive::create(&fx.arc).unwrap();
    let add = a.add_paths(std::slice::from_ref(&fx.src), &opts).unwrap();
    assert!(add.dirs >= 3, "folders were not stored: {}", add.dirs);
    drop(a);

    let out = fx.root.join("out");
    let a = Archive::open_ro(&fx.arc).unwrap();
    let xs = a.extract(&out, None, Overwrite::Fail).unwrap();
    assert!(xs.dirs >= 3, "folders were not created: {}", xs.dirs);

    let back_empty = out.join("src/docs/empty-folder");
    assert!(
        back_empty.is_dir(),
        "an empty folder did not come back: {}",
        back_empty.display()
    );

    let back_ro = out.join("src/docs/readonly.txt");
    let meta = fs::metadata(&back_ro).unwrap();
    assert!(meta.permissions().readonly(), "read-only was not restored");
    let got = filetime::FileTime::from_last_modification_time(&meta);
    assert_eq!(got.unix_seconds(), stamp.unix_seconds(), "seconds differ");
    assert_eq!(
        got.nanoseconds(),
        stamp.nanoseconds(),
        "the sub-second part was rounded away"
    );

    // Leave nothing read-only behind, or the temp directory cannot be removed.
    for p in [&ro, &back_ro] {
        let mut perms = fs::metadata(p).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(p, perms).unwrap();
    }
}

/// A damaged archive must still give up everything it CAN. Stopping at the
/// first bad block is the difference between losing three files and losing all
/// of them, and a truncated file left on disk is worse than none at all —
/// nothing later would ever tell the user it was short.
#[test]
fn extraction_recovers_what_it_can_from_a_damaged_archive() {
    let fx = fixture();
    let opts = PackOptions::new(Tier::Normal);
    let mut a = Archive::create(&fx.arc).unwrap();
    a.add_paths(std::slice::from_ref(&fx.src), &opts).unwrap();
    drop(a);

    let a = Archive::open_ro(&fx.arc).unwrap();
    let stored_files = a.manifest.files.iter().filter(|f| !f.dir).count();
    drop(a);

    let mut bytes = fs::read(&fx.arc).unwrap();
    let at = bytes.len() / 3;
    bytes[at] ^= 0xFF;
    fs::write(&fx.arc, &bytes).unwrap();

    let out = fx.root.join("out");
    let a = Archive::open_ro(&fx.arc).unwrap();
    let xs = a.extract(&out, None, Overwrite::Fail).unwrap();

    assert!(!xs.failed.is_empty(), "the damage went unreported");
    assert!(
        xs.files > 0,
        "one bad block took the whole extraction with it"
    );
    // Nothing half-written is left behind for the files that failed.
    for (path, _) in &xs.failed {
        let target = out.join(path);
        assert!(
            !target.exists(),
            "a partial file was left at {}",
            target.display()
        );
    }
    // Every file is accounted for: recovered or named as lost, never silently
    // missing.
    assert_eq!(
        xs.files + xs.failed.len(),
        stored_files,
        "{} recovered + {} lost != {} stored",
        xs.files,
        xs.failed.len(),
        stored_files
    );
}
