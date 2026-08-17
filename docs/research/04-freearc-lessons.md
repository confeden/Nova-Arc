# 04 — FreeArc Lessons: Filter-Based Compression Architecture for narc

Research date: 2026-08-16. All liveness/version claims verified against live web sources on this date.

---

## 1. Executive summary

FreeArc (Bulat Ziganshin, 2004–2016, GPL-2.0, Haskell + C++) beat 7-Zip and RAR at equal speed
not with a better entropy coder but with an **orchestration layer**: file-type routing, filter
chains, and cheap large-granularity preprocessing before the expensive codec. On the
compressionratings.com corpus, FreeArc `-mx` reached ratio **4.668 @ 2.7 MB/s** where 7-Zip 9.12
`-mx -md48m` reached **4.232 @ 2.8 MB/s** — ~10% better ratio at the same speed, and at the fast
end FreeArc `-m4` got ratio 4.334 @ 9.9 MB/s vs 7-Zip `-mx3` 3.389 @ 7.8 MB/s (better ratio AND
faster). The mechanism, not the codecs, is what narc should copy.

The project is **dead** (last FreeArc release 0.666/2010, FreeArc'Next 0.11 released 2016-10-08,
zero commits or maintainer replies since; a GitHub issue from Aug 2024 sits unanswered). Every
individual component now has a **maintained modern equivalent**, several of them native Rust
(preflate-rs, lepton_jpeg_rust). narc should reimplement the architecture, not port the code
(GPL-2.0 + Haskell makes reuse impractical and license-viral).

Key validation from competitors, 2023–2024: 7-Zip 23.00 added per-file executable parsing and an
ARM64 filter, 24.03 added a RISC-V filter; WinRAR 7.0 (2024) added 64 GB dictionaries plus a
"long range search" mode — which is exactly FreeArc's `rep` filter idea, adopted by RAR 15 years
later. The industry converged on FreeArc's design.

---

## 2. How FreeArc dispatched methods per file type

### 2.1 The three-layer mechanism

1. **`arc.groups`** — an ordered list of filename masks/extensions that (a) defines the *solid-block
   sorting order* (similar files adjacent → better solid compression) and (b) tags each file with a
   *compression group*: `$text`, `$binary` (default), `$obj`, `$exe`, `$compressed`, `$wav`, `$bmp`,
   `$iso`, `$jpg`, `$precomp`…
2. **`arc.ini` / built-in method substitutions** — a macro language mapping (level × group) → a
   concrete **method chain** written as `filter1 + filter2 + codec:params`. Users could define
   named chains (`super = precomp+ccm/$jpg=jpg`) and invoke `-m=super`. `[External compressor]`
   sections wrapped any external binary (precomp, ecm, ppmonstr) as a first-class chain element.
3. **Command-line algebra** — `-mc` edited chains per group without redefining them:
   `-mc-rep` (drop rep everywhere), `-mc$wav=tta`, `-mc-$compressed` etc.

### 2.2 The actual built-in chains (verbatim from `Compression.hs`, FreeArc 0.67)

Top-level: level `#` (3–9) expands to `#rep + exe + #xb  /  $obj=#b  /  $text=#t`, i.e. default
data gets rep→exe→delta→lzma; `$text` gets the text branch; `-m#x` variants drop slow-decoding
parts (asymmetric modes with 8–10× lower unpack memory).

| Group | Level | Chain (verbatim) |
|---|---|---|
| binary default | 9 | `rep:2040m + exe + delta + lzma:255m:max` |
| binary default | 4 | `rep:96m + exe + delta + lzma:96m:normal:mc16` |
| $text | 4 | `dict:p:64m:80% + lzp:64m:65:d1m:s16:h20:90% + ppmd:8:96m` |
| $text | 9 | `dict:p:128m:80% + lzp:160m:145:d1m:s32:h23:92% + ppmd:16:384m` |
| $text fast | 3 | `dict:p:64m:85% + lzp:64m:24:h20:92% + grzip:m3:8m:l` |
| $wav | 3+ | `tta` (fast modes: `tta:m1`) |
| $bmp | 4+ | `mm + grzip:m1:l2048:a` (fast: `mm:d1 + tor:3:t0`) |
| $compressed | 4 | `rep:96m + tor:c3` — cheap LZ pass only, no LZMA waste |
| max external (`-m9p`) | 9 | `rep + exe + delta` → `pmm:25:2040m:r1` (ppmonstr) |

