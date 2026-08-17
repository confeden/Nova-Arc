use anyhow::{bail, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// Raw bytes, no compression.
    Store,
    /// Zstandard.
    Zstd,
}

impl Codec {
    pub fn id(self) -> u8 {
        match self {
            Codec::Store => 0,
            Codec::Zstd => 1,
        }
    }

    pub fn from_id(id: u8) -> Result<Codec> {
        match id {
            0 => Ok(Codec::Store),
            1 => Ok(Codec::Zstd),
            other => bail!("unknown codec id {other} - archive was made by a newer version"),
        }
    }
}

pub fn compress(codec: Codec, level: i32, data: &[u8]) -> Result<Vec<u8>> {
    match codec {
        Codec::Store => Ok(data.to_vec()),
        Codec::Zstd => Ok(zstd::bulk::compress(data, level)?),
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
    }
}
