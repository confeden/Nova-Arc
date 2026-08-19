# Research 12 — Game Files and "Incompressible" Data

*Nova Prism research report. Live-web research August 2026. Every number below is tagged
**[measured]** (published benchmark from a primary source), **[claimed]** (vendor/author assertion,
no independent table), or **[estimate]** (my reasoning from format structure — must be measured
locally before it goes in a roadmap).*

**Companion report:** [02-recompression.md](02-recompression.md) already covers deflate/JPEG/PNG/
MP3/WAV/video recompression and the preflate-rs + lepton_jpeg tooling decision. This report does
**not** repeat it; §7 only adds the game-specific delta. Read 02 first.

> **Adversarial verification pass, 2026-08-17.** Every load-bearing number, licence and quotation
> below was re-checked against its primary source. Blocks marked **"Correction (verification
> pass)"** are places where the first draft was wrong; they are kept visible rather than silently
> patched, because two of them are the kind of error that would otherwise be re-derived later.
> Summary of what changed: the BC1 percentages were misread and the real split gain is **12.3 %**,
> not a 67→27 % collapse (§3.1); the float-filter baseline **is** published, so that gain is a
> measured **≈32 %** rather than a hedge (§6.1); the GDeflate chain is **not** bit-exact end to end
> because un-swizzled GDeflate is not RFC 1951 (§4.5); the 60 GB Unreal composition audit is not a
> usable source (§2); one exe-filter datapoint was withdrawn (§9); and a new **§11.1** checks every
> recommendation against nova's chunking, dedup and bounded-extraction invariants. Verified as
> stated and unchanged: the xtool CRC-mismatch changelog line, Yann Collet's zstd
> no-reproducibility statement, the Ström & Wennersten bpp figures, BC7Prep's 5–15 %, Titanfall's
> 35 GB, the ARM64/RISC-V kernel filter gains, 7-Zip's Swap4 behaviour, `pco`'s 29–94 %, and the
> licence/maintenance status of `pco`, `alp`, `preflate-rs`, `meshoptimizer`, `OptiVorbis`,
> `dxt-lossless-transform` and `lossless-transform-utils`.

---

## 1. Executive summary

The premise "game data is incompressible" is half true, and the half that is true is the half that
matters least. Three findings drive everything:

1. **The single biggest lever is not a new codec — it is undoing the container.** Modern installs
   are ~45–75 % texture + audio **[estimate — the range is my own, spanning two datapoints of
   unequal quality; see the §2 source warning]**, and almost all of it is already inside a per-block
   LZ container (Oodle, LZ4, zlib, GDeflate, zstd). Whether nova can beat 7-Zip on a game folder is
   decided almost entirely by *how many of those containers nova can open bit-exactly*.
2. **Raw BCn texture data has a genuine, cheap, bit-exact win of ~12 %** that no mainstream
   archiver applies, and the transform is ~200 lines of code with no dependency. (12.3 % is derived
   in §3.1 from the one published field-level measurement; the widely repeated "~10 %" figure is
   unsourced.) Chained after preflate on a Bethesda `.ba2` or a UE4 `.pak`, it compounds — but see
   §11.1: nova's content-defined chunking must be taught about block phase first, or the gain is
   lost without any correctness error to warn you.
3. **Oodle is a wall.** Kraken/Mermaid/Leviathan streams — the dominant format in AAA PC games
   since ~2021 — cannot be recompressed by any legal, maintainable FOSS mechanism. Accept it,
   detect it, route to *store*, and spend the saved CPU elsewhere. Anyone promising otherwise is
   describing the repack scene's DLL-borrowing hack, which fails CRC on real data.

Prioritized transform list (details and sourcing in the sections that follow):

| # | Transform | Data class | Expected gain | Bit-exact | Rust | License | Effort |
|---|---|---|---|---|---|---|---|
| 1 | **preflate (deflate/zlib)** — already decided in 02 | UE4 `.pak`, `.bsa`, `.ba2` GNRL/DX10, `.pk3`, `.apk`, save games, `.zip` DLC | 10–60 % of container **[measured]** | yes | `preflate-rs` | Apache-2.0 | done/low |
| 2 | **BCn split transform** (colours ‖ indices, endpoint split, YCoCg-R) | `.dds`, raw BC1–BC7 blobs, DX10 BA2 chunks, post-preflate texture payloads | **12.3 %** on one 20 MB BC1 file **[measured, derived — §3.1]**; 5–15 % BC7 **[claimed, RAD]**; up to ~18 % vs LZMA with a full CM model **[measured, ETC2]** | yes (reversible subset only — see §3.2) | write our own | n/a | **medium — best ROI in this report; needs §11.1 alignment work** |
| 3 | **Byte-split + delta filter for float arrays** | meshes, vertex/index buffers, animation curves, particle/sim data, glTF `.bin`, `.fbx` | 94.5 MB → 22.9 MB vs "roughly 30 MB class" unfiltered **[measured]** | yes | trivial (`~50` LoC) or `pco`/`alp` | Apache-2.0 | low |
| 4 | **Auto-stride delta / record transposition** | save files, DBs, tables, index blobs, `.uasset` header tables | +5–30 % on structured binaries **[estimate; see 04]** | yes | write our own | n/a | medium |
| 5 | **GDeflate de-swizzle → preflate** | DirectStorage-era game packs | upper bound = deflate's (10–60 %), realistically much less **[estimate, revised down §4.5]** | swizzle yes (specified bijection); the deflate half is **not** RFC 1951 — see §4.5 | port from MIT reference **+ extend a preflate model** | MIT | **high**, novel |
| 6 | **PCM/WAV → FLAC** (from 02) | uncompressed game audio (still huge in some titles) | 35–50 % **[measured]** | yes | `flacenc`/libflac | Apache-2.0/BSD | low |
| 7 | **BCJ2 / ARM64 / RISC-V exe filters** | `.exe`, `.dll`, `.so` | 6–8 % from any x86 filter; ~+9 % BCJ2 over BCJ (single file); 5 % ARM64; 7 % RISC-V **[measured]** | yes | `xz2`/own | 0BSD/own | low |
| 8 | **`pco` / ALP for detected numeric columns** | float/int arrays where §3 filter underperforms | 29–94 % over zstd-class on numeric data **[measured, non-game corpora]** | yes | `pco` 1.0.3, `alp` 0.0.2 | Apache-2.0 | medium |
| 9 | **Vorbis recompression** | Ogg music, Wwise/FMOD Vorbis | ~6 % avg, 26 % best case **[measured, OGGRE]** | yes | none exists | — | high, no library |
| — | **Oodle / Opus / AAC / video / encrypted** | most of a modern AAA install | **0 %** — store tier | n/a | n/a | n/a | none (detect & skip) |

---

## 2. Ground truth: what a modern game install actually contains

Composition, from the only itemized public audit I could find plus corroborating press reporting:

