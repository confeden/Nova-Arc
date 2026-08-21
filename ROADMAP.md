# Nova Prism — working context (Claude-only, English)

## Current state

- Cargo workspace, Rust 1.95, edition 2021: `nova-core` (format,
  `#![forbid(unsafe_code)]`), `nova-cli` (binary `nova`), `nova-platform` (OS
  and unsafe), `nova-bsc` (libbsc FFI), `nova-gui` (Tauri 2), `ui/` (TS+Vite).
- Format v0.2 = v0.1 + solid blocks + per-chunk filter/param bytes + LZMA2/PPMd7
  + manifest `geometry`. Container VERIFIED: append-only chunk log, FastCDC,
  blake3-128 dedup+integrity, MessagePack+zstd manifest, 80-byte offset-bound
  footer, generation counter, resumable crash recovery, rw-open truncates the
  uncommitted tail, `compact` verifies then replaces atomically. `docs/format.md`.
- Packing pipeline (`pipeline.rs`): reader hashes+dedups, workers compress,
  writer appends in submission order, byte-budget backpressure; extract threads
  key off the archive's codecs. Flags `-j`/`--memory`/`--eco`/`--full`.
- CLI: create/add/extract/list/remove/rename/compact/info (+aliases); extract
  has `--force`/`--skip-existing` (default: refuse to clobber). `rename` moves
  an entry or folder by rewriting the manifest ONLY: 77 entries in 0.053 s.
- GUI (nova-gui, VERIFIED running, UI fully RU): open/create/add/extract/
  remove/compact, virtualized list with glyphs + unit badges, sortable columns,
  multi-select, context menu, double-click opens from a temp dir, Explorer
  drag&drop, throttled progress, level/memory in toolbar (DEFAULT max), argv[1].
- `test/` = playground (gitignored), benches `test/*.sh`.
- 125 tests green (130 with `--features rar`), clippy clean in both. Covered:
  roundtrip/append/dedup/replace/remove/rename/selective extract · crash
  recovery, torn manifest, embedded+forged footer, writer lock · overwrite
  policy, pre-1970 mtime, zip-slip · progress contract, decode lanes · every
  filter round-trip (deflate/JPEG/PDF/PCM/split .wav/MP3) · legacy fixtures.
- Compression v2 DONE: codec+filter per class, solid blocks by extension,
  LZMA2 + PPMd7 + bsc, BCJ x86 + delta (BCJ verified against liblzma).
- Peak RAM tracks `--memory` on EXTRACT (256M→153 MiB, default→1.0 GiB).

## Architecture & invariants

- Archive layout: `[header][manifest g1][footer g1]` from `create`, then
  `[chunks…][manifest gN][footer gN]` per update; committed bytes are NEVER
  rewritten except by `compact`. Commit = manifest → fsync → footer → fsync;
  without the barrier a valid footer can point at a torn manifest.
- Footer self-hash covers its own absolute offset, so a `.nva` inside another
  archive cannot be mistaken for a commit. Readers verify each candidate's
  manifest and resume the backward scan on failure (≤64).
- A SKIPPED FOOTER IS DAMAGE, NOT A CRASH, and confusing the two destroyed the
  archive. Commit order is manifest → fsync → footer → fsync, so a crash leaves
  NO valid footer in the tail — which is why `open_rw` may truncate it. A footer
  that passed its self-hash but whose MANIFEST will not decode is a COMMITTED
  record, and everything behind it is committed data. `Archive::damage` records
  that; `open_rw` REFUSES, `open_ro` warns, `info` stops calling those bytes
  reclaimable. MEASURED before the fix on 12,583,583 B: one flipped manifest bit
  made `list` print "0 file(s)" exit 0, `info` advise `compact`, and `compact`
  then write 133 B and `add` 429 B, both reporting success.
- Packing invariant: the writer appends chunks in submission order, so the reader
  predicts each index as `base + submission index` and builds entries early.
- PROGRESS CONTRACT (`Progress`, `Phase`, `archive::Reporter`). `bytes_done` =
  source bytes whose work is FINISHED, fed by the writer; `bytes_read` = what
  the reader took off disk, up to an in-flight budget ahead. Monotone, never
  above the total, equal to it exactly once — the single `Phase::Done` reading,
  others clamped to total-1 so "100%" structurally means finished. INVARIANT:
  never hold the Reporter mutex while entering the pipeline or the reader
  sleeps in `Budget::acquire` holding it. Throttle in core, clock in the GUI.
- Memory model: `budget ≈ 32 MiB base + workers × (tables + 8 MiB) + queued`;
  workers capped by budget, in-flight bytes by workers (table cost per tier
  in `Tier::worker_memory()`). Chunk hash = blake3(uncompressed)[..16]:
  dedup AND integrity; dead records stay as dedup sources until compact.
- INTRA-FILE DECODE LANES. Threads the file count cannot use become lanes
  INSIDE each file (`lanes_per_worker = budget / workers`), so a one-file
  archive stops using one thread while concurrency, memory, ordering, overwrite
  policy and mtime all stay put. enwik8 at normal 4.80 → **1.63 s**. GROUP BY
  DISTINCT UNIT, not by extent: consecutive extents usually share a unit and
  the sequential path got that free from `UnitCache`; per extent it decoded the
  same unit once per lane, enwik8 4.8 → 8.2 s. Fewer handles, fewer lanes.
