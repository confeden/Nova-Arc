# Nova Prism — agent rules

Rust archiver for Windows: CLI `nova`, GUI `nova-prism`, format `.nva`. Cargo
workspace — `nova-core` (format, `#![forbid(unsafe_code)]`), `nova-cli`,
`nova-platform` (OS + unsafe), `nova-bsc` (libbsc FFI), `nova-gui` (Tauri 2),
`ui/` (TS+Vite). It wins by taking already-compressed data apart, not by
out-tuning LZMA.

## Commands
| Task | Command | cwd |
|---|---|---|
| build | `cargo build --release` | repo root |
| test | `cargo test --workspace --release` | repo root |
| lint | `cargo clippy --workspace --all-targets` | repo root |
| GUI | `npm --prefix ui run build` FIRST, then `cargo build --release -p nova-gui` | repo root |
| installer | `ui/node_modules/.bin/tauri build` | `crates/nova-gui` |
| benches | `test/bench-std.sh`, `test/mp3-bench.sh`, `test/scaling.sh` | repo root |
| corpora | `python test/fetch-{precomp,audio,mp3,photos}.py` | repo root |

`test/` is a gitignored playground; put scratch work there. Adding a recipe file
to it needs `git add -f`.

## Never
- **Never truncate on open when a committed footer was skipped.** A footer that
  verified its own hash but whose manifest will not decode is damage, not a
  crash. Getting this backwards erased a 12 MB archive while reporting success.
- **Never size a `Vec` from a count a payload claims.** Bound it by the RECORD
  size, not the buffer length, and grow as records parse. Rust aborts on
  allocation failure, so this is a crash an archive can ask for.
- **Never add a filter id by widening the delta range**, and never change a
  derived size (LZMA2 dict, PPMd7 pool) — both silently mis-decode every
  archive already written. Ids are permanent; spend a new one.
- **Never put `devUrl` in `tauri.conf.json`.** It makes every non-Tauri-CLI
  build come out in dev mode, and a plain `cargo build --release` then
  overwrites a good exe with one that opens localhost.
- **Never re-add `.gitignore`.** Owner's decision: the rules live in
  `.git/info/exclude`.

## Style
- Chat with the owner in Russian. Code, `docs/` and CLI output in English.
  `README.md` and `CHANGELOG.md` are Russian; the GUI ships Russian.
- Claim something works only when it was actually run. Otherwise say
  "built, not verified".
- A number without a measurement behind it does not go in a doc.

## Context
`ROADMAP.md` is the project state — read it once before substantive work, not
for a one-off question. Deep detail lives in `.claude/kb/`, indexed at the end
of `ROADMAP.md`; read the one file the index row names. Both are maintained by
the `roadmap` skill.
