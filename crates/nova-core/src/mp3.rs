//! MPEG audio (Layer III) → separated planes, and back byte for byte.
//!
//! Every archiver stores an MP3 at 100% of its size, and nearly all of it
//! deserves to be: the spectral data is Huffman-coded and a general codec has
//! nothing to say about it. But an MP3 is not only spectral data. Interleaved
//! with it, once every 26 ms, sit a 4-byte frame header and 9-32 bytes of side
//! information — and those are *not* noise. The header is all but constant
//! across a file, and the side info is a set of slowly-drifting numbers
//! (`main_data_begin`, `global_gain`, `part2_3_length`) that a compressor could
//! model easily if it ever saw two of them in a row.
//!
//! It never does, because they are 4 bytes of structure every 400 bytes of
//! noise. **This filter separates them.** Headers go in one plane, side info in
//! another, spectral data in a third; the codec that follows then sees a long
//! run of similar numbers instead of a needle every frame.
//!
//! WHAT MAKES THE ROUND TRIP EXACT is the same principle as `crate::wav`:
//! nothing is reconstructed from a parse. Every byte of the file lands in
//! exactly one plane, and anything not recognised as a frame — an ID3v2 tag, an
//! ID3v1 or APE trailer, a Xing/LAME header frame, padding, or plain garbage —
//! is copied verbatim into a raw plane with its position preserved. Frame
//! lengths are derived from the header bits, which is also how the decoder gets
//! them back, so the plane split is a pure permutation of the input plus a few
//! bytes of segment table.
//!
//! WHAT THIS IS NOT, yet: the spectral data is moved, not re-coded. Undoing the
//! Huffman layer and re-coding it with a context model is where the other ~11%
//! lives (packMP3 measures ~16% total); that needs a decoder for the whole
//! main-data bit reservoir and is a separate, much larger, piece of work. This
//! layer is its prerequisite either way, because the reservoir cannot be walked
//! without exactly this frame and side-info parse.

use anyhow::{bail, ensure, Result};

/// `NM31` — this module's plane layout, which the filter id pins. It says
/// nothing about the MPEG bitstream, which is copied through unchanged.
const MAGIC: &[u8; 4] = b"NM31";
const VERSION: u8 = 1;

/// A run must chain this many frames before the scanner believes it. Two bytes
/// of sync appear constantly inside spectral data; a false positive that
/// survives three consecutive length-consistent frames does not.
///
/// A shorter chain that reaches the end of the buffer is accepted too — the
/// last frames of a file must not fall out of the transform for being last.
const MIN_CHAIN: usize = 3;

/// Below this there is no plane worth building: the segment table and the magic
/// would be most of the output.
const MIN_FRAMES: usize = 8;

/// MPEG version, from the two version bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mpeg {
    /// MPEG-2.5, the unofficial low-rate extension. Real files use it.
    V25,
    V2,
    V1,
}

/// Layer III bitrates in kbit/s. Index 0 is "free format" (the length is not
/// derivable from the header) and 15 is invalid; both are refused.
const BITRATE_V1: [u32; 16] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
];
const BITRATE_V2: [u32; 16] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
];

const RATE_V1: [u32; 3] = [44100, 48000, 32000];
const RATE_V2: [u32; 3] = [22050, 24000, 16000];
const RATE_V25: [u32; 3] = [11025, 12000, 8000];

/// What one frame header says, and the two lengths derived from it. Both
/// lengths are recomputed on decode from the same bits, so neither is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    /// Total frame size in bytes: header + optional CRC + side info + main data.
    len: usize,
    /// 9, 17 or 32 bytes, by version and channel mode.
    side_len: usize,
    /// The protection bit is INVERTED: 0 means a 16-bit CRC follows the header.
    crc: bool,
    mpeg: Mpeg,
    rate_idx: u8,
}

impl Header {
    /// Bytes of spectral data in this frame — what is left after the fixed
    /// parts. Never negative for a header that `parse` accepted.
    fn main_len(&self) -> usize {
        self.len - 4 - self.crc_len() - self.side_len
    }

    fn crc_len(&self) -> usize {
        if self.crc {
            2
        } else {
            0
        }
    }
}