| Source | Breakdown |
|---|---|
| Developer audit of a 60 GB Unreal prototype (via [Dre Dyson](https://dredyson.com/lessons-from-what-is-considered-a-good-game-size-data-for-aaa-game-development-and-performance-the-complete-definitive-proven-tested-guide-on-optimizing-unreal-engine-and-unity-engine-footprint-redu/)) | textures **47 %** (28 GB of 4K), audio **22 %** (14 GB uncompressed), unused assets 9 GB, shader cache 5 GB, Blueprints < 3 % **[claimed — see source warning below; DO NOT cite as measured]** |
| Titanfall (2014), corroborated across [PC Gamer](https://www.pcgamer.com/titanfall-install-audio/), [Engadget](https://www.engadget.com/2014-03-11-titanfall-pcs-48gb-install-the-result-of-uncompressed-audio/), Game Informer, Escapist (all from Respawn lead engineer Richard Baker via Eurogamer) | 48 GB install, **35 GB audio (73 %)** — shipped uncompressed so a dual-core CPU would not spend a core decoding audio **[measured, well corroborated]** |
| Killing Floor 2 vs Red Orchestra (Tripwire, same source) | env. meshes+textures 1.4 GB → 17.4 GB; sound 327 MB → 1.1 GB **[measured]** |
| Unreal 4K texture math (same source) | 1K textures 13.3 GB / 2K 53.3 GB / 4K 223.7 GB for the same content **[claimed]** |

> **Source warning (verification pass).** The 60 GB breakdown is the *only* itemized figure in this
> table and it is the weakest source in the report. The page is an SEO-shaped article ("Complete
> Definitive Proven Tested Guide", 238-char title) whose author claims "I shipped a 60GB experimental
> open-world prototype back in 2023" with **no project name, studio, engine version, platform or
> methodology** beyond naming `listpackagedfiles`. It shows several markers of machine-generated
> content. The earlier draft tagged it **[measured, single project]**; that is not defensible.
> Downgraded to **[claimed, unverifiable]**.
>
> The load-bearing composition claim therefore rests on **Titanfall** (73 % audio, multiply
> corroborated from a named engineer) and on Tripwire's cross-generation growth figures — both of
> which point at **audio**, not textures, as the historically dominant share. "Textures are 47 %" is
> plausible for a 4K-texture UE build and matches the direction of the Tripwire numbers, but nova
> should not build priorities on it. **Action: before scheduling phase B, measure a real install
> tree locally** (`.dds`/`.ktx2`/`.wem`/`.bnk`/`.uasset`/`.ucas` byte shares over 3–4 installed
> games). That is one afternoon of `du`, and it replaces the shakiest input in this report.

**Operational conclusion:** textures and audio together dominate; executables are noise. Any effort
spent on exe filters instead of texture/audio/container handling is misallocated for this workload —
see §9. Which of texture vs audio leads is **title-dependent and currently unmeasured for nova**,
which is another reason phase B (BCn) and WAV→FLAC should be scheduled close together.

### 2.1 Container inventory — what actually wraps the bytes

| Engine / game family | Container | Payload codec | nova's realistic move |
|---|---|---|---|
| UE4 (≤ 4.19 default, and most non-AAA UE4 to this day) | `.pak` | **zlib/deflate**, 64 KB–256 KB blocks | **preflate** — full win |
| UE4.20+ / UE5 shipping AAA | `.pak` + `.ucas`/`.utoc` (IoStore) | **Oodle Kraken** — Epic's docs call Kraken "the usual default" and give level by build config (Debug/Development **3**, Test/Shipping **5**, Distribution **7**). `-compressionblocksize=256KB` appears in Epic's **recommended example settings**, not as a documented default — verified 2026-08 ([Epic docs](https://dev.epicgames.com/documentation/en-us/unreal-engine/oodle-data)). Independently, `ooz`'s `kraken.cpp` comments the container as "Header in front of each 256k block" | **store** (§4.3) |
| UE, some titles | `.pak` | **zstd** plugin, or **LZ4** | version-pinned re-encode gamble (§4.4) |
| Unity | `.bundle`/`.assets` (`UnityFS`) | flags&0x3F: 0 = none, 1 = **LZMA** (whole content stream), 2/3 = **LZ4/LZ4HC** (chunked) ([format](https://imbushuo.net/blog/archives/505/), [Unity manual](https://docs.unity3d.com/6000.1/Documentation/Manual/assetbundles-compression-format.html)) | LZ4HC/LZMA re-encode gamble (§4.4); uncompressed bundles → full normal path |
| Bethesda: Oblivion/FO3/FNV/Skyrim LE | `.bsa` | **zlib** (plus `xmem` in Skyrim LE only) | **preflate** — full win |
| Skyrim SE | `.bsa` v105 | **LZ4** | §4.4 |
| Fallout 4 / 76 | `.ba2` v1/v7/v8, GNRL + DX10 | **zlib** (DX10 = zlib-compressed **DDS chunks**) | **preflate → then BCn transform** — the compounding case |
| Starfield | `.ba2` v2/v3 | **LZ4** (ratio-tunable), zstd in newer variants | §4.4 |
| Source / Source 2 | `.vpk` | **uncompressed** ([Valve wiki](https://developer.valvesoftware.com/wiki/VPK_(file_format))) | full normal path — best case for nova |
| Respawn (Titanfall/Apex) | modified `.vpk`, `.rpak` | proprietary, undocumented | store |
| id Tech (Quake 3 → Doom 3) | `.pk3`/`.pk4` | **zip/deflate** | **preflate** — full win |
| Quake 1/2 | `.pak` | uncompressed | full normal path |
| DirectStorage titles | game-specific | **GDeflate** | §4.5 — de-swizzle then preflate |
| Any engine, loose files | `.dds`, `.ktx2`, `.wav`, `.ogg`, `.wem`, `.fsb`, `.bank`, `.uasset`, `.nif`, `.bin` | see §3, §5, §6 | per-type transforms |

Bethesda BSA/BA2 codec mapping cross-checked against the [`bsa` C++ library docs](https://ryan-rsm-mckenzie.github.io/bsa/index.html), the pure-Rust [`ba2` crate docs](https://docs.rs/ba2/latest/ba2/fo4/) (**0BSD** — see §4.2) and [modding.wiki](https://modding.wiki/en/skyrim/users/bethesda-archives). "zlib for Fallout 4, LZ4 introduced with Starfield" re-confirmed 2026-08; note the FO4 next-gen update added BA2 **v7/v8**, so any parser must handle those.

---

## 3. BCn / DXT / ASTC compressed textures

### 3.1 Why raw BCn resists LZ, and what the fix is

A BC1 block is 8 bytes: `[color0:u16][color1:u16][indices:u32]`. Two statistically opposite
populations are **interleaved every 4 bytes**:

- **endpoints** — smooth, image-like, spatially correlated with neighbouring blocks, low entropy;
- **indices** — 16 × 2-bit selectors, high-frequency, near-random, high entropy.

An LZ matcher fed the interleaved stream must find matches that span both populations, so it finds
almost none. Deinterleaving into two planes gives the entropy coder two homogeneous streams. This
is the entire trick, and it is *free* — a pure permutation of bytes, therefore trivially bit-exact
reversible.

Measured field entropies from Sewer56's analyzer on a 20,015,319-byte BC1 file
([blog](https://sewer56.dev/blog/2025/03/11/a-program-for-helping-create-lossless-transforms.html),
[crate](https://crates.io/crates/struct-compression-analyzer)) **[measured]**:

| Field | bits/byte | raw bytes | zstd bytes | of its own raw size | of whole-file zstd |
|---|---|---|---|---|---|
| whole file (baseline) | 6.78 | 20,015,319 | 13,420,865 | **67.05 %** | 100 % |
| colours group (both endpoints) | 5.50 | 10,007,659 | 3,567,275 | **35.65 %** | 26.58 % |
| `colour0` alone | 5.42 | — | — | — | — |
| `colour1` alone | 4.96 | — | — | — | — |
| indices group | 6.66 | 10,007,659 | 8,199,754 | **81.93 %** | 61.10 % |

> **Correction (verification pass).** The analyzer's `26.58 %` / `61.10 %` columns are each field's
> share of the **whole file's compressed size**, *not* of that field's own size — the earlier draft
> of this section compared `26.58 %` against `67.05 %`, which is two different denominators and
> overstates the effect. The honest reading of the same raw numbers:
>
> - endpoints compress to **35.7 %** of their own bytes; indices to **81.9 %** of theirs — indices
>   are nearly incompressible, endpoints are very compressible. The premise holds.
> - the transform's actual payoff is the **sum of the two planes vs the interleaved file**:
>   3,567,275 + 8,199,754 = **11,767,029 B** vs **13,420,865 B** interleaved ⇒ **12.32 % smaller**
>   **[measured, derived from Sewer56's published figures, zstd level 16, one 20 MB BC1 file]**.
>
> That 12.3 % is the number to quote and the number to reproduce locally. It also independently
> corroborates the author's "~10 %" headline (§3.2) rather than depending on it.

### 3.2 The reversible transform ladder (BC1–BC3)

Primary source: [`dxt-lossless-transform-bc1` README](https://github.com/Sewer56/dxt-lossless-transform/blob/main/src/core/dxt-lossless-transform-bc1/README.MD).

| Step | What it does | Bit-exact? |
|---|---|---|
| 1. **Split blocks** | all `[c0,c1]` pairs into one plane, all `indices` into another | **yes** — pure permutation |
| 2. **Split endpoints** | `colour0` plane and `colour1` plane stored separately. Author: helps **~78 % of the time** **[claimed]** | **yes** |
| 3. **YCoCg-R decorrelation** of the RGB565 endpoints (upper 5 bits of green; low bit untouched) | removes inter-channel correlation | **yes** — YCoCg-R is a lifting transform, exactly invertible |
| 4. **Solid-block normalization** (canonicalize solid blocks to `C0 = colour, C1 = 0, indices = 0`; fully-transparent blocks to all-`0xFF`) | creates long `0x00`/`0xFF` runs | **NO — visually lossless only.** Rewrites BCn bits. Excluded from nova. |

Headline gain claimed for the ladder: **"~10 % saving at ~60 GB/s on a single thread"** for BC1–BC3
**[claimed, author — NOT VERIFIED: I could not locate this exact figure in either the repo root
README or the BC1 README as of 2026-08. Treat it as folklore.]** Two further cautions:

- the crate describes *itself* as "a fast, **visually lossless** transform for the BC1 block
  format", so any headline number it publishes plausibly **includes step 4 (normalization)**, which
  nova must exclude. The reversible-subset gain is not separately published by the author.
- therefore the number nova should plan against is the **12.3 %** derived in §3.1 from the author's
  own raw measurements, which covers steps 1–2 only and is arithmetic we can check.

**Licensing:** `dxt-lossless-transform` is **GPL-3.0 ("Reloaded FAQ" variant)** and GitHub reports
its license as `NOASSERTION` (verified via GitHub API, 2026-08; repo still tagged `[WIP] ...`, last
push 2026-08-10, 10 stars). **Do not link it.** The algorithm is fully documented in the README
above and is a byte permutation plus a published lifting transform — reimplement in ~200 lines of
safe Rust. Its sibling [`lossless-transform-utils`](https://github.com/Sewer56/lossless-transform-utils)
*is* **MIT** (LICENSE file verified) and active (pushed 2026-06-22) — a ~2565 MiB/s
LZ-compressibility estimator, 74.4 % agreement with zstd **[claimed]**. That one is directly usable
in nova's analyzer for the "is this transform worth it" decision and for the incompressible fast
path (§10). *Packaging gotcha:* the published crate declares `license-file = "LICENSE"` rather than
an SPDX id, so crates.io reports its license as `non-standard` and `cargo-deny`/license audits will
flag it until explicitly allow-listed. The estimator numbers themselves come from
`dxt-lossless-transform`'s root README, not from `lossless-transform-utils`' own docs.

### 3.3 What the ceiling looks like, if we cared to go further

| Approach | Result | Bit-exact | Availability |
|---|---|---|---|
| Byte permutation (§3.2) | **12.3 %** over plain LZ **[measured, derived §3.1 — one BC1 file, zstd-16]** | yes | reimplement, ~200 LoC |
| **BC7Prep** (Oodle Texture) — "lossless transform for BC7 blocks ... rearranges their bits" | **5–15 % smaller after subsequent compression** **[claimed, RAD]** ([oodletexture.htm](http://www.radgametools.com/oodletexture.htm)) | yes (needs runtime reverse, GPU-capable) | **proprietary, Epic Games Tools licence** |
| **Ström & Wennersten, HPG 2011**, "Lossless Compression of Already Compressed Textures" — predict pixel colours in the *image* domain to predict per-pixel indices, then entropy-code | ETC2 4 bpp → **2.3 bpp**, vs **2.8 bpp LZMA** and **3.0 bpp ZIP** ⇒ **≈18 % below LZMA** **[measured]** ([paper](http://www.jacobstrom.com/publications/StromWennerstenHPG2011.pdf), [ACM](https://dl.acm.org/doi/10.1145/2018323.2018351)) | yes | paper only, no library |
| **GST: GPU-decodable Supercompressed Textures** (Krajcevski et al., SIGGRAPH Asia 2016) | uses Ström as its lossless baseline; GST itself is **lossy** | no | [ACM](https://dl.acm.org/doi/10.1145/2980179.2982439) |

So: cheap permutation ≈ 12 %, a full context model in the image domain ≈ 18 % vs LZMA. The remaining
~6 points cost an image-domain predictor and an arithmetic coder per texture — a v3 research project,
not a v1 feature (and one that would also cost extraction: reconstruction requires re-running the
predictor over the decoded image, unlike a permutation). Do the permutation now; keep the Ström paper
as the known ceiling. *Note the ceiling is not measured on the same footing:* Ström's 18 % is bpp on
ETC-family textures with LZMA as the baseline, while the 12.3 % is bytes on one BC1 file with zstd-16
as the baseline. They bracket the opportunity; they are not subtractable.

### 3.4 BC7 / BC6H / BC4 / BC5 / ASTC / ETC2 specifics

- **BC4 / BC5** (single- and two-channel — roughness, metalness, **normal maps**, which are
  everywhere): same structure as BC3's alpha block — `[endpoint0:u8][endpoint1:u8][indices:6 bytes]`.
  Same split applies, and normal maps are a large share of any modern texture set. **[estimate:
  gains at least as good as BC1, because two 1-byte endpoint planes are extremely smooth.]**
- **BC7**: 8 modes, each with a different bit layout (palette size, endpoint precision,
  partition tables, shared-LSB "P-bits", up to 3 line segments per block). A useful split must
  **parse the mode nibble and route fields per mode**. Non-RDO BC7 is described by RAD as
  "commonly nearly incompressible" **[claimed]**; BC7Prep's 5–15 % is the realistic target.
  Sewer56's own BC7 work is "barely started" after a year — treat BC7 as a **separate, harder
  project** and ship BC1–BC5 first.
- **BC6H** (HDR): endpoints are delta-coded against a reference endpoint inside the block, plus
  partitioning. Same per-mode parse problem as BC7, smaller install share. Defer.
- **ETC2 / ASTC** (Android/mobile game data, and ASTC increasingly on desktop): ASTC is a
  variable-layout bit-packed format with a per-block "block mode" field and *reverse-order* weight
  bits — there is no clean field boundary to permute on. **[estimate: a naive split gains little;
  the Ström-style image-domain predictor is the only known path, and it was demonstrated on ETC2,
  not ASTC.]** Store-tier ASTC for now.
- **KTX2** containers: `supercompressionScheme` field decides everything
  ([Khronos spec](https://github.khronos.org/KTX-Specification/ktxspec.v2.html)). Scheme 0 (none) →
  raw BCn/ASTC payload, apply §3.2. Scheme = Zstd → §4.4 gamble. Scheme = BasisLZ/ETC1S → payload
  is Basis's own codebook+index entropy coding, already dense: **store**.

### 3.5 Rejected texture approaches (see negative knowledge)

**crunch/crnlib** and **Basis Universal** are *lossy re-encoders*, not recompressors. Crunch's
"clustered DDS" mode deliberately degrades the texture so LZMA compresses it better, and `.CRN` is
a different file. Unity's ETC/Crunch work is instructive on the ceiling — with quantization held
fixed so the decompressed texture is bit-identical, codebook+index coding beat "raw DXT + LZMA"
purely on the lossless back end ([Unity blog](https://blog.unity.com/engine-platform/crunch-compression-of-etc-textures))
— but neither tool can reproduce an input `.dds` byte-for-byte. **Oodle Texture RDO** (10 % near-
lossless, 20–50 % at visible-difference lambdas **[claimed]**) is likewise lossy, requires the
*original uncompressed* texture as input, and RAD explicitly warns against re-encoding from an
already block-compressed source. An archiver cannot use any of them.

---

## 4. Game asset containers

### 4.1 The general rule that decides every case

A container payload is recompressible **iff** nova can regenerate the original compressed bytes.
There are exactly two mechanisms:

- **(a) Parameter-recovery modelling** — decode to plaintext plus a small correction record that
  captures the original encoder's parsing decisions. **Only deflate has a shipped, maintained
  implementation of this** (preflate/preflate-rs). This is why deflate is worth 10–60 % and
  everything else is worth arguing about.
- **(b) Bit-identical re-encode with the exact original encoder** — requires the same library, same
  version, same build flags, same parameters, same thread/job geometry. Fragile by construction.

Everything in §4.3–4.5 is an application of this rule.

### 4.2 Deflate: the reliable jackpot

Directly covered by `preflate-rs` (report 02). In game terms this unlocks: **all UE4-era `.pak`**,
**all pre-SE Bethesda `.bsa`**, **Fallout 4/76 `.ba2`** (both GNRL and the DX10 texture chunks),
**every id `.pk3`/`.pk4`**, Android `.apk`/`.obb`, Minecraft region files, most `.zip`-based mod and
DLC packaging, and a large share of save games. Measured precedent from the repack scene: Precomp
0.4.8dev on the *Eternal Castle* game data reached **~47 %** ([encode.su](https://encode.su/threads/3223-precomp-further-compress-already-compressed-files)) **[measured, single title, forum-reported — not re-verified in the 2026-08 pass]**.
*Do not read that as preflate's number:* Precomp brute-forces zlib parameter combinations and also
handles other stream types, so its figure is an upper bound on a different tool's behaviour. Use
report 02's preflate figures for planning.

**The compounding case worth building a test for:** Fallout 4 `.ba2` DX10 → preflate the zlib
chunks → you now hold raw BCn blocks → apply §3.2. Two independent multipliers on the same bytes.
Nobody ships this combination.

Two details found in the verification pass that make this *easier* than §11.1(1) implies, and one
tool that removes most of the work:

- DX10 texture archives **do not store complete DDS files**. They store per-mip chunks of raw
  compressed texel data and the extractor **synthesizes** `DDS_HEADER`/`DDS_HEADER_DXT10` from the
  archive's own width/height/mip-count/format fields (see `bethesda_structs`' BTDX module for
  readable rebuild logic). So after preflate each chunk is **header-free, block-aligned raw BCn** —
  phase is 0 by construction and the per-chunk phase byte is only needed for loose `.dds` files.
- The chunking is per-mip for the streamer's benefit, so units are naturally small and satisfy
  §11.1(2) without a special case.
- **[`ba2`](https://crates.io/crates/ba2) 3.0.1 is a pure-Rust BSA/BA2 library under 0BSD** (the most
  permissive licence in this report; same author as the C++ `bsa`; repo `Ryan-rsm-McKenzie/bsa-rs`,
  last release 2024-12). It exposes the FO4 DX10 file header, chunk records and chunk compression
  options directly. This was missed in the first draft, which cited only the C++ docs — it removes
  the container-parsing effort from phase A almost entirely. **Verify before depending on it:** no
  release in ~20 months, and confirm it covers BA2 **v7/v8** (the Fallout 4 next-gen update) and
  Starfield's v2/v3 LZ4, not just v1.

### 4.3 Oodle (Kraken / Mermaid / Selkie / Leviathan / LZNA / BitKnit) — **reject**

This is the most important negative result in the report, because it governs the majority of AAA
install bytes since ~2021.

**Technical reality.** There is no parameter-recovery model. Bit-exact reproduction requires the
*exact* Oodle version the game shipped with. The repack scene's method is to load the game's own
`oo2core_N_win64.dll` from the install folder and re-encode with it
([xtool](https://github.com/Razor12911/xtool), [FileForums](https://fileforums.com/archive/index.php/t-102453.html)).
Even then it is not reliable: xtool's own changelog records that **xdelta support was removed from
the oodle and lzo codecs because CRC mismatches generated large diff files**
([changes.txt](https://github.com/Razor12911/xtool/blob/main/changes.txt)) — i.e. the re-encode
frequently does *not* reproduce the original bytes. Encrypted or special-option Oodle streams
(e.g. The Crew 2) fail outright.

**Legal reality.**
- Official Oodle Data is free **only as an Unreal Engine plugin**, not as a standalone SDK
  ([Epic announcement](https://www.unrealengine.com/en-US/blog/oodle-now-free-to-use-in-unreal-engine)).
  An archiver is not an Unreal project.
- The open reverse-engineered **decompressor** `ooz`/`kraken.cpp` is **GPL-3.0-or-later** — note the
  repo carries **no LICENSE file** (GitHub reports no licence); the grant is in the source headers:
  *"This program is free software: you can redistribute it and/or modify it under the terms of the
  GNU General Public License … either version 3 of the License, or (at your option) any later
  version"* (`kraken.cpp`, © 2016 Powzix; repo untouched since 2019-02)
  ([powzix/ooz](https://github.com/powzix/ooz)); the UEViewer author documented exactly this
  licence collision and made ooz an optional component. Compressor forks
  ([zao/ooz](https://github.com/zao/ooz), [rarten/ooz](https://github.com/rarten/ooz)) are
  self-labelled *"For educational use only"* and lean on borrowed `oo2core_7_*.dll` binaries.
- Even a *decompress-only* path is useless for a lossless archiver: without a bit-exact encoder you
  cannot restore the file.

**nova's correct behaviour:** detect Oodle-compressed containers in the analyzer, classify
`incompressible`, route to *store*, do not burn LZMA/PPMd time on them, and let CDC dedupe catch
identical blocks across patch versions (which it will — 256 KB Oodle blocks are stable across
patches for untouched assets, so nova's append-only edit story still shines here even at 0 %
codec gain). **This is a feature to market, not a defeat: a UE5 game folder is where nova's
"replace one asset in 1 s" beats 7-Zip's 20 s rewrite by the widest margin, precisely because
neither tool can compress the payload.**

### 4.4 LZ4, LZ4HC, zstd, LZMA payloads — the version-pinning gamble

Mechanism (b) only. Both upstreams explicitly disclaim output stability:

- **zstd**: *"There is no guarantee of output reproducibility between versions"* — maintainers, in
  response to 1.5.5 vs 1.5.6 producing different bytes at `-T0 --ultra -20`
  ([issue #4049](https://github.com/facebook/zstd/issues/4049)). Also unstable across MT job sizes
  since 1.3.4 ([#1077](https://github.com/facebook/zstd/issues/1077)) and, in some cases, across
  SIMD availability ([#4099](https://github.com/facebook/zstd/issues/4099)). A standing request for
  an output-stable LTS release exists and is unresolved
  ([#4173](https://github.com/facebook/zstd/issues/4173)).
- **LZ4/LZ4HC**: the project guarantees only *format* interoperability. HC's parser heuristics are
  exactly what upstream tunes between releases; lz4-java documents that output differs on
  opposite-endian machines even at matched versions; `LZ4_MEMORY_USAGE` is a compile-time knob that
  changes ratio (hence bytes).

**Verdict: build the mechanism, expect a low hit rate, never assume success — and see §11.1(3),
which is the stronger objection: mechanism (b) makes *extraction* run the original compressor, so a
§4.4 archive extracts at compression speed and compression memory. That collides with nova's hard
"extraction must always work on a weak PC" requirement, independently of hit rate.** Concretely:
1. Vendor a small library of pinned encoders (one or two lz4 versions, one or two zstd versions,
   the LZMA SDK) behind a `transform_id` that records *exactly which* build was used.
2. At pack time, try re-encode across the candidate (encoder, level, block-size) grid and
   `memcmp` against the original block. On any mismatch → store raw. This is cheap because it's
   per-256KB-block and parallel.
3. Keep every pinned encoder callable forever. A transform that cannot be inverted in five years
   is data loss.

Honest expectation: Unity ships a **forked** lz4; Bethesda's Archive2 and Starfield's tunable LZ4
use unknown parameters; UE's zstd plugin version varies per title. **[estimate: single-digit
percentage of blocks will round-trip on a first implementation.]** Do preflate, BCn and float
filters first; revisit this only if measurement on a real corpus shows a worthwhile hit rate.
Unity's *uncompressed* bundles and Source `.vpk` need none of this — they hand nova raw asset bytes
and are the best-case input.

### 4.5 GDeflate — unexploited, but half as easy as it first looks

**Finding:** GDeflate is **not a new compression algorithm**. Microsoft's README says the GDeflate
stream is *"essentially a reformatted version of any DEFLATE stream"* whose data is ordered to
extract 32-way parallelism *"without increasing the size of the input stream"*, so that *"GDeflate
can get very high decompression throughput on the GPU while still maintaining the exact same
compression ratio of DEFLATE (with some small caveats about end effects)"*
([DirectStorage GDeflate README](https://github.com/microsoft/DirectStorage/blob/main/GDeflate/README.md)).
Symbols are assigned to 32 sub-streams *"in the order in which they appear in the original DEFLATE
stream"*, with all bits of one literal or one length+distance pair kept in the same sub-stream, and
the serialization order is fully determined by the 32-bit refill rule
([GDeflate/GDeflate/README.md](https://github.com/microsoft/DirectStorage/blob/main/GDeflate/GDeflate/README.md),
[draft-uralsky-gdeflate-00](https://www.ietf.org/archive/id/draft-uralsky-gdeflate-00.html),
7 July 2024, expired). Pages are **64 KB and fully independent**.

> **Correction (verification pass) — the chain is NOT "bit-exact by construction" end to end.**
> An earlier draft claimed `GDeflate → un-swizzle → canonical DEFLATE → preflate` sidesteps §4.1
> entirely. The first half of that is right; the second half is wrong, and the IETF draft is the
> source that breaks it. **GDeflate is not RFC 1951**, in ways that matter to a deflate
> parameter-recovery model:
>
> - it folds in **DEFLATE64-style extensions**: length code 285 is *"re-purposed to represent an LZ
>   copy of length in the range between 3 and 65538 bytes"* with 16 extra unencoded bits, and
>   distance codes **30–31** (unused in RFC 1951) encode distances up to **65536** bytes with 14
>   extra bits each — i.e. a **64 KB window**, double zlib's 32 KB;
> - stored/uncompressed blocks are changed: *"The one's complement of length is dropped from the
>   header and the header is no longer required to be byte-aligned"*, so the whole page is one
>   contiguous, non-byte-aligned bit stream.
>
> Consequences for nova:
> 1. An un-swizzled GDeflate page is **not a valid RFC 1951 stream**. `preflate-rs` cannot consume
>    it as-is; it would need extended length/distance decoding and a 64 KB window.
> 2. preflate is a model of a *specific* encoder family's parsing decisions (zlib's greedy/lazy
>    match finder). GDeflate streams are produced by Microsoft's own encoder, **not zlib**, so even
>    with the format handled, the correction record is larger and the gain smaller than on real zlib
>    data. This is the same class of risk as §4.4, just milder — the *format* is specified even
>    though the *parse* is not.
> 3. What genuinely survives: **the swizzle itself is a specified, deterministic bijection**, so
>    `GDeflate ⇄ extended-DEFLATE token stream` needs no version pinning — only the tail-padding
>    bits stored verbatim. That is a real and unexploited property. It is the recompression of the
>    resulting token stream that is unsolved work, not a library call.

- Reference implementation: `microsoft/DirectStorage` — **MIT** (verified via GitHub API, active,
  last push 2026-08-14); includes the CPU codec and the HLSL shader. nvCOMP also implements it.
- Cost: a Huffman-decoding pass is required to discover sub-stream boundaries (you cannot
  un-permute without decoding), **plus** extending or forking a preflate model to cover DEFLATE64
  codes and a 64 KB window. Revise **medium-high → high** effort.
- Bounded memory is fine: 64 KB independent pages, so the working set is per-page regardless of
  container size (contrast §11.1).
- Value: real but now clearly second-order. DirectStorage GPU decompression is the industry's answer
  to Oodle on PC and **no archiver recompresses it**, so the *capability* claim stands.

**[estimate, revised down: an upper bound of deflate's gain on the same payload (10–60 % of
container), realistically well under that because the parse model is mismatched and preflate's
correction record grows on non-zlib streams. No published measurement of GDeflate recompression
exists — nobody has tried. Do not schedule this before B/C prove out.]**

---

## 5. Audio

### 5.1 What game audio actually is

Wwise ships four software codecs — **PCM, ADPCM, Vorbis, Opus** — plus platform hardware codecs
([Audiokinetic](https://blog.audiokinetic.com/en/a-guide-for-choosing-the-right-codec)). FMOD FSB5
carries **Vorbis, FADPCM, PCM, MP3**, plus AT9/XMA on console and optional **AES-128 encryption**
for DRM banks. vgmstream's `wwise.c` documents the WEM codec tag map (`0xFFFF` = Vorbis,
`0x0002`/`0x0069` = IMA ADPCM, `0x3039`/`0x3040`/`0x3041` = Opus variants, `0xFFFE` = PCM, XMA2,
ATRAC9, …) ([source](https://github.com/losnoco/vgmstream/blob/master/src/meta/wwise.c)) — that map
is a ready-made detection table for nova's analyzer.

### 5.2 Per-format verdict

| Format | Recompression | Gain | Bit-exact | Verdict |
|---|---|---|---|---|
| **PCM in RIFF/WAV** (still 14 GB in a 60 GB Unreal build; 35 GB in Titanfall) | FLAC on the `data` chunk, all other chunks stored verbatim (see 02 §5) | **35–50 %** **[measured]** | yes (wrapper preserved) | **ship it — largest audio win by far** |
| **Ogg Vorbis** (standard, with codebooks) | **OGGRE** — the only bit-exact Vorbis recompressor | **~6 % avg** (348 files, 44,053,199 → 41,387,188 B); **~26 %** on one large file (1,806,612 → 1,336,503 B). Beat paq8pxd -s9 on the same set **[measured, encode.su]** | yes | **no usable implementation exists** — closed tool, no source, no library. Defer. |
| **Wwise Vorbis / FMOD Vorbis** | — | — | — | **worse than plain Vorbis**: Wwise strips codebook definitions and assumes a fixed set, so codebook-rewriting tools don't apply. Store. |
| **Opus** (Wwise Opus, WEM `0x3039+`, Cyberpunk `.opuspak`) | none known | 0 % | — | store |
| **IMA ADPCM / FADPCM** | Academic only: [Lossless compression for μ-law/A-law and IMA ADPCM on the basis of a fast RLS algorithm](https://ieeexplore.ieee.org/document/871117/) — RLS predictor + Huffman, reproducing the standard's exact output. Reported 3.24 bits/sample vs 8-bit μ-law at 44.1 kHz **[measured, paper]**. No tool, and no published figure for the 4-bit game case. | **[estimate: 10–20 % on the nibble stream via a CM model keyed on `(step_index, previous nibbles)`. Unverified.]** | would be | **research project, not a feature.** A filter would first have to detect nibble order (Intel/DVI4 vs reversed) and block alignment. |
| **MP3** (FMOD banks) | packMP3 — see 02 §5 | ~16 % **[measured]** | yes | LGPL-3.0 plugin, v2 |
| **XMA2 / ATRAC9 / AAC** | none | 0 % | — | store |
| **AES-encrypted FMOD banks** | impossible | 0 % | — | store; detect and skip trial compression |

### 5.3 OptiVorbis — attractive, and wrong for us

[OptiVorbis](https://github.com/OptiVorbis/OptiVorbis) is **pure Rust**, actively developed
(pushed 2026-08-17), and does real work: optimal per-codebook Huffman reassignment, trimming unused
codebook symbols, tight page packing, stripping the reference encoder's bitrate-management padding.
But it is **sample-lossless, not byte-lossless** — the output `.ogg` differs structurally from the
input. An archiver must return the original file. Also **AGPL-3.0**. Both facts are disqualifying.
(It could only ever be an opt-in *lossy* "shrink my Ogg library" feature, which is a different
product.)

---

## 6. Meshes, animation, and float arrays

This is where nova can beat 7-Zip outright, because 7-Zip has **no float handling at all** (only a
manual fixed-stride `Delta:N`).

### 6.1 The measured winner is embarrassingly simple

Aras Pranckevičius's 9-part *Float Compression* series measured many approaches on 94.5 MB of real
game simulation state (2048² water grid × 4 floats, 1024² snow grid × 4 floats, plus float3/float4
arrays) ([intro](https://aras-p.info/blog/2023/01/29/Float-Compression-0-Intro/)):

| Approach | Result on the 94.5 MB corpus |
|---|---|
| **no filter, zstd (baseline)** | **33.8 MB** **[measured, stated in the text of part 3]** |
| AoS → SoA reordering alone | mixed: helped LZ4 marginally, **hurt** high-level zstd and Kraken **[measured]** |
| split floats + XOR delta | zstd reaches Kraken-class, ~28 MB **[measured]** |
| split floats + subtract delta | better again across all three compressors **[measured]** |
| byte-level split (1-byte streams, not 4-byte) | Kraken flat, zstd and LZ4 improve notably **[measured]** |
| **byte-split + delta** | **~22.9 MB — best result of the entire series** **[measured]** |
| fpzip | 24.8 MB, 0.6 s each way **[measured]** |
| meshoptimizer vertex codec + zstd | 24.3 MB **[measured]** |
| ndzip | 38.1 MB, > 1 GB/s **[measured]** |
| SPDP | between zstd and lz4; ~2× faster than fpzip but weaker ratio **[measured]** |
| **zfp in lossless mode** | **ratio < 1.0 — it made the data bigger** — and slow to decode **[measured]** |
| streamvbyte | 1.2× only, but 5.7 GB/s compress / 10 GB/s decompress **[measured]** |

Author's conclusion: *"split by floats + general compressor"* consistently beat the specialised
scientific codecs on this data, and most scientific libraries are tuned for lossy mode or for
higher-dimensional grids
([part 3](https://aras-p.info/blog/2023/02/01/Float-Compression-3-Filters/),
[part 5](https://aras-p.info/blog/2023/02/03/Float-Compression-5-Science/)).

> **Correction (verification pass).** An earlier draft said the unfiltered baseline lived only in
> interactive charts and hedged the §1 gain as "roughly 30 MB class". It is stated in the prose of
> part 3: **33.8 MB** unfiltered zstd, dropping to **~23 MB** with byte-split + delta (22.9 MB in
> part 5's comparison table). So the filter is worth **≈32 % over unfiltered zstd on this corpus**
> **[measured]**, not a hedge. That makes recommendation 3 the best measured ratio-per-LoC in the
> whole report — better supported than the BCn transform, and cheaper.
>
> Still measure locally: this corpus is grid/simulation state, not vertex buffers, and the numbers
> are zstd-class. nova's max tier is LZMA2/PPMd7, where a byte-split changes the match structure
> differently — the filter must go through the existing per-unit trial-compression tournament rather
> than being applied on faith.

**Implementation for nova:** a 4-stream (f32) / 8-stream (f64) byte transpose plus optional
per-stream delta. ~50 lines, SIMD-friendly, exactly invertible, ~GB/s. Same code covers Parquet's
`BYTE_STREAM_SPLIT`, which Apache added for precisely this reason: general-purpose text compressors
*"do not handle FP data very well"* ([PARQUET-1716](https://issues.apache.org/jira/browse/PARQUET-1716)).
Consider also 7-Zip's experimental **Swap4** (32-bit word byte-reversal — big-endian ordering
compresses better under LZMA), which Igor Pavlov ships but does not enable by default because it
helps only pure code/array sections and hurts everything else — a good argument for nova's
per-unit trial-compression selection rather than global filter flags.

### 6.2 When to reach for a real numeric codec

| Library | Language / licence | Numbers | Fit for nova |
|---|---|---|---|
| **[`pco` (pcodec)](https://github.com/pcodec/pcodec)** 1.0.3 | **pure Rust, Apache-2.0** | **29–94 % higher ratio than all alternatives** (Blosc+zstd, Parquet+zstd, TurboPFor+zstd, LZ4, Brotli) at similar compression time, even granting them 50 % more time; 23–48 % storage reduction. Taxi f64: pco **6.89–6.98** vs Parquet+zstd(22) **5.32** vs Blosc+zstd(9) **2.85** **[measured, paper + repo]** ([arXiv:2502.06112](https://arxiv.org/pdf/2502.06112), [benchmarks](https://github.com/pcodec/pcodec/blob/main/docs/benchmark_results.md)) | **strong candidate for detected numeric arrays.** Takes `&[f32]`/`&[f64]`, not bytes — so it needs §6.4 structure detection. Chunk per *column*: interleaving columns into one chunk "gives bad compression" per the docs. Only 11k LoC. |
| **[`alp`](https://github.com/spiraldb/alp)** 0.0.2 (SpiralDB port of CWI's ALP) | **pure Rust, Apache-2.0** | ALP beats Gorilla/Chimp/Elf/Patas/PseudoDecimals and zstd on ratio *and* speed; reproducibility-reported at SIGMOD 2024. ALP-RD gets f64 to ~54 bits typical (~12.5 % saving) and terminates there **[measured, paper]** ([PACMMOD 10.1145/3626717](https://dl.acm.org/doi/10.1145/3626717), [repro report](https://dl.acm.org/doi/10.1145/3687998.3717057)) | classic ALP shines on *decimal-origin* doubles (rare in game data); ALP-RD's ~12.5 % on real doubles is weaker than byte-split+delta. **Second-tier.** Note the Rust port handles ±0/±inf/NaN correctly *because* Rust defines float→int cast semantics — relevant to bit-exactness. |
| **[meshoptimizer](https://github.com/zeux/meshoptimizer)** vertex/index codecs | C/C++, **MIT**; Rust via [`meshopt`](https://github.com/gwihlidal/meshopt-rs) | *"The codec is lossless by itself"*; vertex 2–4× over already-quantized data, indices ~1 byte/index (1–3 bits with a general compressor on top), decode 3–6 GB/s. On Aras's non-mesh float data: 24.3 MB with zstd **[measured]** | **conditionally yes.** Requires the vertex **stride** and a vertex-cache/fetch-optimized ordering; unoptimized input compresses poorly. **Bit-exactness caveat: pin `meshopt_encodeVertexVersion`.** v1 is now default; upstream promises v0+v1 encode/decode "in perpetuity". Padding bytes must be zero-initialized. Its *filters* (oct/quat/exp) are **lossy — never use them.** |
| **fpzip** | C++, BSD-ish | 24.8 MB vs 22.9 MB for byte-split+delta **[measured]** | loses to a 50-line filter. Skip. |
| **zfp** lossless | C++ | **ratio < 1.0 on 1D/2D float game data** **[measured]** | **reject** |
| **SPDP, ndzip, streamvbyte** | C/C++ | see table §6.1 | speed-tier only; nova already has zstd for that role |
| **Draco** (glTF mesh compression) | C++, Apache-2.0 | — | **reject: quantizing, lossy.** Cannot reproduce input bytes. |

### 6.3 What in a game folder is a float array

- glTF/GLB `.bin` buffers (accessor-typed — the *format tells you* the stride and component type,
  so detection is exact, not heuristic);
- FBX binary arrays (typed array records, likewise self-describing, and often already
  deflate-compressed → preflate first);
- `.nif` (Bethesda) vertex/normal/UV blocks, `.uasset` vertex buffers, Unity `Mesh` serialized
  arrays inside a bundle;
- **animation**: keyframe curve tracks are long float sequences with strong first/second-difference
  structure — the single best case for `delta`, possibly `delta-of-delta`;
- particle/simulation caches, heightmaps, navmeshes, physics collision data;
- point clouds and photogrammetry intermediates in dev trees.

### 6.4 Structure detection: the piece nova must build

Everything above hinges on knowing "this byte range is an array of N-byte elements". Three tiers,
in confidence order:

1. **Format-driven (exact).** Parse the container: glTF accessors, FBX array records, DDS/KTX2
   headers, WAV `fmt ` chunk. Zero heuristics, zero false positives. Do this first.
2. **Header-declared stride** in engine formats (Unity `VertexData` channels, UE `FVertexBuffer`).
   Needs per-engine parsers — high value, ongoing maintenance cost.
3. **Blind stride detection (heuristic).** For unknown blobs: for each candidate stride
   *s* ∈ {2,3,4,6,8,12,16,20,24,32,48,64}, compute a cheap score over a sample window — e.g. mean
   |byte[i] − byte[i−s]| (autocorrelation of differences), or per-column byte entropy after a
   virtual transpose. Pick the *s* minimizing total estimated entropy; require a margin over
   *s* = 1 before applying anything. Then **verify by trial compression** — nova already has the
   trial-compression machinery from its analyzer, and `lossless-transform-utils` (MIT) gives a
   2.5 GB/s estimator so the search is affordable. Sewer56's
   [`struct-compression-analyzer`](https://crates.io/crates/struct-compression-analyzer) is prior
   art for the offline version of this workflow (declare a schema, get per-field zstd sizes) and is
   worth reading as a design reference.

Report [04-freearc-lessons.md](04-freearc-lessons.md) already flags auto-stride delta as a nova
differentiator (FreeArc's `delta` filter auto-detected tables; 7-Zip only offers manual `Delta:N`).
This section is the concrete plan for it. Expected **+5–30 % on structured binaries [estimate,
per 04]**; the float measurements in §6.1 are the calibrated end of that range.

---

## 7. Already-compressed generic data — game-specific delta only

Fully covered in [02-recompression.md](02-recompression.md). What changes in a game context:

| Class | Note specific to games |
|---|---|
| **deflate** | Higher hit rate than in a general file set, because so many engine containers are zlib (§2.1). Priority 1 stands. |
| **JPEG** (lepton_jpeg, ~22 %) | **Rare in shipped games** — engines want GPU-native formats. Common in *dev trees*, UI/marketing assets, and Steam/store metadata. Keep it (cheap, pure Rust) but don't expect it to move a game folder. |
| **PNG** (preflate, 5–30 %) | Same: UI atlases, dev source art, mod distributions. Real but secondary. |
| **MP3** (packMP3, ~16 %) | FMOD banks can carry MP3. LGPL plugin, v2. |
| **Video** (`.bk2`, `.usm`, `.webm`, H.264/265 cutscenes) | **Store. Confirmed no maintained lossless recompressor exists** (02 §6). Cutscenes are often the largest single files in an install — the correct win here is *not wasting 20 s of LZMA on them*, which is a speed win, not a ratio win. |
| **AES-encrypted assets** (FMOD DRM banks, some `.pak` with encryption) | Mathematically incompressible. Detect and store. |

---

## 8. Column/record-structured binaries: save files and databases

Same machinery as §6.4, different payloads. What shows up in a game folder:

- **Save games**: very often **already deflate-compressed** (Skyrim/Fallout saves use zlib,
  Minecraft region files use zlib, Factorio saves are zip) → **preflate first**, then the
  decompressed payload is exactly the record-structured data that §6.4 targets. Chain, don't choose.
- **Engine databases and index tables**: UE `.uasset`/`.uexp` name/import/export tables,
  `.utoc` chunk directories, Unity `SerializedFile` type trees, SQLite (telemetry, achievements),
  localization string tables. These are arrays of fixed-width structs full of **near-monotonic
  offsets, IDs and hashes**. Delta on the offset columns is nearly free and can be dramatic.
- **Shader caches** (5 GB of the 60 GB audit): bytecode + a hash-keyed index. The index is
  record-structured; the bytecode is machine code for a virtual ISA — an unexplored target where a
  DXBC/SPIR-V-aware filter could plausibly do what BCJ does for x86. **[estimate: unquantified, no
  published work found. Interesting, speculative, do not schedule.]**

Practical rule: **transposition and delta are orthogonal and both are exactly invertible.** Try
{identity, transpose(s), delta(s), transpose(s)+delta(s)} and keep the smallest per unit. Cost is
bounded by the estimator; risk is zero because everything is verified.

---

## 9. Executables beyond BCJ

Measured numbers, most reliable first:

| Filter | Gain | Source |
|---|---|---|
| **x86 BCJ vs no filter** | **6–8 %** on x86 executables | Igor Pavlov / [7-Zip docs](https://documentation.help/7-Zip/method.htm) **[claimed by author, widely reproduced; the mirror 403s, not re-verified in the 2026-08 pass]**. **Local cross-check: nova's own measurement is +4.4–5.7 % on a real `.exe` across every codec/level (ROADMAP) — i.e. the low end of the published range or slightly below. Quote our number, not Pavlov's.** |
| **BCJ2 vs BCJ** | 7,485 KB `AcroRd32.exe`: BCJ → 2,460 KB, BCJ2 → **2,228 KB** ⇒ **~9.4 % smaller**. Pavlov cautions this test used an 8 MB dictionary and BCJ2's 3 streams/3 dictionaries inflate the advantage; **the gap shrinks with a large dictionary** ([thread](https://sourceforge.net/p/sevenzip/discussion/45797/thread/fd464404/)) **[measured, single file]** |
| **ARM64 filter** | **~5 %** smaller compressed Linux kernel vs unfiltered XZ or plain LZMA — verified against Lasse Collin's own patch text ("xz: Adjust arch-specific options for better kernel compression", 2024; [LWN summary](https://lwn.net/Articles/982751/), [lore thread](https://lore.kernel.org/linux-kernel/20240320183846.19475-6-lasse.collin@tukaani.org/T/), [commit mirror](https://git.zx2c4.com/linux-rng/commit/?id=7472ff8adad8655f38b060a602f66e59c93c4793)) **[measured]** |
| **RISC-V filter** | **~7 %** on the same basis, same patch **[measured]**. Same series notes matching LZMA2 alignment options are worth a further **0–2 %** (nothing on 1-byte-aligned x86, most on 4-byte-aligned archs) — see the alignment row below |
| **dispack** (kkrunchy-derived, disassembly-based; FreeArc) | Corpus-dependent and **occasionally negative**: on `skype.exe` (19,490,344 B) no filter 8,060,598 → BCJ/BCJ2 7,856,207 (−2.6 %) → **disasm filter 8,279,426 (worse than none)**; durilca'light's disasm filter improved its own output by 11.1 %; a separate case in the same thread reports 18.2 % ([encode.su](https://encode.su/threads/557-disasm-based-executable-s-filter)) **[measured, anecdotal]** |
| **Alignment tuning** | Pair the filter with matching LZMA2 params: ARM64 → `pb=2,lp=2,lc=2`; RISC-V with C-extension → `pb=2,lp=1,lc=3` ([xz(1)](https://man.archlinux.org/man/xz.1.en)) **[documented]** |

**Honest scoping for this report's topic:** in a 60 GB game install the executables are well under
1 % of the bytes. *(Correction, verification pass: an earlier draft claimed "x86 BCJ on a Fedora 31
live image saved 30 MB out of ~1.7 GB compressed (1.7 %) while doubling install time". I could not
source those figures and they appear garbled — the actual primary source is Fedora's
[Changes/OptimizeSquashFS](https://fedoraproject.org/wiki/Changes/OptimizeSquashFS) proposal, which
went the other way: as of F31 lorax applied a per-arch BCJ filter, and the proposal **recommends
dropping BCJ x86** in favour of plain `xz -b 1M -Xdict-size 1M`, concluding that has minimal impact
on install time and CPU. The conclusion "BCJ on a mostly-non-executable payload is not worth it" is
supported; the specific 30 MB / 1.7 GB / "doubling" numbers are withdrawn.)*
**Do implement BCJ2 + ARM64 + RISC-V** (cheap,
well-understood, matters a lot for *source trees and OS images*, which is a different report's
fight), but do not expect them to move a game benchmark. **Reject dispack**: unstable sign,
packed/protected binaries break its assumptions, and it is a large disassembler to maintain.

---

## 10. The fast path for genuinely incompressible data

Half of "beating 7-Zip on game data" is *speed*, and the cheapest speed win is refusing to try.

1. **Format-based rejection first** (free): Oodle-compressed container blocks, `.opus`/Opus WEM,
   AAC, `.bk2`/`.usm`/`.webm`, AES-encrypted FMOD banks, BasisLZ/ETC1S KTX2 payloads, `.7z`/`.zst`/
   `.br` payloads.
2. **Entropy/compressibility estimation second** (~2.5 GB/s): `lossless-transform-utils` (MIT,
   Rust, active) reports ~74.4 % agreement with zstd at high levels **[claimed]** — enough to route
   a unit to *store* without a trial compression. Its sibling zstd-level-1 estimator runs at
   ~1060 MiB/s with 79.2 % agreement **[claimed]**.
3. **Never expand.** A store fallback per unit is already nova's design; make sure it is also a
   *per-transform* fallback (transform → verify → compare size → keep the smaller).
4. **Dedupe still pays on incompressible data.** This is the point most reviewers miss: two patch
   versions of the same Oodle-compressed `.ucas` share the vast majority of their 256 KB blocks
   byte-for-byte. nova's FastCDC + blake3-128 dedupe extracts value where every codec scores 0 %.

---

## 11. Recommended build order for nova

| Phase | Work | Why now |
|---|---|---|
| **A0** | **Measure the corpus first**: byte shares by extension over 3–4 real installs (§2 source warning) | One afternoon; replaces the report's weakest input and decides whether B or D leads |
| **A** | Ship preflate (already decided), scoped per §11.1(2) to containers with payload blocks ≤ chunk size, and **immediately test the preflate → BCn chain** on a Fallout 4 `.ba2` and a UE4 `.pak` | Highest measured value, and the chain is the differentiator no competitor has |
| **B** | **BCn split transform for BC1/BC2/BC3/BC4/BC5** (own implementation, ~200 LoC + tests + fuzzing) with `.dds`/KTX2-scheme-0 detection **and the §11.1(1) phase/variant param byte** | **12.3 % measured** (§3.1) on the largest data class in a game install, at memcpy speed. Best ratio-per-line-of-code *after* C |
| **C** | **Byte-split + delta filter** with format-driven detection (glTF/FBX/WAV/DDS headers) and trial-verified blind stride detection | **≈32 % measured** over unfiltered zstd on real sim data (§6.1) for ~50 LoC — the best-supported number in this report. Unlocks meshes, animation, sim data, and doubles as the §8 record transform. **Consider swapping C ahead of B** |
| **D** | WAV→FLAC (from 02); BCJ2 + ARM64 + RISC-V exe filters | Known quantities, low risk |
| **E** | `pco` for detected numeric arrays where C's filter underperforms; Unity/UE structure parsers | 29–94 % over zstd-class **[measured — verbatim from the paper's abstract, on "six real-world columnar datasets", i.e. tabular/scientific, not game data]**; needs C's detection to be trustworthy first |
| **F** | **GDeflate de-swizzle → preflate** | Novel capability, **high** effort (revised: the un-swizzled stream is not RFC 1951, so preflate must be extended — §4.5), growing relevance |
| **G** | BC7/BC6H mode-aware split; Ström-style image-domain index predictor | Research-grade; only after B proves out |
| **Never / not now** | Oodle, zfp, Draco, crunch/Basis, OptiVorbis, dispack, ASTC splitting, video, Opus | See §12 |

**Two non-negotiables** (restating 02's rules because they bind harder here): every transform must
be **round-tripped and byte-compared at pack time** with a store fallback, and every archive must
record `(transform_id, codec_version)` per stream with all old decoders kept callable forever. For
§4.4's re-encode gamble this is not a best practice — it is the only thing that makes it legal to
attempt at all.

### 11.1 Collision check against nova's existing invariants (added in verification pass)

The transforms above were researched in the abstract. Checked against nova's actual format
(`ROADMAP.md`: chunk = compression unit = edit granularity, max tier 1/4/16 MiB FastCDC; solid
blocks 4/16/32 MiB from files < 256 KiB sorted by extension; `filter → compress`, `decompress →
unfilter`; chunk hash covers the **original** bytes; *"all ops bounded by a few MAX_CHUNK buffers +
manifest in RAM — weak-PC extraction is a hard requirement"*), four issues appear that none of the
sections above mention. Two are cheap; two change the build order.

**(1) The BCn split is phase-sensitive and FastCDC does not respect block phase.** A BC1 payload is
8-byte blocks starting at DDS offset 128 (148 with a DX10 header); BC2/BC3/BC5/BC7 are 16-byte
blocks. FastCDC cut points are content-defined, so a chunk generally starts at an offset that is
**not** ≡ payload-start (mod block size). Split such a chunk and the "colours" plane fills with
index bytes: the transform still round-trips (it is a permutation either way, so **no correctness
bug and no test failure**) but the 12 % silently becomes ~0 % or negative. This is the failure mode
that would make phase B look like a dud for the wrong reason.

Fix, and it is small: the per-chunk **param byte** (nova already has one for `delta(N)`) records
`(BCn variant, phase)` where phase = `(chunk_file_offset − payload_start) mod block_size`, computed
by the packer from the analyzer's DDS/KTX2 parse. Critically this is **dedup-safe**, unlike BCJ:
nova's `start_offset 0` rule for BCJ exists because a deduped chunk is stored once and must unfilter
correctly at every position it is referenced from. Here the unfilter is *self-describing* from the
stored param byte, and the hash covers the original bytes, so a shared chunk round-trips correctly
regardless of which reference's phase was used to pack it — only that one copy's ratio is
suboptimal. Also decide explicitly whether `.dds`/`.ktx2` files under 256 KiB are **excluded from
solid blocks**: a solid block is many concatenated files, so it has no single phase, and per-member
transform-before-concatenation is the only way to keep the gain there.

**(2) Container recompression needs a compression unit that is the container's block, not a CDC
chunk.** A 4 GB `.ba2`/`.pak` gets cut into 16 MiB CDC chunks whose boundaries fall in arbitrary
places inside deflate streams; preflate needs a *complete* stream. The phase-A "preflate → BCn
chain" therefore implies a container-parsing path that **overrides chunk geometry** — and geometry
is currently pinned as an archive property. Bethesda/UE4 are the friendly case (independent
64–256 KB blocks, so container-aligned units are naturally small); Unity's `flags&0x3F == 1`
whole-content-stream LZMA and any monolithic single-stream container are the hostile case, because
one indivisible unit can be gigabytes. **Decision to make before phase A: recompression applies
only to containers whose payload blocks are ≤ the tier's chunk size, and monolithic streams route to
store.** Otherwise cheap edits and bounded memory both fall over on exactly the biggest files.

**(3) Mechanism (b) (§4.4) taxes *extraction*, not just packing — this is a harder objection than
the low hit rate.** Under (b) nova stores the plaintext plus "re-encode with encoder X vY params Z";
regenerating the original bytes at **extract** time means *running the original compressor*. LZMA
and zstd-19 encode at roughly 1–5 MB/s/core against 50–200 MB/s decode, with encoder-side memory of
several times the dictionary. So a §4.4 archive extracts at compression speed and compression
memory — a direct collision with nova's declared hard requirement that extraction always work on a
weak PC. The other transforms are safe on this axis: BCn split and byte-split+delta are memcpy-speed
permutations, FLAC decode is fast, preflate's re-deflate is tens of MB/s, GDeflate re-swizzle is
per-64 KB-page. **Add to §4.4's verdict: even a block that round-trips must be rejected if its
re-encode cost exceeds a per-unit budget, and the whole feature needs an `--extract-fast` escape
hatch or it should stay unbuilt.**

**(4) Transform output can exceed MAX_CHUNK.** preflate turns a compressed block into
plaintext + a correction record — potentially 10–60× the input. The unfilter path therefore needs a
buffer sized by the *decompressed* payload, not by the chunk, which the current "few MAX_CHUNK
buffers" invariant does not cover. Cap it: record the expansion ratio per unit at pack time and
refuse the transform above a fixed multiple, so the extract-side working set stays provably bounded.

None of this invalidates a recommendation. (1) and (4) are param-byte and guard-rail work; (2)
narrows recompression's scope to container families that are already the ones worth doing; (3)
demotes §4.4 further, for a better reason than the one given there.

---

## 12. Negative knowledge — investigated and rejected

*(Each entry is a lead that costs days if pursued. The reason is the point.)*

**Textures**
- **Oodle Texture / BC7Prep** — 5–15 % on BC7 and lossless, but proprietary Epic Games Tools
  licence, no FOSS access. Its existence is useful only as a *target number*.
- **crunch / crnlib, Basis Universal, Oodle Texture RDO** — all **lossy re-encoders**. They change
  the BCn bits. An archiver must return the input file. Structurally unusable.
- **zfp in lossless mode** — measured **ratio below 1.0** (expanded the data) on 1D/2D float game
  data, and slow. Do not evaluate again.
- **`dxt-lossless-transform` as a dependency** — **GPL-3.0/NOASSERTION**, and WIP ("BC7 barely
  started"). Read the README, reimplement the reversible steps.
- **The solid-block normalization step** in that project — **not bit-exact** (rewrites BCn bits to
  a canonical form). Visually lossless ≠ lossless. Must be excluded.
- **ASTC field splitting** — variable per-block bit layout with reverse-ordered weight bits; no
  clean permutation boundary. No published lossless recompression result for ASTC.

**Containers / codecs**
- **Oodle Kraken/Mermaid/Leviathan recompression** — three independent blockers: (1) no
  parameter-recovery model exists; (2) bit-exact re-encode needs the game's own version-matched
  `oo2core_N_win64.dll`, and xtool *removed* its oodle xdelta path because CRC mismatches were
  common; (3) licensing — official Oodle is UE-plugin-only, `ooz` is GPL-3.0, its compressor forks
  are "educational use only". **The single most important "no" in this report.**
- **zstd / LZ4 / LZMA payload re-encoding as a *reliable* feature** — upstream zstd states outright
  there is no cross-version output guarantee; LZ4HC's parser is exactly what upstream retunes;
  output can even vary with SIMD availability or endianness. Buildable as a verified-or-store
  gamble; **not** promisable as a ratio number.
- **GDeflate treated as an opaque codec** — it is *not*. The swizzle is a specified, deterministic
  bit permutation, so treating GDeflate as incompressible leaves something on the table (the inverse
  mistake to the Oodle one). **But the corrected form of this lead matters more than the lead:** an
  un-swizzled GDeflate page is **not a valid RFC 1951 stream** — per draft-uralsky-gdeflate-00 it
  uses DEFLATE64-style length code 285 (matches to 65538 B, 16 extra bits), distance codes 30–31
  (to 65536 B, 14 extra bits ⇒ 64 KB window vs zlib's 32 KB), and stored blocks with the
  one's-complement length dropped and no byte alignment. `preflate-rs` cannot consume it, and
  preflate models zlib's parser, not Microsoft's. Do not schedule this as "port a permutation".
- **Reading `struct-compression-analyzer`'s per-field percentages as ratios of that field** — they
  are shares of the **whole file's compressed size**. Comparing the colours field's `26.58 %`
  against the file's `67.05 %` mixes denominators and roughly doubles the apparent effect. Recompute
  from the raw byte counts (§3.1); the true split gain on that file is 12.3 %.
- **`dredyson.com` as a composition source** — SEO-shaped, no project/engine/platform named, marks
  of machine generation. Its 47 % / 22 % split is the only itemized modern breakdown I could find,
  which is a statement about the literature, not about the number's reliability. Measure locally.
- **`ooz`/`gooz`/`Oozle` as a decode-only helper** — GPL-3.0, and useless anyway: decoding without
  a bit-exact encoder cannot restore the file.

**Audio**
- **OptiVorbis** — pure Rust and excellent, but **sample-lossless, not byte-lossless**, and
  **AGPL-3.0**. Two independent disqualifications.
- **OGGRE** — the only bit-exact Vorbis recompressor and it works (~6 % avg, 26 % best), but there
  is **no source, no library, no licence**. Nothing to integrate.
- **Wwise/FMOD Vorbis via codebook-rewriting tools** — Wwise **strips codebook definitions** and
  assumes a fixed decoder-side set, so codebook-optimizing approaches don't apply to game Vorbis at
  all.
- **`mp3packer`** — losslessly *rearranges* frames but does not reproduce the original file bytes
  (already rejected in 02; restated because it keeps resurfacing in game-audio contexts).
- **ADPCM recompression** — the technique is proven in a 2000-era IEEE paper (RLS predictor +
  Huffman, exact standard output) but **no implementation exists in any language**, and no
  published figure for the 4-bit game case. Would require original research plus nibble-order and
  block-alignment detection.
- **Opus / AAC / XMA2 / ATRAC9** — no known lossless recompressor, and all use adaptive
  arithmetic/range coding with little residual redundancy. Store.

**Floats / meshes**
- **Draco** — quantizes. Lossy. Cannot reproduce input bytes.
- **meshoptimizer's oct/quat/exp filters** — explicitly lossy in upstream docs. The *codecs* are
  lossless; the *filters* are not. Easy and fatal confusion.
- **fpzip, SPDP, ndzip, streamvbyte** — all measurably worse than a 50-line byte-split+delta filter
  plus zstd on real game float data. Only ndzip/streamvbyte have a story, and it is throughput,
  which zstd already covers for nova.
- **AoS→SoA reordering *alone*, without delta** — measured to **hurt** high-level zstd and Kraken.
  The delta step is what makes the transposition pay.
- **ALP classic mode for game data** — designed for doubles that originated as decimals
  (finance/sensor). Game floats are real doubles/singles; ALP-RD's ~12.5 % is beaten by
  byte-split+delta. Keep `alp` on the shelf, not in the pipeline.

**Executables / misc**
- **dispack / disfilter** — measured *worse than no filter at all* on one packed real-world binary
  (8,279,426 vs 8,060,598 bytes), swinging to +18 % on others. Unstable sign plus a full x86
  disassembler to maintain. Reject.
- **Chasing exe filters as a game-data win** — executables are < 1 % of a game install; the Fedora
  live-image datapoint shows x86 BCJ delivering 1.7 % overall while doubling pack time. Implement
  them for source trees and OS images, not for this benchmark.
- **Video recompression (H.264/H.265/AV1 cutscenes)** — no maintained tool exists in 2026; the
  useful action is *detection to skip*, which is a speed win.
- **A "structure detector" without trial verification** — blind stride detection on a
  format-agnostic blob has a real false-positive rate. Every detected transform must be
  round-trip-verified and size-compared, or nova will silently make files bigger and, worse,
  eventually corrupt one.

---

## 13. Sources

**Textures**
- Ström & Wennersten, *Lossless Compression of Already Compressed Textures*, HPG 2011: <http://www.jacobstrom.com/publications/StromWennerstenHPG2011.pdf> · <https://dl.acm.org/doi/10.1145/2018323.2018351>
- GST: GPU-decodable Supercompressed Textures: <https://dl.acm.org/doi/10.1145/2980179.2982439>
- Oodle Texture / BC7Prep: <http://www.radgametools.com/oodletexture.htm> · RDO examples: <http://www.radgametools.com/oodletextureexamples.htm>
- cbloom, *Improving the compression of block-compressed textures Revisited*: <http://cbloomrants.blogspot.com/2018/03/improving-compression-of-block.html> · *Performance of various compressors on Oodle Texture RDO data*: <http://cbloomrants.blogspot.com/2020/07/performance-of-various-compressors-on.html>
- dxt-lossless-transform (GPL, read-only reference): <https://github.com/Sewer56/dxt-lossless-transform> · BC1 method: <https://github.com/Sewer56/dxt-lossless-transform/blob/main/src/core/dxt-lossless-transform-bc1/README.MD>
- Sewer56 blog: <https://sewer56.dev/blog/2025/01/15/estimating-compressibility-of-data--bc7.html> · <https://sewer56.dev/blog/2025/03/11/a-program-for-helping-create-lossless-transforms.html>
- `lossless-transform-utils` (MIT): <https://github.com/Sewer56/lossless-transform-utils> · `struct-compression-analyzer`: <https://crates.io/crates/struct-compression-analyzer>
- crunch/crnlib: <https://github.com/BinomialLLC/crunch> · Unity Crunch/ETC: <https://blog.unity.com/engine-platform/crunch-compression-of-etc-textures>
- Basis Universal / KTX2: <https://github.com/BinomialLLC/basis_universal/wiki/KTX2-File-Format-Support-Technical-Details> · Khronos KTX2 spec: <https://github.khronos.org/KTX-Specification/ktxspec.v2.html> · `KHR_texture_basisu`: <https://github.com/KhronosGroup/glTF/blob/main/extensions/2.0/Khronos/KHR_texture_basisu/README.md>

**Containers**
- Unreal Oodle Data: <https://dev.epicgames.com/documentation/en-us/unreal-engine/oodle-data> · Oodle-free-in-UE announcement: <https://www.unrealengine.com/en-US/blog/oodle-now-free-to-use-in-unreal-engine> · UnrealReZen (zlib default): <https://github.com/rm-NoobInCoding/UnrealReZen>
- Unity AssetBundle compression: <https://docs.unity3d.com/6000.1/Documentation/Manual/assetbundles-compression-format.html> · UnityFS binary layout: <https://imbushuo.net/blog/archives/505/>
- Bethesda archives: <https://ryan-rsm-mckenzie.github.io/bsa/index.html> · <https://modding.wiki/en/skyrim/users/bethesda-archives>
- Valve VPK: <https://developer.valvesoftware.com/wiki/VPK_(file_format)> · Titanfall branch: <https://developer.valvesoftware.com/wiki/Titanfall_engine_branch>
- ooz (GPL-3.0): <https://github.com/powzix/ooz> · compressor forks: <https://github.com/zao/ooz> · <https://github.com/rarten/ooz>
- xtool + oodle CRC-mismatch history: <https://github.com/Razor12911/xtool> · <https://github.com/Razor12911/xtool/blob/main/changes.txt> · <https://encode.su/threads/4000-Xtool-Some-tool-repackers-like-to-use> · <https://fileforums.com/archive/index.php/t-102453.html>
- GDeflate: overview + ratio claim <https://github.com/microsoft/DirectStorage/blob/main/GDeflate/README.md> (**this** is the file with the "same compression ratio" sentence — not the nested `GDeflate/GDeflate/README.md`, which covers sub-stream mechanics) · IETF draft (deviations from RFC 1951, 64 KB pages): <https://www.ietf.org/archive/id/draft-uralsky-gdeflate-00.html> · nvCOMP: <https://docs.nvidia.com/cuda/nvcomp/gdeflate.html>
- zstd output stability: <https://github.com/facebook/zstd/issues/4049> · <https://github.com/facebook/zstd/issues/999> · <https://github.com/facebook/zstd/issues/4173> · <https://github.com/facebook/zstd/issues/4099> · <https://github.com/facebook/zstd/issues/1077>
- LZ4 determinism caveats: <https://github.com/lz4/lz4> · <https://github.com/pierrec/lz4/issues/65>
- Precomp/SREP game-repack practice: <https://encode.su/threads/2076-Some-questions-about-games-compressions> · <https://encode.su/threads/3223-precomp-further-compress-already-compressed-files>

**Audio**
- Wwise codec guide: <https://blog.audiokinetic.com/en/a-guide-for-choosing-the-right-codec> · WEM codec tag table: <https://github.com/losnoco/vgmstream/blob/master/src/meta/wwise.c>
- FMOD FSB5: <https://github.com/HearthSim/python-fsb5> · <https://github.com/SamboyCoding/Fmod5Sharp>
- OGGRE / Vorbis recompression thread: <https://encode.su/threads/3256-Lossless-(Re)compression-of-Ogg-files> · page 2: <https://encode.su/threads/3256-Lossless-(Re)compression-of-Ogg-files/page2>
- OptiVorbis (AGPL, sample-lossless): <https://github.com/OptiVorbis/OptiVorbis>
- μ-law/IMA-ADPCM lossless recompression (IEEE): <https://ieeexplore.ieee.org/document/871117/> · IMA ADPCM format: <https://wiki.multimedia.cx/index.php/IMA_ADPCM>

**Floats / meshes / structured data**
- Aras Pranckevičius, *Float Compression* series: <https://aras-p.info/blog/2023/01/29/Float-Compression-0-Intro/> · filters: <https://aras-p.info/blog/2023/02/01/Float-Compression-3-Filters/> · mesh optimizer: <https://aras-p.info/blog/2023/02/02/Float-Compression-4-Mesh-Optimizer/> · science codecs: <https://aras-p.info/blog/2023/02/03/Float-Compression-5-Science/>
- pcodec: <https://github.com/pcodec/pcodec> · paper: <https://arxiv.org/pdf/2502.06112> · benchmarks: <https://github.com/pcodec/pcodec/blob/main/docs/benchmark_results.md>
- ALP: <https://dl.acm.org/doi/10.1145/3626717> · <https://github.com/cwida/ALP> · Rust port: <https://github.com/spiraldb/alp> · <https://spiraldb.com/blog/alp-rust-is-faster-than-c>
- FCBench (FP compression survey): <https://arxiv.org/pdf/2312.10301>
- meshoptimizer: <https://github.com/zeux/meshoptimizer> · <https://meshoptimizer.org/> · Rust: <https://github.com/gwihlidal/meshopt-rs>
- Parquet BYTE_STREAM_SPLIT rationale: <https://issues.apache.org/jira/browse/PARQUET-1716>

**Executables**
- BCJ overview: <https://en.wikipedia.org/wiki/BCJ_(algorithm)> · 7-Zip method docs: <https://documentation.help/7-Zip/method.htm> · 7-Zip history (ARM64/RISCV filters, BCJ2 240 MiB): <https://www.7-zip.org/history.txt>
- BCJ vs BCJ2 head-to-head: <https://sourceforge.net/p/sevenzip/discussion/45797/thread/fd464404/>
- Kernel ARM64/RISC-V filter gains (Lasse Collin, "xz: Updates to license, filters, and compression options", 2024): <https://lwn.net/Articles/982751/> · <https://lore.kernel.org/linux-kernel/20240320183846.19475-6-lasse.collin@tukaani.org/T/> · commit mirror <https://git.zx2c4.com/linux-rng/commit/?id=7472ff8adad8655f38b060a602f66e59c93c4793> · filter/alignment driver <https://github.com/torvalds/linux/blob/master/scripts/xz_wrap.sh>
- 7-Zip method list incl. Swap2/Swap4 (and why Swap4 is off by default): <https://7zip.bugaco.com/7zip/Methods.txt> · <https://sourceforge.net/p/sevenzip/discussion/45797/thread/b8418c1d0b/>
- Fedora SquashFS/BCJ decision (BCJ x86 dropped in favour of plain `xz -b 1M`): <https://fedoraproject.org/wiki/Changes/OptimizeSquashFS>
- xz filter/LZMA2 alignment guidance: <https://man.archlinux.org/man/xz.1.en>
- dispack/disasm filter measurements: <https://encode.su/threads/557-disasm-based-executable-s-filter>

**Install composition**
- <https://www.pcgamer.com/why-are-game-install-sizes-getting-so-big/> · <https://www.pcgamer.com/how-game-sizes-got-so-huge-and-why-theyll-get-even-bigger/>
- Titanfall 48 GB / 35 GB uncompressed audio (Respawn lead engineer Richard Baker, via Eurogamer; multiply corroborated): <https://www.pcgamer.com/titanfall-install-audio/> · <https://www.engadget.com/2014-03-11-titanfall-pcs-48gb-install-the-result-of-uncompressed-audio/> · <https://www.escapistmagazine.com/titanfall-dev-explains-the-games-35-gb-of-uncompressed-audio/>
- ⚠ **Weak source, do not cite as measured** (see §2 warning): <https://dredyson.com/lessons-from-what-is-considered-a-good-game-size-data-for-aaa-game-development-and-performance-the-complete-definitive-proven-tested-guide-on-optimizing-unreal-engine-and-unity-engine-footprint-redu/>
