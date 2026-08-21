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

    /// FastCDC parameters: (min, avg, max) chunk size.
    ///
    /// The chunk is the compression unit, so its size is a direct ratio knob:
    /// measured on Silesia with LZMA2, chunking at 4 MiB costs 1 percentage
    /// point against 64 MiB chunks (24.0% vs 23.0%). It is also the edit
    /// granularity — changing a byte rewrites its chunk — so fast and normal
    /// stay small and only max trades edit cost for ratio.
    /// FastCDC caps these at min 1 MiB / avg 4 MiB / max 16 MiB, so the max
    /// tier takes the largest chunking the algorithm allows.
    pub fn cdc(self) -> (u32, u32, u32) {
        match self {
            Tier::Fast | Tier::Normal => (256 * 1024, 1024 * 1024, 4 * 1024 * 1024),
            Tier::Max => (1024 * 1024, 4 * 1024 * 1024, 16 * 1024 * 1024),
        }
    }

    /// Target size of a compression unit — the single biggest ratio lever.
    /// Measured on a 5739-file source tree with LZMA2 -9e, against one solid
    /// stream: 1 MiB units cost +74%, 4 MiB +50%, 16 MiB +19%, 32 MiB only
    /// +4.9%. The cost is edit granularity, since changing a byte rewrites its
    /// whole unit — so the fast tier keeps units small and only max trades
    /// edit cost for ratio.
    pub fn unit(self) -> u64 {
        match self {
            Tier::Fast => 4 * 1024 * 1024,
            Tier::Normal => 16 * 1024 * 1024,
            Tier::Max => 32 * 1024 * 1024,
        }
    }

    /// Files at or above this size are cut into content-defined chunks before
    /// being grouped, so that editing part of a large file rewrites only the
    /// units that part touched. Smaller files are placed whole.
    pub fn chunked_from(self) -> u64 {
        match self {
            Tier::Fast | Tier::Normal => 256 * 1024,
            Tier::Max => 1024 * 1024,
        }
    }

    /// Codecs to try for a unit the analyzer classified as compressible.
    ///
    /// Fast and normal trust the analyzer's single choice. Max runs a
    /// tournament and keeps the smallest result, because no static rule wins:
    /// measured on 32 MiB units, PPMd7 beats LZMA2 by 13-24% on prose, wiki
    /// text and database records, while LZMA2 beats PPMd7 by 16% on binaries
    /// and by 10-20% on solid blocks of source code.
    pub fn candidates(self, first: Codec, kind: Option<Kind>) -> Vec<(Codec, u8)> {
        if first == Codec::Store {
            return vec![(first, 0)];
        }
        // A unit whose magic says it is ALREADY ENTROPY-CODED, and which only
        // cleared the 1% trial bar, is not worth the full tournament: PPMd7
        // models symbol contexts, and data an entropy coder has already been
        // over has none left to model. It ran anyway and won nothing.
        // `Class::Wav` joins it for the same reason one step later: by the time a
        // codec sees the unit, the record-width filter or FLAC has already run
        // and what is left is an entropy-coded stream. Measured on 518 MB of
        // PCM: LZMA2 and bsc split the units between them, PPMd7 took none.
        if self == Tier::Max
            && matches!(
                kind,
                Some(Class::Precompressed) | Some(Class::Wav) | Some(Class::Mp3)
            )
        {
            return vec![(first, 0), (Codec::Bsc, 0)];
        }
        // The normal tier gets a two-horse race rather than a single pick.
        // MEASURED: enwik8 −24.6%, Silesia −19.0%, PDFs −10.0%, precomp −9.0%,
        // source tree −6.2%, for 1.6-1.9x the encode time. Normal now beats
        // 7z -mx9 on Silesia in a sixth of its time.
        if self == Tier::Normal {
            return vec![(first, 0), (Codec::Bsc, 0)];
        }
        // FAST DELIBERATELY DOES NOT GET IT. Measured twice, the second time
        // after intra-file decode lanes landed, which was the condition the
        // first refusal named. The ratio is tempting — enwik8 −31.9%, Silesia
        // −27.5%, source tree −24.1% — and the lanes did fix the single-file
        // case (enwik8 decode 5.07 → 1.30 s). But they only help when there are
        // fewer files than workers, so a real tree still pays: Silesia decode
        // 0.46 → 3.19 s, source tree 1.84 → 4.14 s.
        //
        // The ladder settles it. Fast with bsc would be 47.8 MB / 2.90 s pack /
        // 3.19 s extract against NORMAL's 46.4 MB / 7.0 s / 2.09 s — smaller
        // only in pack time, worse in both others. That is not a fast tier, it
        // is a worse normal one.
        if self != Tier::Max {
            return vec![(first, 0)];
        }
        let mut out = vec![(Codec::Lzma2, 0)];
        out.extend(
            crate::codec::PPMD7_ORDERS
                .iter()
                .map(|&o| (Codec::Ppmd7, o)),
        );
        // BWT is the fourth entrant, not a replacement, because it is the only
        // one that both wins and loses by a wide margin depending on the data.
        // Measured at nova's own 32 MiB unit: on enwik8 it beats PPMd7 on ratio
        // AND is 35x faster to encode and 20x to decode; on a source tree it is
        // 45% worse than what nova already produces. A tournament settles that
        // per unit, which is the entire argument for having one.
        out.push((Codec::Bsc, 0));
        out
    }

    /// Memory one compression worker holds on top of its chunk buffers,
    /// dominated by the codec's tables. Measured: ~2 MiB for zstd level 3,
    /// ~37 MiB for level 12. LZMA2's bt4 match finder costs ~11x its
    /// dictionary, which equals the chunk size, and PPMd7's model pool is 8x
    /// the chunk capped at 256 MiB — at the max tier those dominate.
    /// Used to decide how many workers fit in the memory budget.
    pub fn worker_memory(self) -> u64 {
        match self {
            Tier::Fast => 4 * 1024 * 1024,
            Tier::Normal => 40 * 1024 * 1024,
            // 32 MiB unit: LZMA2 tables ~370 MiB, PPMd7 pool 256 MiB. They run
            // one after another in the tournament, so the larger one governs.
            Tier::Max => 384 * 1024 * 1024,
        }
    }

    /// The chunking geometry this tier would create a new archive with.
    pub fn geometry(self) -> crate::manifest::Geometry {
        let (chunk_min, chunk_avg, chunk_max) = self.cdc();
        crate::manifest::Geometry {
            chunk_min,
            chunk_avg,
            chunk_max,
            unit: self.unit(),
            chunked_from: self.chunked_from(),
        }
    }
}

