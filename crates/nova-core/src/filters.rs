//! Reversible pre-compression filters: transforms that never shrink data by
//! themselves, but make the codec that follows shrink it much further.
//!
//! - **BCJ x86** rewrites the operand of relative CALL/JMP (`E8`/`E9`) into an
//!   absolute address. Ten calls to the same function are ten *different* byte
//!   strings while the operands are relative, and ten identical ones once they
//!   are absolute — exactly the repetition an LZ matcher lives on. Measured on
//!   whole binaries: 4.6% off zstd-12 and 4.8% off LZMA for this project's own
//!   `nova.exe`, 5.3-5.7% for 4 MiB of `mshtml.dll`; more on code-dense files,
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

use anyhow::{bail, Context, Result};

/// Largest delta distance the manifest byte can encode.
pub const MAX_DELTA_DISTANCE: u8 = 32;

/// Highest id in the DELTA range. This must be split from "highest assigned
/// id": the decode arm is `2..=MAX_DELTA_ID => Delta(id - 1)`, so widening it to
/// cover a new filter would make that filter decode as a delta with a nonsense
/// distance — a silent mis-decode rather than an error.
const MAX_DELTA_ID: u8 = 1 + MAX_DELTA_DISTANCE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// Feed the chunk to the codec unchanged.
    None,
    /// x86 call/jump converter.
    BcjX86,
    /// Byte-difference filter; the payload is the distance in bytes, 1..=32.
    Delta(u8),
    /// Undo the deflate streams inside a container (zip, PNG, gzip, docx…),
    /// keeping the record that rebuilds the original bitstream exactly.
    ///
    /// The only filter that changes length, and the only one that builds a new
    /// buffer instead of mutating in place. Measured on a corpus of zips, PNGs
    /// and gzips: 4,048,085 B of what nova stores today becomes 2,485,916 B,
    /// −38.6%, in a place where 7-Zip can do nothing at all.
    Deflate,
    /// Re-encode a JPEG's quantized DCT coefficients with a context model
    /// instead of its Huffman tables, keeping enough to rebuild the original
    /// file bit for bit — padding bits, restart markers and trailing garbage
    /// included.
    ///
    /// The one transform that helps photographs, which are the bulk of a family
    /// archive and the case every archiver gives up on: measured on this
    /// machine, 7-Zip -mx9 takes 0.78% off a set of camera JPEGs and nova 1.77%,
    /// while lepton takes 15-21% off each file it accepts.
    Jpeg,
    /// Move x86 branch targets into their own stream. See `x86_split_encode`.
    X86Split,
    /// Replace a RIFF/WAVE file's PCM with a FLAC stream, keeping every other
    /// byte of the container so the .wav comes back exactly.
    ///
    /// The one audio transform where the competition has nothing: uncompressed
    /// PCM is stored near raw by every archiver. See `crate::wav` — and note
    /// that this id pins the wrapper and the DECODER, not the encoder, because
    /// what it stores is a standard FLAC stream.
    Wav,
    /// Undo every recompressible stream in a container, deflate and JPEG alike,
    /// with a deflate stream modelled in AS MANY PREFLATE PASSES AS IT TAKES.
    ///
    /// The successor to [`Filter::Container`], which had one blob per stream and
    /// therefore had to skip any stream whose plaintext would not fit the coded
    /// cap whole. `binutils-2.42.tar.gz` is 51,892,456 B of one deflate stream
    /// carrying 319,897,600 B of plaintext: it fitted the solo unit, was refused
    /// by the cap, and came out at 94.6% of itself. Modelled in passes up to the
    /// budget it reaches −32.9%, and the whole stream would be −43.8%.
    ContainerChunked,
    /// Separate an MPEG Layer III file's frame headers and side information
    /// from its spectral data, so the structured 8% stops being interleaved
    /// with the incompressible 92%.
    ///
    /// The one audio format nobody recompresses. See `crate::mp3` — the id pins
    /// the plane layout, not any library, because the MPEG bitstream itself is
    /// copied through byte for byte.
    Mp3,
    /// Undo EVERY recompressible stream in a container, deflate and JPEG alike.
    ///
    /// The successor to [`Filter::Deflate`], which only ever saw deflate. A PDF
    /// carries both: measured on 19 real documents, 72.4% of the bytes are
    /// `/FlateDecode` and **18.5% are `/DCTDecode`** — whole JPEGs that lepton
    /// takes another 20.3% off. Id 34 keeps decoding every archive already
    /// written; only new ones use this.
    Container,
}

/// Filter id for JPEG recompression with lepton 0.5.x.
const JPEG_LEPTON_0_5: u8 = 35;

/// Filter id for x86 call/jump splitting. nova's own transform, so unlike ids
/// 34 and 35 it pins no external library version.
const X86_SPLIT: u8 = 36;

/// Filter id for mixed container recompression: preflate 0.7.x for the deflate
/// streams, lepton 0.5.x for the JPEG ones. It pins BOTH library versions, so
/// an upgrade of either must spend a new id.
const CONTAINER_V2: u8 = 37;

/// Filter id for RIFF/WAVE PCM carried as FLAC. Unlike 34/35/37 this pins a
/// decoder and a wrapper, not a library version: the payload is standard FLAC.
const WAV_FLAC: u8 = 38;

/// Filter id for chunked container recompression: preflate 0.7.x in as many
/// passes as a stream needs, lepton 0.5.x for the JPEG ones. It pins both
/// library versions exactly as 37 does; what it adds is the `NDf3` framing's
/// per-stream pass list, which 37 has no room for.
const CONTAINER_V3: u8 = 40;

/// Filter id for the MPEG Layer III plane split. Pins nothing external — the
/// transform is nova's own and the MPEG bytes pass through unchanged — but it
/// does pin `crate::mp3`'s plane layout, which is why the payload carries its
/// own `NM31` version byte as well.
const MP3_PLANES: u8 = 39;

