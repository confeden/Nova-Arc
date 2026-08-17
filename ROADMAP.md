# Nova Arc — working context (Claude-only, English)

## Current state

- Cargo workspace, Rust 1.95, edition 2021: `narc-core` (format,
  `#![forbid(unsafe_code)]`), `narc-cli` (binary `narc`), `narc-platform`
  (all OS/unsafe code: priorities, I/O hints, memory status, file lock).
- NARC v0 container implemented and VERIFIED: append-only chunk log, FastCDC
  256K/1M/4M, blake3-128 dedup+integrity, MessagePack(named)+zstd manifest,
  80-byte offset-bound footer, generation counter, resumable crash recovery,
  rw-open truncates uncommitted tail, `compact` with verify + atomic replace.
  Spec: `docs/format.md`.
- Multi-threaded packing pipeline (`pipeline.rs`): reader hashes+dedups,
  worker pool compresses, writer appends in submission order; byte-budget
  backpressure. Extraction pool exists but defaults to 1 worker (see negative
  knowledge). CLI: `-j/--threads`, `--memory`, `--eco`, `--full`, and packing
  prints peak RAM.
- CLI: create/add/extract/list/remove/compact/info (+aliases c/a/x/l/rm),
  extract has `--force` / `--skip-existing` (default: refuse to clobber).
- 22 tests green (`cargo test`), clippy clean. Covers roundtrip,
  append-without-rewrite, dedup, replace, remove+compact, selective extract,
  crash recovery, torn-manifest fallback, embedded-footer confusion, forged
  footer, writer lock, selector normalization, pre-1970 mtime, overwrite
  policy, compact-detects-corruption.
- Measured (8 logical cores, this machine, `bash test/bench.sh`):
  · 106 MiB / 4 big files: normal 1.8 s (‑j1: 5.5 s → 2.9× scaling), max 6.0 s
  · 114 MiB / 5751 small files: fast 6.5 s, normal 6.7 s, max 8.0 s
    (reader-bound: small files never reach the worker pool; solid grouping
    is the fix, planned in v0.5)
  · peak RAM tracks `--memory`: 128M→~100 MiB, 256M→~196 MiB, default→~340 MiB
  · extraction 113 MiB in ~1-2 s at ~10 MiB peak RAM
  · 46 MiB tree, edit 1 file → re-save 0.14 s, archive grows ~98 KiB
- `test/` = local playground (gitignored): corpus, bench.sh, RU readme.
- GUI, zip/7z/rar support, GPU: not started.
- Research reports live in `docs/research/01..09-*.md`.

## Architecture & invariants

- Archive layout: `[header][manifest g1][footer g1]` from `create`, then
  `[chunks…][manifest gN][footer gN]` per update; committed bytes are NEVER
  rewritten except by `compact`.
- Commit = manifest write → fsync → footer write → fsync (the barrier is
  required: without it a valid footer can point at a torn manifest).
- Footer self-hash covers its own absolute offset, so a `.narc` stored inside
  another archive cannot be mistaken for a commit. Readers verify the
  manifest of each footer candidate and resume the backward scan on failure
  (≤64 candidates).
- Packing invariant: the writer appends chunks in submission order, so the
  reader predicts each chunk index as `base + submission index` and builds
  file entries without waiting for compression.
- Memory model: `budget ≈ 32 MiB base + workers × (zstd tables + 8 MiB) +
  queued chunks`; workers are capped by the budget, then in-flight bytes are
  capped by the workers. zstd table cost measured per tier in
  `Tier::worker_memory()` (fast 4, normal 40, max 56 MiB).
- Chunk hash = blake3(uncompressed)[..16]; serves dedup AND extract integrity.
  Dead (unreferenced) chunk records stay in manifest as dedup sources until compact.
- Codec ids: 0=store, 1=zstd. Per-chunk raw fallback if compression doesn't pay.
- Two-phase pipeline: phase 1 analysis (`analyze.rs`: magic list + 64 KiB
  trial compress), phase 2 per-chunk compression. Tiers: fast=zstd3,
  normal=zstd12, max=zstd19.
