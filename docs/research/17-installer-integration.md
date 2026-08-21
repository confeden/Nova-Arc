# 17 — Putting nova's max tier inside installers

Owner's ask: let installer authors compress their payload with nova's max
tier as easily as they use LZMA today, under a short name — **nova**, decided
(§7) — with a self-extraction story. Not scheduled; this is the plan to
argue with before any of it is built.

Everything measured here is from this repo's own benches (see ROADMAP for the
harnesses). Two numbers below are explicitly **not** measured and say so.

---

## 1. What "as easy as LZMA" actually demands

LZMA is in NSIS, Inno Setup, 7-Zip's SFX, and a large share of firmware
images for one reason, and it is not ratio: **its decoder is one C file with
no dependencies and no configuration.** You can paste `LzmaDec.c` into a
bootstrapper written in 1998 and it will build. Everything else follows from
that — no runtime, no allocator assumptions worth the name, no build system.

An installer's compressor is therefore judged on:

| Requirement | Why it is non-negotiable |
|---|---|
| Tiny, dependency-free decoder | It ships in *every* installer, including the 900 KB ones |
| Bounded, predictable memory | The bootstrapper runs before anything is installed, on the user's worst machine |
| Streaming decode | Extract while downloading; no room for a scratch copy of the payload |
| Deterministic across machines | A build server compresses; a million different PCs decompress |
| Stable format, forever | An installer built today must still unpack in ten years |

nova already satisfies three of those five. The two it does not are the whole
problem, and they are different in kind: one is an engineering cost, the
other is a design decision nobody has made yet.

---

## 2. Where nova stands against that bar

**Ratio, measured.** Against `7z -mx9`, which is the incumbent in this exact
role:

| Payload | nova max | 7z -mx9 | |
|---|---:|---:|---|
| Silesia (mixed) | 43,036,408 | 48,688,268 | −11.6% |
| Already-compressed corpus | 74,865,900 | 86,991,164 | −13.9% |
| PDF documents | 5,064,071 | 6,606,978 | −23.4% |
| Camera JPEGs | 13,990,872 | 17,289,234 | −19.1% |
| JPEGs stored in a zip | 13,992,258 | 17,289,656 | −19.1% |
| Installed program (Firefox) | 93,467,847 | 87,566,439 | **+6.7%** |

**Decode speed, measured.** Silesia 1.8 s, enwik8 4.6 s on 8 cores. Same
order as LZMA; not a blocker. But bsc decodes single-threaded at ~25 MB/s,
and nova disables libbsc's own threads, so a bsc-heavy payload is the slow
case (normal-tier Silesia extract went 0.5 → 2.1 s when bsc landed).

**Decode memory, measured.** Tracks `--memory`: 256M → 153 MiB peak,
default → 1.0 GiB. Bounded and tunable, which is what matters, but the
*default* is far above what a bootstrapper should assume. An installer
profile would have to pin this low and accept the ratio cost.

**Determinism.** Output is byte-identical at `-j 1` and `-j 8` — an owner
requirement, already enforced by `test/scaling.sh`. Decode has no floating
point anywhere. This requirement is met outright.

---

## 3. The one number that decides everything: decoder size — MEASURED

This was the open question the whole document hung on. It is answered, and the
answer is much better than the 4.80 MiB of `nova.exe` suggested — that figure
is a full CLI at default release settings, and clap plus the encoders are most
of it.

`test/size-probe.sh` builds one binary per feature set with the settings a
shipped stub would use (`opt-level="z"`, fat LTO, `codegen-units=1`,
`panic="abort"`, stripped), each one calling its decoder on a file from argv so
nothing can be dead-code-eliminated. `floor` is a Rust std binary with no
decoder in it, so **feature − floor is what that decoder really costs**:

| build | bytes | | cost over floor |
|---|---:|---|---:|
| floor (Rust std, no decoder) | 117,760 | 115.0 KiB | — |
| + LZMA2 decode | 132,608 | 129.5 KiB | **14.5 KiB** |
| + PPMd7 decode | 131,072 | 128.0 KiB | **13.0 KiB** |
| + claxon (FLAC, filter 38) | 152,064 | 148.5 KiB | **33.5 KiB** |
| + blake3 + MessagePack manifest | 244,224 | 238.5 KiB | **123.5 KiB** |
| + zstd (**C**) | 279,552 | 273.0 KiB | **158.0 KiB** |
| + libbsc (**C++**) | 282,624 | 276.0 KiB | **161.0 KiB** |
| + preflate recreate (filter 34/37) | 284,160 | 277.5 KiB | **162.5 KiB** |
| + lepton decode (filter 35) | 351,232 | 343.0 KiB | **228.0 KiB** |

The pure-Rust strong codecs are nearly free — **LZMA2 decode is 14.5 KiB and
PPMd7 decode is 13.0 KiB**, because a range decoder plus a model is small and
all the memory is allocated at runtime. What costs is the C/C++ libraries
(whose crates build the *encoder* too, so those rows are beatable) and, oddly,
our own manifest layer: blake3 + serde + rmp-serde is 123.5 KiB, the second
largest line item after lepton.

**And the ceiling, also measured.** nova-core exactly as it stands — every
encoder, the four-codec tournament, the zip/7z readers, the whole pack and
extract pipeline — linked into a binary with no CLI, same stub settings:
**1,185,280 B (1.13 MiB)**. A decode-only build cannot be larger than that.
So the honest bracket for a max-everything stub is **0.98–1.13 MiB**, and
4.80 MiB was never the relevant number.

### The design this buys: a profile ladder, not one decoder

A decoder must implement every branch the encoder was allowed to take — that
part of the old objection stands. The answer is to **bound what the encoder is
allowed to take**, per archive, and ship one stub per bound:

| profile | contains | measured stub, decoders only |
|---|---|---:|
| **core** | store + LZMA2, BCJ/BCJ2/delta/record, manifest | **257,536 B (251.5 KiB)** |
| **media** | core + preflate + lepton + FLAC | **672,256 B (656.5 KiB)** |
| **max** | media + PPMd7 + libbsc + zstd | **1,005,568 B (982.0 KiB)** |

Add roughly 200 KiB of nova's own container/extract logic to each (the gap
between the 982 KiB max row and the 1.13 MiB whole-engine ceiling) and a real
stub is ~450 KiB at core, ~1.2 MiB at max.

Three things follow, and they are the plan:

1. **`nova create --profile core|media|max`** — the packer refuses codecs and
   filters outside the profile. An installer build server pins `core` and gets
   an archive any 450 KiB stub can open. Encoder-side only; no format change.
2. **`nova sfx` picks the smallest stub that fits.** The manifest already
   records every codec and filter actually used (`info --units` reads them), so
   the tool computes the required profile from the archive rather than being
   told. Nobody ships lepton to unpack a DLL tree.
3. **The stub size only matters against the payload.** +700 KiB of stub to take
   20% off a 200 MB media pack is 40 MB saved; the same stub on a 5 MB payload
   is absurd. That ratio, not an absolute KiB target, is what the tool should
   reason about — and what the pitch should say.

**One encoder-side fix the core profile needs.** A manifest under 128 KiB is
zstd-compressed, and the codec is detected from the frame magic, so today even
a trivial archive drags **C** into the smallest decoder. Force the manifest to
LZMA2 (or store) under `--profile core` and the core stub is **pure Rust with
no C or C++ at all** — which §9 correctly names as the thing that makes a
decoder easy to integrate. Backward compatible by construction: the reader
already picks the codec off the bytes.

---

## 4. Where this genuinely wins, and where it must not be sold

The Firefox row above is the warning: on an **installed program tree** —
mostly DLLs — nova is 6.7% *worse* than 7-Zip. A generic "use nova for your
installer" pitch would lose on exactly the payload most installers carry.