/// What the analyzer decided for a file or a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub codec: Codec,
    pub filter: Filter,
    /// What the data looked like. Used to keep a unit homogeneous: mixing
    /// unrelated kinds in one stream costs ratio, because a compressor tuned
    /// by the first megabytes then meets something else entirely.
    pub kind: Kind,
}

impl Plan {
    pub const STORE: Plan = Plan {
        codec: Codec::Store,
        filter: Filter::None,
        kind: Kind::Precompressed,
    };
}

/// Content classes worth treating differently.
pub type Kind = Class;

/// Content classes worth treating differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Entropy-coded already: JPEG, MP3, zip, video…
    Precompressed,
    /// Machine code: benefits from the BCJ address transform.
    Executable,
    /// Human-readable or source text: PPMd models it best.
    Text,
    /// A JPEG photograph. Entropy-coded, but reversibly: lepton re-codes the
    /// DCT coefficients and rebuilds the file bit for bit.
    Jpeg,
    /// A container built out of deflate streams — zip, PNG, gzip, docx, jar.
    /// Its bytes are entropy-coded, but reversibly so, which is the one case
    /// where already-compressed data can still be shrunk a lot.
    Deflate,
    /// RIFF/WAVE integer PCM: the opposite case, data that is not compressed at
    /// all and that a waveform model beats every general codec on.
    Wav,
    /// MPEG Layer III. Entropy-coded like `Jpeg`, but only in part: the frame
    /// headers and side info are structure, and separating them from the
    /// spectral data is what `Filter::Mp3` does.
    Mp3,
    Generic,
}

const TRIAL_SAMPLE: usize = 64 * 1024;

/// Deciding whether already-compressed data is *really* finished needs range,
/// not detail: deflate leaves matches that are megabytes apart.
const PRECOMP_TRIAL: usize = 1024 * 1024;