- Memory invariant: all ops bounded by a few MAX_CHUNK (4 MiB) buffers +
  manifest in RAM. Keep it that way — weak-PC extraction is a hard requirement.
- Archive paths: relative, UTF-8, '/'-separated; `paths::sanitize` on extract
  rejects traversal/absolute/drive/ADS/reserved-device/trailing-dot names.
- Owner's machine: 32 GB RAM (often half used), RTX 5060 Ti 8 GB, Windows 11.

## Gotchas

- `tempfile` must be a REGULAR dep of narc-core (compact uses it), not dev-dep.
- Windows: cannot rename over an open file — compact consumes `self`, closes
  the handle, then `NamedTempFile::persist` (atomic replace, no .bak window).
- Windows file locks are MANDATORY per byte range: a whole-file
  `File::try_lock` makes our OWN extraction threads fail with ERROR_LOCK_
  VIOLATION (33). Hence `narc_platform::try_lock_exclusive` locks one byte at
  offset 0xFFFF_FFFF_FFFF_0000 — a pure mutex range, never real data.
- `File::try_clone` SHARES the file position on Windows: extraction workers
  must each `File::open` the archive, never clone a handle.
- zstd already shrinks its window to the source size, so capping WindowLog
  for 4 MiB chunks changes nothing; per-worker memory is match tables.
- A formatter hook rewrites files after every Write/Edit in this repo.
- zstd/blake3 crates build C via MSVC — fine here, but keep pure-Rust
  fallbacks in mind for exotic targets.
- GitHub repo confeden/Nova-Arc pre-existed with a 2-line README initial
  commit; local main is based on it.
- git global identity: user.name "Brent", email confeden@cryptolab.net.

## Resource policy (decided, from research 09)

- Default: `SetPriorityClass(BELOW_NORMAL)` + per-handle
  `SetFileInformationByHandle(FileIoPriorityHintInfo=Low)` +
  `SetProcessInformation(ProcessMemoryPriority=BELOW_NORMAL)`. CPU priority
  alone does NOT prevent UI lag — I/O and memory pressure do it.
- Threads = ALL logical cores (`available_parallelism`), workers at
  THREAD_PRIORITY_NORMAL inside the lowered class. No affinity pinning,
  no "leave one core free".
- Memory budget = clamp(min(0.5·avail, 0.25·total), 512 MiB, 8 GiB) via
  GlobalMemoryStatusEx; `--memory` override; workers W = min(T, budget/per_worker)
  with per_worker from ZSTD_estimateCCtxSize; job-object COMMIT limit at 2×
  budget as guardrail (never working-set caps).
- Pipeline: hand-rolled thread pool + bounded crossbeam channels (2·W),
  global in-flight-bytes semaphore, single reader + single ordered writer.
- `--eco` (opt-in): IDLE class + EcoQoS + IoPriorityHintVeryLow.
  `--full`: NORMAL class + EcoQoS off.

## GPU policy (decided, from research 08)

- GPU is an OPTIONAL accelerator behind a codec-provider trait + `gpu` cargo
  feature + `--gpu auto|on|off`; never a format dependency.
- Only standard bitstreams (zstd/LZ4) via nvCOMP batched API, loaded
  dynamically at runtime (nvcomp.dll never vendored — proprietary EULA).
- VRAM budget ~2 GiB default on 8 GB cards; batches 256–512 MiB double-buffered;
  auto-mode skips GPU for jobs < ~256 MB.
- Rust: cudarc (mature, dlopen) + own small `-sys` binding for nvCOMP.

## Negative knowledge

- Creating RAR archives is legally impossible (RARLAB license); only
  extraction via unrar is allowed. Plan pack: zip/7z/narc; unpack adds rar.
- `PROCESS_MODE_BACKGROUND_BEGIN` — drops I/O and memory priority to Very Low
  (~250× slowdowns, 32 MiB WS squeeze); compose the three APIs instead.
