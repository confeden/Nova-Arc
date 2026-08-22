use std::io::{Read, Write};

use anyhow::{bail, Result};
use lzma_rust2::{Lzma2Options, Lzma2Reader, Lzma2Writer};
use ppmd_rust::{Ppmd7Decoder, Ppmd7Encoder};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    /// Raw bytes, no compression.
    Store,
    /// Zstandard.
    Zstd,
    /// LZMA2 as a bare stream: no xz/7z container, no size field, no CRC.
    Lzma2,
    /// PPMd variant H (the 7-Zip flavour), with the 7z range coder.
    Ppmd7,
    /// Burrows-Wheeler transform plus QLFC, via libbsc.
    ///
    /// Added because it beats PPMd7 on text at nova's own unit size on all
    /// three axes at once — measured on enwik8 at a 32 MiB block: 21,983,674 B
    /// against 22,466,101, 2.0 s against 71.7 s to compress, 0.9 s against
    /// 18.3 s to decompress. It loses heavily on source trees, which is the
    /// whole reason it joins the tournament rather than replacing anything.
    Bsc,
}

impl Codec {
    pub fn id(self) -> u8 {
        match self {
            Codec::Store => 0,
            Codec::Zstd => 1,
            Codec::Lzma2 => 2,
            Codec::Ppmd7 => 3,
            Codec::Bsc => 4,
        }
    }

    pub fn from_id(id: u8) -> Result<Codec> {
        match id {
            0 => Ok(Codec::Store),
            1 => Ok(Codec::Zstd),
            2 => Ok(Codec::Lzma2),
            3 => Ok(Codec::Ppmd7),
            4 => Ok(Codec::Bsc),
            other => bail!("unknown codec id {other} - archive was made by a newer version"),
        }
    }
}

/// `param` carries codec-specific settings that the decoder must know and
/// that are cheaper to store per chunk than to fix for all time: for PPMd7 it
/// is the model order (measurements show no single order wins - order 10 is
/// better on prose and database records, order 16 on XML and source). Codecs
/// that need nothing ignore it.
pub fn compress(codec: Codec, level: i32, param: u8, data: &[u8]) -> Result<Vec<u8>> {
    match codec {
        Codec::Store => Ok(data.to_vec()),
        Codec::Zstd => Ok(zstd::bulk::compress(data, level)?),
        Codec::Lzma2 => lzma2_compress(level, data),
        Codec::Ppmd7 => ppmd7_compress(ppmd7_order(param), data),
        // libbsc needs nothing from `param`: its block sorter, entropy coder
        // and LZP settings are format constants of codec id 4.
        Codec::Bsc => nova_bsc::compress(data),
    }
}

/// Returns exactly `unpacked_len` bytes or an error — callers size buffers and
/// slice solid blocks from that number, so a payload that disagrees with the
/// manifest must not come back as a short or long buffer.
pub fn decompress(codec: Codec, data: &[u8], unpacked_len: usize, param: u8) -> Result<Vec<u8>> {
    match codec {
        // A stored chunk is its own payload, so the length is the only thing
        // there is to check; the other codecs get the check for free from the
        // output buffer they are made to fill.
        Codec::Store => {
            if data.len() != unpacked_len {
                bail!("decompressed chunk size mismatch");
            }
            Ok(data.to_vec())
        }
        Codec::Zstd => {
            let out = zstd::bulk::decompress(data, unpacked_len)?;
            if out.len() != unpacked_len {
                bail!("decompressed chunk size mismatch");
            }
            Ok(out)
        }
        Codec::Lzma2 => lzma2_decompress(data, unpacked_len),
        Codec::Ppmd7 => ppmd7_decompress(ppmd7_order(param), data, unpacked_len),
        // The length is checked against libbsc's own header before anything is
        // allocated, so a forged manifest cannot drive the allocation.
        Codec::Bsc => nova_bsc::decompress(data, unpacked_len),
    }
}

// -- LZMA2 -------------------------------------------------------------------