/// Filter id for deflate recompression with preflate 0.7.x records.
///
/// The version is part of the id's meaning, not a detail: preflate's correction
/// format is not guaranteed stable across releases, so `preflate-rs` is pinned
/// exactly and an upgrade must spend a NEW id rather than change what this one
/// decodes. Old decoders must stay callable forever.
const DEFLATE_PREFLATE_0_7: u8 = 34;

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
    /// garbage. A new id must never be added by widening the delta range.
    ///
    /// A `Delta` distance outside 1..=32 is not representable, so it is
    /// clamped — by `id`, `apply` and `unapply` alike, so the stored byte can
    /// never disagree with the transform that was actually applied.
    pub fn id(self) -> u8 {
        match self {
            Filter::None => 0,
            Filter::BcjX86 => 1,
            Filter::Delta(d) => 1 + clamp_distance(d as usize) as u8,
            Filter::Deflate => DEFLATE_PREFLATE_0_7,
            Filter::Jpeg => JPEG_LEPTON_0_5,
            Filter::X86Split => X86_SPLIT,
            Filter::Container => CONTAINER_V2,
            Filter::Wav => WAV_FLAC,
            Filter::Mp3 => MP3_PLANES,
            Filter::ContainerChunked => CONTAINER_V3,
        }
    }

    pub fn from_id(id: u8) -> Result<Filter> {
        match id {
            0 => Ok(Filter::None),
            1 => Ok(Filter::BcjX86),
            2..=MAX_DELTA_ID => Ok(Filter::Delta(id - 1)),
            DEFLATE_PREFLATE_0_7 => Ok(Filter::Deflate),
            JPEG_LEPTON_0_5 => Ok(Filter::Jpeg),
            X86_SPLIT => Ok(Filter::X86Split),
            CONTAINER_V2 => Ok(Filter::Container),
            WAV_FLAC => Ok(Filter::Wav),
            MP3_PLANES => Ok(Filter::Mp3),
            CONTAINER_V3 => Ok(Filter::ContainerChunked),
            other => bail!("unknown filter id {other} - archive was made by a newer version"),
        }
    }

    /// Transform a chunk before compression.
    ///
    /// Returns what it did, because the caller has to know: on the store
    /// fallback an [`Applied::InPlace`] filter MUST be undone to recover the
    /// original bytes, and a filter that built a new buffer MUST NOT be — the
    /// original is still sitting there untouched. One boolean cannot say both,
    /// and getting it backwards stores the wrong bytes under the right hash.
    pub fn apply(self, data: &mut Vec<u8>) -> Result<Applied> {
        match self {
            Filter::None => Ok(Applied::InPlace),
            Filter::BcjX86 => {
                bcj_x86_encode(data, 0);
                Ok(Applied::InPlace)
            }
            Filter::Delta(d) => {
                delta_encode(data, d as usize);
                Ok(Applied::InPlace)
            }
            Filter::Deflate => {
                *data = deflate_encode(data)?;
                Ok(Applied::Rebuilt)
            }
            Filter::Jpeg => {
                *data = jpeg_encode(data)?;
                Ok(Applied::Rebuilt)
            }
            Filter::X86Split => {
                *data = x86_split_encode(data)?;
                Ok(Applied::Rebuilt)
            }
            Filter::Container => {
                *data = container_encode(data)?;
                Ok(Applied::Rebuilt)
            }
            Filter::ContainerChunked => {
                *data = container_chunked_encode(data)?;
                Ok(Applied::Rebuilt)
            }
            Filter::Wav => {
                *data = crate::wav::encode(data)?;
                Ok(Applied::Rebuilt)
            }
            Filter::Mp3 => {
                *data = crate::mp3::encode(data)?;
                Ok(Applied::Rebuilt)
            }
        }
    }

    /// Undo [`Filter::apply`], after decompression.
    ///
    /// Fallible because a transform that rebuilds data from a record can be
    /// handed a corrupt or hostile one. The in-place transforms cannot fail and
    /// say so by never returning `Err`.
    pub fn unapply(self, data: &mut Vec<u8>) -> Result<()> {
        match self {
            Filter::None => {}
            Filter::BcjX86 => bcj_x86_decode(data, 0),
            Filter::Delta(d) => delta_decode(data, d as usize),
            Filter::Deflate => *data = deflate_decode(data)?,
            Filter::Jpeg => *data = jpeg_decode(data)?,
            Filter::X86Split => *data = x86_split_decode(data)?,
            // One decoder for all three framings: `deflate::decode` reads the
            // magic and hands back a pass list either way.
            Filter::Container | Filter::ContainerChunked => *data = deflate_decode(data)?,
            Filter::Wav => *data = crate::wav::decode(data)?,
            Filter::Mp3 => *data = crate::mp3::decode(data)?,
        }
        Ok(())
    }

    /// Whether this filter can change the length of the buffer. Length-changing
    /// filters are the only ones that need `ChunkRec::filtered`.
    pub fn changes_length(self) -> bool {
        match self {
            Filter::None | Filter::BcjX86 | Filter::Delta(_) => false,
            Filter::Deflate
            | Filter::Jpeg
            | Filter::X86Split
            | Filter::Container
            | Filter::ContainerChunked
            | Filter::Wav
            | Filter::Mp3 => true,
        }
    }
}

/// What [`Filter::apply`] did to the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The buffer was mutated; undoing the filter restores the original.
    InPlace,
    /// The buffer was replaced with a different representation. The original
    /// cannot be recovered by undoing in place — the caller must keep it.
    Rebuilt,
}

/// The lepton settings are a FORMAT CONSTANT for filter id 35, exactly like
/// PPMd7's order and pool size: they decide the bitstream, and none of them is
/// stored per unit. `compat_lepton_vector_write` is the conservative preset —
/// it is what the C++ implementation can also read, and its 16386-pixel limit
/// simply means larger images are not transformed.
fn lepton_features() -> lepton_jpeg::EnabledFeatures {
    lepton_jpeg::EnabledFeatures::compat_lepton_vector_write()
}

/// Undo a JPEG's entropy coding. One thread: nova already runs a worker per
/// unit, and a nested pool would multiply both threads and memory.
fn jpeg_encode(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Cursor;
    let mut out = Vec::with_capacity(data.len());
    lepton_jpeg::encode_lepton(
        &mut Cursor::new(data),
        &mut Cursor::new(&mut out),
        &lepton_features(),
        &lepton_jpeg::SingleThreadPool {},
    )
    .map_err(|e| anyhow::anyhow!("lepton cannot model this jpeg: {e}"))?;
    Ok(out)
}

/// Rebuild the original JPEG, byte for byte.
fn jpeg_decode(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Cursor;
    let mut out = Vec::new();
    lepton_jpeg::decode_lepton(
        &mut Cursor::new(data),
        &mut out,
        &lepton_features(),
        &lepton_jpeg::SingleThreadPool {},
    )
    .map_err(|e| anyhow::anyhow!("cannot rebuild a jpeg: {e}"))?;
    Ok(out)
}

/// Undo every deflate stream in the buffer, and lay the pieces out so they
/// compress: container bytes, then all plaintexts, then all correction records.
///
/// A stream that preflate refuses is simply left as it is — the transform is
/// per stream, and a container full of streams it cannot model still gains from
/// the ones it can. The coded-size cap is enforced the same way: a stream whose
/// growth would cross it is skipped, not the whole container's transform.
fn deflate_encode(data: &[u8]) -> Result<Vec<u8>> {
    container_encode_inner(
        data,
        crate::deflate::Ver::V1,
        crate::archive::MAX_CODED_CHUNK as usize,
    )
}

/// The v2 path: the same framing, but JPEG streams are transformed too.
fn container_encode(data: &[u8]) -> Result<Vec<u8>> {
    container_encode_inner(
        data,
        crate::deflate::Ver::V2,
        crate::archive::MAX_CODED_CHUNK as usize,
    )
}

/// The v3 path: as v2, but a deflate stream may be modelled in SEVERAL preflate
/// passes, and a stream that runs out of budget contributes its prefix instead
/// of nothing at all.
fn container_chunked_encode(data: &[u8]) -> Result<Vec<u8>> {
    container_encode_inner(
        data,
        crate::deflate::Ver::V3,
        crate::archive::MAX_CODED_CHUNK as usize,
    )
}

/// Plaintext one preflate pass may hold.
///
/// NOT MERELY A MEMORY KNOB, which is what it looked like. The parser stops at
/// this limit wherever it happens to be, and if that is mid-block it fails with
/// `PlainTextLimit` instead of ending the pass cleanly — so the value has to
/// exceed the largest single deflate block's plaintext or the walk stops early.
/// Measured on `binutils-2.42.tar.gz`: at 256 KiB and 1 MiB the walk dies after
/// reaching 4.7% of the stream, at 4 MiB it reaches 12.5% of the same budget,
/// and at 32 MiB one pass covers 14.8%. Bigger passes are strictly better here,
/// bounded only by the peak plaintext a worker then holds.
///
/// It also has to be SET rather than defaulted: `PreflateConfig::default()`
/// puts it at 128 MiB, below `MAX_CODED_CHUNK`, so it used to bite first — and
/// silently, because the stream simply came back unmodelled.
///
/// When it does bite, the walk keeps the passes it already had and the stream
/// contributes its prefix. That degradation is verified, not assumed: the
/// collected prefix replays byte-for-byte even when the pass that followed it
/// failed.
const PREFLATE_PASS: usize = 32 * 1024 * 1024;

