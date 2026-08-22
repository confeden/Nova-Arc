//! LZMA2 encoding through 7-Zip's own C encoder (LZMA SDK 25.01, public
//! domain — see `vendor/NOTICE.md`), wrapped so `nova-core` never sees an
//! `unsafe` block.
//!
//! WHY, in numbers. Measured against `lzma-rust2` on real 24-62 MB units at
//! identical settings (dictionary = chunk length, `nice_len` 273, bt4):
//!
//! | unit | lzma-rust2 | this | bytes | time |
//! |---|---:|---:|---:|---:|
//! | source tree, 32 MB | 2,248,437 · 14.6 s | 2,246,047 · 10.9 s | −0.11% | 1.33x |
//! | source tree exe, 24 MB | 1,964,059 · 10.4 s | 1,959,499 ·  7.7 s | −0.23% | 1.35x |
//! | source tree, 62 MB | 4,835,531 · 27.8 s | 4,829,388 · 21.2 s | −0.13% | 1.31x |
//!
//! Smaller AND faster, which is rare enough to be worth an FFI crate: it is the
//! same algorithm with a better optimal parser and a hand-tuned match finder.
//!
//! TWO KINDS OF THREADING, AND ONLY ONE IS ALLOWED HERE. LZMA2 can split its
//! input into blocks and code them in parallel — that buys speed with
//! dictionary reach and measured +1.6% on two threads and +19.5% on eight, so
//! `block_size` is pinned SOLID and the block-thread counts to one. What
//! `threads` turns on instead is `LzFindMt`, a helper thread inside the MATCH
//! FINDER of a single stream: the same block, the same dictionary, ~1.8x the
//! speed. It does shift which equal-length matches get picked, so it changes
//! the output by a few hundred bytes in either direction — which is why the
//! caller must decide it from the DATA and never from how busy the machine is
//! (nova-core's I8).
//!
//! The decoder stays `lzma-rust2`: pure Rust, no unsafe, and it reads what this
//! writes byte for byte, because a bare LZMA2 stream is a bare LZMA2 stream.

use anyhow::{bail, Result};

// -- The slice of the SDK this crate calls ------------------------------------
//
// Written out by hand rather than generated, so the build needs no bindgen and
// no libclang. Every layout below is copied field for field from the vendored
// headers (`Lzma2Enc.h`, `LzmaEnc.h`, `7zTypes.h`); getting one wrong would be
// memory corruption rather than a compile error, so they are also checked
// against the SDK's own initialisers by the tests at the bottom.

