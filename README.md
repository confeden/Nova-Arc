# Nova Arc

An archiver built around a question the popular formats never answered:
**why should changing one file cost a whole repack?**

`.narc` is an append-only container of content-defined chunks. Replacing a
photo inside a 700-photo archive appends the difference and nothing else —
about a second, instead of rebuilding gigabytes. Deleted space is reclaimed
when you ask for it, not before.

Compression is decided per file, not per archive: the analyzer looks at what
the data actually is, then the strongest tier compresses each block with
several codecs and keeps the smallest result. On the Silesia corpus that
matches 7-Zip's best ratio in a seventh of the time. Where it still loses, the
benchmarks in this repository say so plainly.

Written in Rust. Windows first, then Linux, macOS and Android. A desktop app
and a command-line tool, both offline: no telemetry, no ads, no analytics —
and there never will be.

Early development. The format will change before 1.0.

---

- [Format specification](docs/format.md)
- [Research the design is based on](docs/research/)

License: to be chosen before the first release.
