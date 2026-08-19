//! libbsc — Burrows-Wheeler transform plus QLFC entropy coding — behind a safe
//! Rust API.
//!
//! Why it is here at all: measured on this machine at nova's OWN 32 MiB unit,
//! bsc beats the max tier's PPMd7 on enwik8 **on all three axes at once** —
//! 21,983,674 B against 22,466,101 (−2.1%), 2.0 s against 71.7 s to compress,
//! 0.9 s against 18.3 s to decompress. On a source tree it loses badly
//! (13.5 MB against nova's 9.3 MB), which is exactly why it joins the per-unit
//! tournament instead of replacing anything.
//!
//! # Format constants
//!
//! Everything below decides the bitstream and NOTHING of it is stored per
//! chunk, so all of it is part of codec id 4 forever, like PPMd7's order and
//! pool. Changing any one of them makes every existing archive with a bsc unit
//! undecodable.

use std::os::raw::c_int;
use std::sync::OnceLock;

use anyhow::{bail, Result};

/// Burrows-Wheeler transform. The Sort Transform alternatives are not used.
const BLOCKSORTER_BWT: c_int = 1;
/// Adaptive QLFC — `bsc -e2`, the setting our measurements were taken with.
const CODER_QLFC_ADAPTIVE: c_int = 2;
/// LZP preprocessing, at the library's own defaults, which is what the `bsc`
/// command line uses and therefore what was measured.
const LZP_HASH_SIZE: c_int = 15;
const LZP_MIN_LEN: c_int = 72;
/// `LIBBSC_FEATURE_FASTMODE` only. `LIBBSC_FEATURE_MULTITHREADING` is
/// deliberately absent: nova runs one worker per unit already, and letting the
/// library start its own threads would both multiply the thread count and break
/// the memory budget.
const FEATURES: c_int = 1;

/// `LIBBSC_HEADER_SIZE`. Every block libbsc produces starts with this many
/// bytes and they carry the sizes `bsc_block_info` reports.
const HEADER_SIZE: usize = 28;

const LIBBSC_NO_ERROR: c_int = 0;
const LIBBSC_NOT_COMPRESSIBLE: c_int = -3;

extern "C" {
    fn bsc_init(features: c_int) -> c_int;
    fn bsc_compress(
        input: *const u8,
        output: *mut u8,
        n: c_int,
        lzp_hash_size: c_int,
        lzp_min_len: c_int,
        block_sorter: c_int,
        coder: c_int,
        features: c_int,
    ) -> c_int;
    fn bsc_store(input: *const u8, output: *mut u8, n: c_int, features: c_int) -> c_int;
    fn bsc_block_info(
        block_header: *const u8,
        header_size: c_int,
        block_size: *mut c_int,
        data_size: *mut c_int,
        features: c_int,
    ) -> c_int;
    fn bsc_decompress(
        input: *const u8,
        input_size: c_int,
        output: *mut u8,
        output_size: c_int,
        features: c_int,
    ) -> c_int;
}

/// `bsc_init` fills two global statistical models and must run before anything
/// else touches the library.
///
/// After it returns, those globals are READ-ONLY: every call to
/// `bsc_qlfc_init_model` memcpy's the global into a per-call model, so
/// concurrent compression and decompression from nova's worker threads is safe.
/// That was checked in the vendored source, not assumed — it is the one
/// property that makes this crate usable from a thread pool at all.
fn init() -> Result<()> {
    static ONCE: OnceLock<c_int> = OnceLock::new();
    // SAFETY: no arguments to get wrong, and OnceLock guarantees exactly one
    // call however many threads arrive at once.
    let rc = *ONCE.get_or_init(|| unsafe { bsc_init(FEATURES) });
    if rc != LIBBSC_NO_ERROR {
        bail!("libbsc failed to initialise ({rc})");
    }
    Ok(())
}

/// The largest block this codec accepts. libbsc counts in `int`, and nova's
/// units are two orders of magnitude below this anyway.
pub const MAX_BLOCK: usize = c_int::MAX as usize - HEADER_SIZE;

/// Compress one block. The result always starts with libbsc's 28-byte header,
/// so [`decompress`] can check the sizes before allocating.
///
/// Data libbsc cannot shrink comes back in its `store` form rather than as an
/// error: the caller is a tournament that keeps the smallest candidate, and an
/// `Err` here would abort the whole pack instead of losing one round.
pub fn compress(data: &[u8]) -> Result<Vec<u8>> {
    init()?;
    if data.len() > MAX_BLOCK {
        bail!("block of {} bytes is too large for libbsc", data.len());
    }
    let n = data.len() as c_int;
    let mut out = vec![0u8; data.len() + HEADER_SIZE];

    // SAFETY: `input` is readable for `n` bytes and `output` is writable for
    // `n + HEADER_SIZE`, which is what libbsc documents as the required output
    // size. The two buffers do not overlap.
    let rc = unsafe {
        bsc_compress(
            data.as_ptr(),
            out.as_mut_ptr(),
            n,
            LZP_HASH_SIZE,
            LZP_MIN_LEN,
            BLOCKSORTER_BWT,
            CODER_QLFC_ADAPTIVE,
            FEATURES,
        )
    };
    let rc = if rc == LIBBSC_NOT_COMPRESSIBLE {
        // SAFETY: same buffers, and `bsc_store` writes exactly
        // `n + HEADER_SIZE` bytes.
        unsafe { bsc_store(data.as_ptr(), out.as_mut_ptr(), n, FEATURES) }
    } else {
        rc
    };
    if rc < 0 {
        bail!("libbsc could not compress this block ({rc})");
    }
    let produced = rc as usize;
    if produced < HEADER_SIZE || produced > out.len() {
        bail!(
            "libbsc reported {produced} bytes for a {} byte buffer",
            out.len()
        );
    }
    out.truncate(produced);
    Ok(out)
}

