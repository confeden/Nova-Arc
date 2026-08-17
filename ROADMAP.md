# Nova Arc — working context (Claude-only, English)

## Current state

- Cargo workspace: `crates/narc-core` (format library, `#![forbid(unsafe_code)]`)
  + `crates/narc-cli` (binary `narc`). Rust 1.95, edition 2021.
- NARC v0 container implemented and VERIFIED: append-only chunk log, FastCDC
  256K/1M/4M, blake3-128 dedup+integrity, MessagePack(named)+zstd manifest,
  80-byte self-checked footer at EOF, generation counter, backward-scan crash
  recovery, rw-open truncates uncommitted tail, offline `compact` with atomic
  swap (rename dance, .bak fallback). Spec: `docs/format.md`.
- CLI: create/add/extract/list/remove/compact/info (+aliases c/a/x/l/rm).
  Process runs at BELOW_NORMAL priority on Windows (narc-cli/src/main.rs).
- 11 tests green (`cargo test`): roundtrip, append-without-rewrite, dedup,
  replace, remove+compact, selective extract, crash recovery, hostile-file
  rejects, no-clobber create. Cyrillic paths + selective extract verified
  manually. Real-data smoke: 46 MiB tree → edit 1 file → re-save 0.14 s,
  +98 KiB growth; 20 MB duplicate deduped.
- `test/` dir = local playground (gitignored), sample data + RU readme.
- GUI, zip/7z/rar support, multithreading, GPU: not started.
- Research reports live in `docs/research/01..09-*.md`.

## Architecture & invariants

- Archive layout: `[header][chunks…][manifest gN][footer gN]` repeated per
  update; committed bytes are NEVER rewritten except by `compact`.
- Commit point = footer fsynced at EOF. Readers: footer at EOF−80, else
  backward scan for last self-valid footer. Update durability is all-or-nothing.
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
  handle, then rename-to-.bak + persist + rollback on error.
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
- `narc extract` overwrites existing files silently — decide policy.
- Long-path (>260 chars) handling on Windows untested.
- Multithreaded compression pipeline (rayon, bounded queue backpressure) not
  yet implemented; single-threaded today.

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