- IDLE class or EcoQoS as DEFAULT — starved by daemons / frequency-clamped on
  non-hybrid AMD. IoPriorityHintVeryLow as default — 1–3% of disk, self-starvation.
- Job-object working-set caps and CPU-rate control — paging churn / caps CPU
  even when idle. WinRAR-style sleep injection — wastes idle cores.
- rayon global pool / par_bridge as pipeline backbone — no backpressure,
  no ordering, no priorities.
- libzstd internal MT (`ZSTD_c_nbWorkers`) — breaks memory estimation and
  duplicates our chunk parallelism. zstd `--long`/window ≥128 MiB — pointless
  under 4 MiB CDC chunks and breaks bounded extraction.
- Parallel EXTRACTION with zstd — measured slower, not faster: 5751 small
  files take 1.0 s on 1 thread, 1.2 s on 4, 2.5 s on 8 (NTFS directory
  metadata contention + seeks); big files show no gain either (~660 MB/s = 
  I/O speed). Default extract workers = 1; revisit when slow-decoding codecs
  (LZMA/PPMd) land. `-j` still honoured.
- GPU for blake3/dedup — slower than CPU SIMD, PCIe erases gains.
- GPU high-ratio compression (LZMA-class) — does not exist in 2026; GPU codecs
  land near zstd-1..3 ratios. Blackwell HW decompression engine is
  datacenter-only (RTX 5060 Ti does NOT have it).
- Dead/unusable GPU projects: dietgpu (archived), Brotli-G (dormant), multians,
  CULZSS, GST, Gstd (author: decoder non-functional). Rust-CUDA/cust — early stage.
- nvCOMP proprietary codecs (ANS/Bitcomp/Cascaded) inside .narc — would make
  archives unreadable without an NVIDIA GPU.

## Owner decisions

- Language: Rust. Open repo `confeden/Nova-Arc`; do NOT add a LICENSE file
  yet (owner will choose later). Zero telemetry/ads/analytics — ever.
- Chat with owner in Russian; code/docs/CLI output in English; GUI must ship
  RU localization.
- Two-phase compression (analyze → compress) is a core requirement, as is
  editing archives without repack (this is THE differentiator).
- Resource policy: use all cores but at below-normal priority (system must
  stay responsive); bounded, configurable memory (weak PCs: extract must
  always work); GPU (CUDA/nvCOMP-class) acceleration to be attempted.
- Test everything under `test/` (gitignored playground).

## Open issues

- GUI framework decision pending owner confirmation (research report 06).
- Not preserved yet: empty dirs, symlinks, NTFS attrs/ADS, ACLs.
- Manifest fully in RAM (fine ≤ ~1 TB archives); paged index only if needed.
- Long-path (>260 chars) handling on Windows untested.
- Many-small-files packing is reader-bound (single reader thread): needs
  parallel file reading and/or solid grouping.
- `--eco`/`--full`/EcoQoS paths are built but not measured under load; the
  "no lag" claim is unverified (research 09 §10 has the methodology).

## Plans

1. v0.2: apply confirmed review findings; multithreaded chunk pipeline with
   bounded memory + EcoQoS/priority polish; benchmarks vs 7z/FreeArc in test/.
2. v0.3: zip pack/unpack + 7z unpack (sevenz-rust2), rar unpack (unrar crate);
   unified `narc` UX over foreign formats.
3. v0.4: GUI skeleton (framework per owner decision), file list + icons +
   thumbnails, drag&drop, temp-open flow with watcher.
4. v0.5: compression v2 — filters (delta/BCJ), solid small-file groups,
   dictionary per file-type group; max tier upgrade per research (07/01).
5. v0.6: recompression pipeline for deflate/JPEG/MP3 (research 02).
6. Later: GPU acceleration experiments (nvCOMP/GDeflate, research 08),
   encryption (XChaCha20-Poly1305), Explorer shell integration (research 07),
   installers, Linux/macOS/Android ports.