- PPMd7 order (10) and pool (32x CODED len, clamp 1 MiB..256 MiB) and the LZMA2
  dict (clamp 4096..64 MiB) are FORMAT CONSTANTS
  — not stored per chunk, so changing them breaks old archives. Order 10 beat
  12/16: a pool-exhaustion restart costs more than depth gains.
- LZMA2 presets 6..9 differ only in dictionary size, which the chunk cap
  overrides — so max raises nice_len instead (xz -e).
- Codec and filter ids are tabulated in `docs/format.md`; keep it in step. A NEW
  id must never be added by widening the delta range — `2..=MAX_DELTA_ID =>
  Delta(id - 1)` would have made 34 decode as Delta(33), silently. Per-chunk raw
  fallback if not smaller, and then the filter byte MUST be cleared too.
- Fixed order: pack = filter → compress; unpack = decompress → unfilter. The
  chunk hash covers the ORIGINAL bytes, so dedup and integrity are filter-
  independent and the hash check proves the filter round-tripped.
- BCJ always runs with start_offset 0, never the chunk's file position, or
  position-dependent output breaks dedup. Cost: one instruction per boundary.
- Solid blocks: files < `Geometry::chunked_from` (256 KiB fast/normal, 1 MiB
  MAX) sort by extension into `Geometry::unit` streams (4/16/32 MiB), one unit
  each — bounded ON PURPOSE, so editing a member rewrites one block. Cuts are
  content-defined per file with a hard flush at 2x target, so blocks run LARGER
  than target, never smaller; a cut may fire only at or above HALF the target,
  the low tail being loss that also destabilised boundaries. MEASURED both ways.
- A unit's codec+filter come from a BYTE-MAJORITY VOTE of the per-file verdicts,
  never the head sample (`Packer::unit_plan`): one sub-4 KiB `.flac` ahead of
  `.go` made the head say "already compressed" and 8.36 MB was stored raw. Only
  a unit with NO voters reads the head. Two-phase pipeline: `analyze::plan()`
  (magic → class → trial) gives codec and filter; fast=zstd3, normal=zstd12+bsc,
  max=all four.
- `HEAD_SAMPLE` = 1 MiB, load-bearing: deflate leaves matches megabytes apart,
  so a level-1 zip reads as +0.02% on 64 KiB and −25.6% on 1 MiB. Free —
  `add_file` chains the head in front of the file. Sub-tests keep their caps
  (`TRIAL_SAMPLE` 64 KiB; the delta detector MUST stay capped).
- Precompressed magic does NOT mean store — it claims a FORMAT, not bytes;
  storing on it alone cost 1.12 MB on a 4.93 MiB corpus. A 1 MiB zstd-1 trial
  must save >= 1%: compressible deflate lands at −3.9..−25.6%, finished data at
  +0.00..0.01%. Memory invariant: ops bounded by a few MAX_CHUNK buffers plus
  the manifest — weak-PC extraction is a hard requirement.
- Archive paths: relative, UTF-8, '/'-separated; `paths::sanitize` on extract
  rejects traversal/absolute/drive/ADS/reserved-device/trailing-dot names.
  EVERY writer (.nva and foreign alike) walks inputs through the one
  `paths::walk_inputs`, so what `create` accepts cannot drift per format.
  Owner's machine: 32 GB RAM, RTX 5060 Ti 8 GB, Windows 11, 8 logical cores.

## Gotchas