The win is where the payload is **already-compressed data an LZMA-based
installer cannot touch at all**: JPEG/PNG textures and UI art, PDF manuals,
bundled zips and jars, WAV/audio banks. Game installers, creative-suite
installers and anything with a media pack are made of this. −17% to −23%
against 7-Zip on those, and nothing else in the installer world does it.

So the pitch is not "better LZMA". It is: **"your art and media pack shrinks
by a fifth; your DLLs do not."** A serious integration would let an author
mix — nova for asset archives, LZMA for the binary tree — which the format
already supports per unit.

---

## 5. Self-extraction: a specific blocker, already verified

The classic SFX is `copy /b stub.exe + payload.nva setup.exe`. **That cannot
work with the format as it stands**, and not by accident:

- `Footer::encode(&self, at: u64)` hashes the footer bytes *together with its
  own absolute offset* (`footer.rs`), deliberately, so that a `.nva` embedded
  in another file can never be mistaken for a real commit.
- `manifest_offset` is an absolute file offset too.

Prepend a stub of size S and every offset shifts by S; the self-hash stops
matching and the archive refuses to open. The anti-embedding defence and the
SFX trick are the same mechanism pointed in opposite directions.

Three ways out. The first was written off here as a format change; it is not,
and it is the right answer.

1. **Base-offset-aware open — RECOMMENDED, and not a format change.** Teach
   the *reader* an explicit `base` and hash `at - base`. The archive bytes are
   then **byte-identical to a standalone `.nva`**: build it normally, then
   `copy /b stub.exe + payload.nva setup.exe`. Nothing in the file changes,
   nothing in the spec changes, and every existing archive still opens.

   The embedding defence survives intact *provided the base comes from
   out-of-band knowledge, never from the file*. That is the whole rule. A
   normal open uses `base = 0`, so a `.nva` sitting inside a data file still
   cannot pass its self-hash — the defence is exactly as strong as today. Only
   a caller that already knows where its own payload starts may pass a nonzero
   base, and a stub knows that about itself. If the base were ever read out of
   a header, the defence would collapse; so it must not be, and the API should
   make that impossible by taking the base as an argument rather than a field.

2. **Payload as a PE resource or a named section.** Also works, no format
   change, but it costs a resource-extraction step, makes the build need `rc`,
   and hits a practical size ceiling on some toolchains. Keep as the fallback.

3. **Two files** (`setup.exe` + `data.nva`). Zero work, and unacceptable for
   most distributors; listed only to be dismissed.

