# Nova Prism — project state

Rust archiver for Windows (CLI `nova`, GUI `nova-prism`, format `.nva`). It beats
7-Zip by taking already-compressed data APART — JPEG, PDF, zip, MP3, WAV — rather
than out-tuning LZMA. Pre-1.0; format may still change.

## Status
| ID | Component | State | Evidence |
|---|---|---|---|
| S1 | Container: append-only log, dedup, crash recovery, compact | ok | 138 tests, clippy clean |
| S2 | Compression v2: 4-codec per-unit tournament, filters, solid blocks | ok | `test/bench-std.sh` |
| S11 | Multi-core wall clock vs the field (D12's metric) | **broken on general data** | kanzi -l9 is smaller AND 2.4-6x faster; `test/mt-bench.sh` |
| S12 | Extraction wall clock | ok | Silesia 1.9 s, enwik8 3.0 s — 5-80x the CM field, level with 7-Zip |
| S3 | Recompression filters 34-40 (deflate/JPEG/PDF/WAV/MP3/chunked) | ok | corpora below, all byte-exact |
| S4 | Foreign formats: zip+7z+rar read, zip write | ok | `basic.rs` round-trips |
| S5 | GUI (Tauri 2), RU, folder tree, drag&drop, `.nva` association | ok | run and used |
| S6 | EN/RU NSIS installer | ok | built, installs |
| S7 | Damaged-archive refusal (`Archive::damage`) | ok | regression test + manual repro |
| S8 | `--eco` / `--full` / EcoQoS | unverified | built, never measured under load |
| S9 | `nova test` verb; extract skipping a bad chunk | planned | 7z/rar/zpaqfranz all have `t` |
| S10 | Empty dirs, symlinks, NTFS attrs/ADS, ACLs, sub-second mtime | broken | not preserved, nothing warns |

## Map
Paths are repo-relative. `file:line` where the file is over ~800 lines.

| ID | To touch... | Go to |
|---|---|---|
| M1 | open, backward footer scan, `Damage` (I3, G1) | `crates/nova-core/src/archive.rs:482` |
| M2 | commit barrier, manifest tail write (I1) | `crates/nova-core/src/archive.rs:575` · `:1547` |
| M3 | extract, decode lanes, `UnitCache` grouping | `crates/nova-core/src/archive.rs:743` · `:1262` · `:1354` |
| M4 | `compact` (verify → replace, consumes `self`) | `crates/nova-core/src/archive.rs:1171` |
| M5 | progress contract: `Phase`, `Progress`, `Reporter` | `crates/nova-core/src/archive.rs:138-390` |
| M6 | chunk read + `verify_chunk`, `MAX_STORED_CHUNK` bound (N4) | `crates/nova-core/src/archive.rs:1492-1546` |
| M7 | filter id table, `apply`/`unapply`, `Applied` (I5) | `crates/nova-core/src/filters.rs:108-300` |
| M8 | filter 40 chunked container, preflate pass budget (G6, G7) | `crates/nova-core/src/filters.rs:350-570` |
| M9 | BCJ x86 + delta (I9) | `crates/nova-core/src/filters.rs:614-749` |
| M10 | x86 split, filter id 36 | `crates/nova-core/src/filters.rs:788-926` |
| M11 | deflate stream scanner: zip / gzip / png / pdf (G4, G5) | `crates/nova-core/src/deflate.rs:70-460` |
| M12 | `NDf*` container framing and varints | `crates/nova-core/src/deflate.rs:559-606` |
| M13 | MP3 planes: segment, encode, decode (P4) | `crates/nova-core/src/mp3.rs:256` · `:427` · `:519` |
| M14 | WAV/FLAC split and rebuild | `crates/nova-core/src/wav.rs` |
| M15 | tier geometry: `unit`, `chunked_from`, `cdc`, `worker_memory` | `crates/nova-core/src/analyze.rs:20-164` |
| M16 | `plan()`, `classify`, `pays_off`, trial sampling | `crates/nova-core/src/analyze.rs:211-590` |
| M17 | codec ids, LZMA2 dict, PPMd7 order/pool (I6) | `crates/nova-core/src/codec.rs` |
| M18 | unit formation, solo units, byte-majority `unit_plan` (I10) | `crates/nova-core/src/pack.rs:462` · `:600` |
| M19 | worker pool, `Budget` backpressure, `PackOptions::resolve` | `crates/nova-core/src/pipeline.rs:74` · `:196` · `:331` |
| M20 | manifest encode/decode, `Geometry`, `ChunkRec` | `crates/nova-core/src/manifest.rs` |
| M21 | header/footer magic, self-hash, version tuple | `crates/nova-core/src/footer.rs` |
| M22 | `sanitize` + `walk_inputs` — the single input gate (G12) | `crates/nova-core/src/paths.rs` |
| M23 | OS priority, memory budget, byte-range lock (G3), replace | `crates/nova-platform/src/lib.rs` |
| M24 | foreign zip / 7z / rar read, zip write | `crates/nova-core/src/foreign_{zip,7z,rar}.rs` |
| M25 | CLI verbs and flags | `crates/nova-cli/src/main.rs` |
| M26 | GUI commands · frontend · bundle config (G10, G11) | `crates/nova-gui/src/main.rs` · `ui/src/main.ts` · `crates/nova-gui/tauri.conf.json` |
| M27 | integration tests, compat fixtures | `crates/nova-core/tests/basic.rs` |
| M28 | on-disk spec for a third party | `docs/format.md` |
| M29 | benches and corpus recipes | `test/bench-std.sh`, `test/scaling.sh`, `test/mp3-bench.sh`, `test/fetch-*.py` |
| M30 | what already SHIPPED, per release — the record of done work | `CHANGELOG.md` (RU, D7/D8) |

## Invariants
| ID | Rule | Why |
|---|---|---|
| I1 | Commit = manifest → fsync → footer → fsync | without the barrier a valid footer can point at a torn manifest |
| I2 | Committed bytes are never rewritten except by `compact` | the whole cheap-edit design rests on it |
| I3 | A footer that verified but whose manifest will not decode is DAMAGE, not a crash — never truncate, refuse to open rw | a crash leaves no valid footer in the tail; confusing the two erased a 12 MB archive |
| I4 | Chunk hash covers the ORIGINAL bytes, before any filter | makes dedup and integrity filter-independent, and proves the filter round-tripped |
| I5 | Filter ids are permanent; a new one never widens the delta range | `2..=MAX_DELTA_ID` would decode 34 as `Delta(33)`, silently |
| I6 | Derived sizes (LZMA2 dict, PPMd7 pool) come from the CODED length on both sides | otherwise every existing archive stops decoding; see `docs/format.md` |
| I7 | Never size a Vec from a count a payload claims; bound by RECORD size and grow as records parse | two 16x/6x amplifications reached 4 GiB and 1.6 GiB, and Rust aborts on alloc failure |
| I8 | Output byte-identical at `-j 1` and `-j 8` | owner requirement; `test/scaling.sh` |
| I9 | BCJ always runs at start_offset 0 | position-dependent output breaks dedup |
| I10 | One unit = one codec + one filter, chosen by BYTE-MAJORITY vote per chunk | a head sample let one 4 KiB `.flac` store 8.36 MB raw |
| I11 | Extraction memory stays bounded by a few MAX_CHUNK buffers | weak-PC extraction is a hard requirement (packing is NOT bounded — G8) |

## Gotchas
- **G1** `list` says "0 file(s)" exit 0 on a real archive → one flipped manifest bit → damage detection, see `kb/format.md`.
- **G2** Windows: `File::try_clone` SHARES the file position → extraction workers must each `File::open`.
- **G3** Whole-file `try_lock` makes our OWN threads fail ERROR_LOCK_VIOLATION → lock one byte at 0xFFFF_FFFF_FFFF_0000.
- **G4** preflate models 0 of 957 PDF streams while the scanner reports 72% → `/FlateDecode` is RFC 1950 → strip 2 header + 4 adler bytes.
- **G5** A PDF scan takes 47 s for 16 MiB → per-candidate backward scans with no FLOOR at the previous candidate's end.
- **G6** A deflate walk stops at 4.7% of a stream → per-pass plaintext limit fell mid-block → it must exceed the largest deflate block (32 MiB); see `kb/recompression.md`.
- **G7** A feature is built, tests pass, corpus improves by 7 KB → the pass budget stopped AFTER crossing, so the cap check discarded the whole stream.
- **G8** `--memory 512M` packs at a 1.1 GiB peak → a rebuilding filter's working set is charged to nothing → fix is to give `apply` the budget.
- **G9** `"0 B deduplicated"` at max on a library with duplicates → files under `unit/2` share a unit and shared units do not dedup → not a bug, the codec covers it and lands smaller.
- **G10** GUI ships in dev mode / a plain `cargo build` overwrites a good exe → `devUrl` in tauri.conf.json → there must be none; build `ui/dist` first.
- **G11** Installer dies with LGHT0311 after the whole release build → Cyrillic in `fileAssociations.description` under WiX cp1252 → NSIS only, that string stays ASCII.
- **G12** A zip entry path uses `\` (PowerShell `Compress-Archive`) → normalise BEFORE `paths::sanitize`, never inside it.
- **G13** argv[1] truncated → an unquoted path with spaces; `.cmd` launchers mangle Cyrillic → use a BOM'd `.ps1`.
- **G14** A bench records zpaqfranz's decode time as a refusal → `zpaqfranz x … -to DIR` needs a TRAILING SLASH.
- **G15** `create x.zip` lands 0.9% above `7z -tzip -mx9` → miniz_oxide's deflate encoder, on a writer that is deflate-only for interop → NOT a bug, do not re-measure; `kb/platform.md#g15`.
- Full reproductions and the numbers: `kb/format.md`, `kb/recompression.md`, `kb/platform.md`; G9 is in `kb/negative.md`.

## Negative knowledge
- **N1** Ordering files by content similarity, not name → −0.07% on 33 DLLs; extensions already cluster them.
- **N2** Per-unit LZMA lc/lp/pb → 0.01-0.16% on whole units; does not generalise.
- **N3** A stronger FLAC encoder → −0.94%; the ceiling is paq8px's −4.9% at ~700x the budget. SOLVED, drop it.
- **N4** Raising the packer's solo cap alone → it is the READER's bound in disguise; archives packed and then failed to extract.
- **N5** Byte-plane split for 16-bit data → bsc alone beats the filter, and splitting BEFORE bsc is 4.8-5.9% worse.
- **N6** PPMd var.I → within 0.15% of var.H, worse above 4 MiB, segfaults at 16 MiB.
- **N7** Firefox `omni.ja` is not recompressible → every entry is method 0 so the browser can mmap it.
- **N8** Shared/trained dictionaries at creation → net loss at 32 MiB units; a dictionary SUBSTITUTES for solidity. Only the append path is worth revisiting.
- **N9** Alphabet/numeral transforms → remapping 0.0%, RLE ~1%, MTF +202%. Only base64-undo is real.
- **N10** "The record-width filter is only for data that would otherwise be stored" → wrong, it cost 27% on PCM.
- **N11** Flushing a unit on every class change → 246 units of median 1.4 KiB; only files ≥ 4 KiB may trigger it.
- **N12** Letting a class change slide to half a unit → +1.0 MB at max; one codec chain cannot see what a mixed unit gives up.
- **N13** Creating RAR → legally impossible (RARLAB licence); extract only, behind `--features rar`.
- **N14** `PROCESS_MODE_BACKGROUND_BEGIN`, IDLE/EcoQoS by default, job-object working-set caps → ~250x slowdowns, starvation, paging churn.
- **N15** libzstd internal MT, zstd `--long` → breaks memory estimation, pointless under CDC.
- **N16** LZMA2 as the universal max codec → ±2% vs zstd-19 on text; text goes to PPMd7/bsc.
- **N17** Capping zstd WindowLog; parallel EXTRACTION with zstd → no effect; NTFS metadata contention makes 8 threads 2.5x SLOWER than 1.
- **N18** GPU for hashing or high-ratio compression → PCIe erases gains; LZMA-class GPU codecs do not exist and nvCOMP would make `.nva` NVIDIA-only.
- **N19** Bigger units for solidity on executables, BCJ2-style per-site probability → priced and rejected.
- **N20** packMP3 for the MP3 filter (research 02 §MP3, ~16%) → rejected on LICENCE, not on ratio: LGPL-3.0 pre-empts the licence choice D11 leaves open. Filter 39 is our own pure-Rust plane split for exactly that reason.
- **N21** A CM codec (research 14 §10's lpaq/TPAQ) for what BCJ2 leaves on executables → ~2x slower and 1-3 GiB per worker; speed AND memory budgets disqualify the WHOLE CM class, not just D4's cmix/paq8px/nncp end-points.
- **N22** Re-opening research 04 §5, or research 14's "51% of paq8px" executable headroom → CORRECTED and CLOSED: BCJ2 (id 36) IS 04 §5's full recommendation and already collects that headroom at the filter level; what is left is N21's.
- **N23** bsc-m03 as a fifth entrant → −5.8% on enwik8 but DECODE 34.7 s against our 4.6 s, and it is GPL-3.0. Decode speed is defended; no.
- **N24** PPMd7 at order 32/64 as a fifth entrant → wins big per FILE (xml −7.3%, nci −20.1%) and ZERO units in the real packer: inside a 32 MiB unit it competes with bsc and LZMA2, not with itself. Silesia and the source tree came out byte-identical. Do not re-run.
- Full reasoning and every measurement: `kb/negative.md` — except N19's numbers, which sit with the BCJ2 work in `kb/recompression.md`.

## Decisions
- **D1** Rust preferred, not required — a codec may be C/C++ behind FFI; do NOT reject an algorithm for want of a Rust crate.
- **D2** Zero telemetry, ads or analytics, ever.
- **D3** `.gitignore` is NOT tracked; rules live in `.git/info/exclude`.
- **D4** Speed budget: not much slower than `7z -mx9`. Disqualifies cmix/paq8px/nncp BY CONSTRUCTION; they stay a ceiling reference. Together with the memory budget it also closes the whole mid-weight CM class — N21, not just those three names.
- **D5** Benchmark set is plural (7z, xz, brotli, kanzi, zpaqfranz) on STANDARD corpora; prefer data anyone can fetch by link. A private corpus must be labelled one.
- **D6** Scaling matters at both ends, but not equally — see D12. Threads must never cost ratio (I8).
- **D7** Chat with the owner in Russian; code, `docs/` and CLI output in English. README.md and CHANGELOG.md are RUSSIAN; the GUI ships RU.
- **D8** CHANGELOG: `## [X.Y.Z]` newest first, flat bullets, NO DATES.
- **D9** The differentiator, in order: recompress what is already compressed → per-file method choice → editing without repack.
- **D10** Installers call the max tier **`nova`**; PRISM dropped (NSA-programme collision, live Prism/Prisma marks).
- **D11** No LICENSE yet — the owner will choose one before the first release.
- **D12** MULTI-CORE IS THE BENCHMARK, single-core is not. This is a modern archiver: on a machine with cores we should be AHEAD of the competition on wall clock, and that is what to measure and optimise. One core is a weak-machine sanity check where being SLOWER is acceptable — it must work well, not win. Supersedes the old framing that read "the weakness is one core"; a candidate is no longer disqualified for costing encode CPU if that CPU parallelises, and a candidate that only saves single-core time is worth much less than it looked. It already costs real ratio: every LGPL recompressor is off the table while it is open (N20).
- Full reasoning: `kb/decisions.md`.

## Now
Weak spots are mapped per file (`test/weakspots.sh`, `kb/compression.md`): against
the LZ family we are −11.8% on Silesia per file, and the whole gap is to the
CM/BWT family. Four of the five worst files are units the tournament gave to
LZMA2, and two of them — samba and mozilla — are TARBALLS, which is P11.

Filter 40 (chunked container) just landed: an oversized deflate stream is modelled
in as many preflate passes as the budget allows instead of being skipped whole.
Public deflate corpus 74,865,900 → **60,703,266 B (−18.9% on our own previous
output; −30.0% against the best of the rest, zpaqfranz -m5 at 86,761,587)**.
Remaining on it: the last third needs passes to span units, which trades away unit
independence — price it before building (P1).

## Next
| ID | Task | Why / blocked on |
|---|---|---|
| P12 | **Cut tournament wall clock: pick entrants from the unit's class** | S11. Measured by a probe at +33 B over 630 MB for −27% tournament CPU, which under D12 is −27% wall clock. Does not close the kanzi gap alone (enwik8 83.4 s → ~61 s against its 14.0 s) but it is the cheapest move and it costs no ratio. Verify the +33 B claim in the real packer first — N24 is the warning about probes that do not survive it |
| P11 | **Split tar members before unit formation** | MEASURED −8.6% on Silesia's mozilla, −1.1% on samba, −2.75% on Silesia overall; gap to zpaqfranz +7.95% → +4.98%. A tarball switches off BOTH differentiators — per-file method AND nested recompression — and the win comes from the second: eleven jar/deflate units became visible. Compounds with filter 40, which now recovers a 305 MiB tar and hands it over whole. `kb/compression.md#p11` |
| P1 | Decide whether deflate passes may span units | worth ~7.3 MB more on binutils; costs unit independence, which random access and decode lanes rest on |
| P2 | `nova test` + extract that skips a bad chunk instead of dying | S9; every competitor has it and `compact` already has the machinery |
| P3 | Charge a rebuilding filter's working set to `--memory` | G8; `--memory 512M` currently packs at 1.1 GiB |
| P4 | MP3 side info transposed PER FIELD, not per byte column | up to ~1.6% of the MP3 corpus; same parse the Huffman layer would need |
| P5 | Compat fixtures for codec 4 and filters 34-40, and `-j1 == -j8` inside `cargo test` | nothing catches a bitstream change to them today |
| P6 | Ratio cost at 64/128/256 MiB memory budgets | first unmeasured blocker for the installer track |
| P7 | Choose a licence (D11) | everything downstream of `docs/research/17` is blocked on it |
| P8 | `nova create --profile core\|media\|max` + manifest off zstd at core | makes the 251 KiB decode-only stub real; encoder-side only |
| P9 | Preserve empty dirs, Windows attrs, sub-second mtime | S10 — a restored tree is a different tree |
| P10 | Explorer thumbnails (COM handler), then GPU/encryption/ports | research 06/07/08 |

## KB index
| File | Holds | Read it when |
|---|---|---|
| `kb/overview.md` | what exists in detail: crates, container features, CLI verbs, GUI, test coverage, open issues | "what does this actually do already" — before proposing anything |
| `kb/format.md` | archive layout, commit barrier, damage detection, chunk records, progress contract, memory model | touching `archive.rs`, the footer, or anything about opening a broken archive |
| `kb/compression.md` | tiers, the 4-codec tournament, geometry, record-width filter, bsc wiring, resource policy | changing codec choice, unit size or the OS priority policy |
| `kb/recompression.md` | filters 34-40 in full, every trap, the corpora and their numbers, the BCJ2 measurements behind N19/N22 | touching any recompression filter or adding a new format |
| `kb/platform.md` | Windows locking, GUI build, NSIS installer, bench harness quirks | building the GUI or the installer, or a bench behaving oddly |
| `kb/negative.md` | every disproven approach with its measurement, plus G9 | about to propose something that sounds clever |
| `kb/decisions.md` | owner decisions with full reasoning | a decision seems arbitrary or worth revisiting |
| `kb/_legacy-roadmap.md` | verbatim pre-restructure file — historical, may be stale | a fact seems missing above; grep only, never read whole |
