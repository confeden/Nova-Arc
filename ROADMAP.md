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
- 98 tests green, clippy clean: roundtrip, append without rewrite, dedup,
  replace, remove+compact, rename, selective extract, crash recovery, torn
  manifest, embedded/forged footer, writer lock, selectors, pre-1970 mtime,
  overwrite policy, compact-detects-corruption, the progress contract,
  deflate/JPEG/PDF/WAV round-trips, the PDF traps, a stored JPEG inside a zip,
  the record-width filter on PCM, a split .wav, one large file through the
  decode lanes, and the `legacy-*.nva` fixtures.
- Compression v2 DONE: codec+filter per content class, solid blocks by extension,
  LZMA2 + PPMd7 + bsc, BCJ x86 + delta (BCJ verified against liblzma).
- Measured residue: BCJ on real .exe +4.4-5.7% vs unfiltered. PPMd7 vs zstd-19
  on 4 MiB prose -24%; LZMA2 vs zstd-19 on prose ±2%. Peak RAM tracks `--memory`
  on EXTRACT too (256M→153 MiB, default→1.0 GiB) — bounded, but "~10 MiB peak"
  is long gone, units are the cost.
- `test/` = playground (gitignored). zip/7z/rar and GPU: not started.

## Architecture & invariants

- Archive layout: `[header][manifest g1][footer g1]` from `create`, then
  `[chunks…][manifest gN][footer gN]` per update; committed bytes are NEVER
  rewritten except by `compact`.
- Benches: `test/*.sh`.
- Commit = manifest → fsync → footer → fsync; without the barrier a valid
  footer can point at a torn manifest.
- Footer self-hash covers its own absolute offset, so a `.nva` inside another
  archive cannot be mistaken for a commit. Readers verify each candidate's
  manifest and resume the backward scan on failure (≤64).
- Packing invariant: the writer appends chunks in submission order, so the reader
  predicts each index as `base + submission index` and builds entries early.
- PROGRESS CONTRACT (`Progress`, `Phase`, `archive::Reporter`). `bytes_done` =
  source bytes whose work is FINISHED, fed by the writer; `bytes_read` = what the
  reader took off disk, up to an in-flight budget ahead. Monotone, never above
  the total, equal to it exactly once — the single `Phase::Done` reading, others
  clamped to total-1 so "100%" structurally means finished. INVARIANT: the
  Reporter mutex is never held while entering the pipeline, or the reader sleeps
  in `Budget::acquire` holding it and stalls the writer. Throttling lives in
  core, clocks in the GUI — do NOT add a timer thread to core.
- Memory model: `budget ≈ 32 MiB base + workers × (tables + 8 MiB) + queued`;
  workers capped by the budget, in-flight bytes by the workers. Table cost per
  tier in `Tier::worker_memory()`.
- Chunk hash = blake3(uncompressed)[..16]: dedup AND integrity; dead records stay
  as dedup sources until compact.
- INTRA-FILE DECODE LANES. Threads the file count cannot use become lanes
  INSIDE each file (`lanes_per_worker = budget / workers`), so a one-file
  archive stops using one thread while total concurrency, memory, ordering,
  overwrite policy and mtime all stay put. enwik8 at normal 4.80 → **1.63 s**.
  · GROUP BY DISTINCT UNIT, not by extent: consecutive extents usually share a
    unit and the sequential path got that free from `UnitCache`. Per extent it
    decoded the same unit once per lane — enwik8 4.8 → 8.2 s. Running out of
    handles means fewer lanes, not an error.
- PPMd7 order (10) and pool formula (32x chunk, cap 64 MiB) are FORMAT CONSTANTS
  — not stored per chunk, so changing them breaks old archives. Order 10 beat
  12/16: a pool-exhaustion restart costs more than depth gains.
- LZMA2 presets 6..9 differ only in dictionary size, which the chunk cap
  overrides, so max raises nice_len (xz -e).
