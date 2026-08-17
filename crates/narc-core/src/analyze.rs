//! Phase 1 of the two-phase pipeline: decide how a file should be stored
//! before compressing it. v0 uses magic-byte detection of already-compressed
//! formats plus trial compression of a head sample; later milestones add
//! per-type filters (delta, BCJ, recompression of JPEG/deflate/MP3).

use crate::codec::Codec;

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
}

const TRIAL_SAMPLE: usize = 64 * 1024;

/// Pick the storage codec for a file from its head bytes.
pub fn pick_codec(head: &[u8]) -> Codec {
    if head.len() < 256 {
        // Tiny files: compression is cheap; the per-chunk fallback stores the
        // chunk raw anyway if compression does not pay off.
        return Codec::Zstd;
    }
    if is_precompressed(head) {
        return Codec::Store;
    }
    let sample = &head[..head.len().min(TRIAL_SAMPLE)];
    match zstd::bulk::compress(sample, 1) {
        // Require at least 3% savings on the sample to bother compressing.
        Ok(c) if c.len() * 100 < sample.len() * 97 => Codec::Zstd,
        _ => Codec::Store,
    }
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