/// The passes of one stream: each pass's plaintext and its correction record.
type Passes = Vec<(Vec<u8>, Vec<u8>)>;

/// One deflate stream, modelled in as many passes as the budget allows.
///
/// Returns the passes and how many COMPRESSED bytes they cover. That second
/// number is the point: it may be less than the stream, and then the caller
/// keeps only the prefix and lets the container store the tail verbatim. A
/// 51.9 MB `.tar.gz` whose plaintext is 305 MiB cannot be modelled whole inside
/// the coded cap, and used to be skipped entirely; measured, modelling as much
/// as ~192 MiB of plaintext takes LZMA2 from 49,093,075 B to 32,928,672
/// (−32.9%), against −43.8% for the whole stream.
/// `pass` is a parameter for the same reason `cap` is one on
/// `container_encode_inner`: a test can shrink it and exercise the multi-pass
/// path on a few hundred kilobytes instead of the three hundred megabytes the
/// real constant would need.
fn preflate_chunks(raw: &[u8], budget: usize, pass: usize) -> Option<(Passes, usize)> {
    let cfg = preflate_rs::PreflateConfig {
        verify_compression: false,
        plain_text_limit: pass,
        ..Default::default()
    };
    let mut proc = preflate_rs::PreflateStreamProcessor::new(&cfg);
    let (mut consumed, mut plain_total) = (0usize, 0usize);
    let mut chunks: Passes = Vec::new();
    // A failed pass ends the walk and keeps what came before it: the stream
    // then contributes its prefix. That is not a fallback bolted on — it is the
    // same path a budget exhaustion takes, and the prefix is byte-exact either
    // way, which the tests below pin down.
    while let Ok(r) = proc.decompress(&raw[consumed..]) {
        // Without forward progress the next call would repeat this one forever.
        if r.compressed_size == 0 {
            break;
        }
        // `PlainText::text()` skips the retained dictionary, so the passes
        // concatenate into the original plaintext with nothing duplicated.
        let plain = proc.plain_text().text().to_vec();
        // STOP BEFORE CROSSING THE BUDGET, not after. Stopping at `>= budget`
        // overshoots by up to one pass, and the caller's cap check then threw
        // away the WHOLE stream rather than the offending pass — which is
        // exactly how a 51.9 MB tar.gz still came out untransformed after the
        // walk itself was already working. A pass that would cross is dropped
        // whole; the work is wasted, the correctness is not.
        let cost = plain.len().saturating_add(r.corrections.len());
        if plain_total.saturating_add(cost) > budget {
            break;
        }
        plain_total += cost;
        consumed += r.compressed_size;
        chunks.push((plain, r.corrections));
        if proc.is_done() {
            break;
        }
        proc.shrink_to_dictionary();
    }
    if chunks.is_empty() || consumed == 0 {
        return None;
    }
    Some((chunks, consumed))
}

/// `cap` is a parameter rather than reading `MAX_CODED_CHUNK` directly so a
/// test can shrink it and exercise the per-stream skip on ordinary-sized data
/// instead of needing hundreds of MiB to cross the real bound.
fn container_encode_inner(data: &[u8], ver: crate::deflate::Ver, cap: usize) -> Result<Vec<u8>> {
    use crate::deflate::{encode, find_streams, trim, Kind, Piece, Ver};
    let mut streams = find_streams(data);
    // Id 34's framing has no room to say what a stream is, so it may only ever
    // carry deflate. Filtering here rather than in the scanner keeps the two
    // ids producing byte-identical output on containers that hold no JPEG.
    if ver == Ver::V1 {
        streams.retain(|s| s.kind == Kind::Deflate);
    }
    if streams.is_empty() {
        bail!("no recompressible streams to undo");
    }
    // Undoing deflate is the one transform that can multiply its input, and a
    // zip bomb multiplies it by a thousand. The result has to stay inside the
    // bound the decoder enforces, or nova would write archives it refuses to
    // read; and the packing memory budget charges for the unit, not for what a
    // filter might grow it into.
    //
    // preflate verifies each stream itself; the caller then round-trips the
    // whole buffer, which also covers this module's framing.
    let cfg = preflate_rs::PreflateConfig {
        verify_compression: false,
        ..Default::default()
    };
    let mut parts = Vec::new();
    // Charged as the pieces are built, not summed at the end: a PDF of ordinary
    // technical prose recovers SIX times its own size in plaintext, out of
    // hundreds of streams. Checking only after the loop would hold every one of
    // them first, so the bound that exists to stop a bomb would be reached by
    // allocating exactly what it forbids.
    //
    // Seeded with the container's own length, because `encode` emits the bytes
    // no stream covers as well as the plaintexts, and a budget that forgets
    // them lets the transformed form reach `cap + data.len()`. That is not a
    // rounding error: the read side refuses a coded length above the cap, so
    // the overrun is an archive nova writes and then cannot extract.
    //
    // Checked PER STREAM, not summed and judged once at the end: a unit is one
    // container that may hold hundreds of independent streams (a zip, a PDF),
    // and one outsized member must not cancel every other stream's gain. A
    // rejected stream's growth is never folded into `total`, so it does not
    // poison the streams that come after it.
    let mut total = data.len();
    for s in &streams {
        let raw = s.gather(data);
        // A stream either transforms or is left where it is. One refusal is a
        // lost opportunity for that stream, never an error for the container.
        let (chunks, covered) = match s.kind {
            Kind::Deflate if ver == Ver::V3 => {
                // What this stream may spend: whatever the cap has left, less
                // the corrections it will also have to carry. The passes stop
                // at the budget, so an oversized stream contributes its PREFIX
                // rather than being skipped whole.
                let budget = cap.saturating_sub(total);
                if budget == 0 {
                    continue;
                }
                match preflate_chunks(&raw, budget, PREFLATE_PASS) {
                    Some(v) => v,
                    None => continue,
                }
            }
            Kind::Deflate => {
                let Ok((res, plain)) = preflate_rs::preflate_whole_deflate_stream(&raw, &cfg)
                else {
                    continue;
                };
                if res.compressed_size != raw.len() {
                    continue;
                }
                let n = raw.len();
                (vec![(plain.text().to_vec(), res.corrections)], n)
            }
            Kind::Jpeg => match jpeg_encode(&raw) {
                // Lepton output that is not smaller than the JPEG is not worth
                // a record: the container would grow for nothing.
                Ok(blob) if blob.len() < raw.len() => (vec![(blob, Vec::new())], raw.len()),
                _ => continue,
            },
        };
        // The covered compressed bytes are REPLACED by the plaintext, not added
        // to it — `encode` writes them nowhere — so they come back off the
        // budget. `total` was seeded with the container's whole length, which
        // includes them.
        let grown = chunks
            .iter()
            .fold(total.saturating_sub(covered), |acc, (plain, corr)| {
                acc.saturating_add(plain.len()).saturating_add(corr.len())
            });
        if grown > cap {
            continue;
        }
        // A partly modelled stream keeps only the prefix its passes actually
        // reproduce; the compressed tail past that stays where it is and the
        // container writes it out verbatim, so the rebuild is still exact.
        let mut pieces = s.pieces.clone();
        let whole: usize = pieces.iter().map(|p| p.1).sum();
        if covered < whole {
            trim(&mut pieces, 0, whole - covered);
            if pieces.is_empty() {
                continue;
            }
        }
        total = grown;
        parts.push(Piece {
            pieces,
            kind: s.kind,
            chunks,
        });
    }
    if parts.is_empty() {
        bail!("nothing in this unit could be modelled");
    }
    encode(data, &parts, ver)
}

