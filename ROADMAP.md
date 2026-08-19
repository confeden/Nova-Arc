# Nova Prism — working context (Claude-only, English)

## Current state

- Cargo workspace, Rust 1.95, edition 2021: `nova-core` (format,
  `#![forbid(unsafe_code)]`), `nova-cli` (binary `nova`), `nova-platform` (OS
  and unsafe), `nova-bsc` (libbsc FFI), `nova-gui` (Tauri 2). Frontend `ui/`
  (TS + Vite, no framework).
- Format v0.2 = v0.1 + solid blocks + per-chunk filter/param bytes + LZMA2/PPMd7
  + manifest `geometry`. Container VERIFIED: append-only chunk log, FastCDC,
  blake3-128 dedup+integrity, MessagePack+zstd manifest, 80-byte offset-bound
  footer, generation counter, resumable crash recovery, rw-open truncates the
  uncommitted tail, `compact` verifies then replaces atomically. `docs/format.md`.
- Packing pipeline (`pipeline.rs`): reader hashes+dedups, worker pool
  compresses, writer appends in submission order, byte-budget backpressure.
  Extract threads key off the archive's codecs. CLI: `-j`, `--memory`, `--eco`,
  `--full`; packing prints peak RAM.
- CLI: create/add/extract/list/remove/rename/compact/info (+aliases), extract has
  `--force` / `--skip-existing` (default: refuse to clobber).
- `rename` moves an entry or a whole folder by rewriting the manifest ONLY:
  77 entries in 0.053 s, zero new units, byte-identical output.
- GUI (nova-gui, VERIFIED running): open/create/add/extract/remove/compact,
  virtualized list with glyphs + unit badges, sortable columns, multi-select,
  context menu, double-click opens from a temp dir, Explorer drag&drop both ways,
  throttled progress, level/memory in toolbar (DEFAULT max), argv[1] opens.
- 95 tests green, clippy clean: roundtrip, append without rewrite, dedup,
  replace, remove+compact, rename, selective extract, crash recovery, torn
  manifest, embedded/forged footer, writer lock, selectors, pre-1970 mtime,
  overwrite policy, compact-detects-corruption, the progress contract,
  deflate/JPEG/PDF/WAV round-trips, the PDF traps, the record-width filter on
  interleaved PCM, one large file through the decode lanes, and the
  `legacy-*.nva` fixtures.
- Compression v2 DONE: codec+filter per content class, solid blocks by extension,
  LZMA2 + PPMd7 + bsc, BCJ x86 + delta (BCJ verified against liblzma).
- Measured residue: BCJ on real .exe +4.4-5.7% vs unfiltered. PPMd7 vs zstd-19
  on 4 MiB prose -24%; LZMA2 vs zstd-19 on prose ±2%. Peak RAM tracks `--memory`
  on EXTRACT too (256M→153 MiB, default→1.0 GiB) — bounded, but "~10 MiB peak"
  is long gone, units are the cost.
- `test/` = local playground (gitignored): corpora, benches, RU readme. zip/7z/
  rar and GPU: not started. Research: `docs/research/`.

## Architecture & invariants

- Archive layout: `[header][manifest g1][footer g1]` from `create`, then
  `[chunks…][manifest gN][footer gN]` per update; committed bytes are NEVER
  rewritten except by `compact`.
- Commit = manifest write → fsync → footer write → fsync (the barrier is
  required: without it a valid footer can point at a torn manifest).
- Footer self-hash covers its own absolute offset, so a `.nva` stored inside
  another archive cannot be mistaken for a commit. Readers verify each footer
  candidate's manifest and resume the backward scan on failure (≤64).
- Packing invariant: the writer appends chunks in submission order, so the
  reader predicts each index as `base + submission index` and builds file
  entries without waiting for compression.
- PROGRESS CONTRACT (`Progress`, `Phase`, `archive::Reporter`). `bytes_done` =
  source bytes whose work is FINISHED, fed by the writer; `bytes_read` = what
  the reader took off disk, up to a whole in-flight budget ahead. Monotone,
  never above the total, equal to it exactly once — the single `Phase::Done`
  reading, since others are clamped to total-1 so "100%" structurally means
  finished. INVARIANT: the Reporter mutex is never held while entering the
  pipeline, or the reader sleeps in `Budget::acquire` holding it and stalls the
  writer. Throttling lives in core; clocks live in the GUI — do NOT add a timer
  thread to core.
- Memory model: `budget ≈ 32 MiB base + workers × (tables + 8 MiB) + queued`;
  workers capped by the budget, then in-flight bytes capped by the workers.
  Table cost per tier in `Tier::worker_memory()` (fast 4, normal 40, max 56).
- Chunk hash = blake3(uncompressed)[..16]; serves dedup AND extract integrity.
  Dead chunk records stay in the manifest as dedup sources until compact.
- INTRA-FILE DECODE LANES. Threads the file count cannot use become lanes
  INSIDE each file (`lanes_per_worker = budget / workers`), so a one-file
  archive stops using one thread while total concurrency, memory, ordering,
  overwrite policy and mtime all stay put. enwik8 at normal 4.80 → **1.63 s**.
  · GROUP BY DISTINCT UNIT, not by extent: consecutive extents usually share a
    unit and the sequential path got that free from `UnitCache`. Per extent it
    decoded the same unit once per lane — enwik8 4.8 → 8.2 s. Each lane needs
    its OWN `File` (a clone shares the position on Windows); running out of
    handles means fewer lanes, not an error.
