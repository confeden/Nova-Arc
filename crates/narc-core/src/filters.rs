//! Reversible pre-compression filters: transforms that never shrink data by
//! themselves, but make the codec that follows shrink it much further.
//!
//! - **BCJ x86** rewrites the operand of relative CALL/JMP (`E8`/`E9`) into an
//!   absolute address. Ten calls to the same function are ten *different* byte
//!   strings while the operands are relative, and ten identical ones once they
//!   are absolute — exactly the repetition an LZ matcher lives on. Measured on
//!   whole binaries: 4.6% off zstd-12 and 4.8% off LZMA for this project's own
//!   `narc.exe`, 5.3-5.7% for 4 MiB of `mshtml.dll`; more on code-dense files,
//!   nothing at all on the data sections mixed in with them.
//! - **Delta** replaces each byte with its difference from the byte `distance`
//!   back, turning smooth or column-aligned data (PCM audio, uncompressed
//!   bitmaps, tables of fixed-width records) into runs of near-zero bytes.
//!
//! **Chunk-local by design.** Every chunk is filtered on its own, with the
//! position counter starting at 0, and the chunk record stores only the filter
//! id (see [`Filter::id`]). Feeding BCJ the chunk's real offset in the file
//! would be marginally better for compression and much worse for everything
//! else: the filtered bytes would depend on *where* the chunk landed, so two
//! identical chunks would stop deduplicating, and the decoder would need the
//! offset stored per chunk to undo the transform. `start_offset` survives on
//! the free functions for interop and testing only.
//!
//! **Chunk boundaries.** The BCJ scan stops 4 bytes before the end of the
//! buffer (a 5-byte instruction must fit whole) and carries no state between
//! calls, so an instruction straddling a boundary is simply left alone — by
//! encoder and decoder alike, which is what makes the round-trip exact. The
//! cost is at most one missed conversion per boundary, i.e. one call per 4 MiB
//! chunk; the alternative (stateful streaming across chunks) would make a
//! chunk undecodable without its predecessor.

use anyhow::{bail, Result};

/// Largest delta distance the manifest byte can encode.
pub const MAX_DELTA_DISTANCE: u8 = 32;

/// Highest assigned filter id.
const MAX_FILTER_ID: u8 = 1 + MAX_DELTA_DISTANCE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// Feed the chunk to the codec unchanged.
    None,
    /// x86 call/jump converter.
    BcjX86,
    /// Byte-difference filter; the payload is the distance in bytes, 1..=32.
    Delta(u8),
}

impl Filter {
    /// Checked constructor — the only way to build a `Delta` that is
    /// guaranteed to survive a manifest round-trip.
    pub fn delta(distance: u8) -> Result<Filter> {
        if distance == 0 || distance > MAX_DELTA_DISTANCE {
            bail!("delta distance {distance} out of range 1..={MAX_DELTA_DISTANCE}");
        }
        Ok(Filter::Delta(distance))
    }

    /// The byte stored in the chunk record:
    ///
    /// | id       | filter                          |
    /// |----------|---------------------------------|
    /// | 0        | none                            |
    /// | 1        | BCJ x86                         |
    /// | 2..=33   | delta with distance `id - 1`    |
    ///
    /// Ids 34..=255 are unassigned; [`Filter::from_id`] rejects them, so an
    /// archive written by a newer version fails loudly instead of unpacking
    /// garbage.
    ///
    /// A `Delta` distance outside 1..=32 is not representable, so it is
    /// clamped — by `id`, `apply` and `unapply` alike, so the stored byte can
    /// never disagree with the transform that was actually applied.
    pub fn id(self) -> u8 {
        match self {
            Filter::None => 0,
            Filter::BcjX86 => 1,
            Filter::Delta(d) => 1 + clamp_distance(d as usize) as u8,
        }
    }

    pub fn from_id(id: u8) -> Result<Filter> {
        match id {
            0 => Ok(Filter::None),
            1 => Ok(Filter::BcjX86),
            2..=MAX_FILTER_ID => Ok(Filter::Delta(id - 1)),
            other => bail!("unknown filter id {other} - archive was made by a newer version"),
        }
    }