- A zip entry may use `\` instead of `/` (PowerShell's `Compress-Archive`
  does; real 7z from 7-Zip.exe did not, but `foreign_7z` normalizes the
  same way on principle) — fix BEFORE `paths::sanitize`, not inside it, so
  `..\` still hits the real `..`-component rule. `foreign_7z::extract` uses
  `for_each_entries`, one decode pass, never `read_file` per entry, which
  the crate's own docs say redecodes a whole solid block per file.
- Windows: cannot rename over an open file — compact consumes `self`, closes the
  handle, then replaces in place. `tempfile` is a REGULAR dep for it.
- Windows file locks are MANDATORY per byte range: a whole-file `File::try_lock`
  makes our OWN extraction threads fail with ERROR_LOCK_VIOLATION (33). Hence
  `try_lock_exclusive` locks one byte at 0xFFFF_FFFF_FFFF_0000, never real data.
- `File::try_clone` SHARES the file position on Windows: extraction workers
  must each `File::open` the archive, never clone a handle.
- GUI: run `npm --prefix ui run build` FIRST. No `devUrl` in tauri.conf.json —
  it made every non-Tauri-CLI build come out in dev mode, so the shipped exe
  opened localhost:5173 and a plain `cargo build --release` silently overwrote
  a good exe with it, twice. Builds embed `ui/dist`; build.rs FAILS without it.
- INSTALLER: `ui/node_modules/.bin/tauri build`, run FROM `crates/nova-gui`
  (the CLI hunts for tauri.conf.json under the cwd). `bundle.icon` must list an
  `.ico` or bundling dies with "Couldn't find a .ico icon" AFTER the whole
  release build. NSIS only, EN+RU with a selector: MSI bakes its language in and
  WiX writes cp1252, so Cyrillic in `fileAssociations.description` died with
  LGHT0311 — and that string is ONE registry entry for both languages anyway.
  Tauri's NSIS template also bootstraps the WebView2 runtime, without which a
  Tauri app cannot start; hand-rolling (Inno) means reimplementing that.
- Windows: quote a path with spaces or Start-Process -ArgumentList splits it and
  argv[1] is truncated. .cmd launchers mangle Cyrillic (OEM codepage), so use a
  BOM'd .ps1. A formatter hook rewrites files after every Write/Edit.
- `zpaqfranz x ... -to DIR` needs a TRAILING SLASH or it refuses, and a bench
  records the refusal as the decode time. zstd/blake3/libbsc/unrar build C or
  C++ via MSVC. git identity: "Brent", confeden@cryptolab.net.

## Resource policy (decided, from research 09)

- Default composes THREE APIs, because CPU priority alone does NOT prevent UI
  lag — I/O and memory pressure do: `SetPriorityClass(BELOW_NORMAL)` +
  `FileIoPriorityHintInfo=Low` per handle + `ProcessMemoryPriority=
  BELOW_NORMAL`. Threads = ALL logical cores at THREAD_PRIORITY_NORMAL inside
  that lowered class; no affinity pinning, no "leave one core free".
- Memory budget = clamp(min(0.5·avail, 0.25·total), 512 MiB, 8 GiB) via
  GlobalMemoryStatusEx; `--memory` override; W = min(T, budget/per_worker);
  job-object COMMIT limit at 2x budget (never working-set caps). `--eco`: IDLE
  + EcoQoS + IoPriorityHintVeryLow. `--full`: NORMAL, EcoQoS off.

## Compression design (measured, v0.6 max tier)

- No single codec wins: PPMd7 beats LZMA2 13-24% on prose, LZMA2 beats PPMd7 16%
  on binaries. MAX runs a per-unit tournament (LZMA2 + PPMd7 o10/o16 + bsc);
  fast trusts the analyzer, normal races it against bsc.
- THE TOURNAMENT IS NOT FOR EVERYTHING. A unit classed Precompressed gets
  normal's two-horse race: PPMd7 models symbol contexts, an entropy coder has
  left none. On 268 MB of FLAC that is BYTE-IDENTICAL output in 28-43 s instead
  of 109-306, priced at +315 B on the deflate corpus.
- TOURNAMENT WINNERS (`info --units`): enwik8 bsc 2 · Silesia bsc 7 / lzma2 4 ·
  source lzma2 4 / ppmd7-10 1 · Firefox lzma2 13 / **ppmd7-16 4** · pdfs lzma2
  14 / **ppmd7-16 5**. Order 16 earns its place — "it never wins" is true of
  the SOURCE TREE only, and dropping it would cost real bytes.
- Chunk size = compression unit = edit granularity. fast/normal 256K/1M/4M, max
  1M/4M/16M (FastCDC caps at 16 MiB); solid target 4/16/32 MiB by tier. Solid
  boundaries are content-defined PER FILE (cut prob = size/target from the
  file's own blake3): a size-accumulator rule cost 17 MiB for a one-line edit,
  because one file's length shifted every later boundary.
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
    −0.7 / +5.6, .wav −20.5 / **−28.7** — x-ray's zstd margin is the LARGER one,
    so no threshold picks right (by zstd the filter costs 0.77% on Silesia).
- WHERE WE STAND (`test/bench-std.sh`, max tier; competitors predate bsc).
  · enwik8  nova **21,506,314** · zpaq -m5 19,625,056 · kanzi -l9 20,035,684 ·
    7z -mx9 24,799,487 · xz 24,831,656
  · Silesia nova **43,036,408** · zpaq -m5 39,865,713 · kanzi -l9 41,857,930 ·
    7z -mx9 48,688,268 · xz 48,449,928
  · Source: nova 9,292,017 vs 7z 9,131,720 → +1.8%. PPMd7 AS THE TEXT CODEC IS
    DOMINATED: kanzi -l9 is smaller and faster, bsc out-votes it.
- nova max is NOT faster than 7-Zip: Silesia 52.7 s vs 44.4 s, enwik8 77.7 s vs
  43.3 s; the old "6.8 s vs 49 s" predates the tournament, never requote it.
  DECODE is data-dependent: Silesia 1.8 s vs kanzi -l9's 50.5 s; enwik8 4.6 s.
- kanzi -l7: Silesia 47,308,780 B in 6.15 s, the fast tier's bar. Its default
  `-j` is HALF the cores, so bench-std gave it 4 threads against our 8.
  · SHIPPED as codec id 4 (`crates/nova-bsc`, libbsc 3.3.12, Apache-2.0), a
    fourth MAX candidate: enwik8 −4.3%, Silesia −1.5%, source UNCHANGED, encode
    +8%. Block size is MILD on text (32→128 MiB −5.4%), decisive on a file tree.
  · Wiring: `bsc_qlfc_init_model` memcpy's the global model per call, so workers
    are safe after one `bsc_init`. `bwt.cpp`/`st.cpp` include CUDA headers
    UNCONDITIONALLY — vendor both; Windows needs `advapi32`, C and C++ separate.
  · ALSO AT NORMAL, the bigger win: enwik8 −24.6%, Silesia −19.0%, pdfs −10.0%,
    source −6.2%, for 1.6-1.9x the encode; normal beats 7z -mx9 in 1/6 its time.
  · TRAP: `bsc` decodes 100 MB in 0.9 s — multithreaded across blocks, but
    nova disables libbsc's own threads, so single-threaded it's ~25 MB/s.
    Treating it as fast in `extract_workers` made normal Silesia extract in
    8.7 s against zstd's 0.5 s; real cost: 0.5 → 2.1 s for −19%.
  · FAST STAYS WITHOUT IT, measured twice (the second after decode lanes, the
    condition the first refusal named): fast+bsc = 47.8 MB / 2.90 s / 3.19 s
    against NORMAL's 46.4 / 7.0 / 2.09 — better only in pack time, a worse
    normal. libbsc is C++ and nova-core forbids unsafe, hence its own crate.
- Cut floor + byte-majority verdict on test/corpus: fast −7.6%, normal −0.2%,
  max −4.0%. The fast win is the VOTE (the floor ALONE made fast 5.1 MB worse).
  Edit cost at max is ~4.7 MiB and ~40 s — the edited file lives in a 30-60 MB
  unit; cheap edits are a fast/normal property (0.73 / 3.30 MiB).
  nova's LZMA2 is 0.24% BETTER than liblzma -9e on identical boundaries.
- MANIFEST CODEC: a manifest >= 128 KiB is offered to LZMA2 and the smaller
  wins (682,799 B raw: zstd 19 84,565 vs LZMA2 74,690). The codec is read off
  the BYTES — a zstd frame starts 28 B5 2F FD, a raw LZMA2 stream never can —
  so there is no format field and every manifest ever written still decodes.
  Diagnostics: `NOVA_UNIT_TRACE=<file>` logs one line per unit as it is built
  (the cut reason exists only at pack time); `nova info --units` the rest.

## Recompression — DEFLATE, JPEG and PDF ARE LANDED (research 02, measured here)

- Corpora: `test/precomp-web` + `test/audio-pub`/`-pub-wav` (public, SHA-pinned),
  `test/incompress` (control, must stay stored), `test/photos`, `test/zipphoto`
  (same photos, zipped Store), `test/pdfs`, `test/firefox`. Probes in
  `test/probe-preflate` and `test/size-probe`. `/test/` is ignored EXCEPT the
  recipe — manifests, fetch scripts, harnesses — a reproducibility claim nobody
  can open is not one. Photos are Commons unpinned and PDFs printed here;
  README says so PER CORPUS, do not regress it into "all public".
  · COMMONS THROTTLES BULK FETCHES HARD (429; six retries at 120 s were not
    enough for seven files). `fetch-audio.py` resumes; bulk wants archive.org.
- MIN_STREAM = 64 bytes, MEASURED; 4096 cost 15 points. "A record cannot pay
  for itself on 2 KB" is true per FILE, false inside a unit. `preflate-rs 0.7.6`
  is byte-exact on every file of both corpora.
- REPRODUCIBLE DEFLATE CORPUS, and it earns its keep: `test/precomp-web`,
  93,399,733 B in 29 files (Python docs zip, GNU tar.gz, Maven jar, epub, apk,
  the 24 Kodak PNGs). nova max **74,865,900 (80.2%)** against the best of the
  rest — zpaqfranz -m5 86,761,587, 7z -mx9 86,991,164 — **−13.7%**, by anyone.
  · A SECOND CEILING: 28 of 29 units transform; `binutils-2.42.tar.gz` FITS the
    solo cap at 51,892,456 B yet is refused, expanding to 319,897,600 B past
    `MAX_CODED_CHUNK` (256 MiB) — a bound that scales with how well the payload
    compresses, so it bites hardest where recompression pays most. Now charged
    PER STREAM (per unit, one oversized member made the WHOLE unit bail and
    lost its neighbours' gain); binutils is a lone stream. See Plans 1.
  · fast lands at 99.7% here and that is CORRECT: PNG-filtered photographic
    scanlines do not beat the original deflate under zstd-3.
- STORED ZIP ENTRIES ARE SCANNED TOO — a zip does not deflate what deflate
  cannot help, so a photo backup STORES its JPEGs and skipping method-0 threw
  away the archive's best data. `zip` hands such an entry back to `dispatch`
  (depth-capped at 3). A bare JPEG is dispatched only at depth > 0 — at the top
  it is filter 35's business.
  · MEASURED on six camera photos zipped with Store: 17,326,548 →
    **13,992,258 (80.8%)** vs the best of the rest, 16,854,983 — **−17.0%**
    where we used to store it too. Only 27,781 B on the public corpus.
- JPEG (lepton, id 35) SHIPPED. test/photos, 6 camera JPEGs: raw 17,324,730 ·
  best of the rest 16,844,561 · **nova 13,990,872 (−16.9%)**.
  · The stored payload is the LEPTON FORM ITSELF: already entropy-coded, no
    codec ever beat it, so `compress_job` keeps the smallest of {codec over
    filtered, filtered verbatim, original} — a Store record carrying a filter
    is safe. Lepton settings are a FORMAT CONSTANT for id 35 like PPMd7's pool
    (`compat_lepton_vector_write`, single-threaded); its 16386-pixel limit
    skips panoramas, and it is slow, ~0.8 MB/s per thread.
- PDF IMAGES SHIPPED as filter id 37, a mixed container. 18.5% of a real
  19-doc corpus is `/DCTDecode` — whole JPEGs, lepton takes 20.3% off (39 of
  39). pdfs max **5,064,071** vs 7z -mx9's 6,606,978 — **−23.4%**.
  · Id 34's framing cannot say what a stream IS, so it only carries deflate;
    id 37 adds a kind byte and `NDf2` magic and the decoder reads both, so 34 is
    decode-only. A `/DCTDecode` stream IS the JPEG: no zlib wrapper, starts SOI.
  · THE TRAP (found via id 34, deflate-only, before id 37 added JPEG): PDF's
    `/FlateDecode` is RFC 1950, so a stream carries two zlib header bytes and
    a four-byte adler32 that are NOT deflate. Handed those, preflate modelled
    **0 of 957** streams while the scanner reported 72% coverage. Strip 2 + 4.
  · The scan is LEXICAL; "PDF needs a full object parse" deferred it and was
    WRONG. Rules: `stream` must follow `>>`; the FIRST name after `/Filter`
    decides the kind; `/Length` needs whitespace or `/Length1` is read as the
    length; an indirect `/Length 9 0 R` falls back to `endstream`, a MISSING
    one stops the scan. EVERY per-candidate backward scan needs a FLOOR at the
    previous candidate's end, not just a window: `%PDF-` + `>>stream` repeated
    matched neither `obj` nor `/Filter`, so both sweeps ran in full — 47 s for
    16 MiB vs 45 ms of real PDF. Probe: `probe-preflate --bin pdfscan|lepton`.
- WAV → FLAC SHIPPED as filter id 38 (`crate::wav`, flacenc + claxon, pure Rust
  Apache-2.0). PUBLIC corpus (`test/audio-web.json`, 372,452,770 B of PCM from
  Commons): max **202,220,333** / 25.0 s, normal **196,969,329** / 12.4 s, vs
  zpaqfranz -m5 249,323,326 / 280 s and 7z -mx9 255,906,731 / 138 s — max is
  −18.9% on the best of the rest, and NORMAL BEATS MAX by 2.6% because over a
  FLAC stream the codec decides almost nothing. On the FLAC control nobody
  compresses: all four within 0.5%.
  · ID 38 PINS THE DECODER, NOT THE ENCODER, unlike 34/35/37: the payload is a
    standard FLAC stream, so a better encoder never spends an id. It pins the
    WRAPPER — the whole file with only the `data` payload cut out, spliced back
    on decode. That, not FLAC, is what makes the round trip exact.
  · A .wav past the solo cap is CUT: `Packer::add_wav_split` emits unit-sized
    runs of whole frames, header with the first piece, trailing chunks with
    the last. Worth 276,221,291 → 265,279,793 at max when it landed.
  · Middle pieces are bare PCM with no `fmt `, so the format travels in
    `Job.wav` (the record always carried it, so decoding is unchanged). An
    `Extent`'s offset is inside the UNIT, not the file. A .wav that cannot be
    split falls back to the GENERIC path, NOT the precompressed one.
- Installed-program corpus (test/firefox, 341.7 MiB, 68.6% .dll): 7z -mx9
  87,566,439 B vs nova 95,721,462 → **+9.3%, our weakest case**. Findings:
  · The codec+filter vote must be cast per CHUNK (`Packer::current` + `place`):
    cast once at the file's start it put 150 MB of a 176 MB xul.dll in units
    with no BCJ, worth 506,834 B. The gap is ALL x86: .dll +11.8%, omni.ja
    +0.1%, everything else −6.2%.
  · SOLIDITY IS NOT THE CAUSE (234.3 MiB DLL set, LZMA2 -9e): geometry costs
    1.3%, BCJ 2.8%, and 7-Zip still sits 8.7% BELOW solid-with-BCJ. It is the
    FILTER, not the size.
  · 7-Zip's advantage is BCJ2: 4-byte call/jump targets go to separate streams
    so high-entropy addresses stop interrupting the code. `lzma-rust2` ships a
    BCJ2 DECODER only, so we wrote the transform — id 36. Firefox → **−2.4%**,
    source → −2.1%; gap to 7-Zip: Firefox +9.3% → **+6.7%**, source → +1.9%.
  · THE RULE, and three that measured worse: take a site when the absolute
    target lands INSIDE the buffer (68,893,556 B) — not no test (68,994,337),
    not liblzma's top-byte test (72,488,469, WORSE than not splitting: addresses
    land in 0..64M so the top byte is 0x01-0x03), not displacement magnitude
    (69.5-71.1 MB). The WIDTH of the accepted range is what matters, bounded by
    the unit size (128 MiB −1.1%, 256 MiB −4.0%, still 2.8% above 7-Zip).
  · Concatenating the split's three streams costs NOTHING, but the site COUNT
    in the header is load-bearing: the decoder walks a stream the displacement
    bytes are gone from, so at the tail it finds one extra opcode.
- MP3 PLANE SPLIT SHIPPED as filter id 39 (`crate::mp3`, pure Rust, nothing
  pinned — the MPEG bytes are only REORDERED). Headers, CRCs, side info and
  spectral data go to four planes, headers/side COLUMN-MAJOR; anything not a
  frame (ID3v2/v1, Xing, garbage) is copied verbatim in place. PUBLIC corpus
  `test/mp3-pub` (56,392,859 B, 13 files, `test/mp3-web.json`): max
  **51,817,332 (91.89%)** · zpaqfranz -m5 51,660,179 · kanzi -l9 52,326,253 ·
  7z -mx9 52,922,255 — −2.1% on 7z, but **zpaq beats us by 0.30%**. Round trip
  byte-exact on all 13; fast is 93.59% in 0.5 s, already under 7z's 93.85%.
  · THE SPLIT ITSELF IS ONLY WORTH 0.47% (`mp3::corpus`, --ignored): bsc ALONE
    takes an MP3 to 93.25%, BWT sorting frame-periodic structure without help.
  · WHERE THE BYTES STILL ARE, and it is NOT the Huffman layer: header plane →
    0.1-5.7% (done), spectral → 85-99% (finished), SIDE INFO → **62-80%**
    (2,925,000 → 2,082,892). Byte columns MIX BIT-PACKED FIELDS
    (`main_data_begin` is 9 bits, so column 1 holds its LSB plus scfsi); a
    per-field transposition is worth up to ~1.6% of the corpus. Plans 2.
  · NEEDS NO SOLO UNIT unlike wav/zip/jpeg: every frame carries its own length,
    so any slice splits alone. Detection traps, all found by review: "ID3" plus
    7 plausible bytes is NOT a tag (check the first FRAME's id AND size-fits-tag
    AND reserved flag bits — 'Z' repeated passes "four upper-case letters"); the
    tag evidence must be UNCONDITIONAL or a longer head flips yes to no; and
    `chains` must compare `side_len` too, or it declares runs the loop then cuts.
- HOW IT IS WIRED — FORMAT RULES. `ChunkRec.filtered` (0 = same as unpacked) is
  the length the CODEC produced; `unpacked` keeps its one meaning forever, the
  ORIGINAL length that `hash` covers and `Extent` indexes. `lzma2_dict_size` and
  `ppmd7_mem_size` must derive from the CODED length on both sides or every
  existing archive stops decoding. Ids 34/35/37 pin library versions; an upgrade
  spends a NEW id. A container-scanning filter needs a unit of ITS OWN
  (>= 64 KiB, <= 2x unit); id 39 is the exception, it needs no such thing.
- SAFETY, enforced not intended: the packer round-trips every rebuilt unit and
  falls back on any mismatch; a filter that refuses is a fallback; the
  transformed form may not exceed `MAX_CODED_CHUNK` (256 MiB).
  · THE BOUND BELONGS IN `compress_job`, beside `filtered = data.len()`: only
    that line knows the number reaching the manifest, and `verify_chunk`
    REFUSES a coded length above the cap — an unchecked one is an archive nova
    writes and cannot extract. Per-filter budgets do not substitute
    (`deflate_encode` charged plaintexts, not container bytes).
  · NEVER SIZE A Vec FROM A COUNT A PAYLOAD CLAIMS, and NEVER RETAIN A PARSED
    STRUCT PER PAYLOAD ITEM. `mp3::decode` broke both: `n<=data.len()` was 5x
    too loose (record 5 B, entry 16 B → 4 GiB), and keeping a 24-byte `Header`
    per 4 bytes of header plane was a 6x fill no later bounds check could
    precede (1.6 GiB at the 256 MiB cap). Rust ABORTS on allocation failure and
    `verify_chunk` runs `unapply` BEFORE the blake3 gate, so the archive picks
    the crash. Fix: bound by RECORD SIZE, grow as records parse, and re-read the
    plane on a second pass instead of remembering it — 1.6 GiB → 168 B, flat.
    Both found by adversarial review, neither by tests.
- TRAPS that would each have shipped a SILENT mis-decode; the next
  length-changing filter meets every one again. The store fallback must compare
  against the ORIGINAL length and reset the coded length, not just the filter
  byte · an in-place filter MUST be unapplied on that fallback, a rebuilt one
  MUST NOT (hence `Applied`) · widening the delta range for a new id makes it
  decode as `Delta(id-1)` · the deflate class cannot be decided from the head,
  a zip's central directory is at the END · PPMd7's pool saturates at both ends,
  so a length bug passes on big and tiny units and fails only in a band.
- COMPAT FIXTURES: `tests/fixtures/legacy-{max,normal,ppmd}.nva` are real
  pre-recompression archives, extracted in full by one test — any change to the
  derived LZMA2 window or PPMd7 pool is caught there and NOWHERE else. Their
  magic was re-signed in place when the format magic changed (header plus BOTH
  footers — the self-hash covers it); payload untouched, which is the point.

## Negative knowledge

- "0 B deduplicated" at max on a library with duplicates is NOT a bug. Files
  under `unit/2` share a unit, and a shared unit's chunks do not dedup — the
  codec's own matching covers it and lands SMALLER. Measured on two identical
  MP3s plus one other: fast dedups 5.6 MiB → 10,572,385 B, max dedups 0 →
  10,438,615 B. Reproduced on non-MP3 data of the same shape, so it is a
  property of unit sharing, not of any filter.
- Ordering files by content similarity instead of by name — MEASURED −0.07% on
  33 Windows DLLs. One extension's files are already adjacent and a unit is
  mostly one or two large files, so which share a unit barely moves anything.
- Per-unit LZMA lc/lp/pb — MEASURED AND REJECTED. Worth 1.2% on the x86 target
  stream alone, but on whole units the best of six parameter sets beats the
  default by 0.01-0.16%, and that stream cannot get its own without a split.
- A stronger FLAC encoder — MEASURED −0.94% on 268 MB of real music (flacenc,
  LPC order 24 vs `flac -8`'s 12, exhaustive Rice, round-trip verified). Real
  files already sit at the format's limit and every archiver stores FLAC, so
  what is left needs the RESIDUALS re-coded lepton-style, not a better encoder.
  THE CEILING IS ~5%: paq8px -8 takes one 14,394,284 B .wav to 6,613,802
  against our 6,952,563 — **−4.9% in 11m47s**, ~700x past the budget. SOLVED.
- Raising the packer's solo-unit cap on its own — it is the READER's bound in
  disguise. `read_packed` refuses a chunk above `MAX_STORED_CHUNK` (2x
  MAX_CHUNK = 64 MiB) and `unit * 2` merely equalled it. At `unit * 4` a real
  WAV corpus packed, LISTED, and failed on extract with "corrupt manifest:
  implausible chunk size". The cap now reads `min(unit * 2, MAX_STORED_CHUNK)`
  and `Packer::flush` asserts the bound before anything is written.
- Byte-plane split for 16-bit data — research 14 §7.2's top "ship-now"
  (-10..-11% on x-ray and mr vs LZMA2). DEAD, killed twice: bsc ALONE beats the
  proposed filter (x-ray 3,757,028 vs 3,999,186; mr 2,206,462 vs 2,473,757), and
  the split BEFORE bsc makes it WORSE (x-ray +5.9%, mr +4.8%). Structural: BWT
  sorts by following context and handles interleaving natively, while splitting
  planes severs a sample's high byte from its low.
- PPMd var.I (`ppmd-rust`'s `Ppmd8Encoder`) — within 0.15% of var.H on real
  source text, WORSE above 4 MiB, plus a live segfault at 16 MiB.
- Firefox's `omni.ja` is NOT recompressible: it parses as a zip but every entry
  is method 0, stored, so the browser can mmap it at startup — no deflate to
  undo, which is why nova and 7-Zip land within 0.1% of each other.
- Shared/trained dictionaries at creation time — MEASURED NET LOSS at 32 MiB
  units (100.0-100.8%): a dictionary SUBSTITUTES for solidity. Only the append
  path is worth revisiting, −21% to −31% there.
- Alphabet/numeral-system transforms — folklore. MEASURED: frequency remapping
  0.0%, RLE ~1%, MTF +202%, sparse packing 0.4%. Only base64-undo is real. Byte
  transposition on float data (sao) is +25 to +29%, worse than raw.
- "The record-width filter is only for data that would otherwise be stored raw"
  — WRONG, it cost 27% on PCM. See Compression design: the guard is `pays_off`.
- Flushing a unit on every class change — shatters a source tree into 246 units
  (median 1.4 KiB, +1.8 MiB); only files >= 4 KiB may trigger it.
- Letting a class change slide until the unit is half the target — TRIED AND
  REVERTED: a 171 KB win in a fixed-codec counterfactual cost +1,002,157 B at
  max, +598,383 B on Silesia in the real packer. THE LESSON: one LZMA2 chain
  cannot see what a mixed unit gives up — one codec, one filter, per unit.
- Creating RAR is legally impossible (RARLAB licence): extract only, behind the
  OFF-BY-DEFAULT `rar` feature, so the default build links no RARLAB code and
  nova's licence stays an open choice. `sniff` is OUTSIDE the gate (magic bytes
  need none of it) so that build says "is a rar, not built for it" instead of
  "not a NOVA archive". `tests/fixtures/sample.rar` is checked in — nothing
  here can regenerate it.
- `PROCESS_MODE_BACKGROUND_BEGIN` — Very Low I/O/memory priority (~250x
  slowdowns). IDLE/EcoQoS default — starved by daemons. IoPriorityHintVeryLow
  default — 1-3% of disk. Job-object working-set caps/CPU-rate control —
  paging churn. WinRAR sleep injection, rayon/par_bridge — cores wasted, no backpressure.
- libzstd internal MT (`ZSTD_c_nbWorkers`) — breaks memory estimation and
  duplicates chunk parallelism; zstd `--long` — pointless under CDC chunks.
- LZMA2 as the universal max-tier codec — on 4 MiB chunks only ±2% vs zstd-19
  on text, its edge coming from >4 MiB dictionaries chunking removes. Stays
  for binary/generic data; text goes to PPMd7 (−24%).
- Capping zstd WindowLog for 4 MiB chunks — no effect, already shrinks to
  the source size. Parallel EXTRACTION with zstd is SLOWER too (NTFS
  metadata contention): 5751 files, 1.0 s on 1 thread vs 2.5 s on 8; default
  extract workers = 1 for zstd/store, `-j` still honoured.
- GPU for blake3/dedup — slower than CPU SIMD, PCIe erases gains. GPU high-ratio
  compression (LZMA-class) does not exist in 2026: codecs land near zstd-1..3,
  candidates (dietgpu, Brotli-G, CULZSS) are dead, nvCOMP is proprietary.

## Owner decisions

- Language: Rust PREFERRED, not required. The GUI must be Rust and as much else
  as practical, but a codec may be C/C++ behind FFI if Rust has no usable
  implementation — do NOT reject an algorithm for want of a Rust crate. Repo
  `confeden/Nova-Prism`, no LICENSE yet (owner will choose). Zero telemetry, ads
  or analytics, ever. `.gitignore` is NOT tracked — the rules live in
  `.git/info/exclude` (owner's decision); do not re-add the file.
- BENCHMARK SET is plural, not just 7-Zip: 7z -mx9, xz -9e, brotli -q11, kanzi
  -l7/-l9, zpaqfranz -m4/-m5, binaries in `D:/Programs/compressors` (WinRAR
  too — the only thing that can make a .rar fixture). Corpora must be STANDARD
  so our sizes sit beside published ones. STANDING RULE (owner): prefer data
  anyone can fetch by link, for benches AND development. A private corpus is a
  last resort and must be labelled one. Harness: `test/bench-std.sh`.
- Industry adoption: installers call the max tier **`nova`** — DECIDED, PRISM
  dropped (NSA collision, live Prism/Prisma marks); `nova-sfx`/`nova-dec`.
  Detail in `docs/research/17-installer-integration.md`; both old blockers are
  gone. DECODER SIZE MEASURED (`test/size-probe.sh`): core 251.5 KiB / media
  656.5 / max 982.0, whole engine WITH every encoder 1.13 MiB — 4.80 MiB was
  clap. LZMA2 decode is 14.5 KiB and PPMd7 13.0; what costs is lepton 228,
  preflate 162.5, bsc 161, zstd 158 and OUR OWN blake3+rmp manifest at 123.5.
  SFX needs NO format change (reader takes `base`, hashes `at - base`) but the
  embedding defence holds ONLY while base comes from the CALLER, never the
  file; find the payload via PE headers, an EOF marker dies under Authenticode.
- Speed budget: not much slower than 7z -mx9. DISQUALIFIES cmix, paq8px, nncp
  and every LLM compressor by construction; they stay a CEILING reference.
- SCALING is an owner requirement at BOTH ends: work well on ONE core, use 8+
  efficiently, threads must not cost ratio. The unit size comes from the
  geometry, not the thread count, so output must be BYTE-IDENTICAL at -j 1 and
  -j 8. Measured (Silesia, `test/scaling.sh`, before bsc): nova 170.20 → 45.85 s
  = **x3.71**, identical at every count · kanzi -l9 x3.90, identical · 7z -mx9
  x1.79 and its output GROWS 1,407 B past one thread · xz x1.04. THE WEAKNESS IS
  ONE CORE: 170 s vs 7z's 72.5 s, the tournament compressing every unit four
  times — do NOT drop PPMd7 o16 to fix it, it takes 4 Firefox and 5 PDF units.
- MEMORY LOCALITY is an owner direction and the same question as scaling: a
  max-tier worker's LZMA2 table (~370 MB) or PPMd7 pool (256 MB) is 10-20x an
  8-core L3, so max packing is memory-bandwidth-bound BY CONSTRUCTION.
- Chat with owner in Russian; code, docs/ and CLI in English. EXCEPTIONS, both
  owner-set: README.md and CHANGELOG.md are RUSSIAN, and the GUI ships RU.
- CHANGELOG.md: `# История изменений`, `## [X.Y.Z]` newest first, flat bullets
  leading with a bold user-facing sentence. NO DATES — the owner asked for them
  out. Version numbering follows this file's milestones (0.7.0 = recompression);
  Cargo.toml + tauri.conf.json must match.
