use std::io::{Read, Write};

use anyhow::{bail, Result};
use lzma_rust2::{Lzma2Options, Lzma2Reader, Lzma2Writer};
use ppmd_rust::{Ppmd7Decoder, Ppmd7Encoder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// Raw bytes, no compression.
    Store,
    /// Zstandard.
    Zstd,
    /// LZMA2 as a bare stream — no xz/7z container, no size field, no CRC.
    Lzma2,
    /// PPMd variant H (the 7-Zip flavour), 7z range coder.
    Ppmd7,
}

impl Codec {
    pub fn id(self) -> u8 {
        match self {
            Codec::Store => 0,
            Codec::Zstd => 1,
            Codec::Lzma2 => 2,
            Codec::Ppmd7 => 3,
        }
    }

    pub fn from_id(id: u8) -> Result<Codec> {
        match id {
            0 => Ok(Codec::Store),
            1 => Ok(Codec::Zstd),
            2 => Ok(Codec::Lzma2),
            3 => Ok(Codec::Ppmd7),
            other => bail!("unknown codec id {other} - archive was made by a newer version"),
        }
    }
}

pub fn compress(codec: Codec, level: i32, data: &[u8]) -> Result<Vec<u8>> {
    match codec {
        Codec::Store => Ok(data.to_vec()),
        Codec::Zstd => Ok(zstd::bulk::compress(data, level)?),
        Codec::Lzma2 => lzma2_compress(level, data),
        Codec::Ppmd7 => ppmd7_compress(data),
    }
}

pub fn decompress(codec: Codec, data: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    match codec {
        Codec::Store => Ok(data.to_vec()),
        Codec::Zstd => {
            let out = zstd::bulk::decompress(data, unpacked_len)?;
            if out.len() != unpacked_len {
                bail!("decompressed chunk size mismatch");
            }
            Ok(out)
        }
        Codec::Lzma2 => lzma2_decompress(data, unpacked_len),
        Codec::Ppmd7 => ppmd7_decompress(data, unpacked_len),
    }
}

// --- LZMA2 -----------------------------------------------------------------

/// Dictionary ceiling. A chunk never exceeds `archive::MAX_CHUNK`, so a larger
/// window cannot match anything extra — it only enlarges the encoder's match
/// tables (bt4 is ~11.5x the dictionary) and the decoder's ring buffer.
const LZMA2_DICT_MAX: u32 = crate::archive::MAX_CHUNK;

/// The window size is not stored anywhere: a bare LZMA2 stream carries the
/// LZMA props byte but not the dictionary size, and adding a byte per chunk to
/// hold it is not worth it. Both sides therefore derive it from the unpacked
/// length, which the manifest always knows — which also keeps decompression
/// allocation proportional to the caller-supplied length instead of a fixed
/// 4 MiB per worker.
///
/// A decoder window may legally be larger than the encoder's, but never
/// smaller, so the two must round identically; a power of two leaves no room
/// for the rounding to drift.
fn lzma2_dict_size(unpacked_len: usize) -> u32 {
    let want = u32::try_from(unpacked_len).unwrap_or(LZMA2_DICT_MAX);
    want.clamp(lzma_rust2::DICT_SIZE_MIN, LZMA2_DICT_MAX)
        .next_power_of_two()
}

/// Map the tier's zstd level (1..=22) onto an LZMA2 preset (here 1..=9).
///
/// The three tiers land where their names promise: fast (zstd 3) on preset 2,
/// still the hash-chain match finder; normal (zstd 12) on preset 6, the LZMA
/// default (bt4, nice_len 64); max (zstd 19) on preset 9. Presets differ in
/// dictionary size too, but that part is overridden by `lzma2_dict_size`, so
/// above the fast/normal split only the match finder effort changes.
fn lzma2_preset(level: i32) -> u32 {
    match level.clamp(1, 22) {
        1..=2 => 1,
        3..=5 => 2,
        6..=8 => 3,
        9..=11 => 5,
        12..=14 => 6,
        15..=17 => 7,
        18..=20 => 8,
        _ => 9,
    }
}

fn lzma2_compress(level: i32, data: &[u8]) -> Result<Vec<u8>> {
    let mut opts = Lzma2Options::with_preset(lzma2_preset(level));
    opts.lzma_options.dict_size = lzma2_dict_size(data.len());
    let mut w = Lzma2Writer::new(Vec::new(), opts);
    w.write_all(data)?;
    Ok(w.finish()?)
}