- Codec and filter ids are tabulated in `docs/format.md`; keep it in step. A
  NEW id must never be added by widening the delta range — `2..=MAX_DELTA_ID =>
  Delta(id - 1)` would have made 34 decode as Delta(33), silently. Per-chunk raw
  fallback if not smaller, and then the filter byte MUST be cleared too.
- Fixed order: pack = filter → compress; unpack = decompress → unfilter. The
  chunk hash covers the ORIGINAL bytes, so dedup and integrity are filter-
  independent and the hash check proves the filter round-tripped.
- BCJ always runs with start_offset 0, never the chunk's file position, or
  position-dependent output breaks dedup. Cost: one instruction per boundary.
- Solid blocks: files < `Geometry::chunked_from` (256 KiB fast/normal, 1 MiB
  MAX) are sorted by extension, concatenated into `Geometry::unit` streams
  (4/16/32 MiB) and compressed as ONE unit. Size is bounded ON PURPOSE: editing
  one member rewrites one block. Cuts are content-defined per file with a hard
  flush at 2x target, so realized blocks run LARGER than target, never smaller.
- A content-defined cut may only fire at or above HALF the target: the low tail
  is loss and it destabilised boundaries. MEASURED both ways.
- A unit's codec+filter come from a BYTE-MAJORITY VOTE of the per-file verdicts,
  never the head sample (`Packer::unit_plan`). One sub-4 KiB `.flac` ahead of
  `.go` made the head say "already compressed" and 8.36 MB was stored raw. Only
  a unit with NO voters reads the head.
- Two-phase pipeline: `analyze::plan()` (magic → class → trial) gives codec and
  filter. fast=zstd3, normal=zstd12+bsc, max=all four.
- `HEAD_SAMPLE` = 1 MiB, load-bearing: deflate leaves matches megabytes apart,
  so a level-1 zip reads as +0.02% on 64 KiB and −25.6% on 1 MiB. Free —
  `add_file` chains the head in front of the file. Sub-tests keep their caps
  (`TRIAL_SAMPLE` 64 KiB; the delta detector MUST stay capped).
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

- Windows: cannot rename over an open file — compact consumes `self`, closes the
  handle, then replaces in place. `tempfile` is a REGULAR dep for it.
- Windows file locks are MANDATORY per byte range: a whole-file `File::try_lock`
  makes our OWN extraction threads fail with ERROR_LOCK_VIOLATION (33). Hence
  `try_lock_exclusive` locks one byte at 0xFFFF_FFFF_FFFF_0000, never real data.
- `File::try_clone` SHARES the file position on Windows: extraction workers
  must each `File::open` the archive, never clone a handle.
- GUI: run `npm --prefix ui run build` FIRST. There must be no `devUrl` in
  tauri.conf.json — it made every non-Tauri-CLI build come out in dev mode, so
  the shipped exe opened localhost:5173 and a plain `cargo build --release`
  silently overwrote a good exe with that one, twice. Every build now embeds
  `ui/dist` and build.rs FAILS if `ui/dist/index.html` is missing.
- Windows: quote a path with spaces or Start-Process -ArgumentList splits it and
  argv[1] is truncated (this fooled a GUI test).
- .cmd launchers mangle Cyrillic (OEM codepage); use a BOM'd .ps1. A formatter
  hook rewrites files after every Write/Edit here.
- `zpaqfranz x ... -to DIR` needs a TRAILING SLASH or it refuses, and a bench
  records the refusal as the decode time.
- Per-worker memory is match tables, not the window.
- zstd/blake3/libbsc build C or C++ via MSVC. git identity: user.name "Brent", email confeden@cryptolab.net.

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
- `--eco`: IDLE + EcoQoS + IoPriorityHintVeryLow. `--full`: NORMAL, EcoQoS off.
- GPU (research 08), if ever: a codec-provider trait behind `--gpu auto|on|off`,
  standard bitstreams only, nvCOMP at runtime and never vendored. PARKED.

## Compression design (measured, v0.6 max tier)