    /// Transform a chunk in place, before compression.
    pub fn apply(self, data: &mut [u8]) {
        match self {
            Filter::None => {}
            Filter::BcjX86 => bcj_x86_encode(data, 0),
            Filter::Delta(d) => delta_encode(data, d as usize),
        }
    }

    /// Undo [`Filter::apply`] in place, after decompression.
    pub fn unapply(self, data: &mut [u8]) {
        match self {
            Filter::None => {}
            Filter::BcjX86 => bcj_x86_decode(data, 0),
            Filter::Delta(d) => delta_decode(data, d as usize),
        }
    }
}

/// Every entry point clamps the distance the same way, so an out-of-range
/// distance can never make the id disagree with the transform applied.
fn clamp_distance(distance: usize) -> usize {
    distance.clamp(1, MAX_DELTA_DISTANCE as usize)
}

/// Replace every byte with its difference from the byte `distance` back
/// (1..=32, clamped). Bytes before the start of the buffer count as zero, so
/// the first `distance` bytes pass through unchanged.
pub fn delta_encode(data: &mut [u8], distance: usize) {
    let distance = clamp_distance(distance);
    // Backwards: each byte must be subtracted from the *original* predecessor,
    // which is still intact only ahead of the cursor.
    for i in (distance..data.len()).rev() {
        data[i] = data[i].wrapping_sub(data[i - distance]);
    }
}

/// Inverse of [`delta_encode`] for the same distance.
pub fn delta_decode(data: &mut [u8], distance: usize) {
    let distance = clamp_distance(distance);
    // Forwards: the predecessor has already been restored.
    for i in distance..data.len() {
        data[i] = data[i].wrapping_add(data[i - distance]);
    }
}

/// Convert relative CALL/JMP targets to absolute.
pub fn bcj_x86_encode(data: &mut [u8], start_offset: u32) {
    bcj_x86(data, start_offset, true);
}

/// Inverse of [`bcj_x86_encode`] for the same `start_offset`.
pub fn bcj_x86_decode(data: &mut [u8], start_offset: u32) {
    bcj_x86(data, start_offset, false);
}

