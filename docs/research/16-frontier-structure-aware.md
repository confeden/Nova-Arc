# 16 — Frontier: structure-aware and learned transforms

**Question.** What does the 2026 frontier of "teach the compressor what the data
IS" offer Nova Arc that we are not using? Which parts ship, which parts are
research demos?

**Method.** Primary sources (arXiv / USENIX / ACM / IEEE with dates), the Large
Text Compression Benchmark (last updated 2026‑07‑08), real repositories with
licenses, plus **new measurements run for this report** on the owner's machine
(8 logical cores, Windows 11) against `test/Silesia-compression-corpus/raw`.
Scripts live in `test/struct-exp/`.

Everything labelled **MEASURED (this report)** was run on 2026‑08‑17. Numbers
labelled **MEASURED (earlier session)** come from `test/dict-*.log` and
`test/alpha.log`, which already exist in the playground and were not re-run.

---

## 0. Bottom line, ranked by expected gain for narc

> **[REVIEW 2026-08-17] Three of the four `ship-now` rows did not survive
> verification.** Every filter number in this report was measured against
> **LZMA2 alone**, but narc's max tier runs a *tournament* (LZMA2 vs PPMd7
> order 10 vs order 16) and keeps the smallest. Re-measured through the real
> tournament in narc's own codec settings (`test/skeptic/`), **PPMd7 wins both
> files the transpose filter was supposed to help, by 13–14 %**, and the
> transpose makes the tournament result *worse*. See §1.2b. The dictionary row
> is not new work and is superseded by research 15 §1.4b, which measured the
> real `preset_dict` API rather than a proxy. Corrected rows below.

| # | Idea | Expected gain in narc | Effort | Verdict |
|---|------|----------------------|--------|---------|
| 1 | Transpose (byte-shuffle) filter + make the existing `Delta` filter selectable, chosen by a gated two-stage trial | ~~−1.1 % of total archive~~ → **REFUTED: −0.006 % with a correct selector, +0.6 % with the selector this report specifies** (§1.2b) | S | ~~ship-now~~ → **reject** (transpose); **prototype** (delta only, and only if the selector trials all three codecs) |
| 2 | In-archive shared preset dictionary for LZMA2 (`lzma-rust2` already supports it; narc passes `None`) | ~~−5.3 % on 32 MiB solid blocks~~ → **gross −5.25 %, net +0.51 % once the dictionary's own 351 KB is charged** (§6.2b) | M | ~~ship-now~~ → **reject** as specified; the in-archive *unit-as-dictionary* form is research 15's, already measured |
| 3 | Two-stage filter/codec selector: cheap order-1-entropy shortlist → trial-compress the shortlist | ~~0 regret on 11/11~~ → **+5.26 % regret on `x-ray`** as specified; 0 regret only if the trial runs the **whole tournament** (§4.2b) | S | ~~ship-now~~ → **prototype** |
| 4 | ARM64 BCJ filter (we only have x86) | +4.4 % measured on a linked ARM64 binary (external); ~0 on `.o`/`.a`/`.ko` | **XS — already implemented in `lzma-rust2` 0.19, no porting** | **ship-now** (the one survivor) |
| 5 | base64/hex representation undo | −25 % on base64-wrapped binaries (PEM, MIME, data-URIs, `.ipynb`) | M | **prototype** |
| 6 | `preflate-rs` for deflate streams (Apache-2.0, `forbid(unsafe_code)`) | large, but this is research 02's lane | M | **prototype** |
| 7 | OpenZL as an optional codec / source of transform ideas | 20–55 % on structured numeric data — but pre-1.0 wire format | L | **watch** |
| 8 | Per-field typed splitting (SDDL-class) for named formats | up to 2.06× vs 1.64× for LZMA on `sao` | XL | **watch** |
| 9 | Similarity-based file ordering inside solid blocks | unknown; published evidence says path order already wins — **[REVIEW] but the mechanism is proven at the top of the field: article reordering is the whole contribution of a Hutter Prize winner (§8b.4)** | S | **prototype** (now the strongest surviving item in this report) |
| 10 | Sub-chunk delta compression (Odess / Palantir / Argus) | real in backup systems; conflicts with our invariants | L | **watch** |
| 11 | Learned (XGBoost-class) transform selection instead of trial compression | ~0 over #3, and it can be actively wrong | M | **reject** |
| 12 | Grammar compression (GLZA / Re-Pair / Sequitur) | −18 % vs xz **only with a 1 GB window**; erased by our 16 MiB units | L | **reject** |
| 13 | CDC advances since FastCDC (RapidCDC / QuickCDC / SeqCDC / VectorCDC) | 0 ratio; chunking is not our bottleneck | M | **reject** |
| 14 | EXI / structural XML-JSON / WOFF2 / SQLite-VACUUM transcoding | not byte-exact ⇒ unusable | — | **reject** |
| 15 | External reference corpus the decompressor also holds | breaks self-containment for a win #2 already gets | M | **reject** |
| 16 | Alphabet packing, MTF, frequency remap, RLE pre-filters | 0–8 % on synthetic only, ~0 on real data | S | **reject** (already disproven locally) |

---

## 1. Automatic structure detection in binary data

### 1.1 OpenZL — the actual state of the art, and its actual limits

Meta open-sourced **OpenZL** on 2025‑10‑06 (paper arXiv:2510.03203 v1
2025‑10‑03, v2 2025‑10‑30; authors Yann Collet, Nick Terrell, W. Felix Handte
et al.). It is the single most important development in this topic area.

The idea: compression as a **directed acyclic graph of modular codecs**. The
input is parsed into typed streams (integers, floats, strings, fixed-width
fields), each stream gets its own codec chain (delta, transpose, bitpack,
tokenize, then an entropy back-end), and the **whole graph is serialised into
the compressed frame** so that one universal decoder can replay any
configuration. An offline **trainer** does a budgeted search over transform
choices and field clustering to produce a "Plan"; trained plans are tiny
(mostly < 15 KB, worst case 183 KB for census data with column clustering).

Measured, from the LWN write-up (2026‑01‑14) of the Open Source Summit Japan
2025 talk, on the SAO star catalog (7.2 MB — the same `sao` file that is in
Silesia):

| Compressor | Size | Ratio | Comp | Decomp |
|---|---|---|---|---|
| zstd ‑3 | 5.5 MB | 1.31× | 100 MB/s | 750 MB/s |
| LZMA ‑9 | 4.4 MB | 1.64× | 2.9 MB/s | 45 MB/s |
| **OpenZL, format-specific** | **3.5 MB** | **2.06×** | **215 MB/s** | **800 MB/s** |

That is 20 % smaller than LZMA ‑9 while compressing ~74× faster. The paper's
Table 3 reports the same shape elsewhere: on PPMF Person, 55 % better ratio
than xz ‑9 at 11× the compression speed; across their datasets OpenZL exceeds
xz ratios while compressing roughly 10× faster.

**The honest limits, stated by the authors themselves:**

- On **enwik7** (English Wikipedia text) OpenZL only matches **zstd ‑6**, and
  the specialised text filter **xwrt beats OpenZL by nearly 60 %**. Their own
  words: the standard codec library is not currently optimised for human text.
- The **wire format is not stable.** There is no 1.0 release because "the final
  wire protocol needs to be built with the community"; the README warns that
  the API, the compressed format, and the codec/graph set are all subject to
  change. Release-tagged versions promise decompressability "for at least
  several years"; the `dev` branch promises nothing.
- Development cost is the whole point of the paper's motivation section: the
  talk cites ~18 months to develop a format-specific algorithm the old way,
  and OpenZL's contribution is cutting that to days *for someone who already
  knows the format*.

License: BSD-3. Language: C11/C++17. Rust bindings exist but are embryonic:
`openzl-sys` 0.1.2+openzl.0.1.0 (LDeakin, ~858 downloads) and the higher-level
`rust-openzl` (vitorpy).

**Verdict for narc: watch, do not embed.** Putting a pre-1.0, explicitly
unstable wire format inside `.narc` would violate the one thing an archiver
must guarantee — that an archive written today opens in ten years. What we
*should* take from OpenZL is its transform library, which is where §1.2 comes
from.

### 1.2 The cheap subset: transpose and stride-delta — MEASURED

OpenZL's heavy machinery is out of reach, but its two cheapest transforms
(transpose = byte-shuffle, and delta at a detected stride) are ~30 lines of
Rust each and fit narc's existing per-chunk filter byte exactly.

I swept both over Silesia's structured files, 4 MiB slices, `zstd ‑19` and
`LZMA2 preset 6` (`test/struct-exp/stride.py`). Percentages are change in
compressed size vs no filter — negative is better.

**MEASURED (this report), best filter per file:**

| File | codec | best delta | best transpose |
|---|---|---|---|
| `x-ray` (16-bit medical) | zstd‑19 | s=1 −6.78 % | **s=2 −12.51 %** |
| `x-ray` | LZMA2‑p6 | s=2 −5.79 % | **s=2 −9.39 %** |
| `mr` (16-bit medical) | zstd‑19 | s=2 −8.17 % | **s=2 −8.43 %** |
| `mr` | LZMA2‑p6 | s=2 −5.10 % | **s=2 −5.14 %** |
| `sao` (28-byte records) | zstd‑19 | s=28 +3.20 % | **s=28 −6.13 %** |
| `sao` | LZMA2‑p6 | s=28 +8.55 % | s=28 **+4.08 %** |
| `osdb` (DB records) | both | s=1 +11.5 / +17.7 % | none |
| `nci` (chemical text) | both | s=1 +17.3 / +17.5 % | none |

Whole-file confirmation at `LZMA2 preset 9`:

| File | raw | LZMA2‑p9 | best filter | result | delta |
|---|---|---|---|---|---|
| `x-ray` | 8 474 240 | 4 489 807 | transpose 2 | 4 082 675 | **−9.07 %** |
| `mr` | 9 970 564 | 2 750 211 | transpose 2 | 2 607 063 | **−5.20 %** |
| `sao` | 7 251 944 | 4 415 011 | none | 4 415 011 | 0 |
| `osdb` | 10 085 684 | 2 849 848 | none | 2 849 848 | 0 |

**Effect on a real narc archive: −0.52 MiB on the 47 MiB Silesia archive =
−1.12 % of total size, for a selector costing ~11 ms/MiB.** That is a bigger
ratio win than most of the codec tuning left on the table, and it is nearly
free.