- PPMd7 order (10) and pool formula (32x chunk, cap 64 MiB) are FORMAT
  CONSTANTS — not stored per chunk, so changing them breaks old archives. Order
  10 beat 12/16: a pool-exhaustion restart costs more than depth gains.
- LZMA2 presets 6..9 differ only in dictionary size, which we override with the
  chunk cap, so max raises nice_len instead (xz -e style).
- Codec and filter ids are tabulated in `docs/format.md`; keep it in step. A
  NEW id must never be added by widening the delta range — `2..=MAX_DELTA_ID =>
  Delta(id - 1)` would have made 34 decode as Delta(33), silently. Per-chunk raw
  fallback if not smaller, and then the filter byte MUST be cleared too.
- Fixed order: pack = filter → compress; unpack = decompress → unfilter. The
  chunk hash covers the ORIGINAL bytes, so dedup and integrity are
  filter-independent, and the hash check proves the filter round-tripped.
- BCJ always runs with start_offset 0, never the chunk's file position —
  position-dependent output would break dedup. Cost: one unconverted
  instruction per boundary.
- Solid blocks: files < `Geometry::chunked_from` (256 KiB fast/normal, 1 MiB
  MAX) are sorted by extension, concatenated into `Geometry::unit` streams
  (4/16/32 MiB) and compressed as ONE unit; a FileEntry holds one `Extent` into
  it. Size is bounded ON PURPOSE: editing one member rewrites one block. Cuts
  are content-defined per file with a hard flush at 2x target, so realized
  blocks run LARGER than target, never smaller.
- A content-defined cut may only fire at or above HALF the target: the low tail
  is pure loss, and it made boundaries LESS stable. MEASURED both ways.
- A unit's codec+filter come from a BYTE-MAJORITY VOTE of the per-file verdicts,
  never from the unit's head sample (`Packer::unit_plan`). One sub-4 KiB `.flac`
  ahead of `.go` made the head say "already compressed" and 8.36 MB of source
  was stored raw. Only a unit with NO voters reads the head.
- REALIZED unit geometry (test/corpus at max): 6 units, size-weighted mean
  41.42 MiB, 79% of bytes in units >= 16 MiB.
- Two-phase pipeline: `analyze::plan()` (magic → class → trial) gives
  codec+filter, then per-chunk compression. fast=zstd3, normal=zstd12+bsc,
  max=LZMA2/PPMd7/bsc.
- `HEAD_SAMPLE` = 1 MiB, load-bearing: deflate leaves matches megabytes apart,
  so a level-1 zip reads as +0.02% on a 64 KiB sample and −25.6% on a 1 MiB one.
  Free — `add_file` chains the head in front of the file. Sub-tests keep their
  caps (`TRIAL_SAMPLE` 64 KiB; the delta detector MUST stay capped).
- Precompressed magic does NOT mean store — it is a claim about the FORMAT, not
  the bytes; storing on it alone cost 1.12 MB on a 4.93 MiB corpus. A 1 MiB
  zstd-1 trial must save >= 1%: compressible deflate lands at −3.9..−25.6%,
  finished data at +0.00..0.01%.
- Memory invariant: ops bounded by a few MAX_CHUNK buffers + the manifest.
  Weak-PC extraction is a hard requirement.
- Archive paths: relative, UTF-8, '/'-separated; `paths::sanitize` on extract
  rejects traversal/absolute/drive/ADS/reserved-device/trailing-dot names.
  Owner's machine: 32 GB RAM, RTX 5060 Ti 8 GB, Windows 11, 8 logical cores.

## Gotchas

- `tempfile` is a REGULAR dep of nova-core (compact uses it), not a dev-dep.
- Windows: cannot rename over an open file — compact consumes `self`, closes the
  handle, then replaces in place (atomic, no .bak window).
- Windows file locks are MANDATORY per byte range: a whole-file `File::try_lock`
  makes our OWN extraction threads fail with ERROR_LOCK_VIOLATION (33). Hence
  `try_lock_exclusive` locks one byte at 0xFFFF_FFFF_FFFF_0000 — never real
  data.
- `File::try_clone` SHARES the file position on Windows: extraction workers
  must each `File::open` the archive, never clone a handle.
- GUI: run `npm --prefix ui run build` FIRST. There must be no `devUrl` in
  tauri.conf.json — it made every non-Tauri-CLI build come out in dev mode, so
  the shipped exe opened localhost:5173 and a plain `cargo build --release`
  silently overwrote a good exe with that one, twice. Every build now embeds
  `ui/dist` and build.rs FAILS if `ui/dist/index.html` is missing.
- Windows: pass a path with spaces to the exe in QUOTES or Start-Process
  -ArgumentList splits it and argv[1] is truncated (this fooled a GUI test).
- .cmd launchers mangle Cyrillic (OEM codepage); use a BOM'd .ps1 instead.
- `zpaqfranz x ... -to DIR` needs a TRAILING SLASH or it refuses, and a benchmark
  records the refusal as the decode time.
