# Research 02 — Lossless Recompression of Already-Compressed Data

*Nova Arc research report. Status of the ecosystem as of August 2026. All liveness/license claims below were verified against live GitHub/crates.io data on 2026-08-16 (not from memory).*

## 1. The concept

"Recompression" (precompression) = detect an embedded compressed stream, decode it to its
*plaintext* + a small *correction/reconstruction record*, compress the plaintext with a modern
codec, and on extraction re-encode the plaintext so the output is **bit-exact identical** to the
original stream. This is the trick behind FreeArc + external filters, Precomp, and the modern
cloud-storage pipelines (Dropbox Lepton, Microsoft OneDrive/Azure preflate-rs + lepton_jpeg_rust).

Two hard requirements for an archiver:

1. **Verification is mandatory.** Every transform must be round-tripped
   (transform → inverse → byte-compare) *at pack time*; on mismatch fall back to storing raw.
   Precomp's own community explicitly warns it is "proof-of-concept" without this discipline.
2. **Format pinning.** Correction-record formats (preflate-rs, lepton) are not guaranteed stable
   across library versions. `.narc` must record `(transform_id, codec_version)` per stream and
   Nova Arc must keep old decoders callable forever, or old archives become unreadable.

---

## 2. Deflate / zlib stream recompression

Deflate is the highest-value target: ZIP, PNG, PDF, gzip, docx/xlsx/pptx, jar/apk, epub, many
game formats. The encoder has freedom (parsing, lazy matching, block splits), so bit-exact
re-encoding requires modelling the original encoder and storing corrections.

### Candidates