/// Rebuild the container exactly as it was.
fn deflate_decode(data: &[u8]) -> Result<Vec<u8>> {
    use crate::deflate::Kind;
    let d = crate::deflate::decode(data)?;
    let mut rebuilt = Vec::with_capacity(d.streams.len());
    for s in &d.streams {
        rebuilt.push(match s.kind {
            Kind::Deflate if s.chunks.len() > 1 => {
                // Several passes: replay them in order through ONE processor,
                // because each continues the previous one's predictor. The
                // concatenation is the stream's compressed bytes.
                let mut rec = preflate_rs::RecreateStreamProcessor::new();
                let mut out = Vec::new();
                for (plain, corrections) in &s.chunks {
                    let (bytes, _) = rec
                        .recompress(&mut std::io::Cursor::new(plain), corrections)
                        .map_err(|e| anyhow::anyhow!("cannot rebuild a deflate stream: {e}"))?;
                    out.extend_from_slice(&bytes);
                }
                out
            }
            Kind::Deflate => {
                // Exactly one pass, which is what every id 34 and 37 payload
                // ever written contains, and what most v3 streams contain too.
                let (plain, corrections) = s.chunks[0];
                preflate_rs::recreate_whole_deflate_stream(plain, corrections)
                    .map_err(|e| anyhow::anyhow!("cannot rebuild a deflate stream: {e}"))?
            }
            // A JPEG is one lepton blob and never several: `container_encode_inner`
            // only ever pushes a single chunk for it, and the decoder says so
            // rather than silently reading the first of many.
            Kind::Jpeg => {
                if s.chunks.len() != 1 {
                    bail!("a jpeg stream cannot have {} passes", s.chunks.len());
                }
                jpeg_decode(s.chunks[0].0)?
            }
        });
    }
    crate::deflate::rebuild(&d, &rebuilt)
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

// -- x86 call/jump splitting (filter id 36) ----------------------------------

/// Magic for the split form, so a truncated payload is refused before it can
/// drive an allocation.
const X86_MAGIC: &[u8; 4] = b"NX86";

/// Is there a call/jump with a 32-bit displacement at `i`? Returns the offset
/// of the displacement field within the instruction (1 for E8/E9, 2 for the
/// two-byte conditional jumps).
///
/// Deliberately says nothing about whether the displacement itself fits: the
/// decoder walks a stream those four bytes have been REMOVED from, so a test
/// that looked at the remaining length would disagree with the encoder about
/// the last instruction in the buffer. The site count in the header settles
/// that instead.
fn x86_site(buf: &[u8], i: usize) -> Option<usize> {
    let b = *buf.get(i)?;
    if b == 0xE8 || b == 0xE9 {
        Some(1)
    } else if b == 0x0F && matches!(buf.get(i + 1), Some(0x80..=0x8F)) {
        Some(2)
    } else {
        None
    }
}

/// Move x86 branch targets out of the code and into their own stream.
///
/// The in-place BCJ filter rewrites each displacement to an absolute address
/// where it stands, which helps — but the four address bytes still sit between
/// the instructions, and a match finder walking the code keeps tripping over
/// them. MEASURED on 234.3 MiB of Firefox DLLs with LZMA2 -9e at nova's own
/// 64 MiB units: in-place BCJ 71,410,491 B, this split 68,893,556 B (-3.5%).
///
/// This is 7-Zip's BCJ2 idea without its machinery. BCJ2 decides each site with
/// a range-coded probability model and writes four streams; here the decision is
/// the classical "does the absolute target land inside this buffer" test and the
/// answers are one plain bit each, which LZMA2 then compresses to a third of
/// their size. Two rules that sound better both measured worse: liblzma's
/// position-independent top-byte test accepts almost nothing at a 64 MiB unit
/// (72,488,469 B, worse than not splitting at all), and gating on the
/// displacement magnitude instead lost 0.6-3.2% at every reach from 1 to 16 MiB.
fn x86_split_encode(data: &[u8]) -> Result<Vec<u8>> {
    let mut main = Vec::with_capacity(data.len());
    let mut targets: Vec<u8> = Vec::new();
    let mut flags: Vec<u8> = Vec::new();
    let (mut acc, mut bit) = (0u8, 0u32);
    let mut sites = 0u64;
    let mut i = 0usize;
    while i < data.len() {
        let Some(off) = x86_site(data, i).filter(|off| i + off + 4 <= data.len()) else {
            main.push(data[i]);
            i += 1;
            continue;
        };
        sites += 1;
        let at = i + off;
        let rel = i32::from_le_bytes(data[at..at + 4].try_into().expect("checked by x86_site"));
        // The address is relative to this buffer, never to the file: a
        // position-dependent transform would make identical bytes compress to
        // different payloads and dedup would stop working.
        let absolute = rel as i64 + at as i64 + 4;
        let take = absolute >= 0 && absolute < data.len() as i64;
        acc |= (take as u8) << bit;
        bit += 1;
        if bit == 8 {
            flags.push(acc);
            acc = 0;
            bit = 0;
        }
        main.extend_from_slice(&data[i..at]);
        if take {
            // Big-endian, so the high bytes of nearby targets repeat.
            targets.extend_from_slice(&(absolute as u32).to_be_bytes());
        } else {
            main.extend_from_slice(&data[at..at + 4]);
        }
        i = at + 4;
    }
    if bit > 0 {
        flags.push(acc);
    }
    if targets.is_empty() {
        bail!("no x86 branch targets to move");
    }
    let mut out = Vec::with_capacity(data.len() + 32);
    out.extend_from_slice(X86_MAGIC);
    for n in [
        main.len() as u64,
        targets.len() as u64,
        flags.len() as u64,
        sites,
    ] {
        out.extend_from_slice(&n.to_le_bytes());
    }
    out.extend_from_slice(&main);
    out.extend_from_slice(&targets);
    out.extend_from_slice(&flags);
    Ok(out)
}

/// Put the branch targets back. Every length is checked against the buffer
/// before anything is allocated: on this path the input is untrusted.
fn x86_split_decode(data: &[u8]) -> Result<Vec<u8>> {
    const HEAD: usize = 4 + 8 * 4;
    if data.len() < HEAD || &data[..4] != X86_MAGIC {
        bail!("not an x86-split payload");
    }
    let mut n = [0usize; 4];
    for (k, slot) in n.iter_mut().enumerate() {
        let at = 4 + k * 8;
        *slot = u64::from_le_bytes(data[at..at + 8].try_into().expect("fixed width")) as usize;
    }
    let (main_len, targets_len, flags_len, sites) = (n[0], n[1], n[2], n[3]);
    if sites > main_len
        || flags_len != sites.div_ceil(8)
        || targets_len % 4 != 0
        || main_len
            .checked_add(targets_len)
            .and_then(|s| s.checked_add(flags_len))
            .is_none_or(|s| s != data.len() - HEAD)
    {
        bail!("x86-split payload does not match its header");
    }
    let main = &data[HEAD..HEAD + main_len];
    let targets = &data[HEAD + main_len..HEAD + main_len + targets_len];
    let flags = &data[HEAD + main_len + targets_len..];

    let mut out = Vec::with_capacity(main_len + targets_len);
    let (mut t, mut fi, mut bit) = (0usize, 0usize, 0u32);
    let mut seen = 0usize;
    let mut j = 0usize;
    while j < main.len() {
        // The site count is what keeps the two sides in step at the tail, where
        // the encoder may have found one opcode fewer than this scan does.
        let Some(off) = x86_site(main, j).filter(|_| seen < sites) else {
            out.push(main[j]);
            j += 1;
            continue;
        };
        seen += 1;
        let flag = *flags.get(fi).context("x86-split flags ran out")?;
        let take = (flag >> bit) & 1 == 1;
        bit += 1;
        if bit == 8 {
            fi += 1;
            bit = 0;
        }
        out.extend_from_slice(&main[j..j + off]);
        j += off;
        if take {
            if t + 4 > targets.len() {
                bail!("x86-split targets ran out");
            }
            let absolute =
                u32::from_be_bytes(targets[t..t + 4].try_into().expect("fixed width")) as i64;
            t += 4;
            let rel = (absolute - out.len() as i64 - 4) as i32;
            out.extend_from_slice(&rel.to_le_bytes());
        } else {
            if j + 4 > main.len() {
                bail!("x86-split main stream ran out");
            }
            out.extend_from_slice(&main[j..j + 4]);
            j += 4;
        }
    }
    if t != targets.len() || seen != sites {
        bail!(
            "x86-split used {t} of {} target bytes and {seen} of {sites} sites",
            targets.len()
        );
    }
    Ok(out)
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

    /// Uniform and periodic content, which a random generator essentially
    /// never produces and which real archives are full of: zero padding, `FF`
    /// erase patterns, and instruction sequences laid out so that every
    /// candidate overlaps the previous one's operand.
    fn degenerate_bufs(len: usize) -> Vec<Vec<u8>> {
        fn cycle(pattern: &[u8], len: usize) -> Vec<u8> {
            (0..len).map(|i| pattern[i % pattern.len()]).collect()
        }
        vec![
            vec![0x00; len],
            vec![0xFF; len],
            vec![0xE8; len],
            vec![0xE9; len],
            cycle(&[0xE8, 0xFE, 0x00, 0x00, 0x00], len),
            cycle(&[0xE8, 0x01, 0xFF, 0xFF, 0xFF], len),
            cycle(&[0xE8, 0x00, 0xFF, 0xE9], len),
            cycle(&[0xE8, 0xE9], len),
        ]
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

    /// Real machine code to filter: `target/nova.exe` when the CLI happens to
    /// be built, otherwise this test binary, which always exists.
    ///
    /// The fallback is not a nicety. `cargo test -p nova-core` does not build
    /// nova-cli, so on a fresh checkout `target/` holds no `nova` at all — and
    /// a source that can come back empty turns every test below into a silent
    /// no-op, which is the one way a filter test can be worse than no test.
    fn machine_code() -> Vec<u8> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for name in [
            "release/nova.exe",
            "debug/nova.exe",
            "release/nova",
            "debug/nova",
        ] {
            if let Ok(bytes) = std::fs::read(root.join("target").join(name)) {
                if bytes.len() > 64 * 1024 {
                    return bytes;
                }
            }
        }
        let self_path = std::env::current_exe().expect("no path to the test binary");
        std::fs::read(&self_path).expect("cannot read the test binary")
    }

    /// One chunk's worth of a real executable, which is what the filter would
    /// actually be handed.
    fn exe_sample() -> Vec<u8> {
        let bytes = machine_code();
        bytes[..bytes.len().min(MAX_CHUNK)].to_vec()
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
        // 34 is deflate recompression, and the point of this line is that it is
        // NOT a delta: the decode arm is `2..=MAX_DELTA_ID => Delta(id - 1)`, so
        // adding a filter by widening that range would silently turn id 34 into
        // Delta(33) and mis-decode every archive that used it.
        assert_eq!(Filter::from_id(34).unwrap(), Filter::Deflate);
        assert_eq!(Filter::Deflate.id(), 34);
        assert_eq!(Filter::from_id(35).unwrap(), Filter::Jpeg);
        assert_eq!(Filter::Jpeg.id(), 35);
        assert_eq!(Filter::from_id(36).unwrap(), Filter::X86Split);
        assert_eq!(Filter::X86Split.id(), 36);
        assert_eq!(Filter::from_id(37).unwrap(), Filter::Container);
        assert_eq!(Filter::Container.id(), 37);
        assert_eq!(Filter::from_id(38).unwrap(), Filter::Wav);
        assert_eq!(Filter::Wav.id(), 38);
        assert_eq!(Filter::from_id(39).unwrap(), Filter::Mp3);
        assert_eq!(Filter::Mp3.id(), 39);
        assert_eq!(Filter::from_id(40).unwrap(), Filter::ContainerChunked);
        assert_eq!(Filter::ContainerChunked.id(), 40);
        for id in 41..=255u8 {
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
        Filter::Delta(0).apply(&mut a).unwrap();
        Filter::Delta(1).apply(&mut b).unwrap();
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
                        filter.apply(&mut data).unwrap();
                        filter.unapply(&mut data).unwrap();
                        assert_eq!(data, original, "{filter:?} on {len} bytes, seed {seed}");
                    }
                }
            }
        }
    }

    /// The same property on content a random generator cannot reach, at the
    /// sizes that bracket a full chunk.
    #[test]
    fn every_filter_round_trips_degenerate_data() {
        for filter in all_filters() {
            // A full chunk of all 34 filters would dominate a debug test run,
            // and delta behaves the same at every size, so only the BCJ state
            // machine and one delta pay for the big buffers.
            let mut lens = vec![0, 1, 4, 5, 6, 9, 31, 32, 33, 4096];
            if matches!(filter, Filter::BcjX86 | Filter::Delta(1)) {
                lens.extend([MAX_CHUNK - 1, MAX_CHUNK]);
            }
            for len in lens {
                for original in degenerate_bufs(len) {
                    let mut data = original.clone();
                    filter.apply(&mut data).unwrap();
                    filter.unapply(&mut data).unwrap();
                    // Not assert_eq!: a mismatch would dump four megabytes.
                    assert!(
                        data == original,
                        "{filter:?} on {len} bytes of {:02X?}...",
                        &original[..original.len().min(6)]
                    );
                }
            }
        }
    }

    /// `unapply` runs on bytes that came out of an untrusted archive, so it is
    /// reached with input no encoder ever produced. It must terminate — the
    /// conversion has a retry loop — and it must not panic near the end of the
    /// buffer. Re-encoding afterwards also has to give the bytes back, which
    /// is the strongest statement that the two directions walk the same
    /// positions.
    #[test]
    fn bcj_decode_survives_bytes_it_never_produced() {
        for len in [0, 1, 4, 5, 6, 7, 8, 9, 17, 100, 4096, 20_000] {
            let mut inputs = degenerate_bufs(len);
            inputs.push(random_buf(len as u64, len));
            inputs.push(codeish_buf(len as u64 ^ 0xA5, len));
            for original in inputs {
                for offset in [0u32, 1, 2, 3, 5, 255, 0x1_0000, u32::MAX - 1, u32::MAX] {
                    let mut data = original.clone();
                    bcj_x86_decode(&mut data, offset);
                    bcj_x86_encode(&mut data, offset);
                    assert_eq!(data, original, "{len} bytes at offset {offset}");
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
    ///
    /// Checked at several `start_offset`s, not just the 0 the pipeline uses:
    /// the offset feeds the address arithmetic and the retry loop, so a port
    /// that is only right at 0 is right by accident. Lengths above the
    /// reference's internal 4 KiB buffer also make it filter in several
    /// passes, which must still match this one-shot implementation.
    #[test]
    fn bcj_matches_reference_implementation() {
        use lzma_rust2::filter::bcj::{BcjReader, BcjWriter};

        let exe = exe_sample();
        let inputs = [
            codeish_buf(1, 5000),
            random_buf(2, 5000),
            codeish_buf(3, 9),
            exe[..exe.len().min(1 << 20)].to_vec(),
        ];
        for input in inputs {
            for offset in [0u32, 1, 3, 5, 4096, 0x0100_0000, u32::MAX - 4, u32::MAX] {
                let mut mine = input.clone();
                bcj_x86_encode(&mut mine, offset);

                let mut writer = BcjWriter::new_x86(Vec::new(), offset as usize);
                writer.write_all(&input).unwrap();
                let reference = writer.finish().unwrap();
                assert_eq!(
                    mine.len(),
                    reference.len(),
                    "encoder differs from lzma-rust2 at offset {offset}"
                );
                assert_eq!(
                    mine.iter().zip(&reference).position(|(a, b)| a != b),
                    None,
                    "encoder differs from lzma-rust2 at offset {offset}"
                );

                // And the reference decoder must accept what we produced.
                let mut back = Vec::new();
                std::io::copy(
                    &mut BcjReader::new_x86(std::io::Cursor::new(&mine), offset as usize),
                    &mut back,
                )
                .unwrap();
                assert_eq!(back, input, "lzma-rust2 cannot undo our filter");
            }
        }
    }

    #[test]
    fn bcj_round_trips_a_real_executable() {
        let exe = machine_code();
        let mut converted = 0usize;
        // Filter it the way the pipeline would: one 4 MiB chunk at a time.
        for chunk in exe.chunks(MAX_CHUNK) {
            let mut data = chunk.to_vec();
            bcj_x86_encode(&mut data, 0);
            converted += usize::from(data != chunk);
            bcj_x86_decode(&mut data, 0);
            assert_eq!(data, chunk);
        }
        // Only the file as a whole is guaranteed to contain a convertible
        // instruction: a trailing chunk can legitimately be padding, a
        // signature blob or a relocation table with no E8/E9 in it.
        assert!(converted > 0, "BCJ found nothing to convert in a binary");
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
    ///
    /// Gated on the host architecture because the sample is a binary produced
    /// by this build; an x86 filter has nothing to say about ARM code.
    #[test]
    #[cfg_attr(
        not(any(target_arch = "x86", target_arch = "x86_64")),
        ignore = "needs a host binary of x86 machine code"
    )]
    fn bcj_improves_executable_compression() {
        let sample = exe_sample();
        let mut filtered = sample.clone();
        bcj_x86_encode(&mut filtered, 0);

        let plain_zstd = zstd::bulk::compress(&sample, 12).unwrap().len();
        let bcj_zstd = zstd::bulk::compress(&filtered, 12).unwrap().len();
        let plain_lzma = lzma_len(&sample);
        let bcj_lzma = lzma_len(&filtered);
        let gain = |before: usize, after: usize| 100.0 - after as f64 * 100.0 / before as f64;
        eprintln!(
            "BCJ x86 on {} KiB of machine code: zstd-12 {plain_zstd} -> {bcj_zstd} ({:+.2}%), \
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

    /// The coded-size cap used to be judged on the SUM of every stream in the
    /// container, so one oversized member made the whole transform `bail!`,
    /// discarding every stream's gain including the ones already comfortably
    /// under the cap. It must be judged per stream: an outsized member is
    /// skipped, its neighbours are not — the real case is a PDF or a zip with
    /// one implausibly large member among many ordinary ones.
    #[test]
    fn an_oversized_stream_does_not_cancel_its_neighbours() {
        fn zlib_stream(body: &[u8]) -> Vec<u8> {
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(9));
            z.write_all(body).unwrap();
            z.finish().unwrap()
        }
        fn pdf_obj(num: u32, body: &[u8]) -> Vec<u8> {
            let mut o = format!(
                "{num} 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
                body.len()
            )
            .into_bytes();
            o.extend_from_slice(body);
            o.extend_from_slice(b"\nendstream\nendobj\n");
            o
        }
        // Compressible text, so preflate has something real to model rather
        // than a stream it refuses. A unique tail per object keeps the
        // compressed form above MIN_STREAM: pure repetition of one pattern
        // can fold below the 64-byte floor and the scanner would drop it
        // before this function ever sees it.
        fn text(n: usize, tag: u32) -> Vec<u8> {
            const PANGRAM: &[u8] = b"the quick brown fox jumps over the lazy dog ";
            let mut v: Vec<u8> = (0..n).map(|i| PANGRAM[i % PANGRAM.len()]).collect();
            v.extend_from_slice(format!("unique tail {tag}").as_bytes());
            v
        }

        let mut pdf = b"%PDF-1.7\n".to_vec();
        for i in 1..=4u32 {
            pdf.extend(pdf_obj(i, &zlib_stream(&text(3_000, i))));
        }
        let big = pdf_obj(5, &zlib_stream(&text(100_000, 5)));
        pdf.extend_from_slice(&big);
        assert_eq!(
            crate::deflate::find_streams(&pdf).len(),
            5,
            "test setup: the scanner must see all five streams"
        );

        // Comfortably fits the container plus the four small streams' ~3,000
        // bytes of plain text each (~12,000 total), but not the fifth
        // stream's 100,000-byte plain text on top of that.
        let cap = pdf.len() + 20_000;

        let encoded = container_encode_inner(&pdf, crate::deflate::Ver::V1, cap)
            .expect("the four small streams must still transform");
        let decoded = crate::deflate::decode(&encoded).unwrap();
        assert_eq!(
            decoded.streams.len(),
            4,
            "the oversized stream must be skipped, not the whole container"
        );
        assert_eq!(deflate_decode(&encoded).unwrap(), pdf);
    }
}

/// Guess the record width of structured binary data.
///
/// Tables of fixed-width records — database rows, star catalogues, 16-bit
/// audio, arrays of floats, vertex buffers — look like noise to an LZ matcher
/// because no byte sequence repeats, yet consecutive records differ only
/// slightly *column by column*. Subtracting the value one record back turns
/// those columns into runs of near-zero bytes, which every codec compresses
/// well. The width is not declared anywhere, so it has to be inferred.
///
/// The estimator compares the order-0 entropy of the differenced stream for
/// each candidate width against the raw stream and returns the best width, or
/// `None` when differencing does not clearly help. It reads a sample, not the
/// whole input: this runs during analysis, before anything is compressed.
pub fn detect_delta_stride(sample: &[u8]) -> Option<u8> {
    // Too short to tell signal from noise.
    if sample.len() < 4096 {
        return None;
    }
    let raw = entropy_bits(sample, 0);
    let mut best = (raw, 0u8);
    for d in 1..=MAX_DELTA_DISTANCE {
        let e = entropy_bits(sample, d as usize);
        if e < best.0 {
            best = (e, d);
        }
    }
    // Require a real margin: a 3% entropy drop is worth a filter byte, noise
    // in the third decimal is not. Whatever this returns is only a proposal —
    // the packer still stores the chunk raw if compression does not pay.
    (best.1 > 0 && best.0 < raw * 0.97).then_some(best.1)
}

/// Order-0 entropy, in bits per byte, of the stream differenced at `distance`
/// (0 = the raw bytes).
fn entropy_bits(data: &[u8], distance: usize) -> f64 {
    let mut hist = [0u32; 256];
    if distance == 0 {
        for &b in data {
            hist[b as usize] += 1;
        }
    } else {
        if data.len() <= distance {
            return f64::MAX;
        }
        for i in distance..data.len() {
            hist[data[i].wrapping_sub(data[i - distance]) as usize] += 1;
        }
    }
    let total: u32 = hist.iter().sum();
    if total == 0 {
        return f64::MAX;
    }
    let total = total as f64;
    -hist
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / total;
            p * p.log2()
        })
        .sum::<f64>()
}

#[cfg(test)]
mod stride_tests {
    use super::*;

    #[test]
    fn finds_the_record_width_of_a_table() {
        // 12-byte records: a counter, a near-constant, and a slow ramp.
        let mut data = Vec::new();
        for i in 0..4000u32 {
            data.extend_from_slice(&i.to_le_bytes());
            data.extend_from_slice(&(1_000_000u32 + i % 7).to_le_bytes());
            data.extend_from_slice(&(i * 3).to_le_bytes());
        }
        assert_eq!(detect_delta_stride(&data), Some(12));
    }

    #[test]
    fn finds_the_frame_width_of_16_bit_stereo_audio() {
        // A random walk that actually roams, the way a waveform does: a signal
        // that stays near zero has low entropy to begin with and needs no
        // filter, which is a different case (covered by the noise test).
        let mut data = Vec::new();
        let (mut l, mut r) = (0i16, 0i16);
        let mut seed = 0x2545F4914F6CDD1Du64;
        for _ in 0..20000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            l = l.wrapping_add((seed >> 40) as i16 / 64);
            r = r.wrapping_add((seed >> 24) as i16 / 64);
            data.extend_from_slice(&l.to_le_bytes());
            data.extend_from_slice(&r.to_le_bytes());
        }
        // Four bytes is one stereo frame; two would also reduce entropy, so
        // accept either as long as the estimator sees the structure.
        let d = detect_delta_stride(&data).expect("audio frames are structured");
        assert!(d == 4 || d == 2, "unexpected stride {d}");
    }

    #[test]
    fn declines_on_text_and_on_noise() {
        let text = "the quick brown fox jumps over the lazy dog. ".repeat(200);
        assert_eq!(detect_delta_stride(text.as_bytes()), None);

        let mut seed = 0x9E3779B97F4A7C15u64;
        let noise: Vec<u8> = (0..8192)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 33) as u8
            })
            .collect();
        assert_eq!(detect_delta_stride(&noise), None);
    }

    #[test]
    fn round_trips_at_the_detected_stride() {
        let mut data = Vec::new();
        for i in 0..3000u32 {
            data.extend_from_slice(&i.to_le_bytes());
            data.extend_from_slice(&(i / 2).to_le_bytes());
        }
        let stride = detect_delta_stride(&data).expect("a table has a width");
        let f = Filter::delta(stride).unwrap();
        let mut work = data.clone();
        f.apply(&mut work).unwrap();
        assert_ne!(work, data, "the filter should change something");
        f.unapply(&mut work).unwrap();
        assert_eq!(work, data);
    }
}

