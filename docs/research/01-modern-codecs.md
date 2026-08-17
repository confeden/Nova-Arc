# Research 01 — State-of-the-art general-purpose lossless compression (2024–2026)

*Research date: 2026-08-16. All liveness/version/benchmark claims verified against live web sources
(GitHub, LTCB, lzbench, official project pages) — links inline and at the bottom.*

Scope: codec landscape for Nova Arc (.narc), tier recommendations (fast/normal/max), the
"own entropy codec?" question, licenses, and Rust availability for every candidate.

---

## 1. Executive summary

- **zstd is the undisputed mid-tier king** and the only sane default codec in 2026: ~420 MB/s
  compress / ~1.3 GB/s decompress per thread at level 1, with decompression speed nearly flat
  (~1.1 GB/s) even at ultra level 22. Actively developed (v1.6.0, Dec 2025). Dual BSD-3-Clause/GPL-2.0.
- **LZMA/LZMA2 is still the practical ratio champion with fast decompression.** 7-Zip is alive
  (v26.02, June 2026); xz-utils is alive and healthy post-backdoor (v5.8.3, Mar 2026, 0BSD license).
  LZMA decompresses at ~90–120 MB/s — 40–50× faster than any BWT/CM codec of similar ratio class.
- **The BWT family (libbsc, bzip3, kanzi) beats LZMA on ratio for text** and compresses much faster,
  but decompression is symmetric and slow (3–15 MB/s single-thread). Good as an *opt-in* text codec,
  wrong as the max-tier backbone (extraction UX suffers).
- **Context mixing (zpaq, paq8px, cmix) and NN compressors (nncp, ts_zip) are not shippable** in a
  general-purpose archiver: 1.6 KB/s–150 KB/s compression, 7–31 GB RAM, GPU requirements (nncp),
  GPL (paq8px/cmix). They define the ratio ceiling, not the product.
- **Recommended narc codec set:** zstd (fast+normal), LZMA2 + filters (max), PPMd var.H/I and
  optionally a BWT codec as per-type specialists chosen by the analyzer. All of these exist as
  **maintained pure-Rust crates** (`zstd`/`ruzstd`, `lzma-rust2`, `ppmd-rust`, `brotli`) — narc can
  ship its Windows-first MVP with zero C dependencies if it accepts `lzma-rust2`'s encoder, or with
  one C dependency (`liblzma`, 0BSD) for the fastest/most battle-tested LZMA encoder.
