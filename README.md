# Nova Arc

A modern archiver for Windows 10/11 (Linux/macOS/Android planned), built in
Rust around a new archive format — **`.narc`** — designed for *editing*
archives, not just creating them.

> Status: early development, but usable. The `.narc` format, a CLI and a
> desktop GUI work today. zip/7z/rar support and recompression of
> already-compressed data are next. Expect the format to change before v1.0.

## Why another archiver?

Change one photo caption inside a 700-photo zip/7z/rar archive and you pay
for a full repack. In `.narc` an edit costs only the changed data: the
archive is an append-only log of content-defined chunks (FastCDC) with
deduplication, a manifest and a crash-safe footer. Replacing one file in a
46 MiB archive takes ~0.1 s and grows the archive by ~100 KiB; dead space is
reclaimed by an explicit `compact`.

Built on top of that:

- **Two-phase compression** — every file is analyzed first, then compressed
  with the method that suits it: PPMd for text, LZMA2 for binaries, a BCJ
  transform for executables, and nothing at all for data that is already
  compressed. Small files are packed into solid blocks so the compressor can
  exploit what they have in common. At the max tier each unit is compressed by
  several codecs and the smallest result kept. On the Silesia corpus the max
  tier matches 7-Zip's best ratio (23.2% vs 23.0%) in a fraction of the time.
- **A desktop GUI** (Tauri 2): a file list with type glyphs and solid-block
  badges, open a file straight from the archive (extracted to a temp folder
  that is cleaned up automatically), drag & drop with Explorer both ways,
  live progress. No framework telemetry.
- **Familiar formats** (planned) — pack/unpack zip & 7z, unpack rar (creating
  RAR archives is not legally possible for anyone but RARLAB).
- **Recompression** (planned) — losslessly repack deflate (zip/docx/apk/png),
  JPEG and MP3 for gains 7-Zip and WinRAR cannot reach.
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
- `crates/narc-gui` + `ui/` — the desktop app (Rust core, TypeScript frontend)
- `docs/research/` — technology research the design decisions are based on

## Build the app

```bash
cargo build --release                          # the narc CLI
cd ui && npm install && npm run build && cd ..  # the frontend bundle
cd crates/narc-gui && node ../../ui/node_modules/@tauri-apps/cli/tauri.js build --no-bundle
```

The app binary is `target/release/nova-arc.exe`.

## License

TBD (will be chosen before the first release).