- THE differentiator, in this order: recompressing what is already compressed
  FIRST (JPEG/PDF/zip/MP3 — where other archivers have nothing), then per-file
  method choice, then editing without repack. Two-phase compression and cheap
  edits remain core requirements.

## Open issues

- Not preserved: empty dirs, symlinks, NTFS attrs/ADS, ACLs. Manifest in RAM;
  long Windows paths untested. Solid-block members are read whole into RAM at
  pack time. No trained dictionaries; a container past the solo cap is
  recompressed only if audio (Plans 1). Outer extraction is per FILE;
  `--eco`/`--full`/EcoQoS built, not measured on load.
- Progress granularity at max is bounded by the UNIT COUNT, not fixable in the
  reporter: ~6 units compress in PARALLEL, so between completions there is
  nothing finished to report (~38 s of a ~70 s pack). The UI answers with
  elapsed time, a block counter and a pulse.

## Plans

Shipped work is in CHANGELOG.md, format ids in `docs/format.md`. Names: magic
NOVA/NOVAEND1, extension `.nva`, binary `nova`, GUI `nova-prism`. Standing
direction (owner): beat 7-Zip where it has NOTHING, not out-tune LZMA.
Remaining, in measured order of value:

1. FIXED the multi-stream half (per-stream cap, above). The single-stream
   half is UNSOLVED: WAV-style splitting does not generalise — preflate-rs
   only returns reconstruction parameters for its FIRST chunk, so one lone
   stream (`binutils-2.42.tar.gz`) cannot be cut. Left: a format change.
