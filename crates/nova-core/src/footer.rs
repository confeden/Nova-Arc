//! On-disk fixed structures: 16-byte header and 80-byte footer.
//!
//! The newest valid footer lives at EOF-80. After a crash mid-update the file
//! may end with uncommitted garbage; recovery scans backwards for the last
//! valid footer. A footer's self-check only proves the footer itself is
//! intact, so `find_footer_before` is resumable: the caller verifies the
//! manifest a candidate points at and, if that fails (torn write, or a footer
//! image embedded inside stored chunk data), scans further back.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use anyhow::{anyhow, bail, Result};

pub const HEADER_MAGIC: &[u8; 4] = b"NOVA";
pub const HEADER_LEN: u64 = 16;
pub const FOOTER_MAGIC: &[u8; 8] = b"NOVAEND1";
pub const FOOTER_LEN: u64 = 80;
pub const VERSION: (u8, u8) = (0, 3);

/// The oldest layout this build still reads. Every id in the format is
/// permanent, so this has stayed at the first release and should keep staying
/// there; it exists so that the day something DOES have to break, the break is
/// a clear refusal rather than a silent misread.
pub const MIN_READABLE: (u8, u8) = (0, 0);

#[derive(Debug, Clone)]
pub struct Footer {
    pub generation: u64,
    pub manifest_offset: u64,
    pub manifest_packed: u64,
    pub manifest_unpacked: u64,
    /// blake3-128 of the compressed manifest bytes.
    pub manifest_hash: [u8; 16],
}

impl Footer {
    /// Encode a footer that will live at absolute offset `at`. The self-check
    /// hash covers the footer bytes *and* that offset, so a byte-identical
    /// copy of a footer stored elsewhere in the file (e.g. a `.nva` archived
    /// inside another one) can never be mistaken for a real commit.
    pub fn encode(&self, at: u64) -> [u8; FOOTER_LEN as usize] {
        let mut b = [0u8; FOOTER_LEN as usize];
        b[0..8].copy_from_slice(FOOTER_MAGIC);
        b[8..16].copy_from_slice(&self.generation.to_le_bytes());
        b[16..24].copy_from_slice(&self.manifest_offset.to_le_bytes());
        b[24..32].copy_from_slice(&self.manifest_packed.to_le_bytes());
        b[32..40].copy_from_slice(&self.manifest_unpacked.to_le_bytes());
        b[40..56].copy_from_slice(&self.manifest_hash);
        // bytes 56..64 reserved (zero)
        let h = self_hash(&b, at);
        b[64..80].copy_from_slice(&h);
        b
    }

    pub fn decode(b: &[u8; FOOTER_LEN as usize], at: u64) -> Option<Footer> {
        if &b[0..8] != FOOTER_MAGIC {
            return None;
        }
        if b[64..80] != self_hash(b, at) {
            return None;
        }
        let mut manifest_hash = [0u8; 16];
        manifest_hash.copy_from_slice(&b[40..56]);
        Some(Footer {
            generation: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            manifest_offset: u64::from_le_bytes(b[16..24].try_into().unwrap()),
            manifest_packed: u64::from_le_bytes(b[24..32].try_into().unwrap()),
            manifest_unpacked: u64::from_le_bytes(b[32..40].try_into().unwrap()),
            manifest_hash,
        })
    }
}

fn self_hash(b: &[u8; FOOTER_LEN as usize], at: u64) -> [u8; 16] {
    let mut h = blake3::Hasher::new();
    h.update(&b[0..64]);
    h.update(&at.to_le_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&h.finalize().as_bytes()[..16]);
    out
}

pub fn header_bytes() -> [u8; HEADER_LEN as usize] {
    let mut b = [0u8; HEADER_LEN as usize];
    b[0..4].copy_from_slice(HEADER_MAGIC);
    b[4] = VERSION.0;
    b[5] = VERSION.1;
    // bytes 6..8 flags, 8..16 reserved (zero)
    b
}

pub fn check_header(file: &mut File) -> Result<()> {
    let mut b = [0u8; HEADER_LEN as usize];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut b)
        .map_err(|_| anyhow!("file too short to be a NOVA archive"))?;
    if &b[0..4] != HEADER_MAGIC {
        bail!("not a NOVA archive (bad magic)");
    }
    // Pre-1.0 the format may change incompatibly on a minor bump, so the
    // minor version gates too. Once 1.0 ships this relaxes to major-only.
    if b[4] > VERSION.0 || (b[4] == VERSION.0 && b[5] > VERSION.1) {
        bail!(
            "archive format version {}.{} is newer than this build supports ({}.{})",
            b[4],
            b[5],
            VERSION.0,
            VERSION.1
        );
    }
    // The other direction, and it is the one a pre-1.0 promise needs. Every id
    // in this format is permanent (I5, I6) so old archives keep decoding, and
    // that is the intent — but "keeps decoding" has to be a decision, not an
    // accident. If a release ever does break an old layout, raising
    // `MIN_READABLE` makes this build REFUSE those archives by name instead of
    // decoding them into something that passes a length check and fails a hash,
    // or worse, passes both. Owner's rule for the beta: an archive is
    // guaranteed to open in the build that wrote it, and nothing more is
    // promised.
    if b[4] < MIN_READABLE.0 || (b[4] == MIN_READABLE.0 && b[5] < MIN_READABLE.1) {
        bail!(
            "archive format version {}.{} is older than this build reads ({}.{}); \
             open it with the version that wrote it",
            b[4],
            b[5],
            MIN_READABLE.0,
            MIN_READABLE.1
        );
    }
    Ok(())
}

/// Find the newest valid footer that starts strictly before `limit`.
/// Pass the file length to start from EOF; pass a previous candidate's offset
/// to resume the search below a footer whose manifest did not verify.
pub fn find_footer_before(file: &mut File, limit: u64) -> Result<(Footer, u64)> {
    const F: usize = FOOTER_LEN as usize;
    let file_len = file.metadata()?.len();
    let limit = limit.min(file_len);

    // Fast path: a committed archive ends exactly with its footer.
    if limit == file_len && file_len >= HEADER_LEN + FOOTER_LEN {
        let mut buf = [0u8; F];
        file.seek(SeekFrom::Start(file_len - FOOTER_LEN))?;
        file.read_exact(&mut buf)?;
        if let Some(f) = Footer::decode(&buf, file_len - FOOTER_LEN) {
            return Ok((f, file_len - FOOTER_LEN));
        }
    }

    // Backward scan in 1 MiB windows, overlapping by FOOTER_LEN-1 so a footer
    // straddling a window boundary is still found.
    const WIN: u64 = 1 << 20;
    let mut end = (limit + FOOTER_LEN).min(file_len);
    while end > HEADER_LEN {
        let start = end.saturating_sub(WIN).max(HEADER_LEN);
        let size = (end - start) as usize;
        let mut buf = vec![0u8; size];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buf)?;
        if size >= F {
            for i in (0..=size - F).rev() {
                let off = start + i as u64;
                if off >= limit {
                    continue;
                }
                if &buf[i..i + 8] == FOOTER_MAGIC {
                    let cand: &[u8; F] = buf[i..i + F].try_into().unwrap();
                    if let Some(f) = Footer::decode(cand, off) {
                        return Ok((f, off));
                    }
                }
            }
        }
        if start == HEADER_LEN {
            break;
        }
        end = start + (FOOTER_LEN - 1);
    }
    bail!("no valid NOVA footer found - archive is corrupt or truncated")
}
