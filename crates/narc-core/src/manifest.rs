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
    /// Every chunk ever written to the archive (until `compact`). Entries not
    /// referenced by any file are dead space but remain usable for dedup.
    pub chunks: Vec<ChunkRec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Archive-internal path: relative, UTF-8, '/'-separated.
    pub path: String,
    pub size: u64,
    /// Unix seconds, may be negative (pre-1970); 0 = unknown.
    pub mtime: i64,
    /// Indices into `Manifest::chunks`, in file order.
    pub chunks: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRec {
    /// Absolute offset of the packed payload in the archive file.
    pub offset: u64,
    pub packed: u64,
    pub unpacked: u64,
    pub codec: u8,
    /// blake3 of the UNPACKED chunk, truncated to 128 bits. Used for both
    /// dedup and integrity verification on extract.
    #[serde(with = "hash16")]
    pub hash: [u8; 16],
}

impl Manifest {
    /// Serialize (MessagePack with field names, for forward evolution) and
    /// compress. Returns (packed bytes, unpacked length).
    pub fn encode(&self) -> Result<(Vec<u8>, u64)> {
        let raw = rmp_serde::to_vec_named(self)?;
        let packed = zstd::bulk::compress(&raw, MANIFEST_ZSTD_LEVEL)?;
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
        let raw = zstd::bulk::decompress(packed, cap)?;
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
