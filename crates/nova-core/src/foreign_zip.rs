//! Reading and writing a plain, foreign .zip file — not nova's own container.
//!
//! `deflate::zip` already understands zip's central directory as a SOURCE of
//! deflate streams to recompress when a zip is being packed INTO an archive.
//! This module is the opposite direction: treating a `.zip` on disk as a
//! first-class archive in its own right, so `nova list`/`nova extract` work on
//! it without ever creating a `.nva`, and `nova create out.zip` produces one
//! anybody can open.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::analyze::Tier;
use crate::archive::{AddStats, ExtractStats, Overwrite};
use crate::paths;

/// One entry as listed from a foreign zip's central directory.
pub struct ZipEntry {
    pub path: String,
    pub size: u64,
}

/// True if `path` starts with a ZIP local-file-header signature
/// (`PK\x03\x04`). Any read failure (missing file, no permission, too
/// short) reads as "not a zip" — callers fall through to the native `.nva`
/// path, whose own error handling already covers those cases.
pub fn sniff(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).is_ok() && magic == *b"PK\x03\x04"
}

/// List a zip's entries in central-directory order. Directory entries are
/// omitted — parity with `.nva`, which has none either.
pub fn list(path: &Path) -> Result<Vec<ZipEntry>> {
    let mut zip = open(path)?;
    let mut out = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        out.push(ZipEntry {
            path: entry.name().replace('\\', "/"),
            size: entry.size(),
        });
    }
    Ok(out)
}

/// Extract selected (or all, if `select` is `None`) entries to `dest` under
/// the same [`Overwrite`] policy, selector syntax and path sanitization as
/// native `.nva` extraction (`archive.rs`'s `extract_reporting`).
pub fn extract(
    path: &Path,
    dest: &Path,
    select: Option<&[String]>,
    overwrite: Overwrite,
) -> Result<ExtractStats> {
    let mut zip = open(path)?;
    let mut stats = ExtractStats::default();
    let selectors: Option<Vec<String>> =
        select.map(|s| s.iter().map(|x| paths::normalize_selector(x)).collect());
    let mut used = vec![false; selectors.as_ref().map_or(0, |s| s.len())];

    // Selection, path safety and collision checks run once, before any
    // write — same two-pass shape as native extraction, so a refused
    // extraction never leaves half a tree behind.
    let mut seen: HashSet<String> = HashSet::new();
    let mut work: Vec<(usize, String, PathBuf)> = Vec::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        // ZIP mandates '/' (spec 4.4.17.1), but PowerShell's own
        // Compress-Archive writes '\' — normalized here, not in
        // `paths::sanitize`, so a traversal spelled with '\' still hits the
        // real check (the ".." component) instead of the separator check.
        let raw = entry.name().replace('\\', "/");

        if let Some(sel) = &selectors {
            let mut hit = false;
            for (j, s) in sel.iter().enumerate() {
                if raw == *s || raw.starts_with(&format!("{s}/")) {
                    used[j] = true;
                    hit = true;
                }
            }
            if !hit {
                continue;
            }
        }

        // A hostile or damaged entry must not abort the whole extraction —
        // zip-slip (CVE-2025-29787) is exactly what this closes off, and the
        // crate's own fix is not trusted alone: every raw name goes through
        // nova's own sanitizer before it ever touches a path join.
        let safe = match paths::sanitize(&raw) {
            Ok(p) => p,
            Err(e) => {
                stats.warnings.push(format!("skipped: {e}"));
                continue;
            }
        };
        if !seen.insert(paths::collision_key(&raw)) {
            stats.warnings.push(format!(
                "skipped {raw:?}: another entry maps to the same file name"
            ));
            continue;
        }
        work.push((i, raw, dest.join(&safe)));
    }

    if let Some(sel) = &selectors {
        let missing: Vec<&str> = sel
            .iter()
            .zip(&used)
            .filter(|(_, u)| !**u)
            .map(|(s, _)| s.as_str())
            .collect();
        if !missing.is_empty() {
            bail!("not found in archive: {}", missing.join(", "));
        }
    }

    // Fail fast, before writing anything: a refused extraction should not
    // leave half a tree behind.
    if overwrite == Overwrite::Fail {
        if let Some((_, _, target)) = work.iter().find(|(_, _, t)| t.exists()) {
            bail!(
                "{} already exists - use --force to overwrite or --skip-existing",
                target.display()
            );
        }
    }

    for (i, raw, target) in &work {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = match OpenOptions::new().write(true).create_new(true).open(target) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match overwrite {
                Overwrite::Fail => bail!(
                    "{} already exists - use --force to overwrite or --skip-existing",
                    target.display()
                ),
                Overwrite::Skip => {
                    stats.skipped_existing += 1;
                    continue;
                }
                Overwrite::Force => File::create(target)
                    .with_context(|| format!("cannot write {}", target.display()))?,
            },
            Err(e) => return Err(e).with_context(|| format!("cannot write {}", target.display())),
        };
        let mut entry = zip.by_index(*i)?;
        let written = std::io::copy(&mut entry, &mut out)
            .with_context(|| format!("cannot extract {raw:?}"))?;
        stats.files += 1;
        stats.bytes += written;
    }

    Ok(stats)
}