fn lzma2_decompress(data: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    // Sized from the manifest, never from the stream: a corrupt payload can
    // waste time but not memory.
    let mut out = vec![0u8; unpacked_len];
    let mut r = Lzma2Reader::new(data, lzma2_dict_size(unpacked_len), None);
    r.read_exact(&mut out)?;
    Ok(out)
}

// --- PPMd7 -----------------------------------------------------------------

/// PPMd7 model order. Neither order nor pool size is stored per chunk, so both
/// are part of the format: change them and every existing archive with a
/// PPMd7 chunk becomes undecodable.
///
/// 16 is where the curve flattens on <= 4 MiB text (measured by `bench_codecs`
/// on 4 MiB of prose): order 6, the 7-Zip default, is ~9% larger; order 32
/// buys another ~1% for ~1.5x the time.
const PPMD7_ORDER: u32 = 16;

/// Suballocator pool, allocated up front by both encoder and decoder, and the
/// single largest memory item a PPMd7 worker holds: **64 MiB** for a full-size
/// chunk (see `Tier::worker_memory`). Order 16 on 4 MiB of text exhausts a
/// 16 MiB pool and restarts the model mid-chunk, costing ~4% ratio; 64 MiB
/// does not restart, and more than that is never touched.
const PPMD7_MEM_MAX: u32 = 64 << 20;

/// Pool size for a chunk of `unpacked_len` bytes. Scaled with the data so a
/// 64 KiB tail chunk does not cost the same 64 MiB as a full one, and derived
/// only from the length so encoder and decoder always agree.
fn ppmd7_mem_size(unpacked_len: usize) -> u32 {
    let want = (unpacked_len as u64).saturating_mul(16);
    // The pool holds the model, not the data, so it has a useful floor of its
    // own: below ~1 MiB even a short text restarts the model.
    want.clamp(1 << 20, PPMD7_MEM_MAX as u64) as u32
}

fn ppmd7_compress(data: &[u8]) -> Result<Vec<u8>> {
    let mut enc = Ppmd7Encoder::new(Vec::new(), PPMD7_ORDER, ppmd7_mem_size(data.len()))?;
    enc.write_all(data)?;
    // No end marker: the manifest carries the exact unpacked length, and the
    // marker would cost a few bytes on every chunk for nothing.
    Ok(enc.finish(false)?)
}

