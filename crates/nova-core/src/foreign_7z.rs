//! Reading a plain, foreign .7z file directly — not nova's own container.
//!
//! Same shape as [`crate::foreign_zip`]: `nova list`/`nova extract` treat a
//! `.7z` on disk as a first-class, listable, extractable archive, detected by
//! its magic bytes rather than its extension.
//!
//! Extraction runs through [`sevenz_rust2::ArchiveReader::for_each_entries`],
//! a single pass over the archive's blocks, rather than `read_file` per
//! entry: the crate's own docs warn `read_file` "is very inefficient when
//! used with solid archives, since it needs to decode all data before the
//! actual file" — calling it once per selected file would redecode a solid
//! block once per file it contains.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::archive::{ExtractStats, Overwrite};
use crate::paths;

/// One entry as listed from a foreign 7z's header.
pub struct SevenZEntry {
    pub path: String,
    pub size: u64,
}

/// True if `path` starts with the 7z signature (`7z\xBC\xAF\x27\x1C`). Any
/// read failure (missing file, no permission, too short) reads as "not a
/// 7z" — callers fall through to the native `.nva` path, whose own error
/// handling already covers those cases.
pub fn sniff(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 6];
    f.read_exact(&mut magic).is_ok() && magic == *b"7z\xBC\xAF\x27\x1C"
}

/// List a 7z's entries. Directory entries are omitted — parity with `.nva`,
/// which has none either.
pub fn list(path: &Path) -> Result<Vec<SevenZEntry>> {
    let reader = open(path)?;
    let mut out = Vec::with_capacity(reader.archive().files.len());
    for entry in &reader.archive().files {
        if entry.is_directory() {
            continue;
        }
        out.push(SevenZEntry {
            // 7-Zip itself writes '\' on Windows; see `write_one`'s sibling
            // note in foreign_zip for why this is normalized before, not
            // inside, `paths::sanitize`.
            path: entry.name().replace('\\', "/"),
            size: entry.size(),
        });
    }
    Ok(out)
}

/// Extract selected (or all, if `select` is `None`) entries to `dest` under
/// the same [`Overwrite`] policy, selector syntax and path sanitization as
/// native `.nva` extraction and [`crate::foreign_zip::extract`].
pub fn extract(
    path: &Path,
    dest: &Path,
    select: Option<&[String]>,
    overwrite: Overwrite,
) -> Result<ExtractStats> {
    let mut reader = open(path)?;
    let mut stats = ExtractStats::default();
    let selectors: Option<Vec<String>> =
        select.map(|s| s.iter().map(|x| paths::normalize_selector(x)).collect());
    let mut used = vec![false; selectors.as_ref().map_or(0, |s| s.len())];

    // Pass 1: metadata already sits in `reader.archive().files`, no decode
    // needed. Selection, path safety and collision checks run once, before
    // any write — same two-pass shape as native extraction, so a refused
    // extraction never leaves half a tree behind. Keyed by the RAW name,
    // because that is what `for_each_entries` hands back in pass 2.
    let mut seen: HashSet<String> = HashSet::new();
    let mut work: HashMap<String, PathBuf> = HashMap::new();
    for entry in &reader.archive().files {
        if entry.is_directory() {
            continue;
        }
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
        // same defense-in-depth reasoning as foreign_zip's zip-slip note:
        // every raw name is sanitized before it ever touches a path join.
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
        work.insert(entry.name().to_string(), dest.join(&safe));
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
        if let Some(target) = work.values().find(|t| t.exists()) {
            bail!(
                "{} already exists - use --force to overwrite or --skip-existing",
                target.display()
            );
        }
    }

    // Pass 2: one decode pass over the archive's blocks. `hard_error` stops
    // the loop (`Ok(false)`) instead of trying to carry an anyhow::Error
    // through sevenz_rust2's own Result — an unmatched entry is simply not
    // read, which is cheap: the crate advances past it without decoding
    // it into a buffer of our own.
    let mut file_count = 0usize;
    let mut byte_count = 0u64;
    let mut skip_count = 0usize;
    let mut hard_error: Option<anyhow::Error> = None;
    reader.for_each_entries(|entry, data| {
        let Some(target) = work.get(entry.name()) else {
            return Ok(true);
        };
        match write_one(data, target, overwrite) {
            Ok(Some(bytes)) => {
                file_count += 1;
                byte_count += bytes;
                Ok(true)
            }
            Ok(None) => {
                skip_count += 1;
                Ok(true)
            }
            Err(e) => {
                hard_error = Some(e);
                Ok(false)
            }
        }
    })?;
    if let Some(e) = hard_error {
        return Err(e);
    }

    stats.files = file_count;
    stats.bytes = byte_count;
    stats.skipped_existing = skip_count;
    Ok(stats)
}

/// Write one entry's already-decoded stream to `target`. `Ok(None)` means
/// `Overwrite::Skip` left an existing file alone — the same three-way split
/// as native extraction's `extract_one` (archive.rs).
fn write_one(data: &mut dyn Read, target: &Path, overwrite: Overwrite) -> Result<Option<u64>> {
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
            Overwrite::Skip => return Ok(None),
            Overwrite::Force => File::create(target)
                .with_context(|| format!("cannot write {}", target.display()))?,
        },
        Err(e) => return Err(e).with_context(|| format!("cannot write {}", target.display())),
    };
    Ok(Some(std::io::copy(data, &mut out)?))
}