- zstd already shrinks its window to the source size, so capping WindowLog
  for 4 MiB chunks changes nothing; per-worker memory is match tables.
- A formatter hook rewrites files after every Write/Edit here. zstd/blake3/libbsc
  build C or C++ via MSVC.
- git global identity: user.name "Brent", email confeden@cryptolab.net.

## Resource policy (decided, from research 09)

- Default: `SetPriorityClass(BELOW_NORMAL)` + per-handle
  `SetFileInformationByHandle(FileIoPriorityHintInfo=Low)` +
  `SetProcessInformation(ProcessMemoryPriority=BELOW_NORMAL)`. CPU priority
  alone does NOT prevent UI lag — I/O and memory pressure do it.
- Threads = ALL logical cores at THREAD_PRIORITY_NORMAL inside the lowered
  class. No affinity pinning, no "leave one core free".
- Memory budget = clamp(min(0.5·avail, 0.25·total), 512 MiB, 8 GiB) via
  GlobalMemoryStatusEx; `--memory` override; workers W = min(T, budget/per_worker);
  job-object COMMIT limit at 2x budget (never working-set caps).
- `--eco` (opt-in): IDLE class + EcoQoS + IoPriorityHintVeryLow.
  `--full`: NORMAL class + EcoQoS off.
- GPU (research 08), if ever: a codec-provider trait behind `--gpu auto|on|off`,
  standard bitstreams only, nvCOMP at runtime and never vendored. PARKED.

## Compression design (measured, v0.6 max tier)

- No single codec wins: PPMd7 beats LZMA2 13-24% on prose, LZMA2 beats PPMd7
  16% on binaries. MAX runs a per-unit tournament (LZMA2 + PPMd7 orders 10 & 16);
  fast/normal trust the analyzer.
- Chunk size = compression unit = edit granularity. fast/normal 256K/1M/4M;
  max 1M/4M/16M (FastCDC caps max at 16 MiB). Solid target 4/16/32 MiB by tier.
