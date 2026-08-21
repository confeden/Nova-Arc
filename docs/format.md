# NOVA format — v0.3 draft

Status: prototype, subject to change until v1.0. This document describes what
the code in `crates/nova-core` actually implements. Pre-1.0 the reader rejects
any archive with a higher **minor** version, since the format may still change
incompatibly.

## Design goals

1. **Cheap edits.** Modifying, adding or deleting entries in a multi-GB
   archive must cost O(changed data), not O(archive size). No full repack.
2. **Random access.** Extracting a single file reads only the units it lives in.
3. **Bounded memory.** No operation needs more than a couple of units at a
   time, so weak machines can always extract.
4. **Content-defined dedup.** Identical data across files (or across
   versions of a file) is stored once.
5. **Crash safety.** A torn update can never corrupt committed data.

## File layout

```
[header 16B][manifest g1][footer g1]             <- `create` (empty archive)
            [units ...][manifest g2][footer g2]  <- first add
            [units ...][manifest g3][footer g3]  <- update
```

Every update (add/replace/remove) appends new units (if any), then a fresh
manifest and footer. Earlier manifests/footers and unreferenced units become
dead space, reported as `reclaimable` and reclaimed by the offline `compact`
operation (rewrite + atomic replace).

### Header (16 bytes)

| offset | size | field |
|---|---|---|
| 0 | 4 | magic `NOVA` |
| 4 | 1 | version major (0) |
| 5 | 1 | version minor (3) |
| 6 | 2 | flags (0) |
| 8 | 8 | reserved (0) |

### Units

The unit is the compression unit and the thing the format is built around:
one independently decodable stream, holding many small files, a run of one
large file's chunks, or a single file. Its size is the dominant ratio lever —
measured on a 5739-file source tree with LZMA2 `-9e`, against one solid
stream: 1 MiB units cost +74%, 4 MiB +50%, 16 MiB +19%, 32 MiB only +4.9% —
and it is also the edit granularity, since changing a byte rewrites its unit.
Tiers therefore choose the trade: 4 MiB units at fast, 16 MiB at normal,
32 MiB at max.

Files at or above the tier's chunking threshold are first cut with FastCDC
(v2020) so that editing part of a large file rewrites only the units that part
touched. Chunks and whole small files are then accumulated into units.

Unit boundaries are content-defined: after appending an item the unit ends with
probability `len / target`, decided from that item's hash alone. A rule that
consulted the accumulated size would shift every later boundary when one file
changed length, so re-saving a tree after a one-line edit rewrote everything —
measured at 17 MiB of growth.

Three things get a unit of their own rather than sharing:

- data the analyzer finds already compressed (photos, video, archives), which
  gains nothing from neighbours and loses two things by sharing: an identical
  copy elsewhere would no longer form an identical unit and would stop
  deduplicating, and replacing one photo would rewrite everything beside it;
- files at least half a unit long;
- files whose content class differs from what the open unit holds — a unit
  carries one codec and one filter, so mixing kinds forfeits the per-file
  choice. Only files of at least 4 KiB may trigger this: smaller ones cannot
  be classified reliably.

A unit is identified by the blake3 hash of its **original, unfiltered,
uncompressed** content truncated to 128 bits, which serves both dedup and
integrity checking.

Codec ids: `0` = store (raw), `1` = zstd, `2` = LZMA2, `3` = PPMd7, `4` = bsc
(BWT + QLFC).

**Two of those carry a size a decoder must reproduce exactly, and neither is
stored.** Both are derived from the CODED length — `filtered` when it is
non-zero, otherwise `unpacked` — so both sides always land on the same number:

- LZMA2 dictionary = `clamp(coded_len, 4096, 64 MiB)`. A bare LZMA2 stream
  carries the props byte but not the dictionary size, and a decoder window may
  be wider than the encoder's but never narrower.
- PPMd7 suballocator pool = `clamp(coded_len * 32, 1 MiB, 256 MiB)`, with the
  model order in the per-unit `param` byte (`0` means the default, 10).

Get either wrong and the stream decodes into garbage of exactly the right
length: PPMd7's pool saturates at both ends, so a wrong formula still works on
very small and very large units and fails only in a band between them.

Filter ids:

| id | filter | length |
|---|---|---|
| `0` | none | — |
| `1` | BCJ x86 | unchanged |
| `2..=33` | delta at distance `id - 1` | unchanged |
| `34` | deflate streams in a container, preflate 0.7.x | changed, **decode only** |
| `35` | JPEG re-entropy-coded, lepton 0.5.x | changed |
| `36` | x86 branch targets split into their own stream | changed |
| `37` | mixed container: deflate **and** JPEG streams | changed |
| `38` | RIFF/WAVE integer PCM carried as FLAC | changed |
| `39` | MPEG Layer III split into header / side-info / spectral planes | changed |