- No single codec wins: PPMd7 beats LZMA2 13-24% on prose, LZMA2 beats PPMd7 16%
  on binaries. MAX runs a per-unit tournament (LZMA2 + PPMd7 o10/o16 + bsc);
  fast trusts the analyzer, normal races it against bsc.
- THE TOURNAMENT IS NOT FOR EVERYTHING. A unit the analyzer classed
  Precompressed — magic says entropy-coded, and only the 1% trial bar cleared —
  gets normal's two-horse race instead: PPMd7 models symbol contexts, and data
  an entropy coder has already been over has none left to model. On a 268 MB
  FLAC library that is BYTE-IDENTICAL output in 28-43 s instead of 109-306.
  Priced: +315 B on the deflate corpus (+0.01%), where one small precompressed
  unit did give PPMd7 something.
- TOURNAMENT WINNERS by corpus (`info --units`): enwik8 bsc 2 · Silesia bsc 7 /
  lzma2 4 · source tree lzma2 4 / ppmd7-10 1 · Firefox lzma2 13 / **ppmd7-16 4**
  · pdfs lzma2 14 / **ppmd7-16 5**. Order 16 earns its place — "it never wins"
  is true of the SOURCE TREE only, and dropping it would cost real bytes.
- Chunk size = compression unit = edit granularity. fast/normal 256K/1M/4M, max
  1M/4M/16M (FastCDC caps at 16 MiB); solid target 4/16/32 MiB by tier.