#[cfg(test)]
mod x86_split_tests {
    use super::*;

    /// Machine-code-shaped bytes: instructions with real branch targets, some
    /// data that merely contains 0xE8, and displacements that point outside the
    /// buffer so both sides of the decision are exercised.
    fn code(len: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(len);
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        while v.len() < len {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            match seed % 5 {
                0 => {
                    // call to somewhere inside the buffer
                    let here = v.len() as i64;
                    let target = (seed >> 8) as i64 % len.max(1) as i64;
                    v.push(0xE8);
                    v.extend_from_slice(&((target - here - 5) as i32).to_le_bytes());
                }
                1 => {
                    // conditional jump, two-byte opcode
                    let here = v.len() as i64;
                    let target = (seed >> 16) as i64 % len.max(1) as i64;
                    v.push(0x0F);
                    v.push(0x84);
                    v.extend_from_slice(&((target - here - 6) as i32).to_le_bytes());
                }
                2 => {
                    // a displacement that lands outside: must NOT be moved
                    v.push(0xE9);
                    v.extend_from_slice(&0x4000_0000i32.to_le_bytes());
                }
                _ => v.push((seed >> 24) as u8),
            }
        }
        v.truncate(len);
        v
    }

    #[test]
    fn round_trips_and_moves_targets_out() {
        for len in [1, 5, 6, 64, 4096, 300_000] {
            let data = code(len);
            let mut work = data.clone();
            let applied = Filter::X86Split.apply(&mut work);
            match applied {
                Ok(a) => {
                    assert_eq!(a, Applied::Rebuilt);
                    Filter::X86Split.unapply(&mut work).unwrap();
                    assert_eq!(work, data, "len {len}");
                }
                // A buffer with no branch at all has nothing to move.
                Err(_) => assert_eq!(work, data),
            }
        }
    }