/// The verdict for data whose magic says it is already compressed, skipping the
/// deflate-container path. Used when a container is too small to be given a unit
/// of its own, which recompression requires.
pub fn plan_precompressed(head: &[u8], tier: Tier) -> Plan {
    if saves_at_least(head, PRECOMP_TRIAL, 1) {
        Plan {
            codec: general_codec(tier),
            filter: Filter::None,
            kind: Class::Precompressed,
        }
    } else {
        Plan::STORE
    }
}

/// Decide how to store data given its first bytes.
pub fn plan(head: &[u8], tier: Tier) -> Plan {
    // Too small to classify and too small to matter: let the compressor try,
    // the per-chunk fallback stores it raw if that does not pay off.
    if head.len() < 256 {
        return Plan {
            codec: general_codec(tier),
            filter: Filter::None,
            kind: Class::Generic,
        };
    }
    // Checked before the general precompressed test, because these formats are
    // the exception to it: their bytes are compressed, and undoing that is worth
    // 38%. Detection is by MAGIC, never by scanning for streams — a zip's
    // central directory lives at the END of the file, so a head sample would
    // silently disable the whole feature for zips.
    if is_jpeg(head) {
        return Plan {
            // The codec runs over lepton's output, which is already entropy
            // coded; the pipeline keeps whichever is smaller, and measured, the
            // bare lepton blob always is.
            codec: general_codec(tier),
            filter: Filter::Jpeg,
            kind: Class::Jpeg,
        };
    }
    if is_deflate_container(head) {
        return Plan {
            codec: general_codec(tier),
            filter: Filter::ContainerChunked,
            kind: Class::Deflate,
        };
    }
    // PCM is the reverse of the two above — not entropy-coded at all, and so
    // far from it that the record-width filter plus LZMA2 still leaves 8
    // percentage points to a waveform model. Routed by magic for the same
    // reason: the transform needs the whole RIFF container, so the decision has
    // to be made before the file is cut into chunks.
    if crate::wav::is_wav(head) {
        return Plan {
            // The codec runs over the FLAC stream, which is already entropy
            // coded; as with lepton the pipeline keeps whichever is smaller.
            codec: general_codec(tier),
            filter: Filter::Wav,
            kind: Class::Wav,
        };
    }
    // MP3 is the in-between case: mostly entropy-coded, but with a 4-byte
    // header and 9-32 bytes of side info per frame that are pure structure.
    // Routed by magic for the same reason as the three above — the transform
    // wants whole frames, so the decision precedes chunking.
    if crate::mp3::is_mp3(head) {
        return Plan {
            codec: general_codec(tier),
            filter: Filter::Mp3,
            kind: Class::Mp3,
        };
    }
    match classify(head) {
        // The magic says "already entropy-coded", and for JPEG, MP4 or zstd
        // that is the end of it. But it is a claim about the *format*, not
        // about the bytes: a zip written at deflate level 1, or a PNG of a
        // screenshot, still carries real redundancy. MEASURED on a 4.93 MiB
        // corpus of zips, PNGs and gzips: storing on magic alone cost
        // 5,167,845 B, while 7-Zip — which simply tries — reached 4,047,465 B,
        // and plain per-file LZMA2 -9e reached 4,048,662 B. So the verdict has
        // to be earned on a sample.
        //
        // The sample must be a megabyte, and this is the whole trick: at 64 KiB
        // every one of these files looks like noise (+0.02%), while at 1 MiB the
        // compressible ones separate cleanly — source.zip −25.6%, a screenshot
        // PNG −3.9% — and genuinely finished data (7z output, random bytes, a
        // fully-deflated docx) sits at +0.00..0.01%. A 1% bar has three orders
        // of magnitude of margin, and the per-unit raw fallback catches the rest.
        Class::Precompressed if saves_at_least(head, PRECOMP_TRIAL, 1) => Plan {
            codec: general_codec(tier),
            filter: Filter::None,
            kind: Class::Precompressed,
        },
        Class::Precompressed => Plan::STORE,
        // `classify` never returns this — the deflate containers are matched by
        // magic before the classifier runs — but the arm is real rather than
        // unreachable!(), so a future classifier change cannot panic here.
        Class::Deflate => Plan {
            codec: general_codec(tier),
            filter: Filter::ContainerChunked,
            kind: Class::Deflate,
        },
        // Same story as Class::Deflate: matched by magic before the classifier.
        Class::Jpeg => Plan {
            codec: general_codec(tier),
            filter: Filter::Jpeg,
            kind: Class::Jpeg,
        },
        Class::Executable => Plan {
            codec: general_codec(tier),
            // Machine code is full of relative call targets; making them
            // absolute turns repeated calls into repeated byte patterns.
            // At the max tier the targets also LEAVE the code stream, which
            // measured 3.5% better than patching them in place on 234 MiB of
            // Windows DLLs — the four address bytes stop interrupting matches.
            // The cheaper tiers keep the in-place filter: splitting costs a
            // second pass and they exist to be fast.
            filter: if tier == Tier::Max {
                Filter::X86Split
            } else {
                Filter::BcjX86
            },
            kind: Class::Executable,
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
            kind: Class::Text,
        },
        // Routed by magic above, exactly like Jpeg and Deflate; `classify`
        // never returns it.
        Class::Wav => Plan {
            codec: general_codec(tier),
            filter: Filter::Wav,
            kind: Class::Wav,
        },
        // Likewise: matched by `is_mp3` before the classifier runs.
        Class::Mp3 => Plan {
            codec: general_codec(tier),
            filter: Filter::Mp3,
            kind: Class::Mp3,
        },
        Class::Generic => plan_generic(head, tier),
    }
}

