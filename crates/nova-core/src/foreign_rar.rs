//! Reading a plain, foreign .rar file — extraction only, and never writing.
//!
//! Same shape as [`crate::foreign_zip`] and [`crate::foreign_7z`], with one
//! rule they do not have: **nova will never create a RAR.** RARLAB's licence
//! permits using their unrar sources "in any software to handle RAR archives"
//! but forbids using them "to develop RAR (WinRAR) compatible archiver", and
//! the compression side is proprietary. Extraction is the whole feature.
//!
//! Reading sits behind the off-by-default `rar` feature, so a build that does
//! not ask for it links none of the vendored RARLAB code and nova's own
//! licence stays an open choice (research 05 §3). [`sniff`] is deliberately
//! OUTSIDE that gate — comparing six magic bytes needs nothing from RARLAB,
//! and a build without the feature can then say "this is a rar and I was not
//! built to read it" instead of "not a NOVA archive".
//!
//! Unlike the other two readers, this one drives a consuming state machine:
//! `read_header()` hands back a header that must be either extracted or
//! skipped, and each returns the archive for the next round. Entries are
//! therefore visited in stored order and cannot be revisited.

#[cfg(feature = "rar")]
use std::collections::HashSet;
#[cfg(feature = "rar")]
use std::fs;
use std::path::Path;
#[cfg(feature = "rar")]
use std::path::PathBuf;

#[cfg(feature = "rar")]
use anyhow::{bail, Result};

#[cfg(feature = "rar")]
use crate::archive::{ExtractStats, Overwrite};
#[cfg(feature = "rar")]
use crate::paths;

/// One entry as listed from a foreign rar's headers.
#[cfg(feature = "rar")]
pub struct RarEntry {
    pub path: String,
    pub size: u64,
}

/// True if `path` starts with a RAR signature — `Rar!\x1A\x07\x00` for the
/// RAR4 format, `Rar!\x1A\x07\x01\x00` for RAR5. Any read failure reads as
/// "not a rar", exactly as in the zip and 7z readers.
pub fn sniff(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 8];
    // A RAR4 signature is 7 bytes, so a file may legitimately be shorter than
    // the 8 read for RAR5; count what actually arrived rather than demanding
    // a full buffer.
    let Ok(n) = f.read(&mut magic) else {
        return false;
    };
    let head = &magic[..n];
    head.starts_with(b"Rar!\x1A\x07\x00") || head.starts_with(b"Rar!\x1A\x07\x01\x00")
}

/// List a rar's entries. Directory entries are omitted — parity with `.nva`,
/// which has none either.
#[cfg(feature = "rar")]
pub fn list(path: &Path) -> Result<Vec<RarEntry>> {
    let archive = unrar::Archive::new(path)
        .open_for_listing()
        .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", path.display()))?;
    let mut out = Vec::new();
    for entry in archive {
        let entry = entry.map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        if !entry.is_file() {
            continue;
        }
        out.push(RarEntry {
            path: rel_name(&entry.filename),
            size: entry.unpacked_size,
        });
    }
    Ok(out)
}

/// Extract selected (or all, if `select` is `None`) entries to `dest` under
/// the same [`Overwrite`] policy, selector syntax and path sanitization as
/// native `.nva` extraction.
#[cfg(feature = "rar")]
pub fn extract(
    path: &Path,
    dest: &Path,
    select: Option<&[String]>,
    overwrite: Overwrite,
) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let selectors: Option<Vec<String>> =
        select.map(|s| s.iter().map(|x| paths::normalize_selector(x)).collect());
    let mut used = vec![false; selectors.as_ref().map_or(0, |s| s.len())];

    // Pass 1 over the listing, before a single byte is written: which entries
    // were asked for, where each may safely land, and whether anything is in
    // the way. The processing pass below cannot do this itself — its headers
    // arrive one at a time and are consumed as they go, so by the time a
    // clash surfaced half the tree would already be on disk.
    let mut seen: HashSet<String> = HashSet::new();
    let mut work: Vec<(String, PathBuf)> = Vec::new();
    for entry in list(path)? {
        if let Some(sel) = &selectors {
            let mut hit = false;
            for (j, s) in sel.iter().enumerate() {
                if entry.path == *s || entry.path.starts_with(&format!("{s}/")) {
                    used[j] = true;
                    hit = true;
                }
            }
            if !hit {
                continue;
            }
        }
        // Never let unrar pick the path: the name is sanitized here and the
        // extraction below is told the exact file to write, so a hostile
        // entry cannot escape `dest` no matter what its header claims.
        let safe = match paths::sanitize(&entry.path) {
            Ok(p) => p,
            Err(e) => {
                stats.warnings.push(format!("skipped: {e}"));
                continue;
            }
        };
        if !seen.insert(paths::collision_key(&entry.path)) {
            stats.warnings.push(format!(
                "skipped {:?}: another entry maps to the same file name",
                entry.path
            ));
            continue;
        }
        work.push((entry.path, dest.join(&safe)));
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
    if overwrite == Overwrite::Fail {
        if let Some((_, target)) = work.iter().find(|(_, t)| t.exists()) {
            bail!(
                "{} already exists - use --force to overwrite or --skip-existing",
                target.display()
            );
        }
    }

    // Pass 2: walk the archive once, extracting only what pass 1 approved.
    let mut open = unrar::Archive::new(path)
        .open_for_processing()
        .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", path.display()))?;
    while let Some(header) = open
        .read_header()
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?
    {
        let name = rel_name(&header.entry().filename);
        let size = header.entry().unpacked_size;
        let target = header
            .entry()
            .is_file()
            .then(|| {
                work.iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, t)| t.clone())
            })
            .flatten();

        let Some(target) = target else {
            open = header
                .skip()
                .map_err(|e| anyhow::anyhow!("cannot skip {name:?}: {e}"))?;
            continue;
        };
        if target.exists() && overwrite == Overwrite::Skip {
            stats.skipped_existing += 1;
            open = header
                .skip()
                .map_err(|e| anyhow::anyhow!("cannot skip {name:?}: {e}"))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        open = header
            .extract_to(&target)
            .map_err(|e| anyhow::anyhow!("cannot extract {name:?}: {e}"))?;
        stats.files += 1;
        stats.bytes += size;
    }

    Ok(stats)
}