- Solid block boundaries are content-defined PER FILE (cut prob = size/target
  from the file's own blake3). A size-accumulator rule cost 17 MiB for a
  one-line edit, because one file's length shifted every later boundary.
- RECORD-WIDTH FILTER: the detector proposes from entropy, `pays_off` decides,
  and BOTH of its terms are load-bearing. It judges on the WHOLE 1 MiB head — a
  Firefox XML unit read as 4-byte records in its first 64 KiB and cost 176 KB
  over the rest — and it TRIALS WITH BSC from normal up, zstd only at fast.
  · Gating the detector on "already compresses" cost 27% on 16-bit stereo PCM,
    which lands at 82% unfiltered and so never met the filter. Gate removed:
    decoded music 439,050,506 → **341,071,616 (−22.3%)**, which is 0.5% BELOW
    7z -mx9's 342,942,386 where we had been 28% ABOVE it; source tree −0.19%,
    every other corpus byte-identical.
  · A ZSTD VERDICT CANNOT SPEAK FOR BSC and no threshold fixes it. 1 MiB heads,
    zstd margin / bsc margin: x-ray −25.7% / **+0.9%**, mr −12.4 / +1.6, sao
    −0.7 / +5.6, a 16-bit stereo .wav −20.5 / **−28.7**. x-ray's zstd margin is
    the LARGER one, so no threshold takes the right side; judged by zstd the
    filter costs 0.77% on Silesia. Same root cause as the byte-plane split.
- WHERE WE STAND (`test/bench-std.sh`). Competitor columns predate bsc; nova's
  are current. Max tier unless noted.
  · enwik8  nova **21,506,314** · zpaqfranz -m5 19,625,056 · kanzi -l9
    20,035,684 · 7z -mx9 24,799,487 · xz 24,831,656
  · Silesia nova **43,036,408** · zpaqfranz -m5 39,865,713 · kanzi -l9
    41,857,930 · 7z -mx9 48,688,268 · xz 48,449,928
  · Source tree: nova 9,292,017 vs 7z 9,131,720 → +1.8%.
- PPMd7 AS THE TEXT CODEC IS DOMINATED (enwik8): kanzi -l9 is smaller and faster
  both ways, and bsc now out-votes it on text units.
- nova max is NOT faster than 7-Zip: Silesia 52.7 s vs 44.4 s, enwik8 77.7 s vs
  43.3 s. The old "6.8 s vs 49 s" predates the tournament; never requote it.
- DECODE is DATA-DEPENDENT: Silesia nova 1.8 s vs kanzi -l9 50.5 s; enwik8 4.6 s.
- kanzi -l7: Silesia 47,308,780 B in 6.15 s, the fast tier's bar. CAVEAT: kanzi's
  default `-j` is HALF the cores, so bench-std gave it 4 threads against nova's
  8; bwt-sweep and scaling pass `-j`.
- BWT wins on SOME units and loses on others (`test/bwt-sweep.sh`), which is
  what a per-unit tournament settles.
  · enwik8 block sweep: 8 MiB 23,593,148 · 32 21,983,674 · 128 20,803,016 —
    block sensitivity is MILD on text. Source tree is the OPPOSITE: bsc -b32
    13,474,736 against nova's 9,292,017, 45% worse. Architecture, not codec.
  · SHIPPED as codec id 4 (`crates/nova-bsc`, libbsc 3.3.12, Apache-2.0), a
    fourth MAX candidate: enwik8 22,466,101 → **21,506,314 (−4.3%)** with bsc
    winning every unit · Silesia 43,674,657 → **43,036,408 (−1.5%)**, split
    lzma2 22.5 / bsc 18.6 MiB · source tree UNCHANGED. Encode +8%.
  · Wiring: `bsc_qlfc_init_model` memcpy's the global model per call, so workers
    are safe after one `bsc_init`. `bwt.cpp`/`st.cpp` include their CUDA headers
    UNCONDITIONALLY — vendor both. Windows needs `advapi32`; C and C++ need
    separate `cc::Build`s. No Rust port; `libsais-rs` covers the sa core.
  · ALSO AT NORMAL, the bigger win: enwik8 30,269,637 → **22,827,939 (−24.6%)**,
    Silesia 57,362,566 → **46,442,936 (−19.0%)**, pdfs −10.0%, precomp −9.0%,
    source tree −6.2%, for 1.6-1.9x the encode. Normal now beats 7z -mx9 on
    Silesia in a SIXTH of its time.
  · TRAP: `bsc` decodes 100 MB in 0.9 s, which reads like 110 MB/s — but that is
    multithreaded across blocks and nova disables libbsc's own threads. Single
    -threaded it is ~25 MB/s; treating it as fast in `extract_workers` made a
    normal Silesia archive extract in 8.7 s against zstd's 0.5. Quote the cost:
    normal-tier Silesia 0.5 → 2.1 s for −19%.
  · FAST STAYS WITHOUT IT, measured twice, the second time after decode lanes
    landed — which the first refusal had named as its condition. Fast with bsc
    would be 47.8 MB / 2.90 s pack / 3.19 s extract against NORMAL's 46.4 /
    7.0 / 2.09: better only in pack time. Not a fast tier, a worse normal one.
  · libbsc is C++ and nova-core is `#![forbid(unsafe_code)]`, so it needs its
    own crate.
- Cut floor + byte-majority verdict, on test/corpus when they landed: fast −7.6%
  with its edit cost halved, normal −0.2%, max −4.0%. The fast win is the VOTE
  (the floor ALONE made fast 5.1 MB worse); max is the floor.
- Edit cost at max is ~4.7 MiB and ~40 s — the edited file lives in a 30-60 MB
  unit. Cheap edits are a fast/normal property (0.73 / 3.30 MiB).
- The manifest is 91-92 KB of the source tree's gap to 7-Zip — real cost, and
  the reason the manifest gets its own codec.
- MANIFEST CODEC: a manifest >= 128 KiB is offered to LZMA2 and the smaller
  result wins (682,799 B raw: zstd 19 84,565 vs LZMA2 74,690, −18.9%). The codec
  is read off the BYTES — a zstd frame starts 28 B5 2F FD and a raw LZMA2 stream
  never can — so there is no format field and every manifest ever written still
  decodes. Front-coding paths: another 7,663 B, not done, a format change.
- nova's encoder is 0.24% BETTER than liblzma -9e on identical boundaries.
- Tournament on a source tree: LZMA2 wins 5 of 6 units = 99.94% of stored bytes;
  PPMd7 order 10 won one 35 KB unit, order 16 never won.
- Diagnostics: `NOVA_UNIT_TRACE=<file>` logs one line per unit as it is built
  (idx, size, items, WHY it was cut, kind, ext histogram) — the cut reason exists
  only at pack time. `nova info --units` dumps them back out with the winning
  codec. Benches: test/{bench-std,scaling,bwt-sweep,compare-7z,edit-cost}.sh.

## Recompression — DEFLATE, JPEG and PDF ARE LANDED (research 02, measured here)

- Corpora: `test/precomp` (4.93 MiB zips/PNGs/docx/gz), `test/incompress` (7z
  output + random bytes — the control that must stay stored), `test/photos`
  (camera JPEGs), `test/pdfs` (8.14 MiB, 19 documents, four producers),
  `test/firefox` (installed program, 68.6% .dll) + `test/ff-{dll,ja,rest}`,
  `test/audio` (268 MB of FLAC) + `test/audio-wav` (the same, decoded). Probes
  live in `test/probe-preflate`, outside the workspace, and round-trip what they
  measure.
- MIN_STREAM = 64 bytes, MEASURED; 4096 cost 15 points. "A correction record
  cannot pay for itself on 2 KB" is true per FILE, false inside a unit.
- SHIPPED. Deflate corpus: nova **2,408,247 B** vs 3,988,643 (zpaqfranz -m5) as
  best of the rest → **−39.6%**. Byte-exact on all 28 files.
- JPEG (lepton, id 35) SHIPPED, for standalone photographs. test/photos, 6
  camera JPEGs: raw 17,324,730 · best of the rest 16,844,561 · **nova 13,990,872
  (−16.9%)**.
  · The stored payload is the LEPTON FORM ITSELF: already entropy-coded, no
    codec ever beat it, so `compress_job` keeps the smallest of {codec over
    filtered, filtered verbatim, original}. A Store record carrying a filter is
    safe — `filtered` records the coded length and an old reader rejects id 35.
  · Lepton settings are a FORMAT CONSTANT for id 35, like PPMd7's order and
    pool: `compat_lepton_vector_write`, single-threaded. Its 16386-pixel limit
    means gallery panoramas are not transformed.
  · Cost: lepton is slow, ~0.8 MB/s per thread.
- PDF IMAGES SHIPPED as filter id 37, a mixed container. 18.5% of a real
  19-document corpus is `/DCTDecode` — whole JPEGs, which lepton takes 20.3% off
  (39 of 39 accepted). pdfs max 5,290,810 → **5,064,071 (−4.3%)**, normal
  5,580,960 → 5,355,326; against 7z -mx9's 6,606,978 that is **−23.4%**.
  · Id 34's framing has no room to say what a stream IS, so it can only carry
    deflate. Id 37 adds a kind byte and a `NDf2` magic; the decoder reads both,
    so everything already written still opens, and 34 is decode-only now — an id
    is a promise, not a slot to reuse. The kind byte costs 1 B per stream
    (precomp +429 B). A `/DCTDecode` stream is the JPEG itself: no zlib wrapper
    to strip, and it must start at its SOI.
- PDF deflate came first (id 34, now decode-only): 7,626,464 → 5,290,810
  (−30.6%), against brotli's 6,536,147 as best of the rest.
  · THE TRAP: PDF's `/FlateDecode` is RFC 1950, so a stream carries two zlib
    header bytes and a four-byte adler32 that are NOT deflate. Handed those,
    preflate modelled **0 of 957** streams while the scanner reported 72%
    coverage — fully wired and doing nothing. Strip 2 + 4.
  · The scan is LEXICAL; "PDF needs a full object parse" was the reason it was
    deferred and it was WRONG. Rules: `stream` must follow `>>`; the FIRST name
    after `/Filter` decides the kind; `/Length` needs whitespace or `/Length1`
    (a font segment size) is read as the length; an indirect `/Length 9 0 R`
    falls back to `endstream`, a MISSING one stops the scan.
  · EVERY per-candidate backward scan needs a FLOOR at the previous candidate's
    end, not just a window: `%PDF-` + `>>stream` repeated matched neither `obj`
    nor `/Filter`, so both sweeps ran in full — 47 s for 16 MiB vs 45 ms of real
    PDF. Coverage: 74.1% FlateDecode (72.6% modelled), 18.5% DCTDecode.
    Diagnostics: `probe-preflate --bin {pdfscan,pdfhostile,lepton}`.
- WAV → FLAC SHIPPED as filter id 38 (`crate::wav`, flacenc + claxon, both pure
  Rust Apache-2.0). 518 MB of PCM: 341,071,616 → **276,221,291 (−19.0%)**,
  against 7z -mx9's 342,942,386 and zpaqfranz -m5's 335,792,867; normal
  448,646,185 → 305,746,008. Decode is 8.4 s for the corpus — claxon is cheap,
  unlike lepton.
  · ID 38 PINS THE DECODER, NOT THE ENCODER, unlike 34/35/37: the payload is a
    standard FLAC stream, so a better encoder never has to spend an id. What it
    DOES pin is the wrapper — the whole file with only the `data` payload cut
    out, spliced back on decode. That, not FLAC, is what makes the round trip
    exact: chunk order, odd sizes with pad bytes, a `RIFF` length that disagrees
    with the file and trailing garbage all survive because nothing about the
    container is rebuilt from a parse.
  · A .wav past 2x the unit gets no unit of its own and falls back to the
    GENERIC path, NOT the precompressed one: PCM is not precompressed, and
    `plan_precompressed` would drop the record-width filter too. One 71 MB file
    of 16 in the corpus lands there.
- Installed-program corpus (test/firefox, 341.7 MiB, 68.6% .dll): 7z -mx9
  87,566,439 B vs nova 95,721,462 → **+9.3%, our weakest case**. Findings:
  · The codec+filter vote must be cast per CHUNK (`Packer::current` + `place`):
    cast once at the file's start it put 150 MB of a 176 MB xul.dll in units
    with no BCJ. Worth 506,834 B.
  · The gap is ALL x86: .dll +11.8%, omni.ja +0.1%, everything else −6.2%.
  · SOLIDITY IS NOT THE CAUSE (234.3 MiB DLL set, LZMA2 -9e): solid+BCJ
    70,463,272 B · 64 MiB units 71,410,491 · 32 MiB 72,082,105 · no filter
    72,509,466. Geometry costs 1.3%, BCJ is worth 2.8%, and 7-Zip sits 8.7%
    BELOW solid-with-BCJ: the difference is the FILTER, not the size.
  · 7-Zip's advantage is BCJ2: the 4-byte call/jump targets go to separate
    streams so high-entropy addresses stop interrupting the code. `lzma-rust2`
    ships a BCJ2 DECODER only, so we wrote the transform — id 36. Firefox
    95,721,462 → **93,467,847 (−2.4%)**, source tree → 9,309,370 (−2.1%);
    gap to 7-Zip: Firefox +9.3% → **+6.7%**, source +4.1% → +1.9%.
  · THE RULE, and three that measured worse. Take a site when the absolute
    target lands INSIDE the buffer: 68,893,556 B at 64 MiB units, vs 68,994,337
    with no test, 72,488,469 with liblzma's top-byte-is-00-or-FF test (WORSE
    than not splitting — at 64 MiB the addresses land in 0..64M and the top byte
    is 0x01-0x03), and 69.5-71.1 MB gating on displacement magnitude. The WIDTH
    of the accepted range is what matters, and the unit size bounds it. Unit
    size PRICED and rejected: 128 MiB buys −1.1%, 256 MiB −4.0% and still 2.8%
    above 7-Zip. The residual is decision quality — BCJ2 learns a probability
    per site, this applies a fixed test.
  · Concatenating the split's three streams into one unit buffer costs NOTHING
    against compressing them separately (−0.01%).
  · The site COUNT is in the header and is load-bearing: the decoder walks a
    stream the displacement bytes are gone from, so at the tail it finds one
    opcode more than the encoder did.