/// The classic 7-Zip/xz x86 BCJ filter, ported from the reference
/// implementation (`liblzma`'s `x86.c` / XZ-for-Java's `X86.java`); the byte
/// stream it produces is identical to theirs, which is the only sane
/// definition of "correct" for a filter this quirky.
///
/// Reversibility rests on two facts: the scan's decisions depend only on the
/// opcode byte and on whether the operand's most significant byte is
/// `00`/`FF`, and a converted operand always keeps a `00`/`FF` most
/// significant byte. So the decoder walks exactly the same positions and
/// reaches exactly the same state as the encoder did, without being told
/// anything.
fn bcj_x86(data: &mut [u8], start_offset: u32, encode: bool) {
    /// Which `prev_mask` states are still eligible for conversion, and which
    /// operand byte a state points at. Empirical tables from the reference
    /// implementation: they suppress conversions that a nearby earlier E8/E9
    /// makes ambiguous.
    const MASK_TO_ALLOWED_STATUS: [bool; 8] = [true, true, true, false, true, false, false, false];
    const MASK_TO_BIT_NUMBER: [u32; 8] = [0, 1, 2, 2, 3, 3, 3, 3];

    /// A converted operand's most significant byte is written back as `00` or
    /// `FF`, so this test answers the same before and after conversion.
    fn is_ms_byte(b: u8) -> bool {
        b == 0x00 || b == 0xFF
    }

    if data.len() < 5 {
        return;
    }
    // Address of the byte after the instruction at index 0 — x86 relative
    // targets are measured from the end of the instruction.
    let next_ip_base = start_offset.wrapping_add(5);
    let end = data.len() - 5;
    let mut prev_mask: u32 = 0;
    // Position of the previous E8/E9 candidate. Starts "before the buffer" so
    // the first candidate is judged on its own.
    let mut prev_pos: i64 = -1;
    let mut i = 0usize;

    while i <= end {
        if data[i] != 0xE8 && data[i] != 0xE9 {
            i += 1;
            continue;
        }
        let gap = i as i64 - prev_pos;
        prev_pos = i as i64;
        if gap > 3 {
            // Far enough from the previous candidate that its operand bytes
            // cannot overlap this instruction.
            prev_mask = 0;
        } else {
            prev_mask = (prev_mask << (gap - 1)) & 0x7;
            if prev_mask != 0 {
                let back = MASK_TO_BIT_NUMBER[prev_mask as usize] as usize;
                if !MASK_TO_ALLOWED_STATUS[prev_mask as usize] || is_ms_byte(data[i + 4 - back]) {
                    prev_mask = ((prev_mask << 1) & 0x7) | 1;
                    i += 1;
                    continue;
                }
            }
        }
        if !is_ms_byte(data[i + 4]) {
            prev_mask = ((prev_mask << 1) & 0x7) | 1;
            i += 1;
            continue;
        }

        let next_ip = next_ip_base.wrapping_add(i as u32);
        let mut src = u32::from_le_bytes([data[i + 1], data[i + 2], data[i + 3], data[i + 4]]);
        let dest = loop {
            let dest = if encode {
                src.wrapping_add(next_ip)
            } else {
                src.wrapping_sub(next_ip)
            };
            if prev_mask == 0 {
                break dest;
            }
            // An overlapping earlier candidate could claim this byte; flip the
            // low bits and retry so encoder and decoder agree on who owns it.
            let bit = MASK_TO_BIT_NUMBER[prev_mask as usize] * 8;
            if !is_ms_byte((dest >> (24 - bit)) as u8) {
                break dest;
            }
            src = dest ^ ((1u32 << (32 - bit)) - 1);
        };
        data[i + 1] = dest as u8;
        data[i + 2] = (dest >> 8) as u8;
        data[i + 3] = (dest >> 16) as u8;
        // Only bit 24 of the absolute address is kept, sign-extended over the
        // whole byte. That is what keeps the operand's top byte in {00, FF}
        // for the reverse scan, and it is exactly recoverable because the
        // decoder collapses the byte the same way.
        data[i + 4] = if (dest >> 24) & 1 != 0 { 0xFF } else { 0x00 };
        i += 5;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const MAX_CHUNK: usize = crate::archive::MAX_CHUNK as usize;

    /// Every filter the manifest byte can name.
    fn all_filters() -> Vec<Filter> {
        let mut v = vec![Filter::None, Filter::BcjX86];
        v.extend((1..=MAX_DELTA_DISTANCE).map(Filter::Delta));
        v
    }

    /// SplitMix64 — a dependency-free, deterministic source of test data.
    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn byte(&mut self) -> u8 {
            self.next_u64() as u8
        }
    }

    fn random_buf(seed: u64, len: usize) -> Vec<u8> {
        let mut rng = Rng(seed);
        (0..len).map(|_| rng.byte()).collect()
    }

    /// Machine-code-shaped data: a high density of E8/E9 opcodes and of
    /// 00/FF operand bytes, which is what drives the BCJ state machine into
    /// its corners (adjacent candidates, overlapping operands, retry loop).
    fn codeish_buf(seed: u64, len: usize) -> Vec<u8> {
        let mut rng = Rng(seed);
        (0..len)
            .map(|_| match rng.next_u64() % 8 {
                0 => 0xE8,
                1 => 0xE9,
                2 | 3 => 0x00,
                4 => 0xFF,
                _ => rng.byte(),
            })
            .collect()
    }

    /// The degenerate lengths and everything around the 5-byte instruction
    /// window, where off-by-one errors live.
    fn small_lengths() -> Vec<usize> {
        vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 31, 32, 33, 64, 255, 4096]
    }

    /// Sizes around the 4 MiB maximum chunk. A megabyte of random data costs
    /// real time in a debug build, so these are used sparingly.
    fn chunk_lengths() -> Vec<usize> {
        vec![
            MAX_CHUNK - 5,
            MAX_CHUNK - 1,
            MAX_CHUNK,
            MAX_CHUNK + 1,
            MAX_CHUNK + 7,
        ]
    }

    fn interesting_lengths() -> Vec<usize> {
        let mut lens = small_lengths();
        lens.extend(chunk_lengths());
        lens
    }

    /// `target/release/narc.exe` (or the debug build), the real executable
    /// this crate is compiled into.
    fn narc_exe() -> Option<Vec<u8>> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for name in [
            "release/narc.exe",
            "debug/narc.exe",
            "release/narc",
            "debug/narc",
        ] {
            if let Ok(bytes) = std::fs::read(root.join("target").join(name)) {
                if bytes.len() > 64 * 1024 {
                    return Some(bytes);
                }
            }
        }
        None
    }

    /// One chunk's worth of a real executable, which is what the filter would
    /// actually be handed.
    fn exe_sample() -> Option<Vec<u8>> {
        narc_exe().map(|b| b[..b.len().min(MAX_CHUNK)].to_vec())
    }

    #[test]
    fn filter_ids_are_stable() {
        assert_eq!(Filter::None.id(), 0);
        assert_eq!(Filter::BcjX86.id(), 1);
        assert_eq!(Filter::Delta(1).id(), 2);
        assert_eq!(Filter::Delta(32).id(), 33);
        for f in all_filters() {
            assert_eq!(Filter::from_id(f.id()).unwrap(), f, "{f:?}");
        }
        for id in 0..=33u8 {
            assert_eq!(Filter::from_id(id).unwrap().id(), id);
        }
        for id in 34..=255u8 {
            assert!(Filter::from_id(id).is_err(), "id {id} must be rejected");
        }
    }

    #[test]
    fn delta_distance_is_validated() {
        assert!(Filter::delta(0).is_err());
        assert!(Filter::delta(33).is_err());
        assert_eq!(Filter::delta(1).unwrap(), Filter::Delta(1));
        assert_eq!(Filter::delta(32).unwrap(), Filter::Delta(32));
        // A distance that cannot be stored must not be silently applied
        // either: id and apply clamp the same way, so the pair still
        // round-trips.
        let mut a = random_buf(7, 300);
        let mut b = a.clone();
        Filter::Delta(0).apply(&mut a);
        Filter::Delta(1).apply(&mut b);
        assert_eq!(a, b);
        assert_eq!(Filter::Delta(0).id(), Filter::Delta(1).id());
    }

    /// The property the whole module rests on: for arbitrary data and any
    /// filter, unapply(apply(x)) == x.
    #[test]
    fn every_filter_round_trips_arbitrary_data() {
        for filter in all_filters() {
            for len in [0, 1, 2, 3, 4, 5, 6, 7, 15, 16, 17, 33, 100, 1000, 65536] {
                for seed in 0..8u64 {
                    for original in [random_buf(seed, len), codeish_buf(seed ^ 0x5A5A, len)] {
                        let mut data = original.clone();
                        filter.apply(&mut data);
                        filter.unapply(&mut data);
                        assert_eq!(data, original, "{filter:?} on {len} bytes, seed {seed}");
                    }
                }
            }
        }
    }

    #[test]
    fn bcj_round_trips_every_length() {
        for len in interesting_lengths() {
            for original in [random_buf(len as u64, len), codeish_buf(len as u64, len)] {
                for offset in [0u32, 5, 4096, u32::MAX - 7] {
                    let mut data = original.clone();
                    bcj_x86_encode(&mut data, offset);
                    bcj_x86_decode(&mut data, offset);
                    assert_eq!(data, original, "{len} bytes at offset {offset}");
                }
            }
        }
    }

    #[test]
    fn delta_round_trips_every_distance() {
        for distance in 1..=MAX_DELTA_DISTANCE as usize {
            // Delta has no size-dependent behaviour beyond "distance vs
            // length", so only a few distances pay for the 4 MiB cases.
            let mut lens = small_lengths();
            if matches!(distance, 1 | 3 | 32) {
                lens.extend(chunk_lengths());
            }
            for len in lens {
                let original = random_buf((distance * 31 + len) as u64, len);
                let mut data = original.clone();
                delta_encode(&mut data, distance);
                assert_eq!(
                    data[..distance.min(len)],
                    original[..distance.min(len)],
                    "the first {distance} bytes have no predecessor"
                );
                delta_decode(&mut data, distance);
                assert_eq!(data, original, "distance {distance}, {len} bytes");
            }
        }
    }

    /// Chunks are filtered independently, so splitting a buffer anywhere and
    /// filtering the pieces must still round-trip — including when a CALL
    /// instruction straddles the cut.
    #[test]
    fn bcj_round_trips_across_chunk_boundaries() {
        let original = codeish_buf(0xC0FFEE, 40_000);
        for cut in [1, 2, 3, 4, 5, 6, 7, 4095, 4096, 4097, 39_997, 39_999] {
            let mut head = original[..cut].to_vec();
            let mut tail = original[cut..].to_vec();
            bcj_x86_encode(&mut head, 0);
            bcj_x86_encode(&mut tail, 0);
            bcj_x86_decode(&mut head, 0);
            bcj_x86_decode(&mut tail, 0);
            head.extend_from_slice(&tail);
            assert_eq!(head, original, "cut at {cut}");
        }
    }

    /// The last four bytes can never hold a complete instruction, so the scan
    /// must leave them exactly as they are.
    #[test]
    fn bcj_leaves_a_truncated_instruction_alone() {
        let mut data = vec![0x90; 16];
        data.extend_from_slice(&[0xE8, 0x11, 0x22, 0x33]);
        let original = data.clone();
        bcj_x86_encode(&mut data, 0);
        assert_eq!(data[16..], original[16..]);
    }

    /// The port must agree with the reference implementation byte for byte,
    /// otherwise "BCJ x86" would mean something private to this archiver.
    #[test]
    fn bcj_matches_reference_implementation() {
        use lzma_rust2::filter::bcj::{BcjReader, BcjWriter};

        let mut inputs = vec![codeish_buf(1, 5000), random_buf(2, 5000)];
        if let Some(exe) = exe_sample() {
            inputs.push(exe[..exe.len().min(1 << 20)].to_vec());
        }
        for input in inputs {
            let mut mine = input.clone();
            bcj_x86_encode(&mut mine, 0);

            let mut writer = BcjWriter::new_x86(Vec::new(), 0);
            writer.write_all(&input).unwrap();
            let reference = writer.finish().unwrap();
            assert_eq!(mine, reference, "encoder differs from lzma-rust2");

            // And the reference decoder must accept what we produced.
            let mut back = Vec::new();
            std::io::copy(
                &mut BcjReader::new_x86(std::io::Cursor::new(&mine), 0),
                &mut back,
            )
            .unwrap();
            assert_eq!(back, input, "lzma-rust2 cannot undo our filter");
        }
    }

    #[test]
    fn bcj_round_trips_a_real_executable() {
        let Some(exe) = narc_exe() else {
            eprintln!("skipped: no narc binary in target/");
            return;
        };
        // Filter it the way the pipeline would: one 4 MiB chunk at a time.
        for chunk in exe.chunks(MAX_CHUNK) {
            let mut data = chunk.to_vec();
            bcj_x86_encode(&mut data, 0);
            assert_ne!(data, chunk, "BCJ found nothing to convert in x86 code");
            bcj_x86_decode(&mut data, 0);
            assert_eq!(data, chunk);
        }
    }

    fn lzma_len(data: &[u8]) -> usize {
        let mut opts = lzma_rust2::LzmaOptions::with_preset(1);
        opts.dict_size = MAX_CHUNK as u32;
        let mut w = lzma_rust2::LzmaWriter::new_no_header(Vec::new(), &opts, true).unwrap();
        w.write_all(data).unwrap();
        w.finish().unwrap().len()
    }

    /// The point of the filter: it has to actually pay for itself on real
    /// machine code, with both codecs this archiver cares about.
    #[test]
    fn bcj_improves_executable_compression() {
        let Some(sample) = exe_sample() else {
            eprintln!("skipped: no narc binary in target/");
            return;
        };
        let mut filtered = sample.clone();
        bcj_x86_encode(&mut filtered, 0);

        let plain_zstd = zstd::bulk::compress(&sample, 12).unwrap().len();
        let bcj_zstd = zstd::bulk::compress(&filtered, 12).unwrap().len();
        let plain_lzma = lzma_len(&sample);
        let bcj_lzma = lzma_len(&filtered);
        let gain = |before: usize, after: usize| 100.0 - after as f64 * 100.0 / before as f64;
        eprintln!(
            "BCJ x86 on {} KiB of narc.exe: zstd-12 {plain_zstd} -> {bcj_zstd} ({:+.2}%), \
             lzma-p1 {plain_lzma} -> {bcj_lzma} ({:+.2}%)",
            sample.len() / 1024,
            gain(plain_zstd, bcj_zstd),
            gain(plain_lzma, bcj_lzma),
        );
        assert!(bcj_zstd < plain_zstd, "BCJ must not hurt zstd");
        assert!(bcj_lzma < plain_lzma, "BCJ must not hurt LZMA");
    }

    /// Delta earns its keep on fixed-width records, the case the analyzer
    /// picks it for.
    #[test]
    fn delta_improves_fixed_width_records() {
        // 16-bit stereo PCM: a slow sine per channel, i.e. distance 4.
        let mut pcm = Vec::new();
        for i in 0..100_000i32 {
            let l = ((i as f64 / 50.0).sin() * 20_000.0) as i16;
            let r = ((i as f64 / 70.0).sin() * 18_000.0) as i16;
            pcm.extend_from_slice(&l.to_le_bytes());
            pcm.extend_from_slice(&r.to_le_bytes());
        }
        let plain = zstd::bulk::compress(&pcm, 12).unwrap().len();
        let mut filtered = pcm.clone();
        delta_encode(&mut filtered, 4);
        let delta = zstd::bulk::compress(&filtered, 12).unwrap().len();
        assert!(
            delta * 2 < plain,
            "delta-4 on stereo PCM: {plain} -> {delta}"
        );
    }
}

