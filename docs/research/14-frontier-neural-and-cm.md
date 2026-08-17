# 14 — Neural and context-mixing compression in 2026: what is actually shippable

Research date: 2026-08-17. All numbers below are quoted from primary sources with their own
measurement dates; where a source's hardware is unknown or old, that is stated.

> **Adversarial review pass, 2026-08-17.** Every "ship-now" and "prototype" verdict in this
> document was re-checked against primary sources, and the two central claims were tested by
> direct measurement on the owner's machine. Results, in order of how much they change the plan:
>
> 1. **§7's attribution was wrong and its "ship-now" item does not work as specified.** The
>    paq8px→generic-CM gap does *not* sit in the record/16-bit-image files. Per-file numbers from
>    the same Mahoney table put **51% of the headroom in `mozilla`+`samba`+`ooffice`** (executables
>    and binary containers) and **32% in plain text**; records and 16-bit images together are
>    **17%**. Worse, implemented the way §10 proposed — as reversible filters in front of LZMA2 —
>    the record model **measurably loses** (MEASURED HERE: `sao` +3.9%, `osdb` +16.7%). Only the
>    16-bit-image half survives (MEASURED HERE: `x-ray` −11.0%, `mr` −10.1%). Corrected in §7.
> 2. **The CM speed figure was 2.5–3× too pessimistic and the ratio needs no discount at all.**
>    kanzi's README states the level-9 default block size *is* 32 MB, so 41,520,670 on Silesia is
>    already a measurement at narc's own unit granularity, produced with only ~7 of 16 cores busy.
>    Corrected per-core figure: **~2.7–2.9 MB/s**, not 1.1. Corrected in §6 and §10.
> 3. **The memory line was an order of magnitude too optimistic.** This document's own LTCB table
>    lists kanzi TPAQX at 3,100 MB and lpaq9m at 1,542 MB. "64–256 MB per decode thread" is a
>    different, unmeasured operating point, not the one the 41.5 MB came from. Corrected in §10.
> 4. **§9's DRT recommendation is dominated by a measured alternative** in doc 15 §1.4
>    (preset dictionaries built from committed archive units: −12.6…−18% on the source tree, zero
>    stored dictionary bytes, no language dependence). Downgraded to **watch**.
> 5. New: §11 lists what this survey missed — chiefly **model priming**, **integer SIMD in the
>    mixer**, **PPMd→PPMonstr secondary estimation**, and **selective-extract latency**.
>
> Numbers labelled "MEASURED HERE" were produced 2026-08-17 on the owner's box (8 logical cores,
> Windows 11) with `xz --format=raw -T1 --lzma2=preset=9e` against
> `test/Silesia-compression-corpus/raw/`. The baselines reproduce Mahoney's `7z -mx=9` per-file
> column to within 0.2%, which validates the harness.

Reference point for everything in this document — narc as measured on 8 cores / Windows 11:

| Corpus | narc max | 7-Zip -mx9 | narc time |
|---|---|---|---|
| Silesia 202 MiB (211.9 MB) | 47 MiB (~49.3 MB) | 47 MiB / 49 s | 6.8 s (≈31 MB/s aggregate) |
| Source tree 114 MiB, 5751 files | 12 MiB | 8.8 MiB | — |

---

## 0. Executive answer to the critical question

> Is there any neural or CM technique in 2026 that reaches within a few percent of paq-class
> ratio at ≥10 MB/s single-thread on a CPU, with deterministic bit-exact output across machines?

**No. Not within a factor of two, let alone a few percent.** The measured 2026 frontier, on the
same corpus narc benchmarks (Silesia), from Matt Mahoney's Silesia benchmark (last updated
**2026-05-20**) and the kanzi 2.5.0 README benchmark (Ryzen 9 9950X, Ubuntu 25.10):

| Class | Program | Silesia total | vs paq8px | Single-thread throughput |
|---|---|---|---|---|
| paq-class CM | paq8px_v215 -12L | **27,825,511** | — | ~3 KB/s |
| ↳ *note* | *`-L` = LSTM submodel enabled (paq8px README); `-12` = max memory level, ~29 GB. This row is therefore **CM + a float neural net at 29 GB**, not a deterministic-submodel result — see §7* | | | |
| paq-class + preproc | precomp -cn \| cmix v21 | 28,300,000 (approx) | +1.7% | ~1.6 KB/s |
| heavy open CM | zpaq 6.21 -method 7 | 39,112,624 | +41% | ~0.15–1 MB/s |
| open CM | mcm 0.82 -9 -max | 40,068,926 | +44% | ~2.5 MB/s |
| open CM (maintained) | kanzi 2.5.0 -l 9 (TPAQX) | 41,520,670 | +49% | **~2.7–2.9 MB/s/core** |
| BWT+CM (maintained) | bsc 3.3.11 | 46,723,436 … 47,900,848 | +68…72% | ~11 MB/s/core |
| **narc today** | **narc max** | **~49,300,000** | **+77%** | ~4 MB/s/core |
| LZMA | xz 5.8.1 -9 / 7z -mx=9 | 48,802,580 / 48,792,760 | +75% | ~4.9 MB/s |

Read that table twice. Two separate facts fall out of it:

1. **At ≥10 MB/s single-thread there is nothing but BWT.** bsc lands at 46.7–47.9 MB, i.e. ~70%
   larger than paq8px. No CM and no neural model gets near 10 MB/s single-thread in 2026.
2. **The bankable prize is not "paq-class". It is 15–20% over LZMA at ~2–3 MB/s/core.** Every
   maintained open CM codec sits in a tight band, 39–42 MB on Silesia, at 2–3 MB/s per core
   (corrected — see §6). That band is 15–20% better than narc's current 49.3 MB. It is also
   40–50% *worse* than paq8px. **Where the remaining 40–50% lives was mis-attributed in the first
   draft.** Decomposing the same Mahoney table per file: half of it is executables and binary
   containers (`mozilla`, `samba`, `ooffice`), a third is plain text, and the 16-bit-image and
   record files the first draft named are 17%. Some of it is also just paq8px's 29 GB and its
   LSTM. See §7, which now carries the decomposition and a direct measurement.

On determinism the answer is cleaner and more useful:

- **Integer CM is bit-exact and portable, and always has been.** The ZPAQ Level 2 standard makes
  this an explicit design goal — the archive carries the decompression model as ZPAQL bytecode,
  the VM has fixed-width integer arithmetic and no floating point, and predictions are therefore
  reproducible on any machine. paq/lpaq/TPAQ mixers are integer logistic mixing (int weights,
  `stretch`/`squash` lookup tables, 12-bit probabilities, 32-bit range coder).
- **The moment a float layer enters, portability dies.** cmix's own README says it outright:
  build with `-Ofast -march=native` and you get *"incompatibility between different computers due
  to floating-point precision differences"*; use `-O3` for cross-platform compatibility at a
  speed cost. cmix has a float LSTM and a float mixer path. paq8px runtime-selects
  SSE2/AVX2/AVX512/NEON code paths and claims hardware does not affect ratio, but it also warns
  that compiling for a specific architecture lets the compiler auto-vectorise — the classic FP
  reassociation hazard, in a design where one flipped bit desynchronises the coder forever.
- The failure is not theoretical. arXiv **2603.25260** (Mar 2026, LiDAR point clouds) documents
  encoding on an RTX 4090 and decoding on an RTX 5880: *"the reconstructed point cloud collapses
  into an approximately uniform distribution"*. Their fix is a full integer-only inference
  pipeline (int8 weights/activations with int32 accumulate, 32-bit fixed-point requantisation and
  activations, softmax by lookup table). Same conclusion, arrived at from the other direction:
  **if you want a portable archive, the entire prediction path must be integer.**
- One counterexample worth knowing: Bellard's **ts_zip** (v-latest 2024-03-02) claims *"the model
  is evaluated in a deterministic and reproducible way. Hence the result does not depend on the
  exact GPU or CPU model nor on the number of configured threads."* So GPU-side determinism *is*
  achievable (BF16 with pinned evaluation order). It costs a closed binary, a GPU, and ≤1 MB/s.

**Therefore the only neural technique narc can ship is the one the paq lineage has shipped since
2002: integer logistic mixing of many cheap predictors.** Everything with a matmul in it is out.

---

## 1. The Hutter Prize and the LTCB: where the absolute frontier is, and how slow

### Hutter Prize (enwik9, 1 GB)

| Date | Program | Author | Total size (data + decompressor) | Award |
|---|---|---|---|---|
| 2021-05-31 | starlit (article sorting + cmix) | Artemiy Margaritov | 115,352,938 | 9,000 € |
| 2023-07-16 | fast cmix | Saurabh Kumar | 114,156,155 | 5,187 € |
| 2024-02-02 | fx-cmix | Kaido Orav | 112,578,322 | 6,911 € |
| **2024-09-03** | **fx2-cmix** | **Kaido Orav & Byron Knoll** | **110,793,128** | **7,950 €** |

**The record has not moved since September 2024.** As of late 2025 the bar to claim money was
still S ≤ 109,685,196 (a 1% improvement), and only ~29,945 € of the 500,000 € pool has ever been
paid out. Contest limits: ~one CPU core, <10 GB RAM, <100 GB temp disk, decompressor size charged
against you. fx2-cmix's LTCB decompression time is 272,072 ns/byte → **3.7 kB/s → ~75 hours to
decompress 1 GB**, using 8,811 MB.