- **Do NOT write a new entropy codec.** FSE/huff0 (zstd), the LZMA range coder, and rANS are all
  within ~1–3% of entropy on real data. All differentiation in 2026 comes from *modeling and
  filters* (Meta's OpenZL, Oct 2025, is the industry's confirmation of exactly this thesis — and of
  narc's planned two-phase analyze-then-compress design).

---

## 2. Codec-by-codec status (verified 2026)

### 2.1 zstd (Zstandard)

- **Status:** very active. v1.5.7 (Feb 2025): +30% compression speed on small data, multithreading
  became the CLI default, new `--max` mode, `ZSTD_compressSequencesAndLiterals()` API.
  v1.6.0 (Dec 2025): legacy-format support disabled by default.
  [Releases](https://github.com/facebook/zstd/releases) · [CHANGELOG](https://github.com/facebook/zstd/blob/dev/CHANGELOG)
- **Ultra levels:** 20–22 need `--ultra`; window up to 128 MB (level 22). Silesia: 52.3 MB (24.7%)
  at -22 vs 48.7 MB (23.0%) for LZMA -9 — zstd never quite reaches LZMA ratio, but decompresses
  ~11× faster (1073 vs 93 MB/s).
- **Long-range matching (`--long=N`):** separate LDM hash table finds matches up to 2 GB back
  (`--long=31`); default window 128 MB at `--long=27`. Decoder must be told the same window
  (memory = window size on both sides). Critical for narc: big multi-file solid blocks and
  patch-like content dedup almost for free. [Manual](https://man.archlinux.org/man/zstd.1.en)
- **Dictionaries:** `zstd --train` (COVER/fastCOVER) produces dictionaries that give 2–5× ratio
  improvement on small files (<64 KB). Perfect for narc's "many small similar files" grouping phase.
  v1.5.7 made dictionary compression ~5% faster at low levels.
- **Seekable format:** `contrib/seekable_format` — frames + jump table in a skippable frame;
  random access at frame granularity. Not part of the core lib but a stable spec with third-party
  implementations. Highly relevant for narc's random-access-into-huge-archive story.
  [Spec](https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/zstd_seekable_compression_format.md)
- **Multithreading:** native (`-T0`), job-based, scales well; MT is default in CLI since 1.5.7.
- **Memory:** level-dependent, ~10 MB (L1) to ~650 MB (L22) compress side; decompress = window size
  (8 MB default, up to 2 GB with `--long=31`).
- **License:** dual BSD-3-Clause / GPL-2.0.

### 2.2 LZMA / LZMA2, 7-Zip, xz

- **7-Zip:** alive. v24.x (2024) raised default LZMA2 dictionaries (8→16 MB, up to 64 MB defaults),
  +20–60% LZMA decode speed on ARM64, added RISC-V BCJ filter; v26.00 (Feb 2026), v26.02
  (June 2026) — MT and >64-thread improvements. License: LGPL + BSD-3 parts + unRAR clause.
  [7-zip.org](https://www.7-zip.org/download.html) · [Wikipedia](https://en.wikipedia.org/wiki/7-Zip)
- **xz-utils / liblzma:** alive and *healthier than ever* post-2024-backdoor. Maintained by Lasse
  Collin (Tukaani); v5.8.0 (Mar 2025, SSE2 decode speedups), v5.8.1 (CVE-2025-31115 fix in MT
  decoder), v5.8.2 (Dec 2025), v5.8.3 (Mar 2026, CVE-2026-34743 fix). **License: 0BSD** (core) —
  effectively public domain, ideal for embedding. [Releases](https://github.com/tukaani-project/xz/releases) ·
  [tukaani.org](https://tukaani.org/xz/)
- **Numbers (Silesia, 1 thread, EPYC 9554, lzbench):** lzma 24.09 -9: 48.67 MB (22.97%),
  4.0 MB/s comp / 93 MB/s decomp. xz -9: same ratio, 123 MB/s decomp.
- **Character:** asymmetric — slow compress, fast decompress. Exactly what an archiver's max tier
  wants. LZMA2 adds chunking → parallel compression (7-Zip and xz both do MT LZMA2).

### 2.3 brotli

- **Status:** maintained but slow-cadence. v1.2.0 (Oct 2025) after a gap that prompted a
  "Why no release in 2025?" issue; changes were mostly security/API polish.
  [Releases](https://github.com/google/brotli/releases)
- **Numbers:** Silesia -11: 50.4 MB (23.8%) at 0.58 MB/s comp / 389 MB/s decomp. Beats zstd -22 on
  ratio and decode speed, but compresses ~4× slower than LZMA -9 for a slightly worse ratio.
- **Large-window brotli (up to 1 GB window)** exists but is explicitly a research derivative, not
  for production. Standard window is only 16 MB — weak for big archives.
- **Verdict for narc:** dominated by zstd (speed side) and LZMA (ratio side) for archiver use.
  Its niche (web assets, small-window streaming, 96% browser support) is not narc's niche.
  Only value: 7z-compat (sevenz-rust2 supports brotli-in-7z as an extension). License: MIT.

### 2.4 bzip3

- **What:** BWT (via libsais) + LZP + order-0 context-mixed arithmetic coder, by Kamila Szewczyk.
  [GitHub](https://github.com/kspalaiologos/bzip3)
- **Status:** maintained; v1.5.3 (Aug 2025). ~17 MB/s comp / 23 MB/s decomp *per thread* (x64
  Linux, author's numbers); block-parallel via `-j`. Author notes Windows builds are
  "considerably slower" — a red flag for a Windows-first product.
- **Numbers:** Silesia ~55.7 MB at -1 (lzbench); at max block with 16 threads on a 9950X:
  47.26 MB in 2.3 s (kanzi README cross-benchmark) — good MT ratio/speed, but decompression is
  symmetric-slow.
- **License: LGPL-3.0** — usable but the least convenient license in the candidate set for a
  statically-linked Rust binary (relink obligation). Rust: `bzip3` crate = C FFI bindings,
  LGPL-3.0, small user base. Also carries a loud data-loss disclaimer and had fuzzing CVEs in 2023.

### 2.5 libbsc (BWT family flagship)

- **What:** block-sorting compressor by Ilya Grebnov (also author of libsais): BWT/ST + QLFC.
  GPU (CUDA) acceleration for ST/BWT on compute ≥7.5.
- **Status:** active; v3.3.12 (Sep 2025), copyright through 2025. **License: Apache-2.0.**
  [GitHub](https://github.com/IlyaGrebnov/libbsc) · [libbsc.com](http://libbsc.com/)
- **Numbers:** Silesia -m5 -e1: 49.5 MB (23.4%), 28.8 MB/s comp / 13.3 MB/s decomp (1 thread).
  On enwik9 (LTCB): 163.9 MB — crushes xz's 197.3 MB on text. Memory: 16 MB + 5× block size per
  parallel block.
- **Rust:** **no crate exists** (verified on crates.io/lib.rs) — would need self-maintained
  bindgen FFI. C API is clean, so feasible, but it's an ongoing maintenance commitment.

### 2.6 kanzi

- **What:** modular multi-transform compressor (BWT/CM/RLT/TEXT/rANS...), C++/Go/Java.
  Apache-2.0. Very active: kanzi-cpp 2.5.3 (Apr 2026), 2,700+ commits.
  [kanzi-cpp](https://github.com/flanglet/kanzi-cpp)
- **Numbers (Silesia):** 1 thread (lzbench, kanzi 2.3 -9): 41.8 MB (19.73%) — **best practical
  ratio of anything measured** — at 2.4 MB/s comp / 2.35 MB/s decomp. MT on 9950X (author's bench,
  v2.5): -l 7 = 47.3 MB in 1.15 s, -l 9 = 41.5 MB in 11.6 s encode / 12.4 s decode.
  enwik9 in ~1 GB blocks: 161.7 MB at ~2 MB/s.
- **Verdict:** the most interesting "ultra-text" engine of 2026 — Pareto-superior to xz on
  compression side, Apache-licensed, alive. But: C++ (no stable C API documented, no Rust crate),
  symmetric slow decode, and its container/stream format is its own. Candidate for a later
  opt-in "ultra" tier via FFI, not for the v1 core.

### 2.7 PPMd (var. H / var. I)

- The only PPM still in production use (7-Zip `-m0=PPMd`, WinZip method 98). Silesia
  (ppmd8 24.09 -4): 51.2 MB (24.2%), 13.2 MB/s comp / 12.0 MB/s decomp — beats LZMA on plain text
  ratio-per-CPU-second, loses on binaries and decode speed (symmetric).
- ppmonstr/PPMd var. J (Shkarin) is stronger (157.0 MB enwik9) but closed-source freeware,
  abandoned — not usable.
- **Rust: `ppmd-rust`** (hasenbanck) — pure-Rust port of 7-Zip's PPMd7(H)+PPMd8(I), Miri-validated,
  **CC0-1.0/MIT-0**, maintained (it's the PPMd engine inside sevenz-rust2). This makes PPMd
  essentially free to adopt for both 7z-compat and narc's own text tier.
  [ppmd-rust](https://github.com/hasenbanck/ppmd-rust)

### 2.8 Context mixing: zpaq / zpaqfranz, paq8px, cmix, mcm

| Program | Status 2026 | enwik9 (LTCB, upd. Jul 2026) | Speed / RAM | License |
|---|---|---|---|---|
| zpaq 7.15 | abandoned (2016) | 142.3 MB (v6.42, mid method) | ~150 KB/s comp, 14 GB (max method) | Public domain/MIT (libzpaq) |
| **zpaqfranz** v60.x (Feb 2026) | **very active** | same engine (`-715` compatible) | MT I/O, HW-SHA ~270 MB/s dedup path | MIT |
| paq8px v216 | active (research) | 124.7 MB @ -12L | ~3.4 KB/s, up to 29 GB RAM | GPL-2.0+ |
| cmix v21 (2024) | active (research) | 108.0 MB | ~1.6 KB/s, 31 GB RAM | GPL |
| mcm v0.84 | dormant since 2016 | — | ~1 MB/s class CM-LZP | GPL |

- zpaqfranz matters to narc **not as a codec but as a competitor**: journaling append-only archive
  + CDC dedup + MT — the closest existing thing to the .narc concept. Its *compression* tiers,
  however, top out at zpaq method 5 (slow, streaming-CM).
  [zpaqfranz](https://github.com/fcorbelli/zpaqfranz)
- paq8px/cmix: 1.5–3 orders of magnitude too slow to ship; GPL; RAM in tens of GB. They are the
  reference ceiling (0.86–1.0 bpb on enwik9) against which narc's max tier can be honest about
  what it is *not*.

### 2.9 NN/LLM compressors: nncp, ts_zip

- **nncp v3.3** (Bellard, Jun 2024): Transformer + LibNC/CUDA. LTCB #1: enwik9 → 106.6 MB
  (0.853 bpb), but ~242,000 ns/byte ≈ **4 KB/s** → ~2.8 days per GB, 7.6 GB RAM, GPU required for
  sane speed. [bellard.org/nncp](https://bellard.org/nncp/)
- **ts_zip** (Bellard): RWKV-169M LM as the model; enwik9 at 1.084 bpb vs xz 1.707; up to
  **1 MB/s on an RTX 4090**, text-only, experimental, no format stability guarantees.
  [bellard.org/ts_zip](https://bellard.org/ts_zip/)
- **Verdict:** deterministic and reproducible, yes — but GPU-dependent speed, no format stability,
  text-only, and multi-day archive jobs make these research vehicles, not archiver codecs.
  Nothing here is shippable in narc through at least the 2020s. (FineZip and other LLM-compression
  papers, 2024–2026, confirm: even "practical" LLM compression is ~10⁴× slower than zstd.)

### 2.10 OpenZL (context, not a codec to embed yet)

Meta open-sourced **OpenZL** (Oct 2025, BSD): a *format-aware* compression framework — DAG of
transforms described per data format, trained "compression plans", universal self-describing
decoder, Pareto gains over zstd/xz on structured data.
[Meta engineering blog](https://engineering.fb.com/2025/10/06/developer-tools/openzl-open-source-format-aware-compression-framework/)
This is the strongest possible industry validation of narc's planned two-phase
(analyze → per-type plan → compress) architecture. Worth tracking; possibly worth embedding for
columnar/structured data once its API stabilizes.

### 2.11 The old guard narc must beat (for calibration)

- **FreeArc** (Bulat Ziganshin): last stable 0.666 (2010), FreeArc-Next 0.11 (Oct 2016), site dead
  since 2016, repo unmaintained — **abandoned**. Its magic was per-type method dispatch + filters
  (dict, delta, BCJ, mm, precomp integration) over tornado/LZMA/PPMd/GRZip — i.e. exactly the
  "analysis phase" narc plans, with 2010-era codecs. [FreeArc (Wikipedia)](https://en.wikipedia.org/wiki/FreeArc)
- **RAZOR** (Christian Martelock, v1.03.7): excellent ROLZ/LZ ratio + fast decode, but
  closed-source, dormant hobby project — study, don't depend.

---

## 3. Consolidated benchmark tables

### 3.1 Silesia corpus, single thread (lzbench 2.0.1, AMD EPYC 9554, gcc 14.2; 211,947,520 B input)

| Codec (version, level) | Comp MB/s | Decomp MB/s | Size | Ratio |
|---|---:|---:|---:|---:|
| memcpy | 16,332 | 16,362 | 211,947,520 | 100.0% |
| lz4 1.10 (default) | 577 | 3,716 | 100,880,800 | 47.6% |
| zstd 1.5.6 -1 | 422 | 1,347 | 73,421,914 | 34.6% |
| zstd -2 | 344 | 1,246 | 69,503,444 | 32.8% |
| libdeflate 1.23 -6 | 84 | 912 | 67,510,615 | 31.9% |
| zstd -5 | 125 | 1,197 | 63,040,310 | 29.7% |
| brotli 1.1 -5 | 37 | 451 | 59,555,446 | 28.1% |
| bzip2 1.0.8 -9 | 13.1 | 37.5 | 54,572,811 | 25.8% |
| zstd -18 | 3.8 | 1,169 | 53,329,873 | 25.2% |
| zstd -22 (ultra) | 2.1 | 1,073 | 52,333,880 | 24.7% |
| ppmd8 (24.09) -4 | 13.2 | 12.0 | 51,241,932 | 24.2% |
| brotli -11 | 0.58 | 389 | 50,407,795 | 23.8% |
| bsc 3.3.5 -m5 -e1 | 28.8 | 13.3 | 49,522,504 | 23.4% |
| **lzma 24.09 -9** | 4.0 | **93** | **48,674,973** | **23.0%** |
| kanzi 2.3 -9 | 2.4 | 2.4 | **41,807,652** | **19.7%** |

Reading: on the *decompression-speed × ratio* Pareto front the only survivors are
**zstd (any level) → brotli -11 → LZMA -9**. BWT/CM (bsc, kanzi, ppmd) win ratio but fall off the
decode-speed cliff (2–13 MB/s).

### 3.2 Multithreaded reality check (16T, Ryzen 9950X, Silesia; kanzi-cpp README, v2.5 / bzip3 1.5.1)

| Codec | Enc time | Dec time | Size |
|---|---:|---:|---:|
| zstd -2 -T16 | ~0.1 s | — | ~69.4 MB |
| kanzi -l 5 | 0.53 s | 0.26 s | 53.9 MB |
| kanzi -l 7 | 1.15 s | 0.89 s | 47.3 MB |
| bzip3 -j 16 (max blk) | 2.35 s | 2.22 s | 47.3 MB |
| kanzi -l 9 | 11.6 s | 12.4 s | 41.5 MB |

MT flattens the *compression*-time gap; it does nothing for the fundamental decode asymmetry
(LZ decodes fast on one thread; BWT/CM needs all cores just to keep up).

### 3.3 Large Text Compression Benchmark (enwik9 = 10⁹ B; LTCB, last update 2026-07-08)

| Program | enwik9 size | Comp speed | RAM |
|---|---:|---:|---:|
| nncp v3.2 (Transformer, GPU) | 106.6 MB | ~4 KB/s | 7.6 GB |
| cmix v21 | 108.0 MB | ~1.6 KB/s | 31 GB |
| paq8px v206 -12L | 124.7 MB | ~3.4 KB/s | 28 GB |
| zpaq 6.42 (max) | 142.3 MB | ~150 KB/s | 14 GB |
| ppmonstr J | 157.0 MB | ~280 KB/s | 1.7 GB |
| kanzi (1 GB block, TEXT+RLT) | 161.7 MB | ~2 MB/s | 3.1 GB |
| bsc 3.25 | 163.9 MB | ~43 MB/s (MT) | 5 GB |
| xz -9 class | 197.3 MB | ~170 KB/s | 6 GB (comp) |
| ts_zip (RWKV-169M, RTX 4090) | ~135 MB (1.084 bpb) | ≤1 MB/s (GPU) | 4 GB+GPU |

[LTCB](https://mattmahoney.net/dc/text.html)

---

## 4. Recommendation: narc codec tiers

Principle: **narc's edge is the analyzer + container, not exotic codecs.** Every tier must keep
decompression fast except where the user explicitly opts into symmetric codecs for text.

| Tier | Codec(s) | Settings sketch | Why |
|---|---|---|---|
| **store** | none (+ CDC dedup) | — | .narc log handles it |
| **fast** | **zstd** | levels 1–6, `-T0`, LDM auto-on for blocks >128 MB | 400+ MB/s in, 1.3 GB/s out; nothing else is close |
| **normal** (default) | **zstd** | levels 15–19, `--long=27`, trained dictionaries for small-file groups | 25–26% Silesia with still-instant extraction; dictionary + LDM synergize with CDC chunking |
| **max** | **LZMA2 + filters**; analyzer routes: text→**PPMd8** or BWT, exe→BCJ/BCJ2+LZMA2, already-compressed→store/recompression path | LZMA2 dict 64–768 MB, MT block split; PPMd order 8–16 | Matches 7-Zip's ratio with fast (90+ MB/s) extraction; PPMd wins plain text cheaply |
| **ultra** (later, opt-in, clearly labeled "slow to extract") | BWT engine: libbsc (Apache, FFI) or kanzi (Apache, C++ FFI) for text/logs/source | block 100–1000 MB, all cores both directions | 19–23% Silesia, beats everything practical on text; symmetric cost is acceptable only opt-in |

This set beats FreeArc's codec roster (2010-era tornado/GRZip/LZMA/PPMd) on every axis while using
only 3 codec families, and matches 7-Zip max while extracting equally fast. The "smarter than
7-Zip on JPEG/MP3/deflate" goal is NOT solved by any general codec in this report — it requires
recompression filters (precomp/brunsli/packMP3 class), covered in a separate research doc; the
analyzer must detect such data and *never* waste LZMA time on it (7-Zip's classic failure).

## 5. Should narc write its own entropy codec? **No.**

- Modern entropy coding is solved: FSE/huff0 (zstd), rANS (kanzi, many), range coder (LZMA),
  CM arithmetic (bzip3). All sit within 1–3% of the modeling-determined optimum; the *model*
  decides the ratio, the entropy stage decides only speed.
- A new entropy codec = years of fuzzing/hardening (see bzip3 CVEs 2023, xz CVE-2025-31115 — even
  mature codecs still ship decoder bugs) for ≈0% user-visible gain.
- Where narc should spend that budget instead: (a) content analysis + routing, (b) CDC/dedup,
  (c) recompression filters for JPEG/MP3/deflate, (d) BCJ/delta/table filters. This is the OpenZL
  thesis, now industry-proven.
- If a future custom filter needs a raw entropy stage, use an existing rANS/FSE implementation
  (e.g. via zstd's public FSE, or a Rust rANS crate) rather than inventing one.

## 6. Licenses and Rust availability (recommended + evaluated codecs)

| Codec | Upstream license | Rust path (2026) | Maturity | Notes |
|---|---|---|---|---|
| zstd | BSD-3-Clause / GPL-2.0 dual | `zstd` crate (C FFI, MIT) — mature; `ruzstd` (pure Rust, MIT) — decode complete, ~1.4–3.5× slower, encoder young; Trifecta Tech "zstd in Rust" underway | High | Seekable: `zeekstd` (BSD-2, active, spec-current); `zstd-seekable`, `zstd-framed` less active |
| LZMA/LZMA2 (xz) | **0BSD** (liblzma) | `liblzma` crate (maintained fork of dormant `xz2`); `xz` crate (c2rust pure-Rust liblzma); **`lzma-rust2`** (pure Rust enc+dec, powers sevenz-rust2, ~50% decode speedups recently) | High | 0BSD = zero obligations. Pure-Rust encoder exists — rare luxury |
| PPMd7/8 | 7-Zip public-domain lineage | **`ppmd-rust`** (pure Rust, CC0-1.0/MIT-0, Miri-validated, maintained) | Good | Same crate serves 7z- and zip-compat and narc's own text tier |
| brotli | MIT | `brotli` (Dropbox, pure Rust, BSD-3/MIT, maintained, safe-by-default) | High | Not needed in tiers; free to expose for 7z-ext compat |
| bzip2 (compat only) | bzip2-style | `bzip2` crate; `libbz2-rs-sys` pure-Rust backend (Trifecta) | High | Legacy compat only — dominated by everything |
| libbsc | Apache-2.0 | **none** — self-maintained bindgen FFI required | C API clean | Best ultra-text candidate; CUDA optional |
| kanzi | Apache-2.0 | **none** — C++ FFI, no stable C API documented | — | Best practical ratio (19.7% Silesia); FFI cost high |
| bzip3 | LGPL-3.0 | `bzip3` crate (FFI, LGPL-3.0) | Low usage | Rejected (license + Windows perf + history) |
| zpaq/libzpaq | Public domain / MIT (zpaqfranz) | none | — | Competitor study, not a codec |
| paq8px / cmix | GPL | none | — | 3.4 KB/s / 1.6 KB/s — non-shippable |
| nncp / ts_zip | Bellard, source available | none | — | GPU, KB/s–1 MB/s, format-unstable |
| OpenZL | BSD | none yet (C/C++) | New (2025) | Watch; architectural validation of narc |

Ecosystem synergy note: **sevenz-rust2** (pure-Rust 7z read/write: LZMA/LZMA2/PPMd/BCJ/BCJ2/delta,
plus zstd/brotli extensions) already exists and is active — narc's mandated 7z pack/unpack support
and its own max tier can share the exact same `lzma-rust2` + `ppmd-rust` crates.
[sevenz-rust2](https://github.com/hasenbanck/sevenz-rust2)

## 7. Rejected options (negative knowledge)

See structured summary; short list: bzip3 (LGPL, Windows-slow, corruption history), brotli as a
tier codec (dominated), zstd-ultra as the max tier (loses ~2pp ratio to LZMA), BWT as default max
(decode too slow), zpaq engine (150 KB/s, abandoned upstream), paq/cmix/mcm (speed/RAM/GPL),
nncp/ts_zip (GPU, days-per-GB), ppmonstr (closed, dead), writing a custom entropy codec
(no gain, high risk), `xz2` crate (dormant — use `liblzma`), FreeArc/RAZOR code reuse
(abandoned/closed).

## 8. Sources

- lzbench in-memory benchmark (Silesia, EPYC 9554): https://github.com/inikep/lzbench
- Large Text Compression Benchmark (upd. 2026-07-08): https://mattmahoney.net/dc/text.html
- zstd releases/changelog: https://github.com/facebook/zstd/releases · seekable spec:
  https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/zstd_seekable_compression_format.md
- xz-utils (0BSD, v5.8.3): https://github.com/tukaani-project/xz/releases · https://tukaani.org/xz/
- 7-Zip 26.02: https://www.7-zip.org/download.html
- brotli v1.2.0: https://github.com/google/brotli/releases
- kanzi-cpp (2.5.x, benchmarks): https://github.com/flanglet/kanzi-cpp
- libbsc 3.3.12: https://github.com/IlyaGrebnov/libbsc
- bzip3: https://github.com/kspalaiologos/bzip3
- zpaqfranz: https://github.com/fcorbelli/zpaqfranz
- paq8px: https://github.com/hxim/paq8px · cmix: https://github.com/byronknoll/cmix
- nncp: https://bellard.org/nncp/ · ts_zip: https://bellard.org/ts_zip/
- OpenZL: https://engineering.fb.com/2025/10/06/developer-tools/openzl-open-source-format-aware-compression-framework/
- MaskRay, "Benchmarking compression programs" (Aug 2025): https://maskray.me/blog/2025-08-31-benchmarking-compression-programs
- Rust crates: https://crates.io/crates/zstd · https://crates.io/crates/ruzstd ·
  https://crates.io/crates/zeekstd · https://crates.io/crates/liblzma ·
  https://github.com/hasenbanck/sevenz-rust2 · https://github.com/hasenbanck/ppmd-rust ·
  https://github.com/dropbox/rust-brotli · https://crates.io/crates/bzip3
- FreeArc status: https://en.wikipedia.org/wiki/FreeArc · https://github.com/Bulat-Ziganshin/FA