#[cfg(test)]
mod adversarial {
    use super::*;
    use std::io::Write;

    struct R(u64);
    impl R {
        fn n(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
    }

    fn gen(seed: u64, len: usize, kind: u8) -> Vec<u8> {
        let mut r = R(seed);
        (0..len)
            .map(|i| match kind {
                0 => r.n() as u8,
                1 => match r.n() % 6 {
                    0 => 0xE8,
                    1 => 0xE9,
                    2 | 3 => 0x00,
                    4 => 0xFF,
                    _ => r.n() as u8,
                },
                2 => 0x00,
                3 => 0xFF,
                4 => 0xE8,
                5 => 0xE9,
                6 => [0xE8, 0x00, 0xFF, 0xE9][i % 4],
                7 => [0xE8, 0xFE, 0x00, 0x00, 0x00][i % 5],
                8 => [0xE8, 0x01, 0xFF, 0xFF, 0xFF][i % 5],
                9 => (i % 2) as u8 * 0xFF,
                10 => [0xE9, 0xE8, 0xE8, 0xE9, 0x00, 0xFF][i % 6],
                _ => (i as u8).wrapping_mul(37),
            })
            .collect()
    }

    const KINDS: u8 = 12;

    /// Byte-for-byte agreement with lzma-rust2, at every start offset - not
    /// just 0, which is all the module's own test covers.
    #[test]
    fn matches_reference_at_every_offset() {
        use lzma_rust2::filter::bcj::{BcjReader, BcjWriter};
        for kind in 0..KINDS {
            for len in [0usize, 1, 4, 5, 6, 9, 10, 13, 64, 257, 4096, 10_000] {
                for off in [0u32, 1, 2, 3, 4, 5, 255, 256, 4096, 0x0100_0000, u32::MAX - 4, u32::MAX] {
                    let input = gen(len as u64 * 31 + kind as u64, len, kind);
                    let mut mine = input.clone();
                    bcj_x86_encode(&mut mine, off);

                    let mut w = BcjWriter::new_x86(Vec::new(), off as usize);
                    w.write_all(&input).unwrap();
                    let reference = w.finish().unwrap();
                    assert_eq!(
                        mine, reference,
                        "encode differs: kind {kind}, len {len}, offset {off}"
                    );

                    let mut back = Vec::new();
                    std::io::copy(
                        &mut BcjReader::new_x86(std::io::Cursor::new(&mine), off as usize),
                        &mut back,
                    )
                    .unwrap();
                    assert_eq!(back, input, "reference decode: kind {kind}, len {len}, off {off}");

                    let mut ours = mine.clone();
                    bcj_x86_decode(&mut ours, off);
                    assert_eq!(ours, input, "our decode: kind {kind}, len {len}, off {off}");
                }
            }
        }
    }