fn open(path: &Path) -> Result<sevenz_rust2::ArchiveReader<File>> {
    sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())
        .with_context(|| format!("not a valid 7z: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_7z(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = sevenz_rust2::ArchiveWriter::new(std::io::Cursor::new(Vec::new())).unwrap();
        for (name, body) in entries {
            w.push_archive_entry(
                sevenz_rust2::ArchiveEntry::new_file(name),
                Some(std::io::Cursor::new(body.to_vec())),
            )
            .unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn write_temp(dir: &std::path::Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn round_trip_normal_7z() {
        let tmp = tempfile::tempdir().unwrap();
        let sz_bytes = build_7z(&[("a.txt", b"hello"), ("sub/b.txt", b"world!!")]);
        let sz_path = write_temp(tmp.path(), "in.7z", &sz_bytes);

        assert!(sniff(&sz_path));
        let entries = list(&sz_path).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.path == "a.txt" && e.size == 5));
        assert!(entries.iter().any(|e| e.path == "sub/b.txt" && e.size == 7));

        let out = tmp.path().join("out");
        let stats = extract(&sz_path, &out, None, Overwrite::Fail).unwrap();
        assert_eq!(stats.files, 2);
        assert_eq!(stats.bytes, 12);
        assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(out.join("sub/b.txt")).unwrap(), b"world!!");
    }

    #[test]
    fn rejects_traversal_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let sz_bytes = build_7z(&[("../evil.txt", b"bad"), ("good.txt", b"ok")]);
        let sz_path = write_temp(tmp.path(), "in.7z", &sz_bytes);

        let out = tmp.path().join("out");
        let stats = extract(&sz_path, &out, None, Overwrite::Fail).unwrap();
        assert_eq!(stats.files, 1, "only the safe entry extracts");
        assert_eq!(stats.warnings.len(), 1);
        assert!(!tmp.path().join("evil.txt").exists());
        assert!(out.join("good.txt").exists());
    }

    #[test]
    fn rejects_absolute_and_drive_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let sz_bytes = build_7z(&[("/etc/passwd", b"bad"), ("C:/evil.txt", b"bad")]);
        let sz_path = write_temp(tmp.path(), "in.7z", &sz_bytes);

        let out = tmp.path().join("out");
        let stats = extract(&sz_path, &out, None, Overwrite::Fail).unwrap();
        assert_eq!(stats.files, 0);
        assert_eq!(stats.warnings.len(), 2);
    }

    /// 7-Zip itself writes '\' on Windows-authored archives — learned from
    /// the same bug in foreign_zip, fixed here from the start.
    #[test]
    fn normalizes_backslash_separators() {
        let tmp = tempfile::tempdir().unwrap();
        let sz_bytes = build_7z(&[("sub\\b.txt", b"world!!")]);
        let sz_path = write_temp(tmp.path(), "in.7z", &sz_bytes);

        let entries = list(&sz_path).unwrap();
        assert_eq!(entries[0].path, "sub/b.txt");

        let out = tmp.path().join("out");
        let stats = extract(&sz_path, &out, None, Overwrite::Fail).unwrap();
        assert_eq!(
            stats.files, 1,
            "must not be dropped as unsafe: {:?}",
            stats.warnings
        );
        assert_eq!(fs::read(out.join("sub/b.txt")).unwrap(), b"world!!");
    }

    #[test]
    fn rejects_backslash_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let sz_bytes = build_7z(&[("..\\evil.txt", b"bad")]);
        let sz_path = write_temp(tmp.path(), "in.7z", &sz_bytes);

        let out = tmp.path().join("out");
        let stats = extract(&sz_path, &out, None, Overwrite::Fail).unwrap();
        assert_eq!(stats.files, 0);
        assert_eq!(stats.warnings.len(), 1);
        assert!(!tmp.path().join("evil.txt").exists());
    }

    #[test]
    fn sniff_rejects_non_7z() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!sniff(&write_temp(
            tmp.path(),
            "garbage.bin",
            b"not a 7z at all"
        )));
        assert!(!sniff(&write_temp(tmp.path(), "empty.bin", b"")));
        assert!(!sniff(&write_temp(
            tmp.path(),
            "nova.nva",
            b"NOVAxxxxxxxxxxxx"
        )));
    }

    #[test]
    fn sniff_accepts_7z_without_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let sz_bytes = build_7z(&[("a.txt", b"x")]);
        let path = write_temp(tmp.path(), "no_extension_at_all", &sz_bytes);
        assert!(sniff(&path));
    }

    #[test]
    fn overwrite_skip_and_fail_semantics() {
        let tmp = tempfile::tempdir().unwrap();
        let sz_bytes = build_7z(&[("a.txt", b"first")]);
        let sz_path = write_temp(tmp.path(), "in.7z", &sz_bytes);
        let out = tmp.path().join("out");

        extract(&sz_path, &out, None, Overwrite::Fail).unwrap();
        assert!(extract(&sz_path, &out, None, Overwrite::Fail).is_err());

        let stats = extract(&sz_path, &out, None, Overwrite::Skip).unwrap();
        assert_eq!(stats.skipped_existing, 1);
        assert_eq!(stats.files, 0);
        assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"first");
    }
}