> **[REVIEW] This paragraph is wrong, and §1.2b shows why: the archive does not
> compress `x-ray` and `mr` with LZMA2.** Everything above it reproduces
> correctly; the inference from "LZMA2 gets smaller" to "the archive gets
> smaller" does not hold.

### 1.2b [REVIEW] MEASURED through narc's actual tournament — the gain is not there

The measurements above use LZMA2 as the back end. narc's max tier does not: for
every unit it runs `Tier::candidates()` = **LZMA2 + PPMd7 order 10 + PPMd7
order 16** and keeps the smallest (`analyze.rs:73-84`). Nothing in this report
checked which one wins the files it recommends filtering.

Re-measured with narc's exact settings — LZMA2 preset 6 with `nice_len 273` and
`dict = unit len` (`codec.rs:118-136`), PPMd7 orders 10/16 with pool `32 ×
len` capped at 256 MiB (`codec.rs:190`) — on 2026‑08‑17, code in
`test/skeptic/`:

**Whole file as one unit:**

| File | LZMA2 alone | PPMd7 o10 wins at | tournament baseline | + transpose 2 | + delta 2 |
|---|---|---|---|---|---|
| `x-ray` | 4 483 867 | **3 850 778** (−14.1 % vs LZMA2) | 3 850 778 | 4 053 512 **+5.26 %** | 3 999 427 **+3.86 %** |
| `mr` | 2 751 293 | **2 317 181** (−15.8 % vs LZMA2) | 2 317 181 | 2 409 835 **+4.00 %** | 2 314 138 **−0.13 %** |

