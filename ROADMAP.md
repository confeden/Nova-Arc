# Nova Arc — working context (Claude-only, English)

## Current state

- Cargo workspace, Rust 1.95, edition 2021: `narc-core` (format,
  `#![forbid(unsafe_code)]`), `narc-cli` (binary `narc`), `narc-platform`
  (all OS/unsafe code), `narc-gui` (Tauri 2 desktop app). Frontend: `ui/`
  (TS + Vite, no framework). Node 26, WebView2 present on this machine.
- Format v0.2 = v0.1 + solid blocks + per-chunk filter/param bytes +
  LZMA2/PPMd7 + manifest `geometry` (chunk sizes are an archive property).
- NARC v0 container implemented and VERIFIED: append-only chunk log, FastCDC
  256K/1M/4M, blake3-128 dedup+integrity, MessagePack(named)+zstd manifest,
  80-byte offset-bound footer, generation counter, resumable crash recovery,
  rw-open truncates uncommitted tail, `compact` with verify + atomic replace.
  Spec: `docs/format.md`.
- Multi-threaded packing pipeline (`pipeline.rs`): reader hashes+dedups, worker
  pool compresses, writer appends in submission order; byte-budget backpressure.
  Extract threads key off the archive's codecs (1 for zstd/store, all cores for
  LZMA2/PPMd, which are CPU-bound to decode). `Progress` callbacks feed the GUI.
  CLI: `-j/--threads`, `--memory`, `--eco`, `--full`; packing prints peak RAM.
- CLI: create/add/extract/list/remove/rename/compact/info (+aliases c/a/x/l/rm/mv),
  extract has `--force` / `--skip-existing` (default: refuse to clobber).
- `rename` (`Archive::rename`) moves an entry or a whole folder by rewriting the
  manifest ONLY: 77 entries in 0.053 s, zero new units, byte-identical output.
  Reorganising folders must never re-read or re-compress. Cost is one appended
  manifest, reclaimed by `compact`.
- GUI (narc-gui, VERIFIED running): open/create/add/extract/remove/compact,
  virtualized list with type glyphs + unit badges, sortable columns, multi-select,
  context menu, double-click opens a file from a temp dir, Explorer drag&drop
  both ways, throttled progress, level/memory in toolbar, argv[1] opens an
  archive. Shortcuts "nArc (dev)" and "Nova Arc" are on D:\Desktop.
- Progress FIXED (bar sat at 100% while 95% of the work remained; four causes).
  The PROGRESS CONTRACT below is the durable residue.
- 82 tests green (`cargo test`), clippy clean. Covers roundtrip, append without
  rewrite, dedup, replace, remove+compact, rename/move, selective extract, crash
  recovery, torn-manifest fallback, embedded-footer confusion, forged footer,
  writer lock, selector normalization, pre-1970 mtime, overwrite policy,
  compact-detects-corruption, the progress contract, deflate/JPEG/PDF
  recompression round-trips, the PDF scanner's traps, and the `legacy-*.narc`
  fixtures written by the pre-recompression build.
- Compression v2 DONE: analyzer picks codec+filter per content class, solid
  blocks grouped by extension, LZMA2 + PPMd7, BCJ x86 + delta filters (BCJ
  verified byte-identical against liblzma).
- Measured residue (8 logical cores, `bash test/bench.sh`): BCJ on real .exe
  +4.4-5.7% vs unfiltered, every codec/level. PPMd7 vs zstd-19 on 4 MiB prose
  -24%; LZMA2 vs zstd-19 on prose only ±2% (its edge needs big dictionaries).
  Threads scale 3.1x on big text. Peak RAM tracks `--memory` on EXTRACT too, on
  a max archive with 30-60 MB units: 256M→153 MiB, default→1.0 GiB — bounded,
  but "~10 MiB peak" is long gone, units are the cost. (FIXED on the way:
  `PPMD_POOL_BYTES` said 64 MiB while `ppmd7_mem_size` allocates up to 256 MiB,
  so `extract_workers` spawned ~4x the workers the budget allows.)
- `test/` = local playground (gitignored): corpora, bench.sh, RU readme. zip/7z/
  rar support and GPU: not started. Research reports live in `docs/research/`.

## Architecture & invariants

- Archive layout: `[header][manifest g1][footer g1]` from `create`, then
  `[chunks…][manifest gN][footer gN]` per update; committed bytes are NEVER
  rewritten except by `compact`.
- Commit = manifest write → fsync → footer write → fsync (the barrier is
  required: without it a valid footer can point at a torn manifest).
- Footer self-hash covers its own absolute offset, so a `.narc` stored inside
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
  writer. Throttling lives in core (only core knows the totals); clocks live in
  the GUI — do NOT add a timer thread to core.