- HOW IT IS WIRED — FORMAT RULES. `ChunkRec.filtered` (0 = same as unpacked) is
  the length the CODEC produced; `unpacked` keeps its one meaning forever, the
  ORIGINAL length that `hash` covers and `Extent` indexes. `lzma2_dict_size` and
  `ppmd7_mem_size` must derive from the CODED length on both sides or every
  existing archive stops decoding. `Filter::apply` returns `Applied::{InPlace,
  Rebuilt}` because the store fallback must undo the first and NOT the second.
  Ids 34/35/37 pin library versions; an upgrade spends a NEW id. A recompressible
  file gets a unit of ITS OWN (>= 64 KiB, <= 2x unit): the zip scanner searches
  backwards for the central directory, lepton needs one whole JPEG, the PDF
  scanner one whole PDF.
- SAFETY, enforced not intended: the packer round-trips every rebuilt unit and
  falls back on any mismatch; a filter that refuses is a fallback, not an error;
  the transformed form may not exceed `MAX_CODED_CHUNK` (256 MiB), charged as the
  pieces are BUILT and checked again before any decode allocation.
  · THE CODED-LENGTH BOUND BELONGS IN `compress_job`, beside `filtered =
    data.len()`, and nowhere else: only that line knows the number that reaches
    the manifest, and `verify_chunk` REFUSES a coded length above the cap — an
    unchecked one is an archive nova writes and cannot extract. Per-filter
    budgets do not substitute: `deflate_encode` charged plaintexts but not the
    container bytes `encode` also emits, so a ~57 MiB PDF cleared a 256 MiB
    budget and produced more.
  · Never size a Vec from a count a payload claims: `decode` bailed correctly on
    an implausible count but had already reserved for it — ~96 bytes of structure
    per three bytes of header. Grow as records are actually parsed.