| Project | Language | License | Last activity | Health | Verdict |
|---|---|---|---|---|---|
| **[preflate-rs](https://github.com/microsoft/preflate-rs)** (Microsoft, K. Roomp) | **Pure Rust** | Apache-2.0 | pushed 2026-04; crate 0.7.6 on crates.io 2026-03 | Active, production ("used in cloud storage"), `#![forbid(unsafe)]`, chunked/bounded memory, MSRV 1.89 | **WINNER** |
| [preflate](https://github.com/deus-libri/preflate) (Dirk Steinke, original) | C++ | Apache-2.0 | last push **Sep 2018** | Dormant; algorithm lives on in preflate-rs and precomp 0.4.7+ | Reference only |
| [precomp-cpp](https://github.com/schnaader/precomp-cpp) | C++ | Apache-2.0 | last stable release **v0.4.7 (Feb 2019)**; master pushed Jun 2025; 48 open issues | Semi-dormant; monolithic CLI-first design; 0.5 refactor stalled | Idea mine, not a dependency |
| reflate / Zflate (E. Shelwien) | C (closed) | **none** (binaries only) | thread activity ended ~2019 | Closed-source Windows DLLs; author active on GDCC but no open release | **Rejected** — unusable in FOSS |
| [grittibanzli](https://github.com/google/grittibanzli) (Google) | C++ | Apache-2.0 | **archived**, last push Apr 2018 | Dead | **Rejected** |
| AntiZ | C++ | zlib | dead pre-2018 | Abandoned early prototype | **Rejected** |
| [xtool](https://github.com/Razor12911/xtool) (Razor12911) | Delphi | MIT | repo **archived**, dev ended ~2022 | Repack-scene tool; wraps zlib/preflate/reflate/oodle/flac/packjpg/brunsli via external DLLs | Feature checklist only |

### preflate-rs details (verified from README)

- Detects and models the specific compressor: **zlib (all levels, near-zero overhead), zlib-ng,
  libdeflate, miniz/miniz_oxide, Windows shell zlib (PNG codec, Explorer ZIP)**; unknown
  encoders still work with larger correction records.
- Correction overhead: **0.01 % of uncompressed size (zlib) up to ~2.7 % worst case (miniz L2)**;
  most compressors < 1 %.
- Ships a `container` crate that already scans ZIP/PNG/JPEG containers and orchestrates a
  Zstd recompression pipeline — a working template for narc's analyzer.
- Pure Rust, crates.io: `preflate-rs 0.7.6` — integration cost for Nova Arc is essentially zero.

### Expected end-to-end gains (deflate payloads, after preflate + LZMA/zstd-19)

| Input | Typical total saving vs storing the file as-is |
|---|---|
| PDF (deflate-heavy) | **25–50 %** (precomp historical numbers: PDFs shrink to 50–75 %) |
| docx/xlsx/pptx (XML in zip) | **40–60 %** (XML plaintext compresses dramatically better; plus cross-file dedupe) |
| PNG | **5–30 %** (filtered pixel data under LZMA; brute-force zlib often fails on optimized PNGs — preflate handles them) |
| ZIP/jar/apk of mixed content | 10–50 %, content-dependent |
| Game data (encode.su Eternal Castle test, precomp 0.4.8dev) | ~47 % |

Also worth supporting via the same mechanism: **gzip, zlib-wrapped streams inside PDF, SWF, and
bzip2** (precomp supports bzip2 recompression; bzip2 re-encoding is nearly deterministic —
low-effort win, though the format is fading).

---

## 3. JPEG recompression

All serious tools reach roughly the same ceiling (~20–23 %) because they all re-entropy-code the
quantized DCT coefficients with context modelling + arithmetic coding.

| Project | Savings | Language / Rust story | License | Health (2026) | Bit-exact |
|---|---|---|---|---|---|
| **[lepton_jpeg_rust](https://github.com/microsoft/lepton_jpeg_rust)** (Microsoft) | **~22 %** | **Pure Rust**, crates.io `lepton_jpeg 0.5.8` (2026-01), AVX2 SIMD, multithreaded | Apache-2.0 | Active (pushed 2026-04) | Yes — "bit-by-bit recovery", keeps invalid/garbage trailing data |
| [Dropbox lepton](https://github.com/dropbox/lepton) (original) | 22 % avg at Dropbox scale | C++ | Apache-2.0 | **Archived Mar 2024** | Yes |
| [brunsli](https://github.com/google/brunsli) | ~22 % | C++ (FFI needed) | MIT | Surprisingly **alive again** — commits by E. Kliuchnikov 2026-08 (streaming decoder, UB fixes); only tagged release still v0.1 (2019) | Yes |
| JPEG XL lossless transcode ([libjxl](https://github.com/libjxl/libjxl)) | **~20 %** (16–22 %) | C++; encode via `jpegxl-rs 0.14 / jpegxl-sys` (active, `vendored` static build); decode/reconstruct in **pure Rust** via `jxl-oxide` + `jxl-jbr` (active, 0.12.x, 2026-08) | BSD-3 (libjxl) | Active; JXL now re-entering Chrome | Yes — `jbrd` box: byte-for-byte original JPEG (entropy codes, scan script, restart markers, padding bits) |
| [packJPG](https://github.com/packjpg/packJPG) (Stirner lineage) | ~23–24 % but slow | C++ | **LGPL-3.0** | Dormant (last push Apr 2020) | Yes |

**Recommendation: `lepton_jpeg` crate as the primary JPEG transform.** It is the only
actively-maintained, production-grade, *pure Rust*, Apache-2.0 option; supports **baseline and
progressive** JPEG; guards against pathological files (dimension caps, zero-quant-table
rejection). The Lepton container is a private storage format — exactly what narc needs
internally.

**JPEG XL transcode as an alternative/secondary tier:** similar ratio (~20 %), but the stored
object is a *standard* .jxl file — a user-visible bonus ("extract as JXL, 20 % smaller, or as the
original JPEG"). Cost: libjxl C++ FFI for encoding (jpegxl-rs handles it); reconstruction back to
JPEG can stay pure-Rust via `jxl-jbr`. Caveat: libjxl declines transcoding some inputs
(arithmetic-coded JPEGs; some exotic files — see libjxl issue #2693), so lepton + jxl + store
must all be fallback rungs of one ladder.

---

## 4. PNG / GIF

- **PNG** is "deflate + filters". The right move is *not* an image codec but preflate-rs
  (its PNG/IDAT-aware container handling already exists). After undoing deflate, the
  *filtered* scanline data goes to LZMA/zstd. Bit-exact by construction; gains 5–30 %.
  Re-encoding PNG as WebP-lossless/JXL was **rejected**: smaller, but cannot reproduce the
  original PNG file bytes.
- **GIF** is LZW. Precomp implements GIF LZW unpack/repack; gains are modest (LZW plaintext
  then compressed better — typically ~5–15 %) and GIF matters less every year. No maintained
  standalone library exists. **Low priority**: implement LZW re-encode natively later (LZW
  re-encoding is nearly deterministic, easier than deflate) or skip in v1.

---

## 5. Audio

### MP3 — packMP3

- [packMP3](https://github.com/packjpg/packMP3) (Stirner): **~16 % average** saving
  (author's test over 6000 files), bit-exact. C++, **LGPL-3.0**, dormant (last push Apr 2020).
- LGPL-3.0 in a Rust static-link world is awkward: narc would need dynamic linking or a
  relink-capable distribution to stay clean. Options: ship as an optional dynamically-loaded
  plugin DLL, or port the algorithm (it is well documented — Stirner's master's thesis).
- **mp3packer** was investigated and **rejected**: it losslessly *rearranges* frames
  (CBR→minimal VBR, 2–10 %) but does not reproduce the original file bit-exact — unusable
  for an archiver.
- Realistic plan: MP3 support is a *v2* feature via plugin; 16 % on a shrinking format doesn't
  justify blocking v1.

### WAV → FLAC

- Easy, high-value: typical music at 16/44.1 compresses to ~50–65 % of original
  (**35–50 % saving**; flacenc-bin advertises ~60 % reduction on its corpus).
- Bit-exactness strategy: parse RIFF chunks; FLAC-encode only the `data` chunk's PCM
  (8/16/24-bit integer); store *all* other chunks, padding, odd trailing bytes, and the exact
  chunk order verbatim. Refuse float32/ADPCM/WAVE_FORMAT_EXTENSIBLE oddities → store raw.
  FLAC decode of what we encoded is trivially bit-exact for samples; the wrapper guarantees
  file-level identity.
- Rust: [`flacenc` 0.5.1](https://crates.io/crates/flacenc) (pure Rust, Apache-2.0, active,
  SIMD + multithread) — but the maintainer **flags encoder instability** ("encoded file may
  contain distortion"). With narc's mandatory verify-decode-compare this is survivable, but the
  safer default is **libflac via FFI** (`flac-bound`/`libflac-sys`, BSD) with `flacenc` as the
  pure-Rust experiment. Decode side: `claxon` (pure Rust, well-tested).

---

## 6. Video (MP4/MKV, H.264/H.265, plus AAC audio)

**Store-only. No practical lossless recompressor exists in 2026.**

- Research estimates (encode.su, IEEE MPEG-1 re-entropy-coding papers): ~10 % possible on
  CABAC H.264, 15–25 % on CAVLC — but nobody has shipped a maintained, verifiable tool.
  Shelwien has private AAC/audio recompressors (closed). xtool never had video codecs.
- Modern codecs (H.265/AV1) use adaptive arithmetic coding throughout; residual redundancy is
  small and the decode/re-encode surface is enormous (bug surface = corruption risk).
- Correct narc behavior: **detect** video containers in the analyzer, classify as
  "incompressible", route to the *store* tier (no LZMA time wasted), and let CDC chunking
  provide dedupe. Container-level metadata (moov atoms) is too small to matter.

---

## 7. Nested archives (docx/xlsx/pptx, jar/apk, epub, zip-in-zip)

These are the everyday jackpot: Office files are zip+deflate of highly-compressible XML.

- **Recursive analyzer**: walk containers (zip → members → maybe another zip/PNG/JPEG…),
  apply the per-type transform at every level. Precomp does this with a recursion depth
  parameter; preflate-rs's `container` crate scans ZIP/PNG/JPEG already.
- **Safety**: recursion depth cap (e.g. 8), total-expansion cap (zip-bomb defense: abort a
  branch when expansion ratio exceeds e.g. 1000:1 or an absolute budget), per-stream timeouts.
- **Zip specifics**: reproduce member order, local-header quirks, data-descriptor presence,
  timestamps, "extra" fields, and non-deflate members (stored, zstd, lzma) byte-exact. The
  correction record is "zip metadata verbatim + per-member deflate corrections".
- **APK**: same as zip; v2/v3 signatures cover the whole file, but since narc reconstructs
  bit-exact, signatures survive. Note: modern APKs store .so files aligned/uncompressed —
  those go straight to the normal narc compressor.
- **Interaction with .narc CDC dedupe**: chunk the *precompressed* (post-transform) data, not
  the raw file — two docx that differ by one XML element then dedupe almost entirely, and the
  "replace 1 file in 700" edit path stays cheap.

---

## 8. Recommended narc max-tier pipeline

**Phase 1 — analyze** (cheap, parallel): magic + structure sniffing per file; recursive
container walk; emit a stream map `(offset, len, type, confidence)` and per-file plan.

**Phase 2 — transform + compress** (per stream, all with verify-or-fallback):

| Priority | Stream type | Transform | Library (Rust cost) | Expected saving |
|---|---|---|---|---|
| 1 | deflate/zlib/gzip (zip, docx, PDF, PNG, apk…) | preflate → plaintext + corrections | `preflate-rs` (native crate, zero cost) | 10–60 % by container type |
| 2 | JPEG (incl. inside zip/PDF) | Lepton re-entropy-code | `lepton_jpeg` (native crate) | ~22 % |
| 3 | WAV PCM | FLAC (wrapper-preserving) | libflac FFI or `flacenc` | 35–50 % |
| 4 | bzip2 | precomp-style re-encode | port/small C FFI | payload-dependent |
| 5 (v2) | MP3 | packMP3 | C++ FFI, LGPL plugin | ~16 % |
| 6 (v2) | GIF | LZW re-encode | small native impl | 5–15 % |
| — | video/AAC/opus, zstd/brotli/LZMA payloads, encrypted | none — store tier | — | 0 % |

Then group transformed outputs by type (text-like, image-coefficients, binary), and hand groups
to the normal narc codec selection (zstd/LZMA/etc. — topic of report 01/03). JXL transcode is an
optional user-facing alternative to Lepton (standard format at ~20 %).

**Metadata**: each stream stores `(transform_id, codec_version, correction_blob)`; extraction
replays inverses bottom-up. Never auto-upgrade a transform codec without keeping the old
decoder — bit-exactness is a *forever* contract.

---

## 9. Sources

- preflate-rs: <https://github.com/microsoft/preflate-rs>, crate: <https://crates.io/crates/preflate-rs>
- preflate (original): <https://github.com/deus-libri/preflate>; announcement: <https://encode.su/threads/2948-Preflate-An-open-source-recompressor-for-non-zlib-deflate>
- precomp-cpp: <https://github.com/schnaader/precomp-cpp>; thread: <https://encode.su/threads/3223-precomp-further-compress-already-compressed-files>
- reflate: <https://encode.su/threads/1399-reflate-a-new-universal-deflate-recompressor>; Zflate: <https://encode.su/threads/3998-Zflate-a-universal-deflate-recompressor-tool-based-on-reflate>
- grittibanzli: <https://github.com/google/grittibanzli>
- Lepton: <https://github.com/dropbox/lepton> (archived); Rust port: <https://github.com/microsoft/lepton_jpeg_rust>, crate: <https://crates.io/crates/lepton_jpeg>
- brunsli: <https://github.com/google/brunsli>
- JPEG XL: <https://jpegxl.info/>; libjxl: <https://github.com/libjxl/libjxl>; format/jbrd: <https://github.com/libjxl/libjxl/blob/main/doc/format_overview.md>; Google Research benchmark: <https://research.google/pubs/benchmarking-jpeg-xl-lossylossless-image-compression/>; JXL history paper: <https://arxiv.org/pdf/2506.05987>
- Rust JXL: <https://crates.io/crates/jpegxl-rs>, <https://github.com/tirr-c/jxl-oxide>, <https://crates.io/crates/jxl-jbr>, WIP official <https://github.com/libjxl/jxl-rs>
- packJPG/packMP3/packARC: <https://github.com/packjpg/packJPG>, <https://github.com/packjpg/packMP3>, <https://github.com/packjpg/packARC>; packMP3 release thread: <https://encode.su/threads/1549-packMP3-v1-0c-release>
- FLAC in Rust: <https://github.com/yotarok/flacenc-rs/>, <https://crates.io/crates/flacenc>
- xtool: <https://github.com/Razor12911/xtool>; thread: <https://encode.su/threads/4000-Xtool-Some-tool-repackers-like-to-use>
- Video recompression estimates: <https://encode.su/threads/1241-Format-priority-for-recompression>