/// Dictionary ceiling. A chunk never exceeds `archive::MAX_CHUNK`, so a wider
/// window cannot match anything extra; it only grows the encoder's match
/// tables (bt4 costs ~11x the dictionary) and the decoder's ring buffer.
const LZMA2_DICT_MAX: u32 = 64 * 1024 * 1024;

/// A bare LZMA2 stream carries the LZMA props byte but *not* the dictionary
/// size, and spending a byte per chunk to store it is not worth it. Both sides
/// therefore derive the window from the unpacked length, which the manifest
/// always knows — which also keeps the decoder's allocation proportional to
/// the caller-supplied length instead of a flat 4 MiB per worker.
///
/// Correctness rests on both sides calling *this* function: a decoder window
/// may be wider than the encoder's, never narrower.
fn lzma2_dict_size(unpacked_len: usize) -> u32 {
    u32::try_from(unpacked_len)
        .unwrap_or(LZMA2_DICT_MAX)
        .clamp(lzma_rust2::DICT_SIZE_MIN, LZMA2_DICT_MAX)
}

/// Translate the tier's zstd level (1..=22) into an LZMA2 preset.
///
/// | zstd level | preset | encoder                       |
/// |---|---|---|
/// | 1..=2   | 1 | hc4, nice_len 128                  |
/// | 3..=5   | 2 | hc4, nice_len 273 — **fast tier**  |
/// | 6..=8   | 4 | bt4, nice_len 16                   |
/// | 9..=11  | 5 | bt4, nice_len 32                   |
/// | 12..    | 6 | bt4, nice_len 64 — normal *and max*|
///
/// The mapping tops out at 6 because presets 7..=9 differ from it *only* in
/// dictionary size, which the chunk cap erases. The one knob left is the
/// `xz -e` trick of raising `nice_len` to 273, and the max tier DOES take it
/// (below) — an earlier version of this comment said it did not, which stopped
/// being true and stayed here.
///
/// Priced on 28 real units: dropping to 64, which is where `xz -9` and
/// `7z -mx9` both sit, would save 19% of LZMA2's encode time and cost 0.48% of
/// its output. The average understates it — on Silesia's `nci` the deep search
/// is worth 19.9% — so it stays. See `kb/compression.md`.
fn lzma2_options(level: i32, unpacked_len: usize) -> Lzma2Options {
    let level = level.clamp(1, 22);
    let mut opts = Lzma2Options::with_preset(match level {
        1..=2 => 1,
        3..=5 => 2,
        6..=8 => 4,
        9..=11 => 5,
        _ => 6,
    });
    // Presets 7..9 differ from 6 only in dictionary size, which we override
    // anyway, so the max tier buys its extra effort the way `xz -e` does: by
    // searching out to the longest match LZMA can encode. Measured on Silesia
    // slices, that is worth 0.2-1 percentage point.
    if level >= 18 {
        opts.lzma_options.nice_len = 273;
    }
    opts.lzma_options.dict_size = lzma2_dict_size(unpacked_len);
    opts
}

/// Units at or above this get the match finder's helper thread.
///
/// It is a SIZE threshold and not a load decision on purpose. The helper can
/// shift which of several equal-length matches gets picked, so it moves the
/// output by a few hundred bytes in either direction — harmless in itself, but
/// only if the choice depends on the unit alone: an archive that came out
/// differently because the machine happened to be busy would break I8
/// (`-j 1` == `-j 8`). MEASURED: 1.34-1.42x on 24-62 MB units, and across the
/// six corpora it costs at most 753 bytes in 43 MB.
const LZMA2_MT_FROM: usize = 16 * 1024 * 1024;

/// How far the match finder searches down a hash chain. 0 means the SDK's own
/// figure, which at `nice_len` 273 comes out at 152.
const LZMA2_MATCH_CYCLES: u32 = 0;