fx2-cmix's gains over fx-cmix were *removing* models (indirect nonstationary predictors, some
match models and mixers) to buy time budget for complexity elsewhere, plus a reverse dictionary
transform and a paq8px-derived stemmer with new word types. That is the shape of the frontier in
2026: hand-tuned English-specific transforms bolted onto a CM ensemble, fighting for 1%.

### LTCB (Matt Mahoney, page last updated **2026-07-08**)

Times are ns/byte; **not all entries were measured on the same machine**, so cross-row speed
comparisons are indicative only. Mahoney's baseline reference is a 2.188 GHz Athlon-64 3500+.

| Program | Algorithm | enwik9 | Comp ns/B | Decomp ns/B | Memory |
|---|---|---|---|---|---|
| nncp v3.2 | Transformer (GPU) | 106,632,363 | 241,871 | 238,670 | 7,600 MB |
| cmix v21 -t | CM + LSTM | 107,963,380 | 622,949 | 638,442 | 30,950 MB |
| cmix-lex | CM + article reordering | 109,2xx,xxx | 145,029 | 147,029 | 9,993 MB |
| fx2-cmix | CM | 110.4 M | — | 272,072 | 8,811 MB |
| jax-compress | LSTM on TPU | 113.4 M | 110,013 | 112,140 | 41,900 MB |
| tensorflow-compress v4 | LSTM (A100) | 113,542,413 | 291,394 | 290,803 | 45,360 MB |
| starlit | CM + sorting | 115.0 M | 173,953 | 171,682 | 10,233 MB |
| phda9 v1.8 | CM | 116,544,849 | 86,182 | 86,305 | 6,319 MB |
| **gmix v1** | **GLN ensemble, 1 CPU thread** | **122,336,013** | **73,986** | **70,552** | **3,751 MB** |
| paq8px_v206fix1 -12L | CM + LSTM | 124,696,410 | 291,916 | 294,847 | 28,151 MB |
| durilca'kingsize -o41 -m13000 | PPM | 127,376,595 | 1,341 | 1,466 | 13,000 MB |
| paq8hp12any -8 | CM | 132,045,026 | 37,660 | 37,584 | 1,850 MB |
| zpaq v6.42 (max) | CM | 142.3 M | 6,699 | 14,739 | 14,000 MB |
| lpaq9m | CM | 143,943,759 | 868 | 898 | 1,542 MB |
| mcm | CM + LZP | 144,854,575 | 394 | 281 | 5,961 MB |
| nanozip -cc | CM | 148,545,179 | 1,149 | 1,141 | 32,000 MB |
| ppmonstr J -o16 -m1700 | PPM | 157,007,383 | 3,574 | ~3,600 | 1,700 MB |
| zcm | CM | 159,135,549 | 421 | 411 | 3,100 MB |
| bsc-m03 -b1000000000 | BWT + M03 | 160,258,936 | 160 | 135 | 13,000 MB |
| kanzi -b 1024M -e TPAQX | CM | 161,690,495 | 490 | 480 | 3,100 MB |
| bcm 2.03 | BWT + CM | 163,646,387 | 63 | 34 | 4,096 MB |
| bsc 3.25 -b1000 -e2 | BWT + CM | 163,884,462 | 23 | 8 | 5,000 MB |
| paq9a -9 | CM + LZP | 165,193,368 | 3,997 | 4,021 | 1,585 MB |
| lrzip | rzip + CM | 169,318,794 | 2,987 | 2,929 | 2,700 MB |
| **lstm-compress** | **LSTM, CPU** | **173,874,407** | **92,342** | **91,876** | **9 MB** |
| ccmx | CM | 174,142,092 | 1,313 | 1,338 | 1,332 MB |
| xz v5.2.1 | LZMA2 | 197,331,816 | 5,876 | 20 | 6,000 MB |
| zstd -22 | LZ77 | 215,7xx,xxx | 701 | ~2 | 792 MB |
| brotli | LZ77 | 223,597,884 | 3,400 | 5.9 | 437 MB |

Three things to take from this table.

**(a) The neural top of the table is unusable by construction.** nncp v3.2 is 4.1 kB/s *both
ways* — Bellard's own readme reports **enwik9 taking 2.8 days to compress on an RTX 4090, with
decompression at similar speed**. tensorflow-compress and jax-compress need an A100/TPU and
41–45 GB. cmix needs 31 GB.

**(b) `lstm-compress` is the single most decisive data point against "tiny neural nets".** It is
a small LSTM in 9 MB of RAM — exactly the "small model" shape one would hope for — and it lands
at **173.9 MB on enwik9 at 10.8 kB/s**. `bsc 3.25` reaches **163.9 MB, better, at roughly
40,000× the speed**. A tiny LSTM is strictly dominated by a 2010-era BWT compressor. There is no
version of this that becomes interesting.

**(c) The PPM column deserves a second look.** `durilca'kingsize -o41 -m13000` gets enwik9 to
127.4 MB at ~0.75 MB/s — better than every open CM compressor in the table except paq-class, and
narc already has a PPMd7 engine. The lever there is **order and memory** (order 41, 13 GB), not
neural anything. That is a cheaper experiment than a CM codec, though 13 GB collides head-on
with narc's bounded-extraction invariant.

---

## 2. LLM / transformer compressors: the 2023–2026 line, and why every one of them is out

| Project | Date | Model | Ratio | Speed | Determinism | License / blob |
|---|---|---|---|---|---|---|
| **DeepMind, "Language Modeling Is Compression"** (arXiv 2309.10668, ICLR 2024) | 2023-09-19, rev 2024-03-18 | Chinchilla 70B | ImageNet patches 43.4% (PNG 58.5%), LibriSpeech 16.4% (FLAC 30.3%) — **model size excluded** | n/a (research) | n/a | research |
| **LLMZip** (arXiv 2306.04050) | 2023-06 | LLaMA-7B | beats bsc/zpaq/paq8h on text | **9.5 days / 10 MB** | GPU-bound | research |
| **ts_zip** (Bellard) | 2024-03-02 | RWKV 169M v4, int8 weights, BF16 eval | enwik9 1.084 bpb (135.5 MB); enwik8 1.106; alice29 1.142 | "up to 1 MB/s on RTX 4090" | **Claims full hardware independence** | closed binary |
| **NNCP v3.3** (Bellard) | 2024-06-05 | Transformer, trained online | enwik9 106,632,363 (0.853 bpb) | **enwik9: 2.8 days compress, same to decompress**; needs Ampere/ADA/Hopper | not documented | MIT source **but libnc is binary-only** |
| **FineZip** (arXiv 2409.17141) | 2024-09 | Llama-3-8B | ~50% better than classical | **4 hours / 10 MB** (54× faster than LLMZip) | GPU-bound | research |
| **AlphaZip** (arXiv 2409.15046) | 2024-09 | LLM + AC | — | — | GPU-bound | research |
| **llama-zip** (AlexBuz) | 2024→ | any GGUF via llama.cpp, sliding window | strong on text; **can expand binary data** | llama.cpp inference speed | only within one fixed backend/config | MIT tool, model is the real dependency |
| **Nacrith** (arXiv 2602.19626) | 2026-02 | SmolLM2-135M GGUF + light online predictors, CDF 2^24, 32-bit AC | enwik8 0.9389 bpb (11.74 MB); alice29 0.918 bpb, "44% better than cmix v21" | ~1.2 GB VRAM/worker, up to 8 GPU workers | GPU, GGUF | code release not stated |

Verdict on the whole family: **reject**, and the reasons are structural, not incidental.

- **Speed.** The fastest thing here that actually produces an archive is ts_zip at ~1 MB/s
  *with an RTX 4090*, decompression symmetric. On the RTX 5060 Ti 8 GB the owner has, that is
  well under 1 MB/s. FineZip, the paper whose entire point is practicality, needs 4 hours for
  10 MB and its own authors write that *"LLMs are still not a viable solution for large-scale
  text compression."*
- **The model is part of the format.** Nacrith's 0.9389 bpb on enwik8 and the DeepMind numbers
  exclude ~500 MB–140 GB of weights. DeepMind is explicit about the honest accounting: they
  charge 2 bytes per parameter and note that language models *"suffer a huge loss in compression
  rate due to their large size, which cannot be offset when compressing only 1 GB of data."*
  For an archiver this is worse than a size penalty — it makes the archive undecodable without
  bit-identical weights forever. A .narc that cannot be opened in 2035 without a specific GGUF
  file is not an archive.
- **Context.** DeepMind's transformer setting chunks input to **2048 bytes** per compression
  unit. narc's problem is the opposite direction (32 MiB is already too small).
- **Blobs and licences.** NNCP's source is MIT but `libnc` ships binary-only with no source —
  a proprietary blob, disqualified. ts_zip is a closed binary. cmix and paq8px are GPL-2/3.
  gmix is GPL. bsc-m03 is GPL. Of everything surveyed, only **libbsc, libsais and kanzi
  (Apache-2.0)**, **libzpaq (public domain)** and **bzip3 (LGPL-3, static-link friction)** are
  licence-compatible with narc without contaminating it.

---

## 3. cmix / paq8 lineage: what is alive, what it costs