- `preflate-rs 0.7.6` (Microsoft, Apache-2.0, pure Rust, forbid(unsafe)).
- TRAPS that would each have shipped a SILENT mis-decode. Fixed; recorded
  because the next length-changing filter meets every one again: the store
  fallback must compare against the ORIGINAL length and reset the coded length,
  not just the filter byte · an in-place filter MUST be unapplied on that
  fallback and a rebuilt one MUST NOT, which is why `Applied` exists · widening
  the delta range to reach a new id makes it decode as `Delta(id-1)` · the
  deflate class cannot be decided from the head, a zip's central directory is at
  the END · PPMd7's pool must match EXACTLY (LZMA2 tolerates a wider window) and
  saturates at both ends, so a length bug passes on big units and fails only in a
  band, while LZMA2 with a narrow window fails as intermittent corruption.
- COMPAT FIXTURES: `tests/fixtures/legacy-{max,normal,ppmd}.nva` are real
  pre-recompression archives; `archives_from_before_recompression_still_extract`
  extracts them fully. Any change to the derived LZMA2 window or PPMd7 pool is
  caught here and NOWHERE else. Their magic was re-signed in place when the
  format magic changed — header bytes plus BOTH footers, since the footer
  self-hash covers the magic. Payload untouched, which is what the test is for.

## Negative knowledge

- Ordering files by content similarity instead of by name — MEASURED −0.07% on
  33 Windows DLLs. One extension's files are already adjacent and a unit is
  mostly one or two large files, so which share a unit barely moves anything.
- Per-unit LZMA lc/lp/pb — MEASURED AND REJECTED. Worth 1.2% on the x86 target
  stream alone, but on whole units the best of six parameter sets beats the
  default by 0.01-0.16%. It does not generalise, and the target stream cannot
  get its own parameters without splitting the unit into two coder streams.
- A stronger FLAC encoder — MEASURED −0.94% on 268 MB of real music (flacenc,
  LPC order 24 against `flac -8`'s 12, direct MSE, exhaustive Rice, round-trip
  verified). Real files already sit at the format's limit, and every archiver
  stores FLAC (nova max −0.17%, 7z −0.25%, zpaqfranz −0.47%), so what is left
  needs the RESIDUALS re-coded lepton-style, not a better encoder.
- Byte-plane split for 16-bit data — research 14 §7.2's top "ship-now"
  (-10..-11% on x-ray and mr vs LZMA2). DEAD, killed twice: bsc ALONE beats the
  proposed filter (x-ray 3,757,028 vs 3,999,186; mr 2,206,462 vs 2,473,757), and
  applying the split BEFORE bsc makes it WORSE (x-ray +5.9%, mr +4.8%).
  Structural: BWT sorts by following context and handles interleaved records
  natively, while splitting planes severs a sample's high byte from its low.