/// THE ENCODER IS C, THE DECODER IS RUST, and that split is deliberate.
///
/// 7-Zip's own LZMA SDK is the same algorithm with a better optimal parser and
/// a hand-tuned match finder: measured on real 24-62 MB units at identical
/// settings it is 0.11-0.23% SMALLER and 1.31-1.35x faster than `lzma-rust2`,
/// and its threaded match finder is 1.4x on top of that. Smaller and faster at
/// once is rare enough to be worth an FFI crate, and it costs nothing on the
/// read path — what it writes is an ordinary LZMA2 stream, so
/// `lzma2_decompress` below is unchanged, still pure Rust and still the thing
/// that has to keep working in ten years.
///
/// The fallback is not decoration. If the C encoder ever refuses a buffer, the
/// Rust one produces a perfectly good stream in the same format; an archiver
/// that fails to pack because a codec had an opinion would be worse than one
/// that packs a few tenths of a percent larger.
fn lzma2_compress(level: i32, data: &[u8]) -> Result<Vec<u8>> {
    let opts = lzma2_options(level, data.len());
    let dict = opts.lzma_options.dict_size;
    let nice_len = opts.lzma_options.nice_len;
    let threads = if data.len() >= LZMA2_MT_FROM { 2 } else { 1 };
    match nova_lzma::compress(data, dict, nice_len, threads, LZMA2_MATCH_CYCLES) {
        Ok(out) => Ok(out),
        Err(_) => {
            let mut w = Lzma2Writer::new(Vec::new(), opts);
            w.write_all(data)?;
            Ok(w.finish()?)
        }
    }
}

fn lzma2_decompress(data: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    // Sized from the manifest, never from the stream: a hostile payload can
    // waste time but not memory.
    let mut out = vec![0u8; unpacked_len];
    let mut r = Lzma2Reader::new(data, lzma2_dict_size(unpacked_len), None);
    r.read_exact(&mut out)?;
    Ok(out)
}

// -- PPMd7 -------------------------------------------------------------------

/// Model order. Neither the order nor the pool size is stored per chunk, so
/// both are part of the format: change them and every existing archive with a
/// PPMd7 chunk becomes undecodable.
///
/// 10 is the robust choice for chunks of at most 4 MiB, not the strongest one.
/// PPMd7 restarts its model from scratch when the pool runs out, and a restart
/// inside a 4 MiB chunk costs far more than a higher order gains: measured on
/// three 4 MiB samples of real text, order 10 beat zstd-19 on all of them
/// (-3.5%, -19.6%, -8.0%) while order 12 and 16 each lost on one by 5-6%.
const PPMD7_ORDER_DEFAULT: u8 = 10;

/// Orders worth trying. No single one wins: measured on 32 MiB units, order 10
/// is 3-5% better on prose, wiki text and database records, order 16 is 5-8%
/// better on XML and source code.
pub const PPMD7_ORDERS: [u8; 2] = [10, 16];

/// PPMd7 accepts orders 2..=64; a manifest is untrusted input, so clamp
/// instead of trusting it, and treat 0 as "the default" for chunks written
/// before the parameter existed.
fn ppmd7_order(param: u8) -> u32 {
    let o = if param == 0 {
        PPMD7_ORDER_DEFAULT
    } else {
        param
    };
    o.clamp(2, 64) as u32
}

/// Suballocator pool ceiling — the memory a PPMd7 worker holds on top of its
/// chunk buffers, and the number `Tier::worker_memory()` needs. 64 MiB is
/// where a full 4 MiB chunk stops restarting the model; below it the ratio
/// falls off a cliff (48 MiB was 9% worse on one sample), above it nothing
/// changes.
pub const PPMD7_MEM_MAX: u32 = 256 << 20;

/// Pool for a chunk of `unpacked_len` bytes: 32x the data, which is where the
/// output stops changing at every size measured, so small chunks neither pay
/// for nor zero 64 MiB. Derived only from the length, so encoder and decoder
/// always land on the same value.
fn ppmd7_mem_size(unpacked_len: usize) -> u32 {
    (unpacked_len as u64)
        .saturating_mul(32)
        .clamp(1 << 20, PPMD7_MEM_MAX as u64) as u32
}

