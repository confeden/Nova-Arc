# Nova Arc

A modern archiver for Windows 10/11 (Linux/macOS/Android planned), built in
Rust around a new archive format — **`.narc`** — designed for *editing*
archives, not just creating them.

> Status: early development. The `.narc` container v0 and a CLI work today;
> GUI, zip/7z/rar support and the advanced compression pipeline are in
> progress. Expect the format to change before v1.0.

## Why another archiver?

Change one photo caption inside a 700-photo zip/7z/rar archive and you pay
for a full repack. In `.narc` an edit costs only the changed data: the
archive is an append-only log of content-defined chunks (FastCDC) with
deduplication, a manifest and a crash-safe footer. Replacing one file in a
46 MiB archive takes ~0.1 s and grows the archive by ~100 KiB; dead space is
reclaimed by an explicit `compact`.

Planned on top of that:

- **Two-phase compression** — analyze files first, then compress each with
  the best method for its type (filters, solid small-file groups, and
  recompression of already-compressed data: deflate/JPEG/MP3).
- **Familiar formats** — pack/unpack zip & 7z, unpack rar.
- **GUI** with file list, icons and previews; "open from archive in
  Explorer" with automatic temp cleanup and write-back.
- **Polite resource usage** — packing uses every core at below-normal CPU,
  memory and I/O priority, inside a memory budget that adapts to how loaded
  the machine already is (`--memory 512M`, `--eco`, `-j`). Extraction needs
  ~10 MiB of RAM regardless of archive size, so weak PCs can always unpack.
  GPU acceleration is being researched (nvCOMP/CUDA).
- **No telemetry, no ads, no analytics. Ever.**

## Try it

```bash
cargo build --release
target/release/narc create photos.narc D:/Photos
target/release/narc list photos.narc
target/release/narc add photos.narc D:/Photos      # re-save: only changes are written
target/release/narc extract photos.narc -o out   # refuses to overwrite; --force / --skip-existing
target/release/narc info photos.narc               # shows reclaimable dead space
target/release/narc compact photos.narc
```

## Repository layout

- `crates/narc-core` — the NARC format library ([format spec](docs/format.md))
- `crates/narc-cli` — the `narc` command-line tool
- `crates/narc-platform` — OS resource policy (priorities, I/O hints, limits)
- `docs/research/` — technology research the design decisions are based on

## License

TBD (will be chosen before the first release).