- Memory model: `budget ≈ 32 MiB base + workers × (tables + 8 MiB) + queued`;
  workers capped by the budget, then in-flight bytes capped by the workers.
  Table cost per tier in `Tier::worker_memory()` (fast 4, normal 40, max 56).
- Chunk hash = blake3(uncompressed)[..16]; serves dedup AND extract integrity.
  Dead chunk records stay in the manifest as dedup sources until compact.
- PPMd7 order (10) and pool formula (32x chunk, cap 64 MiB) are FORMAT
  CONSTANTS — not stored per chunk, so changing them breaks old archives. Order
  10 beat 12/16 on 4 MiB chunks: a pool-exhaustion restart costs more than
  depth gains.
- LZMA2 presets 6..9 differ only in dictionary size, which we override with the
  chunk cap, so max raises nice_len instead (xz -e style).
- Codec ids: 0=store, 1=zstd, 2=LZMA2, 3=PPMd7. Filter ids: 0=none, 1=BCJ x86,
  2..=33=delta(id-1), 34=deflate (preflate 0.7.x, also PDF), 35=JPEG (lepton
  0.5.x), 36=x86 branch-target split (narc's own). A NEW id must never be added
  by widening the delta range — `2..=MAX_DELTA_ID => Delta(id - 1)` would have
  made 34 decode as Delta(33), silently. Per-chunk raw fallback if the result is
  not smaller, and then the filter byte MUST be cleared too.
- Fixed order: pack = filter → compress; unpack = decompress → unfilter. The
  chunk hash covers the ORIGINAL bytes, so dedup and integrity are
  filter-independent, and the hash check proves the filter round-tripped.
- BCJ always runs with start_offset 0, never the chunk's file position —
  position-dependent output would break dedup. Cost: one unconverted
  instruction per boundary.
- Solid blocks: files < `Geometry::chunked_from` (256 KiB fast/normal, 1 MiB
  MAX) are sorted by extension, concatenated into `Geometry::unit` streams
  (4/16/32 MiB) and compressed as ONE unit; a FileEntry then holds one `Extent`
  into it. Block size is bounded ON PURPOSE: editing one member rewrites one
  block. Boundaries are content-defined per file with a hard flush at 2x target,
  so realized blocks run LARGER than target, never smaller.
- A content-defined cut may only fire at or above HALF the target: the low tail
  is pure loss, and it also made boundaries LESS stable — an edit changes the
  item's hash, so an unguarded cut can appear or vanish. MEASURED both ways.
- A unit's codec+filter come from a BYTE-MAJORITY VOTE of the per-file verdicts,
  never from the unit's head sample (`Packer::unit_plan`). One sub-4 KiB `.flac`
  sorted ahead of `.go` made the head sample say "already compressed" and 8.36
  MB of source was stored raw. Only a unit with NO voters reads the head.
- REALIZED unit geometry (`NARC_UNIT_TRACE`, test/corpus at max, 5751 files):
  6 units, size-weighted mean 41.42 MiB, 79% of bytes in units >= 16 MiB. The
  cut rule is uniform. So "units land in the 4-16 MiB penalty band" is FALSE.
- Two-phase pipeline: `analyze::plan()` (magic → class → trial compress) gives
  codec+filter, then per-chunk compression. fast=zstd3, normal=zstd12,
  max=LZMA2/PPMd7 by class.
- `HEAD_SAMPLE` = 1 MiB, load-bearing: deflate leaves matches megabytes apart,
  so a level-1 zip reads as +0.02% on a 64 KiB sample and −25.6% on a 1 MiB one.
  Free — `add_file` chains the head in front of the file. Sub-tests keep their
  caps (`TRIAL_SAMPLE` 64 KiB; the delta detector MUST stay capped).
- Precompressed magic does NOT mean store — it is a claim about the FORMAT, not
  the bytes, and storing on it alone cost 1.12 MB on a 4.93 MiB corpus. A 1 MiB
  zstd-1 trial must save >= 1%, which separates cleanly: compressible deflate
  −3.9..−25.6%, genuinely finished data +0.00..0.01%.
- Memory invariant: all ops bounded by a few MAX_CHUNK (4 MiB) buffers +
  manifest in RAM. Keep it that way — weak-PC extraction is a hard requirement.
- Archive paths: relative, UTF-8, '/'-separated; `paths::sanitize` on extract
  rejects traversal/absolute/drive/ADS/reserved-device/trailing-dot names.
  Owner's machine: 32 GB RAM, RTX 5060 Ti 8 GB, Windows 11, 8 logical cores.

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
- GUI build trap, FIXED (the old "always build with the Tauri CLI" advice is
  gone with it): `devUrl` in tauri.conf.json made any non-Tauri-CLI build come
  out in dev mode, so the shipped exe opened localhost:5173 — and a plain
  `cargo build --release` silently overwrote a good exe with that one, twice.
  Now there is no `devUrl`, every build embeds `ui/dist`, and build.rs FAILS
  with an explicit message if `ui/dist/index.html` is missing — a stale frontend
  is a compile error, not a blank window. Run `npm --prefix ui run build` first.
- Windows: pass a path with spaces to the exe in QUOTES or Start-Process
  -ArgumentList splits it and argv[1] is truncated (this fooled a GUI test).
- .cmd launchers mangle Cyrillic (OEM codepage); use a BOM'd .ps1 instead.
- `zpaqfranz x ... -to DIR` needs a TRAILING SLASH or it reads the target as a
  single output file, refuses, and a benchmark records the refusal as the
  decode time. Cost an hour of "zpaqfranz decodes in 0.07 s".
- zstd already shrinks its window to the source size, so capping WindowLog
  for 4 MiB chunks changes nothing; per-worker memory is match tables.
- A formatter hook rewrites files after every Write/Edit in this repo.
- zstd/blake3 crates build C via MSVC — fine here; pure-Rust fallbacks only
  matter for exotic targets.
- git global identity: user.name "Brent", email confeden@cryptolab.net.

## Resource policy (decided, from research 09)

- Default: `SetPriorityClass(BELOW_NORMAL)` + per-handle
  `SetFileInformationByHandle(FileIoPriorityHintInfo=Low)` +
  `SetProcessInformation(ProcessMemoryPriority=BELOW_NORMAL)`. CPU priority
  alone does NOT prevent UI lag — I/O and memory pressure do it.
- Threads = ALL logical cores, workers at THREAD_PRIORITY_NORMAL inside the
  lowered class. No affinity pinning, no "leave one core free".
- Memory budget = clamp(min(0.5·avail, 0.25·total), 512 MiB, 8 GiB) via
  GlobalMemoryStatusEx; `--memory` override; workers W = min(T, budget/per_worker);
  job-object COMMIT limit at 2× budget as guardrail (never working-set caps).
- `--eco` (opt-in): IDLE class + EcoQoS + IoPriorityHintVeryLow.
  `--full`: NORMAL class + EcoQoS off.

## Compression design (measured, v0.6 max tier)

- No single codec wins: PPMd7 beats LZMA2 13-24% on prose/wiki/db-records,
  LZMA2 beats PPMd7 16% on binaries and 10-20% on source. MAX runs a per-unit
  tournament (LZMA2 + PPMd7 orders 10 & 16); fast/normal trust the analyzer.
- Chunk size = compression unit = edit granularity. fast/normal 256K/1M/4M;
  max 1M/4M/16M (FastCDC caps max at 16 MiB). Solid target 4/16/32 MiB by tier.
- Solid block boundaries are content-defined PER FILE (cut prob = size/target
  from the file's own blake3). A size-accumulator rule cost 17 MiB for a
  one-line edit, because one file's length shifted every later boundary.
- WHERE WE ACTUALLY STAND, measured on this machine against five references
  (`test/bench-std.sh`; binaries in `D:/Programs/compressors`). narc max is
  clearly AHEAD of the LZ family and clearly BEHIND the CM family:
  · enwik8  narc 22,466,101 / 71.7 s · zpaqfranz -m5 19,625,056 (−12.6%) ·
    kanzi -l9 20,035,684 (−10.8%) · 7z -mx9 24,799,487 (+10.4%) · xz 24,831,656
  · Silesia narc 43,674,657 / 54.9 s · zpaqfranz -m5 39,865,713 (−8.7%) ·
    kanzi -l9 41,857,930 (−4.2%) · 7z -mx9 48,688,268 (+11.5%) · xz 48,449,928
  · enwik9  narc 195,010,922 / 238.9 s · zpaqfranz -m5 168,590,780 (−13.5%) ·
    kanzi -l9 173,379,111 (−11.1%) · 7z -mx9 210,604,386 (+8.0%). Decode: narc
    160.8 s vs 7z 2.7 s — PPMd7 on a gigabyte of text.
  · Source tree: narc 9,292,017 vs 7z 9,131,720 → +1.8%.
- PPMd7 AS THE TEXT CODEC IS DOMINATED, measured on enwik8: kanzi -l9 is 10.8%
  SMALLER (20,035,684 vs 22,466,101), 5.5x faster to compress (13.1 s vs 71.7 s)
  AND faster to decompress (13.3 s vs 18.3 s). Not a ratio-for-time trade — it
  loses on all three axes at once. The remaining unknown is model memory, which
  is the thing to measure before replacing it.
- CORRECTION, and the old claim is now dead: narc max is NOT faster than 7-Zip.
  MEASURED Silesia 54.9 s vs 7z 44.4 s, enwik8 71.7 s vs 43.3 s. The "6.8 s vs
  49 s" figure predates the tournament and big units and must not be requoted.
- The DECODE advantage is real but DATA-DEPENDENT, and the difference is which
  codec the tournament picks. Silesia (mostly LZMA2): narc 6.5 s vs kanzi -l9
  50.5 s — 7.7x faster. enwik8 (PPMd7 wins): narc 18.3 s vs kanzi 13.3 s —
  SLOWER, and kanzi is also 10.8% smaller there. PPMd is symmetric, so on text
  it costs us both axes at once. That is an argument for a CM text codec, not
  against one: we are already paying CM's decode price without its ratio.
- kanzi -l7 is the point worth staring at: Silesia 47,308,780 B in 6.15 s —
  roughly 7-Zip's ratio at seven times its speed. That is the fast tier's bar.
- Cut floor + byte-majority verdict, on test/corpus when they landed: fast
  −7.6% with its edit cost halved, normal −0.2%, max −4.0%, Silesia −0.3%. The
  fast-tier win is the VOTE (the floor ALONE made fast 5.1 MB worse); max is
  the floor.
- Edit cost at max is ~4.7 MiB and ~40 s, NOT the "0.4-1 s / ~98 KiB" of the
  small-block era: the edited file lives in a 30-60 MB unit. Cheap edits are now
  a fast/normal-tier property (0.73 / 3.30 MiB).
- Gap decomposition BEFORE the floor (767,040 B): 81% in files >= 256 KiB, and
  54% of the WHOLE gap from ONE cut splitting a 22.57 MiB run of near-duplicate
  .exe builds — what the floor fixed. The manifest is 91-92 KB (12%), real cost;
  the last 171 KB was the class flush, tried and REVERTED.
- MANIFEST CODEC: a manifest of 128 KiB or more is offered to LZMA2 and the
  smaller result wins. MEASURED on the 5751-file source tree (682,799 B raw):
  zstd 9 92,043 B · zstd 19 84,565 B · LZMA2 74,690 B (−18.9%). Every archive
  shrinks by that, and the one-file EDIT gets cheaper too because a smaller
  manifest is appended. Commit costs +0.1 s. The codec is read off the BYTES —
  a zstd frame starts 28 B5 2F FD and a raw LZMA2 stream never can, its first
  byte being a chunk control — so there is no format field and every manifest
  ever written still decodes. Front-coding the paths would save another 7,663 B;
  not done, it is a real format change.
- narc's own encoder is 0.24% BETTER than liblzma -9e on identical boundaries,
  so its LZMA2 parameters are not the problem.
- Max-tier tournament on a source tree: LZMA2 wins 5 of 6 units = 99.94% of
  stored bytes; PPMd7 order 10 won one 35 KB unit, order 16 never won.
- Diagnostics: `NARC_UNIT_TRACE=<file>` logs one line per unit as it is built
  (idx, size, items, WHY it was cut, kind, deduped, ext histogram) — the cut
  reason exists only at pack time and is the whole point. `narc info --units`
  dumps the same units back out of a finished archive with the winning codec.
  Benchmarks: test/{compare-7z,edit-cost}.sh, test/{unit-anchor,
  unit-counterfactual,exe-split}.py. Corpora: test/Silesia-compression-corpus/
  raw, test/enwik8/enwik8, test/corpus, test/{small,big}256 (its 256 KiB split).

## Recompression — DEFLATE, JPEG and PDF ARE LANDED (research 02, measured here)

- Corpora: `test/precomp` (4.93 MiB of zips/PNGs/docx/gz), `test/incompress`
  (7z output + random bytes — the control that must stay stored), `test/photos`
  (camera JPEGs), `test/pdfs` (8.14 MiB, 19 documents from four producers:
  groff, LaTeX, Chrome/Skia, Firefox's own), `test/firefox` (installed program,
  68.6% .dll) + `test/ff-{dll,ja,rest}`. Probes live in `test/probe-preflate`
  (own crate, outside the workspace) and round-trip what they measure.
- MIN_STREAM = 64 bytes, MEASURED; the first guess of 4096 cost 15 percentage
  points. "A correction record cannot pay for itself on 2 KB" is true per FILE
  and false inside a unit, where a thousand plaintexts compress together.
- SHIPPED. Deflate corpus: narc 4,047,424 → **2,414,283 B (−40.4%)**, 7z -mx9
  4,047,465 B. Byte-exact on all 28 files; peak RAM 152 MiB at `--memory 256M`.
- JPEG (lepton, id 35) SHIPPED. test/photos, 6 camera JPEGs / 16.5 MiB: raw
  17,324,730 · 7z -mx9 17,289,234 (−0.20%) · **narc 13,990,872 B (−19.1% vs
  7-Zip)**. Byte-exact on every file. Photographs are the bulk of a family
  archive and the one thing every archiver gives up on.
  · The stored payload is the LEPTON FORM ITSELF, not a codec's output — it is
    already entropy-coded and no codec ever beat it, so `compress_job` keeps the
    smallest of {codec over filtered, filtered verbatim, original}. A Store
    record carrying a filter is new and safe: `filtered` records the coded
    length, and an old reader rejects id 35 rather than guessing.
  · Lepton settings are a FORMAT CONSTANT for id 35, like PPMd7's order and
    pool: `compat_lepton_vector_write`, single-threaded. Its 16386-pixel limit
    means gallery panoramas are not transformed.
  · Cost: lepton is slow, ~0.8 MB/s per thread.
- PDF SHIPPED, reusing filter id 34 (it is deflate; nothing about the format
  changed). test/pdfs, 8,536,741 B: narc max 7,626,464 → **5,290,810 B
  (−30.6%)**, normal 7,634,888 → 6,201,645, and 7z -mx9 reaches 6,606,978 —
  so narc lands **19.9% below 7-Zip** on documents, which it treats as finished
  data. Byte-exact on all 19; test/precomp and test/corpus are BYTE-IDENTICAL to
  before, so nothing else moved.
  · THE TRAP, and it is the whole difference between working and not: PDF's
    `/FlateDecode` is RFC 1950, so a stream carries two zlib header bytes and a
    four-byte adler32 that are NOT part of the deflate stream. Handed those,
    preflate modelled **0 of 957** streams while the scanner cheerfully reported
    72% coverage — a feature fully wired up and doing nothing. Strip 2 + 4; the
    framing keeps the six bytes verbatim because nothing covers them.
  · The scan is LEXICAL, not an object parse. "PDF needs a full object parse"
    was the reason it was deferred and it was WRONG: a stream's bytes are found
    the same way either way, and a wrong range cannot corrupt anything — preflate
    must consume exactly what it is handed, and the packer round-trips the unit.
  · Scanner rules: `stream` must follow `>>`; `/Filter`'s FIRST name must be
    `/FlateDecode`; `/Length` needs whitespace after it or `/Length1` (a Type 1
    font segment size) is read as the length; an indirect `/Length 9 0 R` falls
    back to `endstream`, and a MISSING `endstream` stops the whole scan.
  · EVERY per-candidate backward scan needs a FLOOR at the previous candidate's
    end, not just a window. With a fixed 16 KiB lookback, `%PDF-` + `>>stream`
    repeated matched neither `obj` nor `/Filter`, so both sweeps ran in full:
    MEASURED 47 s for 16 MiB against 45 ms of real PDF, and the magic alone is
    the entry ticket. The floor makes it linear (47 s → 0.012 s) and is STRICTER
    — a dictionary cannot begin inside the stream before it.
  · Measured coverage: 74.1% of corpus bytes are FlateDecode, 72.6% of them
    modelled, and the recovered plaintext is **5.96x** the deflate it replaces.
  · Diagnostics: `test/probe-preflate --bin pdfscan` (streams, coverage,
    modelling rate) and `--bin pdfhostile` (the timing above).
- Installed-program corpus (test/firefox, 341.7 MiB, 68.6% .dll, 28% omni.ja):
  7z -mx9 87,566,439 B, narc 95,721,462 B → **+9.3%, narc's weakest case**.
  Roundtrip byte-exact. Findings from it:
  · FIXED: the per-file vote for codec+filter was cast once, into whichever unit
    was open when the file STARTED, so 150 MB of a 176 MB xul.dll landed in
    units with no BCJ at all. It is now cast per CHUNK (`Packer::current` +
    `place`), worth 506,834 B here.
  · The gap is ALL x86: .dll +11.8%, omni.ja +0.1%, everything else −6.2%.
  · SOLIDITY IS NOT THE CAUSE (234.3 MiB DLL set, LZMA2 -9e): solid+BCJ
    70,463,272 B · 64 MiB units 71,410,491 · 32 MiB 72,082,105 · no filter
    72,509,466. Geometry costs 1.3%, in-place BCJ is worth 2.8%, and 7-Zip sits
    8.7% BELOW solid-with-BCJ: the difference is the FILTER, not the size.
  · 7-Zip's advantage is BCJ2: it routes the 4-byte call/jump targets into
    separate streams so high-entropy addresses stop interrupting the code.
    CORRECTION to research 16, which listed BCJ2 as "the other free one" —
    `lzma-rust2` 0.19 ships a BCJ2 **decoder only**, so encoding means writing
    the transform. narc's private container needs no 7z compatibility.
  · SHIPPED as filter id 36, narc's own transform. Firefox 95,721,462 →
    **93,467,847 B (−2.4%)**, source tree → **9,309,370 B (−2.1%)**, Silesia
    −14,744 B. Gap to 7-Zip: Firefox +9.3% → **+6.7%**, source +4.1% → +1.9%.
  · THE RULE, and three that measured worse. Take a site when the absolute
    target lands INSIDE the buffer: 68,893,556 B at 64 MiB units, vs 68,994,337
    with no test, 72,488,469 with liblzma's top-byte-is-00-or-FF test (WORSE
    than not splitting — at 64 MiB the addresses land in 0..64M and the top byte
    is 0x01-0x03), and 69.5-71.1 MB gating on displacement magnitude. The WIDTH
    of the accepted range is what matters, and the unit size bounds it. Unit
    size PRICED and rejected: 128 MiB buys −1.1%, 256 MiB (no editability at
    all) −4.0% and still 2.8% above 7-Zip. The residual is the filter's decision
    quality — BCJ2 learns a probability per site, this applies a fixed test.
  · Concatenating the split's three streams into one unit buffer costs NOTHING
    against compressing them separately (−0.01%).
  · The site COUNT is in the header and is load-bearing: the decoder walks a
    stream the displacement bytes are gone from, so at the tail it finds one
    opcode more than the encoder did.
  · Per-unit LZMA lc/lp/pb — MEASURED AND REJECTED. Worth 1.2% on the target
    stream alone, but on whole units the best of six parameter sets beats the
    default by 0.01-0.16%. It does not generalise, and the target stream cannot
    get its own parameters without splitting the unit into two coder streams.
- HOW IT IS WIRED, and these are FORMAT RULES, not implementation detail.
  `ChunkRec.filtered` (0 = "same as unpacked") is the length the CODEC produced;
  `unpacked` keeps its one meaning forever, the ORIGINAL length that `hash`
  covers and `Extent` indexes into. `lzma2_dict_size` and `ppmd7_mem_size` must
  be derived from the CODED length on both sides or every existing archive stops
  decoding. `Filter::apply` returns `Applied::{InPlace, Rebuilt}` because the
  store fallback must undo an in-place filter and must NOT undo a rebuilt one.
  Ids 34/35 pin an external library version; an upgrade must spend a NEW id.
  A recompressible file gets a unit of ITS OWN (>= 64 KiB, <= 2x unit): the zip
  scanner finds the central directory by searching backwards, so a concatenation
  would read the last archive's directory as the first one's; lepton needs one
  whole JPEG; the PDF scanner needs one whole PDF.
- SAFETY, enforced not intended: the packer round-trips every rebuilt unit and
  falls back on any mismatch; a filter that refuses is a fallback, not an error;
  the transformed form may not exceed `MAX_CODED_CHUNK` (256 MiB), charged as
  the pieces are BUILT and checked again before any decode allocation, so a
  bomb drives memory at neither end.
  · THE CODED-LENGTH BOUND BELONGS IN `compress_job`, beside `filtered =
    data.len()`, and nowhere else: only that line knows the number that reaches
    the manifest, and `verify_chunk` REFUSES a coded length above the cap — an
    unchecked one is an archive narc writes and cannot extract. Per-filter
    budgets do not substitute: `deflate_encode` charged plaintexts but not the
    container bytes `encode` also emits, so a ~57 MiB PDF cleared a 256 MiB
    budget and produced more.
  · Never size a Vec from a count a payload claims. `decode` bailed correctly on
    an implausible stream count but had already reserved for it, and one record
    costs ~96 bytes of parsed structure against three bytes of header — 32x, out
    of three bytes of lying. Grow as records are actually parsed.
- `preflate-rs 0.7.6` (Microsoft, Apache-2.0, pure Rust, forbid(unsafe)):
  `preflate_whole_deflate_stream` → `(corrections, plain_text)` and
  `recreate_whole_deflate_stream`.
- TRAPS, each of which would have shipped a SILENT mis-decode. All fixed; they
  are recorded because the next length-changing filter meets every one again:
  · The store-fallback test must compare against the ORIGINAL length, not the
    post-filter one, or a filter that expands 2 MB to 20 MB and compresses back
    to 2.5 MB "wins" against the wrong number. It must reset the coded length as
    well as the filter byte — the two are only equal while no filter resizes.
  · An in-place filter MUST be unapplied on that fallback and a rebuilt one MUST
    NOT be. One boolean cannot express both; hence `Applied`.
  · Widening the delta range to reach a new id makes it decode as `Delta(id-1)`.
  · The deflate class cannot be decided by scanning the head — a zip's central
    directory is at the END of the file. Detect by magic.
  · PPMd7's pool must match EXACTLY (unlike LZMA2, where a wider decoder window
    is safe) and it saturates at both ends, so a length-bookkeeping bug passes on
    big units and fails only in a band. LZMA2 with a narrow window fails
    data-dependently — that is, as intermittent corruption.
