# 14 — Neural and context-mixing compression in 2026: what is actually shippable

Research date: 2026-08-17. All numbers below are quoted from primary sources with their own
measurement dates; where a source's hardware is unknown or old, that is stated.

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
| paq-class + preproc | precomp -cn \| cmix v21 | 28,300,000 (approx) | +1.7% | ~1.6 KB/s |
| heavy open CM | zpaq 6.21 -method 7 | 38,995,519 | +40% | ~0.15–1 MB/s |
| open CM | mcm 0.82 -9 -max | 40,068,926 | +44% | ~2.5 MB/s |
| open CM (maintained) | kanzi 2.5.0 -l 9 (TPAQX) | 41,520,670 | +49% | ~1.1 MB/s/core |
| BWT+CM (maintained) | bsc 3.3.11 | 46,723,436 … 47,900,848 | +68…72% | ~11 MB/s/core |
| **narc today** | **narc max** | **~49,300,000** | **+77%** | ~4 MB/s/core |
| LZMA | xz 5.8.1 -9 / 7z -mx=9 | 48,802,580 / 48,792,760 | +75% | ~4.9 MB/s |

Read that table twice. Two separate facts fall out of it:

1. **At ≥10 MB/s single-thread there is nothing but BWT.** bsc lands at 46.7–47.9 MB, i.e. ~70%
   larger than paq8px. No CM and no neural model gets near 10 MB/s single-thread in 2026.
2. **The bankable prize is not "paq-class". It is 15–20% over LZMA at ~1 MB/s/core.** Every
   maintained open CM codec sits in a tight band, 39–42 MB on Silesia, at 1–2.5 MB/s per core.
   That band is 15–20% better than narc's current 49.3 MB. It is also 40–50% *worse* than
   paq8px. The remaining 40–50% is not mixing depth — it is paq8px's **specialised
   deterministic submodels** for 16-bit images, audio, tables and executables (Silesia contains
   x-ray, mr, sao, osdb, nci). See §7; this is the highest-value finding in the whole document.

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
| **kanzi TPAQ / TPAQX** (flanglet) | TPAQ = binary AC "based initially on Tangelo 2.4, itself derived from FPAQ8", context mixing by one-layer neural networks; TPAQX = "more predictions and more memory" | 161.7 MB (`-b 1024M -e TPAQX`) | ~1.1 MB/s/core (see §6) | **Apache-2.0, actively maintained (kanzi 2.5.0, 2,726 commits)**; C++, Java, Go — **no Rust** |
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
| **kanzi -l 9 (TPAQX)** | **11,618 ms** | **12,381 ms** | **41,520,670** | **18 / 17** |
| *narc max (8 cores, Win11)* | *6,800 ms* | *—* | *~49,300,000* | *~31 / —* |

Per-core, `kanzi -l 9` is ≈1.1 MB/s compress and ≈1.1 MB/s decompress. That is the number to
plan against.

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
5751-file tree, a 36% gap, and that gap comes from 7-Zip having one solid block with a 64 MiB
dictionary. A codec swap worth 12–15% takes 12 MiB → ~10.3 MiB and still loses. **Do not expect
an ultra tier to close the source-tree gap; that is a unit-geometry problem, not a codec problem.**

Separately, the small-input warm-up penalty is real and sharp at the low end: paq8px is ~1.27 bpb
on enwik8 (100 MB) but **1.73 bpb on alice29.txt (152 KB)**, worse than cmix on the same file.
Any CM tier in narc must not be applied to small solid blocks without measuring.

---

## 7. Where the remaining 40–50% actually lives (and it is not mixing)

`paq8px_v215 -12L` reaches **27,825,511 bytes on Silesia with files compressed individually**
(Mahoney, 2026-05-20). `zpaq -m7` reaches 38,995,519 and `mcm -9 -max` 40,068,926 on the same
corpus, same accounting. That 28% gap between paq8px and the best generic CM cannot come from
solidity (both are per-file) and does not come from mixer depth. It comes from Silesia's hostile
files and paq8px's **specialised deterministic submodels**:

- `x-ray` (8.4 MB) and `mr` (9.9 MB): 16-bit medical images → 2D/16-bit predictors
- `sao` (7.25 MB): binary star catalogue → fixed-length **record model** (column-wise contexts)
- `osdb` (10 MB): database dump → record model
- `nci` (33 MB): chemical structures → highly structured text
- `mozilla`, `samba`: executables/objects → exe models beyond plain BCJ

**This is the highest value-per-effort finding in this document.** A record/table model (detect
stride by autocorrelation of byte positions, then use `(column, previous-row-same-column)` as a
context) is a few hundred lines, all integer, exactly reversible, costs almost nothing at decode
time because it is only a context source, and it is where a large fraction of paq's advantage on
real mixed data comes from. narc already has BCJ and delta; this is the same category of work.

