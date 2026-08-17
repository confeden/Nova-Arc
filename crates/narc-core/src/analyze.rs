//! Phase 1 of the two-phase pipeline: decide *how* each file should be
//! stored before any of it is compressed.
//!
//! The decision is made from the file's head bytes: format magic first (an
//! already-compressed file must never be run through a second compressor),
//! then a content class (text, executable, generic binary), then a trial
//! compression to catch unknown formats that simply do not compress. The
//! class picks both the codec and the reversible filter to apply first.

use crate::codec::Codec;
use crate::filters::Filter;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Fast,
    Normal,
    Max,
}

impl Tier {
    pub fn zstd_level(self) -> i32 {
        match self {
            Tier::Fast => 3,
            Tier::Normal => 12,
            Tier::Max => 19,
        }
    }

    /// Memory one compression worker holds on top of its chunk buffers,
    /// dominated by the codec's match tables. Measured on 4 MiB chunks (see
    /// test/bench.sh): ~2 MiB for zstd level 3, ~37 MiB for level 12, ~50 MiB
    /// for level 19; the max tier also runs LZMA2 (4 MiB dictionary, ~10x
    /// dictionary for the encoder) and PPMd, so it reserves more.
    /// Used to decide how many workers fit in the memory budget.
    pub fn worker_memory(self) -> u64 {
        match self {
            Tier::Fast => 4 * 1024 * 1024,
            Tier::Normal => 40 * 1024 * 1024,
            Tier::Max => 96 * 1024 * 1024,
        }
    }
}

/// What the analyzer decided for a file (or a solid block).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub codec: Codec,
    pub filter: Filter,
}

impl Plan {
    pub const STORE: Plan = Plan {
        codec: Codec::Store,
        filter: Filter::None,
    };
}

/// Content classes worth treating differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// Entropy-coded already: JPEG, MP3, zip, video…
    Precompressed,
    /// Machine code: benefits from the BCJ address transform.
    Executable,
    /// Human-readable or source text: PPMd models it best.
    Text,
    Generic,
}

const TRIAL_SAMPLE: usize = 64 * 1024;

/// Decide how to store data given its first bytes.
pub fn plan(head: &[u8], tier: Tier) -> Plan {
    // Too small to classify and too small to matter: let the compressor try,
    // the per-chunk fallback stores it raw if that does not pay off.
    if head.len() < 256 {
        return Plan {
            codec: general_codec(tier),
            filter: Filter::None,
        };
    }
    match classify(head) {
        Class::Precompressed => Plan::STORE,
        Class::Executable => Plan {
            codec: general_codec(tier),
            // Machine code is full of relative call targets; making them
            // absolute turns repeated calls into repeated byte patterns.
            filter: Filter::BcjX86,
        },
        Class::Text => Plan {
            // PPMd's context modelling beats LZ on natural-language and
            // source text, but it is symmetric and slow, so only at max.
            codec: if tier == Tier::Max {
                Codec::Ppmd7
            } else {
                general_codec(tier)
            },
            filter: Filter::None,
        },
        Class::Generic => {
            if compresses(head) {
                Plan {
                    codec: general_codec(tier),
                    filter: Filter::None,
                }
            } else {
                Plan::STORE
            }
        }
    }
}

/// Backwards-compatible helper used where only the codec matters.
pub fn pick_codec(head: &[u8]) -> Codec {
    plan(head, Tier::Normal).codec
}

fn general_codec(tier: Tier) -> Codec {
    match tier {
        // LZMA2 gives ~2 percentage points over zstd -19 at similar speed,
        // and decodes fast enough to stay out of the user's way.
        Tier::Max => Codec::Lzma2,
        _ => Codec::Zstd,
    }
}

/// Cheap check that data is worth compressing at all: 3% savings on a sample.
fn compresses(head: &[u8]) -> bool {
    let sample = &head[..head.len().min(TRIAL_SAMPLE)];
    match zstd::bulk::compress(sample, 1) {
        Ok(c) => c.len() * 100 < sample.len() * 97,
        Err(_) => false,
    }
}