**How the stub finds its own payload, and why not the obvious way.** Do *not*
put a marker at EOF. Installers are code-signed, and Authenticode appends the
certificate table to the end of the file — an EOF trailer written before
signing is no longer at EOF afterwards. Parse the stub's own PE headers
instead: the payload begins at the end of the image (`SizeOfHeaders` plus the
sum of every section's `SizeOfRawData`, rounded to `FileAlignment`). That is
what NSIS and 7-Zip's SFX do, it is stable under signing because the cert table
lands *after* the payload, and it needs nothing appended anywhere.

---

## 6. Integration shapes, ranked

**(a) `nova-sfx`: a decode-only stub that makes a self-extracting exe.**
Smallest surface, no third party involved, and the natural first deliverable.
Blocked on §3's measurement and §5's choice.

**(b) `nova-dec`: a decode-only static lib / DLL with a flat C ABI.**
This is what actually gets adoption: Inno Setup takes external compression
DLLs, NSIS takes plugins, WiX/InstallShield take custom actions. A C ABI of
about five functions — open, list, extract-to-callback, memory-limit, free —
lets every one of those engines integrate without knowing Rust exists.
Requires a stable ABI and a memory-limit knob honoured from the caller.

**(c) A published format spec + reference decoder.** `docs/format.md` already
exists and is kept in step with codec/filter ids. Adoption by anyone outside
this repo needs it to be *complete enough to reimplement*, including the
PPMd7 pool formula and the LZMA2 dictionary derivation — both currently
"FORMAT CONSTANTS" documented as such precisely because they cannot be
inferred from the bitstream. That is a documentation debt to pay before
inviting third parties.

**Order: (a) to prove it, (b) to spread it, (c) to make it survive us.**

---

## 7. The name — DECIDED: **nova**

The method installer authors ask for is called **nova**, not PRISM. Owner's
call, and it also settles the two cautions PRISM carried: the word is widely
known as the NSA surveillance programme disclosed in 2013 — an unforced
headwind for something that ships inside other people's installers — and
several products already use Prism/Prisma, so it would have needed a
trademark search first.

`nova` keeps what made PRISM a good candidate. It is short, it reads as a
method rather than a codec — accurate, since the max tier *is* a tournament
and a filter set rather than one algorithm — and it costs nothing to explain,
because it is already the binary name, the format magic and the product.
"Nova Prism" stays the product; `nova` is the method inside it, the same way
LZMA is the method inside 7-Zip.

Define it precisely wherever it appears in a spec, so it never gets read as
"a new entropy coder": **nova = the max tier — a per-unit tournament over
LZMA2, PPMd7 and BWT, with the recompression filter set.**

Naming that follows from it: `nova-sfx`, `nova-dec`, `NOVA_*` for the C ABI.

---

## 8. Staged plan

| Stage | Deliverable | Gate to the next |
|---|---|---|
| 0 | **DONE** — per-feature and per-profile stub sizes (`test/size-probe.sh`) | 251 KiB core / 982 KiB max: (a) is viable |
| 1 | **DECIDED** — §5: base-offset open, payload appended, found via PE headers | No format change, so no version bump |
| 2 | `--profile core\|media\|max` on the packer + manifest forced off zstd at core | A core archive that a pure-Rust decoder opens |
| 3 | Decode-only cargo features across nova-core (strip the encoders) | The 982 KiB becomes a real binary, not a probe |
| 4 | `nova-sfx` stub + `nova sfx out.exe archive.nva`, picking the smallest fitting profile | A self-extracting exe that runs on a clean VM |
| 5 | Installer profile: pinned low memory, documented ratio cost | Measured ratio at 64/128/256 MiB decode budgets |
| 6 | `nova-dec` C ABI + one real integration (Inno Setup DLL) | An installer built by someone who is not us |
| 7 | Spec completion for third-party decoders | Someone reimplements the decoder from the doc alone |

Stages 0 and 1 are done. Stage 2 is small and encoder-side. Stage 3 is the
real work — feature-gating the encoders out of nova-core touches `analyze`,
`pack`, `pipeline` and `filters` — and it is also the only way the numbers in
§3 turn into something shippable. Nothing past stage 4 is worth starting until
an installer author has said the −20% on media is worth the decoder to them.

---

## 9. What could kill this

- ~~**Decoder size**~~ — RETIRED by §3. 251 KiB at core, under 1.13 MiB for
  everything nova can do. This was the most likely killer and it is not one.
- **Memory floor.** If the installer profile has to sit at 64 MiB, the ratio
  advantage shrinks toward LZMA's and the pitch evaporates. Unmeasured, and
  now the *first* unmeasured thing on the list.
- **bsc's C++ and zstd's C.** A pure-Rust decoder is a much easier sell to
  integrators than one that needs a C++ toolchain. The core profile is already
  pure Rust once the manifest stops using zstd (§3), and it costs nothing on
  the payload most installers carry; at max, bsc and zstd are 319 KiB of the
  982 and both crates build their encoders too, so both rows are beatable.
- **Format stability.** Ids 34/35/37 pin library versions by design; an
  installer built against one and unpacked by another must still work. The
  `legacy-*.nva` fixtures are the existing guard and would need extending.