    #[test]
    fn a_truncated_or_forged_payload_is_refused() {
        let data = code(50_000);
        let mut work = data.clone();
        Filter::X86Split.apply(&mut work).unwrap();
        for cut in [0, 4, 12, work.len() / 2, work.len() - 1] {
            let mut bad = work[..cut].to_vec();
            assert!(Filter::X86Split.unapply(&mut bad).is_err(), "cut {cut}");
        }
        let mut wrong = work.clone();
        wrong[0] = b'X';
        assert!(Filter::X86Split.unapply(&mut wrong).is_err());
        // A header whose lengths do not add up must be refused, not trusted.
        let mut lied = work.clone();
        lied[4] = lied[4].wrapping_add(1);
        assert!(Filter::X86Split.unapply(&mut lied).is_err());
    }

    #[test]
    fn data_that_only_looks_like_code_still_round_trips() {
        let mut data = vec![0xE8u8; 1000];
        data.extend(std::iter::repeat_n(0x0Fu8, 1000));
        let mut work = data.clone();
        if Filter::X86Split.apply(&mut work).is_ok() {
            Filter::X86Split.unapply(&mut work).unwrap();
        }
        assert_eq!(work, data);
    }
}

#[cfg(test)]
mod chunked_tests {
    use super::*;

    /// A deflate stream whose plaintext is long and highly compressible, so a
    /// small pass size really does force several preflate passes.
    fn big_gzip(plain_len: usize) -> (Vec<u8>, Vec<u8>) {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        // Prose-shaped, and that matters. A tiny vocabulary makes deflate emit
        // matches at very short distances, and preflate's predictor could not
        // pick the chain back up across a pass boundary on such a stream —
        // pass two failed on its first token. Real streams (a tar of source,
        // the corpus this feature exists for) have varied match distances and
        // walk to the end. See PREFLATE_PASS and the prefix test below.
        let mut plain = Vec::with_capacity(plain_len);
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let next = |s: &mut u64| {
            *s ^= *s << 13;
            *s ^= *s >> 7;
            *s ^= *s << 17;
            *s
        };
        let vocab: Vec<String> = (0..4096)
            .map(|_| {
                let n = 3 + next(&mut seed) % 6;
                (0..n)
                    .map(|_| (b'a' + (next(&mut seed) % 26) as u8) as char)
                    .collect()
            })
            .collect();
        let mut col = 0usize;
        while plain.len() < plain_len {
            // Heavy head plus a long tail, so distances are varied rather than
            // all short.
            let shift = next(&mut seed).trailing_zeros().min(11) as usize;
            let w = &vocab[(next(&mut seed) as usize) % (vocab.len() >> shift).max(1)];
            plain.extend_from_slice(w.as_bytes());
            col += w.len() + 1;
            if col > 70 {
                plain.push(b'\n');
                col = 0;
            } else {
                plain.push(b' ');
            }
        }
        plain.truncate(plain_len);
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(&plain).unwrap();
        (e.finish().unwrap(), plain)
    }