fn classify(b: &[u8]) -> Class {
    if is_precompressed(b) {
        return Class::Precompressed;
    }
    if is_executable(b) {
        return Class::Executable;
    }
    if is_text(b) {
        return Class::Text;
    }
    Class::Generic
}

fn is_executable(b: &[u8]) -> bool {
    // PE (also .dll/.sys), ELF, and Mach-O 64-bit both endians.
    b.starts_with(b"MZ")
        || b.starts_with(b"\x7FELF")
        || b.starts_with(&[0xCF, 0xFA, 0xED, 0xFE])
        || b.starts_with(&[0xFE, 0xED, 0xFA, 0xCF])
}

/// Heuristic: nearly all bytes are printable text or common whitespace, and
/// there are no NUL bytes. Deliberately conservative — misclassifying binary
/// as text would send it to PPMd, which is slow and would not pay off.
fn is_text(b: &[u8]) -> bool {
    let sample = &b[..b.len().min(8192)];
    let mut printable = 0usize;
    for &c in sample {
        match c {
            0 => return false,
            0x09 | 0x0A | 0x0D | 0x20..=0x7E => printable += 1,
            // UTF-8 continuation and lead bytes: assume text (Cyrillic, CJK…)
            0x80..=0xF4 => printable += 1,
            _ => {}
        }
    }
    printable * 100 >= sample.len() * 95
}

/// Formats that are already entropy-coded; storing them raw is faster and,
/// until the recompression milestone lands, just as small.
fn is_precompressed(b: &[u8]) -> bool {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return true; // JPEG
    }
    if b.starts_with(b"\x89PNG") || b.starts_with(b"GIF8") {
        return true;
    }
    if b.starts_with(b"PK\x03\x04") || b.starts_with(b"PK\x05\x06") {
        return true; // zip (also docx/xlsx/apk/jar)
    }
    if b.starts_with(&[0x1F, 0x8B]) || b.starts_with(b"BZh") || b.starts_with(b"\xFD7zXZ\x00") {
        return true; // gzip / bzip2 / xz
    }
    if b.starts_with(b"7z\xBC\xAF\x27\x1C") || b.starts_with(b"Rar!") {
        return true;
    }
    if b.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return true; // zstd
    }
    if b.starts_with(b"ID3")
        || (b.len() > 1 && b[0] == 0xFF && matches!(b[1], 0xFB | 0xFA | 0xF3 | 0xF2))
    {
        return true; // mp3
    }
    if b.starts_with(b"OggS") || b.starts_with(b"fLaC") {
        return true;
    }
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return true;
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        return true; // mp4/mov/heic/avif
    }
    if b.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return true; // mkv/webm
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_is_stored_verbatim() {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
        jpeg.extend(std::iter::repeat_n(0x5Au8, 1000));
        assert_eq!(plan(&jpeg, Tier::Max), Plan::STORE);
    }

    #[test]
    fn text_goes_to_ppmd_only_at_max() {
        let text = "fn main() { println!(\"hello world\"); }\n".repeat(50);
        assert_eq!(plan(text.as_bytes(), Tier::Max).codec, Codec::Ppmd7);
        assert_eq!(plan(text.as_bytes(), Tier::Normal).codec, Codec::Zstd);
        assert_eq!(plan(text.as_bytes(), Tier::Max).filter, Filter::None);
    }

    #[test]
    fn executables_get_the_bcj_filter() {
        let mut exe = b"MZ\x90\x00\x03\x00\x00\x00".to_vec();
        exe.extend(std::iter::repeat_n(0xE8u8, 500));
        let p = plan(&exe, Tier::Max);
        assert_eq!(p.filter, Filter::BcjX86);
        assert_eq!(p.codec, Codec::Lzma2);
    }

    #[test]
    fn incompressible_unknown_data_is_stored() {
        // deterministic pseudo-random bytes: no known magic, no redundancy
        let mut seed = 0x1234_5678u64;
        let data: Vec<u8> = (0..4096)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 24) as u8
            })
            .collect();
        assert_eq!(plan(&data, Tier::Max), Plan::STORE);
    }
}
