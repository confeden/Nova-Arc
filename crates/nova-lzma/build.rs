//! Builds the vendored LZMA2 encoder.
//!
//! The file list is written out rather than globbed: which translation units go
//! in decides whether the threaded match finder is linked at all, and a glob
//! would silently change that the day someone drops another file in `vendor/`.

fn main() {
    let vendor = std::path::Path::new("vendor");
    println!("cargo:rerun-if-changed=vendor");

    let mut build = cc::Build::new();
    build
        .include(vendor)
        // The encoder, its match finders and the thread pool the multithreaded
        // match finder needs. `MtCoder`/`MtDec` are LZMA2's own block-splitting
        // coder: they have to be present for `Lzma2Enc.c` to link, but nothing
        // here ever asks for more than one block (see lib.rs).
        .files([
            vendor.join("LzmaEnc.c"),
            vendor.join("Lzma2Enc.c"),
            vendor.join("LzFind.c"),
            vendor.join("LzFindMt.c"),
            vendor.join("LzFindOpt.c"),
            vendor.join("MtCoder.c"),
            vendor.join("MtDec.c"),
            vendor.join("Threads.c"),
            vendor.join("CpuArch.c"),
            vendor.join("Alloc.c"),
            // `MtCoder`/`MtDec` reach for it when a block coder reads its input.
            vendor.join("7zStream.c"),
        ]);

    // The SDK builds single-threaded unless told otherwise, and the threaded
    // match finder is the entire reason this is vendored rather than taken
    // from a wrapper crate that omits it.
    if cfg!(windows) {
        build.define("_WIN32_WINNT", "0x0601");
    } else {
        build.define("Z7_AFFINITY_DISABLE", None);
    }

    build.warnings(false).opt_level(3).compile("nova_lzma_sdk");
}
