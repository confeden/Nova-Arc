# GPU Compute Beyond Codecs — What an Archiver Can Actually Offload

Companion to `08-gpu-compression.md` (which answered "can the GPU run a codec?" — answer: only
zstd-1..3-class ratios). This report answers the *other* question: can GPU horsepower help the
**analysis, dictionary, similarity, transform and verification** stages of Nova Prism?

Every number below is tagged **[measured here]** (run on the owner's machine), **[measured
elsewhere]** (published benchmark, link given), or **[claimed]** (vendor/author assertion,
unverified).

An adversarial verification pass re-ran the local benchmarks and re-checked every cited source. What
it changed is marked inline as a *correction*: the Gerbil argument in §4.3 was backwards, the
Migratory Compression dismissal in §5.2 was wrong on the paper's own numbers, MANS is BSD-3-Clause
not MIT, the PCIe throughput figure is derived rather than measured, and bsc's 6.5× carries a
block-size condition (§7.1). The dictionary tables in §4.2 reproduced byte-for-byte; the §1 CPU
baselines reproduced only with best-of-N sampling on an otherwise-idle machine.

---

## 1. Executive summary

**One structural fact decides almost everything: this machine's GPU link is PCIe 3.0 x8, not 5.0 x8.**

```
$ nvidia-smi -q | GPU Link Info
    PCIe Generation:  Max 3 | Current 3 | Device Max 5 | Host Max 3
    Link Width:       8x
```
**[measured here]** — the link generation and width are what `nvidia-smi` reports; the bandwidth
below is **derived, not measured on this machine** (no CUDA toolkit installed, so no
`bandwidthTest` was run). The RTX 5060 Ti supports Gen5, the motherboard slot caps it at Gen3.
PCIe 3.0 runs 8 GT/s per lane with 128b/130b encoding = 984.6 MB/s per lane (PCI-SIG spec
arithmetic), so x8 is **7.88 GB/s theoretical**; real pinned transfers land at ~78–85 % of
theoretical, hence **~6.0–6.8 GB/s practical one way [derived]** — NVIDIA's own pinned-transfer
article measures 6.2 GB/s on an 8 GB/s Gen2 x16 link, i.e. 78 %
([how to optimize data transfers](https://developer.nvidia.com/blog/how-optimize-data-transfers-cuda-cc/)).
Every byte an archiver sends to the GPU and back must cross that wire twice.

Against that ceiling, here is what the CPU on this machine already does **[measured here]**:

| Stage | 1 thread | 8 threads | vs. the ~6 GB/s bus |
|---|---:|---:|---|
| blake3 (4 MiB chunks) | 4.55 GB/s | **31.6 GB/s** | CPU 5× faster than the *bus* |
| order-0 entropy (256-bin histogram) | 1.21 GB/s | **9.5 GB/s** | CPU faster than the bus |
| trial zstd-1 on 64 KiB/chunk (analyzer probe) | **35.8 GB/s of archive bytes** | — | 6× the bus |
| byte-level memcpy | n/a (optimizer elided the loop) | 7.0 GB/s (allocation included) | at or above the bus |
| order-1 (byte-pair) entropy | 0.92 GB/s | **6.6 GB/s** | above the bus |
| MinHash, 4 super-features, full coverage | 0.22 GB/s | 0.97 GB/s | 6× under the bus |
| MinHash, 16 hashes, full coverage | 0.07 GB/s | 0.25 GB/s | 24× under |
| zstd-19 on 32 MiB blocks | 0.0030 GB/s | ~0.024 GB/s | 250× under |
| nova max tier (LZMA2+PPMd tournament) | — | **0.005 GB/s** | 1200× under |
| zstd fastcover dictionary training (optimize) | 0.0006 GB/s | **0.0024 GB/s** | 2500× under |
| *reference:* pure DRAM stream read, 8 threads | — | **39.8 GB/s** | the ceiling every row above shares |

**Two qualifiers on this table, both verified by re-measurement (`src/bin/verify.rs`, best-of-12):**

1. **These are idle-machine peaks.** Re-run with ~65 % background CPU load, the same binary reported
   blake3 8t **5.5 GB/s** (not 31.6), order-0 8t 2.6 GB/s and stream read 3.1 GB/s. During a real
   nova job all 8 cores are compressing, so the throughput actually *spare* for an analysis stage is
   a fraction of the peak. The honest form of the rule below is therefore "below ~3 GB/s **of spare
   capacity**", and doc 08 already treats keeping the CPU free as a feature in itself. It does not
   reverse any verdict here (hashing 114 MiB costs 4 ms idle, ~22 ms saturated, still ≈ the 20 ms
   transfer), but it is the one assumption that would move first.
2. Single-thread figures reproduce closely (blake3 4.61, order-0 1.23, order-1 0.96). **Read the
   8-thread column as an upper bound**: the best of 12 re-runs reached blake3 28.5 and order-0
   6.6 GB/s, short of 31.6 / 9.5, and residual load cannot be ruled out. The 8-thread order-1 number
   was an extrapolation ("~5 GB/s") in the first pass and is now *measured* at 6.6 GB/s — order-1
   statistics are **above** the bus, not "at" it, which strengthens the verdict. The 39.8 GB/s
   stream-read row exists to show the 31.6 GB/s claim is under the platform's DRAM ceiling (80 %) and
   therefore physically possible rather than a timing artefact.

The rule this yields: **a stage is a GPU candidate only if its aggregate CPU throughput is below
~3 GB/s** (upload+download must fit in the time the CPU would have spent), and only if a GPU
implementation exists at *equal output quality*. Everything above 3 GB/s — hashing, entropy stats,
sampled trial compression, byte transforms — is permanently bus-bound and must stay on the CPU.
Everything below it is a candidate *only on paper*, because for four of the five slow stages no GPU
implementation exists.

**Ranking (real speedup × ratio help):**

| # | Candidate | GPU impl. exists? | Realistic speedup of the stage | Effect on ratio | Build it? |
|---|---|---|---|---|---|
| 1 | **BWT / suffix sorting** (libcubwt) | **yes**, Apache-2.0, active | **6.5× end-to-end measured in bsc — but at 318 MB blocks (§7.1); 6–11× per-block over libsais holds down to ~4 MB inputs** | **zero cost — byte-identical output** | **Only if a BWT codec enters the format, and only with the block ≤ nova's chunk cap.** The GPU part is the easy half. |
| 2 | Similarity sketching (MinHash) before solid packing | no (200 lines of our own) | 10–20× of a 0.25 GB/s stage | indirect; MC measured +15–74 % even for 7z with a 64 MB+ dictionary | CPU version first (higher priority than this report first assumed); GPU almost certainly never needed |
| 3 | Dictionary training (COVER/fastcover) | **no** — nothing in CUDA | unknown; CPU has 10× unexploited headroom | **negative in nova's geometry (measured)** | **No** |
| 4 | Analysis: entropy / n-gram / compressibility for many chunks | trivial to write | ≤1× (bus-bound) | none | **No** |
| 5 | Byte transforms (delta, transpose, BCn, float shuffle) | yes (Brotli-G/nvCOMP) | ≤1× (bus-bound) | small, same as CPU | Only as a free rider on GPU *codec* work |
| 6 | Entropy coding (rANS/ANS) | dietgpu archived; MANS alive, ints only | 100×+ on-device, ~0 end-to-end | order-0 class ratio = worse than what we ship | **No** |
| 7 | Verification / hashing many chunks | yes | **negative** | none | **No — confirmed with numbers** |

**Bottom line:** for Nova Prism as designed today, there is **no GPU work worth building** in these
seven areas. The one genuinely attractive item (GPU BWT) is gated behind a *CPU* decision — whether
libbsc-class block sorting deserves a place in the format at all, **at a block size that keeps edits
cheap and extraction bounded**. Make that call on CPU merits; the GPU is then a bonus on the
transform, not a reason. The two CPU-side leads this report generated — a statistics-driven predictor
to replace the max-tier tournament (§3) and similarity-ordered solid blocks (§5) — are worth more
than any GPU item in it.

---

## 2. The machine, measured

```
NVIDIA GeForce RTX 5060 Ti, driver 610.88, 8151 MiB VRAM, compute capability 12.0
PCIe: device max Gen5, host max Gen3, running Gen3 x8, width 8x
VRAM already in use with an idle desktop: 1254 MiB (15 % of the card)
CPU: 8 logical cores; RAM 32 GB
```
**[measured here]** with `nvidia-smi --query-gpu=...`.

Three consequences that any GPU plan must respect:

1. **Bus ≈ 6 GB/s each way**, not the ~25 GB/s that `08-gpu-compression.md` §2.3 assumed for a Gen5
   x8 link. That report's throughput estimates for GPU zstd/LZ4 offload should be re-read with this
   correction: on *this* machine the GPU decompression path is capped near NVMe speed, not above it.
2. **~1.2 GiB of VRAM is gone before nova starts** (1254 MiB at the time of measurement, 1491 MiB on
   a later check — it drifts with what the desktop is doing). A 2 GiB budget is the right default;
   anything that needs 20.5×block (libcubwt) must size blocks accordingly: 8151 MiB / 20.5 ≈ 398 MiB
   absolute max, **≈336 MiB** with the desktop deducted. libcubwt's own benchmark methodology caps
   inputs at 352 MB on a *12 GB* 4070 Ti, which is consistent. Note that the *whole of bsc* needs
   more than libcubwt alone: 21× block for the forward BWT per its README, and Grebnov's forum figure
   is ~30–32× including OS/CUDA overhead — so a block that actually fits here is ~215–230 MiB, not
   318 MB (the size the famous 6.5× used, see §7.1).
3. **This is the display GPU, so the Windows TDR watchdog applies**: any kernel that does not return
   within `TdrDelay` (**default 2 s**) causes a driver reset —
   [Microsoft TDR docs](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/timeout-detection-and-recovery),
   [TDR registry keys](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/tdr-registry-keys).
   Five resets in a minute ⇒ bugcheck 0x117. libbsc's README warns about exactly this for its CUDA
   mode. So every kernel must be chunked to ≲100 ms, and "raise TdrDelay in the registry" is not an
   option for a shipped consumer archiver (Microsoft classes those keys as developer-only).

---

## 3. Candidate 1 — the analysis phase on the GPU

**Idea:** nova's two-phase design classifies each unit (magic bytes → content class → trial
compression). Could the GPU compute entropy, n-gram statistics or compressibility estimates for
thousands of chunks at once?

**It could. It would not matter.** **[measured here]**, on 512 MiB of mixed synthetic data:

| Estimator | 1 thread | 8 threads | Cost for a 114 MiB job |
|---|---:|---:|---:|
| order-0 entropy (full coverage) | 1.21 GB/s | 9.49 GB/s | 12 ms |
| order-1 byte-pair entropy, 64 K bins (full coverage) | 0.92 GB/s | 6.6 GB/s (measured in the verification pass) | 18 ms |
| zstd-1 trial on 64 KiB of every 4 MiB chunk | 35.8 GB/s of archive bytes | — | 3.3 ms |
| zstd-1 trial on **all** bytes | 0.53 GB/s | ~3.5 GB/s | 34 ms |
| zstd-3 trial on all bytes | 0.22 GB/s | ~1.5 GB/s | 80 ms |
| zstd-12 trial on all bytes | 0.035 GB/s | ~0.25 GB/s | 480 ms |

nova's max tier spends **22 s** on that same 114 MiB. The entire analysis budget is therefore
0.02–2 % of the job. Uploading the data to compute it would cost 114 MiB / 6 GB/s ≈ **19 ms just in
transfer**, i.e. more than the order-0 pass costs on the CPU, and the GPU would still have to be fed
by the same disk read.

Two secondary findings worth keeping:

- The literature agrees on the shape: USENIX FAST'13
  ["Effective Resource Usage for Real-Time Compression"](https://www.usenix.org/system/files/conference/fast13/fast13-final38.pdf)
  measured that **entropy heuristics are much faster than prefix-compression estimation**, and that
  prefix estimation "essentially comes for free" on compressible data because you keep the output.
  **[measured elsewhere]**
- Estimator bias is the real research problem, not speed:
  [black-box ratio prediction](https://arxiv.org/pdf/2305.08801) reports block sampling
  *systematically underestimates* ratios; entropy is only a lower bound. A GPU makes a biased
  estimator biased faster.

**Where the analysis idea *does* have value — on the CPU.** nova's max tier runs a 3-way tournament
(LZMA2 + PPMd order 10 + PPMd order 16) on every unit at ~0.005 GB/s aggregate. Replacing the
tournament with a *predictor* built from the cheap statistics above (order-0/order-1 entropy, match
density from a zstd-1 probe, magic class) would cut max-tier time by **at most 3×** — that is the
arithmetic ceiling of dropping 2 of 3 arms, **not a measurement**, and the ratio it costs is
unmeasured too. That is a pure-CPU project and it is worth more than anything in this report.

**Verdict: do not build. Bus-bound, and the stage is already ~1 % of the job.**

---

## 4. Candidate 2 — dictionary training

This is where the compute gap is genuinely huge — and where the ratio payoff turns out to be
**negative for nova's actual geometry**. Both halves were measured here.

### 4.1 What training costs on the CPU today **[measured here]**

Corpus: the project's own source tree (`test/corpus`), files < 256 KiB, sorted by extension exactly
as nova's solid packer sorts them. `ZDICT_optimizeTrainFromBuffer_fastCover(d=8, steps=4, f=20)`,
110 KiB dictionary target, zstd 1.5.7:

| Sample set | 1 thread | 8 threads | speedup | `k` pinned (no sweep), 1 thread |
|---|---:|---:|---:|---:|
| 10.5 MB / 519 files | 17.3 s (0.6 MB/s) | 2.95 s (3.6 MB/s) | 5.9× | 5.2 s (2.0 MB/s) |
| 26.2 MB / 1 437 files | 35.3 s (0.7 MB/s) | 10.2 s (2.6 MB/s) | 3.5× | 11.6 s (2.3 MB/s) |
| 52.4 MB / 3 316 files | 79.5 s (0.7 MB/s) | 25.1 s (2.1 MB/s) | 3.2× | 14.3 s (3.7 MB/s) |
| 77.8 MB / 5 707 files | **137.3 s (0.6 MB/s)** | 32.3 s (2.4 MB/s) | 4.3× | 21.1 s (3.7 MB/s) |
| 26.2 MB, **COVER** (the slow builder), 8 threads | — | 39.1 s (0.67 MB/s) | — | — |

Reference points: the whole 114 MiB max-tier job takes 22 s; zstd-19 on the same samples takes
~26 s single-threaded. **Single-threaded dictionary training over 78 MB of samples costs 137 s —
6× the entire archive job.** Per byte it is the most expensive thing in the pipeline, ~250× the cost
of blake3 and ~4.5× the cost of zstd-19 itself.

Note what the table also says: **the CPU has ~10× of free headroom before a GPU is even relevant.**
`zstd`'s Rust wrapper calls the single-threaded path (`ZDICT_trainFromBuffer` → fastCover with
`nbThreads=1`); simply passing `nbThreads=8` gives 3.2–5.9×, and pinning `k` to skip the
`steps` sweep over k ∈ [50, 2000]
([documented behaviour](https://github.com/facebook/zstd/blob/dev/lib/zdict.h): `ZDICT_trainFromBuffer`
"redirects towards `ZDICT_optimizeTrainFromBuffer_fastCover()` single-threaded, with d=8, steps=4,
f=20, accel=1", and `nbThreads` is "only used for optimization") gives **3.0–6.5× over the
single-threaded sweep** on its own (17.3→5.2, 35.3→11.6, 79.5→14.3, 137.3→21.1 s). The two levers
are not additive in the table above, because the `k`-pinned column is still single-threaded: on the
two largest sample sets pinned-k-1-thread already *beats* the 8-thread sweep by 1.5–1.8×, on the two
smallest it loses to it. Combining both is untested; 137 s → **~10 s** is the order of magnitude to
expect, without a line of GPU code.

### 4.2 What a dictionary is worth in nova's real compression unit **[measured here]**

The usual "-13 % with a trained dictionary" number is measured **per file**. nova does not compress
per file — it concatenates small files into solid blocks. So the same 110 KiB dictionary was tested
against blocks of 64 KiB … 32 MiB, zstd-19, with `windowLog` pinned per block size and
`pledgedSrcSize` set, in three arms (control for zstd API artefacts):

Every row below was **re-run and reproduced byte-for-byte** in an independent verification pass
(same corpus, 77.8 MB of samples, 74.2 MiB packed into blocks, dict 112 640 B).

| Block | no dict | `loadDictionary` (trained, w/ entropy tables) | `refPrefix` (raw dict content as prefix) |
|---|---:|---:|---:|
| 64 KiB (859 blocks) | 12.43 MiB | 11.16 MiB (**−10.21 %**) | 11.16 MiB (−10.25 %) |
| 1 MiB (73) | 9.99 MiB | 10.00 MiB (+0.16 %) | 9.69 MiB (−2.93 %) |
| 8 MiB (10) | 7.56 MiB | 8.23 MiB (**+8.86 %**) | 7.48 MiB (−1.10 %) |
| 32 MiB (3) | 6.60 MiB | 7.54 MiB (**+14.30 %**) | 6.57 MiB (−0.44 %) |

Three conclusions, in order of importance:

1. **A trained dictionary is worth ~0 at nova's block sizes.** Its content adds −1.1 % at 8 MiB and
   −0.44 % at 32 MiB: the block already contains the cross-file redundancy the dictionary was
   supposed to supply. Dictionaries substitute for large blocks; they do not add to them. Bigger
   blocks are worth far more: 64 KiB → 32 MiB blocks alone take 12.43 → 6.60 MiB (**−47 %**).
2. **`ZSTD_CCtx_loadDictionary` with a *trained* dictionary actively hurts large inputs** — +8.9 %
   at 8 MiB, +14.3 % at 32 MiB, with window and source size pinned identically in both arms. The
   `refPrefix` arm (same bytes, no dictionary entropy tables / repcodes) does not show it, so the
   cause is the dictionary's prescribed entropy state and the dictMatchState search path, not
   parameter derivation. **Gotcha for the codebase**: if nova ever attaches a dictionary, verify
   per unit that it helps, and never assume `loadDictionary` is free. (Also note the separate,
   well-known trap this control ruled out: `ZSTD_createCDict` derives cParams from the *dictionary*
   size, so a naive `EncoderDictionary::copy` at level 19 compresses a 32 MiB block with parameters
   sized for 110 KB.)
3. A dictionary is only interesting in the regime nova uses for **cheap edits** — small units
   (64 KiB–1 MiB), where it is worth −10 % / −3 %. That is a real design option ("small units +
   shared dictionary" instead of "big blocks"), but it is a *format* decision, not a GPU one — **and
   the same table refutes it as a ratio play**: 64 KiB units *with* the dictionary still produce
   11.16 MiB against 6.60 MiB for 32 MiB blocks *without* one, i.e. **+69 %**. The dictionary buys
   back only a fifth of what small units give away. It would also introduce nova's first global
   cross-unit dependency: a shared dictionary can only be frozen at archive creation (retraining
   rewrites every unit), so ratio would decay as the archive's later contents drift from the trained
   corpus — the opposite of the append-only edit story. Extraction memory is unaffected (110 KiB).

### 4.3 Is there a GPU dictionary trainer? No.

- **No CUDA/HIP/WGSL implementation of COVER or fastcover exists** (searched; nothing on GitHub,
  crates.io, or in the literature — re-checked independently in the verification pass, which also
  found no GPU LZMA/PPMd-class codec: GPU lossless work is still LZ4/LZSS/Snappy/ANS-shaped, and
  high-ratio research has moved to neural models that use the GPU for inference). Writing one means
  porting the d-mer frequency counting, the segment-scoring pass, and the k/d parameter sweep — and
  the greedy selection loop with gain discounting is inherently sequential, so it would stay on the
  host.
- The nearest published art is GPU **k-mer counting** from bioinformatics —
  [Gerbil](https://github.com/uni-halle/gerbil) (Algorithms Mol Biol 2017),
  [RapidGKC](https://ieeexplore.ieee.org/document/10598059) (ICDE'24),
  [distributed-memory GPU counting](https://ieeexplore.ieee.org/document/9460480) (2021).
  **Correction — an earlier draft of this report had Gerbil's result backwards.** The sentence
  "Gerbil's performance is comparable to existing state-of-the-art open source k-mer counting tools
  for small k < 32, it vastly outperforms its competitors for large k" is about *Gerbil vs other CPU
  tools*, not about *its GPU vs its CPU*. Gerbil's actual GPU ablation says the opposite of what was
  claimed here: "For small k, the use of a GPU improves the running time by a significant amount of
  time", the GPU "accelerates Gerbil's second phase by up to a factor of about two", and the "GPU
  induced speedup nearly vanishes when k exceeds 150" (Table 3: k=28, H. sapiens, 32:04 with GPU vs
  46:41 without). **[measured elsewhere]** So the honest reading is: GPU hash-table counting *does*
  help in the small-key regime that fastcover's d = 6..8 corresponds to — by about **2× on the
  counting phase, ~1.45× end to end**. That is not a disqualifier, it is simply far too small a prize
  to justify writing a CUDA fastcover from scratch when `nbThreads` and `k`-pinning are sitting there
  unused (§4.1) and the ratio payoff at nova's block sizes is ~0 (§4.2).
- The parameter sweep (the part that actually dominates `optimize*` runtime) is embarrassingly
  parallel across *independent training runs* — which is exactly what `nbThreads` already exploits
  on the CPU, for free.

**Verdict: do not build. No implementation, no ratio payoff at nova's block sizes, and 10× of CPU
headroom left unused. Revisit only if nova adopts a small-unit + shared-dictionary format.**

---

## 5. Candidate 3 — similarity detection (MinHash / super-features) for clustering

**Idea:** sketch every chunk, cluster similar chunks, and pack similar data into the same solid
block so the codec can exploit cross-file redundancy that extension-sorting misses.

### 5.1 Cost on the CPU **[measured here]**

One pass of a 64-bit polynomial rolling hash over 32-byte shingles, `n` salted permutations, on
512 MiB:

| Sketch | 1 thread | 8 threads |
|---|---:|---:|
| 16 min-hashes, every position | 0.07 GB/s | 0.25 GB/s |
| 4 super-features, every position | 0.22 GB/s | 0.97 GB/s |
| 16 min-hashes, sampled (64 KiB of each 4 MiB chunk) | **5.13 GB/s of archive bytes** | — |

This is the **best-shaped GPU workload in the entire report**: high arithmetic intensity per byte,
no branching, output of 128 bytes per chunk (so the download side is free), and no ratio risk
whatsoever. Upload at 6 GB/s versus a CPU that manages 0.25 GB/s means a theoretical **~20×**.

### 5.2 Why it still does not justify GPU work

- **The full-coverage variant is not required.** Sampled sketching costs 5.13 GB/s of archive bytes
  on *one* thread — already faster than the disk. Migratory Compression's *file-level* prototype
  defaults to **16 features combined into 4 super-features** ("By default we use sixteen features,
  combined into four SFs … we therefore default to using 4 SFs", §5.4.3) over 8 KiB chunks; the
  12 features → 3 SFs configuration is the one used for archival migration inside DDFS (§6)
  ([FAST'14](https://www.usenix.org/system/files/conference/fast14/fast14-paper_lin.pdf)). So MC does
  use a dense 16-hash sketch — an earlier draft of this report claimed otherwise. The sampling
  argument stands on our own measurement, not on MC's parameter choice.
- **Even full coverage with 4 super-features hits 0.97 GB/s on 8 threads**, which is NVMe read
  speed. The stage would be I/O-bound before it is CPU-bound.
- **The ratio payoff is unproven for nova's data — but the paper does not say it is small.** MC
  reports **+11 % to +105 %** overall **[measured elsewhere]**, and per compressor "23–105 % for gzip,
  18–84 % for bzip2, **15–74 % for 7z** and 11–47 % for rzip". Its footnote about window size ("if the
  chunks were 64 KB, gzip would not match the start of one chunk against the start of the next") is a
  caveat about chunks being *large* relative to the window, not a claim that large-window compressors
  stop benefiting: the paper's own best result is **7z-MAX with MC**, i.e. LZMA with a large
  dictionary still gains from reordering, and it gains at better throughput than 7z-MAX alone. nova's
  32 MiB blocks / 64 MiB LZMA2 dictionary are the same regime as that 7z arm. **Correction to an
  earlier draft: this is the regime where MC's gains are smaller, not smallest-to-nil — 15–74 % is
  still the largest unclaimed ratio prize in this report.** MC's cost is memory: the paper cites
  6 GB of extra reorganization buffers vs gzip ("128 KB compression regions × 48 K regions filled
  simultaneously") for the DDFS deployment — which is exactly the constraint nova's bounded-memory
  invariant would have to answer, and the reason a nova version must reorder *within* a block budget.
- **Editing conflicts with clustering.** nova's differentiator is that a changed file rewrites one
  block. Similarity-clustered blocks are chosen by *content*, so a small edit can move a file into a
  different cluster and invalidate more than one block. Any clustering scheme has to be stable under
  edits — a design problem the GPU does not help with.
- GPU dedup tooling that does exist is built for a scale nova will never see:
  [SEDD](https://arxiv.org/abs/2501.01046) (KDD'26, DOI 10.1145/3770855.3818161 — venue confirmed)
  reports up to **158× vs the CPU-based SlimPajama tool and 7.8× vs NVIDIA NeMo Curator on 30 M
  documents with 4 GPUs** (375× on MinHash signature generation alone), and 1.2 T tokens in 3 h on
  32 V100s **[claimed/measured elsewhere]**. It is an MPI/multi-node C++/CUDA framework over
  `.jsonl` files — not a library one links into a desktop archiver.
  [NeMo Curator's fuzzy dedup](https://docs.nvidia.com/nemo/curator/curate-text/process-data/deduplication/fuzzy)
  is the productised equivalent (RAPIDS/cuDF, GPU-only MinHash+LSH). A 100 GB archive at 1 MiB
  chunks is ~100 000 sketches; SEDD's regime starts six orders of magnitude higher.

**Verdict: the *feature* deserves a CPU experiment, and on the corrected reading of MC it is the
highest-value follow-up in this report** (measure the ratio gain of similarity-ordered solid blocks
vs extension-ordered on real corpora, with the reorder buffer capped so bounded memory holds, and
with cluster assignment made stable under edits). **The GPU version is premature and would remain
premature until archives reach tens of GB with the sketch on the critical path.**

---

## 6. Candidate 4 — GPU entropy coding (rANS/ANS)

| Project | State (verified 2026-08) | License | Fits nova? |
|---|---|---|---|
| [dietgpu](https://github.com/facebookresearch/dietgpu) | **archived** (per doc 08, last push 2026-03) | MIT | reference only |
| [MANS](https://github.com/hpdps-group/MANS) (SC'25) | **alive**, the only actively developed option | **BSD-3-Clause** (its `LICENSE` file; GitHub reports "NOASSERTION") | **no — multi-byte integer data only, explicitly not general byte streams** (its core is ADM, "maps 16/32-bit integers into a compact 8-bit domain"); its README claims **0.52× dietgpu's CUDA compression throughput, 0.47× decompression** **[claimed]** |
| [hipANS](https://github.com/PAA-NCIC/hipANS) | dietgpu port to ROCm | MIT-derived | AMD-only relevance |
| [Recoil](https://dl.acm.org/doi/10.1145/3605573.3605588) (ICPP'23) | paper | — | idea: rANS decodable from arbitrary positions |
| nvCOMP ANS | proprietary bitstream | NVIDIA EULA | banned from `.nva` by existing policy |

Three independent reasons this is dead for nova, any one sufficient:

1. **Ratio class.** An ANS stage compresses to ~order-0 entropy. nova's max tier ships PPMd7 and
   LZMA2. Using GPU ANS would mean *building our own codec* whose modelling stage is also on the
   GPU — the thing doc 08 established does not exist above zstd-1..3 quality.
2. **Bus.** 400 GB/s of on-device ANS throughput against a 6 GB/s wire is 1.5 % utilisation; the
   end-to-end number would be ~6 GB/s minus overhead, i.e. an expensive way to reach LZ4 speed.
3. **Format dependency.** Any GPU-only bitstream breaks "a weak PC with no GPU must always be able
   to extract", which is an owner requirement. We would have to write and maintain a CPU decoder for
   the same stream anyway.

**Verdict: no.**

---

## 7. Candidate 5 — preprocessing transforms

### 7.1 BWT / suffix sorting on the GPU — the one real win

[libcubwt](https://github.com/IlyaGrebnov/libcubwt) (Ilya Grebnov, same author as libsais/libbsc):

| Property | Value | Source |
|---|---|---|
| License | **Apache-2.0** | repo |
| Liveness | **active** — v1.6.3, 2025-08-13, CUDA 13.0 support | repo |
| Speed vs libsais 2.7.1 (CPU) | **6–11×**; 637 MB/s on enwik9 (369 MB), 844 MB/s on dickens, 1298 MB/s on x-ray | repo benchmarks on RTX 4070 Ti + i7-9700K **[measured elsewhere]** |
| VRAM | **20.5 n bytes** ⇒ ≈336 MiB max block on this 8 GB card (libcubwt's own benchmarks capped inputs at 352 MB on a *12 GB* card) | repo |
| Recommended HW | SM 8.9+ (Ada) "due to very large L2 cache"; repo also warns it "is sensitive to fast GPU memory and might not be suitable for some workloads. Please benchmark yourself" | repo |
| **Limitation** | **since v1.5.0 no suffix arrays / inverse suffix arrays — BWT only**; inverse BWT came back in v1.6.0 (2024-01-24) | repo changelog |
| Bus relevance | the repo's benchmark box is a **PCIe 3.0 x16** Z390 board with an i7-9700K — the same CPU as this machine, twice the link width — and its timings **include** host↔device transfer. Redoing the enwik9 point at Gen3 x8 adds ~60 ms to 579 ms, i.e. **~577 MB/s instead of 637**: BWT is the one candidate in this report that survives the bus with room to spare | repo methodology + arithmetic |
| Where it loses / stalls | the Gauntlet corpus rows: `abac` 47.7 MB/s GPU vs 89.6 CPU, `houston` 164.7 vs 188.8, `fib_s14930352` 134 vs 82.6 (only 1.6×). Pathological/highly repetitive inputs are not 6–11×, and two of them are outright losses | repo benchmarks |

End-to-end evidence in a real compressor: bsc 3.3.0 on enwik10 (10 GB), same machine, same command
plus `-G` **[measured elsewhere,
[encode.su, Grebnov, Feb 2023](https://encode.su/threads/586-bsc-new-block-sorting-compressor/page15)]**:

| Mode | Time | Output |
|---|---:|---|
| CPU (i7-9700K) | 159.5 s | 1 694 643 962 B |
| GPU (RTX 4070 Ti, `-G`) | **24.5 s** | **1 694 643 962 B — byte-identical** |

That is a **6.5× speedup at exactly zero ratio cost**, which no other item in this report can claim.
[libbsc](https://github.com/IlyaGrebnov/libbsc) itself is Apache-2.0 (active: v3.3.12, 2025-09),
requires compute capability 7.5+, and documents GPU memory as 20× block for ST / 21× for forward
BWT / 7× for inverse.

**Three conditions on that 6.5 × that must travel with the number:**

1. **It was measured with `-b318`, i.e. 318 MB blocks** (`bsc.exe e enwik10 … -b318 -G`). At libbsc's
   documented 21× that needs 6.7 GB of VRAM; Grebnov's own figure in the same thread is ~30–32×
   including OS/CUDA overhead. This 8 GB card has ~6.9 GB free, so the published configuration is
   borderline-to-impossible here (§2).
2. **When the block does not fit, bsc falls back to the CPU** (Grebnov's explanation in the same
   thread). The counter-example posted there: an i9-13900KS + GTX 1080 Ti got 35.6 s → 30.4 s, i.e.
   **1.17×**, because the block exceeded VRAM. "6.5×" is a property of a matched block/VRAM/GPU
   combination, not of `-G`. (encode.su refuses automated fetches; these quotes come from the
   indexed thread text, so treat them as second-hand until someone opens the page.)
3. **A 318 MB BWT block is incompatible with nova as designed** — the BWT block *is* the compression
   unit, so it is also the edit granularity (edit cost ∝ block) and the extraction working set
   (libbsc's CPU decode is 16 MB + 5× block ⇒ 1.6 GB for a 318 MB block). Any BWT codec in `.nva`
   must keep the block at the existing chunk cap (≤16 MiB), where 5× block = 80 MiB and bounded
   extraction still holds — and where the *ratio* case for BWT is unmeasured, because bsc's published
   ratios all come from huge blocks.

**But the honest framing: this is a codec decision wearing a GPU costume.** GPU BWT is only useful
if `.nva` gains a BWT-based codec. The relevant comparison is therefore *libbsc on the CPU vs
nova's current max tier*, and that is a question for the codec research track (doc 01), not this
one. Two facts that make it worth asking: bsc's CPU path did 10 GB in 159 s (~63 MB/s, **at 318 MB
blocks and 8 threads** — not a like-for-like against nova's 16 MiB units) while nova's max tier does
~5 MB/s, and BWT+CM is historically competitive with PPMd on text. If libbsc earns a place on merit
*at nova's block size*, `-G` is then a bonus on the transform stage for NVIDIA users, and the
fallback is the same code path with `-G` off.

Caveats to carry: libcubwt is CUDA C++ (needs the toolkit at build time, NVIDIA-only at runtime), and
at 16 MiB per unit only 328 MiB of VRAM is needed — the constraint flips from memory to launch
granularity. The good news for small units is that libcubwt's own table still shows 570–1300 MB/s at
0.8–10 MB inputs against libsais' 43–98 MB/s, so the per-block advantage does not evaporate at nova's
scale; the open question is per-launch overhead across thousands of units, which the repo's
minimum-of-five-runs methodology hides. Batching many units per launch would be required.

### 7.2 Delta, byte transposition, float shuffle, BCn

All are memory-bound streaming transforms that the CPU does at 5–15 GB/s — above the bus (**not
measured here**; the closest local anchors are a 39.8 GB/s 8-thread stream read and 7.0 GB/s memcpy
with allocation). On the GPU they are free *only if the data is already resident*, i.e. only as a
rider on GPU codec work (nvCOMP path from doc 08). AMD's
[Brotli-G](https://github.com/GPUOpen-LibrariesAndSDKs/brotli_g_sdk) BCn pre-conditioning
(+10–15 % on textures **[claimed — not stated in the SDK README; treat as unsourced until a GPUOpen
figure is found]**) is the interesting *algorithm*; it is implementable on the CPU at full speed, and
the SDK itself is dormant (MIT, last push 2024-04-18).

**Verdict: BWT — conditional yes, gated on a CPU codec decision. Byte transforms — no.**

---

## 8. Candidate 6 — verification / hashing many chunks: confirmed dead

Doc 08 recorded "GPU blake3 loses" from published sources. This is now confirmed with numbers from
this machine **[measured here]**:

| | Throughput | vs. PCIe 3.0 x8 (~6 GB/s one way) |
|---|---:|---|
| blake3, 1 thread, 512 MiB | 4.55 GB/s | 0.76× — a *single core* nearly saturates the bus |
| blake3, 8 threads, 128 × 4 MiB chunks | **31.6 GB/s** | **5.3× faster than the bus** |

Hashing 114 MiB costs **3.8 ms** on 8 cores. Uploading it costs ~20 ms. The GPU cannot win even if
its kernel took zero time. The same arithmetic kills GPU CRC32, GPU chunk verification during
extract, and GPU-side FastCDC boundary search (whose CPU cost is of the same order as hashing).

The only surviving exception is unchanged from doc 08: checksums of data that is *already* in VRAM
because a GPU codec put it there.

---

## 9. Candidate 7 — practical Rust paths in 2026, and what breaks without an NVIDIA GPU

| Path | Build-time requirement | Runtime requirement | Behaviour with no NVIDIA GPU |
|---|---|---|---|
| **cudarc 0.19.9** (current, 2026-08-11) — default feature set is `["std","cublas","cublaslt","curand","driver","runtime","nvrtc","fallback-dynamic-loading"]`; a plain `dynamic-loading` feature also exists (the README's claim that it is the default is stale — the manifest is authoritative) | none — builds on any machine | driver libs `dlopen`'d lazily | probe fails cleanly → CPU path. **This is the only safe default.** |
| cudarc + **NVRTC** (`compile_ptx`) | none | **`nvrtc64_*.dll` must be present — it ships with the CUDA toolkit, *not* the driver** | fails at first kernel compile; must be caught |
| **Precompiled PTX/cubin + driver API** | CUDA toolkit on *our* build machine | driver only | works on any user machine with an NVIDIA driver — **preferred pattern for shipping our own kernels** |
| Linking a CUDA C++ library (libcubwt, libbsc `-G`) | toolkit + nvcc | `cudart` (static-link it) or the DLL | **danger: dynamically linking `cudart64_*.dll` makes the exe refuse to start on machines without CUDA.** Isolate into an optional plugin DLL loaded with `libloading`, or static-link cudart. |
| **wgpu 30.0.0** (current, 2026-07-01; 29.0.x is the previous line) | none | any DX12/Vulkan/Metal GPU | works on AMD/Intel — but **must reject `DeviceType::Cpu`** adapters or you silently "accelerate" onto WARP/lavapipe and lose 100× |
| Rust-CUDA / `cust` | pinned nightly | — | still early; avoid (unchanged from doc 08) |

WGSL-specific friction for byte-oriented compression work, worth knowing before anyone starts:

- **WGSL has no 8-bit type.** All byte work is emulated by packing into `u32`; the
  `packed_4x8_integer_dot_product` language feature adds `pack4xU8`/`dot4U8Packed` (DP4a), which
  helps but is an *optional* feature ([W3C WGSL spec](https://www.w3.org/TR/WGSL/),
  [MDN WGSLLanguageFeatures](https://developer.mozilla.org/en-US/docs/Web/API/WGSLLanguageFeatures)).
- Default WebGPU limits cap a storage buffer binding at 128 MiB; batching must request higher
  limits and handle refusal.
- wgpu **28** moved subgroup size info from `limits` to `adapter.info` (`subgroup_min_size` /
  `subgroup_max_size`, PR #8609) and 27.x clarified that barriers need both `SUBGROUP` and
  `SUBGROUP_BARRIER` (PR #8203) — so any pre-28 example code is wrong against the current 30.x
  ([wgpu CHANGELOG](https://github.com/gfx-rs/wgpu/blob/trunk/CHANGELOG.md)).
- The TDR watchdog (§2) applies to every backend, not just CUDA.

If nova ever ships GPU code, the shape is fixed by the above: **a separately loaded optional module,
probed at runtime, with a CPU implementation that is always present and always correct**, and no
GPU-only bytes in the format. That is already the recorded GPU policy in `ROADMAP.md`; nothing in
this report changes it.

---

## 10. What would have to change for any of this to become worth building

Kept explicitly, because the answer today is "no" and the reasons are contingent:

1. **A faster host link.** Gen5 x8 (~25 GB/s) instead of Gen3 x8 would move the break-even line from
   ~3 GB/s to ~12 GB/s and put order-1 statistics, dense MinHash and byte transforms in play. This
   is a motherboard change, not a code change — and it would still not create a GPU LZMA.
   *Second-order version of the same lever:* the break-even line is drawn against **idle** CPU
   throughput. A saturated compressor lowers spare CPU throughput ~5× (§1), which moves the line the
   same direction as a faster bus without any hardware change — the reason doc 08 treats offload as a
   responsiveness feature rather than a throughput feature.
2. **A BWT codec in the format, at a block size ≤ the chunk cap.** Then libcubwt is 6–11× per block
   on the heaviest stage. (bsc's headline 6.5× end-to-end is a 318 MB-block number and does not
   transfer — §7.1.)
3. **A small-unit + shared-dictionary format variant** (64 KiB–1 MiB units) adopted for *edit-cost*
   reasons, never for ratio: it is +69 % worse than 32 MiB blocks even with the dictionary (§4.2).
   In that world dictionaries are worth −3…−10 %, training becomes a real cost, and a GPU trainer
   would need writing from scratch — but the multithreaded CPU trainer (§4.1) must be exhausted first.
4. **Archives in the tens of GB with clustering on the critical path.** Then GPU MinHash is the
   cleanest offload in this report.
5. **A GPU codec whose ratio is competitive.** Does not exist; would make every "if the data were
   already resident" caveat in this report evaporate at once.

---

## 11. Reproducing the measurements

All local benchmarks live in the gitignored playground `test/gpubench/` (standalone cargo project,
deps `blake3`, `zstd` with `experimental`+`zstdmt`, `walkdir`):

| Binary | What it measures |
|---|---|
| `src/main.rs` | memcpy, blake3 (1/8 threads), order-0 and order-1 entropy, trial zstd probes, per-file dictionary payoff |
| `src/bin/dict.rs` | fastcover training cost scaling 1 vs 8 threads vs pinned `k`; COVER; first (flawed) block payoff run |
| `src/bin/dictctl.rs` | control: streaming vs bulk API — proves the API is not the cause (±0.1 %) |
| `src/bin/dict3.rs` | final control: `none` vs `loadDictionary` vs `refPrefix` with `windowLog` and `pledgedSrcSize` pinned |
| `src/bin/sketch.rs` | MinHash / super-feature sketching throughput, full coverage and sampled |
| `src/bin/verify.rs` | verification pass: best-of-N re-measurement of blake3 / order-0 / order-1 on 1 and 8 threads plus a pure DRAM stream-read ceiling. **Use best-of-N, not single runs** — with other work on the machine the same binary reports 5.5 GB/s for 8-thread blake3 instead of 28–32 |

Run e.g. `cargo run --release --offline --bin dict3 -- ../corpus`, `--bin verify -- 12`. Hardware
facts come from `nvidia-smi -q`; note `PCIe Generation / Current` reads 2 when the link is idle and
3 under load, so read `Max`/`Host Max`, not `Current`. No CUDA toolkit is installed on this machine,
so **no GPU-side number in this report was measured here** — all of them are cited.

---

## 12. Sources

- Hardware / bus: [NVIDIA — How to Optimize Data Transfers in CUDA](https://developer.nvidia.com/blog/how-optimize-data-transfers-cuda-cc/) ·
  [Microsoft — WDDM TDR](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/timeout-detection-and-recovery) ·
  [TDR registry keys](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/tdr-registry-keys) ·
  [Intel — adjusting TDR](https://www.intel.com/content/www/us/en/docs/oneapi-toolkit/installation-guide-windows/latest/gpu-adjust-timeout-detection-and-recovery-setting.html)
- Dictionaries: [zstd `zdict.h`](https://github.com/facebook/zstd/blob/dev/lib/zdict.h) ·
  [zstd(1) man page](https://github.com/facebook/zstd/blob/dev/programs/zstd.1.md) ·
  [issue #1572 — training time in the wild](https://github.com/facebook/zstd/issues/1572) ·
  [issue #1654 — cover vs fastcover](https://github.com/facebook/zstd/issues/1654)
- GPU k-mer counting (nearest art to a GPU trainer): [Gerbil](https://github.com/uni-halle/gerbil) ·
  [Gerbil paper](https://almob.biomedcentral.com/articles/10.1186/s13015-017-0097-9) ·
  [RapidGKC (ICDE'24)](https://ieeexplore.ieee.org/document/10598059) ·
  [Distributed-memory GPU k-mer counting](https://ieeexplore.ieee.org/document/9460480)
- Similarity / reordering: [Migratory Compression, FAST'14](https://www.usenix.org/system/files/conference/fast14/fast14-paper_lin.pdf) ·
  [FED/SEDD (arXiv 2501.01046)](https://arxiv.org/abs/2501.01046) ·
  [SEDD, SIGKDD'26](https://dl.acm.org/doi/10.1145/3770855.3818161) ·
  [github.com/mcrl/SEDD](https://github.com/mcrl/SEDD) ·
  [NeMo Curator fuzzy dedup](https://docs.nvidia.com/nemo/curator/curate-text/process-data/deduplication/fuzzy)
- Estimation: [FAST'13 — Effective Resource Usage for Real-Time Compression](https://www.usenix.org/system/files/conference/fast13/fast13-final38.pdf) ·
  [Black-box statistical prediction of compression ratios](https://arxiv.org/pdf/2305.08801) ·
  [ClickHouse adaptive codec selection RFC](https://github.com/ClickHouse/ClickHouse/issues/105404)
- BWT/SA on GPU: [libcubwt](https://github.com/IlyaGrebnov/libcubwt) ·
  [libbsc](https://github.com/IlyaGrebnov/libbsc) ·
  [bsc GPU benchmark thread](https://encode.su/threads/586-bsc-new-block-sorting-compressor/page15) ·
  [libsais](https://github.com/IlyaGrebnov/libsais)
- ANS: [dietgpu](https://github.com/facebookresearch/dietgpu) · [MANS](https://github.com/hpdps-group/MANS) ·
  [MANS SC'25](https://dl.acm.org/doi/10.1145/3712285.3759825) · [hipANS](https://github.com/PAA-NCIC/hipANS) ·
  [Recoil ICPP'23](https://dl.acm.org/doi/10.1145/3605573.3605588)
- Rust: [cudarc](https://crates.io/crates/cudarc) · [cudarc features](https://lib.rs/crates/cudarc/features) ·
  [wgpu CHANGELOG](https://github.com/gfx-rs/wgpu/blob/trunk/CHANGELOG.md) ·
  [WGSL spec](https://www.w3.org/TR/WGSL/) ·
  [MDN WGSLLanguageFeatures](https://developer.mozilla.org/en-US/docs/Web/API/WGSLLanguageFeatures)
