# Research 03 — Designing an Updateable Archive Container (.narc)

**Scope:** cheap in-place updates, random access, deduplication for the Nova Arc `.narc` format.
**Date:** 2026-08-16. All liveness/version claims verified against the live web on this date.

---

## 1. Executive summary

- The append-only-log + CDC + compaction design is sound and has 13+ years of production precedent
  (zpaq since 2012, borg since 2010s, restic since 2015). Every one of those projects hit the same
  three pain points: **manifest/index write amplification, compaction crash-safety, and recovery
  when the index is lost**. The v0 sketch as described re-creates all three. Fixes are cheap if done
  now (Section 9).
- Use **FastCDC v2020** (Rust crate `fastcdc` 4.0.1, MIT, ~1.4 M downloads, actively maintained)
  as the chunker. Chunking parameters are format-critical: they must be recorded in the archive
  header, because dedup silently stops working if two sessions chunk with different parameters
  (verified zpaq lesson).
- Store chunks **grouped into compressed segments** (solid blocks of bounded size, 16–64 MiB),
  not compressed one-by-one: per-file/per-chunk compression loses ~20–30 % ratio on many small
  similar files. This is the zpaq "block", restic "pack", borg "segment" pattern.
- On Windows, an append-only file + backward scan for the last valid footer is a viable commit
  protocol, but only with `FlushFileBuffers` barriers, and the recovery scanner must expect a
  **zero-filled tail** (NTFS valid-data-length semantics), not garbage. `ReplaceFile` for
  compaction is *not documented as atomic* — its three error codes enumerate observable
  intermediate states; the recovery path must handle them.
- Rejected as the container: SQLite/sqlar (2 GiB blob hard cap, zlib-era ratio, wrong corruption
  profile), casync-style chunk directories (not a single file), Borg-style in-place compaction
  (Borg 2.0 itself abandoned it).

---

## 2. Prior art: journaling / dedup archive systems

### 2.1 zpaq — the direct ancestor of the .narc idea

zpaq (Matt Mahoney, public domain / MIT) added its **journaling format in Sept 2012 (zpaq 6.00,
spec v2.01)**. It is the closest existing thing to the planned .narc: a single archive file,
append-only updates, CDC dedup, per-update transactions, rollback.