- Solid block boundaries are content-defined PER FILE (cut prob = size/target
  from the file's own blake3). A size-accumulator rule cost 17 MiB for a
  one-line edit: one file's length shifted every later boundary.
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
- PPMd7 AS THE TEXT CODEC IS DOMINATED: on enwik8 kanzi -l9 is smaller and
  faster both ways, and bsc out-votes it.
- nova max is NOT faster than 7-Zip: Silesia 52.7 s vs 44.4 s, enwik8 77.7 s vs
  43.3 s. The old "6.8 s vs 49 s" predates the tournament; never requote it.
- DECODE is DATA-DEPENDENT: Silesia nova 1.8 s vs kanzi -l9 50.5 s; enwik8 4.6 s.
- kanzi -l7: Silesia 47,308,780 B in 6.15 s, the fast tier's bar. Its default
  `-j` is HALF the cores, so bench-std gave it 4 threads against our 8.
  · enwik8 block sweep: 8 MiB 23,593,148 · 32 21,983,674 · 128 20,803,016 — MILD
    on text. Source tree is the OPPOSITE: bsc -b32 13,474,736 vs our 9,292,017.
  · SHIPPED as codec id 4 (`crates/nova-bsc`, libbsc 3.3.12, Apache-2.0), a
    fourth MAX candidate: enwik8 22,466,101 → **21,506,314 (−4.3%)**, Silesia
    43,674,657 → **43,036,408 (−1.5%)**, source tree UNCHANGED. Encode +8%.
  · Wiring: `bsc_qlfc_init_model` memcpy's the global model per call, so workers
    are safe after one `bsc_init`. `bwt.cpp`/`st.cpp` include their CUDA headers
    UNCONDITIONALLY — vendor both. Windows needs `advapi32`; C and C++ need
    separate builds. No Rust port; `libsais-rs` covers the sa core.
  · ALSO AT NORMAL, the bigger win: enwik8 −24.6%, Silesia −19.0%, pdfs −10.0%,
    precomp −9.0%, source tree −6.2%, for 1.6-1.9x the encode. Normal beats
    7z -mx9 on Silesia in a SIXTH of its time.
  · TRAP: `bsc` decodes 100 MB in 0.9 s, which reads like 110 MB/s — but that is
    multithreaded across blocks and nova disables libbsc's own threads. Single
    -threaded it is ~25 MB/s; treating it as fast in `extract_workers` made a
    normal Silesia archive extract in 8.7 s against zstd's 0.5. The real cost:
    normal Silesia 0.5 → 2.1 s for −19%.
  · FAST STAYS WITHOUT IT, measured twice, the second time after decode lanes
    landed — which the first refusal had named as its condition. Fast with bsc
    would be 47.8 MB / 2.90 s pack / 3.19 s extract against NORMAL's 46.4 /
    7.0 / 2.09: better only in pack time. Not a fast tier, a worse normal one.
  · libbsc is C++ and nova-core is `#![forbid(unsafe_code)]`, so it needs its
    own crate.
- Cut floor + byte-majority verdict on test/corpus: fast −7.6%, normal −0.2%,
  max −4.0%. The fast win is the VOTE (the floor ALONE made fast 5.1 MB worse).
- Edit cost at max is ~4.7 MiB and ~40 s — the edited file lives in a 30-60 MB
  unit. Cheap edits are a fast/normal property (0.73 / 3.30 MiB).
- nova's LZMA2 is 0.24% BETTER than liblzma -9e on identical boundaries; a max
  run over test/corpus realizes 6 units averaging 41.42 MiB.
- MANIFEST CODEC: a manifest >= 128 KiB is offered to LZMA2 and the smaller wins
  (682,799 B raw: zstd 19 84,565 vs LZMA2 74,690). The codec is read off the
  BYTES — a zstd frame starts 28 B5 2F FD, a raw LZMA2 stream never can — so
  there is no format field and every manifest ever written still decodes.
- Diagnostics: `NOVA_UNIT_TRACE=<file>` logs one line per unit as it is built —
  the cut reason exists only at pack time. `nova info --units` dumps the rest.

## Recompression — DEFLATE, JPEG and PDF ARE LANDED (research 02, measured here)

- Corpora: `test/precomp-web` (public, below; supersedes `test/precomp`),
  `test/incompress` (control, must stay stored), `test/photos`,
  `test/zipphoto` (same photos, zipped Store), `test/pdfs`, `test/firefox`,
  `test/audio` + `test/audio-wav`. Probes in `test/probe-preflate`, outside the workspace.
- MIN_STREAM = 64 bytes, MEASURED; 4096 cost 15 points. "A record cannot pay
  for itself on 2 KB" is true per FILE, false inside a unit. `preflate-rs 0.7.6`
  (Microsoft, Apache-2.0) is byte-exact on every file of both corpora.
- REPRODUCIBLE DEFLATE CORPUS, and it earns its keep: `test/precomp-web`,
  93,399,733 B in 29 files, every one a versioned public URL with its SHA-256 in
  `test/precomp-web.json` (`test/fetch-precomp.py`, `test/bench-precomp-web.sh`)
  — Python docs zip, a GNU tar.gz, a Maven jar, a Gutenberg epub, an F-Droid apk
  and the 24 Kodak PNGs. nova max **74,865,900 (80.2%)** · zpaqfranz -m5
  86,761,587 · 7z -mx9 86,991,164 · brotli 87,005,350 · xz 87,027,656 · kanzi
  -l9 87,363,217. **−13.7% to the best of the rest**, repeatable by anyone.
  · A SECOND CEILING, found the moment the corpus stopped being 4.9 MiB. The
    filter reaches 28 of 29 units (41,507,277 → 25,800,674); the 29th is
    `binutils-2.42.tar.gz`, which FITS the solo cap at 51,892,456 B and is still
    refused, because it expands to 319,897,600 B — past `MAX_CODED_CHUNK`
    (256 MiB). Worse than the solo cap: it scales with how well the payload
    compresses, so it bites hardest where recompression would pay most.
  · That cap was charged PER UNIT: one oversized member made the WHOLE unit
    `bail!`, losing every other stream's gain too. FIXED — an outsized member
    is now skipped alone, not its neighbours. Byte-identical here: binutils
    is a lone stream, so this fixes a different case, untested by this corpus.
  · fast lands at 99.7% here and that is CORRECT: PNG-filtered photographic
    scanlines do not beat the original deflate under zstd-3.
- STORED ZIP ENTRIES ARE SCANNED TOO, and skipping them had been throwing away
  the best data in the archive: a zip does not deflate what deflate cannot help,
  so a photo backup STORES its JPEGs, an epub its illustrations, an apk its
  PNGs. `zip` now hands a method-0 entry back to `dispatch` (depth-capped at 3),
  which covers stored JPEG, stored PNG and zip-in-zip with one arm. A bare JPEG
  is only dispatched at depth > 0 — at the top it is filter 35's business and
  adding it would change what every existing unit scans to.
  · MEASURED on the case it exists for, six camera photos zipped with method
    Store: 17,326,548 → **13,992,258 (80.8%)** against zpaqfranz -m5's
    16,854,983 and 7z -mx9's 17,289,656, so **−17.0% to the best of the rest**
    where we used to store it too. On the public corpus it is worth only 27,781
    B — that corpus holds 257 KB of stored JPEG and 571 KB of stored PNG.
- JPEG (lepton, id 35) SHIPPED. test/photos, 6 camera JPEGs: raw 17,324,730 ·
  best of the rest 16,844,561 · **nova 13,990,872 (−16.9%)**.
  · The stored payload is the LEPTON FORM ITSELF: already entropy-coded, no codec
    ever beat it, so `compress_job` keeps the smallest of {codec over filtered,
    filtered verbatim, original}. A Store record carrying a filter is safe.
  · Lepton settings are a FORMAT CONSTANT for id 35, like PPMd7's order and pool:
    `compat_lepton_vector_write`, single-threaded. Its 16386-pixel limit leaves
    panoramas untransformed, and it is slow: ~0.8 MB/s per thread.
- PDF IMAGES SHIPPED as filter id 37, a mixed container. 18.5% of a real
  19-document corpus is `/DCTDecode` — whole JPEGs, which lepton takes 20.3% off
  (39 of 39). pdfs max → **5,064,071**; against 7z -mx9's 6,606,978, **−23.4%**.
  · Id 34's framing cannot say what a stream IS, so it only carries deflate. Id
    37 adds a kind byte and a `NDf2` magic; the decoder reads both, so everything
    written still opens and 34 is decode-only — an id is a promise, not a slot.
    A `/DCTDecode` stream is the JPEG itself: no zlib wrapper, starts at SOI.
  · THE TRAP (found via id 34, deflate-only, before id 37 added JPEG): PDF's
    `/FlateDecode` is RFC 1950, so a stream carries two zlib header bytes and
    a four-byte adler32 that are NOT deflate. Handed those, preflate modelled
    **0 of 957** streams while the scanner reported 72% coverage. Strip 2 + 4.
  · The scan is LEXICAL; "PDF needs a full object parse" deferred it and was
    WRONG. Rules: `stream` must follow `>>`; the FIRST name after `/Filter`
    decides the kind; `/Length` needs whitespace or `/Length1` is read as the
    length; an indirect `/Length 9 0 R` falls back to `endstream`, a MISSING one
    stops the scan.
  · EVERY per-candidate backward scan needs a FLOOR at the previous candidate's
    end, not just a window: `%PDF-` + `>>stream` repeated matched neither `obj`
    nor `/Filter`, so both sweeps ran in full — 47 s for 16 MiB vs 45 ms of real
    PDF. Diagnostics: `probe-preflate --bin {pdfscan,pdfhostile,lepton}`.
- WAV → FLAC SHIPPED as filter id 38 (`crate::wav`, flacenc + claxon, pure Rust
  Apache-2.0). 518 MB of PCM: 439,050,506 → **265,279,793**, against 7z -mx9's
  342,942,386 and zpaqfranz -m5's 335,792,867. Decode 14 s for the corpus at
  233 MiB peak — claxon is cheap, unlike lepton.
  · ID 38 PINS THE DECODER, NOT THE ENCODER, unlike 34/35/37: the payload is a
    standard FLAC stream, so a better encoder never spends an id. What it DOES
    pin is the wrapper — the whole file with only the `data` payload cut out,
    spliced back on decode. That, not FLAC, is what makes the round trip exact:
    chunk order, odd sizes with pad bytes, a `RIFF` length that disagrees with
    the file and trailing garbage all survive, none of it rebuilt from a parse.
  · A .wav past the solo cap is CUT, not given up on: `Packer::add_wav_split`
    emits unit-sized runs of whole frames, the header riding with the first
    piece and the trailing chunks with the last. 276,221,291 → **265,279,793**
    at max, 305,746,008 → **265,307,452** at normal — and normal now MATCHES
    max, because over a FLAC stream the codec barely decides anything.
  · Middle pieces are bare PCM with no `fmt `, so the format travels in
    `Job.wav`; the record always carried it, so decoding is unchanged. An
    `Extent`'s offset is inside the UNIT, not the file — the file offset gave
    "extent outside its unit" on extract.
  · A .wav that cannot be split falls back to the GENERIC path, NOT the
    precompressed one — `plan_precompressed` would drop the delta filter too.
- Installed-program corpus (test/firefox, 341.7 MiB, 68.6% .dll): 7z -mx9
  87,566,439 B vs nova 95,721,462 → **+9.3%, our weakest case**. Findings:
  · The codec+filter vote must be cast per CHUNK (`Packer::current` + `place`):
    cast once at the file's start it put 150 MB of a 176 MB xul.dll in units
    with no BCJ. Worth 506,834 B.
  · The gap is ALL x86: .dll +11.8%, omni.ja +0.1%, everything else −6.2%.
  · SOLIDITY IS NOT THE CAUSE (234.3 MiB DLL set, LZMA2 -9e): solid+BCJ
    70,463,272 B · 64 MiB units 71,410,491 · 32 MiB 72,082,105 · no filter
    72,509,466. Geometry costs 1.3%, BCJ 2.8%, and 7-Zip sits 8.7% BELOW
    solid-with-BCJ: the difference is the FILTER, not the size.
  · 7-Zip's advantage is BCJ2: the 4-byte call/jump targets go to separate
    streams so high-entropy addresses stop interrupting the code. `lzma-rust2`
    ships a BCJ2 DECODER only, so we wrote the transform — id 36. Firefox
    95,721,462 → **93,467,847 (−2.4%)**, source tree → 9,309,370 (−2.1%);
    gap to 7-Zip: Firefox +9.3% → **+6.7%**, source +4.1% → +1.9%.
  · THE RULE, and three that measured worse. Take a site when the absolute
    target lands INSIDE the buffer: 68,893,556 B at 64 MiB units, vs 68,994,337
    with no test, 72,488,469 with liblzma's top-byte-is-00-or-FF test (WORSE
    than not splitting — the addresses land in 0..64M so the top byte is
    0x01-0x03), and 69.5-71.1 MB gating on displacement magnitude. The WIDTH of
    the accepted range is what matters, and the unit size bounds it. Unit size
    PRICED and rejected: 128 MiB buys −1.1%, 256 MiB −4.0% and still 2.8% above
    7-Zip. BCJ2 learns a probability per site; this applies a fixed test.
  · Concatenating the split's three streams into one buffer costs NOTHING. The
    site COUNT is in the header and load-bearing: the decoder walks a stream the
    displacement bytes are gone from, so at the tail it finds an extra opcode.