/// Read a frame header, or decide these four bytes are not one.
///
/// Strict on every reserved value. A false accept costs a broken run and, at
/// worst, a refused transform; being generous here buys nothing, because a
/// stream nova cannot parse is stored the ordinary way and loses only the gain.
fn parse(b: &[u8]) -> Option<Header> {
    if b.len() < 4 || b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
        return None;
    }
    let mpeg = match (b[1] >> 3) & 0x3 {
        0 => Mpeg::V25,
        // 01 is reserved.
        1 => return None,
        2 => Mpeg::V2,
        _ => Mpeg::V1,
    };
    // Layer III only: 01. Layers I and II have a different frame geometry and
    // no side info in this sense, and nova gains nothing on them.
    if (b[1] >> 1) & 0x3 != 1 {
        return None;
    }
    let crc = b[1] & 1 == 0;

    let bitrate_idx = (b[2] >> 4) as usize;
    let rate_idx = ((b[2] >> 2) & 0x3) as usize;
    if rate_idx == 3 {
        return None;
    }
    let kbps = match mpeg {
        Mpeg::V1 => BITRATE_V1[bitrate_idx],
        _ => BITRATE_V2[bitrate_idx],
    };
    // Free format (0) carries no length in the header, and 15 is invalid.
    if kbps == 0 {
        return None;
    }
    let rate = match mpeg {
        Mpeg::V1 => RATE_V1[rate_idx],
        Mpeg::V2 => RATE_V2[rate_idx],
        Mpeg::V25 => RATE_V25[rate_idx],
    };
    let padding = (b[2] >> 1) & 1 == 1;

    let mode = (b[3] >> 6) & 0x3;
    let mono = mode == 3;
    // Emphasis 10 is reserved.
    if b[3] & 0x3 == 2 {
        return None;
    }

    // 1152 samples per frame on MPEG-1, 576 on the half-rate versions — which
    // is the whole of the 144 vs 72 below (samples / 8 bits per byte).
    let coefficient = if mpeg == Mpeg::V1 { 144 } else { 72 };
    let len = (coefficient * kbps * 1000 / rate) as usize + usize::from(padding);

    let side_len = match (mpeg, mono) {
        (Mpeg::V1, true) => 17,
        (Mpeg::V1, false) => 32,
        (_, true) => 9,
        (_, false) => 17,
    };

    // A frame has to have room for its own fixed parts. Some very low bitrates
    // at high sample rates do not, and those headers are not frames.
    if len < 4 + usize::from(crc) * 2 + side_len {
        return None;
    }
    Some(Header {
        len,
        side_len,
        crc,
        mpeg,
        rate_idx: rate_idx as u8,
    })
}

/// One frame, located in the source buffer.
#[derive(Clone, Copy, Debug)]
struct Frame {
    at: usize,
    h: Header,
}

/// A stretch of the file: either bytes nova does not claim to understand, or a
/// chain of frames it does.
enum Seg {
    Raw { at: usize, len: usize },
    Run(Vec<Frame>),
}

const KIND_RAW: u8 = 0;
const KIND_RUN: u8 = 1;

/// Do at least `MIN_CHAIN` frames start at `at`, each landing exactly where the
/// previous one ends? Reaching the end of the buffer counts as chaining: the
/// tail of a file is still a frame.
fn chains(data: &[u8], at: usize, first: &Header) -> bool {
    let mut p = at + first.len;
    let mut n = 1;
    while n < MIN_CHAIN {
        if p == data.len() {
            // Ended cleanly on a frame boundary.
            return true;
        }
        match parse(data.get(p..p + 4).unwrap_or(&[])) {
            // A run may change bitrate freely (that is what VBR is), but a
            // change of version or sample rate inside one file is a re-sync,
            // not a frame — and it would change `side_len` under the plane.
            Some(h) if h.mpeg == first.mpeg && h.rate_idx == first.rate_idx => {
                if p + h.len > data.len() {
                    return false;
                }
                p += h.len;
                n += 1;
            }
            _ => return false,
        }
    }
    true
}