    /// The whole point of id 40: a stream is modelled in as many passes as it
    /// takes, and the passes replay into exactly the bytes that went in.
    #[test]
    fn a_stream_is_modelled_in_several_passes_and_replays_exactly() {
        let (gz, plain) = big_gzip(4 << 20);
        let streams = crate::deflate::find_streams(&gz);
        let s = streams
            .iter()
            .find(|s| s.kind == crate::deflate::Kind::Deflate)
            .expect("a deflate stream");
        let raw = s.gather(&gz);

        // One pass per MiB of plaintext, so 4 MiB needs several. The pass must
        // still be large enough to hold a whole deflate block — see
        // PREFLATE_PASS — which is why this is 1 MiB and not 64 KiB.
        let (chunks, covered) =
            preflate_chunks(&raw, usize::MAX, 1 << 20).expect("the stream must model");
        assert!(
            chunks.len() >= 2,
            "expected several passes, got {}",
            chunks.len()
        );
        assert_eq!(covered, raw.len(), "an unbounded budget must reach the end");

        // The passes concatenate into the ORIGINAL plaintext, with nothing
        // duplicated — `PlainText::text()` skips the retained dictionary, and
        // if it did not this would be the assertion that caught it.
        let joined: Vec<u8> = chunks.iter().flat_map(|(p, _)| p.clone()).collect();
        assert_eq!(joined, plain, "the passes do not reconstruct the plaintext");

        // And replaying them reproduces the compressed bytes exactly.
        let mut rec = preflate_rs::RecreateStreamProcessor::new();
        let mut back = Vec::new();
        for (p, c) in &chunks {
            let (bytes, _) = rec.recompress(&mut std::io::Cursor::new(p), c).unwrap();
            back.extend_from_slice(&bytes);
        }
        assert_eq!(back, raw, "the replayed stream is not byte-identical");
    }