    /// The decoder runs on bytes straight out of an untrusted archive: it must
    /// terminate and never panic, whatever it is fed.
    #[test]
    fn decode_survives_arbitrary_bytes() {
        for kind in 0..KINDS {
            for len in [0usize, 1, 5, 6, 7, 8, 9, 17, 100, 1000, 20_000] {
                for off in [0u32, 1, 2, 3, 5, 0xFF, 0x1_0000, u32::MAX - 1, u32::MAX] {
                    for seed in 0..4u64 {
                        let mut d = gen(seed * 7919 + len as u64, len, kind);
                        bcj_x86_decode(&mut d, off);
                        let mut e = gen(seed * 7919 + len as u64, len, kind);
                        bcj_x86_encode(&mut e, off);
                    }
                }
            }
        }
    }

    /// Round-trip over every filter on degenerate and adversarial content,
    /// including the sizes that straddle the maximum chunk.
    #[test]
    fn round_trip_degenerate_and_chunk_sized() {
        let mut filters = vec![Filter::None, Filter::BcjX86];
        filters.extend((1..=MAX_DELTA_DISTANCE).map(Filter::Delta));
        let max = crate::archive::MAX_CHUNK as usize;
        for kind in 0..KINDS {
            for len in [0usize, 1, 2, 3, 4, 5, 31, 32, 33, max - 1, max, max + 1] {
                let original = gen(kind as u64, len, kind);
                for f in &filters {
                    let mut data = original.clone();
                    f.apply(&mut data);
                    f.unapply(&mut data);
                    assert_eq!(data, original, "{f:?} kind {kind} len {len}");
                }
            }
        }
    }