/// A rar header's name as an archive path. RAR stores '\' separators on
/// Windows-authored archives, the same trap the zip reader hit, so the
/// separator is normalized BEFORE `paths::sanitize` sees it — a `..\` entry
/// then meets the real `..`-component rule instead of being dropped for
/// containing a separator.
#[cfg(feature = "rar")]
fn rel_name(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[cfg(all(test, feature = "rar"))]
mod tests {
    use super::*;

    /// A real archive written by RAR 7.22, checked in because nothing in this
    /// project can produce one: creating RAR is what the licence forbids, so
    /// the fixture is the only way this reader is testable at all.
    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.rar")
    }

    #[test]
    fn sniff_accepts_a_real_rar_and_rejects_others() {
        assert!(sniff(&fixture()));

        let tmp = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            ("garbage.bin", &b"not a rar at all"[..]),
            ("empty.bin", b""),
            ("nova.nva", b"NOVAxxxxxxxxxxxx"),
            ("zip.zip", b"PK\x03\x04zzzzzzzz"),
        ] {
            let p = tmp.path().join(name);
            fs::write(&p, bytes).unwrap();
            assert!(!sniff(&p), "{name} must not sniff as rar");
        }
    }

    #[test]
    fn lists_and_extracts_a_real_rar() {
        let tmp = tempfile::tempdir().unwrap();
        let mut names: Vec<String> = list(&fixture())
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        names.sort();
        assert_eq!(names, ["src/a.txt", "src/sub/b.txt"]);

        let out = tmp.path().join("out");
        let stats = extract(&fixture(), &out, None, Overwrite::Fail).unwrap();
        assert_eq!(stats.files, 2);
        assert!(stats.warnings.is_empty(), "{:?}", stats.warnings);
        assert_eq!(
            fs::read_to_string(out.join("src/a.txt")).unwrap().trim(),
            "hello from rar"
        );
        assert_eq!(
            fs::read_to_string(out.join("src/sub/b.txt"))
                .unwrap()
                .trim(),
            "nested rar content"
        );
    }

    #[test]
    fn extracts_only_the_selected_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let sel = vec!["src/sub".to_string()];
        let stats = extract(&fixture(), &out, Some(&sel), Overwrite::Fail).unwrap();
        assert_eq!(stats.files, 1);
        assert!(out.join("src/sub/b.txt").exists());
        assert!(!out.join("src/a.txt").exists());
    }

    #[test]
    fn an_unmatched_selector_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let sel = vec!["nope/missing.txt".to_string()];
        assert!(extract(
            &fixture(),
            &tmp.path().join("out"),
            Some(&sel),
            Overwrite::Fail
        )
        .is_err());
    }

    #[test]
    fn overwrite_skip_and_fail_semantics() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");

        extract(&fixture(), &out, None, Overwrite::Fail).unwrap();
        assert!(extract(&fixture(), &out, None, Overwrite::Fail).is_err());

        fs::write(out.join("src/a.txt"), "kept").unwrap();
        let stats = extract(&fixture(), &out, None, Overwrite::Skip).unwrap();
        assert_eq!(stats.skipped_existing, 2);
        assert_eq!(stats.files, 0);
        assert_eq!(fs::read_to_string(out.join("src/a.txt")).unwrap(), "kept");

        let stats = extract(&fixture(), &out, None, Overwrite::Force).unwrap();
        assert_eq!(stats.files, 2);
        assert_eq!(
            fs::read_to_string(out.join("src/a.txt")).unwrap().trim(),
            "hello from rar"
        );
    }
}