- PPMd var.I (`ppmd-rust`'s `Ppmd8Encoder`) — MEASURED and REJECTED: within
  0.15% of var.H on real source text and WORSE above 4 MiB, plus a live segfault
  in `RestoreMethod::CutOff` at 16 MiB (`probe-preflate --bin ppmd8min`).
- Firefox's `omni.ja` is NOT recompressible: it parses as a zip but every entry
  is method 0, stored — Mozilla keeps it uncompressed so the browser can mmap it
  at startup. No deflate to undo, which is why nova and 7-Zip land within 0.1%
  of each other. A tail-EOCD zip scan would find these files and gain nothing.
- Shared/trained dictionaries at creation time — MEASURED NET LOSS at 32 MiB
  units (100.0-100.8% of no-dictionary). A dictionary SUBSTITUTES for solidity.
  Only the append path is worth revisiting: -21% to -31% there.
- Alphabet/numeral-system transforms — folklore. MEASURED: frequency remapping
  0.0%, RLE ~1%, MTF +202%, sparse packing 0.4%. Only base64-undo is real.
- Byte transposition on float data (sao) — +25 to +29%, worse than raw.
- "The record-width filter is only for data that would otherwise be stored raw"
  — WRONG, it cost 27% on PCM. See Compression design: the guard is `pays_off`.
- Flushing a unit on every change of content class — shatters a source tree
  into 246 units (median 1.4 KiB) and costs 1.8 MiB. Only files >= 4 KiB may
  trigger a class split.
- Letting a class change slide until the unit is half the target — TRIED AND
  REVERTED: a 171 KB win in a fixed-codec counterfactual cost +1,002,157 B at
  max and +598,383 B on Silesia in the real packer. THE LESSON invalidates every
  fixed-codec counterfactual: one LZMA2 chain cannot see what a mixed unit gives
  up, because a unit has ONE codec and ONE filter, and one winner cannot filter
  two classes.
- Creating RAR archives is legally impossible (RARLAB license); only
  extraction via unrar is allowed. Plan pack: zip/7z/nova; unpack adds rar.
- `PROCESS_MODE_BACKGROUND_BEGIN` — Very Low I/O and memory priority (~250x
  slowdowns); compose the three APIs instead. IDLE/EcoQoS as DEFAULT — starved
  by daemons. IoPriorityHintVeryLow as default — 1-3% of disk. Job-object
  working-set caps and CPU-rate control — paging churn. WinRAR sleep injection —
  wastes cores. rayon/par_bridge as the pipeline — no backpressure or ordering.
- libzstd internal MT (`ZSTD_c_nbWorkers`) — breaks memory estimation and
  duplicates our chunk parallelism. zstd `--long` — pointless under CDC chunks.
- LZMA2 as the universal max-tier codec — on 4 MiB chunks it is only ±2 % vs
  zstd-19 on text (its edge comes from >4 MiB dictionaries, which chunking
  removes) while being much slower. It stays only for binary/generic data;
  text goes to PPMd7 (-24 %).
- Capping zstd WindowLog for 4 MiB chunks — no effect, zstd already shrinks
  the window to the source size.
- Parallel EXTRACTION with zstd — measured SLOWER: 5751 small files take 1.0 s
  on 1 thread, 2.5 s on 8 (NTFS metadata contention + seeks). Default extract
  workers = 1 for zstd/store; `-j` still honoured.
- GPU for blake3/dedup — slower than CPU SIMD, PCIe erases gains.
- GPU high-ratio compression (LZMA-class) — does not exist in 2026; GPU codecs
  land near zstd-1..3 ratios. Blackwell HW decompression engine is
  datacenter-only (RTX 5060 Ti does NOT have it).
- Dead/unusable GPU projects: dietgpu, Brotli-G, multians, CULZSS, GST, Gstd.
  nvCOMP's proprietary codecs would make a .nva unreadable without an NVIDIA GPU.

## Owner decisions

- Language: Rust PREFERRED, not required. The GUI must be Rust and as much else
  as practical, but a codec may be C/C++ behind FFI if Rust has no usable
  implementation — do NOT reject an algorithm for want of a Rust crate. Open repo
  `confeden/Nova-Prism`; no LICENSE yet (owner will choose). Zero telemetry, ads
  or analytics — ever. `.gitignore` is NOT tracked; the rules live in
  `.git/info/exclude` (owner's decision) — do not re-add the file.
- BENCHMARK SET is plural, not just 7-Zip: 7z -mx9, xz -9e, brotli -q11,
  kanzi -l7/-l9 (CM), zpaqfranz -m4/-m5 (CM). Binaries in
  `D:/Programs/compressors`. Corpora must be STANDARD (enwik8, enwik9, Silesia)
  so our sizes sit next to published ones; improvised corpora only for the
  recompression differentiator. Harness: `test/bench-std.sh`.
- Speed budget: not much slower than 7z -mx9. This DISQUALIFIES cmix, paq8px,
  nncp and every LLM compressor by construction, not by taste. They stay as a
  CEILING reference — how much redundancy is left — never as a target.
- SCALING is an owner requirement at BOTH ends: work well on ONE core, use 8+
  efficiently — and threads must not cost ratio. Most archivers cut data into
  smaller blocks and pay per thread; nova's unit size comes from the geometry,
  so output must be BYTE-IDENTICAL at -j 1 and -j 8. `test/scaling.sh`. (kanzi
  numbers here are the C++ build, flanglet/kanzi-cpp, not Go/Java.)
- SCALING (Silesia, `test/scaling.sh`, 1→8 threads, before bsc): nova 170.20 →
  45.85 s = **x3.71**, output BYTE-IDENTICAL at every count · kanzi -l9 x3.90,
  identical · 7z -mx9 x1.79 and its output GROWS 1,407 B once threads > 1 ·
  xz x1.04, +18,324 B.
  · THE WEAKNESS IS ONE CORE: 170 s vs 7z's 72.5 s, because the tournament
    compresses every unit four times. Lever: PPMd7 order 16 has never won a unit
    on the source tree — dropping it removes a quarter of the work.
- MEMORY LOCALITY is an owner direction and the same question as scaling: a
  max-tier worker's LZMA2 table (~370 MB) or PPMd7 pool (256 MB) is 10-20x an
  8-core desktop's L3, so max packing is memory-bandwidth-bound BY CONSTRUCTION.
  The scaling curve is the diagnostic; prefer cache-sized structures where ratio
  allows; stop whole-buffer copies (`compress_job` clones the unit to verify).
- Chat with owner in Russian; code, docs/ and CLI output in English. EXCEPTIONS,
  both owner-set: README.md and CHANGELOG.md are RUSSIAN. GUI ships RU too.
- CHANGELOG.md: `# История изменений`, `## [X.Y.Z]` newest first, flat bullets
  leading with a bold user-facing sentence. NO DATES — the owner asked for them
  out; do not reintroduce them. Version numbering follows this file's milestones
  (0.7.0 = recompression), and Cargo.toml + tauri.conf.json must match.
- THE differentiator is now stated in this order: recompressing what is already
  compressed FIRST (JPEG/PDF/zip — where other archivers have nothing), then
  per-file method choice, then editing without repack. Two-phase compression
  (analyze → compress) and cheap edits remain core requirements.
- Test everything under `test/` (gitignored playground).

## Open issues

- Not preserved: empty dirs, symlinks, NTFS attrs/ADS, ACLs. Manifest in RAM
  (fine ≤ ~1 TB). Long Windows paths (>260) untested.
- Solid-block members are read whole into RAM at pack time (up to 8 MiB outside
  the pipeline budget). No trained dictionaries; no audio recompression.
- The max tournament runs on units it cannot help: a FLAC library spends minutes
  for −0.17%, with only 32.6 of 255 MiB reaching Store and the rest "won" by
  tenths of a percent. Precompressed units should skip the tournament.
- A container larger than 2x the unit (64 MiB at max) is never recompressed — it
  cannot get a unit of its own, so a big PDF or zip silently loses the feature.
- The outer extraction loop is still per FILE; lanes engage only below workers.
- `--eco`/`--full`/EcoQoS built but not measured under load; "no lag" unverified.
- Progress granularity at max is bounded by the UNIT COUNT, not fixable in the
  reporter: ~6 units compress in PARALLEL, so between completions there is no
  finished work to report (~38 s of a ~70 s pack). The UI answers with elapsed
  time, a block counter and a pulse.

## Plans

DONE: v0.2 (pipeline, memory, priority) · v0.5 (filters, solid groups,
LZMA2/PPMd) · v0.6 (tournament, geometry, cut floor, byte-majority verdict) ·
v0.7 recompression — deflate id 34, JPEG id 35, x86 split id 36, PDF images id
37 · bsc codec id 4 · GUI (basic) · manifest LZMA2 · rename to the current name
(magic NOVA/NOVAEND1, extension `.nva`, binary `nova`, GUI `nova-prism`).

The standing direction is the owner's: beat 7-Zip where it has NOTHING, rather
than out-tune LZMA. Remaining, in measured order of value:

1. AUDIO: PCM is done (record-width filter, then FLAC as id 38). What is left
   is FLAC ITSELF — everyone stores it and its residuals are Rice-coded, so
   re-coding them lepton-style is the win; re-encoding is not (−0.94%). Then
   MP3, which needs packMP3 (LGPL-3.0, dormant) as a plugin.
2. A container larger than 2x the unit silently loses recompression — now more
   pressing, since .wav routinely runs to hundreds of MB. FLAC frames are
   independent, so audio could be cut at frame boundaries instead.
3. Executable modelling (BCJ2-class, section splitting) — 51% of the remaining
   headroom to paq8px on Silesia, five times images and tables combined.
4. zip/7z unpack + zip pack (sevenz-rust2), rar unpack (unrar). GUI: shell
   icons/thumbnails (research 06/07), .nva association + installer, folder tree,
   RU localization. Later: GPU (research 08), encryption (XChaCha20-Poly1305),
   Explorer integration, installers, Linux/macOS/Android ports.

NOT on this list, and deliberately: bigger units to buy solidity on executables,
and BCJ2-style per-site probability. Both PRICED and rejected — see Recompression.
