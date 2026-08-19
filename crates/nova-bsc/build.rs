//! Build the vendored libbsc 3.3.12 (Apache-2.0, `vendor/LICENSE`).
//!
//! Two things are deliberately NOT enabled:
//!
//! - `LIBBSC_OPENMP`. nova already runs one compression worker per unit, so
//!   libbsc's own threads would multiply the thread count and wreck the memory
//!   accounting — the same reason `ZSTD_c_nbWorkers` is rejected in ROADMAP.
//! - `LIBBSC_CUDA_SUPPORT`. The GPU sort transform needs nvcc and a runtime we
//!   will not ship; the CUDA sources are not vendored at all.

use std::path::Path;

const CPP: &[&str] = &[
    "libbsc/adler32/adler32.cpp",
    "libbsc/bwt/bwt.cpp",
    "libbsc/coder/coder.cpp",
    "libbsc/coder/qlfc/qlfc.cpp",
    "libbsc/coder/qlfc/qlfc_model.cpp",
    "libbsc/filters/detectors.cpp",
    "libbsc/filters/preprocessing.cpp",
    "libbsc/libbsc/libbsc.cpp",
    "libbsc/lzp/lzp.cpp",
    "libbsc/platform/platform.cpp",
    "libbsc/st/st.cpp",
];

fn main() {
    let vendor = Path::new("vendor");
    assert!(
        vendor.join("libbsc/libbsc.h").exists(),
        "vendored libbsc is missing from crates/nova-bsc/vendor"
    );

    let mut cpp = cc::Build::new();
    cpp.cpp(true).include(vendor).warnings(false);
    for f in CPP {
        cpp.file(vendor.join(f));
    }
    cpp.compile("bsc_cpp");

    // libsais is C, not C++, and mixing the two in one cc::Build unit makes
    // MSVC compile it as C++ — which it does not survive.
    let mut c = cc::Build::new();
    c.include(vendor).warnings(false);
    c.file(vendor.join("libbsc/bwt/libsais/libsais.c"));
    c.compile("bsc_sais");

    // `bsc_platform_init` asks for the large-page privilege on Windows. We never
    // enable LIBBSC_FEATURE_LARGEPAGES, but the calls are compiled in
    // unconditionally, so the import library is needed anyway.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-lib=advapi32");
    }

    println!("cargo:rerun-if-changed=vendor");
    println!("cargo:rerun-if-changed=build.rs");
}