An id is a promise, not a slot to reuse: 34 still decodes every archive that
used it, and 37 — which can say what each stream *is* — is what new archives
get. Ids 34, 35 and 37 pin the library that wrote them, so upgrading it spends
a new id. Ids 38 and 39 do not: 38 stores a standard FLAC stream plus a wrapper
of this format's own, and 39 stores the MPEG bytes themselves, merely
reordered. Both carry a version byte inside the payload (`NWv1`, `NM31`), so
the layout can grow without spending an id, while the encoder may change freely
because the decoder keeps reading everything ever written.

A length-changing filter records the coded length in `ChunkRec::filtered`;
`unpacked` always means the ORIGINAL length, the one `hash` covers and `Extent`
indexes into.

Order of operations is fixed: pack = `filter → compress`; unpack =
`decompress → unfilter`. Per-unit fallback: if the result is not smaller than
the input, the unit is stored raw with no filter recorded.

BCJ is always applied with a start offset of 0, never the unit's position in
the file. Position-dependent filtering would make identical units encode
differently and break dedup; the cost is one unconverted instruction per unit
boundary.

### Manifest

MessagePack (with field names, for forward evolution), then compressed. A
manifest below 128 KiB is compressed with zstd; at or above it, LZMA2 is also
tried and the smaller wins. There is no format field for which one was used —
the codec is read off the bytes, because a zstd frame starts `28 B5 2F FD` and
a raw LZMA2 stream cannot, so every manifest ever written still decodes.

```
Manifest  { generation: u64, files: [FileEntry], chunks: [ChunkRec], geometry: Geometry? }
Geometry  { chunk_min: u32, chunk_avg: u32, chunk_max: u32, unit: u64, chunked_from: u64 }
FileEntry { path: str, size: u64, mtime: i64, extents: [Extent] }
Extent    { unit: u32, off: u64, len: u64 }
ChunkRec  { offset: u64, packed: u64, unpacked: u64, filtered: u64,
            codec: u8, param: u8, filter: u8, hash: bin16 }
```

`filtered` is what a length-changing filter (ids 34-39) produced and therefore
the length the CODEC was given; `0` means "same as `unpacked`". A decoder must
read it, because `unpacked` keeps one meaning forever — the ORIGINAL length,
the one `hash` covers and `Extent` indexes into — and the LZMA2 window and
PPMd7 pool are derived from the CODED length on both sides.

`chunks` is the unit table. A file's `extents` name byte ranges of units, in
file order: one extent for a small file inside a shared unit, a run of them
for a large file.

`geometry` is fixed when the archive is created and reused by every later
`add`, whatever compression level is asked for. Deduplication only works when
identical data lands on identical unit boundaries, and an archive re-chunked
at a different tier deduplicates nothing: measured, re-adding an untouched
28 MiB tree at another tier stored all of it again.

`path` is relative, UTF-8, `/`-separated. `mtime` is Unix seconds and may be
negative (pre-1970); `0` means unknown. `hash` is a MessagePack `bin` of 16
bytes. Fields that are empty or zero are omitted entirely.

`chunks` may contain units not referenced by any file (dead after a
replace/remove); they remain valid dedup sources until `compact`.

Both writing and extraction validate every path component with the same rules
— no `..`, no absolute paths, no drive letters or ADS colons, no control or
`<>"|?*` characters, no trailing dot/space, no Windows reserved device names.
Writers therefore cannot produce an archive that extraction would refuse, and
a hostile archive cannot escape the destination directory.

### Footer (80 bytes, at EOF)

| offset | size | field |
|---|---|---|
| 0 | 8 | magic `NOVAEND1` |
| 8 | 8 | generation (LE u64) |
| 16 | 8 | manifest offset |
| 24 | 8 | manifest packed size |
| 32 | 8 | manifest unpacked size |
| 40 | 16 | blake3-128 of packed manifest |
| 56 | 8 | reserved (0) |
| 64 | 16 | self-check: blake3-128 of `bytes[0..64] ‖ footer_offset_le` |

The self-check covers the footer's own absolute offset. Without that binding,
a `.nva` file stored *inside* another archive would place a byte-identical,
self-consistent footer image in the middle of the outer archive, where the
crash-recovery scan could mistake it for a real commit.

## Commit protocol & crash recovery

Writing an update is: append chunks → append manifest → **fsync** → append
footer → **fsync**. The barrier matters: without it a crash can leave a valid
footer pointing at a half-written manifest.

Readers look for a valid footer at EOF−80, then scan backwards. Because a
footer's self-check only proves the footer itself is intact, the reader
*verifies the manifest* each candidate points at (bounds, blake3, decode) and
resumes the backward scan below any candidate that fails, up to 64 candidates.
The result is always the last fully committed generation. A read-write open
truncates the uncommitted tail.