Observations that matter for narc:

- **`rep` runs first, on raw data**, with a dictionary 8–20× bigger than the codec's (2040 MB rep
  in front of 255 MB LZMA). Long-range redundancy is removed cheaply so the expensive codec only
  sees what it can actually model.
- **Already-compressed data is never fed to LZMA.** The `$compressed` group gets a
  ~100 MB/s `rep+tornado` pass that captures stored/duplicated regions and costs almost nothing.
  This is the answer to "handle JPEG/MP3/deflate smarter": route, don't brute-force.
- **Every filter has per-level parameter scaling** and a global memory governor (`-lc`/`-ld`
  capped compression/decompression memory at 75% RAM, auto-shrinking dictionaries).
- Fallback logic: `arc.groups` line `$precomp $compressed` = "use $precomp if the current method
  defines it, else $compressed" — graceful degradation between profiles.

Weakness to fix in narc: grouping was **extension-based**, only partially content-based (planned
for 0.50, never shipped; misrouted DDS textures through the exe filter — see dispack below).
narc's phase-1 analyzer must detect by content (magic bytes + entropy/stride probes + PE/ELF
header parse), which is also what 7-Zip 23.00 now does for executables.

---

## 3. FreeArc's filters and codecs — what each one did

| Component | Type | What it does | Typical gain | Speed (era HW) | Modern equivalent (2026, verified alive) |
|---|---|---|---|---|---|
| **rep** | filter | LZ77 with huge window (up to 2 GB), finds only *long* matches (≥ ~512 B) via rolling hash; tiny memory per window byte | 2–10× on redundant corpora (game repacks, VM images); ~0 on unique data | very fast, ~100+ MB/s | zstd `--long` LDM (128 MB window); WinRAR 7 "long range search"; srep (frozen); narc's own CDC dedup covers the ≥ chunk-size half |
| **dict** | filter | text: replaces frequent words with 1–2-byte codes before codec | +3–10% on text before LZ; less before PPMd | fast | kanzi `TEXT` transform (kanzi-cpp 2.1+, active); XWRT (dormant) |
| **lzp** | filter | text: removes long repeats with order-1 context prediction, shrinks PPMd input | +2–5% and big PPMd speedup | fast | kanzi `LZP`; usually subsumed by LDM+strong codec today |
| **delta** | filter | auto-detects tables/records in binary data (stride 2/4/8…), subtracts columns | +5–30% on DB tables, structured binaries | fast | 7-Zip `Delta:{N}` (fixed stride, manual); kanzi has none as auto — **auto-stride detection is a differentiator narc should build** |
| **exe (bcj)** | filter | x86 E8/E9 call/jump rel→abs conversion | +5–8% on x86 code (Pavlov's own number) | ~GB/s, trivial | 7-Zip BCJ/BCJ2/ARM64/RISCV; xz filters; ~200 LOC to reimplement |
| **dispack** | filter | full x86 disassembly, splits opcode/operand streams (from kkrunchy) | +2–4% over BCJ on plain 32-bit PE; **hurts** on non-x86 data, 32-bit PE only | medium | none maintained; 7-Zip chose header-parse + BCJ2 instead — see §5 |
| **mm** | filter | multimedia: channel separation + delta for PCM/BMP-like data, autodetected stride | large vs raw LZ on uncompressed audio/images | fast | 7z Delta (manual); kanzi `MM`; or route to FLAC/PNG-style predictors |
| **tta** | codec | TrueAudio lossless codec for `$wav` | ~30–40% smaller WAV vs ~10–15% for generic LZ | 3–10 MB/s then | **FLAC** (BSD, active, faster+smaller per Hydrogenaudio comparisons); TTA itself is dead (GPL/LGPL, unmaintained) |
| **grzip** | codec | BWT+WFC/ST4 block codec for fast text modes | bzip2-class ratio at high speed | fast | **libbsc** v3.3.12 (2025, Apache-2.0, GPU-optional, libsais); **bzip3** 1.5.3 (2025-08, LGPL-3.0) |
| **tornado** | codec | fast LZ77 family, `tor:1..16` — the "zstd before zstd" | gzip..lzma-lite ratios at 4–100+ MB/s | fast | **zstd** (superior on every axis today) |
| **4x4** | wrapper | splits stream into blocks, compresses N blocks in parallel with any inner method (`4x4:lzma:...`); also the `#$compressed` MT trick | ~linear MT scaling at small ratio loss | — | standard block-parallel design (zstdmt, xz -T, 7-Zip); narc gets it naturally from chunk groups |
| **lzma** | codec | 7-Zip's LZMA with Bulat's match-finder improvements + BT4 tweaks | baseline strong codec | — | liblzma/7-Zip LZMA2; FLZMA2 (fast-lzma2) for MT |
| **ppmd** / pmm | codec | PPMd var.I / ppmonstr for `$text` | best text ratio of its era | slow, symmetric | 7-Zip PPMd (still shipped); modern alternative: BWT stack (libbsc) or CM (kanzi TPAQ) — RAR *dropped* PPMd in RAR5 as "too slow on modern multi-core" |
| **precomp/ecm** (external) | filter | deflate/CD-image expansion before compression | +20–50% on zip/pdf/png-bearing data | slow | **preflate-rs** (Microsoft, Rust, v0.7.x, active) — see §6 |

Sources: [FreeArc 0.40 docs](http://freearc.sourceforge.net/FreeArc040-eng.htm),
[Introduction](https://freearc.sourceforge.net/Introduction.htm),
[Compression.hs](https://github.com/svn2github/freearc/blob/master/Compression.hs),
[FAZip announcement](https://groups.google.com/g/freearc-announces/c/-Ujl6KKF5yI),
[BCJ algorithm](https://en.wikipedia.org/wiki/BCJ_(algorithm)),
[kanzi-cpp releases](https://github.com/flanglet/kanzi-cpp/releases),
[libbsc releases](https://github.com/IlyaGrebnov/libbsc/releases),
[bzip3 releases](https://github.com/iczelia/bzip3/releases).

### Why FreeArc won at equal speed — the causal chain

1. **Speed budget spent where it pays.** rep/delta/exe/mm run at 100 MB/s–GB/s and shrink or
   reshape data so the 2–5 MB/s codec sees less of it and models it better. Rivals spent 100% of
   the budget inside one codec.
2. **No wasted effort on incompressible data.** `$compressed` routing alone is worth double-digit
   percent of total time on real user data.
3. **Right codec per class**: PPMd-family for text, LZMA for binary, BWT for fast-text, TTA for
   audio — each 5–20% ahead of one-size-fits-all on its class.
4. **Solid-block sorting** (arc.groups order) so similar files share context.
5. **Smart solid updates**: only changed solid blocks recompressed — the same requirement narc's
   append-only log already solves more radically.

Benchmark evidence (same corpus, [compressionratings.com FreeArc](https://compressionratings.com/i_freearc.html) / [7-Zip](https://compressionratings.com/i_7-zip.html)):

| Archiver, mode | Ratio | Comp | Decomp |
|---|---|---|---|
| FreeArc 0.666 `-m1` | 2.790 | 92.2 MB/s | 64.4 MB/s |
| FreeArc `-m4` | **4.334** | 9.9 MB/s | 34.1 MB/s |
| 7-Zip 9.12 `-mx3` | 3.389 | 7.8 MB/s | 29.0 MB/s |
| FreeArc `-mx` | **4.668** | 2.73 MB/s | 14.3 MB/s |
| 7-Zip 9.12 `-mx -md48m` | 4.232 | 2.81 MB/s | 31.0 MB/s |

(Note 7-Zip's 2× decompression-speed edge at max — FreeArc's `-m#x` asymmetric modes existed
precisely to trade that back. narc's default tier should stay decompression-fast; only the max
tier may use symmetric/slow-unpack methods, clearly labeled.)

---

## 4. Liveness audit (verified 2026-08)

| Project | Last activity | License | Verdict |
|---|---|---|---|
| FreeArc 0.666/0.67 | 2010 (0.666); sources mirrored at [svn2github/freearc](https://github.com/svn2github/freearc) | GPL-2.0 | **Dead.** Site gone, binaries only on archive.org |
| FreeArc'Next ([Bulat-Ziganshin/FA](https://github.com/Bulat-Ziganshin/FA)) | Release 0.11, 2016-10-08; issue #31 (Aug 2024) unanswered; encode.su thread alive but community-only | GPL-2.0 | **Dead.** Never reached feature parity (no .arc support, no archive modification, full-RAM extraction). Valuable as *design notes*: dedup, zstd, Lua config, CELS codec API |
| FAZip (standalone filter/codec CLI) | v0.3, Dec 2013 | GPL | **Dead.** Concept (expose each filter as standalone tool) worth copying for narc debugging |
| Community forks (M-Gonzalo/FreeArc, j2969719/freearc-old) | mirrors/build fixes only | GPL | No development |
| PeaZip | **Alive** — 11.2.0 (2026), backends 7z/p7zip 26.02 | LGPL | Bundles FreeArc *binaries* as one legacy backend; not developing it. Proof there is demand for a maintained successor |
| 7-Zip | **Alive** — 26.02 (2026-06); 23.00 ARM64 filter + per-file exe parsing, 24.03 RISCV filter, 24.09 larger LZMA2 dicts | LGPL + unRAR restriction | Reference implementation for BCJ2/Delta/ARM64/RISCV filters |
| WinRAR | **Alive** — 7.x (2024+): 64 GB dict, long-range search (`rep` reborn), exhaustive search `-mcx`; dropped RAR4 creation | proprietary | RAR5 kept only delta + x86 + ARM filters; dropped Itanium/text/audio/truecolor as obsolete |
| srep | frozen ~2014 | — | Superseded for narc by CDC dedup + rep |
| precomp-cpp | dormant; author redirected users to Rust successors | Apache-2.0 | Superseded by preflate-rs |
| preflate-rs ([microsoft/preflate-rs](https://github.com/microsoft/preflate-rs)) | **Alive**, v0.7.x 2025, Rust | Apache-2.0 | Use it |
| lepton_jpeg_rust ([microsoft/lepton_jpeg_rust](https://github.com/microsoft/lepton_jpeg_rust)) | **Alive**, 2025, Rust (crate `lepton_jpeg`) | Apache-2.0 | Use it |
| brunsli | dormant standalone; absorbed as JPEG XL Annex M (`cjxl --lossless_jpeg`) | MIT | Alternative JPEG path via libjxl |
| packMP3 | stable, unmaintained ([packjpg GitHub](https://github.com/packjpg)) | LGPL-3.0 (relicensing offered by author) | Only game in town for MP3 (~16% gain) |
| kanzi-cpp | **Alive**, 2.1.x (EXE codec rewritten for x86+ARM64, UTF codec, TEXT/MM/RLT transforms) | Apache-2.0 | Design reference for transform pipeline + content detection |
| libbsc | **Alive**, v3.3.12 2025 | Apache-2.0 | grzip successor (fast-text/BWT tier) |
| bzip3 | **Alive**, 1.5.3 2025-08 | LGPL-3.0 | alternative BWT tier |
| RAZOR (C. Martelock) | demo binary, closed source, v2 "when done" | proprietary | Existence proof: rep-style dedup (1 GB window) + ROLZ + exe filter ≈ CM ratio at LZMA decode speed. Architecture to emulate, nothing to reuse |

**License consequence for narc:** FreeArc code (GPL-2.0, Haskell/C++) must not be linked or
translated line-by-line if narc wants MIT/Apache dual licensing. Chains, parameters, and
algorithm *ideas* are not copyrightable — reimplement from documentation and papers. All
recommended Rust dependencies above are Apache-2.0/MIT except packMP3 (LGPL-3.0 — dynamic-link or
subprocess isolation, or negotiate; author explicitly invites relicensing requests) and bzip3
(LGPL-3.0, same treatment; prefer libbsc which is Apache-2.0).

---

## 5. Executable filters: 7-Zip BCJ/BCJ2 vs dispack — decision

Numbers ([Wikipedia/BCJ](https://en.wikipedia.org/wiki/BCJ_(algorithm)), [NSIS forum, Pavlov](https://nsis-dev.github.io/NSIS-Forums/html/t-243346.html), encode.su tests):

- firefox.exe 7.6 MB, LZMA max: no filter 3,004,746 B (+5.1%); **BCJ 2,858,179**; **BCJ2 2,782,313 (−2.7% vs BCJ)**.
- Pavlov: BCJ ≈ +6–8% ratio on x86 executables. Fedora 31 squashfs: BCJ saved ~30 MB of 1.7 GB.
- BCJ2 = 4-stream design (main + call targets + jump targets + control); target streams compress
  with small-dict LZMA. Section-size tuning matters (`f=BCJ2:d9M`; modern default 240 MiB).
- dispack (kkrunchy-derived, integrated after FreeArc 0.60): wins a further ~2–4% on clean 32-bit
  PE, but **only supports 32-bit x86 PE**, and misfires on mixed data — documented case: 500 DDS
  files grew from 390.5 MB to 399.5 MB because FreeArc applied it blindly; single files bloated
  1.7 MB → 2.3 MB. On protected executables (skype.exe test) it *lost* to BCJ (8,279,426 vs
  7,856,207 B).

**Decision for narc:** implement BCJ-class rel→abs converters per ISA (x86, x86-64, ARM64;
RISC-V optional later — 7-Zip 24.03 and xz both ship one), selected by *parsing the PE/ELF/Mach-O
header* (7-Zip 23.00 approach), applied only to executable sections. Add BCJ2-style stream
splitting in the max tier. **Do not build a dispack-class disassembler filter** — poor
gain/complexity ratio, x86-32 only, and modern binaries (CFG, retpolines, mixed
code/data, x64-heavy world) erode its assumptions further.

RAR confirmation of the same taste ([RAR5 format notes](https://techshelps.github.io/WinRAR/html/HELPRAR5Format.htm)):
RAR5 deleted Itanium, text/PPMd, raw-audio and true-color algorithms ("less widespread…too slow")
and kept exactly **delta + IA-32 + ARM** — the two filters with the best win/cost, same shortlist
as below.

---

## 6. Already-compressed data (the "smarter than 7-Zip/FreeArc" requirement)

FreeArc's answer was routing (`$compressed` → cheap rep+tornado) plus *external* precomp. narc
can do the same natively with maintained Rust libraries:

| Input | Tool (2026) | Gain | Cost | Notes |
|---|---|---|---|---|
| deflate streams (zip, gz, png, docx/xlsx, pdf) | **preflate-rs** 0.7.x | recompressed with LZMA/zstd afterwards: typically 20–50% vs storing the deflate stream | high CPU, but streamable; corrections-based model | Rust, Apache-2.0, Microsoft-maintained. Feed *expanded* data through the normal narc pipeline, store correction stream |
| JPEG | **lepton_jpeg_rust** | up to ~22% (baseline+progressive), bit-exact | high CPU, MT-capable | Rust crate `lepton_jpeg`. Alternative: JPEG XL transcode (brunsli lineage) — better ecosystem but adds a big C++ dep |
| MP3 | packMP3 1.0g | ~16% typical | medium | LGPL-3.0 C; subprocess or dylib isolation; stable, no known bugs |
| zstd/brotli/lzma streams inside files | none practical | — | — | detect and store; recompression of non-deflate codecs is research territory (negative knowledge) |
| generic high-entropy | route to store/fast-LZ tier | avoids 10× time waste | ~free | entropy probe in phase 1: mean bytewise entropy + tiny trial compression |

Pipeline nuance proven by the game-repacking community (precomp → srep → lzma ordering): stream
expansion must happen **before** long-range dedup, because deflate hides identical content from
the dedup layer. For narc this means recompression transforms run in phase 1/ingest, before CDC
chunk hashing — which also makes dedup of the *decoded* content work across files.

Reference for the 7-Zip world: the [MFilter plugin](https://www.tc4shell.com/en/7zip/mfilter/)
does brunsli/lepton JPEG recompression inside 7z archives — proof this integrates cleanly with an
archive pipeline.

---

## 7. Deliverable: prioritized filter list for narc's two-phase analyzer

Phase 1 (analyze) tags each file/chunk-group with {type, stride, ISA, embedded-stream map,
entropy}; phase 2 applies the chain. Costs are relative to modern desktop CPU, single thread;
everything below parallelizes per chunk group.

### Priority 0 — the dispatcher itself (biggest single win)
Content-based detection + routing + solid-group sorting. This is not a filter but it is where
FreeArc's ~10%-at-equal-speed edge came from. Detect by magic/header parse + entropy + stride
probe; never by extension alone. Route incompressible data away from the max codec.
**Win: up to 10% overall ratio at ~zero CPU; also 2–5× time saved on precompressed input.**

### Priority 1 — cheap, universal, implement first
1. **rep / long-range matcher within solid blocks** (below CDC granularity, window ≥ codec dict).
   Win: 2–10× on redundant corpora, ~0 elsewhere; cost: ~100+ MB/s, memory ≈ window.
   Implementation: rolling-hash long-match LZ à la rep/zstd-LDM. (narc's CDC dedup already
   covers cross-file ≥ chunk-size redundancy; rep covers 512 B–64 KB matches CDC misses.)
2. **BCJ-class exe converters, header-selected** (x86, x64, ARM64). Win: +5–8% on executables at
   ~GB/s; ~200 LOC per ISA. Max tier: BCJ2-style call/jump stream splitting (+2–3% more).
3. **delta with automatic stride detection** (period 2/4/8/16 via autocorrelation or FreeArc-style
   heuristic), covers WAV/BMP/tables/float arrays. Win: +5–30% on structured binary; cost: fast
   scan + subtract. 7-Zip has only manual `Delta:N` — auto-detection is a visible differentiator.

### Priority 2 — per-class specialists (the "max tier competes with FreeArc" layer)
4. **Deflate expansion (preflate-rs)** for zip/gz/png/docx/pdf ingest. Win: 20–50% on
   deflate-bearing data; cost: high CPU, do it in analyze/ingest phase, cache verdicts.
5. **JPEG recompression (lepton_jpeg_rust)**. Win: ~20% on photo collections — the single biggest
   ratio lever on typical user data; cost: high CPU, MT.
6. **Text transform (dict/word-replacement + optional LZP)** before the text codec. Win: +3–10%
   on logs/source/HTML; cost: fast. Model on kanzi TEXT+UTF rather than FreeArc dict params.
7. **Audio/multimedia**: `mm`-style channel split + delta for PCM (route WAV to FLAC-class
   predictor in max tier; FLAC is BSD, alive; TTA is dead). Win: 20–40% vs generic LZ on PCM.

### Priority 3 — max-tier only / later
8. **Strong text codec tier**: PPMd-class or BWT (libbsc, Apache-2.0) for `$text` at max. RAR
   dropped PPMd for speed; narc can keep it optional behind the asymmetric-tier warning.
9. **MP3 recompression (packMP3)** — LGPL isolation needed; ~16% on MP3 libraries.
10. **RISC-V/other ISA BCJ** — parity with 7-Zip 24.03 / xz ≥ 5.6 when Linux target lands.

### Explicitly rejected
- **dispack-style disassembling exe filter** (§5) — +2–4% best case, 32-bit-only heritage, bloats
  wrong inputs, high maintenance.
- **Tornado/grzip ports** — zstd and libbsc dominate them today.
- **TTA codec** — dead upstream; FLAC wins on all axes (Hydrogenaudio comparison).
- **One-size LZMA for everything** — the anti-pattern FreeArc existed to disprove.

### Correct application order (per solid group / chunk group)

```
ingest:    detect → [format expansion: preflate / lepton / packMP3]   (phase 1)
           → CDC chunking + cross-file dedup (narc core)
pipeline:  rep(long-range, in-group) → ISA filter (exe sections only)
           → delta(auto-stride) | text-transform | mm  (mutually exclusive, per type)
           → codec: zstd tier | lzma tier | ppmd/bwt text tier | store
```

Rationale for the order: expansion first (reveals redundancy to everything downstream); dedup/rep
next (largest granularity, cheapest, shrinks work for all later stages); structural transforms
next (reshape what remains); entropy-heavy codec last. This is FreeArc's `rep+exe+delta+lzma`
order generalized, and the precomp→srep→lzma order the repacking community converged on
independently.

---

## 8. Pointers

- FreeArc docs: [Introduction](https://freearc.sourceforge.net/Introduction.htm) ·
  [0.40 manual](http://freearc.sourceforge.net/FreeArc040-eng.htm) ·
  [source mirror](https://github.com/svn2github/freearc) (Compression/ has per-codec dirs:
  REP, Dict, Delta, MM, LZP, GRZip, Tornado, LZMA, PPMD, CLS/CELS plugin API)
- FreeArc'Next: [Bulat-Ziganshin/FA](https://github.com/Bulat-Ziganshin/FA) —
  read `FreeArc-archive-format.md` and the CELS API for codec-plugin design ideas
- Known-good user chain (community folklore, Martin Ankerl):
  `-m=rep:1024mb+mm+delta+dispack+4x4:lzma:50mb` — evidence users want chain-level control; expose
  the same power in narc's config
- 7-Zip: [method switch docs](https://documentation.help/7-Zip/method.htm) ·
  [history](https://www.7-zip.org/history.txt) (23.00 ARM64+auto-parse, 24.03 RISCV, 26.02 current)
- WinRAR 7.0: [release news](https://www.win-rar.com/singlenewsview.html?L=0&tx_ttnews%5Btt_news%5D=251) —
  64 GB dict, long-range search, exhaustive `-mcx`
- Rust deps: [preflate-rs](https://github.com/microsoft/preflate-rs) ·
  [lepton_jpeg_rust](https://github.com/microsoft/lepton_jpeg_rust) ·
  [kanzi-cpp (reference)](https://github.com/flanglet/kanzi-cpp) ·
  [libbsc](https://github.com/IlyaGrebnov/libbsc) · [bzip3](https://github.com/iczelia/bzip3)
- Benchmarks: [compressionratings — FreeArc](https://compressionratings.com/i_freearc.html) ·
  [— 7-Zip](https://compressionratings.com/i_7-zip.html) ·
  [Squeeze Chart](https://www.squeezechart.com/) ·
  [PeaZip benchmark](https://peazip.github.io/peazip-compression-benchmark.html)