Design facts (from the [spec](https://mattmahoney.net/dc/zpaq201.pdf) and
[zpaqdoc](https://mattmahoney.net/dc/zpaqdoc.html)):

- Four block types appended per update: **`c`** (transaction/date header with a *jump pointer* over
  the compressed data so metadata can be read without decompressing), **`d`** (packed data
  fragments, grouped by file type and compressed independently, multithreaded), **`h`** (fragment
  SHA-1 hashes + sizes), **`i`** (index delta: added/deleted files). Note: `h` and `i` are
  **deltas, not full-index rewrites** — this is why a zpaq update touches O(changed data), not
  O(archive).
- **Transacted append:** zpaq first appends a *temporary* update header, then the data and index
  blocks, and **updates the header as the last step**. An interrupted update is ignored and
  overwritten on the next run.
- **Rollback = truncate:** the archive can be reverted to any earlier state by truncating at the
  first "future" date block. Versioned reads ("extract as of update N") come for free.
- **Overhead numbers:** each fragment costs ~28 bytes in the archive and ~40 bytes of RAM;
  default average fragment size 64 KiB (`-fragment 6`, i.e. 2^6 KiB). Scales to 250 TB / 4×10^9
  files.
- **Chunking-parameter lesson (critical):** archives made with a non-default `-fragment` value
  are spec-valid and extract fine, **but adding the same files with a different value produces
  zero dedup** — boundaries differ. Chunking params are part of the format contract.
- **Self-describing decompression (ZPAQL bytecode in every block)** gave zpaq forward
  compatibility but enormous complexity; nobody else adopted it. Lesson: version the *format*,
  don't ship a VM.
- Liveness: upstream zpaq frozen at 7.15 (Aug 2016). The active fork is
  **[zpaqfranz](https://github.com/fcorbelli/zpaqfranz) v64.8, released 2026-06-29** (MIT + mixed
  bundled licenses) — actively maintained, Win/Linux/BSD. zpaqfranz replaced SHA-1 with XXH3/BLAKE3
  options and added paranoid verify modes — evidence that hash agility matters.

### 2.2 Borg 1.x → 2.0 — the compaction cautionary tale

Borg 1.x ([data structures doc](https://borgbackup.readthedocs.io/en/stable/internals/data-structures.html))
is a log-structured KV store: numbered **segment files up to 500 MB**
(`max_segment_size = 524288000`), each a series of `PUT` / `DELETE` / `COMMIT` entries; a
transaction ends with a tiny **17-byte commit tag**. Only the last entry per key matters.

- **Compaction** (`borg compact`) is threshold-based: a segment is rewritten only if ≥ 10 %
  (default `--threshold`) of it is dead — avoids rewriting 500 MB to reclaim kilobytes. Sparsity
  is tracked in a "hints" file to avoid scanning all segments. Compaction is itself transacted
  (intermediate commits per segment; old segment deleted only after refcount hits zero).
- Pain points documented in their own docs: hints-file format migrations, a 1.1 bug that littered
  repos with thousands of 17-byte commit-only segments (needed a one-time `--cleanup-commits`),
  compaction needing free space *before* freeing space, append-only mode silently doing nothing
  on `compact`.
- **Borg 2.0 (beta, 2026) deleted the whole segment+compaction machinery**: it now stores each
  chunk as an individual object in a directory store (borgstore), because "objects can be found
  directly by their ID" and "no segment files compaction is required anymore"
  ([release notes](https://www.borgbackup.org/releases/borg-2.0.html)).
  That option is unavailable to a *single-file archive* format, but the lesson stands:
  **log compaction with refcounting is the most bug-prone subsystem; keep it brutally simple**
  (offline, whole-file rewrite, threshold-gated).

### 2.3 restic — pack files, index, and prune ordering

restic ([design doc](https://github.com/restic/restic/blob/master/doc/design.rst)) stores
CDC blobs (Rabin fingerprint, 64-byte window; blobs 512 KiB–8 MiB, target 1 MiB; files < 512 KiB
not split) inside **pack files** whose *header is at the end* (written after streaming the blobs),
plus separate **index files** kept < 8 MiB each with a `supersedes` field for index rewrites.

- **Prune safety ordering (adopt verbatim for .narc compaction):**
  1. write the new pack, 2. add the updated index, 3. delete the old index, 4. only then delete
  the old pack. A pack must be unreferenced by any live index before deletion.
- The old prune (pre-2020, [issue #2547](https://github.com/restic/restic/issues/2547)) read every
  pack (sometimes twice), used O(repo) memory and was not tunable → redesigned into plan/execute
  phases with `--max-unused`, `--max-repack-size`, `--pack-size`. Lesson: **make compaction
  incremental and budgeted from day one** ("rewrite at most N MB now").
- If the index is lost, it can be **rebuilt by reading pack headers** — possible only because
  packs are self-describing. The v0 "raw chunk log" fails this test (see critique).

### 2.4 casync / desync — chunk store as a directory

casync ([announcement](https://0pointer.net/blog/casync-a-tool-for-distributing-file-system-images.html))
serializes a tree to a `.catar` stream, chunks it with a **buzhash** rolling hash (boundaries
ignore file boundaries → cross-file dedup of small files), names each compressed chunk by its
SHA512/256 in a `.castr` directory, and keeps a linear index (`.caidx`/`.caibx`).
GC is mark-and-sweep: `casync gc --store=… index1 index2` deletes unreferenced chunks.

- Liveness: systemd/casync is near-dormant (last push 2025-09-22, 68 open issues, no releases for
  years). The Go reimplementation **[desync](https://github.com/folbricht/desync)** is active
  (last push 2026-08-05, BSD-3, 409 stars) and 10× faster on warm stores.
- Relevance: the *seeding* idea (chunk any local file/dir with the same parameters and use it as a
  chunk source) is directly reusable for "update archive from changed folder" flows.
- Rejected as a container model: a directory of thousands of chunk files is not a single
  double-clickable archive; filesystem overhead per chunk is real (Borg 2.0 accepts it, an
  archiver cannot).

### 2.5 Why zip can update per-file and solid 7z cannot

| | ZIP | 7z |
|---|---|---|
| Metadata location | **Central directory at EOF**; each entry also has a redundant local file header before its data ([structure](https://users.cs.jmu.edu/buchhofp/forensics/formats/pkzip.html)) | **32-byte signature header at start** containing `NextHeaderOffset` (u64) pointing to the *end header*; end header may itself be LZMA-compressed and/or encrypted ([py7zr spec](https://py7zr.readthedocs.io/en/latest/archive_format.html)) |
| Compression unit | One file = one deflate stream | **Folder** = solid block of many files through a coder chain |
| Add a file | Append data + append rewritten central directory. O(new data + directory) | Append new folder + rewrite end header — possible, and 7-Zip does copy untouched folders, but... |
| Replace/delete one file | Append new copy, rewrite directory; old bytes remain as dead space until repack (this is exactly an append-only log with the directory as manifest) | Any file inside a solid folder ⇒ **the whole folder must be decompressed and recompressed**; with default solid block size = min(dict×128, 4 GiB) that means gigabytes of work for a 1-byte change |
| Random access | Yes (per-file streams) | Only per-folder; extracting file *N* of a solid folder decompresses files 1…N−1 |

- The dead-space-until-repack behavior of ZIP updates is the *same* model as the planned .narc
  log + compaction — ZIP just never automated the compaction.
- ZIP64 exists because the original format used 4-byte sizes/offsets ([ZIP64 notes](https://blog.yaakov.online/zip64-go-big-or-go-home/)). **Use 64-bit fields everywhere from v0.**
- The redundant local headers are what make ZIP archives salvageable by scanning when the central
  directory is destroyed — the property the raw .narc chunk log currently lacks.
- 7-Zip itself is alive (v26.02, 2026-06-25, [history](https://www.7-zip.org/history.txt));
  WinRAR 7.20 beta (Oct 2025) advertises *faster deletion from solid archives* — even the
  incumbents are being pushed toward cheap partial updates.

### 2.6 SQLite as container (sqlar) — evaluated and rejected as the primary format

[SQLAR](https://sqlite.org/sqlar.html) = an SQLite DB with one table
`sqlar(name TEXT PRIMARY KEY, mode INT, mtime INT, sz INT, data BLOB)`, zlib-compressed blobs.

Pros: single file; per-file in-place update/delete with real transactions (WAL); queryable
metadata; ~2 % larger than an equivalent ZIP; SQLite's crash-safety is the best-tested on Earth;
[appfileformat.html](https://sqlite.org/appfileformat.html) makes a strong general case.

Cons that kill it as the .narc outer container:
- **BLOB hard cap 2^31−3 bytes (≈ 2 GiB), default `SQLITE_MAX_LENGTH` 10^9**
  ([limits](https://sqlite.org/limits.html)) → every large file needs app-level chunking anyway.
- No solid compression, no dedup, zlib-only in the reference tooling → ratio loses badly to zstd
  /LZMA solid blocks; storing zstd blobs yourself means you use SQLite only as a KV log with
  ~2× write amplification (WAL writes pages twice) on bulk data.
- Rewriting a blob rewrites whole DB pages; big deletes leave free pages reclaimed only by
  `VACUUM` = full rewrite — the same compaction problem, with less control.
- A half-synced/torn SQLite file is opaque; a footer-chained log can be recovered by scanning.

**Verdict:** don't use SQLite as the archive. It *is* worth considering later as an optional
sidecar cache (dedup hash index, GUI file listing) that can be regenerated from the archive.

### 2.7 zstd seekable format — the random-access compressed-stream precedent

[Spec](https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/zstd_seekable_compression_format.md)
(still in `contrib/`, stable since 2017): the stream is split into independently-compressed frames;
a **seek table lives in a skippable frame at EOF** (skippable magic `0x184D2A5E`), so any stock
zstd decoder still decompresses the whole file. Footer = 9 bytes:
`Number_Of_Frames (4) | Seek_Table_Descriptor (1) | Seekable_Magic 0x8F92EAB1 (4)`, read backwards
from EOF. Each entry: `Compressed_Size (4) | Decompressed_Size (4) [| XXH64-low32 (4)]` → 8–12
bytes/frame; 4-byte sizes cap a frame at 4 GiB.

Takeaways for .narc:
- The "magic at the very last bytes, parse backwards" pattern is proven — same as the planned
  footer.
- Per-frame independent compression + tiny table = random access at a measurable ratio cost;
  frame size is the ratio/latency dial (same dial as solid-block size).
- Rust implementation: [`zeekstd` 0.6.2](https://crates.io/crates/zeekstd) (BSD-2, Dec 2025,
  active). Nova Arc doesn't need the exact format internally, but *emitting* seekable-zstd for
  single-file compression mode would give free interop.

---

## 3. Chunking: FastCDC and the Rust crate

**Papers:** [FastCDC, USENIX ATC 2016](https://www.usenix.org/system/files/conference/atc16/atc16-paper-xia.pdf)
and the extended [IEEE TPDS 2020 version](https://ieeexplore.ieee.org/document/9055082/).
Techniques: Gear rolling hash (shift+add per byte), zero-padded mask judgment (`fp & mask == 0`
with spread mask bits → effective 10-byte window), **sub-minimum cut-point skipping**, and
**normalized chunking** (two masks: harder mask before the target size, easier after → chunk sizes
cluster near the average, recovering the dedup lost to cut-point skipping). Results: **~10× faster
than Rabin, ~3× faster than plain Gear/AE at equal dedup ratio**; the 2020 "rolling two bytes"
variant is another 30–40 % faster. A 2024 survey ([arXiv:2409.06066](https://arxiv.org/pdf/2409.06066))
confirms normalization ≤ NC-3 costs only marginal dedup.

**Crate:** [`fastcdc` 4.0.1](https://docs.rs/fastcdc/latest/fastcdc/v2020/index.html)
(nlfiedler/fastcdc-rs, MIT, ~1.4 M downloads, last push 2026-06; use the `v2020` module).
- Constructors: `FastCDC::new(data, min, avg, max)` (Normalization Level 1 default) or
  `with_level(…, Normalization::Level0..Level3)`; parameter bounds: min 64 B–64 MiB,
  avg 256 B–256 MiB, max 1 KiB–1 GiB.
- `with_level_and_seed(…)` XORs the gear table with a seed — worth exposing per-archive to
  prevent chunk-boundary fingerprinting attacks on encrypted archives later.
- `StreamCDC` / `AsyncStreamCDC` for streaming (buffer = max_size); mmap via `memmap2` is faster
  for big files.
- Gotcha (4.0 breaking change): parameter validation became `debug_assert!` — **invalid sizes no
  longer panic in release**; validate parameters ourselves.
- Determinism: same input + same params + same seed ⇒ identical cut points, guaranteed by the
  crate. Corollary (zpaq lesson): *min/avg/max/normalization/seed must be stored in the archive
  header and reused for every subsequent update of that archive.*

**Recommended .narc defaults** (aligned with zpaq/restic practice): min 16 KiB / avg 64 KiB /
max 256 KiB, Normalization Level 1, per-archive seed 0 for v0. At 64 KiB average and ~32 B of
metadata per chunk, metadata overhead ≈ 0.05 % of stored data; RAM for the dedup index ≈ 40–48 B
per chunk ⇒ ~0.7 GB RAM per 1 TB of unique data — acceptable, and small-file archives use far
less. Files < ~2× min size should be stored as a single chunk without running the chunker
(restic skips files < 512 KiB entirely).

---

## 4. Solid grouping vs per-file chunking

Evidence ([Wikipedia/solid compression](https://en.wikipedia.org/wiki/Solid_compression),
[access-time tradeoff paper arXiv:1602.08829](https://arxiv.org/pdf/1602.08829), vendor data):

- Concatenating similar small files before LZ compression yields **20–30 % better ratio** for
  text/source; for 1000 similar XML files, solid 7z can be **60–80 % smaller than per-file ZIP**.
- Cost: extracting one file from a solid block decompresses everything before it in the block →
  block size bounds worst-case latency. 7z default solid block = min(dict×128, 4 GiB) is tuned
  for ratio, not access; RAR and tar.zst users routinely cap blocks at 16–64 MiB.
- Dedup chunking is orthogonal, and the production pattern is **two-level**: CDC chunks (dedup
  granularity) are *grouped* into compressed segments (compression granularity) — zpaq packs
  fragments into `d`-blocks grouped by file type; restic packs ~1 MiB blobs into pack files and
  compresses each blob (v2 repos, zstd); borg compresses each chunk into 500 MB segments.
- Compressing each 64 KiB chunk independently would cost real ratio (small zstd windows, no
  cross-chunk context). Mitigations, in .narc-relevant order:
  1. **Solid segments:** compress a *sequence of chunks* (grouped by file type/extension, zpaq
     style) as one zstd stream, 16–64 MiB per segment; random access decompresses at most one
     segment.
  2. **zstd dictionaries:** train a dictionary per file-type group; helps when chunks must be
     independently decompressible.
  3. zstd long mode (`--long=27+`) inside a segment recovers distant-match redundancy that
     chunk-level dedup missed.

This dovetails with the two-phase compression plan: phase 1 (analysis) assigns files to groups;
groups map 1:1 onto solid segments with a per-group codec choice.

---

## 5. Crash safety of append-only files on Windows

Verified against [FlushFileBuffers](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers),
[ReplaceFileW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew),
OSR's VDL articles ([1](https://www.osr.com/nt-insider/2015-issue1/logical-physical-file-sizes-windows/),
[2](https://www.osr.com/nt-insider/2015-issue2/maintaining-valid-data-length/)), and community
analyses ([antonymale.co.uk](https://antonymale.co.uk/windows-atomic-file-writes.html),
[rust-atomicwrites #27](https://github.com/untitaker/rust-atomicwrites/issues/27),
[danluu](https://danluu.com/file-consistency/)).

**Durability of appends**
- `WriteFile` goes to cache; **`FlushFileBuffers` is the fsync equivalent** — flushes file data
  *and* metadata and issues a disk cache flush. MS docs note it is expensive if called per-write;
  for heavy-write paths they recommend `FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH` instead.
  For an archiver, one or two flushes per *commit* is the right granularity (cost: ms-range).
- **NTFS valid-data-length (VDL) semantics:** NTFS persists VDL and journals metadata, so after a
  power loss the tail of an appended file reads as **zeros, never stale garbage** — but the
  surviving length may be *any* prefix of what you wrote (data/metadata ordering is not
  guaranteed without a flush). Documented hole: **memory-mapped writes bypass the VDL zero
  guarantee** — do not mmap-write the archive.
- Consequence for recovery: the backward scanner must treat a zero-filled tail as the common
  crash artifact, and a "valid-looking but stale" footer as possible (see §9, F-6).

**Atomic replace (needed by compaction)**
- `MoveFileEx(MOVEFILE_REPLACE_EXISTING)` atomicity is **not documented**; a Microsoft employee
  on the archived MSDN thread states MoveFile is *not always atomic*
  ([thread](https://learn.microsoft.com/en-us/archive/msdn-technet-forums/449bb49d-8acc-48dc-a46f-0760ceddbfc3)).
  Adding `MOVEFILE_WRITE_THROUGH` makes it flush before returning, not atomic.
- **`ReplaceFile` is the practical best**: preserves creation time, ACLs, object ID, named
  streams, file ID; requires all files on the same volume. But its own docs enumerate observable
  intermediate states via error codes — `ERROR_UNABLE_TO_REMOVE_REPLACED` (1175: nothing changed),
  `ERROR_UNABLE_TO_MOVE_REPLACEMENT` (1176: **replaced file may already be gone**, replacement
  still under old name), `ERROR_UNABLE_TO_MOVE_REPLACEMENT_2` (1177) — i.e. it is a multi-step
  operation, atomic only against concurrent observers in the common case, not against crashes.
  `REPLACEFILE_WRITE_THROUGH` is documented **"not supported"** → flush the temp file yourself
  first.
- `SetFileInformationByHandle(FILE_RENAME_INFO{FILE_RENAME_FLAG_POSIX_SEMANTICS})` gives
  POSIX-style rename-over on Win10 1607+ NTFS — good primary path, with `ReplaceFile` fallback.
- **TxF / `MoveFileTransacted` is deprecated** by Microsoft — do not build on it.
- There is **no directory-fsync on Windows**; NTFS's metadata journal covers the namespace change.
  On Linux (later port) the equivalent step *does* require `fsync` of the parent directory.

**Commit protocol that follows from this** (matches zpaq's transacted append and SQLite-WAL
reasoning): `append segment data + manifest delta` → `FlushFileBuffers` → `append footer` →
`FlushFileBuffers`. The first flush guarantees the footer is never durable before the data it
commits; the footer's manifest hash makes any torn state detectable; recovery truncates to the
last footer that validates transitively.

---

## 6. Container strategy comparison

| Property | ZIP | 7z (solid) | zpaq journaling | borg 1.x segments | restic packs | sqlar | **.narc v0 target** |
|---|---|---|---|---|---|---|---|
| Single file | yes | yes | yes | no (repo dir) | no (repo dir) | yes | yes |
| Add file w/o repack | yes (append+CD rewrite) | partial (new folder + header rewrite) | yes (append) | yes | yes | yes | yes |
| Replace 1 of 700 photos | append new + dead space | recompress whole solid block | append delta | append PUT | append blob+index | UPDATE blob (page rewrite) | append delta |
| Dedup | no | no | CDC (SHA-1→XXH3/BLAKE3 in zpaqfranz) | CDC buzhash | CDC Rabin | no | CDC FastCDC+BLAKE3 |
| Random access | per file | per solid block | per d-block | per chunk | per blob | per row | per segment |
| Old versions retained | until repack | no | yes (rollback by truncate) | yes | yes (snapshots) | no | yes (until compaction) |
| Space reclaim | rewrite ("repack") | rewrite | **none in-format** (must extract+recreate) | in-place segment compaction | prune/repack | VACUUM (full rewrite) | offline compaction (rewrite) |
| Salvage w/o main index | yes (local headers) | poor (single header, maybe encrypted) | good (blocks self-describing) | good (segment entries framed+CRC) | good (pack headers) | n/a | **only if log entries are framed** |
| Liveness 2026 | ubiquitous | 7-Zip 26.02 (06/2026) | zpaq dead; zpaqfranz 64.8 (06/2026) | active; 2.0 changed model | active 0.19.x | SQLite active | — |

Note zpaq's one real format hole: it has **no compaction at all** — deleted data lives forever
unless you rebuild the archive. .narc's offline compaction is the differentiator; borg/restic
supply the safety playbook for it.

---

## 7. The planned v0 design (restated)

> 16-byte header · raw chunk log · MessagePack manifest compressed with zstd · fixed 80-byte
> footer at EOF with blake3 self-check · generation counter · append-only updates · offline
> compaction.

Reading of the intent: each update appends new chunks, then a fresh full manifest, then a footer;
readers find the footer at EOF; compaction rewrites the file offline.

---

## 8. Critique — concrete pitfalls

**P-1. Full-manifest rewrite per update kills the killer feature.**
If every update serializes the *entire* MessagePack manifest, update cost is O(total files), not
O(changed files). 1 M files × ~100 B/entry ≈ 100 MB of manifest written to replace one photo —
plus that dead manifest stays in the file until compaction. zpaq ships only *deltas* (`i`/`h`
blocks); borg/restic likewise. This is the single biggest design bug.

**P-2. Raw (unframed) chunk log = unrecoverable archive when the manifest area is damaged.**
ZIP survives central-directory loss via local headers; restic rebuilds indexes from pack headers;
borg salvages via framed, checksummed segment entries. A raw byte-blob log has no such property:
one bad manifest ⇒ total loss. Framing costs ~16–32 B per record (≈ 0.05 % at 64 KiB chunks).

**P-3. Footer self-check checks only the footer.**
blake3-of-footer proves the 80 bytes are intact, not that the manifest or data they point to are.
A crash between "manifest written" and "flush completed" can leave a valid footer pointing at torn
manifest bytes unless ordering is enforced and the footer carries the manifest's hash+length.

**P-4. 16-byte header is too small and omits format-critical state.**
Needs at minimum: magic (8), format version (2), **min-reader version** (2, so old readers fail
politely on new archives), feature flags (8, reserve for encryption/solid modes now — 7z's
encrypted-header shows flags arrive later), **chunking params + seed** (zpaq lesson: wrong params
⇒ silent dedup loss), archive UUID (16, for multi-volume/sidecar binding), default hash algo ID
(hash agility — zpaq was stuck with SHA-1 for a decade). Realistic: 64 bytes.

**P-5. Torn/ambiguous footer at EOF.**
(a) An 80-byte footer can straddle a sector boundary → torn write leaves half a footer.
(b) After a crash, NTFS leaves a zero-filled tail — the scanner must skip zeros efficiently.
(c) If a crashed append happens to end exactly at an *old* footer boundary, backward scan finds a
stale-but-valid footer — harmless for consistency (it's the previous generation) but the
generation counter must be trusted over file length, and the tool should warn it recovered.

**P-6. Fixed-EOF footer vs third-party file growth.** Cloud-sync placeholders, AV quarantine,
resumable copies and `copy /b` concatenation can leave bytes after the true footer. Backward scan
must tolerate arbitrary trailing junk, bounded (e.g. scan last 1 MiB, then fail with a clear
error), and the footer needs its magic in its **last** bytes so scanning is cheap.

**P-7. MessagePack-with-zstd manifest is all-or-nothing at open time.**
Fine at 700 photos; at 10 M entries it's hundreds of MB to decompress+parse before listing one
directory. No mmap/partial access into a compressed msgpack blob. (Not a v0 blocker — but the
format should allow sharded manifests later; see F-4.)

**P-8. No persisted dedup index.**
If the hash→chunk table isn't stored, every *update* session must decompress/rescan metadata to
rebuild it. zpaq stores `h`-blocks precisely for this (28 B/fragment in-archive, 40 B RAM).

**P-9. Compaction unspecified = where the data-loss bugs will live.**
Needs the restic ordering (new file fully written+flushed → swap → delete old), temp-file-on-same-
volume discipline, `ReplaceFile` error-state handling (1175/1176/1177 each need a distinct
recovery action), 2× transient disk space, and a borg-style threshold (default ~10 % dead space)
so users don't rewrite 100 GB to reclaim 10 MB. In-place compaction: rejected for v0 outright.

**P-10. Field widths and alignment.**
Any 4-byte size/offset is a future ZIP64. All offsets/sizes u64 (or varints in framed records).
Consider 4 KiB alignment for *segment* starts only (mmap/direct-IO friendly; padding at 32 MiB
segments is negligible, whereas aligning every 64 KiB chunk would waste ~3 %).

**P-11. Concurrency.** Two processes appending to one file = interleaved corruption. v0 needs a
single-writer guarantee: open the archive with `FILE_SHARE_READ` only (no share-write) — Windows
gives this for free; add an advisory lock file on the eventual Linux port.

**P-12. Generation counter details.** 64-bit, monotonically increasing, stored in header (birth)
and every footer; tie-break duplicate generations by higher footer offset; compaction resets the
log but must *carry the generation forward* (never reuse older generation numbers, or stale
sidecar caches/indexes will validate against the wrong state).

---

## 9. Improvements — recommended v0.1 shape

**F-1. Delta manifests (fixes P-1).** Each commit appends a *manifest delta* (files
added/replaced/deleted + new chunk refs, MessagePack, zstd-compressed) and the footer points to
the newest delta; deltas chain backwards (each stores `prev_offset`). Every N deltas or when
chain length/parse cost exceeds a budget, write a full **manifest snapshot** and let the chain
start there (SQLite-WAL checkpoint / zpaq `c`-block analog). Open cost = snapshot + few deltas.
Bonus: the chain *is* version history — "extract as of generation G" and zpaq-style rollback
(truncate after footer G) come free.

**F-2. Frame every log record (fixes P-2).** Record header:
`magic u32 | type u8 (segment/manifest-delta/manifest-snapshot/chunk-index/pad) | flags u8 |
len u64 | blake3_128 of payload`. A lost-footer archive is then recoverable by forward scan, and
every record is independently integrity-checked (paranoid-verify mode = zpaqfranz selling point).

**F-3. Commit = footer with a hash chain (fixes P-3, P-5).** Footer (pad to 128 B, still "fixed
size at EOF"): `generation u64 | manifest_head_offset u64 | manifest_head_len u64 |
manifest_head_blake3_256 | total_committed_len u64 | header_uuid echo | footer_blake3 |
footer_magic (last 8 B)`. Write protocol: data+delta → `FlushFileBuffers` → footer →
`FlushFileBuffers`. Recovery: scan backward past zeros/junk (bounded), validate footer hash, then
validate manifest head hash; on failure keep scanning to the previous footer; finally truncate
the file to `total_committed_len` (zpaq's revert-by-truncate).

**F-4. Two-level data layout (fixes §4 ratio problem).** CDC chunks (16/64/256 KiB, FastCDC
v2020 L1, params in header) grouped by the phase-1 analyzer into **solid segments ≤ 32 MiB**,
each segment one zstd stream (or per-group codec later); segment record stores the chunk table
(chunk hash, uncompressed len, offset-in-segment) — which doubles as the restic-style
rebuildable index. Already-compressed files (JPEG/MP3) get store-only segments — their chunks
still dedup.

**F-5. Persisted chunk index (fixes P-8).** Periodically (with each snapshot) append a
`chunk-index` record: sorted (blake3_short → segment, offset) table. Update sessions load it in
one read instead of walking all segment headers. ~24–32 B per unique chunk.

**F-6. Compaction spec (fixes P-9).** Offline only, in v0: write `archive.narc.tmp` on the same
volume (copy live segments, drop dead ones, write fresh snapshot+footer, generation+1),
`FlushFileBuffers`, then `SetFileInformationByHandle`+POSIX rename with `ReplaceFile` fallback;
handle 1175 (retry), 1176/1177 (temp file is the good archive — finish the swap on next open).
Gate on dead-space ratio (default ≥ 10 % or ≥ 256 MB). Report reclaimable space in `list` so the
GUI can suggest it.

**F-7. Header = 64 bytes** with the P-4 field list. Keep the version/flags policy: readers must
refuse unknown *required* flags, ignore unknown *optional* ones (zstd skippable-frame spirit).

Residual risks to carry into ROADMAP: FlushFileBuffers on USB sticks/SD (removable media often
lie about cache flush — nothing an app can do; document it); mmap-write ban on the archive file;
Android (later target) has different fsync semantics (F2FS, ext4) — revisit §5 for the port;
per-archive gear seed + encryption interact (chunk-size side channel) — decide before the
encryption feature, not after.

---

## 10. Sources

- zpaq: [spec v2.01 PDF](https://mattmahoney.net/dc/zpaq201.pdf) · [zpaqdoc](https://mattmahoney.net/dc/zpaqdoc.html) · [zpaq_compression paper](https://mattmahoney.net/dc/zpaq_compression.pdf) · [zpaqfranz GitHub](https://github.com/fcorbelli/zpaqfranz) (v64.8, 2026-06-29)
- FastCDC: [ATC'16 paper](https://www.usenix.org/system/files/conference/atc16/atc16-paper-xia.pdf) · [TPDS 2020](https://ieeexplore.ieee.org/document/9055082/) · [CDC survey 2024](https://arxiv.org/pdf/2409.06066) · [fastcdc crate docs](https://docs.rs/fastcdc/latest/fastcdc/v2020/index.html) (4.0.1, MIT)
- restic: [design.rst](https://github.com/restic/restic/blob/master/doc/design.rst) · [prune redesign #2547](https://github.com/restic/restic/issues/2547)
- borg: [data structures](https://borgbackup.readthedocs.io/en/stable/internals/data-structures.html) · [compact](https://borgbackup.readthedocs.io/en/stable/usage/compact.html) · [Borg 2.0 release](https://www.borgbackup.org/releases/borg-2.0.html)
- casync/desync: [announcement](https://0pointer.net/blog/casync-a-tool-for-distributing-file-system-images.html) · [systemd/casync](https://github.com/systemd/casync) · [folbricht/desync](https://github.com/folbricht/desync)
- zstd seekable: [format spec](https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/zstd_seekable_compression_format.md) · [zeekstd](https://crates.io/crates/zeekstd)
- ZIP/7z: [PKZip structure](https://users.cs.jmu.edu/buchhofp/forensics/formats/pkzip.html) · [ZIP64](https://blog.yaakov.online/zip64-go-big-or-go-home/) · [py7zr 7z spec](https://py7zr.readthedocs.io/en/latest/archive_format.html) · [7-Zip history](https://www.7-zip.org/history.txt) · [solid compression](https://en.wikipedia.org/wiki/Solid_compression) · [access-time tradeoffs](https://arxiv.org/pdf/1602.08829)
- sqlar/SQLite: [sqlar](https://sqlite.org/sqlar.html) · [appfileformat](https://sqlite.org/appfileformat.html) · [limits](https://sqlite.org/limits.html) · [HN critique](https://news.ycombinator.com/item?id=28668615)
- Windows crash safety: [FlushFileBuffers](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers) · [ReplaceFileW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew) · [MoveFileEx atomicity thread](https://learn.microsoft.com/en-us/archive/msdn-technet-forums/449bb49d-8acc-48dc-a46f-0760ceddbfc3) · [Atomic file writes on Windows](https://antonymale.co.uk/windows-atomic-file-writes.html) · [rust-atomicwrites #27](https://github.com/untitaker/rust-atomicwrites/issues/27) · [OSR: file sizes](https://www.osr.com/nt-insider/2015-issue1/logical-physical-file-sizes-windows/) · [OSR: VDL](https://www.osr.com/nt-insider/2015-issue2/maintaining-valid-data-length/) · [danluu: file consistency](https://danluu.com/file-consistency/)
