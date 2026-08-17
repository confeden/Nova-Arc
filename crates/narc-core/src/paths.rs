//! Archive path handling. Internal paths are relative, UTF-8, '/'-separated.
//!
//! The same rules are enforced on BOTH sides: a path that extraction would
//! refuse can never enter an archive we write (otherwise `create` would
//! happily produce archives that `extract` cannot unpack). Extraction still
//! re-validates, because archives from elsewhere are untrusted input and must
//! never be able to write outside the destination directory.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Result};

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validate a single path component against the rules shared by add and
/// extract.
fn check_component(comp: &str) -> Result<()> {
    if comp.is_empty() || comp == "." || comp == ".." {
        bail!("unsafe path component {comp:?}");
    }
    if comp.contains(['\\', ':', '/']) {
        bail!("path component contains a separator or drive/stream colon: {comp:?}");
    }
    if comp.bytes().any(|b| b < 0x20) || comp.contains(['<', '>', '"', '|', '?', '*']) {
        bail!("path component contains a control or reserved character: {comp:?}");
    }
    if comp.ends_with(' ') || comp.ends_with('.') {
        bail!("path component ends with a space or dot: {comp:?}");
    }
    let stem = comp.split('.').next().unwrap_or(comp);
    if RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
        bail!("reserved device name in path: {comp:?}");
    }
    Ok(())
}

/// Convert an on-disk relative path into the archive's canonical form.
/// Rejects anything extraction would later refuse.
pub fn normalize_rel(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Normal(os) => match os.to_str() {
                Some(s) => {
                    check_component(s).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
                    parts.push(s.to_string());
                }
                None => bail!("path is not valid UTF-8: {}", path.display()),
            },
            Component::CurDir => {}
            _ => bail!("unsupported path component in {}", path.display()),
        }
    }
    if parts.is_empty() {
        bail!("empty path");
    }
    Ok(parts.join("/"))
}

/// Validate an archive-internal path and produce a safe relative PathBuf for
/// extraction. Rejects traversal, absolute paths, drive letters / ADS colons,
/// control characters and Windows reserved device names.
pub fn sanitize(rel: &str) -> Result<PathBuf> {
    if rel.is_empty() {
        bail!("empty path in archive");
    }
    let mut out = PathBuf::new();
    for comp in rel.split('/') {
        check_component(comp)
            .map_err(|e| anyhow::anyhow!("unsafe path in archive {rel:?}: {e}"))?;
        out.push(comp);
    }
    Ok(out)
}

/// Normalize a user-supplied selector ("src\docs" on Windows) to the
/// archive's canonical '/'-separated form, without trailing separator.
pub fn normalize_selector(sel: &str) -> String {
    sel.replace('\\', "/").trim_end_matches('/').to_string()
}

/// Case- and separator-insensitive key used to detect entries that would
/// collide on case-insensitive filesystems (NTFS, APFS default).
pub fn collision_key(rel: &str) -> String {
    rel.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{normalize_rel, normalize_selector, sanitize};
    use std::path::Path;

    #[test]
    fn rejects_traversal_and_absolute() {
        for bad in [
            "..",
            "../x",
            "a/../b",
            "/abs",
            "a//b",
            "C:\\evil",
            "c:/evil",
            "ads:stream",
            "nul",
            "NUL.txt",
            "con.jpg",
            "a\\b",
            "trailing. ",
            "trailing.",
            "x\u{0007}y",
            "wild*card",
            "pipe|name",
        ] {
            assert!(sanitize(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn accepts_normal() {
        for good in ["a.txt", "dir/sub/файл.jpg", "weird name (1).txt", "a.b.c"] {
            assert!(sanitize(good).is_ok(), "should accept {good:?}");
        }
    }

    /// Whatever add accepts, extract must accept too.
    #[test]
    fn add_and_extract_rules_agree() {
        for bad in ["note.", "name ", "a:b", "con.jpg", "nul"] {
            assert!(
                normalize_rel(Path::new(bad)).is_err(),
                "add should reject {bad:?} because extract does"
            );
        }
        for good in ["a.txt", "файл.jpg"] {
            let norm = normalize_rel(Path::new(good)).unwrap();
            assert!(sanitize(&norm).is_ok());
        }
    }

    #[test]
    fn selectors_normalize_windows_separators() {
        assert_eq!(normalize_selector("src\\docs\\"), "src/docs");
        assert_eq!(normalize_selector("src/docs"), "src/docs");
    }
}