| Project | Status (2026) | License | Cost |
|---|---|---|---|
| **paq8px** (hxim) | **Alive.** v215 results posted to encode.su in 2026 | GPL-2+ | `-12L` uses ~29 GB RAM; ~3 kB/s |
| **cmix** (byronknoll) | v21, 2024-09-10, incorporating fx2-cmix | GPL-3 | ≥32 GB RAM recommended; 1.6 kB/s |
| **gmix** (byronknoll) | Active; GLN ensemble, **CPU single thread, no GPU** | GPL | 122.3 MB enwik9 at 13.5 kB/s; "not yet competitive with cmix" |
| **fx2-cmix / fx-cmix** (kaitz) | Hutter submissions, 2024 | — | 75 h to decode 1 GB |
| **paq8gen** (GotthardtZ) | Genomic fork of paq8px v200 | GPL | — |
| **tensorflow-compress** (byronknoll) | Superseded by gmix | — | A100, 45 GB |
| **lstm-compress** (byronknoll) | Shares cmix's LSTM code | GPL | 173.9 MB, 10.8 kB/s |
| **mcm, zcm, nanozip, Razor, EMMA, UDA, ZPAQ** | **development on hold / abandoned** | mixed / closed | — |

Two structural observations:

- **gmix is the most interesting thing in this lineage for us.** It replaces cmix's
  hand-specialised models with an ensemble combined by a **gated linear network**, runs on a
  single CPU thread with no GPU, needs only 3.75 GB, and makes no assumptions about byte meanings
  (unlike cmix, which hardcodes space-separated words). GLNs are, in effect, the modern theory
  of what lpaq's mixer already is: a context-selected weight vector updated by online gradient
  descent, i.e. constant-time table lookups rather than a matmul. The 2026 AITDCC challenge saw
  an entry built exactly this way — sparse GLN neurons over hashed n-gram contexts of orders
  1,2,3,4,6,8,16 plus partial-byte state, one weight per prediction, cascaded logistic mixers.
  **That is an integer-friendly, cache-friendly, deterministic architecture.** It is also, in
  narc terms, "write lpaq in Rust with a better justification". Which is fine — but do not
  mistake the framing for a speed breakthrough: gmix is 13.5 kB/s.
- **The `-Ofast` warning in cmix's README is the whole determinism story in one line.** Any
  narc CM implementation must have a test that compresses on one machine and decompresses on
  another, and must forbid `-ffast-math`-equivalent behaviour by simply having no floats in the
  prediction path.

---

## 4. The 2026 research literature on "lightweight" neural compression: uniformly worse than 2010 practice

This is the most important negative result in the document, because the papers *sound* like the
answer to narc's question and are not.

| Paper | Date | Claim | Reality check |
|---|---|---|---|
| **Chained Lightweight Neural Predictors with Information Inheritance** (arXiv 2604.15472; Kim & Belyaev, ITMO) | 2026-04 | 6 cascaded predictors (orders 1–4, 8, 16), MLP/CNN/GRU units, logit-bias inheritance; 0.02–0.82 M params/unit; "high throughput, low memory, deterministic decoding" as motivation | **enwik9 1.48 bps ≈ 185 MB**, i.e. only ~6% better than xz — at **43–244 kB/s encode / 98–429 kB/s decode on an RTX 4060 Ti**. `bsc` gets 163.9 MB at ~100× the speed on a CPU. Weights are float32, post-hoc k-means quantised; **no determinism guarantee stated**; no code |
| **StateSMix** (arXiv 2605.02904) | 2026-05 | Online Mamba SSM (DM=32, NL=2, ~120 K active params) + 9 sparse n-gram hash tables, pure C with AVX2, **no GPU**, no pretrained weights | **2.123 bpb on 1 MB of enwik8**, beating `xz -9e` by 8.7% / 5.4% / 0.7% at 1/3/10 MB — the advantage *vanishes as input grows*. **~2,000 tokens/s ≈ 2 kB/s**; OpenMP gives 1.9× on 4 cores. Precision not stated |
| **Nacrith** (arXiv 2602.19626) | 2026-02 | 135M transformer + light online predictors, CDF precision 2^16→2^24 removing "~75% of quantisation overhead" | GPU, GGUF weights uncounted (§2) |
| **STC: Reversible Digit-Context Decomposition for BWT-family text compression** (arXiv 2606.03570 v3, Du/Shen/Xiang) | 2026-08-11 | Split digit runs out of text into context-conditioned side streams before BWT | **enwik9 157,388,188** (157,571,362 with decoder) — a genuine **1.6% gain** over the no-split control, exactly reversible, deterministic. But: 694 s encode / 670 s decode, **12.5 GiB peak RAM**, and it inherits GPL from bsc-m03's model tables |
| **2026 AIT Data Compression Challenge** (arXiv 2606.17712, Ribeiro et al., 2026-06-16; aitdcc.github.io) | 2026-06 | 117 valid submissions, 16 heterogeneous files, hidden test partition, **≤8 GB RAM, decompressor ≤1 MB**, AC/range coding encouraged | Conclusion: *"performance depends strongly on the optimization criterion"* — zstd-1/brotli-1 win speed, "modelling-intensive" compressors win size at much higher cost. **No submission broke the ratio/speed trade-off.** Notable entry `G7-V10`: block-level selection among raw / BWT / LZP / x86-filtered transforms and models — i.e. narc's tournament idea, independently validated |
| **Integer-Only Discrete Flows** (arXiv 2206.08869, ICML 2022) | 2022 | int8 integer-only neural compressor, 5.9–8.7× faster, ~10× over fastest neural compressors | TensorRT **on GPU**; images only |
| **Practical Lossless Neural Compression for LiDAR** (arXiv 2603.25260) | 2026-03 | First cross-platform **integer-only** inference pipeline for neural PCC; int8 GEMM w/ int32 accumulate, fixed-point requant/activations, LUT softmax; ~14 FPS, no ratio loss vs float | Point clouds, not general data. **Valuable as a technique reference, not as a codec** |

**Pattern:** the 2026 "lightweight/practical neural" literature is competing against gzip and
xz, on 1–10 MB files, at 2–400 kB/s, and losing to a 2009 BWT compressor. The papers are honest
about their own limits; the risk is reading the abstracts and not the tables.

**Exception worth respecting:** the *techniques* for integer-only determinism (2603.25260,
2206.08869) are real and directly reusable — but the thing narc should apply them to is a
lpaq-class mixer, where the answer is trivially "already integer".

---

## 5. Small mixers that already run at MB/s: the actual state of the art

Strip away the marketing and the fast end of "neural compression" is one architecture, shipped
in five places, all integer, all deterministic:

**Integer logistic mixing.** Each of N cheap predictors emits p; `t_i = stretch(p_i) = ln(p_i/(1-p_i))`
from a lookup table; `p = squash(Σ w_i·t_i)` with int weights in fixed point; weights updated by
`w_i += lr · err · t_i` after each bit. This *is* a one-layer neural network trained online. It
is what makes paq work, and it costs a handful of integer multiply-adds per bit.