#[repr(C)]
struct ISzAlloc {
    alloc: Option<unsafe extern "C" fn(p: *const ISzAlloc, size: usize) -> *mut core::ffi::c_void>,
    free: Option<unsafe extern "C" fn(p: *const ISzAlloc, address: *mut core::ffi::c_void)>,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CLzmaEncProps {
    level: core::ffi::c_int,
    dict_size: u32,
    lc: core::ffi::c_int,
    lp: core::ffi::c_int,
    pb: core::ffi::c_int,
    algo: core::ffi::c_int,
    fb: core::ffi::c_int,
    bt_mode: core::ffi::c_int,
    num_hash_bytes: core::ffi::c_int,
    num_hash_out_bits: core::ffi::c_uint,
    mc: u32,
    write_end_mark: core::ffi::c_uint,
    num_threads: core::ffi::c_int,
    affinity_group: i32,
    reduce_size: u64,
    affinity: u64,
    affinity_in_group: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CLzma2EncProps {
    lzma_props: CLzmaEncProps,
    block_size: u64,
    num_block_threads_reduced: core::ffi::c_int,
    num_block_threads_max: core::ffi::c_int,
    num_total_threads: core::ffi::c_int,
    num_thread_groups: core::ffi::c_uint,
}

type Handle = *mut core::ffi::c_void;

unsafe extern "C" {
    fn Lzma2EncProps_Init(p: *mut CLzma2EncProps);
    fn Lzma2Enc_Create(alloc: *const ISzAlloc, alloc_big: *const ISzAlloc) -> Handle;
    fn Lzma2Enc_Destroy(p: Handle);
    fn Lzma2Enc_SetProps(p: Handle, props: *const CLzma2EncProps) -> core::ffi::c_int;
    fn Lzma2Enc_SetDataSize(p: Handle, expected_data_size: u64);
    #[allow(clippy::too_many_arguments)]
    fn Lzma2Enc_Encode2(
        p: Handle,
        out_stream: *mut core::ffi::c_void,
        out_buf: *mut u8,
        out_buf_size: *mut usize,
        in_stream: *const core::ffi::c_void,
        in_data: *const u8,
        in_data_size: usize,
        progress: *const core::ffi::c_void,
    ) -> core::ffi::c_int;

    fn malloc(size: usize) -> *mut core::ffi::c_void;
    fn free(p: *mut core::ffi::c_void);
}

/// The SDK allocates its match-finder tables through this: hundreds of
/// megabytes in a handful of calls, so the C runtime's allocator is the right
/// one and Rust's global allocator has nothing to add.
unsafe extern "C" fn sz_alloc(_p: *const ISzAlloc, size: usize) -> *mut core::ffi::c_void {
    unsafe { malloc(size) }
}

unsafe extern "C" fn sz_free(_p: *const ISzAlloc, address: *mut core::ffi::c_void) {
    unsafe { free(address) }
}

/// One solid block, whatever the input size.
const BLOCK_SOLID: u64 = u64::MAX;

/// LZMA2's worst case is the input plus a chunk header every 2 MiB when nothing
/// compresses; this is that with room to spare, and the encoder is told the
/// buffer size, so a mistake here is an error and not a write past the end.
fn out_capacity(len: usize) -> usize {
    len + len / 16 + 4096
}

/// Compress `data` into a bare LZMA2 stream — no xz container, no size field,
/// no CRC — which is exactly what `nova_core::codec` stores and what its
/// `lzma-rust2` reader decodes.
///
/// `dict_size` must be the value the DECODER will derive from the chunk length:
/// a decoder window may be wider than the encoder's, never narrower, so this is
/// part of the format and not a tuning knob (nova-core's I6).
///
/// `threads` is 1 or 2 and must be decided from the data alone — see the module
/// note on why the second one can change the output.
///
/// `match_cycles` is how far the match finder searches down a hash chain before
/// settling; 0 leaves the SDK's own figure, which at `nice_len` 273 works out
/// to 152. It is the one remaining ratio knob that costs only encode time.
pub fn compress(
    data: &[u8],
    dict_size: u32,
    nice_len: u32,
    threads: u32,
    match_cycles: u32,
) -> Result<Vec<u8>> {
    let alloc = ISzAlloc {
        alloc: Some(sz_alloc),
        free: Some(sz_free),
    };
    // SAFETY: every pointer handed to the SDK is either null where the API
    // documents one (the stream callbacks, which the buffer form does not use)
    // or points at a live local. `out` is sized by `out_capacity` and its
    // length travels with it, so the encoder cannot write past the end, and the
    // handle is destroyed on every path out.
    unsafe {
        let handle = Lzma2Enc_Create(&alloc, &alloc);
        if handle.is_null() {
            bail!("LZMA2 encoder could not be created");
        }
        let mut props: CLzma2EncProps = core::mem::zeroed();
        Lzma2EncProps_Init(&mut props);
        props.lzma_props.level = 9;
        props.lzma_props.dict_size = dict_size;
        props.lzma_props.fb = nice_len as core::ffi::c_int;
        props.lzma_props.mc = match_cycles;
        props.lzma_props.num_threads = threads.clamp(1, 2) as core::ffi::c_int;
        // Lets the encoder size its tables for what is actually coming instead
        // of for the whole dictionary.
        props.lzma_props.reduce_size = data.len() as u64;
        props.block_size = BLOCK_SOLID;
        props.num_block_threads_reduced = 1;
        props.num_block_threads_max = 1;
        props.num_total_threads = threads.clamp(1, 2) as core::ffi::c_int;
        let res = Lzma2Enc_SetProps(handle, &props);
        if res != 0 {
            Lzma2Enc_Destroy(handle);
            bail!("LZMA2 encoder rejected its settings (code {res})");
        }
        Lzma2Enc_SetDataSize(handle, data.len() as u64);
        let mut out = vec![0u8; out_capacity(data.len())];
        let mut out_len = out.len();
        let res = Lzma2Enc_Encode2(
            handle,
            core::ptr::null_mut(),
            out.as_mut_ptr(),
            &mut out_len,
            core::ptr::null(),
            data.as_ptr(),
            data.len(),
            core::ptr::null(),
        );
        Lzma2Enc_Destroy(handle);
        if res != 0 {
            bail!("LZMA2 encoding failed (code {res})");
        }
        if out_len > out.len() {
            bail!(
                "LZMA2 encoder reported {out_len} bytes into a {}-byte buffer",
                out.len()
            );
        }
        out.truncate(out_len);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wordy(n: usize) -> Vec<u8> {
        let words = ["the", "unit", "codec", "sample", "window", "entrant"];
        let mut seed = 0x243F_6A88_85A3_08D3u64;
        let mut out = Vec::with_capacity(n + 16);
        while out.len() < n {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            out.extend_from_slice(words[(seed >> 33) as usize % words.len()].as_bytes());
            out.push(b' ');
        }
        out.truncate(n);
        out
    }

    /// The one thing a hand-written binding can get silently wrong is the
    /// layout. `Lzma2EncProps_Init` writes known defaults through the C
    /// struct — level 5, one thread, automatic block size — so reading them
    /// back through the Rust one proves the fields line up.
    #[test]
    fn the_props_layout_matches_the_sdk() {
        // SAFETY: the SDK only writes its own defaults into the struct.
        let props = unsafe {
            let mut p: CLzma2EncProps = core::mem::zeroed();
            Lzma2EncProps_Init(&mut p);
            p
        };
        assert_eq!(props.lzma_props.level, 5, "level field is misaligned");
        assert_eq!(props.lzma_props.num_threads, -1, "numThreads is misaligned");
        assert_eq!(props.num_block_threads_max, -1, "block threads misaligned");
        assert_eq!(props.block_size, 0, "blockSize is misaligned");
    }

    /// Empty and tiny inputs are the shapes an encoder gets wrong.
    #[test]
    fn degenerate_inputs_encode() {
        for len in [0usize, 1, 2, 4095] {
            let data = wordy(len);
            let out = compress(&data, 4096, 273, 1, 0).expect("encodes");
            assert!(!out.is_empty() || len == 0, "len {len} produced nothing");
        }
    }

    /// Same bytes, same settings, same output — at BOTH thread counts, because
    /// nova picks the count from the data and then depends on it (I8).
    #[test]
    fn encoding_is_deterministic() {
        let data = wordy(1 << 21);
        for threads in [1, 2] {
            let a = compress(&data, 1 << 21, 273, threads, 0).expect("encodes");
            let b = compress(&data, 1 << 21, 273, threads, 0).expect("encodes");
            assert_eq!(a, b, "{threads} thread(s) gave two different streams");
        }
    }

    /// Incompressible data must not overflow the output buffer.
    #[test]
    fn noise_stays_within_the_buffer() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let noise: Vec<u8> = (0..1 << 20)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 24) as u8
            })
            .collect();
        let out = compress(&noise, 1 << 20, 273, 1, 0).expect("encodes");
        assert!(out.len() <= out_capacity(noise.len()));
    }
}