Writers take an exclusive advisory lock on the archive file: two concurrent
writers would otherwise append at the same stale EOF and overwrite each
other's chunks.

Untrusted size fields never drive allocations: manifests are capped at 256 MiB
and 1000× their packed size, units at twice the largest unit any tier writes,
and every unit offset must lie inside the file. `compact` verifies each unit's
checksum before copying it, so compaction cannot bake corruption into the new
archive.

`compact` writes a temporary file in the same directory and atomically
replaces the archive (`MoveFileEx`/`rename`), so a crash leaves either the old
or the new archive, never neither.

## Two-phase compression

Phase 1 (analysis), per file and again per unit, from the head bytes:

| content | filter | fast / normal | max |
|---|---|---|---|
| deflate container (zip, PNG, gzip, docx, PDF) | mixed container (37) | zstd | LZMA2 |
| JPEG photograph | lepton (35) | zstd | LZMA2 |
| RIFF/WAVE integer PCM | FLAC (38) | zstd | LZMA2 |
| MPEG Layer III (.mp3) | plane split (39) | zstd | LZMA2 |
| already compressed (video, 7z…) | — | store | store |
| executable (PE/ELF/Mach-O) | BCJ x86 / x86 split at max | zstd | LZMA2 |
| text / source | — | zstd | PPMd7 |
| other, compressible | — | zstd | LZMA2 |
| other, fixed-width records | delta at the detected width | zstd | LZMA2 |
| other, incompressible | — | store | store |

A zip is scanned through its central directory, and a **stored** entry (method
0) is handed back to the same dispatcher as if it were a file on disk, up to
three levels deep. That is where a zip keeps what deflate could not help:
photographs, an epub's illustrations, an apk's assets.

The first two rows need the WHOLE file: a zip's central directory is at its end
and lepton reads one complete JPEG. Such a file is given a unit of its own,
which is only possible between 64 KiB and twice the unit size — that upper
bound is the largest chunk a reader accepts, not a tuning knob. Outside the
range the file takes the ordinary path for its content.

A `.wav` is the exception, because FLAC frames are independent: a file past the
bound is cut into unit-sized runs of whole frames, each its own unit. Only the
first piece carries the RIFF header and only the last carries whatever follows
the `data` chunk; the pieces in between are bare PCM. Their format therefore
cannot be re-read from the bytes and travels with the packer's plan — but it is
written into every record, so nothing about decoding changes.

An `.mp3` needs no such treatment at all: every MPEG frame carries its own
length in its own header, so any contiguous stretch of a file can be split into
planes on its own. It takes the ordinary chunking path at any size, and a piece
that begins mid-frame simply starts with a raw segment.

The record width is not declared anywhere, so it is inferred: the width that
minimises the order-0 entropy of the differenced stream, then **verified** by
compressing a sample with and without the filter and kept only if it wins by
at least 2%. The verification matters more than the estimate — on Silesia's
`sao` the estimate proposed a width that made LZMA2 8-60% worse — and it is
made with the codec that will actually run, because BWT already models
interleaved fixed-width records and differencing them first makes it worse.

Phase 2 (compression): the unit is filtered, then compressed at the tier level
(zstd 3 / 12 / 19; LZMA2 presets 2 / 6 / 6+nice_len). The normal tier races
its codec against bsc; the max tier races LZMA2, PPMd7 at two model orders and
bsc, and the smallest result wins — no static rule picks the right codec for
arbitrary data. A filtered form that is already entropy-coded (lepton, FLAC)
often beats every codec run over it, so the smallest of {codec over filtered,
filtered verbatim, original} is what gets stored.

Measured: PPMd7 is ~25% smaller than zstd-19 on prose; LZMA2 beats PPMd7 by
16% on binaries; BCJ gains 4.4-5.7% on real executables; the record filter
gains 11-17% on database and catalogue data and 24% on 16-bit stereo PCM;
FLAC takes another 19% off that PCM.

Later milestones: re-coding an MP3's spectral data rather than only moving it
(filter 39 separates the planes; the Huffman layer is untouched), and a
dictionary for units created by `add` after the archive exists (measured at
-21% to -31% there, and a net loss anywhere else).

## Limits

- Unit count per archive: 2^32.
- Manifest is held in memory; at ~40 bytes per unit and per extent (less after
  zstd), a 1 TB archive needs a manifest of a few MB. Fine for v0; a paged
  index is a v1+ topic if it ever matters.
- Not yet stored: empty directories, symlinks, NTFS attributes/ADS, ACLs.
- Two entries whose paths differ only in letter case cannot both be extracted
  on Windows/macOS; the writer warns and extraction skips the duplicate.
- Extraction memory is one unit plus its packed form: ~10 MiB for a fast
  archive, ~80 MiB for a max one.
- Single-writer format, enforced by an advisory file lock.