- COMPAT FIXTURES: `tests/fixtures/legacy-{max,normal,ppmd}.narc` are real
  archives from the pre-recompression build (store, zstd, LZMA2, LZMA2+BCJ,
  PPMd7 order 16). `archives_from_before_recompression_still_extract` extracts
  them fully, verifying every unit against its blake3. Any change to the two
  derived numbers (LZMA2 window, PPMd7 pool) is caught here and NOWHERE else.

## GPU policy (decided, from research 08)

- GPU is an OPTIONAL accelerator behind a codec-provider trait + `gpu` cargo
  feature + `--gpu auto|on|off`; never a format dependency.
- Only standard bitstreams (zstd/LZ4) via nvCOMP batched API, loaded
  dynamically at runtime (nvcomp.dll never vendored — proprietary EULA).
- VRAM budget ~2 GiB default on 8 GB cards; batches 256–512 MiB double-buffered;
  auto-mode skips GPU for jobs < ~256 MB.
- Rust: cudarc (mature, dlopen) + own small `-sys` binding for nvCOMP.

## Negative knowledge

- Ordering files by content similarity instead of by name — research 16 called
  it "the strongest surviving item"; MEASURED at −0.07% on 33 Windows DLLs.
  Files of one extension are already adjacent, and a unit is mostly one or two
  large files, so which of them share a unit barely moves anything.