- HOW IT IS WIRED — FORMAT RULES. `ChunkRec.filtered` (0 = same as unpacked) is
  the length the CODEC produced; `unpacked` keeps its one meaning forever, the
  ORIGINAL length that `hash` covers and `Extent` indexes. `lzma2_dict_size` and
  `ppmd7_mem_size` must derive from the CODED length on both sides or every
  existing archive stops decoding. `Filter::apply` returns `Applied::{InPlace,
  Rebuilt}` because the store fallback must undo the first and NOT the second.
  Ids 34/35/37 pin library versions; an upgrade spends a NEW id. A recompressible
  file gets a unit of ITS OWN (>= 64 KiB, <= 2x unit): the scanners each need one
  whole container.
- SAFETY, enforced not intended: the packer round-trips every rebuilt unit and
  falls back on any mismatch; a filter that refuses is a fallback; the
  transformed form may not exceed `MAX_CODED_CHUNK` (256 MiB), charged as pieces
  are BUILT and checked again before any decode allocation.
  · THE CODED-LENGTH BOUND BELONGS IN `compress_job`, beside `filtered =
    data.len()`: only that line knows the number that reaches the manifest, and
    `verify_chunk` REFUSES a coded length above the cap — an unchecked one is an
    archive nova writes and cannot extract. Per-filter budgets do not
    substitute: `deflate_encode` charged plaintexts but not container bytes.
  · Never size a Vec from a count a payload claims: `decode` bailed on an
    implausible count but had already reserved for it. Grow as records parse.