2. MP3: filter 39 is in, packMP3 is NOT the plan (own code, pure Rust, no LGPL
   question). NEXT IS **1b, NOT 2**, and the measurement says so: transpose the
   side info PER FIELD instead of per byte column (up to ~1.6%, and it is the
   same parse stage 2 needs). Only then the Huffman layer.
3. CORRECTED (research 14): the 51%-of-paq8px executable headroom is
   ALREADY SHIPPED at the filter level — BCJ2 (id 36) is research 04 §5's
   full recommendation, dispack explicitly rejected there. What remains
   needs a CM codec: research 14 §10's lpaq/TPAQ tier, 41.5-43 MiB on
   Silesia (today ~49.3), 2x slower, 1-3 GiB/worker. Owner call first.
4. DONE: zip+7z+rar READ, zip WRITE, folder tree, RU, `.nva` association +
   icon + EN/RU installer (Gotchas). `create x.zip` is deflate-only on purpose
   (interop; ratio is `.nva`'s job) and lands 0.9% above `7z -tzip -mx9` at
   max — miniz_oxide's deflate, not a bug. LEFT: Explorer THUMBNAILS, a COM
   handler (research 06/07); later GPU (08), encryption, ports.

NOT on this list, deliberately: bigger units for solidity on executables, and
BCJ2-style per-site probability. PRICED and rejected — see Recompression.
