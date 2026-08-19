use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Manifest compression level: the manifest is small and rewritten on every
/// commit, so a mid-level zstd is the right trade-off.
const MANIFEST_ZSTD_LEVEL: i32 = 9;

/// Hard ceiling on a decoded manifest. A 256 MiB manifest already describes
/// tens of millions of chunks; anything larger is a corrupt or hostile
/// footer trying to make us allocate.
const MAX_MANIFEST_UNPACKED: u64 = 256 * 1024 * 1024;

/// Plausible zstd ratio bound for manifest data. Real manifests compress
/// ~3-10x; 1000x means the claimed size is fabricated.
const MAX_MANIFEST_RATIO: u64 = 1000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub generation: u64,
    pub files: Vec<FileEntry>,
    /// Compression units. Each is one independently decodable stream; a unit
    /// may hold many small files, part of one large file, or exactly one file.
    pub chunks: Vec<ChunkRec>,
    /// How this archive cuts data into units, fixed when it was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<Geometry>,
}

/// Unit geometry — a property of the *archive*, not of a single operation.
///
/// Deduplication only works when identical data lands on identical unit
/// boundaries, so an archive created at one compression tier and later added
/// to at another must keep cutting the same way. Otherwise every unchanged
/// file looks new: measured, re-adding an untouched 28 MiB tree at a
/// different tier stored all of it again and deduplicated nothing.
///
/// The compression *method* is free to differ per operation — dedup keys are
/// content hashes, not codec output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    pub chunk_min: u32,
    pub chunk_avg: u32,
    pub chunk_max: u32,
    /// Target size of a compression unit. Units group consecutive small files
    /// and consecutive chunks of a large file, because unit size is the single
    /// biggest ratio lever: measured on a source tree, 4 MiB units cost 50%
    /// more than one solid stream while 32 MiB units cost only 5%.
    pub unit: u64,
    /// Files at or above this size are chunked before grouping instead of
    /// being placed whole.
    pub chunked_from: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Archive-internal path: relative, UTF-8, '/'-separated.
    pub path: String,
    pub size: u64,
    /// Unix seconds, may be negative (pre-1970); 0 = unknown.
    pub mtime: i64,
    /// Where the file's bytes live, in file order. A small file is one extent
    /// inside a shared unit; a large file is a run of extents across units.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extents: Vec<Extent>,
}

/// A byte range of one compression unit.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Extent {
    /// Index into `Manifest::chunks`.
    pub unit: u32,
    /// Offset of the file's bytes inside the (decompressed) unit.
    pub off: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRec {
    /// Absolute offset of the packed payload in the archive file.
    pub offset: u64,
    pub packed: u64,
    pub unpacked: u64,
    pub codec: u8,
    /// Codec-specific setting the decoder must know: PPMd7's model order.
    /// Zero means "the codec's default", which is also what pre-parameter
    /// archives contain.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub param: u8,
    /// Reversible transform applied before compression (see `filters`).
    /// 0 = none, which is the common case, so it stays out of the manifest.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub filter: u8,
    /// Length of the buffer the CODEC produced — `unpacked` after the filter
    /// ran. `0` means "the same as `unpacked`", which is every chunk written
    /// before recompression existed and every chunk whose filter preserves
    /// length (BCJ, delta).
    ///
    /// The two numbers were the same thing until a filter could change length,
    /// and `unpacked` kept the older meaning: the length of the ORIGINAL bytes,
    /// which is what `hash` covers and what `Extent` indexes into. A decoder
    /// needs the other one, because it sizes the output buffer, the LZMA2
    /// window and the PPMd7 model pool — none of which are stored anywhere
    /// else. Read it as `if filtered == 0 { unpacked } else { filtered }`, never
    /// as an `Option` where 0 is a legal value: inverting that reading makes
    /// every archive ever written stop decoding.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub filtered: u64,
    /// blake3 of the UNPACKED unit, truncated to 128 bits. Used for both
    /// dedup and integrity verification on extract. The hash covers the
    /// original bytes, before any filter, so dedup is filter-independent.
    #[serde(with = "hash16")]
    pub hash: [u8; 16],
}