- TRAPS that would each have shipped a SILENT mis-decode. Recorded because the
  next length-changing filter meets every one again: the store fallback must
  compare against the ORIGINAL length and reset the coded length, not just the
  filter byte · an in-place filter MUST be unapplied on that fallback and a
  rebuilt one MUST NOT, which is why `Applied` exists · widening the delta range
  to reach a new id makes it decode as `Delta(id-1)` · the deflate class cannot
  be decided from the head, a zip's central directory is at the END · PPMd7's
  pool must match EXACTLY and saturates at both ends, so a length bug passes on
  big units and fails only in a band, while LZMA2 with a narrow window fails as
  intermittent corruption.
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
- THE WHOLE AUDIO CEILING IS ~5%, measured, and it prices the one idea left:
  paq8px -8 takes `01. Breach.wav` (14,394,284 B) to 6,613,802 against our
  6,952,563 through filter 38 — **−4.9% in 11m47s for 14 MB**, ~700x slower than
  the speed budget allows. Re-coding FLAC's Rice residuals could capture only
  part of that 5%, not the 16-22% deflate and JPEG gave. Audio is SOLVED.
- Raising the packer's solo-unit cap on its own — it is the READER's bound in
  disguise. `read_packed` refuses a chunk above `MAX_STORED_CHUNK` (2x
  MAX_CHUNK = 64 MiB) and `unit * 2` merely equalled it. At `unit * 4` a real
  WAV corpus packed, LISTED, and failed on extract with "corrupt manifest:
  implausible chunk size". The cap now reads `min(unit * 2, MAX_STORED_CHUNK)`
  and `Packer::flush` asserts the bound before anything is written.