- Per-unit LZMA lc/lp/pb — 0.01 to 0.16% on real units. See Recompression.
- PPMd var.I (`ppmd-rust` ships `Ppmd8Encoder`, research 15 called it a free
  fourth codec) — MEASURED and REJECTED. On 1-24 MiB of real source text it is
  within 0.15% of var.H and slightly WORSE above 4 MiB (24 MiB: 2,211,987 vs
  2,211,670). It also has a live bug: `RestoreMethod::CutOff` SEGFAULTS at 16 MiB
  with a 256 MiB pool, reproducible with `test/probe-preflate --bin ppmd8min`.
  Do not add it to the tournament on the strength of the literature.
- Firefox's `omni.ja` is NOT recompressible: it parses as a zip but every entry
  is method 0, stored — Mozilla keeps it uncompressed so the browser can mmap it
  at startup. No deflate to undo, which is why narc and 7-Zip land within 0.1%
  of each other. A tail-EOCD zip scan would find these files and gain nothing.
- Shared/trained dictionaries at creation time — MEASURED NET LOSS at 32 MiB
  units (100.0-100.8% of the no-dictionary total). A dictionary SUBSTITUTES for
  solidity rather than adding to it. Only the append path (units created after
  `create`) is worth revisiting: -21% to -31% there.