/// Cut the buffer into raw stretches and frame runs. Never fails: anything not
/// understood becomes raw, which is what makes the transform total.
fn segment(data: &[u8]) -> Vec<Seg> {
    let mut segs = Vec::new();
    let mut raw_at = 0usize;
    let mut i = 0usize;

    while i + 4 <= data.len() {
        let Some(h) = parse(&data[i..i + 4]) else {
            i += 1;
            continue;
        };
        if i + h.len > data.len() || !chains(data, i, &h) {
            i += 1;
            continue;
        }
        if i > raw_at {
            segs.push(Seg::Raw {
                at: raw_at,
                len: i - raw_at,
            });
        }
        // Take the run as far as it goes. `side_len` is fixed for the run
        // because the side plane is column-major over it; a channel-mode change
        // mid-file (legal, vanishingly rare) simply ends one run and starts
        // another.
        let side_len = h.side_len;
        let mut frames = Vec::new();
        let mut p = i;
        while p + 4 <= data.len() {
            match parse(&data[p..p + 4]) {
                Some(f)
                    if f.side_len == side_len
                        && f.mpeg == h.mpeg
                        && f.rate_idx == h.rate_idx
                        && p + f.len <= data.len() =>
                {
                    frames.push(Frame { at: p, h: f });
                    p += f.len;
                }
                _ => break,
            }
        }
        segs.push(Seg::Run(frames));
        i = p;
        raw_at = p;
    }
    if raw_at < data.len() {
        segs.push(Seg::Raw {
            at: raw_at,
            len: data.len() - raw_at,
        });
    }
    segs
}

/// Does an ID3v2 tag's first frame read like one? Called only when the tag
/// claims to run past the head sample, so there are no MPEG frames to check and
/// this is the only evidence there is.
///
/// A frame is an id, a size and (from v2.3) two flag bytes whose reserved bits
/// are defined to be zero. Requiring all three together is what separates a tag
/// from data that merely starts with the right ten bytes: a run of `Z`s clears
/// the "four upper-case letters" bar on its own, and clears none of the rest.
fn first_id3_frame_looks_real(b: &[u8], tag_size: usize) -> bool {
    let id_char = |c: &u8| c.is_ascii_uppercase() || c.is_ascii_digit();
    if b[3] == 2 {
        // v2.2: three-byte id, three-byte size, no flags.
        return b.len() >= 16
            && b[10..13].iter().all(id_char)
            && matches!(
                u32::from_be_bytes([0, b[13], b[14], b[15]]) as usize,
                n if n > 0 && n <= tag_size
            );
    }
    if b.len() < 20 || !b[10..14].iter().all(id_char) {
        return false;
    }
    let raw = u32::from_be_bytes([b[14], b[15], b[16], b[17]]) as usize;
    // v2.4 sizes are syncsafe, v2.3's are plain. Read it the way the version
    // says, and require it to fit inside the tag it claims to be in.
    let size = if b[3] == 4 {
        if (b[14] | b[15] | b[16] | b[17]) & 0x80 != 0 {
            return false;
        }
        (raw & 0x7F) | ((raw >> 1) & 0x3F80) | ((raw >> 2) & 0x1F_C000) | ((raw >> 3) & 0x0FE0_0000)
    } else {
        raw
    };
    if size == 0 || size > tag_size {
        return false;
    }
    // Frame flags. v2.3 is %abc00000 %ijk00000; v2.4 is %0abc0000 %0h00kmnp.
    if b[3] == 4 {
        b[18] & 0x8F == 0 && b[19] & 0xB0 == 0
    } else {
        b[18] & 0x1F == 0 && b[19] & 0x1F == 0
    }
}