- Byte-plane split for 16-bit data — research 14 §7.2's top "ship-now"
  (-10..-11% on x-ray and mr vs LZMA2). DEAD, killed twice: bsc ALONE beats the
  proposed filter (x-ray 3,757,028 vs 3,999,186; mr 2,206,462 vs 2,473,757), and
  applying the split BEFORE bsc makes it WORSE (x-ray +5.9%, mr +4.8%).
  Structural: BWT sorts by following context and handles interleaved records
  natively, while splitting planes severs a sample's high byte from its low.
- PPMd var.I (`ppmd-rust`'s `Ppmd8Encoder`) — within 0.15% of var.H on real
  source text, WORSE above 4 MiB, plus a live segfault at 16 MiB.
- Firefox's `omni.ja` is NOT recompressible: it parses as a zip but every entry
  is method 0, stored, so the browser can mmap it at startup. No deflate to undo,
  which is why nova and 7-Zip land within 0.1% of each other.
- Shared/trained dictionaries at creation time — MEASURED NET LOSS at 32 MiB
  units (100.0-100.8% of no-dictionary). A dictionary SUBSTITUTES for solidity.
  Only the append path is worth revisiting: -21% to -31% there.
- Alphabet/numeral-system transforms — folklore. MEASURED: frequency remapping
  0.0%, RLE ~1%, MTF +202%, sparse packing 0.4%. Only base64-undo is real. Byte
  transposition on float data (sao) is +25 to +29%, worse than raw.
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
- Creating RAR archives is legally impossible (RARLAB license); unrar may only
  extract. Plan: pack zip/7z/nova, unpack adds rar.
- `PROCESS_MODE_BACKGROUND_BEGIN` — Very Low I/O and memory priority (~250x
  slowdowns); compose the three APIs instead. IDLE/EcoQoS as DEFAULT — starved
  by daemons. IoPriorityHintVeryLow as default — 1-3% of disk. Job-object
  working-set caps and CPU-rate control — paging churn. WinRAR sleep injection —
  wastes cores. rayon/par_bridge as the pipeline — no backpressure or ordering.
- libzstd internal MT (`ZSTD_c_nbWorkers`) — breaks memory estimation and
  duplicates our chunk parallelism. zstd `--long` — pointless under CDC chunks.
- LZMA2 as the universal max-tier codec — on 4 MiB chunks it is only ±2% vs
  zstd-19 on text, its edge coming from >4 MiB dictionaries that chunking
  removes. It stays for binary/generic data; text goes to PPMd7 (−24%).
- Capping zstd WindowLog for 4 MiB chunks — no effect, already shrinks to
  the source size. Parallel EXTRACTION with zstd is SLOWER too (NTFS
  metadata contention): 5751 files, 1.0 s on 1 thread vs 2.5 s on 8; default
  extract workers = 1 for zstd/store, `-j` still honoured.
- GPU for blake3/dedup — slower than CPU SIMD, PCIe erases gains. GPU high-ratio
  compression (LZMA-class) does not exist in 2026; GPU codecs land near zstd-1..3
  and Blackwell's HW decompressor is datacenter-only (a 5060 Ti has none).
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
  45.85 s = **x3.71**, BYTE-IDENTICAL at every count · kanzi -l9 x3.90, identical
  · 7z -mx9 x1.79 and its output GROWS 1,407 B past one thread · xz x1.04.
  · THE WEAKNESS IS ONE CORE: 170 s vs 7z's 72.5 s, because the tournament
    compresses every unit four times. Do NOT reach for dropping PPMd7 order 16
    to fix it — see the winners list; it takes 4 Firefox units and 5 PDF ones.
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

- Not preserved: empty dirs, symlinks, NTFS attrs/ADS, ACLs. Manifest in RAM;
  long Windows paths untested.
- Solid-block members are read whole into RAM at pack time (up to 8 MiB outside
  the pipeline budget). No trained dictionaries; no MP3 recompression.
- A container past the solo cap is recompressed only if it is audio (Plans 1).
- The outer extraction loop is still per FILE; lanes engage only below workers.
  `--eco`/`--full`/EcoQoS are built but not measured under load.
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

1. FIXED the multi-stream half (per-stream cap, above). The single-stream
   half is UNSOLVED: WAV-style splitting does not generalise — preflate-rs
   only returns reconstruction parameters for its FIRST chunk, so one lone
   stream (`binutils-2.42.tar.gz`) cannot be cut. Left: a format change.
2. AUDIO IS OTHERWISE DONE — see Negative knowledge for why FLAC residuals are
   not worth it. MP3 (packMP3, ~16%, LGPL-3.0, dormant) is the only piece left,
   and it is a licensing question before it is a code one.
3. Executable modelling (BCJ2-class, section splitting) — 51% of the remaining
   headroom to paq8px on Silesia, five times images and tables combined.
4. zip/7z unpack + zip pack (sevenz-rust2), rar unpack (unrar). GUI: shell
   icons/thumbnails (research 06/07), .nva association + installer, folder tree,
   RU localization. Later: GPU (research 08), encryption (XChaCha20-Poly1305),
   Explorer integration, installers, Linux/macOS/Android ports.

NOT on this list, and deliberately: bigger units to buy solidity on executables,
and BCJ2-style per-site probability. Both PRICED and rejected — see Recompression.