| Implementation | Contexts / structure | enwik9 | Speed | License / status |
|---|---|---|---|---|
| **lpaq1 / lpaq9m** (Mahoney, Rhatushnyak) | orders 1,2,3,4,6 + lowercase word + match model → one of 80 mixers selected by context → 2 SSE stages → AC | 143.9 MB (lpaq9m) | ~0.45–1.15 MB/s (0.45 MB/s measured on a Xeon E5-2650 in the LpaqHP FPGA paper) | research, unmaintained; ~500 lines of C++ |
| **kanzi TPAQ / TPAQX** (flanglet) | TPAQ = binary AC "based initially on Tangelo 2.4, itself derived from FPAQ8", context mixing by one-layer neural networks; TPAQX = "more predictions and more memory" | 161.7 MB (`-b 1024M -e TPAQX`) | **2.0–2.9 MB/s/core** (see §6) | **Apache-2.0, actively maintained (kanzi 2.5.0, 2,726 commits)**; C++, Java, Go — **no Rust** |
| **ZPAQ ICM/ISSE/MIX** (Mahoney) | ICM = context → bit history → probability; ISSE = bit-history-selected weight pair, `p' = squash(w1·stretch(p) + w2)`, chained to raise order; MIX = context-selected logistic mixing. Model shipped **inside the archive** as ZPAQL bytecode | 142.3 MB (max, 14 GB) | -m5 class ≈ 1 MB/s/core; the 14 GB max config is 0.15 MB/s | **libzpaq public domain**; zpaqfranz fork MIT, active |
| **mcm 0.82** (Chartier) | CM + LZP folded into the CM contexts (LZP's predicted char used as `256 + expected char` for the order-0 XOR context, not emitted as a separate token stream) | 144.9 MB | ~2.5 MB/s comp, 3.6 MB/s decomp | abandoned |
| **gmix** (Knoll) | ensemble + **gated linear network** | 122.3 MB | 13.5 kB/s | GPL, active |
| **lstm-compress** | genuine small LSTM | 173.9 MB | 10.8 kB/s | GPL |

The ranking is unambiguous: **the more the mixer looks like a real neural network, the worse the
ratio-per-second gets.** lstm-compress (real LSTM) is worse *and* 4 orders of magnitude slower
than mcm (integer mixer + match model). This is not a tooling artefact; it is that a per-bit
matmul cannot be amortised, while a context-selected weight lookup can.

**Rust availability: there is none.** No maintained crates.io crate implements PAQ/lpaq/TPAQ
context mixing. The only Rust-side asset is **`zpaq_rs`** (CC0 crate wrapping libzpaq via a
`cc`-built C++ shim, statically linked, `nojit` feature available for platforms without an x86
JIT; can drive the real zpaq JIDAC engine in-process for multi-file archives, dedup, append).
Everything else would be a port.

---

## 6. The one measurement that matters for narc: Silesia, modern hardware, 2025/2026

From the **kanzi 2.5.0 README** — `silesia.tar` = 211,957,760 bytes, **AMD Ryzen 9 9950X 16-core,
Ubuntu 25.10**, competitors run with `-T16`/`-j 16` where they support it, kanzi with its default
job count:

| Program | Encode | Decode | Size | Implied MB/s (16 threads) |
|---|---|---|---|---|
| zstd 1.5.8 -T16 -19 | 11,290 ms | 130 ms | 52,830,213 | 19 / 1,630 |
| **xz 5.8.1 -9 (1 thread)** | **43,611 ms** | **931 ms** | **48,802,580** | **4.9 / 228** |
| bsc 3.3.11 -T16 | **1,201 ms** | **698 ms** | 47,900,848 | **176 / 304** |
| kanzi -l 7 (BWT class) | 1,153 ms | 888 ms | 47,330,422 | 184 / 239 |
| bzip3 1.5.1 -j 16 | 2,348 ms | 2,218 ms | 47,260,281 | 90 / 96 |
| **kanzi -l 8 (TPAQ)** | **4,473 ms** | **4,881 ms** | **42,962,913** | **47 / 43** |
| **kanzi -l 9 (TPAQX)** | **11,618 ms** | **12,381 ms** | **41,520,670** | **18 / 17** aggregate — but only ~7 of 16 cores were busy |
| *narc max (8 cores, Win11)* | *6,800 ms* | *—* | *~49,300,000* | *~31 / —* |

**Do not divide those wall times by 16.** The kanzi README states, verbatim:

> "The default block size at level 9 is 32MB. This limits the number of threads in use,
> especially with smaller files like enwik8, but all tests below are performed with default
> values."

Two consequences, and both matter more than anything else in this document:

1. **41,520,670 is already a 32 MB-block measurement.** It needs no discount for narc's 32 MiB
   unit cap — the cap is exactly the geometry it was measured at. This is the single most
   directly transferable number in the survey.
2. **Only ~7 of the 16 cores were busy.** 211,957,760 / 32 MB = 7 blocks, one job per block. So
   per-core throughput is one 32 MB block per wall second, not 1/16th of the corpus:
   - encode: 33.5 MB / 11.618 s = **2.89 MB/s/core**
   - decode: 33.5 MB / 12.381 s = **2.71 MB/s/core**

Two independent cross-checks agree, and the first draft's own tables contain both of them:

- LTCB (§1): `kanzi -b 1024M -e TPAQX` = 490 / 480 ns/B = **2.04 / 2.08 MB/s**, single block,
  therefore single thread. 2× the first draft's figure, on older hardware.
- enwik8 on the M3 below: 100 MB / 32 MB = 4 blocks, 8,260 ms wall → **4.06 MB/s/core**.

So the defensible per-core band is **2–3 MB/s both directions**, and the first draft's
"≈1.1 MB/s/core" was low by 2.5–3×. Every projection in §10 has been recomputed.

**But note what the two cross-checks disagree about, because it is a real risk to §10's central
argument.** 4.06 MB/s/core with 4 concurrent jobs (M3) vs 2.89 MB/s/core with 7 concurrent jobs
(9950X) is the signature of **memory-bandwidth contention**: a CM codec is random access into
hash tables measured in GB (§1: kanzi TPAQX 3,100 MB), which is the worst possible cache
behaviour, and narc's whole case for tolerating a 2 MB/s codec is "we run 8 of them at once".
Per-core CM throughput under N-way parallelism must be measured, not assumed linear. Provisional
planning figure: **~2 MB/s/core at 8-way**, i.e. do not bank the 2.89.

And the corresponding enwik8 run (Apple M3, 24 GB, macOS 15.7.3): `kanzi -l 9` → **20,035,144
bytes in 8,260 ms, decode 8,760 ms**. For scale on the same file: `xz -9e` ≈ 1.99 bpb ≈ 24.9 MB,
`bsc-m03` = 20,293,393, `paq8px` ≈ 1.27 bpb ≈ 15.9 MB, cmix ≈ 1.17 bpb ≈ 14.6 MB.

### The unit-size tax, quantified

CM is often blamed for needing huge inputs. It does — but **so does everything else**, and narc
already pays this tax:

| Program | enwik8 bpb | enwik9 bpb | Gain from 10× more data |
|---|---|---|---|
| kanzi TPAQX | 1.528 | 1.294 | 15.3% |
| bsc-m03 | 1.624 | 1.282 | 21.1% |
| xz | ~1.99 | 1.579 | 20.7% |

So moving to a CM codec does **not** make narc's 32 MiB unit cap worse in relative terms. But it
also does **not fix the source-tree loss**: narc is 12 MiB vs 7-Zip's 8.8 MiB on the 114 MiB /
5751-file tree, a 36% gap. A codec swap worth 12–16% takes 12 MiB → ~10.1 MiB and still loses.
**Do not expect an ultra tier to close the source-tree gap.**

**Correction to the first draft's diagnosis, and it matters.** The first draft attributed that gap
to "7-Zip having one solid block with a 64 MiB dictionary", i.e. window size, and closed the
question as "unit geometry, not a codec problem". Doc 15 §1.2–1.4 separated the two confounded
variables by direct measurement on this exact corpus and found the window saturates at 64 MiB
(16 → 192 MiB buys 3.3%), while **unit independence alone costs 21.5%** — it is a *cold-start*
problem, and cold start is buyable: preset dictionaries taken from already-committed archive units
gave **−12.6% at depth ≤ 4 and −18.0% on an unbounded adjacent chain**, beating 7-Zip with 16 MiB
units and zero stored dictionary bytes. Do not repeat "unit geometry, unfixable"; the correct
statement is "cold start, fixable, see doc 15 §1".

Separately, the small-input warm-up penalty is real and sharp at the low end: paq8px is ~1.27 bpb
on enwik8 (100 MB) but **1.73 bpb on alice29.txt (152 KB)**, worse than cmix on the same file.
Any CM tier in narc must not be applied to small solid blocks without measuring.

**And this is the interaction the first draft missed entirely: CM's cold start is worse than
LZMA2's, so an ultra tier is penalised hardest on exactly the corpus where narc already loses.**
A CM model must learn its hash tables, its mixer weights and its SSE tables from scratch per unit,
where LZMA2 only loses its match window and its range-coder probabilities. Two consequences:

- The 41.5 MB Silesia projection will **not** scale to a tree of 5751 small files. Measure a CM
  candidate on `test/corpus` units before believing any projection there.
- The fix is the same one doc 15 found for LZMA2, applied one layer deeper: **prime the model** on
  a committed dictionary unit before coding (see §11.1). This is standard practice in the lineage
  the document surveys — paq8px ships `-T`/`-E` for exactly this — and it is missing from §10.

---

## 7. Where the remaining 40–50% actually lives — decomposed, then measured

`paq8px_v215 -12L` reaches **27,825,511 bytes on Silesia with files compressed individually**
(Mahoney, 2026-05-20). `zpaq -m7` reaches 39,112,624 and `mcm -9 -max` 40,068,926 on the same
corpus, same accounting. The first draft asserted, without decomposing it, that the gap "comes
from paq8px's specialised deterministic submodels" for 16-bit images and records, and called that
the document's highest-value finding. **Both halves of that claim are wrong.** Mahoney's table is
per file, so the claim was testable, and here it is tested.

### 7.1 The decomposition the first draft skipped

Per-file sizes in KB (Mahoney, Silesia, 2026-05-20, files compressed individually):

| File | 7z -mx=9 | mcm -9 -max | paq8px -12L | headroom vs 7z | share of total headroom |
|---|---|---|---|---|---|
| `mozilla` (exe/objects) | 13,366 | 12,238 | 6,094 | **7,272** | **34.7%** |
| `webster` (text) | 8,384 | 5,791 | 4,401 | 3,983 | 19.0% |
| `samba` (source+objects) | 3,764 | 3,076 | 1,587 | **2,177** | **10.4%** |
| `ooffice` (exe) | 2,426 | 1,724 | 1,212 | 1,214 | 5.8% |
| `nci` (structured text) | 1,738 | 1,234 | 776 | 962 | 4.6% |
| `dickens` (text) | 2,830 | 2,181 | 1,860 | 970 | 4.6% |
| `x-ray` (16-bit image) | 4,486 | 3,680 | 3,503 | 983 | 4.7% |
| `mr` (16-bit image) | 2,749 | 2,154 | 1,750 | 999 | 4.8% |
| `osdb` (records) | 2,849 | 2,214 | 1,969 | 880 | 4.2% |
| `sao` (records) | 4,423 | **4,483** | 3,723 | 700 | 3.3% |
| `reymont` (Polish text) | 1,317 | 965 | 699 | 618 | 2.9% |
| `xml` | 454 | 323 | 245 | 209 | 1.0% |
| **total** | **48,793** | **40,069** | **27,826** | **20,967** | 100% |

- **Executables and binary containers are 50.9% of the headroom** (`mozilla`+`samba`+`ooffice`).
- **Plain and structured text is 32.1%** (`webster`+`dickens`+`nci`+`reymont`+`xml`).
- **The four files the first draft built its recommendation on are 17.0%** — and against the
  generic-CM comparator, only 13%: paq8px beats `mcm` by just **4.8% on `x-ray`**, the flagship
  16-bit image, even though `mcm` has no image model at all. Generic CM captures 82% of `x-ray`'s
  headroom; the image-specific model captures 18%.
- **The one file that genuinely vindicates a record model is `sao`**, where generic CM is *worse
  than 7-Zip* (4,483 vs 4,423) and paq8px is 15.8% better. Structure there is invisible to both LZ
  and generic CM, and visible only to an explicit column model.
- **Part of the gap is not a model at all.** `-12` is paq8px's maximum memory level (~29 GB) and
  `-L` enables its **LSTM submodel** (paq8px README). This document's §10 says "never a float in
  the prediction path"; the number it uses to justify its top recommendation was produced with
  one. Attributing the gap requires a model-ablation run (paq8px with individual models disabled),
  which nobody has published. Until then, treat the split above as the only evidence available.

### 7.2 The proposed implementation, measured — and it mostly fails

§10 proposed shipping these as *reversible filters in the existing tournament* (narc has no CM
codec to plug a context source into, so filters are the only shippable form). That is a different
mechanism from paq8px's, so it was measured directly. Filters: stride detection by byte-position
autocorrelation (which works — it recovers `sao`'s 28-byte records and the 16-bit stride of
`x-ray`/`mr` on the first try), then transpose and/or delta, then `xz --format=raw -T1
--lzma2=preset=9e`.

| File | LZMA2 baseline | best cheap filter | Δ | paq8px | filter's share of the LZMA2→paq8px gap |
|---|---|---|---|---|---|
| `x-ray` | 4,491,203 | plane-split + delta1 → 3,999,186 | **−10.96%** | 3,503,000 | **50%** |
| `mr` | 2,751,831 | plane-split + delta1 → 2,473,757 | **−10.11%** | 1,750,000 | **28%** |
| `sao` | 4,425,604 | transpose28 → 4,596,475 | **+3.86%** | 3,723,000 | **0%** |
| `osdb` | 2,844,493 | delta1 → 3,319,742 | **+16.71%** | 1,969,000 | **0%** |

(MEASURED HERE, 2026-08-17. `sao` also tried `delta28` → +6.96% and `transpose28+delta1` →
+12.51%; `osdb` also tried `transpose8` → +113% and `transpose16` → +156%. Baselines land within
0.2% of Mahoney's `7z -mx=9` column, which validates the harness.)

**Conclusions, replacing the first draft's:**

1. **Byte-plane split + delta on detected 16-bit data is real and cheap: ship it.** ~10% on both
   16-bit files, ~770 KB total, **≈1.0% of a 48.8 MB Silesia archive**. Free at decode, exactly
   reversible, ~150 lines, and the tournament already guards against false positives.
2. **A record/table model is not a filter.** Every natural transform form of it (transpose by
   stride, delta by stride, both) makes LZMA2 *worse* on the two record files, by 4–17%. paq8px's
   15.8% on `sao` is a context-mixing gain that exists only inside a mixer. This item therefore
   **cannot ship ahead of a CM codec** — it is a sub-item of §10's step 4, not a standalone
   "ship-now", and its value is bounded by `sao`+`osdb` = 1,580 KB = 3.2% of Silesia *if* a CM
   tier exists to host it.
3. **The first draft aimed at 17% of the headroom and missed the 51%.** Half the available ratio
   on Silesia is in `mozilla`, `samba` and `ooffice` — executables, object files and binary
   containers, where paq8px beats 7-Zip by 54%, 58% and 50%. That is the target, and doc 04
   already has the shippable form of it (BCJ2, dispack-class disassembly filters, per-section
   splitting). Cross-reference it instead of re-deriving.
4. Expected total from this whole family, honestly: **1.0% of Silesia now** (16-bit filter),
   ~3% more only after a CM tier exists, and **0% on the source tree**, which is where narc
   actually loses.

The 2026 AITDCC's noted entry `G7-V10` (block-level selection among raw, BWT, LZP and x86-filtered
transforms) does validate the *tournament* frame — but note that the corroboration is for
per-block method selection, which narc already has, not for the submodels. The paper's abstract
(verified 2026-08-17) supports only the general conclusion that "performance depends strongly on
the optimization criterion"; the `G7-V10` detail is a body claim and was not re-verified.

---

## 8. Hybrids: LZ output re-modelled, and CM as a fallback

The owner's question — has anyone measured mixing a strong LZ codec with a light CM stage? —
splits into three distinct things, with three different answers.

**(a) "Re-model an existing LZMA stream with CM."** No public measurement found. And the framing
is a trap: to re-model LZMA's output you must reimplement LZMA's own probability model exactly
(that is what recompression tools like precomp/reflate do for deflate — see doc 02), then improve
on it. The gain would be bounded by what LZMA's order-1 literal coder loses, and you pay a full
extra modelling pass. **Do not pursue this framing.**

**(b) "LZ + CM in one codec."** This *is* measured, and it is the standard design: a **match
model** as one predictor inside the CM ensemble, plus LZP folding. Evidence:
- `mcm 0.82` folds LZP into the CM contexts rather than emitting LZ77 tokens (the predicted
  character enters as `256 + expected char` in the order-0 XOR context) → **enwik9 144.9 MB at
  ~2.5 MB/s**, Silesia 40.1 MB. That is the best open ratio/speed point ever measured for this
  design.
- lpaq1 and ZPAQ's `mid`/`max` configs both include a MATCH component at order 6–7 alongside
  ICM/ISSE chains.
- `paq9a` does explicit LZP precompression then CM → 165.2 MB (worse; the separated design
  loses).
- The closed proprietary instances that beat LZMA by 5–10% at usable speed — **Razor (rz),
  NanoZip -cc/-nz, CCM** — are all **abandoned and closed source** (nanozip -cc: 148.5 MB, 0.87
  MB/s). There is nothing to adopt.

**Conclusion: "LZ + light CM" and "CM with a match model" are the same thing, and the match model
is mandatory, not optional.** Any narc CM codec must have one.

**(c) "CM fallback for units where LZ does badly."** This is free for narc — the per-unit codec
tournament already exists. The work is not plumbing; it is (i) a cheap predictor of when CM will
win so the tournament does not have to actually run a 1 MB/s codec on every unit, and (ii) the
memory/tier policy. A good cheap predictor: run the existing LZMA2 and PPMd7 candidates, and only
try CM when PPMd7 beat LZMA2 (PPMd winning is a strong signal of a statistical, non-repetitive
unit) or when the LZMA2 ratio is poor in absolute terms.

**(d) One adjacent non-CM finding that outranks most of this document.** `bsc 3.3.11` compressed
Silesia to **47,900,848 bytes in 1.2 s / 0.7 s on 16 cores** — better ratio than 7-Zip -mx9 and
xz -9, roughly **36× faster to compress than xz**. libbsc and libsais are **Apache-2.0** and
Grebnov maintains both. If narc's goal were "beat 7-Zip on ratio *and* speed" rather than "beat
paq", a BWT tier is a far cheaper move than a CM tier. It is not neural and not in scope for this
document, but it should not be missed because the question was framed around neural work.

*Review note:* two things to carry rather than re-derive. (i) On Silesia a BWT tier is worth only
~2% over `xz -9` (47.9 vs 48.8 MB); its real prize is text, where LTCB puts `bsc` at 163.9 MB vs
`xz`'s 197.3 MB — **17%, at 8 ns/B decode** — so it belongs in the tournament as a data-dependent
entry, exactly like PPMd7. (ii) It is not free of the bounded-memory constraint either: the inverse
BWT needs the whole block plus its index resident, several times the unit size, so it lands in the
same "declare peak decode memory in the unit header" bucket as CM. Doc 15 §5 has the full
treatment and rates it **prototype**; defer to it.

---

## 9. Preprocessing: DRT / word-replacement dictionaries

Measured, and the result is counter-intuitive in a useful way:

| Back end | Without DRT | With DRT | Gain |
|---|---|---|---|
| `ppmonstr J -m1650 -o64` (enwik8) | 19,098,634 | 18,120,770 – 18,122,785 | **~5.1%** |
| `phda9 1.6` (enwik8) | 15,040,647 | 15,015,895 | 0.16% |
| `cmix` (enwik8) | 14,965,334 | 14,953,755 | 0.08% |
| `cmix9` with English dictionary already enabled | 15,627,679 | 15,626,305 | 0.009% |

DRT shrinks the enwik8 byte stream itself by 5–8% (60,520,510 → 55,852,090 in phda9's pipeline),
but the final-size gain **collapses to ~0.1% once the back end has its own word model**.

**Implication for narc, which uses PPMd7:** a DRT/WRT-style transform is worth ~5% on English
text units *today*, before any CM work. Costs: a ~200–500 KB dictionary shipped in the binary (and
charged against the gain if you care about self-containment), English-only benefit, and a transform
that must be exactly reversible on arbitrary bytes. And if narc later ships a CM codec with a word
model, this gain mostly evaporates — so build it as a transform in the tournament, not as an
always-on stage.

**Review: downgraded from "ship-now" to "watch". Four problems, any one of which is enough.**

1. **The 5.1% is not measured with anything resembling narc's back end.** It is `ppmonstr J
   -m1650 -o64`: order **64**, 1.65 GB, and PPMonstr rather than PPMd (§11.3 — different program,
   materially stronger model). narc runs PPMd7 at orders 10/16 under a bounded memory budget. The
   transfer is plausible in direction and completely unquantified in size. The table's own trend is
   the warning: the gain falls from 5.1% (ppmonstr) to 0.16% (phda9) to 0.08% (cmix) as the back
   end's own text modelling improves.
2. **It buys nothing on either corpus narc is judged on.** Silesia is ~50 MB of prose out of 202;
   the source tree is identifiers, punctuation and paths, where a natural-language dictionary is a
   poor fit. The 5% is a 5% of a fraction.
3. **A measured alternative strictly dominates it.** Doc 15 §1.4: preset dictionaries drawn from
   already-committed archive units give **−12.6% (depth ≤ 4) to −18% (adjacent chain)** on the
   source tree, cost **zero stored dictionary bytes**, are language-independent, and need no
   dictionary trainer — doc 15 explicitly measured the synthetic-dictionary variant as the *worst*
   option because you must store the sample. A shipped English `.dic` is the same idea in its
   weakest form: fixed, language-locked, and charged against the gain.
4. **Licensing is not free.** The reference implementations are contaminated: XWRT bundles lpaq6
   (PAQ lineage, GPL-derived) alongside LZMA and PPMVC, so the combined work is effectively GPL,
   and its `wrt-eng.dic` cannot simply be vendored into an Apache/MIT project without provenance
   work. Building a clean dictionary from a permissively-licensed corpus is extra work that the
   first draft's "per line of code" framing did not count.

**Do this instead:** implement doc 15's preset-dictionary field first. If, after that, English text
units are still a measurable share of real archives, revisit a word transform whose dictionary is
*built from the archive's own data and stored as a unit* — which is the preset-dictionary mechanism
again, not a new one.

---

## 10. What narc should realistically adopt as an optional "ultra" tier

### Honest projected numbers

Baseline: narc max = ~49.3 MB on Silesia, 6.8 s compress on 8 cores (~31 MB/s aggregate).

| Ultra tier design | Projected Silesia size | Projected compress (8 cores) | Projected decompress (8 cores) | Memory per worker |
|---|---|---|---|---|
| lpaq/TPAQ-class integer CM, 32 MiB units, match model, 6–8 ICM/ISSE contexts | **41.5–43 MB** (−13…−16%) | ~16 MB/s → **~13 s** (2× slower) | ~16 MB/s → **~13 s** | **1–3 GB, not 64–256 MB** |
| + record/table model + 16-bit image/audio submodels | **39–41.5 MB** (−16…−21%) | ~14 MB/s → ~15 s | ~14 MB/s → ~15 s | 1–3 GB |
| Table-shrunk variant that actually fits 64–256 MB/worker | **unmeasured — do not plan on it** | faster | faster | 64–256 MB |

Derivation, corrected (§6): kanzi -l 9 reaches 41,520,670 on Silesia **at a 32 MB default block
size** — narc's own unit granularity, so no discount for the unit cap is warranted; the ratio band
is 41.5 MB plus whatever a first-generation Rust implementation gives away against 2,726 commits of
tuning, call it 41.5–43 MB. Per-core throughput is 2.7–2.9 MB/s on a 9950X and 2.0 MB/s on LTCB's
older single-threaded run; planning at **2 MB/s/core at 8-way parallelism** to leave room for
memory-bandwidth contention gives 8 × 2 = 16 MB/s aggregate → ~13 s for 202 MiB, about **2× slower
than narc max today**, not 3.5×.

**The architectural point that makes this viable at all:** narc compresses units in parallel, so
a codec that is 2 MB/s single-thread is ~16 MB/s aggregate on the owner's 8-core box. The
"≥10 MB/s single-thread" bar in the original question is the wrong bar for narc's architecture.
The right bar is **≥1 MB/s single-thread with per-unit independence**, and that bar is met by
integer CM today.

**Three caveats on that argument, none of which the first draft stated.**

- **The parallelism is capped by unit count, not core count.** 202 MiB / 32 MiB = 7 units, so on
  the owner's 8-core box the ultra tier can never use more than 7 threads on this corpus, and small
  archives get proportionally worse. This is the same effect that made kanzi idle 9 of 16 cores.
- **Per-core CM throughput under N-way parallelism is unmeasured** and the two available data
  points (4.06 MB/s at 4-way on M3, 2.89 MB/s at 7-way on 9950X) suggest it degrades. A codec whose
  working set is 1–3 GB of hash tables does not scale like LZMA2.
- **The decompression baseline it is being compared against was never measured.** "LZMA2's
  ~200+ MB/s" traces to the kanzi README's `xz -9` decode row (211,957,760 B / 931 ms = 228 MB/s),
  which is implausibly fast for single-threaded LZMA2 decode and is probably measuring a
  multi-block stream and/or warm cache. **Measure narc's own extraction throughput before quoting
  any slowdown factor.** The honest statement today is "decode drops to ~16 MB/s aggregate, from an
  unmeasured baseline that is at least an order of magnitude higher".

### Hard constraints: which ones this breaks

| narc constraint | Status under an ultra CM tier |
|---|---|
| Editing one file stays cheap; append-only; no full repack | **Preserved.** CM runs per 32 MiB unit, independently, no cross-unit state |
| Extraction in bounded memory, 10–80 MiB today | **BROKEN, and worse than the first draft admitted.** The first draft said "64–256 MB per decode thread"; this document's own LTCB table lists **kanzi TPAQX at 3,100 MB, lpaq9m at 1,542 MB, mcm at 5,961 MB, zcm at 3,100 MB** — every CM in the table is 1.5–6 GB. Those are the configurations the quoted ratios came from. So the real figure is **1–3 GB per decode thread, a 20–40× breach of today's budget**, and 8-way parallel decode would want 8–24 GB. A 64–256 MB table-shrunk configuration is a *different operating point whose ratio nobody has measured* — it is not free, and the 41.5 MB number does not travel with it. Measure the ratio-vs-table-size curve first; it may be the fact that kills the tier |
| Exactly reversible, bit for bit | **Preserved, if and only if the prediction path is integer-only.** No floats anywhere. Add a cross-machine round-trip test (different CPU vendor, different SIMD width, `-C target-cpu=native` vs baseline). Note: integer SIMD is *not* a hazard here and is worth 2–4× — see §11.2 |
| Decompression speed matters | **Degraded ~10×+, and latency degraded ~10×+ too.** ~16 MB/s aggregate against an unmeasured LZMA2 baseline. The first draft's "3–5×" understated it and, more importantly, missed **selective-extract latency**: pulling one 4 KB file out of a 32 MiB CM unit costs a full single-threaded unit decode, ≈16 s at 2 MB/s, versus well under a second today. That attacks narc's core value proposition (cheap random access), not just its throughput, and it compounds with doc 15's dictionary chains (× chain depth). Acceptable only as an opt-in tier, never as a default, and the UI must warn at *create* time that this archive will be slow to browse |
| Pure Rust preferred, FFI OK, no GPL, no blobs | **Satisfiable.** Port from Apache-2.0 kanzi / public-domain libzpaq. **Do the licence due diligence:** kanzi's wiki states TPAQ is *"based initially on Tangelo 2.4"*, itself *"derived from FPAQ8"* — trace that chain before vendoring code, since fpaq8/Tangelo ancestry may not be Apache-2.0. Reimplementing from the published algorithm is the safe path |
| Zero telemetry, offline, consumer hardware, GPU optional | **Preserved.** No GPU, no network, no model download |

### Format invariants the tier must add

1. **Pin the model in the unit header**, ZPAQ-style: number and type of components, context orders,
   hash table sizes, mixer learning rates, SSE table sizes. narc already pins chunk geometry to
   the archive; a CM model's table size changes its predictions, so it is geometry too. Without
   this, a future narc build with a different default table size cannot decode old archives.
2. **Version the codec ID separately from the tier name.** "ultra" is a UI concept; the codec ID
   is a format promise.
3. **Declare peak decode memory per unit in the unit header** so the extractor can schedule
   threads within a user-set memory budget, and refuse-with-a-message rather than OOM on a weak PC.

### Recommended order of work

Revised after the review pass. The first draft's steps 1 and 2 were the two items that did not
survive; the measurement step moved to the front because it is now nearly free.

1. **Measure, with a CLI, before writing any Rust.** `kanzi -l 9` already uses 32 MB blocks by
   default, so `kanzi -l 9` (or `-b 32m` explicitly) on narc's own corpora *is* the experiment —
   an afternoon of shell scripting, no FFI, no feature flag, Apache-2.0, actively maintained.
   Required numbers: ratio and wall time on `test/corpus` (the source tree, where CM's cold start
   should hurt most), per-core throughput at 1/2/4/8-way parallelism, and peak RSS per process.
   **Do not use `zpaq_rs` for this.** It is five months old (1.0.4, 2026-02-27), 22 commits,
   ~1.8 k downloads, one author, no releases since February — and its built-in method levels stop
   at `-m5`, so it cannot reproduce the `-m7` number quoted in §0 without hand-written ZPAQL config
   files. It buys FFI risk to measure a weaker configuration. (Licence, at least, is clean: zpaq
   7.01+ is public domain; the GPL-3 that covered `zpaq.cpp` applies only to ≤ 7.00.)
2. **Byte-plane split + delta filter for detected 16-bit data**, in the existing tournament.
   MEASURED HERE at −10…−11% on `x-ray` and `mr` (§7.2), ≈1.0% of Silesia, free at decode,
   ~150 lines. This is the only part of the first draft's step 1 that works.
3. **Executable and object-file modelling**, per doc 04. §7.1 shows this is 51% of the paq8px
   headroom on Silesia — five times the record/image files combined — and doc 04 already has the
   shippable plan (BCJ2, dispack-class filters, section splitting).
4. **The ratio-vs-model-memory curve**, before committing to a tier. Run `kanzi -l 8` (TPAQ, less
   memory) against `-l 9` (TPAQX) on narc's corpora and, if possible, an lpaq-class build at
   several table sizes. If a 256 MB-per-thread configuration gives up most of the 16%, the tier is
   dead and steps 5–6 should not happen.
5. **Model priming** for whichever back end wins, using doc 15's committed-dictionary-unit
   mechanism (§11.1). This is the item that decides whether an ultra tier helps the source tree at
   all, and it is cheaper than the codec itself.
6. **Only then**: a Rust integer CM codec — order 1/2/3/4/6 ICM + ISSE chain + match model +
   word model + 2 SSE stages + context-selected logistic mixer, all int, integer-SIMD mixer
   (§11.2), ~1,500–3,000 lines, plus a lot of tuning. Budget the tuning honestly; kanzi has 2,726
   commits. Fold the record/column model in here — §7.2 shows it does not work anywhere else.
7. **Cheaper and skipped entirely by the first draft:** before any of 4–6, try **PPMd7 →
   secondary estimation** (§11.3). Same model family narc already ships, published algorithm, no
   new format tier, and the prize is plausibly half the CM tier's at a fraction of the cost.
8. **Never**: anything with a matmul, a GPU, a model file, or a float in the prediction path.

---

## 11. What this survey missed (added by the review pass)

Ordered by how much each one changes the plan. Items 1–3 are cheap and directly affect the ultra
tier's viability; 4–6 are corrections of framing; 7–8 are gaps in the evidence base.

### 11.1 Model priming — the biggest omission

The document quotes CM's warm-up penalty (paq8px: 1.27 bpb on enwik8 vs **1.73 bpb on alice29.txt**)
and then, two sections later, declares the source-tree gap unfixable by a codec. The standard fix
for both facts is the same and it is missing: **prime the model on a shared prefix before coding,
and do not emit the prefix.** This is not speculative — it is shipped in the very lineage under
review. paq8px's README documents `-T`, which pre-trains the text models from `english.dic` /
`english.exp`, and `-E`, which pre-trains the EXE model from the paq8px binary itself.

The critical design lesson is the *warning* attached to those flags, and it points straight at
narc's format rules. paq8px states the training files are "used only to pre-train models before
compression" and are not stored in the archive, and that an archive made with `-E` will differ if a
different `paq8px.exe` is used. **That is exactly the failure mode narc must not adopt**: an archive
whose decodability depends on an external file or a specific binary is not an archive. The narc-safe
form is doc 15's: the priming source must be an **already-committed unit inside the archive**,
referenced by chunk id, with a depth cap. Then priming costs zero stored bytes, is language- and
corpus-independent, and preserves append-only and cheap edits.

Priming applies to all three back ends narc has or might have — LZMA2 (measured, doc 15 §1.4,
−12.6…−18% on the source tree), PPMd7 (feed the dictionary through the model, suppress output;
no format change beyond the reference), and a CM tier (feed the bits, update all tables and mixer
weights, emit nothing). For CM the effect should be *larger* than for LZMA2, because CM has strictly
more state to warm up. **This is the missing link between doc 14 and doc 15, and it is the only
mechanism found in either that attacks the source-tree loss and the CM cold-start penalty at once.**

### 11.2 Integer SIMD in the mixer is safe, and worth 2–4× on the dominant cost

The document treats SIMD purely as a determinism hazard, warns about `-Ofast`/`-march=native`, and
proposes a test matrix of "different SIMD width, `-C target-cpu=native` vs baseline". That is
correct **for floating point and only for floating point**. Integer SIMD is bit-exact by
definition: `_mm256_madd_epi16` and friends have no reassociation freedom and no rounding mode.
The mixer's dot product and weight update — the hot loop of the whole codec, executed once per bit
per model — vectorise exactly.

The proof is in a program this document already cites: paq8px "supports SSE2, SSE3, SSE41, AVX2,
AVX512 and ARM NEON", detects "the highest usable SIMD instruction set … automatically", and states
that only runtime, not compression ratio or memory use, depends on hardware. So the lineage's
reference implementation already does the thing the first draft's §10 gate would forbid. Taking the
lesson: **forbid floats, not SIMD.** The round-trip test should compare a baseline build against an
AVX2/AVX-512/NEON build and assert byte-identical output — as a guard, not as a reason to avoid
vectorising. On the corrected speed numbers (§6) this is the difference between ~2 MB/s/core and
possibly 4–6 MB/s/core, which is most of the objection to the tier.

### 11.3 PPMd7 → secondary estimation: the cheap experiment nobody proposed

The document's own §9 table uses `ppmonstr J` as a back end and its §1 table lists `ppmonstr J
-o16` — without ever noting what PPMonstr *is*. PPMd and PPMonstr were released together (var. I,
April 2002) as two variants of the **same** PPMII model from Shkarin's DCC 2002 paper "PPM: one step
to practicality": PPMd is the speed-oriented variant (the one 7-Zip adopted, and the one narc
inherits), PPMonstr is the maximum-ratio variant, and the principal difference is that **PPMonstr
applies secondary estimation (SSE-style probability refinement) far more aggressively**, trading
speed for ratio.

Why this matters more than a new codec tier: narc already ships the PPMII model. Adding a secondary
estimation stage over it is an upgrade *within an existing tournament entry* — published algorithm,
integer, deterministic, no GPU, no new memory class, no new format geometry beyond a codec ID, and
it reuses code that already exists and is already tested. The document's own numbers hint at the
size of the prize (`ppmonstr J -m1650 -o64` = 19,098,634 on enwik8 vs 7-Zip-class PPMd in the low
21 MB range), i.e. plausibly **~10% on text at ~2× slower**, versus the CM tier's 16% at ~10×
slower decode and 20–40× the memory. **Measure narc's PPMd7 against `ppmonstr` on the same files
before spending weeks on CM.** Caveat to respect: PPMonstr itself is a closed binary; only the
algorithm is published (Shkarin's paper, mirrored at ctxmodel.net), with PPMd var. I sources
circulating (e.g. `cielavenir/ppmdj1`). Provenance of any borrowed code needs checking.

### 11.4 Selective-extract latency, not just memory

Covered in the constraints table above, but it belongs here as a category the survey lacked: the
document reasons about throughput and memory and never about **latency for a single small file**.
For an archiver whose differentiator is cheap edits and browsing, a 16-second single-core decode to
read one 4 KB file is a product-level regression, and it is invisible in every benchmark cited.

### 11.5 The dedup side of the source-tree gap is not mentioned at all

The document frames narc's 36% source-tree loss as codec-or-geometry. A 2026 storage researcher
would ask a third question first: **are near-duplicate chunks being delta-compressed?** That
literature is mature and directly applicable to an append-only, content-defined-chunked archive:
Finesse (FAST '19), Odess (super-feature sketching by content-defined sampling, ~31× faster
resemblance detection than N-Transform at equal ratio), Palantir (ASPLOS '24, hierarchical
super-features, +7.3% total data reduction over N-Transform and Odess), Argus (ACM TOS 2025, up to
2.29× higher delta-compression ratio than Finesse/Odess/N-Transform). Doc 15 §7 already carries this
as a prototype; doc 14 should reference it rather than presenting codec choice as the only lever.

### 11.6 The unit-size cap deserves one sentence of retirement, not repetition

The document repeats "our unit is capped at 32 MiB" as an explanation. In an append-only design an
edit *appends* a new unit and marks the old one dead, so a larger solid block costs space
amplification until `compact`, not a repack — the cap is a policy choice, not a hard constraint.
Doc 15 §1.3 measured the payoff and it is small: 32 MiB → 64 MiB units buys 1.6%. So the correct
disposition is "measured, retired", which is more useful than an unquantified excuse.

### 11.7 Two cited benchmarks were never actually used

- **GDCC (4th edition, results 2025-05-15, €77,500)** appears in Sources and contributes nothing to
  the analysis, despite its speed classes (rapid / balanced / HCR) bracketing narc's exact operating
  point and its winners being the practical state of the art rather than the paq end of the scale.
  Attempted retrieval on 2026-08-17 failed (`gdcc.tech` refused the connection), which is worth
  recording rather than papering over; the leaderboard notation is documented (GP = grand prize,
  NS = non-student, 1–2 = first-to-second-place gap prize) but the winner list was not obtained.
  **Open action: retrieve `gdcc.tech/results/` and extract the balanced/HCR winners and methods.**
- **AITDCC** contributes one abstract quotation. Its regime (≤ 8 GB RAM, decompressor ≤ 1 MB, 117
  submissions, hidden test partition, 16 heterogeneous files) is the closest published match to
  narc's constraints of anything in this document, and its per-file leaderboard would answer
  "which cheap transforms actually pay on heterogeneous data" better than Silesia does.

### 11.8 No ablation discipline anywhere in the evidence base

The document's central recommendation rested on attributing a 12 MB gap to a named cause without
decomposing it, when the decomposition was one column of arithmetic away in the source table it
already cited (§7.1), and the configuration it cited had a float LSTM enabled while the
recommendation forbade floats. The methodological rule to carry forward: **before attributing a
compression gap to a mechanism, either decompose it per file or ablate the mechanism.** Both were
available here and neither was done.

---

## Negative knowledge from the review pass (do not retry)

- **Record/table transforms in front of LZMA2 lose.** MEASURED HERE on Silesia: `sao`
  transpose-28 +3.9%, delta-28 +7.0%, transpose+delta +12.5%; `osdb` delta-1 +16.7%,
  transpose-8 +113%, transpose-16 +156%. Column structure is exploitable as a *context*, not as a
  reordering. Do not implement a record filter without a CM codec to host it.
- **Byte-plane split + delta on 16-bit data wins and is the cheapest confirmed ratio item found:**
  −11.0% on `x-ray`, −10.1% on `mr` (MEASURED HERE). Stride detection by byte-position
  autocorrelation is reliable — it recovered 28 for `sao` and 2 for `x-ray`/`mr` on the first pass.
- **Do not discount CM ratio projections for narc's 32 MiB units.** kanzi -l 9's default block size
  *is* 32 MB; its 41,520,670 on Silesia is already the right geometry.
- **Do not divide multi-threaded benchmark wall times by core count** when the block size limits
  the job count. That error made CM look 2.5–3× slower than it is.
- **Do not plan a CM tier at 64–256 MB per thread and expect the published ratios.** Every CM in
  the LTCB table that achieves them uses 1.5–6 GB.
- **`zpaq_rs` is not the right measurement shortcut** (five months old, 22 commits, `-m5` ceiling,
  FFI). Use the `kanzi` CLI, which already defaults to 32 MB blocks.
- **A shipped English dictionary transform (DRT/WRT) is dominated** by preset dictionaries built
  from committed archive units (doc 15 §1.4), and its reference implementations are GPL-contaminated.

---

## Sources

- Large Text Compression Benchmark, Matt Mahoney — last update 2026-07-08: https://www.mattmahoney.net/dc/text.html
- Silesia benchmark, Matt Mahoney — last update 2026-05-20: https://www.mattmahoney.net/dc/silesia.html
- Hutter Prize: http://prize.hutter1.net/ · https://en.wikipedia.org/wiki/Hutter_Prize
- fx2-cmix (Hutter record, 2024-09-03): https://github.com/kaitz/fx2-cmix
- cmix v21 (GPL-3, `-Ofast` FP-portability warning): https://github.com/byronknoll/cmix/blob/master/README
- gmix (GLN ensemble, single CPU thread, GPL): https://github.com/byronknoll/gmix
- lstm-compress / tensorflow-compress: https://github.com/byronknoll/tensorflow-compress
- paq8px (GPL-2+, v215 in 2026): https://github.com/hxim/paq8px
- NNCP v3.3, 2024-06-05 (MIT source, binary-only libnc, 2.8 days for enwik9 on RTX 4090): https://bellard.org/nncp/
- ts_zip, 2024-03-02 (RWKV 169M, ≤1 MB/s on RTX 4090, claims hardware-independent determinism): https://bellard.org/ts_zip/
- Delétang et al., "Language Modeling Is Compression", arXiv 2309.10668 (ICLR 2024): https://arxiv.org/abs/2309.10668
- LLMZip, arXiv 2306.04050: https://arxiv.org/pdf/2306.04050
- FineZip, arXiv 2409.17141 (4 h / 10 MB): https://arxiv.org/abs/2409.17141
- llama-zip: https://github.com/AlexBuz/llama-zip
- Nacrith, arXiv 2602.19626 (2026-02): https://arxiv.org/pdf/2602.19626
- Kim & Belyaev, "Chained Lightweight Neural Predictors", arXiv 2604.15472 (2026-04): https://arxiv.org/html/2604.15472
- StateSMix, arXiv 2605.02904 (2026-05): https://arxiv.org/html/2605.02904
- STC (BWT-family digit-context decomposition), arXiv 2606.03570 v3 (2026-08-11): https://arxiv.org/html/2606.03570 · code: https://github.com/thu-nmrc/STC-for-BWT-FamilyText-Compression
- 2026 AIT Data Compression Challenge, arXiv 2606.17712 (2026-06-16): https://arxiv.org/abs/2606.17712 · https://aitdcc.github.io
- "Towards Practical Lossless Neural Compression for LiDAR Point Clouds" (integer-only cross-platform pipeline; RTX 4090→5880 decode failure), arXiv 2603.25260 (2026-03): https://arxiv.org/html/2603.25260v1
- Integer-Only Discrete Flows, arXiv 2206.08869 (ICML 2022): https://arxiv.org/abs/2206.08869
- kanzi-cpp 2.5.0 (Apache-2.0) + Silesia/enwik8 benchmarks: https://github.com/flanglet/kanzi-cpp · wiki: https://github.com/flanglet/kanzi-cpp/wiki/Main-page
- bsc-m03 (GPL, v0.5.5 2024-05-08): https://github.com/IlyaGrebnov/bsc-m03
- bzip3 (LGPL-3; libsais/LZP Apache-2.0): https://github.com/kspalaiologos/bzip3
- ZPAQ specification and algorithm (public domain libzpaq; ZPAQL integer VM): https://mattmahoney.net/dc/zpaq.html · https://mattmahoney.net/dc/zpaq201.pdf · https://mattmahoney.net/dc/zpaq_compression.pdf
- zpaqfranz (MIT fork, append-only, dedup): https://github.com/fcorbelli/zpaqfranz
- zpaq_rs (CC0 Rust bindings to libzpaq): https://crates.io/crates/zpaq_rs
- LpaqHP FPGA accelerator (lpaq at 0.45 MB/s on Xeon E5-2650), ACM 2024: https://dl.acm.org/doi/fullHtml/10.1145/3673038.3673051
- DRT / dictionary-transform measurements: https://encode.su/threads/2858-Hutter-Prize-4-17-improvement-is-here/page3 · https://encode.su/threads/3586-Towards-optimal-dictionary-transforms
- MCM LZP+CM design notes: https://encode.su/threads/2127-MCM-LZP
- Benchmarking compression programs, MaskRay, 2025-08-31: https://maskray.me/blog/2025-08-31-benchmarking-compression-programs
- Global Data Compression Competition, 4th edition 2025 (results 2025-05-15, €77,500 pool, rapid/balanced/HCR speed classes): https://gdcc.tech/results/
- Abandonment status of EMMA, MCM, NanoZip, PackJPG, Razor, UDA, ZCM, ZPAQ: https://encode.su/threads/3575-Packers-in-active-development

### Added by the review pass (all retrieved 2026-08-17)

- kanzi-cpp README — "The default block size at level 9 is 32MB. This limits the number of threads in use…", plus the Silesia (Ryzen 9 9950X / Ubuntu 25.10) and enwik8 (Apple M3 / macOS 15.7.3) tables: https://raw.githubusercontent.com/flanglet/kanzi-cpp/master/README.md
- Mahoney Silesia per-file columns used for the §7.1 decomposition (paq8px_v215 -12L, zpaq 6.21 -m7 = 39,112,624, mcm 0.82 -9 -max, 7z -mx=9; "files are compressed individually", page dated 2026-05-20): https://www.mattmahoney.net/dc/silesia.html
- paq8px README — `-T` pre-trains text models from `english.dic`/`english.exp`, `-E` pre-trains the EXE model from the binary itself (and archives differ if the binary differs), `-L` = LSTM submodel, SIMD SSE2→AVX512/NEON auto-detected with ratio hardware-independent; GPL-2, v216: https://github.com/hxim/paq8px
- 2026 AITDCC, arXiv 2606.17712 — abstract verified: 117 valid submissions, ≤8 GB RAM, decompressor ≤1 MB, "performance depends strongly on the optimization criterion": https://arxiv.org/abs/2606.17712
- zpaq licence history — `zpaq.cpp` GPL-3 for ≤ 7.00, public domain from 7.01 (2015-02-09); libzpaq always public domain: https://mattmahoney.net/dc/zpaq.html
- zpaq_rs 1.0.4 published 2026-02-27, CC0-1.0, 22 commits, ~1.8 k downloads: https://crates.io/crates/zpaq_rs · https://github.com/turtle261/zpaq-rs
- Shkarin, "PPM: one step to practicality", DCC 2002 (PPMII; PPMd = speed variant, PPMonstr = ratio variant with aggressive secondary estimation): https://ieeexplore.ieee.org/document/999958/ · PDF mirror http://ctxmodel.net/files/PPMd/ShkarinPPMII.pdf · PPMd var. J1 sources https://github.com/cielavenir/ppmdj1
- XWRT (Skibiński) — bundles zlib, LZMA, PPMVC and lpaq6, so the combined work is GPL-derived; ships `wrt-eng.dic`: https://github.com/inikep/XWRT
- Post-dedup delta compression: Finesse (FAST '19) https://www.usenix.org/conference/fast19/presentation/zhang · Odess (ACM TOS) https://dl.acm.org/doi/10.1145/3584663 · Palantir (ASPLOS '24) https://dl.acm.org/doi/10.1145/3620665.3640353 · Argus (ACM TOS 2025) https://dl.acm.org/doi/10.1145/3747839
- GDCC 4th edition results page — **could not be retrieved 2026-08-17, connection refused**; leaderboard notation and prize structure from the organisers' pages: https://gdcc.tech/results/ · https://gdcc.tech/tag/4th-edition/
- Sibling doc with the measured geometry experiments referenced above: `docs/research/15-frontier-algorithms.md` §1 (preset dictionaries), §5 (BWT), §6 (similarity ordering), §7 (delta between similar chunks)