/// Is this plausibly an MPEG Layer III file? Answered from the head alone,
/// because the packer needs it before the file is cut into chunks — a
/// recompressible file gets a unit of its own.
///
/// Deliberately cheap and deliberately optimistic in one direction only: a
/// "yes" that turns out wrong costs one refused transform, while a "no" throws
/// the gain away silently.
pub fn is_mp3(b: &[u8]) -> bool {
    if b.len() < 64 {
        return false;
    }
    // An ID3v2 tag says MP3 as loudly as anything can, and its size field says
    // where to look for the first frame. Check the whole header, not just the
    // three magic bytes: "ID3" followed by noise is not a tag, and trusting it
    // would hand a size field of up to 256 MiB to the line below.
    let mut start = 0usize;
    let id3 = &b[0..3] == b"ID3"
        && matches!(b[3], 2..=4)
        && b[4] != 0xFF
        // Undefined flag bits are zero in every published version.
        && b[5] & 0x0F == 0
        // The size is syncsafe: seven bits per byte, top bit always clear.
        && (b[6] | b[7] | b[8] | b[9]) & 0x80 == 0;
    if id3 {
        let size = ((b[6] as usize) << 21)
            | ((b[7] as usize) << 14)
            | ((b[8] as usize) << 7)
            | (b[9] as usize);
        start = 10 + size;
        if start >= b.len() {
            // The tag runs past the head sample — embedded cover art does this
            // routinely, so a plain "no" here would lose the transform on the
            // files most likely to have one. Believe the tag, but only after
            // reading its FIRST FRAME, which is where a tag stops being just
            // ten plausible bytes. An extended header (flag 0x40) sits where
            // that frame would be, so it is accepted unchecked.
            return b[5] & 0x40 != 0 || first_id3_frame_looks_real(b, size);
        }
    }
    // Otherwise look for a real chain within reach of the head.
    let limit = b.len().min(start + 64 * 1024);
    let mut i = start;
    while i + 4 <= limit {
        if let Some(h) = parse(&b[i..i + 4]) {
            if i + h.len <= b.len() && chains(b, i, &h) {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn put_u32(out: &mut Vec<u8>, v: usize) {
    out.extend_from_slice(&(v as u32).to_le_bytes());
}

fn take_u32(data: &[u8], at: &mut usize) -> Result<usize> {
    ensure!(*at + 4 <= data.len(), "mp3: truncated header");
    let v = u32::from_le_bytes([data[*at], data[*at + 1], data[*at + 2], data[*at + 3]]);
    *at += 4;
    Ok(v as usize)
}

/// Split an MP3 into planes. Refuses — which is a fallback, not an error — when
/// there is not enough frame structure to pay for the segment table.
pub fn encode(data: &[u8]) -> Result<Vec<u8>> {
    let segs = segment(data);
    let frames: usize = segs
        .iter()
        .map(|s| match s {
            Seg::Run(f) => f.len(),
            Seg::Raw { .. } => 0,
        })
        .collect::<Vec<_>>()
        .iter()
        .sum();
    if frames < MIN_FRAMES {
        bail!("mp3: only {frames} frames found, not worth a transform");
    }

    let mut out = Vec::with_capacity(data.len() + 64 + segs.len() * 5);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    put_u32(&mut out, segs.len());
    for s in &segs {
        match s {
            Seg::Raw { len, .. } => {
                out.push(KIND_RAW);
                put_u32(&mut out, *len);
            }
            Seg::Run(f) => {
                out.push(KIND_RUN);
                put_u32(&mut out, f.len());
            }
        }
    }

    // Plane 1: everything not claimed as a frame, in file order.
    for s in &segs {
        if let Seg::Raw { at, len } = s {
            out.extend_from_slice(&data[*at..*at + *len]);
        }
    }
    // Plane 2: frame headers, COLUMN-MAJOR. Byte 0 is 0xFF for every frame in
    // the file and byte 1 changes only across a re-sync, so two of the four
    // columns collapse to a run; byte 2 carries the bitrate and the padding
    // bit, which is where a VBR file's only real variation lives.
    for s in &segs {
        if let Seg::Run(f) = s {
            for col in 0..4 {
                for fr in f {
                    out.push(data[fr.at + col]);
                }
            }
        }
    }
    // Plane 3: the CRCs, for the minority of files that carry them.
    for s in &segs {
        if let Seg::Run(f) = s {
            for fr in f {
                if fr.h.crc {
                    out.extend_from_slice(&data[fr.at + 4..fr.at + 6]);
                }
            }
        }
    }
    // Plane 4: side info, column-major over the run. The fields are not byte
    // aligned, but they are laid out in a fixed order, so column j holds the
    // same bit positions of every frame — `main_data_begin`'s high bits in
    // column 0, and so on down. Each column is then a slowly-drifting series
    // rather than one byte every 400.
    for s in &segs {
        if let Seg::Run(f) = s {
            let side_len = f.first().map_or(0, |fr| fr.h.side_len);
            for col in 0..side_len {
                for fr in f {
                    out.push(data[fr.at + 4 + fr.h.crc_len() + col]);
                }
            }
        }
    }
    // Plane 5: the spectral data, verbatim and in order. Entropy-coded already;
    // moving it here is what lets the four planes above be contiguous.
    for s in &segs {
        if let Seg::Run(f) = s {
            for fr in f {
                let start = fr.at + 4 + fr.h.crc_len() + fr.h.side_len;
                out.extend_from_slice(&data[start..start + fr.h.main_len()]);
            }
        }
    }
    Ok(out)
}

/// Rebuild the original file from its planes.
///
/// Every length here is derived from the frame headers, exactly as the encoder
/// derived them, so a manifest cannot drive an allocation and a corrupt payload
/// fails a bounds check rather than producing plausible garbage.
pub fn decode(data: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        data.len() >= 9 && &data[0..4] == MAGIC,
        "mp3: not an NM31 payload"
    );
    ensure!(data[4] == VERSION, "mp3: unknown NM31 version {}", data[4]);

    let mut at = 5usize;
    let n_seg = take_u32(data, &mut at)?;
    // A segment costs 5 bytes here and at least one byte of payload, so a count
    // larger than the buffer is a forgery, not a big file.
    ensure!(n_seg <= data.len(), "mp3: implausible segment count");

    let mut table: Vec<(u8, usize)> = Vec::with_capacity(n_seg);
    for _ in 0..n_seg {
        ensure!(at < data.len(), "mp3: truncated segment table");
        let kind = data[at];
        at += 1;
        let n = take_u32(data, &mut at)?;
        ensure!(
            kind == KIND_RAW || kind == KIND_RUN,
            "mp3: bad segment kind"
        );
        table.push((kind, n));
    }

    let raw_total: usize = table
        .iter()
        .filter(|(k, _)| *k == KIND_RAW)
        .map(|(_, n)| *n)
        .sum();
    let frame_total: usize = table
        .iter()
        .filter(|(k, _)| *k == KIND_RUN)
        .map(|(_, n)| *n)
        .sum();

    // Plane bases, in the order `encode` wrote them. Only the first two can be
    // sized before the headers are read; the rest come from what they say.
    let raw_base = at;
    let hdr_base = raw_base
        .checked_add(raw_total)
        .filter(|v| *v <= data.len())
        .ok_or_else(|| anyhow::anyhow!("mp3: raw plane overruns the payload"))?;
    let hdr_len = frame_total
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!("mp3: frame count overflows"))?;
    ensure!(
        hdr_base + hdr_len <= data.len(),
        "mp3: header plane overruns the payload"
    );

    // Read every header first: they carry all the remaining lengths.
    let mut headers: Vec<Vec<Header>> = Vec::new();
    let mut hp = hdr_base;
    let mut crc_total = 0usize;
    let mut side_total = 0usize;
    let mut main_total = 0usize;
    for (kind, n) in &table {
        if *kind != KIND_RUN {
            continue;
        }
        let n = *n;
        let mut run = Vec::with_capacity(n);
        let mut side_len = None;
        for i in 0..n {
            // Column-major: byte `col` of frame `i` sits at `hp + col * n + i`.
            let b = [
                data[hp + i],
                data[hp + n + i],
                data[hp + 2 * n + i],
                data[hp + 3 * n + i],
            ];
            let h = parse(&b).ok_or_else(|| anyhow::anyhow!("mp3: corrupt frame header"))?;
            match side_len {
                None => side_len = Some(h.side_len),
                Some(s) => ensure!(s == h.side_len, "mp3: side length changed inside a run"),
            }
            crc_total += h.crc_len();
            main_total = main_total
                .checked_add(h.main_len())
                .ok_or_else(|| anyhow::anyhow!("mp3: frame lengths overflow"))?;
            run.push(h);
        }
        side_total += n * side_len.unwrap_or(0);
        hp += 4 * n;
        headers.push(run);
    }

    let crc_base = hdr_base + hdr_len;
    let side_base = crc_base + crc_total;
    let main_base = side_base + side_total;
    let end = main_base
        .checked_add(main_total)
        .ok_or_else(|| anyhow::anyhow!("mp3: planes overflow"))?;
    ensure!(end <= data.len(), "mp3: planes overrun the payload");

    let mut out =
        Vec::with_capacity(raw_total + frame_total * 4 + crc_total + side_total + main_total);
    let mut raw_p = raw_base;
    let mut crc_p = crc_base;
    let mut side_p = side_base;
    let mut main_p = main_base;
    let mut hp = hdr_base;
    let mut run_i = 0usize;

    for (kind, n) in &table {
        if *kind == KIND_RAW {
            out.extend_from_slice(&data[raw_p..raw_p + *n]);
            raw_p += *n;
            continue;
        }
        let n = *n;
        let run = &headers[run_i];
        run_i += 1;
        let side_len = run.first().map_or(0, |h| h.side_len);
        for (i, h) in run.iter().enumerate() {
            for col in 0..4 {
                out.push(data[hp + col * n + i]);
            }
            if h.crc {
                out.extend_from_slice(&data[crc_p..crc_p + 2]);
                crc_p += 2;
            }
            for col in 0..side_len {
                out.push(data[side_p + col * n + i]);
            }
            out.extend_from_slice(&data[main_p..main_p + h.main_len()]);
            main_p += h.main_len();
        }
        hp += 4 * n;
        side_p += n * side_len;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a frame the scanner must accept: MPEG-1 Layer III, joint stereo,
    /// no CRC, 44.1 kHz. `payload` seeds the side info and spectral bytes so a
    /// round trip has something to get wrong.
    fn frame(bitrate_idx: u8, padding: bool, seed: u8) -> Vec<u8> {
        let mut b = vec![
            0xFF,
            0xFB, // MPEG-1, Layer III, no CRC
            // Sample rate index 0 (44.1 kHz) occupies bits 3..2 and is left at 0.
            (bitrate_idx << 4) | (u8::from(padding) << 1),
            0x40, // joint stereo, emphasis none
        ];
        let h = parse(&b).expect("test frame header must parse");
        assert_eq!(h.side_len, 32);
        for i in 0..h.len - 4 {
            b.push(seed.wrapping_add(i as u8).wrapping_mul(31));
        }
        assert_eq!(b.len(), h.len);
        b
    }

    fn cbr(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..n {
            v.extend_from_slice(&frame(9, false, i as u8));
        }
        v
    }

    fn round_trip(data: &[u8]) -> Vec<u8> {
        let enc = encode(data).expect("encode");
        let dec = decode(&enc).expect("decode");
        assert_eq!(dec, data, "mp3 filter did not round-trip");
        enc
    }

    #[test]
    fn header_lengths_match_the_spec() {
        // 128 kbit/s at 44.1 kHz is the canonical 417-byte frame, 418 padded.
        let h = parse(&frame(9, false, 0)[..4]).unwrap();
        assert_eq!(h.len, 417);
        let h = parse(&frame(9, true, 0)[..4]).unwrap();
        assert_eq!(h.len, 418);
        // 320 kbit/s.
        let h = parse(&frame(14, false, 0)[..4]).unwrap();
        assert_eq!(h.len, 1044);
    }

    #[test]
    fn rejects_reserved_and_free_format() {
        // Free format: no length in the header.
        assert!(parse(&[0xFF, 0xFB, 0x00, 0x40]).is_none());
        // Bitrate index 15.
        assert!(parse(&[0xFF, 0xFB, 0xF0, 0x40]).is_none());
        // Sample rate index 3.
        assert!(parse(&[0xFF, 0xFB, 0x9C, 0x40]).is_none());
        // Reserved MPEG version (byte 1 = 111 01 01 1).
        assert!(parse(&[0xFF, 0xEB, 0x90, 0x40]).is_none());
        // Layer II, not III.
        assert!(parse(&[0xFF, 0xFD, 0x90, 0x40]).is_none());
        // Reserved emphasis (10).
        assert!(parse(&[0xFF, 0xFB, 0x90, 0x42]).is_none());
        // No sync.
        assert!(parse(&[0xFE, 0xFB, 0x90, 0x40]).is_none());
    }

    #[test]
    fn round_trips_plain_cbr() {
        let data = cbr(64);
        let enc = round_trip(&data);
        // A permutation plus a segment table: never materially larger.
        assert!(
            enc.len() <= data.len() + 64,
            "{} vs {}",
            enc.len(),
            data.len()
        );
    }

    #[test]
    fn round_trips_vbr_with_padding() {
        let mut data = Vec::new();
        for i in 0..48 {
            // Walk the bitrate and toggle padding: the two things a real VBR
            // encoder changes frame to frame.
            data.extend_from_slice(&frame(5 + (i % 8) as u8, i % 3 == 0, i as u8));
        }
        round_trip(&data);
    }

    #[test]
    fn round_trips_with_id3_and_trailer() {
        let mut data = b"ID3\x04\x00\x00\x00\x00\x02\x01".to_vec();
        data.extend_from_slice(&[0u8; 257]);
        data.extend_from_slice(&cbr(32));
        // ID3v1 is 128 bytes and starts with TAG.
        data.extend_from_slice(b"TAG");
        data.extend_from_slice(&[0x20u8; 125]);
        round_trip(&data);
        assert!(is_mp3(&data));
    }

    #[test]
    fn round_trips_with_crc_frames() {
        let mut data = Vec::new();
        for i in 0..24u8 {
            let mut b = vec![0xFF, 0xFA, 0x90, 0x40];
            let h = parse(&b).unwrap();
            assert!(h.crc);
            for j in 0..h.len - 4 {
                b.push(i.wrapping_mul(7).wrapping_add(j as u8));
            }
            data.extend_from_slice(&b);
        }
        round_trip(&data);
    }

    #[test]
    fn round_trips_mono_and_mpeg2() {
        // MPEG-2, Layer III, mono: side info is 9 bytes, frames are short.
        let mut data = Vec::new();
        for i in 0..32u8 {
            let mut b = vec![0xFF, 0xF3, 0x80, 0xC0];
            let h = parse(&b).unwrap();
            assert_eq!(h.side_len, 9);
            for j in 0..h.len - 4 {
                b.push(i.wrapping_add(j as u8));
            }
            data.extend_from_slice(&b);
        }
        round_trip(&data);
    }

    /// The case the whole design turns on: a file that is mostly garbage still
    /// comes back byte for byte, because everything unrecognised is copied.
    #[test]
    fn round_trips_garbage_between_runs() {
        let mut data = vec![0xFFu8; 40];
        data.extend_from_slice(&cbr(16));
        data.extend_from_slice(&[0xFF, 0xFB, 0x90]); // a truncated tease
        data.extend_from_slice(b"not a frame at all, just bytes");
        data.extend_from_slice(&cbr(16));
        data.push(0xFF);
        round_trip(&data);
    }

    #[test]
    fn refuses_data_that_is_not_mp3() {
        assert!(encode(b"just some text, nothing like a frame here at all").is_err());
        assert!(!is_mp3(&[0u8; 4096]));
        // A lone false sync must not start a run.
        let mut noise = vec![0u8; 4096];
        noise[100] = 0xFF;
        noise[101] = 0xFB;
        assert!(!is_mp3(&noise));
        assert!(encode(&noise).is_err());
    }

    #[test]
    fn decode_rejects_corrupt_payloads() {
        let enc = encode(&cbr(32)).unwrap();
        assert!(decode(&enc[..enc.len() / 2]).is_err());
        assert!(decode(b"NM31").is_err());
        assert!(decode(&[]).is_err());
        let mut bad = enc.clone();
        bad[4] = 99;
        assert!(decode(&bad).is_err());
        // A forged segment count must not drive an allocation.
        let mut bad = enc.clone();
        bad[5..9].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&bad).is_err());
    }

    /// The point of the transform, on data shaped like a real file: headers and
    /// side info are structured, spectral data is not. Separating them has to
    /// make the structured part compressible.
    #[test]
    fn planes_beat_the_interleaved_original() {
        let mut data = Vec::new();
        let mut state = 0x1234_5678u32;
        for i in 0..400 {
            let mut b = vec![0xFF, 0xFB, (9 << 4) | u8::from(i % 7 == 0) << 1, 0x40];
            let h = parse(&b).unwrap();
            // Side info: slowly drifting numbers, which is what it really is.
            for c in 0..h.side_len {
                b.push(((i / 4) as u8).wrapping_add(c as u8));
            }
            // Spectral data: noise, which is what it really is.
            for _ in 0..h.main_len() {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                b.push((state >> 24) as u8);
            }
            data.extend_from_slice(&b);
        }
        let enc = encode(&data).unwrap();
        assert_eq!(decode(&enc).unwrap(), data);

        let plain = crate::codec::compress(crate::codec::Codec::Lzma2, 12, 0, &data)
            .unwrap()
            .len();
        let split = crate::codec::compress(crate::codec::Codec::Lzma2, 12, 0, &enc)
            .unwrap()
            .len();
        assert!(
            split < plain,
            "plane split did not help: {split} vs {plain}"
        );
    }
}

/// What the split is actually worth, per file, on a real corpus.
///
/// Isolates the filter from the codec: the same bytes go to the same codec
/// twice, once interleaved and once in planes, so the difference is the
/// transform and nothing else. Also reports where the bytes are, which is what
/// decides whether the next move is a better side-info layout or the Huffman
/// layer underneath it.
///
/// Ignored because it needs a corpus:
/// `NOVA_MP3_CORPUS=<abs path> cargo test -p nova-core --release mp3::corpus -- --ignored --nocapture`
/// (absolute: cargo runs the test with the CRATE directory as its cwd)
#[cfg(test)]
#[test]
#[ignore]
fn corpus() {
    use crate::codec::{compress, Codec};
    let Ok(dir) = std::env::var("NOVA_MP3_CORPUS") else {
        eprintln!("set NOVA_MP3_CORPUS to a directory of .mp3 files");
        return;
    };
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("corpus directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp3")))
        .collect();
    files.sort();

    println!(
        "{:<26} {:>11} {:>7} {:>7} {:>11} {:>11} {:>7}",
        "file", "bytes", "hdr%", "side%", "bsc raw", "bsc split", "gain"
    );
    let (mut t_raw, mut t_plain, mut t_split, mut t_struct) = (0u64, 0u64, 0u64, 0u64);
    for p in &files {
        let data = std::fs::read(p).unwrap();
        let enc = match encode(&data) {
            Ok(v) => v,
            Err(e) => {
                println!("{:<26} refused: {e}", p.file_name().unwrap().to_string_lossy());
                continue;
            }
        };
        assert_eq!(decode(&enc).unwrap(), data, "{p:?} did not round-trip");

        // Plane sizes, straight from the segmentation.
        let segs = segment(&data);
        let (mut hdr, mut side, mut main, mut raw) = (0usize, 0usize, 0usize, 0usize);
        for s in &segs {
            match s {
                Seg::Raw { len, .. } => raw += len,
                Seg::Run(f) => {
                    for fr in f {
                        hdr += 4 + fr.h.crc_len();
                        side += fr.h.side_len;
                        main += fr.h.main_len();
                    }
                }
            }
        }
        // Each plane on its own, so it is visible WHICH one still holds bytes.
        // The side-info plane is the one to watch: its fields are bit-packed,
        // so a byte column mixes two of them, and if it is still fat there is a
        // better layout to be had before anything as large as the Huffman layer.
        let mut hp: Vec<u8> = Vec::new();
        let mut sp: Vec<u8> = Vec::new();
        let mut mp: Vec<u8> = Vec::new();
        for s in &segs {
            if let Seg::Run(f) = s {
                let sl = f.first().map_or(0, |fr| fr.h.side_len);
                for col in 0..4 {
                    for fr in f {
                        hp.push(data[fr.at + col]);
                    }
                }
                for col in 0..sl {
                    for fr in f {
                        sp.push(data[fr.at + 4 + fr.h.crc_len() + col]);
                    }
                }
                for fr in f {
                    let st = fr.at + 4 + fr.h.crc_len() + fr.h.side_len;
                    mp.extend_from_slice(&data[st..st + fr.h.main_len()]);
                }
            }
        }
        let c = |v: &[u8]| {
            if v.is_empty() {
                0
            } else {
                compress(Codec::Bsc, 19, 0, v).unwrap().len()
            }
        };
        let (chp, csp, cmp_) = (c(&hp), c(&sp), c(&mp));
        println!(
            "    planes: hdr {} -> {chp} ({:.1}%) · side {} -> {csp} ({:.1}%) · spectral {} -> {cmp_} ({:.2}%)",
            hp.len(), chp as f64 * 100.0 / hp.len().max(1) as f64,
            sp.len(), csp as f64 * 100.0 / sp.len().max(1) as f64,
            mp.len(), cmp_ as f64 * 100.0 / mp.len().max(1) as f64,
        );

        let plain = compress(Codec::Bsc, 19, 0, &data).unwrap().len();
        let split = compress(Codec::Bsc, 19, 0, &enc).unwrap().len();
        let n = data.len();
        println!(
            "{:<26} {n:>11} {:>6.2}% {:>6.2}% {plain:>11} {split:>11} {:>6.2}%",
            p.file_name().unwrap().to_string_lossy().chars().take(26).collect::<String>(),
            hdr as f64 * 100.0 / n as f64,
            side as f64 * 100.0 / n as f64,
            (plain as f64 - split as f64) * 100.0 / plain as f64,
        );
        let _ = (main, raw);
        t_raw += n as u64;
        t_plain += plain as u64;
        t_split += split as u64;
        t_struct += (hdr + side + raw) as u64;
    }
    println!(
        "\ntotal {t_raw} B · structure {t_struct} ({:.2}%) · bsc raw {t_plain} ({:.2}%) \
         · bsc split {t_split} ({:.2}%) · the split is worth {:.2}%",
        t_struct as f64 * 100.0 / t_raw as f64,
        t_plain as f64 * 100.0 / t_raw as f64,
        t_split as f64 * 100.0 / t_raw as f64,
        (t_plain as f64 - t_split as f64) * 100.0 / t_plain as f64,
    );
}