fn ppmd7_compress(order: u32, data: &[u8]) -> Result<Vec<u8>> {
    let mut enc = Ppmd7Encoder::new(Vec::new(), order, ppmd7_mem_size(data.len()))?;
    enc.write_all(data)?;
    // No end marker: the manifest carries the exact unpacked length, so the
    // marker would only add bytes to every chunk.
    Ok(enc.finish(false)?)
}

fn ppmd7_decompress(order: u32, data: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; unpacked_len];
    let mut dec = Ppmd7Decoder::new(data, order, ppmd7_mem_size(unpacked_len))?;
    dec.read_exact(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Codec; 5] = [
        Codec::Store,
        Codec::Zstd,
        Codec::Lzma2,
        Codec::Ppmd7,
        Codec::Bsc,
    ];
    /// The levels the tiers actually use: fast, normal, max.
    const LEVELS: [i32; 3] = [3, 12, 19];
    const CHUNK: usize = crate::archive::MAX_CHUNK as usize;

    /// xorshift64. Test corpora must be reproducible, so nothing here is
    /// allowed to reach for a real RNG.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        /// Index into a vocabulary with a heavy head: half the draws fall in
        /// the first half of the list, a quarter in the first quarter, and so
        /// on. Integer-only, because `powf` is not bit-identical across libms
        /// and the corpus must be the same on every machine.
        fn zipf(&mut self, n: usize) -> usize {
            let shift = self.next().trailing_zeros().min(12) as usize;
            (self.next() as usize) % (n >> shift).max(1)
        }
    }

    /// Prose-shaped input, the case these codecs exist for. Real text is a
    /// large vocabulary drawn with a heavy head, plus strong word-to-word
    /// correlation; both are needed, because a corpus built from a handful of
    /// words compresses to almost nothing and tells us nothing.
    fn text(len: usize) -> Vec<u8> {
        const VOCAB: usize = 4096;
        const SUCCESSORS: usize = 3;
        const CONSONANTS: &[u8] = b"tnshrdlcmwfgypbvkjqxz";
        const VOWELS: &[u8] = b"eaoiuy";

        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut vocab: Vec<Vec<u8>> = Vec::with_capacity(VOCAB);
        for _ in 0..VOCAB {
            let mut word = Vec::new();
            for _ in 0..1 + rng.next() % 3 {
                word.push(CONSONANTS[(rng.next() % CONSONANTS.len() as u64) as usize]);
                word.push(VOWELS[(rng.next() % VOWELS.len() as u64) as usize]);
                if rng.next().is_multiple_of(3) {
                    word.push(CONSONANTS[(rng.next() % CONSONANTS.len() as u64) as usize]);
                }
            }
            vocab.push(word);
        }
        // Each word has a few likely successors: a bigram chain, which is
        // where real prose gets its short-range redundancy.
        let successors: Vec<u32> = (0..VOCAB * SUCCESSORS)
            .map(|_| rng.zipf(VOCAB) as u32)
            .collect();

        let mut out = Vec::with_capacity(len + 128);
        let mut word = 0usize;
        let mut column = 0usize;
        let mut until_period = 6 + rng.next() % 14;
        while out.len() < len {
            out.extend_from_slice(&vocab[word]);
            column += vocab[word].len() + 1;
            until_period -= 1;
            if until_period == 0 {
                out.push(b'.');
                until_period = 6 + rng.next() % 14;
            }
            if column > 72 {
                out.push(b'\n');
                column = 0;
            } else {
                out.push(b' ');
            }
            word = if rng.next() % 100 < 95 {
                successors[word * SUCCESSORS + (rng.next() as usize % SUCCESSORS)] as usize
            } else {
                rng.zipf(VOCAB)
            };
        }
        out.truncate(len);
        out
    }

    /// Structured binary: a record table with slowly moving fields —
    /// compressible, but nothing a text model can help with.
    fn binary(len: usize) -> Vec<u8> {
        let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
        let mut out = Vec::with_capacity(len + 16);
        let mut counter: u32 = 0;
        while out.len() < len {
            counter = counter.wrapping_add(1);
            out.extend_from_slice(&counter.to_le_bytes());
            out.extend_from_slice(&(counter as f32 * 0.5).to_le_bytes());
            out.extend_from_slice(&[(rng.next() & 0x3) as u8; 4]);
            out.extend_from_slice(&(rng.next() as u32 & 0xFFFF).to_le_bytes());
        }
        out.truncate(len);
        out
    }

    /// Whatever a codec does to this, it must not shrink it.
    fn random(len: usize) -> Vec<u8> {
        let mut rng = Rng(0x1234_5678_9ABC_DEF0);
        let mut out = Vec::with_capacity(len + 8);
        while out.len() < len {
            out.extend_from_slice(&rng.next().to_le_bytes());
        }
        out.truncate(len);
        out
    }

    fn round_trip(codec: Codec, level: i32, data: &[u8]) -> Vec<u8> {
        let packed = compress(codec, level, 0, data).expect("compress failed");
        let out = decompress(codec, &packed, data.len(), 0).expect("decompress failed");
        assert_eq!(out, data, "{codec:?} level {level} did not round-trip");
        packed
    }

    #[test]
    fn codec_ids_round_trip() {
        for codec in ALL {
            assert_eq!(Codec::from_id(codec.id()).unwrap(), codec);
        }
        let err = Codec::from_id(5).unwrap_err().to_string();
        assert!(err.contains("unknown codec id 5"), "{err}");
    }

    #[test]
    fn round_trip_text() {
        let data = text(700 * 1024);
        for codec in ALL {
            for level in LEVELS {
                round_trip(codec, level, &data);
            }
        }
    }

    #[test]
    fn round_trip_binary() {
        let data = binary(700 * 1024);
        for codec in ALL {
            for level in LEVELS {
                round_trip(codec, level, &data);
            }
        }
    }

    #[test]
    fn round_trip_incompressible() {
        let data = random(700 * 1024);
        for codec in ALL {
            for level in LEVELS {
                round_trip(codec, level, &data);
            }
        }
    }

    #[test]
    fn round_trip_empty() {
        for codec in ALL {
            for level in LEVELS {
                round_trip(codec, level, &[]);
            }
        }
    }

    #[test]
    fn round_trip_one_byte() {
        for codec in ALL {
            for level in LEVELS {
                round_trip(codec, level, b"N");
            }
        }
    }

    /// The chunker's hard maximum, and the size the dictionary and pool caps
    /// are tuned for. Split by input kind because these two are by far the
    /// slowest tests in a debug build, and the harness runs them in parallel.
    #[test]
    fn round_trip_max_chunk_text() {
        let data = text(CHUNK);
        assert_eq!(data.len(), CHUNK);
        for codec in ALL {
            round_trip(codec, 19, &data);
        }
    }

    #[test]
    fn round_trip_max_chunk_binary() {
        let data = binary(CHUNK);
        assert_eq!(data.len(), CHUNK);
        for codec in ALL {
            round_trip(codec, 19, &data);
        }
    }

    /// Same input, same bytes out. Dedup across runs and the `compact` rewrite
    /// both assume it.
    #[test]
    fn deterministic() {
        let data = text(300 * 1024);
        for codec in ALL {
            for level in LEVELS {
                let a = compress(codec, level, 0, &data).unwrap();
                let b = compress(codec, level, 0, &data).unwrap();
                assert_eq!(a, b, "{codec:?} level {level} is not deterministic");
            }
        }
    }

    /// A truncated or hostile payload must come back as an error, never as a
    /// panic and never as an allocation larger than `unpacked_len`.
    #[test]
    fn corrupted_input_errors() {
        let data = text(200 * 1024);
        for codec in [Codec::Zstd, Codec::Lzma2, Codec::Ppmd7, Codec::Bsc] {
            let packed = compress(codec, 12, 0, &data).unwrap();

            let truncated = &packed[..packed.len() / 2];
            assert!(
                decompress(codec, truncated, data.len(), 0).is_err(),
                "{codec:?} accepted a truncated payload"
            );
            assert!(
                decompress(codec, &random(packed.len()), data.len(), 0).is_err(),
                "{codec:?} accepted a random payload"
            );
            assert!(
                decompress(codec, &[], data.len(), 0).is_err(),
                "{codec:?} accepted an empty payload"
            );

            // A single flipped bit may still decode into garbage of the right
            // length - catching that is the chunk hash's job - but it must not
            // take the process down or hand back a differently sized buffer.
            let mut flipped = packed.clone();
            let last = flipped.len() - 1;
            flipped[last] ^= 0x80;
            if let Ok(out) = decompress(codec, &flipped, data.len(), 0) {
                assert_eq!(out.len(), data.len());
            }
        }
    }

    /// `unpacked_len` comes from the manifest, which an attacker controls
    /// independently of the payload. Whatever it says, the buffer handed back
    /// must be exactly that long or the call must fail: `extract_one` slices a
    /// solid block with lengths taken from the same manifest, so a codec that
    /// quietly returned a different amount would hand one file another file's
    /// bytes. Store is the one that has to be told, having no output buffer of
    /// its own to be sized by.
    #[test]
    fn decompress_returns_exactly_the_declared_length() {
        let data = text(50 * 1024);
        for codec in ALL {
            let packed = compress(codec, 12, 0, &data).unwrap();
            for claim in [0, 1, data.len() - 1, data.len() + 1, 4 * data.len()] {
                if let Ok(out) = decompress(codec, &packed, claim, 0) {
                    assert_eq!(
                        out.len(),
                        claim,
                        "{codec:?} returned {} bytes for a declared {claim}",
                        out.len()
                    );
                }
            }
        }
    }

    /// The reason these two codecs exist. The LZMA2 margin over zstd-19 is
    /// genuinely thin on text (~1%, same as on real source trees); PPMd is
    /// where the tier earns its seconds.
    #[test]
    fn strong_codecs_beat_zstd_on_text() {
        let data = text(CHUNK);
        let zstd = compress(Codec::Zstd, 19, 0, &data).unwrap().len();
        let lzma2 = compress(Codec::Lzma2, 19, 0, &data).unwrap().len();
        let ppmd = compress(Codec::Ppmd7, 19, 0, &data).unwrap().len();
        assert!(lzma2 < zstd, "lzma2 {lzma2} >= zstd19 {zstd}");
        assert!(ppmd < zstd, "ppmd7 {ppmd} >= zstd19 {zstd}");
    }

    /// Ratio and throughput per codec. Ignored because it compresses tens of
    /// megabytes at the slowest settings; run it with
    /// `cargo test -p nova-core --release codec::tests::bench -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench() {
        use std::time::Instant;

        for (name, data) in [("text", text(CHUNK)), ("binary", binary(CHUNK))] {
            for codec in [Codec::Zstd, Codec::Lzma2, Codec::Ppmd7] {
                for level in LEVELS {
                    let t = Instant::now();
                    let packed = compress(codec, level, 0, &data).unwrap();
                    let enc = t.elapsed();
                    let t = Instant::now();
                    let out = decompress(codec, &packed, data.len(), 0).unwrap();
                    let dec = t.elapsed();
                    assert_eq!(out, data);
                    let mb = data.len() as f64 / (1024.0 * 1024.0);
                    eprintln!(
                        "{name:6} {codec:?} L{level:<2} ratio {:.4} ({:>7} B)  enc {:6.1} MB/s  dec {:6.1} MB/s",
                        packed.len() as f64 / data.len() as f64,
                        packed.len(),
                        mb / enc.as_secs_f64(),
                        mb / dec.as_secs_f64(),
                    );
                }
            }
        }
    }
}