    /// Two independently filtered halves must concatenate back to the whole,
    /// at every cut, or dedup would be silently corrupting data.
    #[test]
    fn chunk_boundaries_never_corrupt() {
        for kind in 0..KINDS {
            let original = gen(0xBEEF + kind as u64, 3000, kind);
            for cut in 0..=original.len() {
                let mut head = original[..cut].to_vec();
                let mut tail = original[cut..].to_vec();
                bcj_x86_encode(&mut head, 0);
                bcj_x86_encode(&mut tail, 0);
                bcj_x86_decode(&mut head, 0);
                bcj_x86_decode(&mut tail, 0);
                head.extend_from_slice(&tail);
                assert_eq!(head, original, "kind {kind} cut {cut}");
            }
        }
    }
}

#[cfg(test)]
mod liblzma_vectors {
    use super::*;

    #[test]
    fn matches_liblzma() {
        let Ok(dir) = std::env::var("NARC_BCJ_VECTORS") else {
            eprintln!("skipped");
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let mut n = 0;
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|f| f.starts_with("in_"))
            .collect();
        entries.sort();
        for name in entries {
            let input = std::fs::read(dir.join(&name)).unwrap();
            let reference = std::fs::read(dir.join(name.replace("in_", "ref_"))).unwrap();
            let mut mine = input.clone();
            bcj_x86_encode(&mut mine, 0);
            assert_eq!(mine.len(), reference.len(), "{name}");
            let diff = mine
                .iter()
                .zip(&reference)
                .position(|(a, b)| a != b);
            assert_eq!(diff, None, "{name}: first differing byte");
            let mut back = mine.clone();
            bcj_x86_decode(&mut back, 0);
            assert_eq!(back, input, "{name}: round trip");
            n += 1;
        }
        eprintln!("verified {n} liblzma vectors");
        assert!(n > 100);
    }
}