fn ppmd7_decompress(data: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; unpacked_len];
    let mut dec = Ppmd7Decoder::new(data, PPMD7_ORDER, ppmd7_mem_size(unpacked_len))?;
    dec.read_exact(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Codec; 4] = [Codec::Store, Codec::Zstd, Codec::Lzma2, Codec::Ppmd7];
    /// Levels the tiers actually use (fast / normal / max).
    const LEVELS: [i32; 3] = [3, 12, 19];
    const CHUNK: usize = crate::archive::MAX_CHUNK as usize;

    /// xorshift64*; tests must be reproducible, so no thread_rng anywhere.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// Prose-shaped input: the case PPMd is supposed to win.
    fn text(len: usize) -> Vec<u8> {
        const WORDS: [&str; 24] = [
            "the",
            "archive",
            "format",
            "stores",
            "every",
            "chunk",
            "once",
            "and",
            "reads",
            "only",
            "what",
            "a",
            "single",
            "file",
            "needs",
            "because",
            "memory",
            "stays",
            "bounded",
            "while",
            "compression",
            "workers",
            "run",
            "deterministically",
        ];
        let mut rng = Rng(0x9E3779B97F4A7C15);
        let mut out = Vec::with_capacity(len + 32);
        let mut in_sentence = 0;
        while out.len() < len {
            out.extend_from_slice(WORDS[(rng.next() % WORDS.len() as u64) as usize].as_bytes());
            in_sentence += 1;
            if in_sentence >= 12 {
                out.extend_from_slice(b".\n");
                in_sentence = 0;
            } else {
                out.push(b' ');
            }
        }
        out.truncate(len);
        out
    }

    /// Structured binary: a table of little-endian records with slowly moving
    /// fields, i.e. compressible but not text.
    fn binary(len: usize) -> Vec<u8> {
        let mut rng = Rng(0xDEADBEEFCAFEF00D);
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
        let packed = compress(codec, level, data).expect("compress failed");
        let out = decompress(codec, &packed, data.len()).expect("decompress failed");
        assert_eq!(out, data, "{codec:?} level {level} did not round-trip");
        packed
    }

    #[test]
    fn codec_ids_round_trip() {
        for codec in ALL {
            assert_eq!(Codec::from_id(codec.id()).unwrap(), codec);
        }
        let err = Codec::from_id(4).unwrap_err().to_string();
        assert!(err.contains("unknown codec id 4"), "{err}");
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

    /// The chunker's hard maximum: the size every codec must survive, and the
    /// size the dictionary/pool caps are tuned for.
    #[test]
    fn round_trip_max_chunk() {
        for (name, data) in [("text", text(CHUNK)), ("binary", binary(CHUNK))] {
            assert_eq!(data.len(), CHUNK);
            for codec in ALL {
                let packed = round_trip(codec, 19, &data);
                eprintln!("{name} {codec:?}: {} -> {}", data.len(), packed.len());
            }
        }
    }

    /// Same input twice must give byte-identical output: the format's dedup
    /// and the `compact` rewrite both assume it.
    #[test]
    fn deterministic() {
        let data = text(300 * 1024);
        for codec in ALL {
            for level in LEVELS {
                let a = compress(codec, level, &data).unwrap();
                let b = compress(codec, level, &data).unwrap();
                assert_eq!(a, b, "{codec:?} level {level} is not deterministic");
            }
        }
    }

    /// A hostile or bit-rotted payload must come back as an error. It must
    /// never panic and never allocate beyond `unpacked_len`.
    #[test]
    fn corrupted_input_errors() {
        let data = text(200 * 1024);
        for codec in [Codec::Zstd, Codec::Lzma2, Codec::Ppmd7] {
            let packed = compress(codec, 12, &data).unwrap();

            let truncated = &packed[..packed.len() / 2];
            assert!(
                decompress(codec, truncated, data.len()).is_err(),
                "{codec:?} accepted a truncated payload"
            );

            let garbage = random(packed.len());
            assert!(
                decompress(codec, &garbage, data.len()).is_err(),
                "{codec:?} accepted a random payload"
            );

            // A single flipped bit may still decode into garbage of the right
            // length - that is what the chunk hash is for - but it must not
            // take the process down or hand back a differently sized buffer.
            let mut flipped = packed.clone();
            let last = flipped.len() - 1;
            flipped[last] ^= 0x80;
            if let Ok(out) = decompress(codec, &flipped, data.len()) {
                assert_eq!(out.len(), data.len());
            }
        }
    }

    /// Why the analyzer sends text to PPMd at the max tier: context modelling
    /// wins where LZ does not.
    ///
    /// Note what this measures and what it does not. LZMA2's usual edge over
    /// zstd comes largely from a big dictionary, and NARC caps the dictionary
    /// at the 4 MiB chunk size, so on a single chunk of prose the two land
    /// within a couple of percent of each other (LZMA2 pays for itself on
    /// binary data and with the BCJ filter, not here). PPMd, whose advantage
    /// is the model rather than the window, wins by ~25%.
    #[test]
    fn ppmd_beats_zstd_on_text() {
        let data = text(CHUNK);
        let zstd = compress(Codec::Zstd, 19, &data).unwrap().len();
        let lzma2 = compress(Codec::Lzma2, 19, &data).unwrap().len();
        let ppmd = compress(Codec::Ppmd7, 19, &data).unwrap().len();
        eprintln!(
            "text {}: zstd19 {zstd}, lzma2 {lzma2}, ppmd7 {ppmd}",
            data.len()
        );
        assert!(ppmd * 100 < zstd * 90, "ppmd7 {ppmd} vs zstd19 {zstd}");
        assert!(
            lzma2 * 100 < zstd * 105,
            "lzma2 {lzma2} is far worse than zstd19 {zstd}"
        );
    }

    /// Ratio and throughput per codec. Ignored because it compresses tens of
    /// megabytes; run with
    /// `cargo test -p narc-core --release codec::tests::bench_codecs -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_codecs() {
        use std::time::Instant;

        for (name, data) in [("text", text(CHUNK)), ("binary", binary(CHUNK))] {
            for codec in [Codec::Zstd, Codec::Lzma2, Codec::Ppmd7] {
                for level in LEVELS {
                    let t = Instant::now();
                    let packed = compress(codec, level, &data).unwrap();
                    let enc = t.elapsed();
                    let t = Instant::now();
                    let out = decompress(codec, &packed, data.len()).unwrap();
                    let dec = t.elapsed();
                    assert_eq!(out, data);
                    let mb = data.len() as f64 / (1024.0 * 1024.0);
                    eprintln!(
                        "{name:6} {codec:?} L{level:<2} ratio {:.3} ({} B)  enc {:.1} MB/s  dec {:.1} MB/s",
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