/// The verdict for binary data of no recognised format. Reachable on its own
/// because a .wav too large for a unit of its own still wants the record-width
/// filter — `plan_precompressed` would give it `Filter::None` and throw away
/// 27%, since PCM is not precompressed at all.
pub fn plan_generic(head: &[u8], tier: Tier) -> Plan {
    // Fixed-width records — database rows, catalogues, audio frames,
    // arrays of numbers — look like noise to a match finder because
    // nothing repeats byte for byte, yet each column changes slowly.
    // Differencing at the record width exposes that, and it is the
    // one case where data everyone calls incompressible is not.
    //
    // "Already compresses" is NOT a reason to skip the detector, and
    // an early return on it cost 27% on 16-bit stereo PCM: a .wav
    // compresses to 82% unfiltered, cleared the gate, and never met
    // the filter that takes it to 60%. `pays_off` is the guard — it
    // trials the transform and keeps it only on a clear win, which is
    // exactly what the source trees the gate was written for need.
    //
    // Capped deliberately: the detector runs one entropy pass per
    // candidate distance, so it is the one test whose cost scales with
    // the sample. 64 KiB is plenty to see a record structure.
    let filter = crate::filters::detect_delta_stride(&head[..head.len().min(TRIAL_SAMPLE)])
        .and_then(|d| Filter::delta(d).ok())
        .filter(|f| pays_off(head, *f, tier));
    match filter {
        Some(filter) => Plan {
            codec: general_codec(tier),
            filter,
            kind: Class::Generic,
        },
        // No stride helps. Compressible data still goes to the codec;
        // only data that is noise to both is stored.
        None if compresses(head) => Plan {
            codec: general_codec(tier),
            filter: Filter::None,
            kind: Class::Generic,
        },
        None => Plan::STORE,
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

/// Does this filter actually make the data smaller?
///
/// The record-width estimator proposes a transform from entropy alone, which
/// is a proxy and sometimes wrong: on Silesia's `sao` star catalogue every
/// differencing width *hurts* (LZMA2 grows 8-60%), because the fields it
/// separates were already being matched. So the proposal is tried on the
/// sample and kept only if it wins.
///
/// TWO THINGS THIS STEP HAS TO GET RIGHT, both learned by regression.
///
/// It runs on the WHOLE head, not the detector's 64 KiB: a Firefox XML unit
/// read as a 4-byte record structure in its first 64 KiB and cost 176 KB once
/// the filter met the other 1.4 MB.
///
/// And it trials with the codec that will actually run, which from the normal
/// tier up means bsc. BWT sorts by following context, so it already models
/// interleaved fixed-width records, and differencing them severs the sample's
/// high byte from its low — the same reason the byte-plane split died. A zstd
/// verdict cannot see this and no threshold separates the cases: on the 1 MiB
/// head Silesia's `x-ray` shows **-25.7% under zstd and +0.9% under bsc**,
/// while a 16-bit stereo `.wav` shows -20.5% and **-28.7%**. Judged by zstd
/// the filter is approved for both and costs 0.77% on Silesia; judged by bsc
/// `mr` (+1.6%), `x-ray` and `sao` (+5.6%) are all refused and PCM still wins
/// by a mile.
fn pays_off(head: &[u8], filter: Filter, tier: Tier) -> bool {
    let Some(plain) = trial(tier, head) else {
        return false;
    };
    let mut filtered = head.to_vec();
    if filter.apply(&mut filtered).is_err() {
        return false;
    }
    match trial(tier, &filtered) {
        // Require a clear win: a filter byte and a decode-side pass are not
        // worth a fraction of a percent.
        Some(c) => c * 100 < plain * 98,
        None => false,
    }
}

/// The stand-in for the codec this tier will reach for. Fast has no bsc
/// (measured twice and refused), so zstd speaks for it.
fn trial(tier: Tier, data: &[u8]) -> Option<usize> {
    match tier {
        Tier::Fast => zstd::bulk::compress(data, 1).ok().map(|v| v.len()),
        _ => nova_bsc::compress(data).ok().map(|v| v.len()),
    }
}

/// Cheap check that data is worth compressing at all: 3% savings on a sample.
fn compresses(head: &[u8]) -> bool {
    saves_at_least(head, TRIAL_SAMPLE, 3)
}

/// Does a fast trial compression of the first `cap` bytes save at least
/// `percent`?
fn saves_at_least(head: &[u8], cap: usize, percent: u64) -> bool {
    let sample = &head[..head.len().min(cap)];
    match zstd::bulk::compress(sample, 1) {
        Ok(c) => (c.len() as u64) * 100 < (sample.len() as u64) * (100 - percent),
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

/// Containers whose payload is deflate, and which `crate::deflate` can find
/// streams in. A format listed here that yields no stream just falls back to
/// the ordinary path, so the list may be optimistic — a PDF whose streams are
/// all JPEG images costs one wasted scan and nothing else.
/// JPEG, by the SOI marker plus the first marker byte of a segment. Deliberately
/// the same test `is_precompressed` uses, so the two cannot disagree about a
/// file.
pub fn is_jpeg(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0xD8, 0xFF])
}

pub fn is_deflate_container(b: &[u8]) -> bool {
    b.starts_with(&[0x50, 0x4B, 0x03, 0x04])
        || b.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])
        || (b.len() > 3 && b[0] == 0x1F && b[1] == 0x8B && b[2] == 0x08)
        || b.starts_with(b"%PDF-")
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

    /// Pseudo-random bytes: no structure for any codec to find.
    fn noise(n: usize) -> Vec<u8> {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 24) as u8
            })
            .collect()
    }

    /// 16-bit stereo PCM: two slowly-moving channels plus dither, interleaved.
    /// Compresses without help, which is exactly why it used to be skipped.
    fn pcm_stereo_16(frames: usize) -> Vec<u8> {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut out = Vec::with_capacity(frames * 4);
        for i in 0..frames {
            let t = i as f64;
            let l = (t * 0.013).sin() * 9000.0 + (t * 0.0009).sin() * 4000.0;
            let r = (t * 0.011).sin() * 8500.0 + (t * 0.0007).cos() * 4200.0;
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let dither = ((seed >> 40) as i16) % 24;
            out.extend_from_slice(&(l as i16).wrapping_add(dither).to_le_bytes());
            out.extend_from_slice(&(r as i16).wrapping_sub(dither).to_le_bytes());
        }
        out
    }

    /// THE GATE THAT COST 27%. The record-width detector used to be skipped
    /// whenever the data compressed at all, so 16-bit stereo PCM — which lands
    /// at 82% unfiltered — never met the filter that takes it to 60%. On a real
    /// 518 MB corpus of decoded music that gate was worth 22.3%.
    #[test]
    fn interleaved_pcm_gets_the_record_width_filter() {
        let pcm = pcm_stereo_16(400_000);
        assert!(
            compresses(&pcm),
            "the point of this test is data that clears the old gate"
        );
        for tier in [Tier::Normal, Tier::Max] {
            let p = plan(&pcm, tier);
            assert_eq!(
                p.filter,
                Filter::Delta(4),
                "{tier:?} should difference at the 4-byte frame"
            );
            assert_ne!(p.codec, Codec::Store);
        }
    }

    /// A JPEG is proposed for recompression, not stored on sight. Whether the
    /// transform survives is the pipeline's decision — it round-trips the unit
    /// and falls back to storing when lepton cannot model the file, which is
    /// what happens to this synthetic one.
    #[test]
    fn a_jpeg_is_offered_to_lepton() {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
        jpeg.extend(noise(200_000));
        let p = plan(&jpeg, Tier::Max);
        assert_eq!(p.filter, Filter::Jpeg);
        assert_eq!(p.kind, Class::Jpeg);
        assert_ne!(p.codec, Codec::Store);
    }

    /// The magic is a claim about the format, not about the bytes. Data that
    /// still compresses must not be stored just because it says "JPEG" — that
    /// cost 1.12 MB on a 4.93 MiB corpus of zips and PNGs.
    #[test]
    fn precompressed_magic_does_not_excuse_compressible_bytes() {
        // An ID3 header with no frames behind it: still `Class::Precompressed`
        // (a real MP3 goes to `Class::Mp3` instead), so the test stays about
        // the magic-versus-bytes rule rather than about the JPEG path.
        let mut mp3 = b"ID3   ".to_vec();
        mp3.extend(std::iter::repeat_n(0x5Au8, 200_000));
        let p = plan(&mp3, Tier::Max);
        assert_ne!(p.codec, Codec::Store);
        assert_eq!(p.kind, Class::Precompressed);
    }

    /// An ID3 header followed by noise is not an MP3: there is no frame chain
    /// behind it and no first frame id inside it. A false positive would give a
    /// 200 KB blob a unit of its own for no gain.
    #[test]
    fn id3_shaped_noise_is_stored_verbatim() {
        let mut mp3 = b"ID3   ".to_vec();
        mp3.extend(noise(200_000));
        assert_eq!(plan(&mp3, Tier::Max), Plan::STORE);
    }

    /// A real frame chain does earn the transform, at every tier.
    #[test]
    fn an_mp3_frame_chain_gets_the_plane_filter() {
        let mut mp3 = Vec::new();
        for i in 0..64u8 {
            // 128 kbit/s, 44.1 kHz, joint stereo, no CRC: a 417-byte frame.
            let mut f = vec![0xFF, 0xFB, 0x90, 0x40];
            f.extend(std::iter::repeat_n(i, 413));
            mp3.extend_from_slice(&f);
        }
        for tier in [Tier::Fast, Tier::Normal, Tier::Max] {
            let p = plan(&mp3, tier);
            assert_eq!(p.filter, Filter::Mp3, "{tier:?}");
            assert_eq!(p.kind, Class::Mp3, "{tier:?}");
            assert_ne!(p.codec, Codec::Store, "{tier:?}");
        }
    }

    #[test]
    fn text_goes_to_ppmd_only_at_max() {
        let text = "fn main() { println!(\"hello world\"); }\n".repeat(50);
        assert_eq!(plan(text.as_bytes(), Tier::Max).codec, Codec::Ppmd7);
        assert_eq!(plan(text.as_bytes(), Tier::Normal).codec, Codec::Zstd);
        assert_eq!(plan(text.as_bytes(), Tier::Max).filter, Filter::None);
    }

    /// Machine code gets an x86 filter, and WHICH one is a tier decision: max
    /// moves the branch targets into a stream of their own (measured 3.5%
    /// better on 234 MiB of Windows DLLs), the cheaper tiers patch them in
    /// place because splitting costs a second pass.
    #[test]
    fn executables_get_an_x86_filter() {
        let mut exe = b"MZ\x90\x00\x03\x00\x00\x00".to_vec();
        exe.extend(std::iter::repeat_n(0xE8u8, 500));
        assert_eq!(plan(&exe, Tier::Max).filter, Filter::X86Split);
        assert_eq!(plan(&exe, Tier::Max).codec, Codec::Lzma2);
        assert_eq!(plan(&exe, Tier::Normal).filter, Filter::BcjX86);
        assert_eq!(plan(&exe, Tier::Fast).filter, Filter::BcjX86);
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