/// Write a plain zip at `path` holding every file under `inputs`.
///
/// Deflate only, at a level taken from the tier. The stronger methods zip
/// allows (bzip2, zstd, xz) would beat it on ratio, but a zip is written to
/// be opened by something that is not nova — and deflate is the one method
/// every reader in existence supports. Anyone who wants ratio wants `.nva`.
///
/// Entries keep the order the walk produced. Sorting by extension is what
/// `.nva` does to pack similar files into one solid unit; a zip compresses
/// each entry on its own, so the sort would buy nothing and only scramble
/// the listing.
pub fn create(path: &Path, inputs: &[PathBuf], tier: Tier) -> Result<AddStats> {
    let mut stats = AddStats::default();
    let walk = paths::walk_inputs(inputs, |_| {})?;
    stats.symlinks_skipped = walk.symlinks_skipped;

    // create_new, like `Archive::create`: an archiver must not silently
    // destroy whatever the output path already names.
    let out = File::create_new(path)
        .with_context(|| format!("cannot create {} (already exists?)", path.display()))?;
    let mut zip = zip::ZipWriter::new(out);
    let level = match tier {
        Tier::Fast => 1,
        Tier::Normal => 6,
        Tier::Max => 9,
    };

    let mut seen: HashSet<String> = HashSet::new();
    for f in &walk.files {
        if !seen.insert(paths::collision_key(&f.rel)) {
            stats.warnings.push(format!(
                "skipped {:?}: another file maps to the same archive name",
                f.rel
            ));
            continue;
        }
        // ZIP64 costs 20 bytes an entry, and without it the crate refuses a
        // member over 4 GiB outright — so it is asked for per entry, from the
        // size the walk already read, instead of always or never.
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(level))
            .large_file(f.size >= u32::MAX as u64);
        zip.start_file(&f.rel, opts)
            .with_context(|| format!("cannot start zip entry {:?}", f.rel))?;
        let mut src =
            File::open(&f.disk).with_context(|| format!("cannot read {}", f.disk.display()))?;
        let n = std::io::copy(&mut src, &mut zip)
            .with_context(|| format!("cannot store {}", f.disk.display()))?;
        stats.files += 1;
        stats.bytes_in += n;
    }
    zip.finish()
        .with_context(|| format!("cannot finish {}", path.display()))?;

    // The zip's own size on disk, not a sum of per-entry sizes: it is the
    // number the user sees, headers and central directory included.
    stats.bytes_stored = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(stats)
}