- Alphabet/numeral-system transforms — folklore in front of a modern codec.
  MEASURED: frequency remapping 0.0%, RLE ~1%, MTF +202% (catastrophic),
  sparse packing 0.4% for zstd. Only base64-undo is real (-28%).
- Byte transposition on float data (sao) — +25 to +29%, worse than raw.
- Applying the record-width filter to data that already compresses — measured
  worse; the filter is only for data that would otherwise be stored raw.
- Flushing a unit on every change of content class — shatters a source tree
  into 246 units (median 1.4 KiB) and costs 1.8 MiB. Only files >= 4 KiB may
  trigger a class split.
- Letting a class change slide until the unit is half the target — TRIED AND
  REVERTED: a 171 KB win in a fixed-codec counterfactual cost +1,002,157 B at
  max and +598,383 B on Silesia in the real packer. THE LESSON invalidates any
  future fixed-codec counterfactual: one LZMA2 chain cannot see what a mixed
  unit gives up, because a unit has ONE codec and ONE filter. Voting by bytes
  does not rescue it — one winner cannot filter two classes.
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
- LZMA2 as the universal max-tier codec — on 4 MiB chunks it is only ±2 % vs
  zstd-19 on text (its edge comes from >4 MiB dictionaries, which chunking
  removes) while being much slower. It stays only for binary/generic data;
  text goes to PPMd7 (-24 %).
