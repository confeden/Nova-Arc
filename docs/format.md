# NARC format — v0.1 draft

Status: prototype, subject to change until v1.0. This document describes what
the code in `crates/narc-core` actually implements. Pre-1.0 the reader rejects
any archive with a higher **minor** version, since the format may still change
incompatibly.

## Design goals

1. **Cheap edits.** Modifying, adding or deleting entries in a multi-GB
   archive must cost O(changed data), not O(archive size). No full repack.
2. **Random access.** Extracting a single file reads only that file's chunks.
3. **Bounded memory.** No operation needs more than a few chunk buffers
   (max chunk = 4 MiB), so weak machines can always extract.
4. **Content-defined dedup.** Identical data across files (or across
   versions of a file) is stored once.
5. **Crash safety.** A torn update can never corrupt committed data.

## File layout

```
[header 16B][manifest g1][footer g1]              <- `create` (empty archive)
            [chunks ...][manifest g2][footer g2]  <- first add
            [chunks ...][manifest g3][footer g3]  <- update
```

Every update (add/replace/remove) appends new chunks (if any), then a fresh
manifest and footer. Earlier manifests/footers and unreferenced chunks become
dead space, reported as `reclaimable` and reclaimed by the offline `compact`
operation (rewrite + atomic replace).

### Header (16 bytes)

| offset | size | field |
|---|---|---|
| 0 | 4 | magic `NARC` |
| 4 | 1 | version major (0) |
| 5 | 1 | version minor (1) |
| 6 | 2 | flags (0) |
| 8 | 8 | reserved (0) |

### Chunks

Files are split with FastCDC (v2020), min/avg/max = 256 KiB / 1 MiB / 4 MiB.
Each chunk is stored as its (possibly compressed) payload, back to back; there
is no per-chunk framing — chunks are located purely via the manifest. A chunk
is identified by the blake3 hash of its **uncompressed** content truncated to
128 bits; that hash provides both dedup and integrity checking on extract.

Codec ids: `0` = store (raw), `1` = zstd. Per-chunk fallback: if compression
does not shrink a chunk, it is stored raw regardless of the file's codec.

### Manifest

MessagePack (with field names, for forward evolution), compressed with zstd.

```
Manifest  { generation: u64, files: [FileEntry], chunks: [ChunkRec] }
FileEntry { path: str, size: u64, mtime: i64, chunks: [u32] }   // indices into Manifest.chunks
ChunkRec  { offset: u64, packed: u64, unpacked: u64, codec: u8, hash: bin16 }
```

`path` is relative, UTF-8, `/`-separated. `mtime` is Unix seconds and may be
negative (pre-1970); `0` means unknown. `hash` is a MessagePack `bin` of 16
bytes.

`chunks` may contain records not referenced by any file (dead after a
replace/remove); they remain valid dedup sources until `compact`.

Both writing and extraction validate every path component with the same rules
— no `..`, no absolute paths, no drive letters or ADS colons, no control or
`<>"|?*` characters, no trailing dot/space, no Windows reserved device names.
Writers therefore cannot produce an archive that extraction would refuse, and
a hostile archive cannot escape the destination directory.

### Footer (80 bytes, at EOF)

| offset | size | field |
|---|---|---|
| 0 | 8 | magic `NARCEND1` |
| 8 | 8 | generation (LE u64) |
| 16 | 8 | manifest offset |
| 24 | 8 | manifest packed size |
| 32 | 8 | manifest unpacked size |
| 40 | 16 | blake3-128 of packed manifest |
| 56 | 8 | reserved (0) |
| 64 | 16 | self-check: blake3-128 of `bytes[0..64] ‖ footer_offset_le` |

The self-check covers the footer's own absolute offset. Without that binding,
a `.narc` file stored *inside* another archive would place a byte-identical,
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
and 1000× their packed size, chunks at 8 MiB, and every chunk offset must lie
inside the file. `compact` verifies each chunk's checksum before copying it,
so compaction cannot bake corruption into the new archive.

`compact` writes a temporary file in the same directory and atomically
replaces the archive (`MoveFileEx`/`rename`), so a crash leaves either the old
or the new archive, never neither.

## Two-phase compression (v0 seed)

Phase 1 (analysis): sample the first 64 KiB of each file — magic-byte
detection of already-compressed formats (JPEG/PNG/zip/MP3/video/...) plus a
trial zstd-level-1 compression; choose Store or Zstd.
Phase 2 (compression): per-chunk zstd at the tier level (fast=3, normal=12,
max=19) with per-chunk raw fallback.

Later milestones extend phase 1 into a real dispatcher: per-type filters
(delta, BCJ for executables), solid grouping of small files, recompression of
deflate/JPEG/MP3 streams, stronger codecs for the max tier.

## Limits

- Chunk count per archive: 2^32.
- Manifest is held in memory; at ~40 bytes/chunk raw (less after zstd),
  a 1 TB archive with 1 MiB chunks needs a manifest of tens of MB. Fine for
  v0; a paged index is a v1+ topic if it ever matters.
- Not yet stored: empty directories, symlinks, NTFS attributes/ADS, ACLs.
- Two entries whose paths differ only in letter case cannot both be extracted
  on Windows/macOS; the writer warns and extraction skips the duplicate.
- Single-writer format, enforced by an advisory file lock.