The 2026 AITDCC's noted entry `G7-V10` did precisely this: *block-level selection among raw, BWT,
LZP and x86-filtered transforms and models*. narc's per-unit codec tournament is already the
right frame; it is under-populated.

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
text units *today*, before any CM work — the largest single-digit win identified in this document
per line of code. Costs: a ~200–500 KB dictionary shipped in the binary (and charged against the
gain if you care about self-containment), English-only benefit, and a transform that must be
exactly reversible on arbitrary bytes. And if narc later ships a CM codec with a word model, this
gain mostly evaporates — so build it as a transform in the tournament, not as a always-on stage.

---

## 10. What narc should realistically adopt as an optional "ultra" tier

### Honest projected numbers

Baseline: narc max = ~49.3 MB on Silesia, 6.8 s compress on 8 cores (~31 MB/s aggregate).

| Ultra tier design | Projected Silesia size | Projected compress (8 cores) | Projected decompress (8 cores) | Memory per worker |
|---|---|---|---|---|
| lpaq/TPAQ-class integer CM, 32 MiB units, match model, 6–8 ICM/ISSE contexts | **41–43.5 MB** (−12…−16%) | ~9 MB/s → **~24 s** (3.5× slower) | ~9 MB/s → **~24 s** | 64–256 MB |
| + record/table model + 16-bit image/audio submodels | **38–41 MB** (−17…−23%) | ~8 MB/s → ~27 s | ~8 MB/s → ~27 s | 64–256 MB |
| libzpaq via `zpaq_rs`, `-m5`-equivalent model, as a measurement shortcut | ~39–42 MB | ~8 MB/s | ~8 MB/s | 100–500 MB |

Derivation, so the owner can check it: kanzi -l 9 on Silesia is 41,520,670 at ~1.1 MB/s/core with
its own default block size on a 9950X. Discounting for (i) narc's 32 MiB unit cap and (ii) a
first-generation Rust implementation being slower than 2,726 commits of tuning, 41–43.5 MB at
~1.1 MB/s/core is the defensible band. 8 cores × 1.1 MB/s ≈ 9 MB/s aggregate.

**The architectural point that makes this viable at all:** narc compresses units in parallel, so
a codec that is 1.1 MB/s single-thread is ~9 MB/s aggregate on the owner's 8-core box. The
"≥10 MB/s single-thread" bar in the original question is the wrong bar for narc's architecture.
The right bar is **≥1 MB/s single-thread with per-unit independence**, and that bar is met by
integer CM today.

### Hard constraints: which ones this breaks

| narc constraint | Status under an ultra CM tier |
|---|---|
| Editing one file stays cheap; append-only; no full repack | **Preserved.** CM runs per 32 MiB unit, independently, no cross-unit state |
| Extraction in bounded memory, 10–80 MiB today | **BROKEN as-is.** A useful CM model wants 64–256 MB *per decode thread*. This must become an explicit, documented tier cost, with a single-threaded low-memory decode path for weak machines. Shrinking the tables to fit 80 MiB costs real ratio — measure before committing |
| Exactly reversible, bit for bit | **Preserved, if and only if the prediction path is integer-only.** No floats anywhere. Add a cross-machine round-trip test (different CPU vendor, different SIMD width, `-C target-cpu=native` vs baseline) |
| Decompression speed matters | **Degraded 3–5×.** ~9 MB/s aggregate vs LZMA2's ~200+ MB/s. Acceptable only as an opt-in tier, never as a default |
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

1. **Record/table model + 16-bit image/audio submodels as new *filters* in the existing
   tournament.** Cheap, integer, reversible, no new tier, no memory blow-up, and §7 says this is
   where most of paq8px's Silesia advantage actually lives.
2. **DRT/WRT-style word-replacement transform as a tournament entry in front of PPMd7.** ~5%
   on English text with a PPM back end (§9).
3. **Measure before building:** wire `zpaq_rs` (or shell out to `kanzi -l 9`) behind a feature
   flag, run narc's own corpora through it at 32 MiB unit granularity, and get a real number for
   what CM buys *on narc's units* rather than on whole corpora. This is a day of work and it
   decides whether step 4 is worth weeks.
4. **Only then**: a Rust integer CM codec — order 1/2/3/4/6 ICM + ISSE chain + match model +
   word model + 2 SSE stages + context-selected logistic mixer, all int, ~1,500–3,000 lines,
   plus a lot of tuning. Budget the tuning honestly; kanzi has 2,726 commits.
5. **Never**: anything with a matmul, a GPU, a model file, or a float in the prediction path.

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