fn is_zero(v: &u8) -> bool {
    *v == 0
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

impl ChunkRec {
    /// How many bytes the codec produced, and therefore how many the decoder
    /// must ask it for. See [`ChunkRec::filtered`].
    pub fn coded_len(&self) -> u64 {
        if self.filtered == 0 {
            self.unpacked
        } else {
            self.filtered
        }
    }
}

/// A manifest big enough that squeezing it is worth a slower codec. Below this
/// the difference is a few hundred bytes and the commit path stays quick — it
/// runs on every `add`, including the one-file edit the format exists for.
const MANIFEST_LZMA_FROM: usize = 128 * 1024;

/// zstd's frame magic. A raw LZMA2 stream cannot start with it: its first byte
/// is a chunk control, which is 0x00-0x02 or 0x80-0xFF, never 0x28. That is what
/// lets the codec be chosen per archive without a format field, and what keeps
/// every manifest ever written readable.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

impl Manifest {
    /// Serialize (MessagePack with field names, for forward evolution) and
    /// compress. Returns (packed bytes, unpacked length).
    ///
    /// The manifest is the largest single thing nova stores that is not file
    /// data: on a 5751-file source tree it is 92 KB of a 9.3 MB archive, and
    /// more than half of what still separates that archive from 7-Zip's.
    /// MEASURED on that manifest: zstd 9 92,043 B · zstd 19 84,565 B ·
    /// LZMA2 74,690 B. So a large one is offered to LZMA2 and the smaller
    /// result wins; a small one is not, because this runs on every commit.
    pub fn encode(&self) -> Result<(Vec<u8>, u64)> {
        let raw = rmp_serde::to_vec_named(self)?;
        let mut packed = zstd::bulk::compress(&raw, MANIFEST_ZSTD_LEVEL)?;
        if raw.len() >= MANIFEST_LZMA_FROM {
            if let Ok(lzma) = crate::codec::compress(crate::codec::Codec::Lzma2, 12, 0, &raw) {
                if lzma.len() < packed.len() && !lzma.starts_with(&ZSTD_MAGIC) {
                    packed = lzma;
                }
            }
        }
        Ok((packed, raw.len() as u64))
    }

    /// Decode a manifest. `unpacked_len` comes from the (untrusted) footer,
    /// so it is bounds-checked before it can drive an allocation.
    pub fn decode(packed: &[u8], unpacked_len: u64) -> Result<Manifest> {
        if unpacked_len > MAX_MANIFEST_UNPACKED
            || unpacked_len > (packed.len() as u64).saturating_mul(MAX_MANIFEST_RATIO)
        {
            bail!("corrupt archive: implausible manifest size ({unpacked_len} bytes)");
        }
        let cap = usize::try_from(unpacked_len).context("manifest too large for this platform")?;
        // The codec is read off the bytes, not out of a field: every manifest
        // nova has ever written is a zstd frame and still decodes here, and a
        // raw LZMA2 stream can never be mistaken for one (see `ZSTD_MAGIC`).
        let raw = if packed.starts_with(&ZSTD_MAGIC) {
            zstd::bulk::decompress(packed, cap)?
        } else {
            crate::codec::decompress(crate::codec::Codec::Lzma2, packed, cap, 0)
                .context("manifest is neither a zstd frame nor an LZMA2 stream")?
        };
        Ok(rmp_serde::from_slice(&raw)?)
    }
}

/// Serialize a 16-byte hash as MessagePack `bin`, not as an array of 16
/// integers (which serde's default for `[u8; 16]` would produce, costing
/// ~27 bytes per chunk record instead of 18).
mod hash16 {
    use std::fmt;

    use serde::de::{self, SeqAccess, Visitor};
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 16], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(v)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 16], D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = [u8; 16];

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("16 bytes")
            }

            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                v.try_into().map_err(|_| E::invalid_length(v.len(), &self))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut out = [0u8; 16];
                for (i, slot) in out.iter_mut().enumerate() {
                    *slot = a
                        .next_element()?
                        .ok_or_else(|| de::Error::invalid_length(i, &self))?;
                }
                Ok(out)
            }
        }
        d.deserialize_bytes(V)
    }
}