- Capping zstd WindowLog for 4 MiB chunks — no effect, zstd already shrinks
  the window to the source size.
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

- Language: Rust PREFERRED, not required. Owner-set: the GUI must be Rust and
  as much else as practical, but a codec may be C/C++ behind FFI if Rust has no
  usable implementation — do NOT reject an algorithm for want of a Rust crate.
  Open repo `confeden/Nova-Arc`; no LICENSE file yet (owner will choose).
  Zero telemetry/ads/analytics — ever. `.gitignore` is NOT tracked; the rules
  live in `.git/info/exclude` (owner's decision) — do not re-add the file.
- BENCHMARK SET is plural, not just 7-Zip: 7z -mx9, xz -9e, brotli -q11,
  kanzi -l7/-l9 (CM), zpaqfranz -m4/-m5 (CM). Binaries in
  `D:/Programs/compressors`. Corpora must be STANDARD (enwik8, enwik9, Silesia)
  so our sizes sit next to published ones; improvised corpora only for the
  recompression differentiator. Harness: `test/bench-std.sh`.
- Speed budget: not much slower than 7z -mx9. This DISQUALIFIES cmix, paq8px,
  nncp and every LLM compressor by construction, not by taste. They stay as a
  CEILING reference — how much redundancy is left — never as a target.
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
- Resource policy: use all cores but at below-normal priority (system must
  stay responsive); bounded, configurable memory (weak PCs: extract must
  always work); GPU (CUDA/nvCOMP-class) acceleration to be attempted.
- Test everything under `test/` (gitignored playground).

## Open issues

- Not preserved yet: empty dirs, symlinks, NTFS attrs/ADS, ACLs.
- Manifest fully in RAM (fine ≤ ~1 TB); paged index only if needed. Long paths
  (>260 chars) on Windows untested.
- Solid-block members are read whole into RAM at pack time (fs::read), so a
  block costs up to 8 MiB extra outside the pipeline budget.
- No trained dictionaries; no MP3/audio recompression.
- A container larger than 2x the unit (64 MiB at max) is never recompressed —
  it cannot be given a unit of its own, so a big PDF or zip silently loses the
  feature and takes the ordinary already-compressed path.
- `--eco`/`--full`/EcoQoS paths are built but not measured under load; the
  "no lag" claim is unverified (research 09 §10 has the methodology).
- Progress granularity at max is bounded by the UNIT COUNT and cannot be fixed
  in the reporter: ~6 units compress in PARALLEL, so between two completions
  there is genuinely no finished work to report (~38 s of a ~70 s pack). The UI
  answers with elapsed time, a block counter and a pulse. Finer would mean
  reporting per tournament candidate — not built.

## Plans

DONE: v0.2 (pipeline, memory, priority) · v0.5 (filters, solid groups,
LZMA2/PPMd) · v0.6 (tournament, geometry, cut floor, byte-majority verdict) ·
v0.7 recompression — deflate id 34, JPEG id 35, x86 split id 36, PDF · GUI
(basic) · manifest LZMA2.

The standing direction is the owner's: beat 7-Zip where it has NOTHING, rather
than out-tune LZMA. Remaining, in measured order of value:

1. Byte-plane split for detected 16-bit data. MEASURED -10..-11% on x-ray and
   mr, free at decode, ~150 lines. The only measured, unbuilt item left.
2. More formats where the competition stores bytes verbatim — the cheapest and
   most exclusive win we have. Next: PDF images (`/DCTDecode` streams are whole
   JPEGs and lepton is already wired), then audio.
3. Replace PPMd7 on text. It is dominated on all three axes (see Current state).
   Measure the ratio-vs-model-memory curve on kanzi -l8 vs -l9 FIRST — that one
   experiment decides whether a CM tier is viable at all, and costs no code.
   Rust is not required for the codec: FFI to a C implementation is allowed.
4. Executable modelling (BCJ2-class, section splitting) — 51% of the remaining
   headroom to paq8px on Silesia, five times images and tables combined.
5. zip/7z unpack + zip pack (sevenz-rust2), rar unpack (unrar).
6. GUI: shell icons/thumbnails (research 06/07), .narc association + installer,
   in-archive folder tree, RU localization.
7. Later: GPU experiments (research 08), encryption (XChaCha20-Poly1305),
   Explorer integration, installers, Linux/macOS/Android ports.

NOT on this list, and deliberately: bigger units to buy solidity on executables,
and BCJ2-style per-site probability. Both PRICED and rejected — see Recompression.