/// Rebuild a block. `unpacked_len` comes from the manifest, and the header's
/// own claim must agree with it.
///
/// Both sizes are checked BEFORE the output buffer is allocated, because on
/// this path the payload is untrusted: a forged header must cost an error, not
/// a gigabyte.
pub fn decompress(data: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    init()?;
    if data.len() < HEADER_SIZE {
        bail!(
            "libbsc block is {} bytes, shorter than its header",
            data.len()
        );
    }
    if data.len() > c_int::MAX as usize || unpacked_len > c_int::MAX as usize {
        bail!("libbsc block sizes exceed what the library can address");
    }
    let mut block_size: c_int = 0;
    let mut data_size: c_int = 0;
    // SAFETY: `data` holds at least HEADER_SIZE bytes, checked above, and the
    // two out-parameters are valid pointers to initialised locals.
    let rc = unsafe {
        bsc_block_info(
            data.as_ptr(),
            HEADER_SIZE as c_int,
            &mut block_size,
            &mut data_size,
            FEATURES,
        )
    };
    if rc != LIBBSC_NO_ERROR {
        bail!("libbsc block header is unreadable ({rc})");
    }
    if block_size < 0 || block_size as usize != data.len() {
        bail!(
            "libbsc header claims a {block_size} byte block, payload is {}",
            data.len()
        );
    }
    if data_size < 0 || data_size as usize != unpacked_len {
        bail!("libbsc header claims {data_size} bytes out, manifest says {unpacked_len}");
    }

    let mut out = vec![0u8; unpacked_len];
    // SAFETY: `input` is readable for `block_size` bytes (equal to data.len(),
    // checked), `output` is writable for `data_size` (equal to unpacked_len,
    // checked, and that is how `out` was just sized).
    let rc = unsafe {
        bsc_decompress(
            data.as_ptr(),
            block_size,
            out.as_mut_ptr(),
            data_size,
            FEATURES,
        )
    };
    if rc < 0 {
        bail!("libbsc could not rebuild this block ({rc})");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(len: usize) -> Vec<u8> {
        let mut v = "the quick brown fox jumps over the lazy dog. "
            .repeat(len / 45 + 1)
            .into_bytes();
        v.truncate(len);
        v
    }

    fn noise(len: usize) -> Vec<u8> {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut v = Vec::with_capacity(len + 8);
        while v.len() < len {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            v.extend_from_slice(&seed.to_le_bytes());
        }
        v.truncate(len);
        v
    }

    #[test]
    fn round_trips_and_shrinks_text() {
        let data = text(400_000);
        let packed = compress(&data).unwrap();
        assert!(packed.len() < data.len() / 4, "packed {}", packed.len());
        assert_eq!(decompress(&packed, data.len()).unwrap(), data);
    }

    /// Data libbsc cannot shrink must still round-trip, because the tournament
    /// asks every codec for a candidate and only then compares.
    #[test]
    fn round_trips_incompressible_data() {
        let data = noise(300_000);
        let packed = compress(&data).unwrap();
        assert_eq!(decompress(&packed, data.len()).unwrap(), data);
    }

    #[test]
    fn round_trips_the_degenerate_sizes() {
        for n in [0usize, 1, 2, 63, 64, 4096] {
            let data = text(n);
            let packed = compress(&data).unwrap();
            assert_eq!(
                decompress(&packed, n).unwrap(),
                data,
                "{n} bytes did not round-trip"
            );
        }
    }

    #[test]
    fn same_input_gives_the_same_bytes() {
        let data = text(200_000);
        assert_eq!(compress(&data).unwrap(), compress(&data).unwrap());
    }

    /// A hostile payload must be refused before anything is allocated from the
    /// numbers it carries.
    #[test]
    fn refuses_a_forged_or_truncated_block() {
        let data = text(100_000);
        let packed = compress(&data).unwrap();

        assert!(decompress(&packed[..HEADER_SIZE - 1], data.len()).is_err());
        assert!(decompress(&packed[..packed.len() - 1], data.len()).is_err());
        assert!(decompress(&packed, data.len() + 1).is_err());
        assert!(decompress(&[], 10).is_err());

        // A header claiming a gigabyte out of a small payload is the case that
        // must not turn into a gigabyte of allocation.
        let mut forged = packed.clone();
        forged[4..8].copy_from_slice(&1_000_000_000i32.to_le_bytes());
        assert!(decompress(&forged, data.len()).is_err());
        assert!(decompress(&forged, 1_000_000_000).is_err());
    }

    /// The models are global; this is the property that makes the crate usable
    /// from nova's worker pool.
    #[test]
    fn survives_concurrent_use() {
        let data = text(120_000);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let d = data.clone();
                std::thread::spawn(move || {
                    for _ in 0..4 {
                        let p = compress(&d).unwrap();
                        assert_eq!(decompress(&p, d.len()).unwrap(), d);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
}