fn open(path: &Path) -> Result<zip::ZipArchive<BufReader<File>>> {
    let f = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    zip::ZipArchive::new(BufReader::new(f))
        .with_context(|| format!("not a valid zip: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default();
        for (name, body) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(body).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn write_temp(dir: &std::path::Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn round_trip_normal_zip() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(&[("a.txt", b"hello"), ("sub/b.txt", b"world!!")]);
        let zip_path = write_temp(tmp.path(), "in.zip", &zip_bytes);

        assert!(sniff(&zip_path));
        let entries = list(&zip_path).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.path == "a.txt" && e.size == 5));
        assert!(entries.iter().any(|e| e.path == "sub/b.txt" && e.size == 7));

        let out = tmp.path().join("out");
        let stats = extract(&zip_path, &out, None, Overwrite::Fail).unwrap();
        assert_eq!(stats.files, 2);
        assert_eq!(stats.bytes, 12);
        assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(out.join("sub/b.txt")).unwrap(), b"world!!");
    }

    /// PowerShell's `Compress-Archive` writes entry names with '\' instead
    /// of the spec-mandated '/' (found by manually smoke-testing a zip it
    /// produced) — `sanitize` alone reads a bare '\' as a hostile character
    /// and drops the entry, so real Windows-made zips lost every nested file.
    #[test]
    fn normalizes_backslash_separators() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(&[("sub\\b.txt", b"world!!")]);
        let zip_path = write_temp(tmp.path(), "in.zip", &zip_bytes);

        let entries = list(&zip_path).unwrap();
        assert_eq!(entries[0].path, "sub/b.txt");

        let out = tmp.path().join("out");
        let stats = extract(&zip_path, &out, None, Overwrite::Fail).unwrap();
        assert_eq!(
            stats.files, 1,
            "must not be dropped as unsafe: {:?}",
            stats.warnings
        );
        assert_eq!(fs::read(out.join("sub/b.txt")).unwrap(), b"world!!");
    }

    /// A traversal spelled with '\' must still be caught, by the ".."
    /// component check rather than the separator check.
    #[test]
    fn rejects_backslash_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(&[("..\\evil.txt", b"bad")]);
        let zip_path = write_temp(tmp.path(), "in.zip", &zip_bytes);

        let out = tmp.path().join("out");
        let stats = extract(&zip_path, &out, None, Overwrite::Fail).unwrap();
        assert_eq!(stats.files, 0);
        assert_eq!(stats.warnings.len(), 1);
        assert!(!tmp.path().join("evil.txt").exists());
    }

    #[test]
    fn rejects_traversal_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(&[("../evil.txt", b"bad"), ("good.txt", b"ok")]);
        let zip_path = write_temp(tmp.path(), "in.zip", &zip_bytes);

        let out = tmp.path().join("out");
        let stats = extract(&zip_path, &out, None, Overwrite::Fail).unwrap();
        assert_eq!(stats.files, 1, "only the safe entry extracts");
        assert_eq!(stats.warnings.len(), 1);
        assert!(!tmp.path().join("evil.txt").exists());
        assert!(out.join("good.txt").exists());
    }

    #[test]
    fn rejects_absolute_and_drive_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(&[("/etc/passwd", b"bad"), ("C:/evil.txt", b"bad")]);
        let zip_path = write_temp(tmp.path(), "in.zip", &zip_bytes);

        let out = tmp.path().join("out");
        let stats = extract(&zip_path, &out, None, Overwrite::Fail).unwrap();
        assert_eq!(stats.files, 0);
        assert_eq!(stats.warnings.len(), 2);
    }

    #[test]
    fn sniff_rejects_non_zip() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!sniff(&write_temp(
            tmp.path(),
            "garbage.bin",
            b"not a zip at all"
        )));
        assert!(!sniff(&write_temp(tmp.path(), "empty.bin", b"")));
        assert!(!sniff(&write_temp(
            tmp.path(),
            "nova.nva",
            b"NOVAxxxxxxxxxxxx"
        )));
    }

    #[test]
    fn sniff_accepts_zip_without_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(&[("a.txt", b"x")]);
        let path = write_temp(tmp.path(), "no_extension_at_all", &zip_bytes);
        assert!(sniff(&path));
    }

    #[test]
    fn overwrite_skip_and_fail_semantics() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(&[("a.txt", b"first")]);
        let zip_path = write_temp(tmp.path(), "in.zip", &zip_bytes);
        let out = tmp.path().join("out");

        extract(&zip_path, &out, None, Overwrite::Fail).unwrap();
        assert!(extract(&zip_path, &out, None, Overwrite::Fail).is_err());

        let stats = extract(&zip_path, &out, None, Overwrite::Skip).unwrap();
        assert_eq!(stats.skipped_existing, 1);
        assert_eq!(stats.files, 0);
        assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"first");
    }

    /// Build a tree on disk, zip it, then read it back through this module's
    /// own reader — and through the `zip` crate directly, so the test would
    /// still catch a writer that only our own reader happens to accept.
    #[test]
    fn create_round_trips_a_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        let body = "compressible text ".repeat(500);
        fs::write(src.join("a.txt"), &body).unwrap();
        fs::write(src.join("sub/b.bin"), [7u8; 300]).unwrap();

        let made = tmp.path().join("made.zip");
        let stats = create(&made, std::slice::from_ref(&src), Tier::Normal).unwrap();
        assert_eq!(stats.files, 2);
        assert_eq!(stats.bytes_in, body.len() as u64 + 300);
        assert!(stats.bytes_stored > 0);
        assert!(
            stats.bytes_stored < stats.bytes_in,
            "deflate must actually compress: {} vs {}",
            stats.bytes_stored,
            stats.bytes_in
        );

        assert!(sniff(&made), "what we write must sniff as a zip");
        let mut names: Vec<String> = list(&made).unwrap().into_iter().map(|e| e.path).collect();
        names.sort();
        assert_eq!(names, ["src/a.txt", "src/sub/b.bin"]);

        let back = tmp.path().join("back");
        extract(&made, &back, None, Overwrite::Fail).unwrap();
        assert_eq!(fs::read(back.join("src/a.txt")).unwrap(), body.as_bytes());
        assert_eq!(fs::read(back.join("src/sub/b.bin")).unwrap(), [7u8; 300]);
    }

    #[test]
    fn create_refuses_to_clobber() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("f.txt");
        fs::write(&src, b"x").unwrap();
        let out = write_temp(tmp.path(), "taken.zip", b"do not destroy me");

        assert!(create(&out, &[src], Tier::Fast).is_err());
        assert_eq!(fs::read(&out).unwrap(), b"do not destroy me");
    }

    /// Every tier must produce a zip, and a stronger one must not be bigger —
    /// the tier only picks a deflate level, so this pins the mapping without
    /// asserting an exact size the zlib version could move.
    #[test]
    fn create_honours_the_tier() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("f.txt");
        fs::write(&src, "the quick brown fox ".repeat(2000)).unwrap();

        let mut sizes = Vec::new();
        for (i, tier) in [Tier::Fast, Tier::Normal, Tier::Max]
            .into_iter()
            .enumerate()
        {
            let out = tmp.path().join(format!("t{i}.zip"));
            let stats = create(&out, std::slice::from_ref(&src), tier).unwrap();
            assert_eq!(stats.files, 1);
            sizes.push(stats.bytes_stored);
        }
        assert!(
            sizes[0] >= sizes[2],
            "max must not be larger than fast: {sizes:?}"
        );
    }
}
