# 17 — Putting nova's max tier inside installers

Owner's ask: let installer authors compress their payload with nova's max
tier as easily as they use LZMA today, under a short name — proposed
**PRISM** — with a self-extraction story. Not scheduled; this is the plan to
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

## 3. The one number that decides everything: decoder size

`nova.exe` (full CLI, release) is **4.80 MiB measured**. That figure includes
every encoder — flacenc, lepton's encoder, preflate's encoder, the four-codec
tournament, the zip/7z/rar readers, clap — none of which a decoder needs.

A decode-only build is **NOT MEASURED**. It still has to carry, at minimum:

- LZMA2 decode (`lzma-rust2`, pure Rust)
- PPMd7 decode (`ppmd-rust`, pure Rust)
- bsc decode (`nova-bsc` → **libbsc, C++**)
- zstd decode (**C**)
- the filters: BCJ, BCJ2, delta, record-width (ours, pure Rust, small)
- preflate *recreate* (for filter 34/37 payloads)
- lepton *decode* (for filter 35)
- claxon (for filter 38)

That list is the honest answer to "why not just make it small": nova's max
tier is not a codec, it is a tournament plus a filter set, and **a decoder
must implement every branch the encoder was allowed to take.** Recompression
is where nova's advantage comes from, and each of those filters drags a
third-party library into the bootstrapper.

> **Measure this first, before anything else in this document.** Build a
> decode-only binary behind a cargo feature that strips the encoders, and
> report its stripped size. If it lands near 1 MiB, the SFX story is viable
> for mid-sized installers. If it lands near 4 MiB, only very large payloads
> can justify it and the plan below narrows to §6a.

---

## 4. Where this genuinely wins, and where it must not be sold

The Firefox row above is the warning: on an **installed program tree** —
mostly DLLs — nova is 6.7% *worse* than 7-Zip. A generic "use PRISM for your
installer" pitch would lose on exactly the payload most installers carry.

The win is where the payload is **already-compressed data an LZMA-based
installer cannot touch at all**: JPEG/PNG textures and UI art, PDF manuals,
bundled zips and jars, WAV/audio banks. Game installers, creative-suite
installers and anything with a media pack are made of this. −17% to −23%
against 7-Zip on those, and nothing else in the installer world does it.

So the pitch is not "better LZMA". It is: **"your art and media pack shrinks
by a fifth; your DLLs do not."** A serious integration would let an author
mix — PRISM for asset archives, LZMA for the binary tree — which the format
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

Three ways out, in order of preference:

1. **Base-offset-aware open.** Teach the reader an explicit `base` and hash
   `at - base`. Small, but it is a **format-visible change** and weakens the
   embedding defence unless the base is authenticated too. Needs care.
2. **Payload as a PE resource or a named section**, not concatenated. The
   stub reads it through a byte range it already knows. No format change, no
   weakening — costs a resource-extraction step and a 2 GB-ish practical
   ceiling on some toolchains.
3. **Two files** (`setup.exe` + `data.nva`). Zero work, and unacceptable for
   most distributors; listed only to be dismissed.

**Recommendation: (2) first** — it needs nothing from the format — and treat
(1) as a format-version question to decide deliberately, not as a patch.

---

## 6. Integration shapes, ranked

**(a) `prism-sfx`: a decode-only stub that makes a self-extracting exe.**
Smallest surface, no third party involved, and the natural first deliverable.
Blocked on §3's measurement and §5's choice.

**(b) `prism-dec`: a decode-only static lib / DLL with a flat C ABI.**
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

## 7. The name

"PRISM" is short, it matches the product, and it reads as a method rather
than a codec — which is accurate, since the max tier *is* a tournament and a
filter set rather than one algorithm. On that count it is a good name.

Two cautions, both worth checking before it appears in anyone's installer:

- **Collision.** PRISM is widely known as the NSA surveillance programme
  disclosed in 2013. For a format that ships inside other people's installers
  and touches user data, that association is an unforced headwind. It is not
  disqualifying — plenty of software reuses the word — but it should be a
  decision, not an accident.
- **Trademark.** Several shipping products use Prism/Prisma. A search is
  cheap and is the owner's call.

If a change is wanted, the same logic ("names a method, not a codec") is
served by anything short and neutral. If PRISM stays, define it precisely in
the spec: **PRISM = nova's max tier — per-unit tournament over LZMA2, PPMd7
and BWT, with the recompression filter set** — so it never gets read as "a
new entropy coder".

---

## 8. Staged plan

| Stage | Deliverable | Gate to the next |
|---|---|---|
| 0 | Decode-only build behind a cargo feature; report stripped size | Size decides whether (a) is viable at all |
| 1 | Decide §5: PE resource vs base-offset format change | Owner decision; format change needs a version bump |
| 2 | `prism-sfx` stub + `nova sfx out.exe archive.nva` | A self-extracting exe that runs on a clean VM |
| 3 | Installer profile: pinned low memory, documented ratio cost | Measured ratio at 64/128/256 MiB decode budgets |
| 4 | `prism-dec` C ABI + one real integration (Inno Setup DLL) | An installer built by someone who is not us |
| 5 | Spec completion for third-party decoders | Someone reimplements the decoder from the doc alone |

Stage 0 is a day. Stage 1 is a decision, not work. Nothing past stage 2 is
worth starting until an installer author has said the −20% on media is worth
the decoder to them.

---

## 9. What could kill this

- **Decoder size** (§3). The single most likely reason this never ships.
- **Memory floor.** If the installer profile has to sit at 64 MiB, the ratio
  advantage shrinks toward LZMA's and the pitch evaporates. Unmeasured.
- **bsc's C++.** A pure-Rust decoder is a much easier sell to integrators
  than one that needs a C++ toolchain; dropping bsc from an installer profile
  costs ratio on text but might be the right trade. Measurable today.
- **Format stability.** Ids 34/35/37 pin library versions by design; an
  installer built against one and unpacked by another must still work. The
  `legacy-*.nva` fixtures are the existing guard and would need extending.
