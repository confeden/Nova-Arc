# 05 — Rust Ecosystem & Licensing Audit for Nova Prism

Research date: **2026-08-16**. All version numbers, dates and download counts were pulled live from the crates.io API and GitHub on this date (not from memory). Downloads are all-time / recent (last-90-days) as reported by crates.io.

Scope: crate maturity, maintenance, license, pure-Rust vs FFI, RAR legalities, the 2024 xz lesson, and a dependency shortlist for the MVP and the max-compression tier. Nova Prism's own license is TBD, so both permissive and copyleft scenarios are analyzed.

---

## 1. Master table — all audited crates

| Crate | Latest | License | Last publish | Downloads (all / 90d) | Pure Rust? | Verdict |
|---|---|---|---|---|---|---|
| [zip](https://crates.io/crates/zip) | 8.6.0 | MIT | 2026-04-25 | 239.9M / 60.9M | Yes (core) | **Use** |
| [sevenz-rust2](https://crates.io/crates/sevenz-rust2) | 0.21.4 | Apache-2.0 | 2026-08-01 | 803K / 413K | Yes | **Use** |
| [unrar](https://crates.io/crates/unrar) | 0.5.8 | MIT OR Apache-2.0 (wrapper only!) | 2025-02-19 | 456K / 228K | No (C++ unrar) | **Use, isolated** (see §3) |
| [tar](https://crates.io/crates/tar) | 0.4.46 | MIT OR Apache-2.0 | 2026-05-18 | 209.9M / 47.7M | Yes | **Use** |
| [flate2](https://crates.io/crates/flate2) | 1.1.9 | MIT OR Apache-2.0 | 2026-02-03 | 618.6M / 137.8M | Yes (default backend) | **Use** |
| [zlib-rs](https://crates.io/crates/zlib-rs) | 0.6.7 | Zlib | 2026-08-03 | 111.0M / 42.9M | Yes | **Use** (as flate2 backend) |
| [zstd](https://crates.io/crates/zstd) | 0.13.3 | MIT (libzstd: BSD-3/GPLv2 dual) | 2025-02-20 | 356.3M / 72.3M | **No** (zstd-sys, cc) | **Use** |
| [ruzstd](https://crates.io/crates/ruzstd) | 0.9.0 | MIT | 2026-07-26 | 58.6M / 13.7M | Yes (decoder) | Optional fallback |
| [xz2](https://crates.io/crates/xz2) | 0.1.7 | MIT/Apache-2.0 | **2022-06-06** | 61.9M / 11.8M | No | **AVOID — dormant** |
| [liblzma](https://crates.io/crates/liblzma) | 0.4.8 | MIT OR Apache-2.0 | 2026-08-09 | 18.8M / 4.7M | No (bundles XZ 5.8) | **Use** (max tier) |
| [lzma-rust2](https://crates.io/crates/lzma-rust2) | 0.18.1 | Apache-2.0 | 2026-08-05 | 17.9M / 9.2M | Yes | **Use** (via sevenz-rust2; standalone option) |
| [lzma-rs](https://crates.io/crates/lzma-rs) | 0.3.0 | MIT | **2023-01-04** | 35.4M | Yes | **AVOID — dormant** |
| [brotli](https://crates.io/crates/brotli) | 8.0.4 | BSD-3-Clause AND MIT | 2026-06-14 | 237.7M / 52.4M | Yes (all safe code) | **Use** (max tier) |
| [bzip2](https://crates.io/crates/bzip2) | 0.6.1 | MIT OR Apache-2.0 | 2025-10-16 | 151.0M / 29.8M | Yes (libbz2-rs-sys default) | **Use** (compat only) |
| [bzip3](https://crates.io/crates/bzip3) | 0.12.0 | **LGPL-3.0-only** | 2025-12-30 | 100.7K / 27.3K | No (C libbz3, cc) | Conditional (license risk, §5) |
| libbsc (FFI) | — (no crate) | Apache-2.0 (upstream) | upstream active | — | No (C++, custom FFI) | **Build own FFI** (max tier) |
| [ppmd-rust](https://crates.io/crates/ppmd-rust) | 1.4.0 | CC0-1.0 OR MIT-0 | 2026-01-24 | 16.4M | Yes | **Use** (max tier, text) |
| [fastcdc](https://crates.io/crates/fastcdc) | 4.0.1 | MIT | 2026-04-26 | 1.4M / 843K | Yes | **Use** (.nva core) |
| [blake3](https://crates.io/crates/blake3) | 1.8.6 | CC0-1.0 OR Apache-2.0 (± LLVM-exc.) | 2026-08-05 | 164.4M / 38.3M | Mostly (C/asm SIMD via cc by default; pure fallback exists) | **Use** (.nva core) |
| [memmap2](https://crates.io/crates/memmap2) | 0.9.11 | MIT OR Apache-2.0 | 2026-06-22 | 311.1M / 69.4M | Yes (syscalls only) | **Use** |
| [rayon](https://crates.io/crates/rayon) | 1.12.0 | MIT OR Apache-2.0 | 2026-04-14 | 497.4M / 111.4M | Yes | **Use** |
| [notify](https://crates.io/crates/notify) | 8.2.0 | CC0-1.0 | 2026-05-02 | 139.2M / 35.3M | Yes | **Use** (GUI phase) |
| [infer](https://crates.io/crates/infer) | 0.22.0 | MIT | 2026-07-15 | 109.7M / 21.9M | Yes, no_std | **Use** (analyzer phase 1) |
| [file_format](https://crates.io/crates/file_format) | 0.29.0 | MIT/Apache-2.0 | 2026-03-27 | 1.65M / 440K | Yes | **Use** (analyzer phase 1) |
| [filetime](https://crates.io/crates/filetime) | 0.2.29 | MIT/Apache-2.0 | 2026-05-12 | 345.9M / 76.5M | Yes | **Use** |
| [windows](https://crates.io/crates/windows) | 0.62.2 | MIT OR Apache-2.0 | 2025-10-06 | 295.7M / 66.9M | Yes (bindings; links system DLLs, no C toolchain) | **Use** |
| [clap](https://crates.io/crates/clap) | 4.6.6 | MIT OR Apache-2.0 | 2026-08-06 | 1,048.0M / 215.7M | Yes | **Use** |

All of the above except `bzip3` and `unrar`'s vendored C++ are unproblematic for **either** a permissive or a copyleft Nova Prism license (details in §5).

---

## 2. Foreign-format crates in detail

### zip (zip-rs/zip2) — read/write ZIP
- Repo: <https://github.com/zip-rs/zip2>, MIT, MSRV 1.88, OpenSSF Best Practices badge, fuzzed with cargo-afl. The original `zip-rs/zip-old` repo was declared unmaintained ([issue #446](https://github.com/zip-rs/zip-old/issues/446)); zip2 is the continuation and publishes to the same `zip` crate name (now at 8.x — release cadence is fast, expect frequent semver-major bumps).
- **Read & write:** Stored, Deflate, Bzip2, Zstandard, XZ, PPMd. **Read-only:** Deflate64, LZMA, Implode, Shrink, Reduce. AES encryption (AE-1/AE-2), legacy ZipCrypto decryption. Multi-disk archives NOT supported.
- Security history: **CVE-2025-29787** (zip-slip via symlinks, versions 1.3.0–2.2.x, fixed in 2.3.0; Snyk severity 7.3). Current 8.x is fixed, but Nova Prism must still do its own extraction-path sandboxing (defense in depth) since we drive extraction ourselves.
- Note: enabling its `zstd` feature pulls the C `zstd` crate; the deflate path via `flate2`/`miniz_oxide` and bzip2 via `libbz2-rs-sys` stay pure Rust.

### sevenz-rust2 — read/write 7z, pure Rust
- Repo: <https://github.com/hasenbanck/sevenz-rust2> (108★). Fork of the **unmaintained** `sevenz-rust` (dyz1990); RustSec PR [#2086](https://github.com/rustsec/advisory-db/pulls) proposed flagging the original as unmaintained. Actively maintained by hasenbanck (last publish 2026-08-01), Apache-2.0.
- Codecs: LZMA/LZMA2 (via `lzma-rust2`), BZIP2, PPMd (via `ppmd-rust`), optional Brotli/Deflate/LZ4/Zstd; BCJ filters (x86/ARM/…), Delta filter; **AES-256 for both compress and decompress** (encrypted archives + encrypted headers).
- This is the only viable pure-Rust 7z writer in the ecosystem. Ecosystem migration confirms it (e.g. `mise` migrated from sevenz-rust to sevenz-rust2 and hardened extraction against traversal).
- Caveat: 0.x API, breaking changes between minors; pin and wrap behind our own trait.

### unrar (muja/unrar.rs) — extract/list RAR only
- Repo: <https://github.com/muja/unrar.rs> (124★, last publish 2025-02-19 — slow but responsive cadence; wraps a stable upstream so low churn is normal).
- Wrapper code is MIT OR Apache-2.0, but it **vendors RARLAB's unrar C++ sources**, which carry their own non-OSI freeware license (§3). Needs MSVC (C++).
- Explicitly: "This library can only extract and list archives, it cannot create them." No random access into arbitrary entries, no byte-stream input — plan UX accordingly (RAR = sequential extract path).

### tar
- Repo moved to <https://github.com/composefs/tar-rs> (maintenance continued after alexcrichton handed it off). 0.4.46, 2026-05-18, MIT OR Apache-2.0, pure Rust, streaming (never needs whole archive in memory). Cheap to include for .tar/.tar.gz/.tar.zst support.

---

## 3. The RAR legal situation (verified)

Source: RARLAB's `license.txt` shipped with the unrar sources (mirror: <https://github.com/aawc/unrar/blob/master/license.txt>). Key verbatim terms:

> "UnRAR source code may be used in any software to handle RAR archives without limitations free of charge"

> "...cannot be used to develop RAR (WinRAR) compatible archiver and to re-create RAR compression algorithm, which is proprietary"

> "Distribution of modified UnRAR source code in separate form or as a part of other software is permitted, provided that full text of this paragraph ... is included in license, or in documentation if license is not available"

Conclusions for Nova Prism:
1. **Extraction: allowed.** Shipping unrar-based RAR extraction is free of charge and explicitly permitted, including inside other software packages. We must reproduce the license paragraph in our licensing docs.
2. **Creation: legally impossible for us.** The RAR compression algorithm is proprietary; the license explicitly forbids using the unrar sources to build a RAR-compatible archiver or reverse-derive the algorithm. RARLAB does not license the algorithm to third parties — the only legal way to *create* RAR is RARLAB's own `rar.exe`/WinRAR under a paid license held by the end user. Even a "shell out to rar.exe if installed" integration is possible technically, but Nova Prism cannot ship or bundle rar.exe. Recommendation: **do not offer RAR creation at all** (matches 7-Zip's position).
3. **License compatibility:** the unrar license's field-of-use restriction makes it non-free (not OSI). It coexists fine with a permissive Nova Prism license (the restriction just rides along), but it is **GPL-incompatible** — if Nova Prism chooses GPL, unrar must be isolated (separate process/helper binary or dlopen'd plugin with its own license), or Nova Prism's GPL must carry an explicit additional-permission exception. Keep `unrar` behind a cargo feature + subprocess boundary from day one; that keeps every licensing option open.

---

## 4. Codec crates in detail

### Deflate: flate2 (+ zlib-rs backend)
- flate2 1.1.9, rust-lang org — as mainstream as it gets. Backend is selectable: `miniz_oxide` (pure Rust, default), **`zlib-rs`** (pure Rust, Trifecta Tech Foundation, Zlib license, currently the fastest safe zlib — 111M downloads on its own), zlib-ng (C). Recommendation: default features (miniz_oxide) for MVP; benchmark `features = ["zlib-rs"]` for the fast path. Either way, zero C toolchain.

### Zstd: zstd (C FFI) + ruzstd (pure decoder)
- `zstd` 0.13.3 binds real libzstd via `zstd-sys` (bundled source, built with `cc` → needs MSVC). MIT wrapper; libzstd itself is dual BSD-3-Clause/GPLv2 (use the BSD leg). Multithreaded compression via the `zstdmt` feature. Last publish 2025-02-20 — cadence is slow (tracks upstream libzstd releases) but the repo (651★) has CI green across Linux/Windows/macOS/WASM; treat as stable-mature, not abandoned.
- `ruzstd` 0.9.0 — actively developed pure-Rust *decoder*; useful if we ever want a zero-FFI build profile, not a substitute for writing.
- For .nva's default codec we want real libzstd (speed, levels 1–22, dictionaries, long-distance matching). Accept the FFI.

### XZ/LZMA: liblzma (successor of xz2) and lzma-rust2 (pure Rust)
- **xz2 0.1.7 is frozen since 2022-06-06 — do not use.**
- `liblzma` 0.4.8 (Portable-Network-Archive org) is the maintained fork: drop-in from xz2 at 0.1.x, now **bundles XZ 5.8**, optional `parallel` (multithreaded xz) feature, WASM support. MIT OR Apache-2.0. FFI (cc).
- `lzma-rust2` 0.18.1 (hasenbanck) — pure-Rust LZMA/LZMA2/LZIP/XZ ported from tukaani "XZ for Java"; it is what sevenz-rust2 uses, very actively published (2026-08-05), Apache-2.0. Slower than native liblzma but removes the C dependency entirely.
- Strategy: MVP gets LZMA support "for free" through sevenz-rust2/zip (lzma-rust2 underneath). Max-compression tier adds `liblzma` with `parallel` for fastest/strongest xz when a C toolchain is present.

**The 2024 xz backdoor lesson (CVE-2024-3094).** Malicious maintainer "Jia Tan" backdoored xz-utils 5.6.0/5.6.1 (discovered 2024-03-29 by Andres Freund); the payload hid in *binary test files* activated by build scripts. The same poisoned test files even landed in the Rust `liblzma-sys` crate (0.3.0–0.3.2; removed in 0.3.3 on 2024-04-10 — the activating build logic was never present, so Rust users were not exploited, per Snyk/The Hacker News). Durable lessons for Nova Prism:
1. Prefer pure-Rust codec implementations where performance allows (lzma-rust2, zlib-rs, libbz2-rs-sys, brotli, ppmd-rust) — smaller supply-chain and memory-safety surface.
2. Every `-sys` crate we ship must pin an exact version, be covered by `cargo audit` + `cargo vet`/`cargo deny` in CI, and be built from reviewed, checked-in sources (no network fetch at build time).
3. Treat opaque binary blobs in dependencies (test fixtures included) as a red flag; keep the dependency tree auditable.

### Brotli: dropbox/rust-brotli
- 8.0.4, publishes steadily (2026-06-14), 237.7M downloads. **License "BSD-3-Clause AND MIT"** — both permissive, fine everywhere. Pure Rust, "all included code is safe", no_std-capable, pluggable allocator. Both compressor and decompressor. Good for web-asset-heavy inputs in the max tier and for reading .zip variants; also a candidate internal filter.

### Bzip2 (legacy compat) — trifectatechfoundation/bzip2-rs
- 0.6.1 (2025-10-16), MIT OR Apache-2.0. Since 0.6 the default backend is `libbz2-rs-sys`, a **pure-Rust libbzip2** from Trifecta Tech. Needed only to read legacy .bz2/.tar.bz2/7z-bzip2 and zip-bzip2 entries. Not a codec we should ever *write* by default.

### Bzip3 — bindings `bzip3` (bczhc/bzip3-rs)
- Crate 0.12.0 (2025-12-30), **LGPL-3.0-only**, ~101K downloads — small but alive. FFI over C libbz3.
- Upstream <https://github.com/kspalaiologos/bzip3> (1.2k★, C, LGPL-3.0 with Apache-2.0 libsais inside): "a better, faster and stronger spiritual successor to BZip2", BWT + context-mixing entropy coding. Upstream README carries a loud data-loss disclaimer and notes heavy performance dependence on compiler/arch: **17–23 MiB/s per thread on x64 Linux clang; Windows and 32-bit builds notably slower** — a real concern for a Windows-first product built with MSVC.
- Verdict: attractive ratio on text, but (a) LGPL-3.0 static-linking obligations (must allow relinking → ship object files or dynamically link) are painful for a single-exe Windows archiver, (b) Windows performance is second-class upstream, (c) format stability disclaimer. Keep as an **optional, dynamically-loaded plugin at most**; do not put in the core.

### libbsc — high-end BWT coder, FFI required
- Upstream <https://github.com/IlyaGrebnov/libbsc> (350★, C++, **Apache-2.0**, copyright notices through 2025 — maintained at a slow pace, which is fine for a stable codec). BWT/ST + LZP, parallel block processing, optional CUDA (compute ≥7.5).
- **No Rust bindings exist on crates.io** (searches for "libbsc" and "bsc compression" return zero relevant crates — verified 2026-08-16). Using it means writing our own `bsc-sys` with `cc`/`bindgen` against MSVC (C++17). That is a contained, one-time cost, and Apache-2.0 is license-clean for every scenario — **preferable to bzip3 as the max-tier BWT engine**. FreeArc historically leaned on exactly this class of codec for text.

### PPMd — ppmd-rust
- 1.4.0 (2026-01-24), hasenbanck, **CC0-1.0 OR MIT-0** (public-domain-equivalent), pure Rust port of PPMd var.H/var.I (7z-compatible). Already a sevenz-rust2 dependency. Excellent text ratio at low speed — a natural "max tier, text class" pick and needed anyway for 7z PPMd entries.

---

## 5. License compatibility matrix (Nova Prism license TBD)

| Dependency license | If Nova Prism = MIT/Apache-2.0 | If Nova Prism = GPL-3.0 | Notes |
|---|---|---|---|
| MIT / Apache-2.0 / BSD-3 / Zlib (almost everything) | OK | OK | No obligations beyond attribution |
| CC0-1.0 (notify, blake3, ppmd-rust) | OK | OK | Public-domain-equivalent |
| BSD-3-Clause AND MIT (brotli) | OK | OK | Both permissive |
| libzstd BSD-3/GPLv2 dual | OK (BSD leg) | OK | — |
| **LGPL-3.0-only (bzip3 crate + libbz3)** | Risky if statically linked: must permit user relinking (§4 LGPL) — ship .obj files or dylib | OK (GPLv3 subsumes) | Main argument to prefer libbsc |
| **UnRAR freeware license (vendored in unrar crate)** | OK, must include license paragraph; restriction (no RAR archiver) rides along | **Incompatible** — isolate in separate process/plugin or add GPL exception | See §3 |

Bottom line: if every dependency except `bzip3` and `unrar` is used, Nova Prism can still pick **any** license later. Keep `unrar` behind a process boundary and prefer `libbsc` over `bzip3`, and the choice stays fully open.

---

## 6. Pure Rust vs FFI — C/C++ toolchain (MSVC) requirements

**Zero C toolchain needed (pure Rust):** zip (default features), sevenz-rust2 (default features), tar, flate2 (miniz_oxide or zlib-rs), lzma-rust2, ppmd-rust, brotli, bzip2 (libbz2-rs-sys), ruzstd, fastcdc, memmap2, rayon, notify, infer, file_format, filetime, clap, windows/windows-sys (pre-generated bindings; links system DLLs only).

**Needs MSVC (cc / cxx build):** zstd (zstd-sys), liblzma (liblzma-sys), bzip3 (libbz3), unrar (unrar_sys, C++), custom libbsc FFI (C++17), blake3 *by default* (bundled C/asm SIMD kernels via `cc`; has a portable pure-Rust fallback if the C path is disabled — keep default for the ~×4 SIMD speedup).

Practical consequence: the **MVP can build with nothing but `rustup` + MSVC Build Tools** (zstd/blake3 need cl.exe, which CI has anyway). A fully C-free build profile (`--no-default-features`-style) is achievable for audits/WASM by swapping zstd→ruzstd (read-only) and blake3→pure — worth wiring as a cargo feature from day one.

---

## 7. Infrastructure crates for .nva

- **fastcdc 4.0.1** (nlfiedler/fastcdc-rs, MIT, publish 2026-04-26) — canonical pure-Rust FastCDC; implements both the 2016 and the improved 2020 (normalized chunking) variants, plus streaming. Exactly what the .nva append-only log needs for content-defined chunking. 1.4M downloads — modest but it's *the* reference implementation.
- **blake3 1.8.6** — official crate, SIMD + rayon-based multithreaded hashing of large files; the natural chunk-ID/integrity hash for .nva (faster than SHA-256 by ~×5–10 on large inputs). License CC0/Apache — clean.
- **memmap2 0.9.11** — the maintained successor of the abandoned `memmap` (RazrFalcon fork); the standard choice for mapping huge archives. Remember: on Windows, a mapped file section blocks truncation — compaction must unmap first (design note for .nva compactor).
- **rayon 1.12.0** — data-parallel compression of independent chunks/blocks. Standard.

## 8. Platform / UX crates

- **notify 8.2.0** (CC0) — cross-platform FS watching (ReadDirectoryChangesW on Windows); for GUI auto-refresh and "watch folder → update archive" scenarios. Known Windows caveat: coarse-grained events, needs debouncing (`notify-debouncer-*` companions exist in the same org).
- **infer 0.22.0** (MIT, no_std) vs **file_format 0.29.0** (MIT/Apache): infer = tiny, magic-bytes only, ~hundreds of types, extensible with custom matchers; file_format = broader (~400 formats incl. OOXML/legacy office nuances). For the two-phase analyzer use **both**: infer for the hot path (first-bytes sniff), file_format for refinement of ambiguous containers. Neither does content entropy analysis — that's our own code.
- **filetime 0.2.29** — set mtime/atime on extraction, cross-platform. Trivial, maintained.
- **windows 0.62.2** (microsoft/windows-rs; project extremely active — monthly "Rust for Windows" newsletters through May 2026, though the umbrella `windows` crate itself publishes at a slower cadence than `windows-core`/`windows-bindgen`). COM support is first-class. Shell thumbnails confirmed available: `windows::Win32::UI::Shell::IShellItemImageFactory` ([docs](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/UI/Shell/struct.IShellItemImageFactory.html)) — enable features `Win32_UI_Shell`, `Win32_System_Com`, `Win32_Graphics_Gdi`; pattern: `SHCreateItemFromParsingName::<_, IShellItemImageFactory>(path, None)?` → `.GetImage(SIZE{cx,cy}, SIIGBF_RESIZETOFIT|...)` → caller owns the `HBITMAP` (free with `DeleteObject`); never call on the UI thread (disk access even on cache hit). Prefer `windows-sys` for plain Win32 calls in hot paths (no COM runtime cost).
- **clap 4.6.6** — 1.05 **billion** downloads, weekly cadence; derive API. Unquestioned default for the CLI.

---

## 9. Dependency shortlists

### MVP (pack/unpack zip & 7z, unpack rar, .nva v0, CLI)

```toml
# formats
zip = "8"                    # MIT — read/write zip incl. AES
sevenz-rust2 = "0.21"        # Apache-2.0 — read/write 7z incl. AES-256
tar = "0.4"                  # tar/tgz support, nearly free
unrar = { version = "0.5", optional = true }  # feature "rar", subprocess-isolated
# codecs
flate2 = "1"                 # deflate (miniz_oxide; try feature zlib-rs)
zstd = { version = "0.13", features = ["zstdmt"] }  # .nva default codec (FFI)
# .nva core
fastcdc = "4"                # content-defined chunking
blake3 = { version = "1", features = ["rayon"] }    # chunk ids / integrity
memmap2 = "0.9"
rayon = "1"
# analyzer phase 1
infer = "0.22"
file_format = "0.29"
# platform
filetime = "0.2"
windows-sys / windows = "0.62"   # features Win32_UI_Shell, Win32_System_Com, Win32_Graphics_Gdi, ...
clap = { version = "4", features = ["derive"] }
```

### Max-compression tier (adds)

```toml
liblzma = { version = "0.4", features = ["parallel"] }  # strongest xz, threads (FFI)
ppmd-rust = "1"              # PPMd var.H/I for text (pure Rust, CC0/MIT-0)
brotli = "8"                 # web assets / dictionaries (pure Rust)
# bsc-sys — OUR OWN crate wrapping libbsc (Apache-2.0, C++17/MSVC): BWT class for text
# optional plugin only, NOT core: bzip3 (LGPL-3.0) — only if libbsc benchmark loses
```

Recompression of already-compressed data (JPEG/MP3/deflate-stream reflate) has **no maintained Rust crates** — that entire area (lepton/brunsli/packMP3/precomp-style) is FFI-or-port territory and is covered by the codec-research topic, not this audit.

### Post-MVP (GUI phase)
`notify = "8"` (+ debouncer) for live folder watching.

---

## 10. Avoid list

| Crate | Reason |
|---|---|
| `xz2` / `xz` (alias) | Frozen since 2022-06-06; superseded by `liblzma` (same API at 0.1.x) |
| `sevenz-rust` (original) | Unmaintained; RustSec flagging proposed (advisory-db PR #2086); use `sevenz-rust2` |
| `lzma-rs` | Dormant since 2023-01-04; maintainer's own post-xz-backdoor blog discusses limits; use `lzma-rust2` |
| `zip < 2.3.0` | CVE-2025-29787 zip-slip (symlink traversal) |
| `memmap` (original) | Abandoned years ago; `memmap2` is the maintained fork |
| `bzip3` in core | LGPL-3.0-only + weak MSVC performance + format-stability disclaimer; plugin at most |
| `compress-tools` (libarchive) | Rejected on architecture grounds: LGPL C libarchive blob, no control over per-format behavior, contradicts pure-Rust-first supply-chain policy |
| Any RAR *creation* path | Legally impossible — see §3 |

---

## Sources

- crates.io API records for every crate above (fetched 2026-08-16)
- <https://github.com/zip-rs/zip2> · <https://github.com/zip-rs/zip-old/issues/446>
- <https://github.com/hasenbanck/sevenz-rust2> · <https://github.com/hasenbanck/lzma-rust2> · <https://github.com/hasenbanck/ppmd-rust>
- <https://github.com/muja/unrar.rs> · UnRAR license: <https://github.com/aawc/unrar/blob/master/license.txt>
- <https://github.com/Portable-Network-Archive/liblzma-rs> · <https://github.com/alexcrichton/xz2-rs>
- xz backdoor: <https://thehackernews.com/2024/04/popular-rust-crate-liblzma-sys.html> · <https://gendignoux.com/blog/2024/04/08/xz-backdoor.html>
- CVE-2025-29787: <https://security.snyk.io/vuln/SNYK-RUST-ZIP-9460813> · <https://www.sentinelone.com/vulnerability-database/cve-2025-29787/>
- <https://github.com/kspalaiologos/bzip3> · <https://github.com/IlyaGrebnov/libbsc> · <https://github.com/bczhc/bzip3-rs>
- <https://github.com/trifectatechfoundation/zlib-rs> · <https://github.com/trifectatechfoundation/bzip2-rs>
- <https://github.com/microsoft/windows-rs> ("Rust for Windows" newsletters through May 2026) · IShellItemImageFactory docs: <https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/UI/Shell/struct.IShellItemImageFactory.html> · <https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-ishellitemimagefactory-getimage>
- <https://github.com/composefs/tar-rs> · <https://github.com/rust-lang/flate2-rs> · <https://github.com/gyscos/zstd-rs> · <https://github.com/KillingSpark/zstd-rs> · <https://github.com/dropbox/rust-brotli> · <https://github.com/nlfiedler/fastcdc-rs> · <https://github.com/BLAKE3-team/BLAKE3> · <https://github.com/RazrFalcon/memmap2-rs> · <https://github.com/rayon-rs/rayon> · <https://github.com/notify-rs/notify> · <https://github.com/bojand/infer> · <https://github.com/mmalecot/file-format> · <https://github.com/alexcrichton/filetime> · <https://github.com/clap-rs/clap>