**First 4 MiB (narc's average max-tier chunk), same conclusion:**

| File | tournament baseline (winner) | + transpose | + delta |
|---|---|---|---|
| `x-ray` | 1 923 055 (ppmd16; LZMA2 2 219 032) | +4.31 % | +2.99 % |
| `mr` | 926 822 (ppmd10; LZMA2 1 093 069) | +3.23 % | −0.38 % |
| `sao` | 2 562 214 (**lzma2**; ppmd10 2 765 468) | +4.67 % (t28) | +7.43 % (d28) |
| `osdb` | 1 007 192 (ppmd10; LZMA2 1 202 231) | +21.56 % (t2) | +19.50 % (d1) |
| `ooffice` | 1 783 888 (ppmd16; LZMA2 1 814 076) | +26.14 % (t2) | +24.38 % (d1) |

**Three conclusions:**

1. **PPMd7 already extracts the 16-bit structure, and better than LZMA2 +
   transpose does.** A byte transpose is a crude way of telling an LZ matcher
   "the sample interval is 2"; PPMd's context model learns that from the data,
   and destroying byte adjacency to help LZ costs PPMd more than LZ gains.
2. **Applying this report's plan as written makes the Silesia archive bigger.**
   `x-ray` +202 734 B, `mr` +92 654 B ⇒ **+0.28 MiB ≈ +0.6 %**, the opposite
   sign to the headline. The claimed −0.52 MiB is a gain against a codec the
   archive does not use on those files.
3. **A perfect oracle over `{none, delta, transpose}` scored through the whole
   tournament is worth −3 043 B on the entire 47 MiB archive (−0.006 %).** The
   only positive cell left is `delta 2` on `mr`, and it is 0.13 % — noise.

The transpose filter is therefore **reject for narc as it stands**. It becomes
interesting again only in a configuration where LZMA2 is the sole back end
(the fast/normal tiers, where filters are not currently selected at all and the
codec is zstd), or if a future tier drops PPMd7.

**Three findings that matter more than the headline number:**

1. **A blind stride sweep is dangerous, not merely useless.** Wrong strides
   cost up to **+174 %** (transpose 32 on `osdb`) and **+60 %** (delta 12 on
   `sao`). A filter must never be applied on a guess.
2. **`sao` is the counter-example to naive transposition.** Its true record
   width is 28 bytes and the detector finds it (sharp minima at 28 and 56), but
   whole-record transposition *helps zstd (−6.1 %) and hurts LZMA2 (+4.1 %)*.
   LZMA already exploits 28-byte periodicity through its rep-distance model;
   transposing destroys the whole-record matches it was using. So the 2.06×
   OpenZL gets on `sao` is **not** reachable with a transpose filter — it needs
   per-field typed dispatch (§1.5, §8 of the verdict table).
3. **The win is confined to fixed-width numeric arrays.** Nine of eleven
   Silesia files want no filter at all. Executables (`mozilla`, `ooffice`),
   text (`dickens`, `webster`), markup (`xml`), tarballs (`samba`), record text
   (`nci`) and database dumps (`osdb`) all reject both transforms.

**Implementation note.** narc's `Filter` enum already has `Delta(1..=32)` with
ids 2..=33, round-trip tests, and format documentation — **but
`analyze::plan()` never returns it.** Grep confirms the only filters the
analyzer can emit are `None` and `BcjX86`. So the delta filter is currently
dead weight in the format. Filter ids 34..=255 are unassigned and `from_id`
rejects them (fails loudly), so `Transpose(2..=32)` can take ids 34..=64 with
no compatibility risk. Because the chunk hash covers the *original* bytes, both
dedup and integrity are filter-independent — the invariant already documented
in ROADMAP holds unchanged.

### 1.3 Stride detection: what works, what does not

Published algorithms for inferring record width:

- **Autocorrelation via Wiener–Khinchin** (inverse FFT of the power spectrum,
  O(n log n)) — the classic approach; used as an explicit stride detector for
  instruction-width inference in arXiv:2410.21558 (2024‑10) via Pearson
  correlation between a byte series and its lag-k copy.
- **Separator discovery** for variable-length records (US 8,051,060) — infers
  unknown record and field delimiters so the data can be reorganised from
  record-major into field-major order. Relevant to CSV/log-shaped data, not to
  fixed-width binaries.
- **Cheap classifier features** — the AIT‑2026 challenge's `G2‑V3` entry
  classifies from at most the first 64 kB using zero-order entropy, alphabet
  size, printable-ASCII ratio, **even/odd byte imbalance**, and float32
  exponent-byte patterns. The even/odd imbalance feature is a cheap stride-2
  proxy, which is exactly the case that pays in §1.2.

**MEASURED (this report) — order-0 entropy cannot score a transpose.** My first
detector used `H0` and picked `s=1` (no-op) for transpose on every file: byte
transposition is a *permutation*, so the byte histogram — and therefore `H0` —
is invariant under it. This is obvious in hindsight and is the kind of thing a
plausible-looking implementation gets wrong silently.

Switching to **order-1 conditional entropy** `H(x_i | x_{i-1})` fixes it: it is
permutation-sensitive, costs one 256×256 bincount, and correctly surfaced
`transp2` for `x-ray` and `mr` and `transp28`/`delta28` for `sao`. But `H1`
alone is still not sufficient — on `nci` it picked `transp3`, which costs
+35.5 %. **The entropy features are good enough to shortlist, never to
decide.** See §4.2 for the selector that works.

### 1.4 Float-specific compressors

| Tool | What it does | Measured |
|---|---|---|
| **SPDP** (Claggett/Azimi/Burtscher, DCC 2018) | auto-synthesised from 9.4 M component combos; ends up as difference coding + byte shuffle + fast LZ77 | On smooth double-precision fields, ratios land 1.01–1.11× — even the best (fpzip) only reduced 11 % |
| **fpzip / zfp / FPC** | value prediction + residual entropy coding | Lossless FPZIP/FPC generally cap at ~2:1 because trailing mantissa bits are essentially random |
| **FCBench** (VLDB 2024, vol. 17 p. 1418) | 13 lossless FP compressors × 33 datasets | Recommends **`bitshuffle::zstd` and MPC as the general default**, fpzip for HPC, `nvCOMP::LZ4` for time series, Chimp for DB data. bitshuffle methods rank top overall for robustness |
| **TDT** (arXiv:2506.18062, 2025‑06‑22) | typed data transformation, groups related bytes | geomean **1.16×** ratio over zstd, 1.18–3.79× throughput |
| **Blosc bitshuffle** | bit-level rather than byte-level shuffle | 1.4–2.4× ratio over plain shuffle on numeric data at 2–4× filter cost; AVX‑512 in c-blosc2 2.11 recovered ~20 % of the speed |

**Reading for narc.** The literature's general-purpose recommendation is
literally "bitshuffle then zstd" — i.e. §1.2's transpose filter, one level
finer. Two cautions: bitshuffle's advantage over byte shuffle shrinks or
reverses at high zstd levels (Blosc's own guidance says test bytedelta or plain
shuffle for ZSTD at high clevels), and narc's max tier runs LZMA2/PPMd7 where
§1.2 already shows transposition sometimes flips sign. So: **byte transpose
first, bitshuffle only as a second candidate in the same tournament.**

Scientific float data is also a small share of a desktop archive. The reason to
build this is not HPC — it is that the same filter catches 16-bit audio, 16-bit
imagery, vertex buffers, sample tables and heightmaps, which do show up.

### 1.5 Columnar encodings applied to arbitrary files

**BtrBlocks** (SIGMOD 2023, Kuschewski/Sauerwein/Alhomssi/Leis) is the clearest
statement of the columnar position: they **refuse to use general-purpose
compressors** because "general-purpose schemes layered on top of simple
encodings are quite inefficient to decompress", and instead cascade lightweight
type-specific encodings recursively, with scheme selection by sampling — for a
64 000-value chunk they sample **10 runs of 64 values** from non-overlapping
random positions.

Downstream evidence (Spiral's Vortex, which implements BtrBlocks-style
cascading): on TPC-H SF10, files are **38 % smaller and decompress 10–25×
faster than Parquet+ZSTD**, using no general-purpose codec at all.

Companion pieces: **FSST** (VLDB 2020) for short strings, **ALP** (SIGMOD 2024)
for floats, **PIDS** for mining sub-attributes out of string columns. The
honest caveat from the FSST literature itself: FSST-style compressors *lose on
pure ratio* to block compressors, because block compressors exploit redundancy
across larger byte ranges; they trade ratio for random access.

There is also a Rust proof-of-concept in exactly narc's shape:
**DataCortex** — schema inference + columnar reorganisation + typed encoding for
JSON/NDJSON, claiming 2–3× over zstd on structured data and byte-exact
round-trips, citing ALP/FSST/BtrBlocks/CLP. Unvetted, but it is direct evidence
the approach is implementable in Rust.

**Verdict.** The random-access motivation does not apply to us — narc decodes a
whole 4–32 MiB unit anyway. So the columnar literature's ratio wins mostly come
from the *encodings* (delta, transpose, bitpack, dictionary), which §1.2 covers,
not from the columnar layout per se. Full per-field splitting for arbitrary
files is the OpenZL/SDDL project: **watch**.

### 1.6 Kanzi — the shipping reference implementation of this whole idea

**Kanzi** (flanglet, Apache-2.0, Java + C++ + Go, no Rust port) is the best
existing model of "many small transforms, auto-selected per block" in a
general-purpose compressor. Its transform set is worth reading as a menu:

| Transform | What it does | Relevance to narc |
|---|---|---|
| **EXE** | relative→absolute jump addresses, **x86 *and* ARM64** | We only have x86. Direct gap. |
| **FSD** ("MM") | "decorrelate values separated by a constant distance (step) and encode residuals" — fixed-step delta with detected stride | Exactly §1.2's delta, already shipping elsewhere |
| **TEXT** | dictionary replacing common words with an index | WRT-class; see §3.3 |
| **UTF** | replaces UTF-8 codewords with frequency-based aliases | Untested here |
| **PACK** | replaces unused symbols with frequency-based aliases | Disproven locally — §1.7 |
| **DNA** | PACK, gated on DNA detection | Niche |
| **LZP / RLT / ZRLT / SRT / RANK / MTFT** | BWT-adjacent stages | Not our architecture |

Level presets chain them, e.g. level 6 = `TEXT+UTF+BWT+SRT+ZRLT&FPAQ`. Useful
calibration on **enwik8**: Kanzi ‑l5 = 23 293 550, ‑l6 = 21 988 529 — around
11 % below `xz ‑9e` territory. Two cautions from Kanzi's own docs: the
**bitstream format is not finalised**, and the levels are "calibrated to
improve compression monotonically … this is not guaranteed".

The three transforms worth copying are **EXE/ARM64**, **FSD**, and **TEXT**.
The rest are BWT-pipeline furniture.

### 1.7 Alphabet packing, MTF, remap, RLE — already disproven here

**MEASURED (earlier session, `test/alpha.log`).** These were tested on real and
synthetic data and all failed:

| Transform | zstd‑19 | lzma‑9e |
|---|---|---|
| 2-bit packing, DNA-like 2 MiB | 99.6 % | 92.4 % |
| 4-bit packing, 16-value table | 99.6 % | 96.1 % |
| frequency remap, 4 MiB source text | 100.0 % | 100.8 % |
| RLE, 4 MiB source text | 99.2 % | 98.9 % |
| **MTF, 512 KiB source text** | **301.7 %** | **296.1 %** |
| frequency remap, synthetic log | 101.8 % | 97.6 % |

The one real win in that log is **representation undo**: base64-encoded binary
at 1 398 104 bytes compresses to 334 266 with lzma‑9e as-is, but decoding it
back to the 1 048 576-byte payload gives **249 057 — a 25.5 % saving**. Hex is
worth ~0 (104.0 % / 100.7 %), because both codecs already model the nibble
structure.

**Verdict: reject packing/MTF/remap/RLE. Prototype base64 undo.** base64 is
everywhere in real corpora (PEM certificates and keys, MIME email, `data:`
URIs inside JS/CSS bundles, embedded images in `.ipynb` and SVG). Reversibility
is the hard part, not the transform: line-length conventions, padding, the
`+/` vs `-_` alphabets, and interior whitespace all have to be captured as
metadata, and the filter must bail out to raw on anything it cannot reproduce
byte-for-byte.

---

## 2. Grammar-based compression: Re-Pair, Sequitur, GLZA

### 2.1 The numbers

Large Text Compression Benchmark, enwik9 (1 GB), page last updated
**2026‑07‑08**:

| Program | Options | enwik9 | comp ns/B | decomp ns/B | mem MB |
|---|---|---|---|---|---|
| zpaq 6.42 | `-m s10.0.5fmax6` | 142 252 605 | 6 699 | 14 739 | 14 000 |
| nanozip 0.09a | | 148 545 179 | 1 149 | 1 141 | 32 000 |
| **xwrt 3.2** | `-l14 -b255 -m96 …` | **151 171 364** | 2 537 | 2 328 | 1 691 |
| ppmonstr J | `-m1700 -o16` | 157 007 383 | 3 574 | ~3 600 | 1 700 |
| **GLZA 0.12** | `c -x -o0.6 -p4 -r16000` | **161 678 356** | **11 771** | **9.4** | **15 331** |
| **bsc 3.25** | `-b1000 -e2` | **163 884 462** | **23** | **8** | 5 000 |
| 7zip 4.46a | `-m0=ppmd:mem=1630m:o=10` | 178 965 454 | 503 | 546 | 1 630 |
| ppmd J1 | | 183 964 915 | 880 | 895 | 256 |
| xz 5.2.1 | `--lzma2=preset=9e,dict=1GiB,lc=4,pb=0` | 197 331 816 | 5 876 | 20 | 6 000 |
| zstd 0.6.0 | `-22 --ultra` | 215 674 670 | 701 | 2.2 | 792 |
| brotli | `-q 11 -w 24` | 223 597 884 | 3 400 | 5.9 | 437 |

GLZA's headline claim (Conrad & Wilson, DCC 2016, "Grammatical Ziv-Lempel
Compression: Achieving PPM-Class Text Compression Ratios With LZ-Class
Decompression Speed") holds up: **18.1 % smaller than xz** with **decompression
at 9.4 ns/byte ≈ 106 MB/s, 2× faster than xz's 20 ns/byte**. That is a genuinely
attractive asymmetric point for an archiver, which decompresses many times and
compresses once.

### 2.2 Why it is still a reject for narc

Three independent killers:

1. **Compression speed: 11 771 ns/byte ≈ 85 KB/s.** enwik9 takes ~3.3 hours.
   narc's whole max tier does Silesia in 6.8 s. Even accepting a slow max tier,
   85 KB/s is two to three orders of magnitude off.
2. **Memory: 15 331 MB for a 1 GB input (~15× input).** Re-Pair's classic
   figure is ~5× input for linear time, and one comparison measured a real
   implementation at ~30×n bytes; the best practical low-space variant (Bille,
   Gørtz & Prezza, arXiv:1704.08558) gets to about (1.5+ε)n *words* including
   the text. Even at 5× input, narc's 16 MiB unit needs 80 MB per worker.
3. **The gain comes from the window, and our architecture removes the window.**
   The DCC 2016 paper deliberately benchmarks against a *large-window* LZMA
   variant so that memory limits do not distort the enwik9 comparison — the
   whole point is that GLZA exploits redundancy across the entire 1 GB. narc's
   compression unit is capped at 16 MiB (32 MiB solid blocks). This is the
   **identical mechanism** that already killed LZMA2 as our universal text
   codec (ROADMAP negative knowledge: "its edge comes from >4 MiB dictionaries,
   which chunking removes"). There is no reason to expect grammar compression
   to behave differently, and every reason to expect it to behave worse, since
   grammar inference needs *more* data than LZ to find profitable productions.

The nail in the coffin is one row down the same table: **libbsc 3.25 (BWT) gets
163 884 462 — 1.4 % larger than GLZA — at 23 ns/byte compression (≈43 MB/s),
512× faster than GLZA, and 8 ns/byte decompression.** Whatever GLZA is buying,
a BWT gets ~99 % of it for 1/500th of the compression cost.

There is also no Rust implementation of Re-Pair or GLZA, and GLZA's practical
tuning is counter-intuitive (running Re-Pair to completion can *worsen* the
ratio; limiting dictionary size compresses larger files better — contrary to
Larsson and Moffat's original expectation).

**Why it never shipped widely:** slow-and-huge to compress, no format standard,
a single-author codebase (last releases mirrored by third parties at
`terrelln/GLZA` and `jrmuizel/GLZA`), and — decisively — BWT and CM occupy the
same ratio band at vastly better compression cost.

**Side note worth a separate look (not this topic).** libbsc's row above is the
interesting accident of this table: **163.9 MB vs ppmd J1's 184.0 MB — 11 %
better than PPMd var J at 43 MB/s.** narc currently routes text to PPMd7. This
is a 1 GB-block number and will not transfer directly to 32 MiB units (BWT's
advantage also grows with block size), so it is unverified for us — but it
belongs in research 01's queue.

---

## 3. Semantic / format-specific transcoding beyond images

The governing constraint: **an archiver's transform must be exactly reversible,
bit for bit.** That single rule eliminates most of this section.

### 3.1 Fonts: WOFF2 as a cautionary case study

WOFF2 (W3C spec) applies a `glyf`/`loca` transform plus `hmtx` transform to
expose redundancy, then Brotli-compresses the whole font. Gains: **~30 % average
over WOFF 1.0**, up to 50 %+; ~25–35 % on CJK vs ~60 % on Latin. The spec also
quantifies CFF de-subroutinisation at 5–10 % smaller compressed output while
inflating the uncompressed size ~10 %.

**But a WOFF2 round-trip is not byte-exact** — the glyph transform normalises
padding and table ordering when reconstructing `glyf`, so the output is
functionally identical and a few bytes different. WOFF 1.0 round-trips exactly;
WOFF2 does not.

For narc this means: usable only if we also store the housekeeping diff
(padding bytes, original table order, checksum adjustments). That is doable but
it is a font-format-specific project for a file type that is a rounding error
in a general archive. **Reject on priority, not on principle.**

### 3.2 PDF object streams and deflate

PDF's `FlateDecode` streams are just deflate, so this collapses into the
recompression problem that research 02 owns. The 2026 state of the art in Rust
is new information worth recording here:

**`microsoft/preflate-rs`** — Apache-2.0, `#![forbid(unsafe_code)]` in every
crate, "used in production cloud storage systems", 189 commits. It splits a
deflate stream into uncompressed data plus reconstruction information, detecting
zlib, MiniZ and libdeflate encoder fingerprints. Correction overhead as a
percentage of uncompressed data:

| Encoder | overhead |
|---|---|
| zlib | 0.01–0.08 % |
| zlib-ng | 0.01–1.07 % |
| libdeflate | 0.25–1.51 % |
| miniz_oxide | 0.01–2.70 % |

Crucially: "unrecognized compressors still round-trip correctly — the
corrections overhead is simply higher." Note its format is **incompatible** with
the original C++ preflate (different arithmetic coder, shared with Lepton).

Precomp's historical PDF figure for context: PDFs typically shrank to 25–50 % of
original size, with bit-exact restoration guaranteed (unlike a
`pdftk uncompress`/recompress round-trip, which cannot be restored losslessly).

**Verdict: prototype, and hand it to research 02.** The licensing and
`forbid(unsafe_code)` alignment with `narc-core` is unusually good.

### 3.3 XML / JSON: EXI, CBOR, and structural compressors

**EXI** (W3C, Efficient XML Interchange) measurements are strong — the W3C
Working Group Note of 2009‑04‑07 reports EXI consistently smaller than gzipped
XML regardless of document size or structure, "in some cases over 10 times
smaller", and 5.4× faster average encoding than gzipped XML. **But EXI is a
schema-aware re-serialisation, not a byte-exact transform**: whitespace,
attribute order, comments, DTD internals and entity usage are preserved only
under specific `preserve` options, and the headline numbers come from per-case
tuning (schema on when available, compression option toggled per case). For an
archiver this is disqualifying.

The structural-JSON family (JSON BinPack, DataCortex, PIDS, JSON Tiles/JSONB)
has the same problem in a different costume: the wins come from discarding
key order, whitespace and number formatting.

Where this *does* apply is `narc`-external: **CLP** (OSDI 2021) reaches an
average **32:1 on text logs vs 16:1 for gzip** (43:1 on Hadoop logs), and
**LogGrep** (EuroSys 2023) is a further **2.14× average over CLP** at **0.10×
gzip's compression speed**. **LogShrink** (ICSE 2024) reports 4.57× average over
gzip and 1.16–5.54× over lzma. These are searchable-store designs, and their
ratios are not byte-exact-archiver ratios; also a documented instability —
LogShrink's ratio on HealthApp swings from 13 to 65, and 13 is *worse* than a
general-purpose compressor.

**Verdict: reject for `.narc`.** The only byte-exact-safe idea in this family is
"split a text file into streams at detected delimiters", which is a much weaker
transform than any of the above and is well covered by §1.2's measurement that
`nci` and `xml` want no filter.

Note the one text transform that *does* pay: **XWRT** (Skibiński,
*Effective asymmetric XML compression*, Softw. Pract. Exper. 2008) improves
general-purpose compressors **~35 % for gzip and ~17 % for LZMA** on XML
corpora, and is the tool that beats OpenZL by ~60 % on enwik7. On plain English
a 2026 reimplementation measured a much smaller effect on enwik8 — LZMA 26.38 %
→ 25.51 % of input (+0.87 pp ≈ 3.3 % relative), zstd essentially unchanged
(−0.08 pp) — and the authors note their reimplementation omits capitalisation
modelling, number encoding and multi-type boundary markers, so real XWRT does
better. **XWRT-class word replacement is a plausible prototype for our text
path, with the caveat that PPMd7 (our text codec) already models words, so the
17 % LZMA figure will not transfer.** That belongs in research 01, not here.

### 3.4 SQLite, wasm, Java/.NET bytecode, Docker layers

- **SQLite.** The best static-archive advice in the literature is "`VACUUM`, then
  LZMA the file", which beats any page-aware scheme on ratio. `VACUUM` is **not
  byte-preserving** — it rewrites the file — so an archiver cannot do it. Page-
  level (`sqlite_zstd_vfs`, ~40 % on a 1 MB Chinook DB) and row-level
  (`sqlite-zstd`, up to 80 % smaller) approaches exist but both *change the
  file*. **Reject.** A `.db` file is just a generic binary to us; free pages are
  zeros and compress fine.
- **wasm.** No genuine wasm-aware lossless transforms exist in the literature.
  The measured wins are (a) use Brotli instead of gzip (12.4 MB wasm: 4.8 MB
  gzip → 3.5 MB Brotli, −27 %) and (b) strip custom/debug sections, which is
  *lossy* and whose post-compression benefit is much smaller than raw section
  sizes suggest, since the name section is highly compressible ASCII anyway.
  **Reject.**
- **pack200 — the definitive negative case study.** Removed from the JDK in 14
  (JEP 367, submitted 2019‑10‑08, resolved 2019‑12‑18). Read the motivation
  carefully, because it is the exact failure mode a format-specific transcoder
  in an archiver would hit: the file format was *tightly coupled to the class
  and JAR formats*, both of which "evolved in ways unforeseen by JSR 200" (JEP
  309's new constant-pool entry kind, JEP 238's multi-release JAR metadata); the
  implementation was split between Java and native code and hard to maintain;
  and the API obstructed platform modularisation. **The JEPs never argue the
  ratio was bad — they argue the maintenance cost outweighed it.** Any per-
  format transcoder we ship signs up for this bill forever. **Reject.**
- **Docker layers.** The research (DupHunter, USENIX ATC 2020; ACM ToS 2024;
  MiDedup; BED) is about registry-side dedup, and its central finding is a
  warning for us: **gzip-compressed files have very low deduplication ratios**,
  so dedup must operate on decompressed content — and naive fine-grained dedup
  causes up to **8× restore I/O slowdowns**. narc already stores `.tar.gz`
  layers as opaque blobs; making layers dedup-able means decompressing them,
  which is research 02's recompression pipeline again. **Reject as a separate
  idea; it is a downstream benefit of gzip recompression.**

---

## 4. Learned / AI-assisted transform selection

This is narc's `analyze.rs` question: can a model beat magic-bytes + trial
compression?

### 4.1 What the literature actually says

**MLcomp** (Burtscher's group, DCC 2024, "Using Machine Learning to Predict
Effective Compression Algorithms for Heterogeneous Datasets") is the strongest
result — and it is quietly deflationary. They evaluate ~9 000 files against a
library of >100 000 synthesised pipelines (generated by CRUSHER) and reach
**97.8 % of the best-possible average compression ratio**, versus **77 %** for
the best single algorithm applied to everything. But the model is trained using
**nothing but the compression ratios of a few algorithms as features**. That is
not a learned replacement for trial compression — **it is trial compression,
with a model interpolating from a small probe set to a large candidate set.**
The lesson is "probe cheaply, extrapolate", not "predict from statistics".

The **2026 Algorithmic Information Theory Data Compression Challenge**
(arXiv:2606.17712, 2026‑06‑16; 117 valid submissions, 16 heterogeneous files,
8 GB memory cap, 1 MB decompressor cap) contains the two purest examples of the
learned-selection idea:

- **`G1-V22`** uses "two XGBoost-based selectors to automate pipeline
  decisions" over a **38-dimensional statistical feature vector** (entropy
  measures, run statistics, byte-distribution properties, histogram bins,
  conditional entropies, LZ77-based features). Labels are obtained empirically
  by compressing corpus chunks with all candidate transforms and taking the
  smallest. Its heuristic modes explicitly trade speed for quality by *either*
  trusting predictions *or* empirically testing top-ranked combinations on
  samples — i.e. even the ML entry falls back to trial compression when it wants
  quality.
- **`G2-V3`** does classical detection: at most the first 64 kB, eight classes
  (protein, text, structured text, random, float32, generic binary,
  interleaved, ELF binary), then type-specific preprocessing.

Results: the top of the ratio leaderboard is `xEnc3` at 1.806 and `paq8px-1` at
1.793 — **neither is a classifier-driven entry**. `G1-V25` places 5th at 1.745.
On the external 250 M-symbol datasets, `G2-V3` did shine where its detector had
something to detect: **0.822 bits/byte on the XML bibliography set, beating
lzma‑9 and brotli‑9**. So *detection* pays on structured text; *learned
selection* did not win anything.

### 4.2 MEASURED (this report): the selector that actually works

I built and measured the two-stage design the literature implies, on 4 MiB
Silesia units, 16 filter candidates (`none`, delta{1,2,3,4,8,12,16,28},
transpose{2,3,4,8,12,16,28}), ground truth = `LZMA2 preset 6` on the full unit:

| Selector | regret vs oracle | cost per 4 MiB unit |
|---|---|---|
| `H0` entropy on 64 KiB sample | fails structurally (permutation-invariant) | ~5 ms |
| `H1` entropy on 64 KiB sample, argmin | up to **+42 %** (`nci` → `transp3`) | ~30 ms |
| **zstd‑1 on a 256 KiB sample, all 16 candidates** | 0 on 10/11; **+4.08 % on `sao`** | ~25 ms |
| **target codec (LZMA2‑p1) on a 256 KiB sample, all 16** | **0 on 11/11** | 135–743 ms |
| **`H1` shortlist (top 3) → LZMA2‑p1 trial on 64 KiB** | **0 on 11/11** | **~45 ms (11 ms/MiB)** |

The last row is the recommendation. Full output:

```
file      oracle        or% gated pick  gated%  regret  gate ms  trial ms shortlist
sao       none       +0.00% none       +0.00%  +0.00%      43       21  [none, transp28, delta28]
x-ray     transp2    -9.39% transp2    -9.39%  +0.00%      31       21  [none, transp2, transp4, transp8]
mr        transp2    -5.14% transp2    -5.14%  +0.00%      22       12  [none, transp2, delta1, transp4]
osdb      none       +0.00% none       +0.00%  +0.00%      29       16  [none, transp2, delta1]
nci       none       +0.00% none       +0.00%  +0.00%      31       25  [none, delta1, transp3]
ooffice   none       +0.00% none       +0.00%  +0.00%      36       17  [none, transp2, delta1]
mozilla   none       +0.00% none       +0.00%  +0.00%      35       12  [none, transp4, transp16]
dickens   none       +0.00% none       +0.00%  +0.00%      22       17  [none, delta1, transp2]
webster   none       +0.00% none       +0.00%  +0.00%      24       19  [none, delta1, transp2]
xml       none       +0.00% none       +0.00%  +0.00%      15        6  [none, delta1, transp2]
samba     none       +0.00% none       +0.00%  +0.00%      24       19  [none, transp2, delta1]
total selector cost for 11 x 4 MiB units: 497 ms (11 ms per MiB)
```

Four design rules fall out of this:

1. **`none` must always be in the shortlist.** It is the oracle's answer on 9 of
   11 files and the shortlist mechanism must never be able to drop it.
2. **Trial with the codec you will actually use.** The zstd‑1 proxy is 20× faster
   but disagrees with LZMA2 on `sao` — precisely because the filter's value
   depends on the back-end's own modelling of periodicity (§1.2 finding 2).
   Using `LZMA2 preset 1` as the probe for an `LZMA2 preset 9` unit costs 20 ms
   and eliminates the disagreement.
3. **Cheap statistics shortlist; they never decide.** `H1` alone would have cost
   +42 % on `nci`.
4. **This is measured in NumPy Python.** A Rust implementation of `H1`
   (one 256×256 bincount) and the transforms will be several times faster;
   11 ms/MiB → ~2–4 ms/MiB, i.e. under 1 s over Silesia against narc's current
   6.8 s. Gate it to the max tier if even that is too much.

### 4.2b [REVIEW] The "0 regret" claim, corrected

Rule 2 above — "trial with the codec you will actually use" — is right in
principle and **wrong in its instantiation**: at the max tier there is no
singular "the codec". The selector as specified probes `LZMA2 preset 1`, so on
`x-ray` it picks `transpose 2`, and the unit is then handed to a tournament that
would have chosen PPMd7 on the *unfiltered* bytes. Measured regret is
**+5.26 %** on that file (§1.2b), not 0.

The ground truth in the table above is `LZMA2 preset 6 on the full unit`. That
is the oracle *for LZMA2*, not the oracle for narc, so the whole row "0 regret
vs oracle on 11/11" measures agreement with the wrong reference.

A correct selector must score each candidate transform through **all three
back ends**, which multiplies the probe cost by 3 and adds PPMd7 — the slowest
probe — to the inner loop. On the corrected evidence the only thing it would
ever select is `none`, so the honest recommendation is: **do not build the
selector until a filter exists that beats the tournament baseline.**

Two smaller defects in the same table: the run covers **11 of Silesia's 12
files** (`reymont` is silently absent), and the selector was designed *and*
evaluated on the same files, with no hold-out — an n=11 in-sample result.

### 4.3 Cheap compressibility estimators generally

The patent and systems literature converges on the same shape: predict
compressibility from an entropy estimate over a sample and skip the codec when
the estimate says "incompressible" (Microsoft US2014/0244604A1 ties this to a
dedup system's chunker; US9710166/US9946464 describe hardware detectors using
hashed intervals and a hit counter, noting 11 of 16 bits suffice for accurate
prediction). For error-bounded lossy scientific compression there is a real
black-box estimator (SVD truncation + quantised entropy in a linear model),
robust across compressors.

narc already does the useful version of this: `analyze::compresses()` runs
`zstd level 1` on a 64 KiB sample and stores raw below 3 % savings. **That is
the right design and the literature does not offer better.** The refinement
worth making is not a better estimator — it is extending the *same* probe to
also rank filters (§4.2).

**NCD** (normalized compression distance) appears repeatedly in search results
and is a red herring here: it is a similarity metric built *from* compressed
sizes, useful for clustering (§6.3), not for predicting a codec. A NeurIPS 2024
workshop paper is explicitly titled around "the disconnect between compression
and classification".

---

## 5. Content-defined chunking since FastCDC, and sub-chunk dedup

### 5.1 The independent benchmark says: stop optimising the chunker

The decisive source is **"A Thorough Investigation of Content-Defined
Chunking"** (Gregoriadis, Balduf, Scheuermann, Pouwelse; arXiv:2409.06066,
2024‑09‑09) — the only study benchmarking the whole family under one harness on
four ~10 GiB realistic datasets (CODE 22.1 % entropy, WEB 72.8 %, PDF 87.5 %,
LNX 98.6 %) plus synthetic RAND.

Throughput (MiB/s, 2 KiB target, RAND):

| Algorithm | MiB/s |
|---|---|
| Gear64+ (SIMD) | 941 |
| RAM | 887 |
| BFBC-L | 876 |
| AE | 734 |
| Gear | 599 |
| Buzhash | 338 |
| Rabin | 202 |

Their conclusions: **"Gear with normalized chunking emerges as a robust and
efficient alternative"** (that is FastCDC), AE is the only algorithm competitive
across all metrics, classic sliding-window algorithms remain unbeaten on dedup
*efficacy*, and BFBC/BFBC* underperformed their published claims.

The rest of the family confirms the picture — all the recent work buys
**throughput**, not ratio:

| Work | Claim | Nature of the gain |
|---|---|---|
| FastCDC (ATC 2016 / TPDS 2020, doc 9055082) | 3–12× faster than SOTA CDC, "nearly the same or higher" dedup ratio than Rabin | speed; the 2020 version adds two-byte-per-iteration rolling for ~30–40 % more throughput and produces **identical boundaries** |
| RapidCDC (SoCC 2019), QuickCDC (2021) | skip past previously-seen chunks using duplicate locality | speed, **only on highly redundant streams**; ~0 on unique data |
| SeqCDC (Middleware 2024) | 1.5–3.1× chunking throughput, "similar space savings"; throughput *increases* with chunk size | speed |
| VectorCDC (FAST 2025) | 15× over unaccelerated, 1.2–1.35× over other vectorised CDC | speed |

**Verdict: reject, unambiguously.** narc's chunker is not the bottleneck by
three orders of magnitude. FastCDC runs at ~1–2 GB/s; our max-tier codecs run at
single-digit MB/s. Every published advance here trades implementation
complexity for throughput we do not need, and the independent survey says none
of them improves the dedup ratio over what we already have. This belongs in
ROADMAP's negative knowledge.

### 5.2 Sub-chunk granularity: post-dedup delta compression

This *is* where the ratio is, and it is a mature line of work:

| Method | Venue | Result |
|---|---|---|
| N-transform Super-Feature | classic | baseline; compute-intensive (Rabin rolling hash + N linear transforms) |
| **Finesse** | USENIX FAST 2019 | sub-chunk features grouped into super-features; high throughput, **lower accuracy and ratio** |
| **Odess** | ACM ToS 2023 (DCC 2021) | content-defined sampling + Gear hash: **31.4× faster than N-transform, 7.9× faster than Finesse**, keeps N-transform's ratio, **1.22× better ratio than Finesse**; 3.20×/1.41× end-to-end throughput |
| **Palantir** | ASPLOS 2024 | hierarchical detection: **+27.4 % similarity coverage** over N-transform and Odess, +95.8 % over Finesse, ≤7.7 % throughput penalty, false-positive filter worth up to +6.4 % ratio |
| **Argus** | ACM ToS 2025 | bin-wise partitioning: up to **2.29× higher delta ratio** than Finesse/Odess/N-transform, 1.18× faster feature generation than Odess |

**Verdict: watch, prototype at best — because it collides with two narc
invariants.**

- **Bounded extraction memory / self-contained chunks.** Today a chunk is
  independently decodable and its blake3 hash proves it. A delta chain means
  extracting file X requires locating and decoding base chunks elsewhere in the
  archive, so extraction memory becomes a function of chain depth, and a seek
  pattern replaces a linear read.
- **`compact` and `remove`.** Dead chunks currently stay in the manifest as
  dedup sources and are dropped by `compact`. A delta base is not dead — it is
  load-bearing for a live chunk. `compact` would need a reachability pass over
  delta edges, and `remove` could no longer treat a chunk as garbage just
  because no file entry points at it.

Both are solvable (cap chain depth at 1, mark bases as pinned, refuse to
`compact` a pinned base) but this is a format-level change with real crash-
safety surface. It should come after §6, which gets a large share of the same
cross-unit redundancy with none of these problems.

---

## 6. "Reference archive" / shared-corpus compression

### 6.1 The external version: reject

The evidence against an external corpus the decompressor must also hold:

- **SDCH is dead.** Chrome's original shared-dictionary mechanism was disabled
  years ago — non-standard, with **oracle-based side-channel security risks**.
- **Dictionary drift is a real operational cost.** LinkedIn's SDCH deployment
  needed regeneration ideally at every front-end deploy, at **~7 hours per
  generation**; deploying up to 3×/day they fell back to "near-line" generation
  every two weeks. Versioning static content was their second named challenge.
- **The dictionary counts against you.** Corpus-compression research measures an
  "archived ratio" that includes the compressed dictionary, and notes it does
  **not** change monotonically as the dictionary is pruned.
- **Large dictionaries have their own costs:** more expensive lookups, and every
  reference into a big dictionary costs more bits, which can erase the gain.
- **It breaks the one property an archive must have.** A `.narc` that needs an
  external 500 MB corpus is not an archive; it is half of a backup.

The modern web version does work (Compression Dictionary Transport, shared
brotli standardised as **RFC 9841**, Sept 2025; YouTube JS down up to 90 % for
returning users, Google search HTML nearly 50 %) — but that is a client/server
system with a live negotiation channel, which an archiver does not have.

### 6.2 The in-archive version: this is the shippable one — and it is closer than ROADMAP thinks

Store the dictionary **inside the archive**, once, and use it as a preset
dictionary for every unit. The archive stays self-contained; the dictionary is
immutable, so editing one file still rewrites exactly one unit; a new
dictionary generation can be appended for later units without touching old ones.

**Three facts make this a `ship-now`:**

1. **The plumbing already exists.** narc depends on `lzma-rust2` 0.19, which
   supports preset dictionaries on **both** sides:
   `Lzma2Options.lzma_options.preset_dict: Option<Vec<u8>>` on the encoder and
   `Lzma2Reader::new(inner, dict_size, preset_dict: Option<&[u8]>)` on the
   decoder. `codec.rs` currently passes `None`. (Contrast liblzma, where
   `preset_dict` works **only with raw encoding/decoding** and none of its
   container formats support it on decode — and `xz2`'s `LzmaOptions` exposes no
   setter at all. Our pure-Rust choice accidentally handed us the better API.)
2. **MEASURED (earlier session, `test/dict-C.log`) — it works at our block
   size.** LZMA2 ‑9e, 74.24 MiB of 5 707 source files:

   | Block size | no dict | + 1 MiB COVER dict | + raw-sample dict |
   |---|---|---|---|
   | 1 MiB (77 blocks) | 9.663 MiB | **8.564 (88.6 %)** | 9.270 (95.9 %) |
   | 4 MiB (19) | 8.309 | **7.599 (91.4 %)** | 8.078 (97.2 %) |
   | 16 MiB (5) | 6.616 | **6.201 (93.7 %)** | 6.500 (98.2 %) |
   | 32 MiB (3) | 5.812 | **5.507 (94.7 %)** | 5.703 (98.1 %) |
   | one solid stream, 64 MiB dict | 5.540 | — | — |

   At 32 MiB blocks a trained dictionary gives **−5.3 %**, and 5.507 MiB
   **beats the single-solid-stream 5.540 MiB** — i.e. a preset dictionary buys
   more than 7-Zip's whole-archive solid model while keeping 32 MiB edit
   granularity. Also note trained (COVER) beats a raw sample by ~4 points, so
   training matters.

   > **[REVIEW] The comparison in the sentence above is not like-for-like and
   > the conclusion reverses when it is.** 5.507 MiB (5 774 510 B) is the block
   > payload *excluding* the dictionary; 5.540 MiB (5 808 715 B) is a complete
   > self-contained stream. Charging the dictionary its own measured storage
   > cost of **351 380 B** (`test/dict-G.log`) gives **6 125 890 B vs
   > 5 808 715 B — the solid stream wins by 5.5 %**, and the dictionary is a
   > **net +0.51 % against plain 32 MiB blocks** (6 094 630 B). Research 10
   > §2.6 states this rule explicitly ("the dictionary must be stored in the
   > archive, so its compressed size is charged against it") and this report
   > reproduces the exact error it claims to be correcting.

3. **The earlier "it's a wash" conclusion was an artefact of corpus size.**
   *(→ [REVIEW]: research 10 §2.6 had already done this arithmetic and put
   break-even at ≈90 MiB of small files; this is a restatement, not a
   correction. See §6.2b.)*
   `test/dict-G.log` counted the stored dictionary and got 99.9–100.8 % of
   no-dict — no gain. But do the arithmetic: the gain was **320 120 B over 3
   blocks = 106 707 B per 32 MiB block**, and the stored 1 MiB dictionary cost
   **351 380 B**. Break-even is **3.3 blocks ≈ 105 MiB of small-file input.**
   That corpus was sitting *exactly on* break-even. Above it the fixed cost
   amortises and the ratio converges to −5.3 %; a 1 GiB small-file archive
   (~32 blocks) would save ~3.3 MiB against a 351 KiB cost. **ROADMAP currently
   records "no trained dictionaries yet" as an open issue and "trained
   dictionaries per file-type group" as a plan; it should instead record the
   break-even threshold, because that is the decision rule.**

Supporting calibration from the same logs:

- **`dict-A.log`** — per-file zstd‑19 with a dictionary: 4 K dict → 91.1 % of
  base, 64 K → 80.5 %, 256 K → 76.3 %, 1 MiB → 67.4 %. Holdout tracks self
  within ~2 points, so this is not overfitting.
- **`dict-B.log`** — the ceiling effect: a 110 KiB zstd dictionary **helps** at
  0.25 MiB blocks (93.9 %) and **actively hurts** from 4 MiB up (105.2 %, 108.4 %,
  112.6 %, 115.3 %). A 1 MiB dictionary stays neutral-to-positive to 32 MiB
  (100.5 %). **Dictionary size must scale with block size, and a too-small
  dictionary on a large block is worse than none.** This is the concrete form of
  the general zstd guidance (~10 % gain at 64 KB inputs, up to 5× under 1 KB).
- **`dict-D.log`** — per-extension dictionaries beat one global dictionary by
  **10.40 %** on per-file compression (`.c` 15.87 %, `.pl` 16.96 %, `.s`
  14.34 %), but cost 880 KiB of storage vs 110 KiB, net −405 KiB on that corpus.
  Same amortisation logic: per-extension dictionaries need a bigger archive to
  pay.
- **`dict-T.log`** — training cost is manageable: `fastcover d=8 k=200` reaches
  77.8 % in **21.3 s** vs the library default optimise mode's 78.9 % in 107.2 s,
  and training on **10 % of files** gets 78.6 % in 4.4 s. So dictionary training
  can run on a sample during phase 1 without wrecking pack time.
- **`dict-F.log`** — brotli's *free built-in* dictionary reaches 87.9 % of
  zstd‑19 per-file where a trained zstd dictionary reaches 77.8 %. Interesting
  as a floor, not actionable (we do not ship brotli).

### 6.2b [REVIEW] This is not new, it is the weakest of three measured variants, and the evidence is a proxy

Three problems, in ascending order of seriousness.

1. **The numbers are a proxy, not the API this section says is "already
   plumbed".** `test/dict-C.log` and `test/dict-G.log` estimate a preset
   dictionary as `C(dict ‖ data) − C(dict)` (research 10 §1.3 states this).
   That primes the range coder's probability model as well as the LZ window;
   `lzma-rust2`'s real `set_preset_dict` (`lz/lz_encoder.rs:255-267`) primes
   **the window only**. The real API can therefore only do worse than every
   positive number quoted here. Research 10 already flagged this and asked for
   a real-API run before trusting any positive LZMA2 dictionary number.

2. **That run exists, and it is research 15 §1.4b** — `lzma-rust2` preset 9 +
   `nice_len 273`, one encoder per unit, with an asserted bit-exact round-trip
   through `Lzma2Reader` using the same preset dict, on the same 5751-file tree.
   Its result on the *stored-dictionary* variant this section recommends: a
   16 MiB strided-sample dictionary compresses the units to 7.582 MiB but costs
   2.202 MiB to store = 9.784 MiB, **worse than every zero-storage variant**,
   and its conclusion is literally "Do not build a dictionary trainer for this.
   Use existing archive units as dictionaries — they cost zero bytes."

3. **Two cheaper things dominate it.** From the same measured table:
   **32 MiB independent units = 9.389 MiB and 64 MiB independent units =
   9.246 MiB with no format change at all**, versus 9.784 MiB for the trained
   stored dictionary. Research 10 §6.3 makes the same point from the other
   side: not fragmenting into blocks is worth 4.9 % for free, while the best
   dictionary configuration is break-even. And research 10 §6.1 identifies the
   one regime where a stored dictionary *is* strong and this report never
   mentions: **units created by `add` after the archive exists** — the
   per-file regime, hold-out gain −20.8 % (110 KiB dict) / −30.8 % (1 MiB
   dict). That is narc's differentiator and it is the case worth building.

**Corrected verdict: reject the "train a dictionary and store it in the
archive" design. The live questions are (a) unit geometry and (b) priming from
an already-committed unit, both owned by research 15.** The one thing this
section contributes that research 15 does not is the observation that its
variant keeps extraction at *one* unit decode, whereas a chain costs up to N —
worth carrying into that design, not into a new one.

**PPMd7 caveat.** PPMd has no preset-dictionary concept. The equivalent is
priming the model by encoding the dictionary and discarding the output, which
the decoder must mirror — costing `dict_size / unit_size` extra decode work per
unit. At a 1 MiB dictionary and 32 MiB units that is ~3 %, which is acceptable,
but it is unimplemented in `ppmd-rust` and would need care. **LZMA2 first.**

**What to be careful about:** the dictionary becomes a format-level object that
units *reference*, so (a) it must be identified by hash in the manifest and
verified on open, (b) `compact` must never drop a referenced dictionary, and
(c) a corrupt dictionary is a multi-unit failure rather than a single-chunk one
— so it should be stored with the same integrity guarantees as a chunk, and
arguably duplicated.

### 6.3 Related: file ordering inside solid blocks

Adjacent, cheap, and currently unexamined. narc sorts small files **by
extension** before packing them into solid blocks. The one published
measurement I found argues that is not obviously right: on an LLVM source
snapshot, **plain lexical path order gave the best result, beating sort-by-size
by as much as 8 %, and sorting by suffix (grouping similar file types) did *not*
beat path order** (Michał Górny, 2022‑11‑18).

The MinHash-clustering idea has strong support only in genomics, where
sketch-distance clustering plus reference-based compression gives 20–30 % on
NCBI / 1000 Genomes sets. Górny's data suggests the effect is largest where path
order does *not* correlate with content — hash-named blobs, flat document dumps,
container layers — and smallest on source trees, which is narc's main
many-small-files case.

**Verdict: prototype.** It is a comparison of three sort keys on an existing
corpus (`test/corpus`, 5 751 files) with no format change whatsoever, so it
costs an afternoon and might silently be worth a percent.

---

## 7. Negative knowledge (tested/published and disproven — do not retry)

1. **Grammar compression (GLZA / Re-Pair / Sequitur) in narc.** GLZA is 18 %
   below xz on enwik9 but needs 85 KB/s compression and 15.3 GB for a 1 GB
   input, and its edge comes from a 1 GB window that our 16 MiB units delete —
   the same mechanism that already disqualified LZMA2 as our text codec.
   libbsc reaches 99 % of GLZA's ratio at 512× the compression speed.
2. **CDC advances since FastCDC.** RapidCDC, QuickCDC, SeqCDC and VectorCDC buy
   only chunking throughput; the independent 2024 survey (arXiv:2409.06066)
   finds no dedup-ratio gain over Gear-with-normalized-chunking. Our chunker is
   ~1000× faster than our codecs.
3. **Order-0 entropy as a transpose detector.** `H0` is invariant under
   permutation, so it scores every transpose identically to the identity. Use
   order-1 conditional entropy — and only to shortlist.
4. **Any single cheap statistic as the filter *decision*.** `H1` argmin costs
   +42 % on `nci`. Blind strides cost up to +174 %. Trial-compress the
   shortlist with the codec you will actually use.
5. **zstd‑1 as the trial proxy for an LZMA2 unit.** Disagrees on `sao` (+4.08 %
   regret) because filter value depends on the back-end's own periodicity
   modelling.
6. **Whole-record transposition as a substitute for per-field splitting.** On
   `sao` (28-byte records) it helps zstd (−6.1 %) and hurts LZMA2 (+4.1 %).
   OpenZL's 2.06× on that file is not reachable this way.
7. **Alphabet packing / MTF / frequency remap / RLE pre-filters.** 0–8 % on
   synthetic sparse alphabets, ~0 or negative on real data; MTF is
   catastrophic (+300 %). Already measured in `test/alpha.log`.
8. **Small dictionaries on large blocks.** A 110 KiB zstd dictionary *hurts*
   from 4 MiB blocks up (105–115 % of no-dict). Dictionary size must scale with
   the compression unit.
9. **Non-byte-exact transcoders.** EXI, WOFF2's glyf transform, SQLite
   `VACUUM`, wasm section stripping and every structural JSON/XML format
   normalise something. An archiver cannot use them without also storing the
   normalisation diff.
10. **pack200-style per-format bytecode transcoding.** Removed from the JDK
    (JEP 367) not for poor ratio but because the format was tightly coupled to
    formats that kept evolving. Every per-format transcoder is a permanent
    maintenance liability.
11. **External reference corpora.** SDCH was disabled over side-channel risks;
    LinkedIn measured ~7 h per dictionary regeneration and named versioning as
    a blocker; and the win is available in-archive without giving up
    self-containment.
12. **Embedding OpenZL's wire format in `.narc`.** No 1.0; README states the
    compressed format is subject to change; `dev` branch offers no guarantees.
    *(Verified 2026‑08‑17: latest release is **v0.2.0, 2026‑05‑07**; the README
    promises release-tagged frames stay decompressible "for at least the next
    several years" — an expiry date is not an archive guarantee.)*

**Added by the review pass (all MEASURED 2026‑08‑17, `test/skeptic/`):**

13. **Byte-transpose as a pre-filter anywhere in narc's max tier.** PPMd7
    beats LZMA2 by 13–16 % on exactly the fixed-width numeric files transpose
    targets (`x-ray`, `mr`), and transposing then costs +4.0…+5.3 % of the
    tournament result. The whole win reported in §1.2 is a win against a codec
    the archive does not use on those files.
14. **Scoring any filter against one codec when the tier runs a tournament.**
    The filter's value and the codec's identity are not separable: the same
    transpose that helps LZMA2 on `sao` by −6.1 % hurts it by +4.1 %, and the
    same transpose that helps LZMA2 on `x-ray` by −9.4 % makes the *unit* 5.3 %
    bigger once PPMd7 is in the candidate set. Any future filter probe must
    score `filter × codec` jointly.
15. **LZMA2 `lc`/`lp`/`pb` tuning as a free ratio knob on general data.**
    `lzma-rust2` exposes all three and narc could carry them in the per-chunk
    `param` byte, which LZMA2 currently ignores — so this costs *no* format
    change. Measured over `{lc0lp1pb1, lc2lp1pb1, lc3lp0pb0, lc0lp0pb1,
    lc4lp0pb0}` on 4 MiB of `x-ray`/`mr`/`sao`/`osdb`/`ooffice`: best case
    −0.5 % (`ooffice`, `lc4pb0`), worst +4.9 % (`sao`), and **zero effect on
    every unit PPMd7 wins**. Not worth a probe on general data. It stays
    relevant only paired with an alignment filter (xz documents `pb=2 lp=2
    lc=2` for ARM64 code), i.e. as part of §8b.1, not on its own.

---

## 8. Concrete next steps, in order

> **[REVIEW] Corrected order: Step 2 is the only one that survives as written.**
> Step 1 is refuted (§1.2b) — do not build it. Step 3 is superseded by
> research 15 §1.4b (§6.2b) — do not build a dictionary trainer. Steps 4, 5, 6
> stand as prototypes. The new first item is **BCJ coverage**, which is
> cheaper than this report thought and is the only measured, unclaimed win
> left in the topic: see §8b.1.

**Step 1 — Transpose filter + selectable delta (ship).** ~~ship~~ **DO NOT
BUILD** — §1.2b measures it at +0.6 % on the Silesia archive, not −1.1 %.
Add `Filter::Transpose(2..=32)` at filter ids 34..=64. Implement the two-stage
selector from §4.2 in `analyze.rs`: order-1 conditional entropy over 16
candidates on a 64 KiB sample → keep `none` plus the best 3 → trial-compress
those on 64 KiB with the tier's codec at its cheapest preset → apply the winner
to the whole unit. Gate the whole sweep behind a structural signal (skip it
entirely when `classify()` returned `Text` or `Precompressed`) so ordinary
content pays nothing.
Acceptance: Silesia archive shrinks ≥1 %, no file regresses, pack time grows
<5 %, `Delta` stops being dead code.

**Step 2 — ARM64 BCJ (ship).**
Kanzi's EXE transform covers x86 and ARM64 in one filter; liblzma has shipped an
ARM64 filter since 5.4. Our measured x86 BCJ win is +4.4–5.7 %. Detect PE/ELF
machine type rather than just `MZ`/`\x7FELF`, and pick the right converter.
Verify byte-identical round-trip the way BcjX86 was verified against liblzma.

> **[REVIEW] Cheaper than stated, and with two caveats this report omits.**
> Nothing needs porting from Kanzi or liblzma: **`lzma-rust2` 0.19 — already a
> narc dependency — ships `arm`, `arm64`, `riscv`, `ppc`, `sparc` and `ia64`
> BCJ filters** (`src/filter/bcj/{arm,riscv,ppc,sparc,ia64}.rs`, exposed as
> `BcjWriter::new_arm64` / `new_riscv` / …, Apache-2.0, last release
> 2026‑08‑16). The work is filter ids + machine-type detection + round-trip
> tests, not a port. Caveats: (a) the **32-bit `arm` filter applied to AArch64
> code is worse than no filter** (measured on `barebox.bin`: unfiltered 84.7
> KiB, `--arm` 85.5 KiB, `--arm64` 81.0 KiB — so misdetection is a regression,
> not a no-op); (b) gains on **unlinked `.o` / `.a` / `.ko`** are much smaller
> because address fields hold filler — relevant, since narc's small-file corpus
> is a source tree. The +4.4 % figure is *linked binaries only*.

**Step 3 — In-archive shared preset dictionary for LZMA2 (ship, biggest ratio lever).**
Train with `fastcover d=8 k=200` on a 10 % sample during phase 1 (measured
4.4 s / 78.6 %). Size the dictionary to the block size — 1 MiB for 32 MiB
blocks; never ship a 110 KiB dictionary for a ≥4 MiB block. Store it as a
hash-identified manifest object, allow multiple generations, forbid `compact`
from dropping a referenced generation. **Only enable it when the archive's
small-file payload exceeds the break-even (~105 MiB, i.e. ~4 blocks)** — below
that it costs bytes. Expected: closes a real part of the 12 MiB vs 8.8 MiB gap
against 7-Zip on the 114 MiB source tree, without unbounded units and without
touching edit cost.

**Step 4 — Solid-block ordering experiment (cheap prototype).**
Compare extension order (current), lexical path order, and MinHash-cluster
order on `test/corpus` at the max tier. Published evidence says path order may
already beat our extension sort.

**Step 5 — base64 undo (prototype).**
Detect long base64 runs, decode, store layout metadata (line length, padding,
alphabet variant, interior whitespace), and bail to raw unless the re-encode is
byte-identical. Measured value: −25.5 % on base64-wrapped binary.

**Step 6 — hand to research 02.** `preflate-rs` (Apache-2.0,
`forbid(unsafe_code)`, production-used, 0.01–2.7 % correction overhead) is the
right Rust vehicle for deflate/PDF/zip recompression, and it subsumes the
"Docker layer" and "PDF object stream" items in this report.

**Do not do:** OpenZL in the format, grammar codecs, a CDC replacement, an
XGBoost transform selector, EXI/WOFF2/SQLite transcoding, external corpora.

---

## 8b. [REVIEW] What this report missed

### 8b.1 The rest of the BCJ family is already vendored, and BCJ2 is the real gap

`lzma-rust2` 0.19 ships **x86, ARM, ARM64, RISC-V, PowerPC, SPARC and IA-64**
BCJ filters plus a **BCJ2 decoder** (`src/filter/bcj/`, `src/filter/bcj2/`).
This report names only ARM64 and sources it to Kanzi and liblzma, i.e. it looked
outward for something already inside the dependency tree.

More importantly it never mentions **BCJ2**, which is the single most widely
deployed structure-aware binary transform in existence: 7-Zip's default for PE
and ELF at `-mx9`. BCJ2 does not rewrite operands in place — it *splits* the
call/jump targets into separate streams and compresses each with its own
context, which is precisely the "per-field typed splitting" this report calls
XL-effort and unreachable (§1.5, §8 of the verdict table) while a shipping
implementation of it for machine code sits in 7-Zip and in narc's own crate's
decoder. It is also a plausible part of the unexplained gap on `mozilla` and
`ooffice`, where 7-Zip uses BCJ2 and narc uses BCJ1.

Honest cost: BCJ2 emits **four streams**, which does not fit narc's
one-chunk-one-payload record without a length-prefixed sub-container, and
`lzma-rust2` has no BCJ2 *encoder*. So it is a prototype, not a ship-now — but
it belongs in the table and it is a better use of the "structure-aware binary"
budget than a transpose filter.

### 8b.2 The report never ran narc

Every number in it comes from Python (`test/struct-exp/*.py`), from `xz`, or
from logs produced by other proxies. The repository has `test/bench.sh` and
`test/compare-7z.sh`, and the tournament is 12 lines of `analyze.rs`. One
end-to-end pack of Silesia at the max tier — or one read of
`Tier::candidates()` — would have caught §1.2b before the recommendation was
written. **Rule for the next report in this series: a claim about "the archive"
must be measured on an archive.**

### 8b.3 It does not cite the three sibling reports that already own its ground

`docs/research/10-dictionaries-and-alphabets.md` (dictionaries, base64, alphabet
packing), `14-frontier-neural-and-cm.md` (Hutter Prize, CM, model priming) and
`15-frontier-algorithms.md` (preset dictionaries measured through the real API,
OpenZL, libbsc/bzip3/Kanzi, similarity ordering) are all absent from §9. The
consequences are not cosmetic: §6.2 re-derives report 10's break-even and calls
it a correction; §1.7's base64 and alphabet results are report 10 §4; the
libbsc "side note" in §2.2 is report 15 §5, where it was already **downgraded**
because the LTCB margin is a 1 GB-block artefact; and §6.3's file ordering is
report 15 §6.

### 8b.4 The strongest evidence for its own §6.3 is the Hutter Prize lineage

§6.3 rates member ordering "unknown; published evidence says path order already
wins", citing one 2022 blog post and genomics. The decisive evidence is that
**article reordering is the entire contribution of a Hutter Prize winner**:
STARLIT (Margaritov, 2021) reorders enwik9's articles to maximise mutual
information between neighbours before cmix, and every subsequent winner carries
it — fx2-cmix (accepted 2024‑10‑08, 110 351 665 B) and cmix-lex (announced
2026‑06‑26, 109 190 109 B) both list *improved article sorting* as a source of
gain. The stated mechanism transfers directly to narc: model state is finite,
so putting similar members near each other lets shared context be reused
**before it is evicted** — which is exactly what a 32 MiB solid block fed to
PPMd7's bounded pool does. That raises §6.3 from "might be worth a percent" to
"the mechanism is proven at the top of the field"; it does not make it
ship-now, because narc's members are source files, not encyclopaedia articles.

### 8b.5 base64 undo has two shipping reference implementations

§1.7 proposes it as new work and lists the reversibility hazards (line length,
padding, alphabet variant, interior whitespace) as things to solve. Precomp and
paq8px both already ship a base64 transform with those edge cases handled, and
paq8px's is the one to read before writing ours.

### 8b.6 One number in the report is worth more than the report thinks

The LWN/OpenZL table gives LZMA ‑9 on `sao` a ratio of 1.64×. Independently, on
this machine, narc's own LZMA2 settings give 4 194 304 → 2 562 214 on the first
4 MiB of `sao` = **1.637×**. The external benchmark and our codec agree to three
digits, which means **OpenZL's 2.06× on that file is a real, calibrated ~20 %
gap against narc on record-structured numeric data**, not a marketing figure —
and §1.2b proves a transpose filter cannot close it. That is the honest case for
keeping OpenZL on "watch" and for treating typed field splitting (§1.5) as the
only known route to it.

---

## 9. Sources

**Structure-aware frameworks**
- OpenZL paper — https://arxiv.org/abs/2510.03203 (v1 2025‑10‑03, v2 2025‑10‑30)
- OpenZL repo (BSD-3, C11/C++17) — https://github.com/facebook/openzl
- Meta engineering announcement, 2025‑10‑06 — https://engineering.fb.com/2025/10/06/developer-tools/openzl-open-source-format-aware-compression-framework/
- LWN, "Format-specific compression with OpenZL", 2026‑01‑14 — https://lwn.net/Articles/1053018/
- SDDL docs — https://openzl.org/sddl/getting-started/
- Rust bindings — https://github.com/LDeakin/openzl-sys , https://github.com/vitorpy/rust-openzl
- Kanzi (Apache-2.0) — https://github.com/flanglet/kanzi-cpp , wiki https://github.com/flanglet/kanzi-cpp/wiki/Main-page

**Numeric / float / columnar**
- SPDP (DCC 2018) — https://userweb.cs.txstate.edu/~burtscher/research/SPDPcompressor/
- FCBench (PVLDB 17) — https://www.vldb.org/pvldb/vol17/p1418-tao.pdf , https://arxiv.org/pdf/2312.10301
- TDT, 2025‑06‑22 — https://arxiv.org/abs/2506.18062
- Blosc bitshuffle — https://blosc.org/posts/new-bitshuffle-filter/ , bytedelta https://blosc.org/posts/bytedelta-enhance-compression-toolset/
- BtrBlocks (SIGMOD 2023) — https://www.cs.cit.tum.de/fileadmin/w00cfj/dis/papers/btrblocks.pdf , repo https://github.com/maxi-k/btrblocks
- Vortex / cascading compression — https://spiraldb.com/post/cascading-compression-with-btrblocks
- DataCortex (Rust, JSON structure inference) — https://github.com/RushikeshMore/datacortex
- Stride detection via autocorrelation — https://arxiv.org/pdf/2410.21558

**Grammar compression**
- Conrad & Wilson, DCC 2016 — https://ieeexplore.ieee.org/document/7786210/
- GLZA releases — https://github.com/terrelln/GLZA
- Practical/effective Re-Pair — https://arxiv.org/abs/1704.08558 ; Re²Pair (ESA 2024) — https://pmc.ncbi.nlm.nih.gov/articles/PMC11275962/
- Large Text Compression Benchmark (updated 2026‑07‑08) — https://mattmahoney.net/dc/text.html

**Format-specific transcoding**
- WOFF2 spec — https://www.w3.org/TR/WOFF2/
- preflate-rs (Apache-2.0) — https://github.com/microsoft/preflate-rs ; Precomp — https://github.com/schnaader/precomp-cpp
- EXI evaluation (W3C Note, 2009‑04‑07) — https://www.w3.org/TR/2009/WD-exi-evaluation-20090407/
- XWRT — https://github.com/inikep/XWRT ; Skibiński, *Effective asymmetric XML compression* (2008) — https://onlinelibrary.wiley.com/doi/abs/10.1002/spe.859
- JEP 336 / JEP 367 (pack200 deprecation & removal) — https://openjdk.org/jeps/336 , https://openjdk.org/jeps/367
- CLP (OSDI 2021) — https://www.usenix.org/system/files/osdi21-rodrigues.pdf ; LogGrep (EuroSys 2023) — https://yangwang83.github.io/papers/eurosys23-final39.pdf ; LogShrink (ICSE 2024) — https://arxiv.org/pdf/2309.09479
- DupHunter (ATC 2020) — https://www.usenix.org/conference/atc20/presentation/zhao
- sqlite_zstd_vfs — https://github.com/mlin/sqlite_zstd_vfs ; sqlite-zstd — https://github.com/phiresky/sqlite-zstd

**Learned selection**
- MLcomp (DCC 2024) — https://ieeexplore.ieee.org/document/10533784/ , PDF https://userweb.cs.txstate.edu/~burtscher/papers/dcc24b.pdf
- 2026 AIT Data Compression Challenge (2026‑06‑16) — https://arxiv.org/abs/2606.17712
- Neural NCD and the compression/classification disconnect — https://arxiv.org/pdf/2410.15280

**Chunking and dedup**
- FastCDC (ATC 2016) — https://www.usenix.org/conference/atc16/technical-sessions/presentation/xia ; TPDS 2020 — https://ieeexplore.ieee.org/document/9055082/
- CDC survey (2024‑09‑09) — https://arxiv.org/abs/2409.06066
- SeqCDC (Middleware 2024) — https://cs.uwaterloo.ca/~alkiswan/papers/SeqCDC_Middleware24.pdf ; VectorCDC (FAST 2025) — https://www.usenix.org/system/files/fast25-udayashankar.pdf
- Finesse (FAST 2019) — https://www.usenix.org/conference/fast19/presentation/zhang ; Odess (ToS 2023) — https://dl.acm.org/doi/10.1145/3584663 ; Palantir (ASPLOS 2024) — https://henryhxu.github.io/share/hongming-asplos24.pdf ; Argus (ToS 2025) — https://dl.acm.org/doi/10.1145/3747839

**Dictionaries**
- zstd dictionary guidance — https://github.com/facebook/zstd
- RFC 9841, Shared Brotli Data Format (2025‑09) — https://datatracker.ietf.org/doc/html/rfc9841
- Compression Dictionary Transport — https://developer.chrome.com/blog/shared-dictionary-compression , https://developer.chrome.com/blog/search-compression-dictionaries
- SDCH at LinkedIn (drift/regeneration cost) — https://engineering.linkedin.com/shared-dictionary-compression-http-linkedin
- liblzma `preset_dict` limits — https://tukaani.org/xz/liblzma-api/structlzma__options__lzma.html
- zstd `--patch-from` / `refPrefix` — https://github.com/facebook/zstd/wiki/Zstandard-as-a-patching-engine
- tar member ordering vs xz ratio (2022‑11‑18) — https://blogs.gentoo.org/mgorny/2022/11/18/tar-sorting-vs-xz-compression-ratio/

**Local measurements**
- `test/struct-exp/stride.py`, `test/struct-exp/proxy.py` — this report
- `test/dict-A.log` … `test/dict-T.log`, `test/alpha.log` — earlier session
- `test/skeptic/` — **review pass, 2026‑08‑17**: narc's real max-tier
  tournament (LZMA2 preset 6 + `nice_len 273` vs PPMd7 o10/o16, pool 32×len)
  with and without transpose/delta, whole-file and 4 MiB units (§1.2b, §4.2b)

**Added by the review pass (retrieved 2026‑08‑17)**
- `lzma-rust2` 0.19.0 (Apache-2.0, released 2026‑08‑16) — `src/filter/bcj/`
  ships x86/ARM/ARM64/RISC-V/PPC/SPARC/IA-64; `src/filter/bcj2/` is decode-only;
  `LzmaOptions::{lc,lp,pb,preset_dict}`; `lz/lz_encoder.rs:255-267` primes the
  LZ window only — https://github.com/hasenbanck/lzma-rust2
- OpenZL releases: v0.1.0 2025‑10‑06, **v0.2.0 2026‑05‑07**, no 1.0 —
  https://github.com/facebook/openzl/releases
- preflate-rs: Apache-2.0, v0.7.6 on crates.io 2026‑03‑20, last push
  2026‑04‑25, 33 stars — https://crates.io/crates/preflate-rs
- ARM64 BCJ measured on a linked aarch64 binary (unfiltered 84.7 KiB, `--arm`
  85.5 KiB, `--arm64` 81.0 KiB); xz docs give the family a 0–15 % range and
  recommend `pb=2 lp=2 lc=2` with it — https://man.archlinux.org/man/xz.1.en ,
  https://lore.barebox.org/barebox/ZPmcWuI9wqCPfCpK@tour/
- Hutter Prize lineage for member reordering: STARLIT (2021) —
  https://github.com/amargaritov/starlit ; fx2-cmix accepted 2024‑10‑08 and
  cmix-lex 2026‑06‑26 — http://prize.hutter1.net/
- Sibling reports this one duplicates: `docs/research/10-dictionaries-and-alphabets.md`
  §1.3 §2.6 §6.1 §6.3, `docs/research/15-frontier-algorithms.md` §1.4b §1.4c §5.4 §6.3