    /// A budget smaller than the stream yields a PREFIX rather than nothing.
    /// Before id 40 this case produced no transform at all, which is how a
    /// 51.9 MB tar.gz came out at 94.6% of itself.
    #[test]
    fn a_budget_too_small_still_models_the_prefix() {
        let (gz, _) = big_gzip(4 << 20);
        let streams = crate::deflate::find_streams(&gz);
        let raw = streams
            .iter()
            .find(|s| s.kind == crate::deflate::Kind::Deflate)
            .unwrap()
            .gather(&gz);

        const BUDGET: usize = 1 << 20;
        let (chunks, covered) =
            preflate_chunks(&raw, BUDGET, 1 << 20).expect("a prefix must still model");
        assert!(covered > 0 && covered < raw.len(), "covered {covered}");
        let spent: usize = chunks.iter().map(|(p, c)| p.len() + c.len()).sum();
        // NEVER over the budget. Overshooting by one pass is what made the
        // caller's cap check discard the entire stream instead of that pass,
        // and it is why a 51.9 MB tar.gz still came out untransformed after
        // the walk itself was already correct.
        assert!(spent <= BUDGET, "budget {BUDGET} exceeded: {spent}");
        // And close enough to it that the budget is used rather than wasted.
        assert!(spent > BUDGET / 2, "budget barely used: {spent}");

        let mut rec = preflate_rs::RecreateStreamProcessor::new();
        let mut back = Vec::new();
        for (p, c) in &chunks {
            let (bytes, _) = rec.recompress(&mut std::io::Cursor::new(p), c).unwrap();
            back.extend_from_slice(&bytes);
        }
        assert_eq!(back, raw[..covered], "the prefix is not byte-identical");
    }

    /// End to end through the real filter: a container holding such a stream
    /// transforms and comes back byte for byte.
    #[test]
    fn the_chunked_filter_round_trips_a_container() {
        let (gz, _) = big_gzip(4 << 20);
        let mut data = gz.clone();
        let enc = container_chunked_encode(&data).expect("must transform");
        assert!(
            enc.starts_with(b"NDf3"),
            "the chunked path must write the v3 framing"
        );
        let mut back = enc.clone();
        Filter::ContainerChunked.unapply(&mut back).unwrap();
        assert_eq!(back, gz, "the container did not round-trip");

        // And through `apply`, which is what the pipeline calls.
        let applied = Filter::ContainerChunked.apply(&mut data).unwrap();
        assert_eq!(applied, Applied::Rebuilt);
        Filter::ContainerChunked.unapply(&mut data).unwrap();
        assert_eq!(data, gz);
    }
}

#[cfg(test)]
mod legacy_framing_tests {
    use super::*;

    /// Ids 34 and 37 are no longer WRITTEN — the analyzer routes containers to
    /// 40 — so nothing else would notice if their decode path rotted. Archives
    /// carrying them exist, and an id is a promise.
    ///
    /// This encodes with each older framing deliberately and decodes it through
    /// the shipped filter, so the promise is checked rather than assumed.
    #[test]
    fn the_older_container_framings_still_decode() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let plain: Vec<u8> = std::iter::repeat_n(
            b"the quick brown fox jumps over the lazy dog, repeatedly. ".as_slice(),
            4000,
        )
        .flatten()
        .copied()
        .collect();
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(&plain).unwrap();
        let gz = e.finish().unwrap();

        let cap = crate::archive::MAX_CODED_CHUNK as usize;
        for (ver, filter, magic) in [
            (crate::deflate::Ver::V1, Filter::Deflate, &b"NDf1"[..]),
            (crate::deflate::Ver::V2, Filter::Container, &b"NDf2"[..]),
            (crate::deflate::Ver::V3, Filter::ContainerChunked, &b"NDf3"[..]),
        ] {
            let enc = container_encode_inner(&gz, ver, cap)
                .unwrap_or_else(|e| panic!("{ver:?} must transform this gzip: {e}"));
            assert!(enc.starts_with(magic), "{ver:?} wrote the wrong magic");
            let mut back = enc.clone();
            filter
                .unapply(&mut back)
                .unwrap_or_else(|e| panic!("{filter:?} must decode {ver:?}: {e}"));
            assert_eq!(back, gz, "{filter:?} did not rebuild the container");
        }
    }

    /// The three ids must stay distinct and keep their numbers forever.
    #[test]
    fn the_container_ids_do_not_move() {
        assert_eq!(Filter::Deflate.id(), 34);
        assert_eq!(Filter::Container.id(), 37);
        assert_eq!(Filter::ContainerChunked.id(), 40);
    }
}
