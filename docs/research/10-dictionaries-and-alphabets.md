# Research 10 — Adaptive dictionaries and alphabet/representation transforms

Scope: (a) trained dictionaries, (b) alphabet / numeral-system / representation
transforms, (c) a concrete design for narc. Everything labelled **MEASURED**
was run on this machine against narc's own corpus during this study; everything
labelled **CLAIMED** is a citation whose number I did not reproduce.

---

## 0. Bottom line

1. **The owner's core hypothesis is wrong at narc's current block size — and the
   measurement is unambiguous.** The idea was: "a dictionary stored once and
   used by every chunk gives cross-file redundancy WITHOUT enlarging the
   compression unit, so edits stay cheap." At 32 MiB solid blocks the whole
   64 KiB→1 MiB sweep lands **between −0.1% and +0.8% of the no-dictionary total
   once the dictionary's own bytes are stored** — break-even within noise, with
   the requested sizes all net losses and only an accidental 34 KiB dictionary
   scraping a 0.1% win. A 32 MiB block already contains ~2400 files of its own
   history; the dictionary is redundant with it.
2. **A dictionary is a *substitute* for solidity, not an addition to it.** This
   is stated outright by zstd's author (quoted in §2.2) and is exactly what the
   measurements show: the dictionary's gain decays from −16% at 256 KiB units to
   0% at 32 MiB units.
3. **But there is one place where it genuinely pays, and it is narc's
   differentiator:** the *append/edit* path. Data added after creation forms
   small units, and there a dictionary trained from the archive's own content
   gives a **hold-out-validated 21-31% reduction**. That is the recommendation.
4. **Unit size dominates dictionaries as a ratio lever — but narc's small-file
   path is already fine, and the 7-Zip gap is somewhere else entirely.**
   MEASURED: 4 MiB units cost **+50.0%** vs one solid stream, 32 MiB units
   **+4.9%**, and narc's *realized* geometry (blocks hit the 64 MiB hard cap, not
   the 32 MiB target) only **+1.3%**. §6.3's verification pass localizes the
   whole 7-Zip gap on this corpus to the **large-file path**: 12 files ≥ 1 MiB
   cost 5.495 MiB compressed one-by-one versus 2.229 MiB solid, a 3.27 MiB loss
   against a ~3.1 MiB total gap. Small-file block geometry and dictionaries are
   both dead ends for that gap.
5. **Alphabet/representation transforms are almost entirely folklore in front of
   a modern codec — with exactly one real exception.** Undoing base64 is worth
   **−28%**; everything else measured between −1% and *catastrophically worse*
   (MTF: **+202%**). The one honourable mention is sparse-alphabet packing,
   which is worth 4-8% to LZMA2 but 0.4% to zstd.

---

## 1. Method and anchors

### 1.1 Corpus

narc's own test corpus, restricted to files `< 256 KiB`, sorted by extension
then path — narc's own solid-block ordering.

> **CORRECTED (verification pass).** 256 KiB is `Tier::solid_max_file()` for
> **fast and normal**. This report is about the **max** tier, where
> `analyze.rs:59-64` returns **1 MiB**. The ROADMAP still says
> "files < SOLID_MAX_FILE (256 KiB) … <= SOLID_BLOCK (8 MiB)", which is stale
> since commit 871403e; the author took the number from there rather than from
> the code. Measured on `test/corpus`: `<256 KiB` = 5707 files / 74.24 MiB,
> `256 KiB-1 MiB` = **32 files / 12.26 MiB**, `>=1 MiB` = 12 files / 26.91 MiB.
> So the max tier's real solid-block set is **5739 files / 86.50 MiB** and the
> chunked set is **12 files / 26.91 MiB**, not the 44 / 39.2 MiB claimed below.
> Everything measured here is therefore a 86%-of-the-real-input sample of the
> max-tier solid path. It does not flip any sign (the excluded files are 256 KiB
> to 1 MiB — larger than any dictionary, so they can only *weaken* a
> dictionary's share), but §6.3's gap arithmetic is built on the wrong split and
> is corrected there.

| property | value |
|---|---|
| files | 5707 |
| raw bytes | 77,846,354 (74.24 MiB) |
| mean file size | 13.3 KiB |
| largest groups | `.cc` 15.7 MiB, `.rs` 14.5 MiB, `.s` 10.8 MiB, `.h` 9.0 MiB, `.c` 7.1 MiB |

The other 44 files in the tree (39.2 MiB) are ≥ 256 KiB; 32 of them (12.26 MiB)
still go into solid blocks at the max tier and only 12 (26.91 MiB) go down
narc's chunked path. They are out of scope here because a dictionary cannot help
a file that is already far larger than the dictionary.

Scripts (all in the gitignored playground): `test/dict-bench.py` (A/B/C),
`test/dict-bench2.py` (D), `test/dict-bench3.py` (F), `test/dict-decisive.py`
(G), `test/dict-why.py`, `test/dict-why2.py`, `test/dict-traincost.py`,
`test/alphabet-bench.py`, `test/7z-anchor.py`.

### 1.2 Tools

zstd 1.5.7 via `python-zstandard` 0.25.0; liblzma via CPython 3.14 `lzma`
(`FORMAT_RAW`, LZMA2 preset `9|PRESET_EXTREME`); brotli 1.2.0; 7-Zip 26.02.

### 1.3 LZMA2 preset-dictionary proxy

liblzma exposes `preset_dict` but CPython's `lzma` module does **not** (verified:
`ValueError: Invalid filter specifier for LZMA filter`). So LZMA2 preset
dictionaries are measured as

```
size(dict, data) = C(dict || data) − C(dict)
```

The encoder state after consuming `dict` *is* the preset-dictionary state, so
this is faithful to within the flush of the reference stream (a few bytes). It
is also the semantically correct definition of a preset dictionary, and §3.4
shows it differs sharply from what zstd's *dictionary API* does.

> **CORRECTED — the proxy is not neutral, it is optimistic.** Read
> `lzma-rust2` 0.19 `lz/lz_encoder.rs:255-267`: `set_preset_dict` copies the
> dictionary into the window and calls `match_finder.skip()`. It primes the **LZ
> window only** — the range coder's probability model (literal contexts, match
> length, rep distances) starts from its initialised state. The proxy
> `C(dict||data) − C(dict)` primes **both**, because `dict` is actually encoded.
> So a real `preset_dict` gains **≤** the proxy. That makes §2.6's negative
> result *stronger* (the real API can only do worse), but it means §2.7's
> "training is worth 2-3× a raw sample" and any other positive LZMA2 number here
> is an upper bound. `crates/narc-core/examples/dict_probe.rs` already drives the
> real `preset_dict` API through `lzma-rust2`; run it before trusting any
> positive LZMA2 dictionary number.

### 1.4 Anchors — the numbers narc must beat

MEASURED, same 5707 files. 7-Zip figures include stored path metadata (~5707
paths); the raw-stream figures do not, which is worth ~0.14 MiB.

| configuration | size | % of raw | time |
|---|---|---|---|
| 7z `-mx9` (LZMA2, 64 MiB dict, solid) | **5.684 MiB** | **7.66%** | 16.5 s |
| 7z `-mx9 -md=256m` | 5.684 MiB | 7.66% | 17.5 s |
| 7z `-mx9 -m0=PPMd:o=16:mem=256m` (solid) | 8.120 MiB | 10.94% | 9.9 s |
| 7z `-mx9 -ms=off` (non-solid) | 15.563 MiB | 20.96% | 11.6 s |
| LZMA2 `-9e` raw, one solid stream | 5.540 MiB | 7.46% | — |
| LZMA2 `-9e` raw, 32 MiB blocks (narc max) | 5.812 MiB | 7.83% | — |
| zstd-19, 64 MiB blocks (2 blocks) | 5.943 MiB | 8.00% | — |
| zstd-19, one stream | **5.816 MiB** | **7.83%** | — |
| zstd-19, per file | 15.743 MiB | 21.21% | 34 s |

The 7-Zip rows were **re-run during the verification pass** and reproduce to the
byte (5.684 / 5.684 / 8.120 / 15.563 MiB); times came out 14.5 / 15.0 / 7.6 /
10.4 s, i.e. the times above are ~15% pessimistic but the sizes are solid.
CORRECTED: the row originally labelled "zstd-19, one stream" was the 64 MiB /
2-block measurement (`test/dict-B.log`); the true single-stream figure is the
128 MiB row of that same log, 6,098,483 B = 5.816 MiB = 7.83%.

Two things to note immediately:

- **Non-solid 7-Zip (20.96%) ≈ per-file zstd-19 (21.21%).** Solidity is worth
  **2.7×** on this corpus. That is the size of the prize.
- **PPMd is 43% worse than LZMA2 on this source tree** (10.94% vs 7.66%).
  narc's max tier runs a per-unit LZMA2/PPMd tournament and keeps the smaller,
  so this should not hurt — but it is a reminder that PPMd's win in the ROADMAP
  (`-24%` on prose) does not transfer to source code.

---

## 2. Part (a) — Trained dictionaries

### 2.1 What a zstd dictionary is

Per [RFC 8878 §5](https://www.rfc-editor.org/rfc/rfc8878.html) (which obsoletes
RFC 8478 and keeps the dictionary layout identical), a formatted dictionary is:

```
Magic_Number (4B, 0xEC30A437 LE) | Dictionary_ID (4B LE, ≠0) | Entropy_Tables | Content
```

- **Content** is the tail and is the important part: it acts as virtual history
  that match offsets can reach into. It is *not* copied to the output.
- **Entropy_Tables** are Huffman tables for literals plus FSE tables for
  offsets / match lengths / literal lengths, plus 3 seed values for the
  repeat-offset codes.
- **Any buffer ≥ 8 bytes is a valid "raw content" dictionary** — no header, no
  ID, no tables. If the leading 4 bytes are not the magic, the whole buffer is
  treated as content.

MEASURED — how the two halves split the gain (identical bytes, only the
interpretation flipped between `DICT_TYPE_FULLDICT` and `DICT_TYPE_RAWCONTENT`,
110 KiB dictionary, zstd-19, per file):

| | size | gain vs no dict |
|---|---|---|
| no dictionary | 15.743 MiB | — |
| full dictionary (tables + content) | 12.415 MiB | 21.1% |
| same bytes as raw content | 12.538 MiB | 20.4% |
| **entropy tables' share** | | **0.78 pp** |
| on `<4 KiB` files only: tables' share | | **3.24 pp** |

**Verdict:** the entropy tables are worth ~4% of the dictionary's total value
overall, ~15% of it on sub-4-KiB files. The content is what matters. This is
good news for narc: a **raw-content dictionary is codec-agnostic**, so one
stored blob can serve zstd and LZMA2 alike.

> **CORRECTED — not PPMd.** The original text said the blob serves "zstd, LZMA2
> and PPMd alike". Verified against narc's actual dependency: `ppmd-rust` 1.4.0
> exposes only `Ppmd7Encoder::new(writer, order, mem_size)` /
> `Ppmd7Decoder::new(reader, order, mem_size)` — **there is no priming or
> preset-dictionary API on either side.** The only way to prime a PPMd model is
> to encode `dict || data` and have the decoder decode-and-discard the prefix,
> which puts the dictionary's *compressed bytes inside every unit* and so
> destroys the entire "stored once, amortised" premise. Consequence for §6.2: at
> the max tier the LZMA2/PPMd tournament would be asymmetric — only the LZMA2 arm
> can use a dictionary — so a dictionary can *change which codec wins* and the
> per-unit record must capture `dict_id` and codec together (it does), but the
> analyzer must not assume the dictionary is available to whichever arm wins.

### 2.2 The mechanism, and why it caps out — primary source

Two statements settle the design question. From zstd's own README, [The case for
Small Data compression](https://github.com/facebook/zstd#the-case-for-small-data-compression):

> "Dictionary gains are mostly effective in the first few KB. Then, the
> compression algorithm will gradually use previously decoded content to better
> compress the rest of the file."

And Yann Collet (zstd's author) in
[facebook/zstd#3783](https://github.com/facebook/zstd/issues/3783):

> "Compressing large data should always win, though by how much depends on the
> data […] Dictionary will help to close the gap, but typically cannot overtake
> the large data scenario, especially when the cost of the dictionary (its size)
> is taken in consideration."

> "Dictionary compression is for scenarios where one cannot concatenate these
> similar records together, for example because the records must be sent
> immediately and can't wait inside a batch queue."

**narc can concatenate.** It already does — that is what a solid block is.
So narc is not in the regime dictionaries were designed for. Everything below
is the quantification of that sentence.

### 2.3 Verifying the "10× on small files" claim

CORRECTED — **that quote is not in zstd's documentation.** The verification pass
checked `programs/zstd.1.md` in `facebook/zstd@dev` and the rendered
`zstd(1)` on manpages.debian.org: neither contains "10% at 64 KB" or "5×", only
the unquantified "dictionary compression … greatly improves efficiency on small
files and messages". Treat the 10%/5× curve as folklore with no upstream source;
what *is* documented and verified is the README's ~10× on *github-users* and
`zdict.h`'s "The larger a file is, the less benefit a dictionary will have."
The README's ~10× figure comes from the *github-users* sample set:
roughly 10K records of about 1 KB each, scraped from GitHub's public API — i.e.
near-duplicate JSON records.

MEASURED on real source files (110 KiB dictionary, zstd-19, per file):

| bucket | files | raw | no dict | + dict | gain | claim |
|---|---|---|---|---|---|---|
| `<1 KiB` | 934 | 0.37 MiB | 0.211 MiB | 0.131 MiB | **38.0%** (1.6×) | folklore "5×" |
| `1-4 KiB` | 1569 | 3.43 MiB | 1.443 MiB | 0.828 MiB | **42.6%** (1.7×) | — |
| `4-16 KiB` | 1968 | 16.18 MiB | 4.693 MiB | 3.274 MiB | **30.2%** | — |
| `16-64 KiB` | 1017 | 31.35 MiB | 6.307 MiB | 5.336 MiB | **15.4%** | — |
| `64-256 KiB` | 219 | 22.92 MiB | 3.090 MiB | 2.846 MiB | **7.9%** | "10% at 64 KB" |

**Verdict: the *shape* zstd documents reproduces exactly** — the gain rises
monotonically as files shrink, from 7.9% in the 64-256 KiB bucket to 38% under
1 KiB, which is `zdict.h`'s "the larger a file is, the less benefit" made
quantitative. **The magnitude of the folklore figures does not reproduce:**
1.6× at the small end, not 5×. The 5-10× figures require *near-duplicate*
records, and
Collet confirms that for such data plain concatenation would exceed 10× anyway.
Treat "10× on small files" as true-but-inapplicable marketing for narc's data.

### 2.4 Dictionary size vs gain — and a trap

MEASURED, per-file zstd-19. `self` = dictionary trained on the same 5707 files
(best case: a dictionary built from the archive's own content at create time).
`holdout` = deterministic 50/50 split by path hash, trained on one half and
measured on the other (the honest case for data appended *later*).

| requested | actual dict | self (% of no-dict) | hold-out (% of no-dict) | train time |
|---|---|---|---|---|
| 4 KiB | 4 KiB | 91.1% | 93.4% | 30.6 s |
| 16 KiB | 16 KiB | 85.9% | 85.8% | 78.3 s |
| 64 KiB | 64 KiB | 80.5% | 80.7% | 120.8 s |
| 110 KiB (zstd default) | 110 KiB | 78.9% | 79.2% | 131.3 s |
| 256 KiB | 256 KiB | 76.3% | 76.7% | 144.8 s |
| **1 MiB** | 1 MiB | **67.4%** | **69.2%** | 217.7 s |
| 4 MiB | **23 KiB (!)** | 91.6% | 91.2% | 278.3 s |
| 16 MiB | **18 KiB (!)** | 91.9% | 91.2% | 173.5 s |

Three findings:

- **Hold-out ≈ self-trained** (within 2 pp at every size). The dictionary
  generalizes to unseen files of the same kind. This is the single most
  encouraging result in the study and it is what makes the append-path
  recommendation (§6.1) viable.
- **Gain keeps improving well past zstd's 110 KiB default**, up to at least
  1 MiB (−32.6%). `zdict.h` says "A reasonable dictionary size, the
  `dictBufferCapacity`, is about 100KB"
  ([zdict.h](https://github.com/facebook/zstd/blob/dev/lib/zdict.h), quote
  verified); on this corpus that recommendation is too conservative by ~10×.
  **CORRECTED:** the report originally also cited "we don't expect dictionary
  compression to be effective past 100KB" as being about dictionary size. It is
  not — in `zdict.h` that sentence is preceded by "The larger a file is, the less
  benefit a dictionary will have", so the 100KB refers to the size of the **file
  being compressed**, not the dictionary. §2.3's bucket table *confirms* that
  statement rather than contradicting it.
- **GOTCHA — asking for a dictionary bigger than the trainer can fill silently
  collapses it.** Requesting 4 MiB yielded **23 KiB**; requesting 16 MiB yielded
  **18 KiB**, and both were *worse than the 16 KiB dictionary*. COVER stops
  emitting once it runs out of segments that score above threshold. **Always
  check the returned dictionary's actual length**, never assume `maxdict` was
  honoured.

### 2.5 The dilution effect — dictionary gain vs unit size

This is the heart of the matter. MEASURED, same corpus concatenated into blocks
of increasing size, zstd-19, with the dictionary attached via zstd's
dictionary API:

| block size | blocks | no dict | % of raw | + 110 KiB dict | + 1 MiB dict |
|---|---|---|---|---|---|
| 256 KiB | 333 | 11.184 MiB | 15.06% | 93.9% | **83.7%** |
| 1 MiB | 77 | 10.028 MiB | 13.51% | 99.7% | 88.5% |
| 4 MiB | 19 | 8.623 MiB | 11.61% | **105.2%** | 92.3% |
| 8 MiB | 10 | 7.692 MiB | 10.36% | **108.4%** | 95.0% |
| 16 MiB | 5 | 6.872 MiB | 9.26% | **112.6%** | 98.2% |
| 32 MiB | 3 | 6.097 MiB | 8.21% | **115.3%** | **100.5%** |
| 64 MiB | 2 | 5.943 MiB | 8.00% | **116.7%** | 101.7% |
| 128 MiB (one stream) | 1 | 5.816 MiB | 7.83% | 106.6% | 98.5% |

CORRECTED: the 128 MiB row was **omitted** from the original table (it is in
`test/dict-B.log`). It matters because it breaks the monotone story: at one
single stream the 110 KiB dictionary's penalty *drops* from 116.7% to 106.6% and
the 1 MiB dictionary turns into a 1.5% *gain*. That is a property of zstd's
`dictMatchState` path interacting with block count, not evidence that
dictionaries help big units — §3.2's prepend experiment puts the honest value of
a shared prefix on a 32 MiB block at 0.6% gross.

Read the columns, not the rows: **the dictionary's benefit decays to nothing as
the unit grows, and the two knobs are substitutes.** Going from 256 KiB units to
32 MiB units buys 45% (15.06% → 8.21%); a dictionary buys at most 16% and only
at the small end. You cannot have both, because they are competing for the same
redundancy.

Note also the values **above 100%**: with zstd's dictionary API a 110 KiB
dictionary makes a 32 MiB block **15% larger**. §3.4 dissects that.

### 2.6 THE DECISIVE TABLE — LZMA2, 32 MiB blocks, storage included

narc's max tier compresses 32 MiB solid blocks with LZMA2/PPMd. This is the
configuration that matters. The dictionary must be stored in the archive, so its
compressed size is charged against it.

MEASURED, LZMA2 `-9e`, 3 blocks of ≤ 32 MiB, preset-dict proxy (§1.3):

| dictionary | requested | actual | blocks (B) | dict stored (B) | **TOTAL (B)** | vs no dict | vs one solid stream |
|---|---|---|---|---|---|---|---|
| none | — | — | 6,094,630 | 0 | **6,094,630** | 100.0% | 104.9% |
| trained | 64 KiB | 64 KiB | 6,069,811 | 26,527 | **6,096,338** | **100.0%** | 105.0% |
| trained | 256 KiB | 256 KiB | 6,007,758 | 96,065 | **6,103,823** | **100.2%** | 105.1% |
| trained | 1 MiB | 1 MiB | 5,792,939 | 351,380 | **6,144,319** | **100.8%** | 105.8% |
| trained | 4 MiB | 183 KiB | 6,057,678 | 40,571 | **6,098,249** | **100.1%** | 105.0% |
| trained | 16 MiB | **34 KiB** | 6,082,621 | 6,790 | **6,089,411** | **99.9%** | 104.8% |
| — | one solid stream, 64 MiB dict | | | | **5,808,715** | 95.3% | 100.0% |

**CORRECTED — two errors in the original version of this table, both found by
re-reading `test/dict-G.log` and `test/dict-decisive.py`:**

1. The last row was **omitted**. The sweep requests
   `(64 K, 256 K, 1 M, 4 M, 16 M)`; the 4 MiB request collapsed to 183 KiB and
   the 16 MiB request collapsed to **34 KiB**, and that 34 KiB dictionary is a
   **net win of 5,219 B (99.9%)**. So the original claim "Every row is ≥ 100.0%.
   The dictionary never wins" is **false against the author's own log.**
2. The 183 KiB row was labelled "16 MiB requested"; it was the **4 MiB** request.

**The honest statement:** across a 64 KiB→1 MiB sweep the best net result is
**−0.1% and the worst is +0.8%** — the dictionary is *break-even within noise* at
32 MiB blocks, and the only configuration that is not a loss is an accidental
34 KiB dictionary whose gross gain (0.2%) barely exceeds its own 6.8 KB. That
kills the hypothesis just as thoroughly as "always a loss" would, but the
absolute phrasing has to go. The 1 MiB dictionary saves 301,691 bytes of block
data and costs 351,380 bytes to store — a net loss of ~50 KB. The gross gain
before storage is real but small (−4.9%), and it is entirely eaten by the
dictionary itself.

Amortization does not rescue it at plausible scales. Gross saving scales with
input (~4.9% of compressed output) while dictionary cost is fixed at ~351 KB, so
break-even needs ≈ 7.1 MB of compressed block output ≈ **90 MiB of small files**
— just above where this corpus sits. Above that the dictionary starts
paying, but only ~4-5% at best, and that assumes a 1 MiB dictionary keeps
working as the corpus grows more diverse (it will not: §2.4's hold-out held only
because this corpus is homogeneous source code).

**Meanwhile, simply not fragmenting into blocks is worth 4.9% for free.**

### 2.7 Trained vs. untrained dictionary content

Is COVER training worth it, or would a slab of representative bytes do? MEASURED,
LZMA2 `-9e`, 1 MiB dictionary, preset-dict proxy. "raw sample" = every 6th file
concatenated, tail-truncated to 1 MiB (LZMA keeps the *last* `dict_size` bytes).

| block size | no dict | + COVER-trained 1 MiB | + raw sample 1 MiB |
|---|---|---|---|
| 1 MiB | 10,132,661 | 8,979,690 (**88.6%**) | 9,720,386 (95.9%) |
| 4 MiB | 8,713,037 | 7,967,710 (**91.4%**) | 8,469,953 (97.2%) |
| 16 MiB | 6,937,633 | 6,502,109 (**93.7%**) | 6,815,324 (98.2%) |
| 32 MiB | 6,094,630 | 5,774,510 (**94.7%**) | 5,980,301 (98.1%) |

**Training is worth ~2-3× the raw sample's gain, and it transfers to LZMA2
even though the dictionary was trained by zstd's COVER.** That confirms §2.1:
the content section is codec-agnostic. If dictionaries are used at all, train
them — do not just staple sample bytes together.

### 2.8 Per-extension vs one global dictionary

zstd's README is explicit: "there is no *universal dictionary*", recommending
"one dictionary per type of data". MEASURED, per-file zstd-19, 110 KiB per
dictionary, eight largest extension groups:

| ext | files | no dict | one global dict | per-ext dict | per-ext extra gain |
|---|---|---|---|---|---|
| `.cc` | 1116 | 3892 K | 2878 K | 2711 K | 5.82% |
| `.rs` | 1379 | 2982 K | 2614 K | 2407 K | 7.93% |
| `.s` | 284 | 1187 K | 1092 K | 936 K | 14.34% |
| `.h` | 644 | 2263 K | 1652 K | 1488 K | 9.90% |
| `.c` | 656 | 2096 K | 1321 K | 1111 K | 15.87% |
| `.asm` | 87 | 464 K | 421 K | 362 K | 13.97% |
| `.pl` | 122 | 779 K | 677 K | 562 K | 16.96% |
| `.go` | 234 | 796 K | 646 K | 549 K | 15.02% |
| **TOTAL** | | 14.120 MiB | 11.036 MiB | **9.888 MiB** | **10.40%** |

Storing 8 dictionaries instead of 1 costs 770 KiB extra but saves 1176 KiB →
**net −405 KiB**. Per-group dictionaries are the right structure *if* you build
dictionaries at all. narc already sorts small files by extension, so the
grouping exists for free.

### 2.9 Brotli's built-in dictionary — a free partial win

Brotli's format embeds a ~122 KiB static text dictionary plus ~120 word
transforms. It costs zero storage and zero training because it is *in the
format*. MEASURED, per file:

| bucket | zstd-19 | zstd-19 + trained 110 KiB dict | brotli q11 (free built-in dict) | brotli vs zstd |
|---|---|---|---|---|
| `<1 KiB` | 0.211 MiB | 0.128 MiB | 0.179 MiB | 84.8% |
| `1-4 KiB` | 1.443 MiB | 0.792 MiB | 1.190 MiB | 82.5% |
| `4-16 KiB` | 4.693 MiB | 3.198 MiB | 4.008 MiB | 85.4% |
| `16-64 KiB` | 6.307 MiB | 5.298 MiB | 5.627 MiB | 89.2% |
| `64-256 KiB` | 3.090 MiB | 2.838 MiB | 2.842 MiB | 92.0% |
| **TOTAL** | 15.743 MiB | **12.253 MiB (77.8%)** | **13.846 MiB (87.9%)** | 87.9% |

Brotli's free dictionary captures **roughly half** of what a corpus-trained
dictionary achieves (−12.1% vs −22.2%), for zero storage, zero training and zero
format-versioning risk. It is a legitimate cheap option for narc's *small
appended text units* — but note brotli loses to LZMA2/PPMd on large units, so it
would be a niche fourth codec, and Python's `brotli` module exposes no custom
dictionary API (custom dictionaries need brotli ≥1.1.0 C API or CLI).

### 2.10 LZMA2 preset dictionaries: who exposes them

| implementation | encoder | decoder | container carries it? |
|---|---|---|---|
| liblzma (xz-utils) | yes (`lzma_options_lzma.preset_dict`) | raw only | **no** — `.xz`/`.lzma` cannot decode with a preset dict |
| `xz` CLI | **no option at all** | — | — |
| 7-Zip / `.7z` | not exposed | not exposed | **no** |
| CPython `lzma` | **no** (verified: rejects `preset_dict`) | no | — |
| **`lzma-rust2` 0.19 (narc's crate)** | **yes** | **yes** | narc's own container — so yes |
| zstd | yes | yes | out-of-band, identified by `Dictionary_ID` |
| brotli ≥ 1.1.0 | yes | yes | out-of-band (RFC 9842 for HTTP) |

liblzma's own documentation is blunt: preset dictionaries "should be used only
in special situations. For now, it works correctly only with raw encoding and
decoding", because none of liblzma's container formats allow a preset dictionary
when decoding. Upstream's guidance is to use raw LZMA2 streams and handle
dictionary identity yourself
([liblzma docs](https://tukaani.org/xz/liblzma-api/structlzma__options__lzma.html),
[xz-utils discussion](https://sourceforge.net/p/lzmautils/discussion/708858/thread/e40fbf99/)).

**Good news for narc, verified by reading the crate source** (re-verified in the
verification pass against the vendored `lzma-rust2-0.19.0` in the cargo
registry — the exact version `Cargo.toml` pins; **Apache-2.0**, upstream
`hasenbanck/lzma-rust2`, released 2026-08-16 and actively maintained, with
releases 0.16.x→0.19.0 in the last three months): `lzma-rust2` 0.19
supports `preset_dict` on both sides —
`enc/lzma2_writer.rs:31` (`pub preset_dict: Option<Vec<u8>>`), applied at
`:227-228` via `lzma.lz.set_preset_dict(...)`; and `lzma2_reader.rs:95-97` /
`lzma_reader.rs:98,166-182` on the decode side. narc writes raw LZMA2 chunks in
its own container, so the `.xz`/`.7z` limitation does not apply.

Two gotchas found in that source:

- `lz/lz_encoder.rs:263-265` keeps only the **last** `min(preset_dict.len(),
  dict_size)` bytes: `copy_size = preset_dict.len().min(dict_size)`, `offset =
  preset_dict.len() - copy_size`. So **put the most valuable content at the END
  of the dictionary**, and set `dict_size ≥ preset_dict_size + unit_size` so the
  dictionary is never evicted mid-unit. Upstream gives the same two rules.
- `enc/lzma2_writer_mt.rs:76` does `single_chunk_options.lzma_options.preset_dict
  = None` — **the multi-threaded LZMA2 writer drops the preset dictionary.**
  narc parallelizes across chunks itself, so it should use the single-threaded
  writer anyway, but this would silently produce larger output if the MT writer
  were used.

Also relevant: preset-dictionary encoder setup is not free — the dictionary must
be run through the match finder on every unit. For narc that is 1 MiB of extra
match-finder work per 32 MiB block (tolerable) but 1 MiB per 13 KiB file if
applied per-file (catastrophic — a 80× overhead).

### 2.11 Interaction with dedup — a non-issue for narc, and the real hazard

The question was: "what happens to dedup when chunks are compressed against a
shared dictionary?"

**For narc: nothing.** ROADMAP invariant — `chunk hash = blake3(uncompressed)
[..16]`, and "Chunk hash covers the ORIGINAL bytes, so dedup and integrity are
filter-independent". A dictionary changes only the compressed representation, so
dedup and integrity are untouched. This is a genuine architectural advantage
over systems that hash post-compression.

This is also why restic/borg were never affected: they chunk *then* compress.
From [restic#3775](https://github.com/restic/restic/issues/3775), maintainer
`greatroar`:

> "zstd's dictionary mode makes it start with a different initial state than its
> empty state […] Note that zstd still performs 'adaptive' compression even when
> a dictionary is used: it should only make a difference at the start of each
> chunk, say the first few KiB."

That thread is worth knowing in full because **restic's maintainers explained
why the feature is unnecessary for a chunk-then-compress store, and the request
was withdrawn** — CORRECTED attribution: the closing line "So no dictionaries or
other additional options necessary" is the **requester's** (`JsBergbau`), not a
maintainer's decision, and no restic design document rejects dictionaries. What
is verifiable is the architecture: restic chunks, dedups, *then* zstd-compresses
per blob, so a dictionary has nothing to fix. The requester had conflated
dictionaries with gzip's `--rsyncable`
(state *resets*, which aid dedup) — they are unrelated mechanisms. Borg
likewise compresses per chunk with an `auto,lz4` incompressibility heuristic and
no dictionary.

**The real hazard is immutability, not dedup.** A dictionary is decode-critical
state shared by many committed chunks. Consequences:

- A dictionary can **never** be changed or removed once any chunk references it.
  Retraining means adding a *new* dictionary, never editing the old one.
- `compact` must treat dictionaries as reachable from any chunk that names them,
  or it will delete a live dictionary and destroy the archive.
- Corrupting one dictionary loses every unit compressed against it — a blast
  radius far larger than a single chunk. It needs its own hash and ideally
  redundancy.
- RFC 9842 makes the same call for HTTP: dictionaries are identified by SHA-256
  of their content and "must be treated with the same security precautions as
  the content, because a change to the dictionary can result in a change to the
  decompressed content."

### 2.12 Training cost — and how to make it a non-issue

`python-zstandard`'s `train_dictionary` defaults to COVER/fastCover
**optimization mode**, which searches `k` and `d`. That is why §2.4's times are
30-280 s per dictionary. It is also, on this corpus, both slower *and worse*
than fixing the parameters.

MEASURED, 110 KiB dictionary, whole 74.24 MiB corpus, per-file zstd-19:

| training configuration | train time | packed | % of no-dict |
|---|---|---|---|
| optimization mode (library default) | **107.2 s** | 12.415 MiB | 78.9% |
| fastcover `d=8 k=200` | 21.3 s | 12.253 MiB | **77.8%** |
| fastcover `d=8 k=1000` | 20.3 s | 12.289 MiB | 78.1% |
| fastcover `d=6 k=200` | 22.1 s | **12.232 MiB** | **77.7%** |
| fastcover `d=8 k=200 accel=10` | **7.9 s** | 12.300 MiB | 78.1% |

**Optimization mode is 13.6× slower than `accel=10` and produces a worse
dictionary.** Never use it in a pack pipeline.

Then the sizing question — how much corpus does training actually need?
MEASURED, fastcover `d=8 k=200`, dictionary always 110 KiB, always evaluated
against the *full* corpus:

| training sample | files | sample size | train time | packed | % of no-dict |
|---|---|---|---|---|---|
| 2% | 114 | 1.63 MiB | **0.71 s** | 12.622 MiB | 80.2% |
| **5%** | 285 | 3.43 MiB | **1.19 s** | 12.435 MiB | **79.0%** |
| 10% | 570 | 7.28 MiB | 4.44 s | 12.369 MiB | 78.6% |
| 25% | 1426 | 18.49 MiB | 6.13 s | 12.300 MiB | 78.1% |
| 50% | 2853 | 36.34 MiB | 12.53 s | 12.249 MiB | 77.8% |
| 100% | 5707 | 74.24 MiB | 24.71 s | 12.210 MiB | 77.6% |

**Training cost is a solved problem.** A 3.4 MiB sample trained in **1.2 s**
captures 79.0% vs 77.6% for the full corpus at 24.7 s — i.e. **~94% of the
available benefit for 5% of the cost**. Diminishing returns set in hard past
~7 MiB of samples, which is exactly zstd's documented 100× rule (~11 MB of
samples for a 110 KB dictionary).

Other documented rules worth honouring: **> 100 samples**; only the **first
128 KiB** of each large sample is used; training fails if most samples are
< 8 bytes; `ZDICT_trainFromBuffer` uses ~6 MB, though COVER has been reported at
~11× the sample size (fastCover is the cheaper default).

**Conclusion:** at ~1-2 s per class-dictionary from a small sample, training is
cheap enough to run at pack time. It is *not* the obstacle. §2.6 is the
obstacle.

---

## 3. Why zstd's dictionary API *hurts* large blocks

§2.5 showed a 110 KiB dictionary inflating a 32 MiB block by 15%. That is
surprising enough to need a cause, because if it were an artifact the whole
table would be wrong.

### 3.1 Not the entropy tables

MEASURED — identical bytes, only `dict_type` flipped:

| block | no dict | FULLDICT | RAWCONTENT |
|---|---|---|---|
| 1 MiB | 10,515,423 | 99.5% | 99.5% |
| 4 MiB | 9,041,817 | 105.0% | 105.0% |
| 32 MiB | 6,783,273 | 113.0% | 113.0% |

Byte-for-byte identical. Not the tables.

### 3.2 Not a window_log artifact, not a CDict parameter override

MEASURED — with an explicit `window_log` sized to the block and with the library
default, results are identical to the digit; and pre-computing the CDict with
the *same* parameters used for the data changes nothing:

| block | no dict | dict API | CDict precomputed with same params | **dictionary literally prepended** |
|---|---|---|---|---|
| 4 MiB (`wl=23`) | 9,041,817 | 9,493,658 (105.0%) | 9,493,658 (105.0%) | **8,896,402 (98.4%)** |
| 32 MiB (`wl=26`) | 6,393,487 | 7,362,986 (115.2%) | 7,362,986 (115.2%) | **6,356,544 (99.4%)** |

### 3.3 The answer

**The penalty is inherent to zstd's dictionary code path** (`dictMatchState`
match search), not to the idea of a shared prefix. When the same dictionary bytes
are simply *prepended* to the block and compressed as one ordinary stream — the
true semantics of a preset dictionary — the result is **98.4% / 99.4%**, a
small gain instead of a large loss.

### 3.4 Two consequences

1. **Do not attach a zstd dictionary to a multi-MiB unit.** It is not a
   micro-regression; it is up to −17%. zstd's dictionary path is tuned for the
   small-frame case it documents.
2. **The honest value of a 110 KiB shared prefix on a 32 MiB block is ~0.6%**,
   which independently confirms §2.6 from a different direction. The LZMA2
   proxy numbers in §2.6-2.7 use prepend semantics and are therefore the fair
   ones.

---

## 4. Part (b) — Alphabet and representation transforms

All MEASURED against the same codec on the same bytes, at settings narc uses.
A transform counts only if it beats the untransformed baseline *after* the codec.

### 4.1 base64 and hex — the one real win, and its limit

Payload: a real 1 MiB `.exe` from the corpus.

| variant | input bytes | zstd-19 | vs raw | LZMA2 `-9e` | vs raw |
|---|---|---|---|---|---|
| raw payload (lower bound) | 1,048,576 | 265,867 | base | 249,057 | base |
| base64 text, compressed as-is | 1,398,104 | 369,281 | **138.9%** | 334,266 | **134.2%** |
| base64 → decode → compress | 1,048,576 | 265,867 | 100.0% | 249,057 | 100.0% |
| hex text, compressed as-is | 2,097,152 | 276,483 | **104.0%** | 250,871 | **100.7%** |

**Undoing base64 is worth −28% (zstd) / −25% (LZMA2).** Undoing hex is worth
−4% / −0.7% — essentially nothing.

The difference is **byte alignment**, and it is the whole lesson. Hex is 2
characters per byte: nibble-aligned, so every repeat in the payload is still a
byte-aligned repeat in the text, and the entropy coder recovers the 4 wasted
bits almost perfectly. Base64 is 4 characters per 3 bytes: a repeat starting at
a different residue mod 3 encodes to *different characters*, so LZ match-finding
is destroyed.

**Actionable:** a base64 detector-and-decoder is a real, cheap filter worth ~28%
on base64 payloads (PEM certificates, `.eml` attachments, data-URIs, some JSON
blobs). A hex decoder is not worth writing. Both must be exactly reversible
including line breaks, padding and whitespace — the round-trip metadata can
easily cost more than the gain on small inputs.

> **CORRECTED — narc's architecture limits where this can fire.** A `Filter` is
> a per-*unit* property, and at the max tier every file < 1 MiB is concatenated
> into a solid block whose plan is chosen **once for the whole block** from a
> 64 KiB head sample (`archive.rs` `flush_solid` → `analyze::plan(&buf[..
> HEAD_SAMPLE], tier)`). So a `.pem` sitting inside a solid block cannot get its
> own base64 filter, and applying the filter to a whole mixed block is not
> reversible. The filter is therefore reachable only for standalone chunked units
> — files ≥ 1 MiB that are predominantly base64 — plus any future per-member
> filtering. The measurement above (a 1 MiB base64 blob as its own unit) is
> exactly that case, so −28% is real *for that case*; it is not a −28% on
> archives that merely contain some base64. Sub-range base64 detection inside a
> mixed unit is a different and much larger feature.

### 4.2 Sparse alphabet packing

| variant | input | zstd-19 | vs | LZMA2 `-9e` | vs |
|---|---|---|---|---|---|
| random ACGT, 2 MiB | 2,097,152 | 526,337 | base | 567,431 | base |
| → 2-bit packed (4 symbols) | 524,293 | 524,314 | **99.6%** | 524,321 | **92.4%** |
| ACGT with repeats | 2,219,306 | 21,151 | base | 20,678 | base |
| → 2-bit packed | 554,832 | 20,215 | 95.6% | 19,616 | 94.9% |
| 16-value table, 2 MiB | 2,097,152 | 1,052,424 | base | 1,091,458 | base |
| → 4-bit packed | 1,048,593 | 1,048,626 | **99.6%** | 1,048,648 | **96.1%** |

Look at the absolute numbers: on random ACGT, zstd-19 produces 526,337 bytes for
2,097,152 symbols = **2.007 bits/symbol** against a theoretical floor of 2.000.
**zstd's FSE entropy coder already reaches the bit-packing bound to within
0.4%.** Same for the 16-value case: 1,052,424 bytes = 4.014 bits/symbol vs a
4.000 floor.

So packing is **folklore for zstd (0.4%)** but **worth 4-8% for LZMA2**, whose
literal coder is weaker on very low-cardinality data (note LZMA2 is *worse* than
zstd on random ACGT: 567,431 vs 526,337 — packing merely repairs that deficit).

Two caveats that mostly kill it anyway: packing destroys byte alignment of
repeats (the ACGT-with-repeats row loses most of the theoretical 4× because the
matches move), and it only applies when a whole unit has ≤ 16 distinct byte
values, which for a general-purpose archiver is rare. The one durable benefit is
**speed**: 4× less input to the codec.

This matches how the field actually does it: htscodecs/CRAM gate `PACK` behind
alphabet cardinality and compression level, and note that "the bit-packing modes
of rANS are not relevant […] due to the cardinality of the data" for ordinary
40-value Illumina qualities — packing is only used on 4-value binned data
([htscodecs BENCHMARKS.md](https://github.com/samtools/htscodecs/blob/master/BENCHMARKS.md)).

### 4.3 MTF, RLE and frequency remapping

| variant | input | zstd-19 | vs | LZMA2 `-9e` | vs |
|---|---|---|---|---|---|
| source text 4 MiB, raw | 4,194,304 | 389,294 | base | 379,007 | base |
| → frequency remap (+256 B table) | 4,194,560 | 389,393 | **100.0%** | 382,006 | **100.8%** |
| → RLE | 3,689,463 | 386,187 | 99.2% | 374,952 | 98.9% |
| source text 512 KiB, raw | 524,288 | 78,637 | base | 76,781 | base |
| → **MTF** | 524,288 | 237,212 | **301.7%** | 227,344 | **296.1%** |
| → frequency remap | 524,544 | 79,020 | 100.5% | 77,554 | 101.0% |
| synthetic log 5.6 MiB, raw | 5,907,440 | 226,539 | base | 159,310 | base |
| → frequency remap | 5,907,696 | 230,677 | 101.8% | 155,499 | **97.6%** |
| → RLE | 5,907,331 | 230,315 | 101.7% | 162,911 | 102.3% |

- **MTF without BWT is a disaster: 3× worse.** MTF only makes sense on
  BWT-clustered output; standalone it converts a low-entropy byte stream into a
  high-entropy rank stream and annihilates LZ matches.
- **Frequency remapping is worth 0.0%.** Exactly nothing for zstd on source
  text. Occasionally −2.4% (LZMA2 on logs), occasionally +1.8% (zstd on logs).
  Noise around zero.
- **RLE is worth ~1%** and sometimes negative. Both codecs already encode runs
  as matches.

### 4.4 Literature agreement — the definitive published numbers

[Kalcher, "Frequency-Ordered Tokenization for Better Text Compression"
(arXiv:2602.22958)](https://arxiv.org/abs/2602.22958) measures exactly this
question at scale. Table I, enwik8 (100 MB), ratios as % of original, lower is
better; "Ours" = BPE tokenization + frequency reordering + varints, including the
0.20% vocabulary overhead:

| compressor | raw | with transform | improvement |
|---|---|---|---|
| zlib-9 | 36.48% | 29.40% | **+7.08 pp** |
| LZMA | 26.38% | 24.69% | **+1.69 pp** |
| zstd-22 | 25.27% | 24.51% | **+0.76 pp** |
| bz2 | 29.01% | 29.07% | **−0.06 pp** |
| **PPMd-16** | **22.83%** | 23.27% | **−0.44 pp** |

Table II ablation (10 MB subset) separates the two stages:

| stage | zlib-9 | zstd-22 | LZMA | PPMd-16 |
|---|---|---|---|---|
| tokenization gain | +3.64 | **−0.38** | **−0.26** | **−1.64** |
| reordering gain | +3.67 | +1.73 | +1.76 | +0.14 |

**This is the single most important citation in part (b), for three reasons:**

1. **The gain is inversely proportional to codec strength** — 7.08 pp for zlib,
   0.76 pp for zstd, *negative* for PPMd. The authors' explanation: "zstd uses
   finite-state entropy (tANS), which already adapts to skewed byte
   distributions, and LZMA uses range coding with sophisticated context
   modeling, so both capture some of the structure our preprocessing provides."
2. **PPMd-16 raw (22.83%) is the best practical compressor in the table, and the
   transform makes it worse.** narc's max tier routes text to PPMd. So this
   entire family of transforms is a *negative* for narc's strongest text path.
3. **BWT-based bz2 gains nothing**, because "BWT already groups similar contexts
   and assigns small integers via move-to-front" — the transform is redundant
   with what the codec does natively.

Historical context for the word-based variants: Kruse & Mukherjee reached up to
20% for **bzip2**, and Skibiński et al.'s Word Replacing Transform 3-14%, both
against weak or BWT back-ends and both requiring an *external language-specific*
dictionary. XWRT is still used as a text filter inside PAQ variants, so the
family is not dead — but its remaining headroom is against `zlib`-class and
BWT-class back-ends, which narc does not use.

Also worth knowing: Delétang et al. showed tokenization does **not** improve
*neural* compressors either, for the same reason — the model already captures
what tokenization exposes.

### 4.5 Bijective base conversion and ANS with adaptive alphabets

Both were listed in the brief; both should be dismissed, for different reasons.

**Bijective base conversion cannot compress.** A bijection between
representations preserves entropy by definition; the only thing it can recover
is *slack in the representation* — i.e. exactly the sparse-alphabet packing case
in §4.2, where the measurement says a modern entropy coder has already taken it
(2.007 vs 2.000 bits/symbol). There is nothing left for a numeral-system trick
to find. Any scheme claiming otherwise is either doing dictionary/model work
under another name, or is wrong.

**rANS/tANS is a speed technology, not a ratio technology.** Duda's own framing
is "entropy coding combining speed of Huffman coding with compression rate of
arithmetic coding" ([arXiv:1311.2540](https://arxiv.org/abs/1311.2540)); tANS
carries a small, tunable redundancy — CLAIMED, and **not sourced**: the
verification pass confirmed the title and the speed-vs-rate framing in
arXiv:1311.2540 but the specific "~0.01 bits/symbol at 2-4× alphabet size in
states, ~0.001 at 8-16×" figures are not in that paper's abstract and no source
is given for them here. Treat the magnitude as unverified; the *sign* (tANS has
non-zero excess rate vs arithmetic coding) is not in dispute. So swapping an
entropy coder for rANS buys throughput
and *loses* a hair of ratio. narc's codecs already use the right coders (zstd
FSE/tANS + Huffman; LZMA2 and PPMd7 range coding with adaptive models). An
"adaptive alphabet" in rANS means periodically re-transmitting frequency tables
— which is what zstd already does per block, and what PPMd's model does
continuously and better.

### 4.6 Part (b) summary

| transform | zstd-19 | LZMA2 `-9e` | verdict |
|---|---|---|---|
| base64 undo | **−28.0%** | **−25.5%** | **REAL — implement** |
| hex undo | −3.9% | −0.7% | not worth the code |
| sparse packing (≤4 symbols) | −0.4% | **−7.6%** | LZMA2-only, narrow, but a 4× speed win |
| sparse packing (≤16 symbols) | −0.4% | −3.9% | marginal |
| RLE | −0.8% | −1.1% | folklore |
| frequency remap / alphabet reduction | ±0.0% | −0.8%…+0.8% | **folklore** |
| MTF (standalone) | **+202%** | **+196%** | **actively harmful** |
| BWT+MTF (bz2 as proxy) | — | — | dominated by LZMA2 and PPMd on ratio *and* speed |
| word/BPE tokenization + reorder | +0.76 pp (lit.) | +1.69 pp (lit.) | **negative for PPMd (−0.44 pp)** |
| bijective base conversion | — | — | cannot work (information-theoretic) |
| rANS/tANS swap | — | — | speed only; slight ratio *loss* |

narc already ships the two representation transforms that actually pay — **BCJ
x86** (+4.4-5.7% on real `.exe`, per ROADMAP) and **delta**. Those are
*structure-aware* transforms that convert absolute values into predictable
relative ones. That is the category that works; generic alphabet shuffling is
not.

---

## 5. How dictionary-using systems handle versioning

| system | approach | dictionary identity | lifecycle |
|---|---|---|---|
| **restic** | **none — considered and rejected** ([#3775](https://github.com/restic/restic/issues/3775)) | — | chunk-then-compress; per-blob zstd; compression algorithm recorded in the pack header; capability gated by *repository format version* (v2 needs restic ≥ 0.14) |
| **borg** | none | — | per-chunk, `auto,lz4` incompressibility heuristic |
| **zstd (format)** | out-of-band | 32-bit `Dictionary_ID` in the frame header; ≤ 32767 and ≥ 2^31 reserved for public distribution | spec explicitly does *not* define how the dictionary is obtained |
| **RFC 9842 (HTTP)** | out-of-band, negotiated | **SHA-256 of the dictionary content**, `Available-Dictionary` / `Use-As-Dictionary` / `Dictionary-ID` headers | dictionaries are immutable and versioned by distinct URL/hash; `dcb` streams embed a 32-byte dictionary hash |
| **LinkedIn SDCH / femtozip** | trained per deployment | per-deployment id | regeneration cost ~7 h drove them to background retraining every two weeks |

**The lesson is unanimous: nobody mutates a dictionary.** Identity is
content-derived (hash) or an opaque immutable id, and the dictionary is fetched
out of band. Where dictionaries were not needed (restic, borg) the reason was
always the same — *the system already batches, so it does not need one*, which is
narc's situation too.

Note that restic's *format-version* gate is the right model for narc: a
capability that changes what decoders must implement belongs to a version bump,
not to a per-chunk flag alone.

---

## 6. Part (c) — Concrete proposal for narc

### 6.1 What to actually build (and what not to)

**DO NOT** train a dictionary for the max-tier 32 MiB solid-block path. §2.6 is
decisive: net loss at every dictionary size. This is the owner's original idea
and it does not survive measurement.

**DO** consider a dictionary for exactly one case: **units created by `add`
after the archive already exists.** This is narc's differentiator and the one
regime where the numbers are strong:

- A file edited or appended later cannot join an existing committed block
  (committed bytes are never rewritten). It becomes a small unit — the per-file
  regime. **CORRECTED — this holds only for `narc add <archive> <a few files>`.**
  Read `Archive::add` and `AddCtx::add_small_file`: an `add` walks whatever input
  it is given, sorts it, and fills the solid builder from *that* input. If the
  user re-adds the whole tree after editing one file — which is the flow the
  ROADMAP's "~98 KiB" figure measures — the packer rebuilds **full-size** solid
  blocks; unchanged blocks dedup away by unit hash and the one block containing
  the edit is re-stored as a full-size block. That block is in the +0.5%
  dictionary regime of §2.5, not the −21% per-file regime. So the recommendation
  is scoped to *incremental adds of small batches*, not to tree re-saves.
- MEASURED hold-out gain there (§2.4): **−20.8% with a 110 KiB dictionary,
  −30.8% with a 1 MiB dictionary**, on files the dictionary never saw.
- The dictionary is trained **once**, at `create`, from the initial content —
  which is precisely the content later edits will resemble.
- Storage is amortized across all future appends rather than charged against a
  single pack.

~~Worked example on this corpus: the ROADMAP notes a 1-file edit in a 46 MiB tree
grows the archive ~98 KiB. At −21% that becomes ~77 KiB.~~ **CORRECTED — this
example does not support the recommendation.** Three problems: (a) that ~98 KiB
comes from the *tree re-save* flow, which produces a full-size solid block, not a
per-file unit (see above); (b) an unknown and possibly dominant share of it is
the re-written manifest, which no dictionary touches; (c) the −21% is a
**zstd-19** number, while narc's max tier compresses these units with LZMA2/PPMd
— and PPMd cannot use a dictionary at all (§2.1 correction). The per-file gains
in §2.4 are real for zstd; **no measurement in this study establishes the gain
for an LZMA2 preset dictionary on a ~13 KiB unit.** Run
`crates/narc-core/examples/dict_probe.rs` at per-file granularity before
committing to any number here.

This inverts the original framing in a useful way: **the dictionary is not a
ratio feature for archive creation, it is a ratio feature for archive
maintenance.**

### 6.2 If built, the exact design

**Format additions (v0.3):**

1. New manifest section `dicts: Vec<DictRecord>` with
   `{ id: u32, hash: [u8;16], class: ClassId, len: u32, chunk: ChunkIdx }`.
   `id` is dense and assigned on append; `hash` is `blake3(content)[..16]`,
   matching narc's existing convention and giving content-addressed identity as
   RFC 9842 does.
2. Dictionary bodies are stored as **ordinary chunks** in the append-only log —
   so they inherit crash-safety, integrity checking and the existing writer
   path. They are `store`d or zstd-compressed, never dictionary-compressed
   (no recursion).
3. Per-unit: one extra `dict_id` field, `0` = none. It sits next to the existing
   per-chunk codec/filter/param bytes. **Not** a global archive property —
   §6.4 requires per-unit granularity.
4. Bump the format version. Old readers must refuse archives using dictionaries
   rather than mis-decode them (restic's model, §5).

**Invariants (these are the load-bearing part):**

- **Dictionaries are immutable and append-only.** Retraining appends a new
  dictionary with a new `id`; the old one stays forever while any unit
  references it.
- **`compact` must treat a dictionary chunk as reachable from every unit that
  names it.** Today `compact` drops unreferenced chunks — it will destroy the
  archive unless dictionary references are added to the reachability walk. This
  is the single most dangerous line of this design.
- **Dictionaries are raw-content only** (no zstd header), so one blob serves
  zstd and LZMA2 `preset_dict` alike (§2.1, §2.7). **It cannot serve PPMd7** —
  `ppmd-rust` 1.4 has no priming API (§2.1 correction). Any unit whose
  tournament winner is PPMd must be recorded with `dict_id = 0`.
- **Order the content with the most valuable bytes LAST**, because
  `lzma-rust2` keeps the tail (§2.10).
- Set LZMA2 `dict_size ≥ preset_dict_len + unit_size` so the dictionary is not
  evicted mid-unit. **This is a format-compatibility trap, not a tuning knob.**
  `codec.rs:95-98` derives `dict_size` from `unpacked_len` alone and
  `codec.rs:148` calls the *same* function on the decode path — the value is
  never stored. So the moment a preset dictionary changes the required
  `dict_size`, the decoder must derive the identical value from
  `(unpacked_len, dicts[dict_id].len)`, and the derivation must stay conditional
  on `dict_id != 0` or every existing archive becomes undecodable. Knock-on:
  `MAX_CHUNK`/`MAX_STORED_CHUNK` (`archive.rs:33,39`) and the memory model bound
  the decoder window at 32 MiB; a 1 MiB dictionary on a 32 MiB unit raises the
  real window to 33 MiB. Bounded-memory extraction is preserved, but the bound
  changes and the plausibility checks must be updated with it.
- Never use the MT LZMA2 writer with a preset dictionary — it silently drops it
  (§2.10).
- **Never attach the dictionary through zstd's dictionary API on units > ~1 MiB**
  (§3): −17%. For zstd, either use it only on genuinely small units or prepend.

**When and on what to train:**

- Trigger: at `create`, when the input has **> ~2000 small files**. Training is
  cheap enough (~1-2 s per class) that it need not be opt-in — but ship it
  behind `--dict` for the first release so the format change can be validated.
- Sample: **~5% of the class's files, capped around 4-8 MiB**, uniformly
  sampled, each sample capped at 128 KiB (what the trainer uses anyway).
  MEASURED: that captures ~94% of the benefit of training on everything, in
  1.2 s instead of 24.7 s (§2.12).
- Parameters: **fixed `d=8, k=200`** (optionally `accel=10`), **never
  optimization mode** — 13.6× faster and measurably better (§2.12).
- Size: **110 KiB per class** as the default. 1 MiB gives more (−31% vs −21%)
  but costs 351 KB stored and 3× the training time; make it `--dict-size`.
  Always verify the returned length (§2.4's collapse trap).
- Granularity: **one dictionary per content class**, not one per archive —
  measured 10.4% better and net-positive after storage (§2.8). narc's analyzer
  already produces a content class per file, and small files are already sorted
  by extension.

### 6.3 The real lever — where narc is actually losing to 7-Zip

This belongs in this report because it is the honest alternative to the
dictionary, and it is worth far more.

MEASURED cost of fragmenting this corpus, LZMA2 `-9e`:

| unit size | compressed | % of raw | penalty vs one solid stream |
|---|---|---|---|
| 1 MiB | 10,132,661 | 13.02% | **+74.5%** |
| 4 MiB | 8,713,037 | 11.19% | **+50.0%** |
| 16 MiB | 6,937,633 | 8.91% | **+19.4%** |
| 32 MiB | 6,094,630 | 7.83% | **+4.9%** |
| one stream | 5,808,715 | 7.46% | — |

**A 32 MiB block costs only 4.9%. A 4 MiB block costs 50%.** No dictionary
recovers 50%; the best measured dictionary gain anywhere in this study was 16%.

Now the arithmetic that does not add up. From the ROADMAP, on the full 114 MiB
tree: narc max = 12 MiB, 7z `-mx9` = 8.8 MiB. This study measures 7z solid =
5.684 MiB on the `<256 KiB` part, so 7z's remainder ≈ 3.1 MiB. If narc's
small-file part matched plain LZMA2 at 32 MiB blocks (5.812 MiB) and its
remainder matched 7z (~3.1-3.5 MiB), narc would land at **~9.2-9.5 MiB**, not
12 MiB. **~2.5 MiB (≈ 25%) is unexplained by the block-size cap.**

> **CORRECTED — the arithmetic above is wrong and the conclusion it reaches is
> wrong. The verification pass ran the measurement this section asked for, and
> the entire gap is in the BIG-file path, not the small-file/block path.**

**MEASURED (verification pass), realized geometry from `narc info` on the
max-tier archive of this corpus** (`test/diag.narc`, 5751 files, 11.9 MiB):

```
Solid blocks: 2 (min 22.5 MiB, median 64.0 MiB, max 64.0 MiB)
Stored by:    lzma2 8.8 MiB, ppmd7 3.0 MiB
```

Blocks are **not** below the 32 MiB target — they sit at the `2 × target`
hard flush (`archive.rs:1008`), i.e. 64 MiB. Candidate cause (1) below is
refuted before it was tested.

**MEASURED, LZMA2 `-9e`, split at narc's real max-tier cutoff of 1 MiB**
(`test/verify10.py`, `test/verify10b.py`):

| set | configuration | size | vs solid |
|---|---|---|---|
| 5739 small files, 86.50 MiB | one solid stream | 6.531 MiB | — |
| same | **2 blocks, narc's realized geometry** | **6.619 MiB** | **+1.3%** |
| same | 3 blocks of ≤ 32 MiB | 6.829 MiB | +4.6% |
| 12 big files, 26.91 MiB | one solid stream | 2.229 MiB | — |
| same | **each file compressed alone (narc's path)** | **5.495 MiB** | **+146.5%** |
| same | 7z `-mx9` | 2.201 MiB | — |
| whole corpus, 113.41 MiB | 7z `-mx9` | **8.705 MiB** | — |

**The decomposition now closes exactly.** 6.619 + 5.495 = **12.11 MiB** against
narc's actual **11.8 MiB live** — narc is slightly *better* than that floor
(dedup plus PPMd winning some units). 7-Zip's 8.705 MiB = the same small part
(~6.5) plus a big part of only 2.2 MiB. **The gap is 5.495 − 2.229 = 3.27 MiB
of lost cross-file redundancy among the 12 large files, versus a total gap of
~3.1 MiB.** The small-file path — the entire subject of this report — is within
**1.3%** of its own solid ceiling and contributes essentially nothing to the gap.

Why: the 12 files ≥ 1 MiB are 9 `.exe` builds of narc itself plus 3 generated
`.rs` tables. The `.exe`s are near-duplicates of one another. 7-Zip puts them in
one solid stream with a 64 MiB window and collapses them; narc chunks each file
independently, and CDC dedup catches almost nothing because a recompile shifts
bytes throughout. **Caveat: this corpus is narc's own `target/` directory, so
the near-duplicate-binary effect is unusually strong here.** It is a real
phenomenon in build trees and backups, but do not assume a 3.3 MiB prize on
arbitrary data — re-measure on a corpus that is not our own build output.

Candidate causes as originally listed, with verdicts:

1. ~~**Actual block sizes are below the 32 MiB target.**~~ **REFUTED by
   measurement.** They are at 64 MiB, the `2 × target` cap. Realized geometry
   costs +1.3%, not 19-50%. The histogram this section called "the highest-value
   next step in this whole area" has been run and it closed the question in the
   opposite direction.
2. **The max-tier tournament picking PPMd.** Still open, and now the most
   plausible remaining small lever: `narc info` shows **ppmd7 holding 3.0 MiB of
   11.8**. The tournament keeps the smaller *per unit*, so it cannot lose against
   its own alternatives — but confirm PPMd is not winning solid source blocks,
   where 7z PPMd measured 43% worse than LZMA2 (10.94% vs 7.66%).
3. ~~**LZMA2 dictionary pinned to unit size.**~~ **CORRECTED — wrong lead, do not
   spend time on it.** For a unit compressed *independently*, a dictionary larger
   than the unit cannot help: there is no history outside the unit's own bytes
   for the extra window to reach. `codec.rs:95-98` already sets
   `dict_size = unpacked_len` (clamped to `[4 KiB, 64 MiB]`), always ≥ the unit.
   7-Zip's 64 MiB dictionary matters because its *stream* is the whole archive,
   not because of a per-unit setting. Raising narc's per-unit `dict_size` would
   only raise decoder memory.
4. ~~**Per-extension grouping splitting blocks.**~~ **CORRECTED — does not
   happen.** `add_small_file` (`archive.rs:966-1012`) flushes only on the
   content-defined cut and the `2 × target` cap; extension is a *sort key*
   (`solid_group_key`), not a flush boundary. Blocks straddle extension groups
   freely.

**Revised recommendation: the lever is cross-file solidity for LARGE similar
files.** Options worth measuring, in cost order: group large files of the same
content class into shared solid units the way small files already are; or raise
the chunked path's effective window so consecutive similar files share history.
Both trade directly against cheap edits — that is the real design tension in
narc, and it is a *different* tension from the one this report set out to study.
Neither dictionaries (≤5%, plus a format change, an immutability invariant and a
`compact` hazard) nor small-file block geometry (1.3% and already spent) is the
answer.

### 6.4 Append-only interaction — the questions the brief asked

**"A dictionary cannot change without invalidating old chunks — how do
restic/borg/zstd-based systems handle this?"**

They avoid the problem entirely (restic and borg use no dictionary; zstd pushes
it out of band). Nobody solved it, because nobody with an immutable chunk store
needed to. For narc the answer is the per-unit `dict_id` in §6.2: **the
dictionary set only ever grows, and each unit permanently records which member
it used.** Old chunks stay valid because their dictionary is never touched.

**New files arriving later.** Nothing forces a new unit to use the newest
dictionary. Concretely:

- New small units use the current dictionary for their class; if none exists,
  `dict_id = 0` and they compress standalone. No failure mode.
- The dictionary can drift out of relevance as the archive's content changes.
  Detect it cheaply: track a rolling ratio of dictionary-compressed vs
  standalone output on a sample. When the gain falls below ~3%, append a new
  dictionary rather than editing the old one.
- Never retrain during `add` — a dictionary that changes mid-operation would
  make units in the same commit mutually inconsistent for no benefit.
- `compact` is the natural retraining point: it already rewrites everything, so
  it can train a fresh dictionary, re-encode with it and drop dictionaries that
  end up with zero referencing units.

**The `--patch-from` option, and why I am not recommending it.** `zstd
--patch-from` is dictionary compression with the *entire old file* as the
dictionary; using the previous version of a file as the dictionary for its
replacement would be dramatically more effective than any corpus dictionary for
the edit case. But it makes the new chunk's decodability depend on a *specific
old chunk*, which:

- turns `compact`'s "drop unreferenced chunks" into a correctness bug of the
  worst kind (the old file's data is logically deleted but physically required);
- makes extraction of one file require decoding a chain of ancestors, breaking
  narc's bounded-memory extraction invariant;
- grows that chain without bound over repeated edits.

If it is ever attempted, it needs an explicit chain-depth cap and a pinning
mechanism in the reachability walk. Not for v0.3.

### 6.5 Recommended sequence

1. ~~**Measure the realized solid-block size histogram.**~~ **DONE in the
   verification pass** (`narc info` now reports it): 2 blocks, median 64.0 MiB —
   blocks sit at the `2 × target` cap, and realized geometry costs only +1.3%
   versus one solid stream (§6.3). This lever is spent.
2. ~~Fix block geometry so realized blocks approach the 32 MiB target; consider
   raising the target.~~ **DROPPED — the premise was false** (they already exceed
   it) and the wording was wrong on three counts anyway: (a) edit cost is
   proportional to block size — editing one member rewrites its whole block, so a
   larger target directly taxes narc's differentiator; (b) the "64 MiB is 2.3%
   behind" figure came from the **zstd** sweep (`dict-B.log`), not the LZMA2
   sweep, which has no 64 MiB row; (c) `MAX_CHUNK = 32 MiB` is a format-level
   constant used for hostile-manifest plausibility checks *and* the memory model,
   so changing it is a format change plus a decoder memory increase, not a tuning
   knob. **Replacement item: attack cross-file solidity for LARGE similar files**
   — measured at 3.27 MiB on this corpus, i.e. the entire 7-Zip gap (§6.3).
   Design it against the cheap-edit invariant from the start.
3. Verify the max-tier tournament is not selecting PPMd for source-code blocks
   (`narc info` shows ppmd7 holding 3.0 of 11.8 MiB).
4. Add a **base64 detector + decoder filter** (§4.1): −28% on base64 payloads,
   self-contained, no format-compatibility risk beyond a new filter id, and it
   fits narc's existing filter framework alongside BCJ and delta. **Scope it
   honestly first** — see the §4.1 correction: at the max tier every file
   < 1 MiB is inside a solid block that gets **one** plan for the whole block,
   so this filter can only fire on standalone units ≥ 1 MiB that are
   predominantly base64. Measure how much such data real archives contain before
   building it; it may well be less valuable than item 2.
5. Only then, and only if the edit workload justifies it, implement §6.2's
   dictionary support scoped to post-create appends — and first measure an
   **LZMA2** preset dictionary on per-file units with
   `crates/narc-core/examples/dict_probe.rs`, because every positive per-file
   number in this report is zstd's and PPMd cannot use a dictionary at all
   (§2.1, §6.1 corrections).
6. Do not implement: alphabet remapping, MTF, BWT, RLE, bijective base
   conversion, rANS swaps, hex undo, word/BPE tokenization (§7).

---

## 7. Negative knowledge — investigated and rejected

Ordered by how much time a wrong lead would cost.

- **A shared trained dictionary for 32 MiB solid blocks — the original
  hypothesis. REJECTED, break-even at best.** MEASURED (§2.6): the whole sweep
  lands in −0.1%…+0.8% of the no-dictionary total once the dictionary is stored;
  every *requested* size is a net loss and the single winner is an accidental
  34 KiB dictionary at 99.9%. (The original text claimed "every size ≥ 100.0%",
  which its own log contradicts — corrected in §2.6.) Root cause: a 32 MiB block
  already holds ~2400 files of its own history, so the dictionary is redundant
  with it. Dictionaries and solidity are *substitutes*.
- **PPMd7 cannot use a shared dictionary at all.** `ppmd-rust` 1.4 exposes no
  priming API; the only workaround puts the dictionary's compressed bytes inside
  every unit (§2.1 correction). Any dictionary design is LZMA2/zstd-only, which
  makes the max-tier tournament asymmetric.
- **"Raise the solid-block target past 32 MiB" as a cheap ratio win. REJECTED as
  free.** It is a direct trade against the cheap-edit differentiator (edit cost
  scales with block size), the remaining LZMA2 headroom above 32 MiB is ≤ ~2-3 pp
  of a 4.9% total gap, and `MAX_CHUNK` is a format constant tied to the memory
  model (§6.5 item 2).
- **"A dictionary gives cross-file redundancy without enlarging the compression
  unit." REJECTED as stated.** It gives *some* cross-unit redundancy, but the
  amount decays with unit size and vanishes by 32 MiB (§2.5): −16.3% at 256 KiB,
  −11.5% at 1 MiB, −1.8% at 16 MiB, +0.5% at 32 MiB. Confirmed independently by
  zstd's author (§2.2) and by the prepend experiment (§3.2: a 110 KiB prefix on
  a 32 MiB block is worth 0.6%).
- **Attaching a zstd dictionary to multi-MiB units. REJECTED, actively
  harmful.** MEASURED −5% at 4 MiB, **−15.3% at 32 MiB, −16.7% at 64 MiB**.
  Not the entropy tables (FULLDICT ≡ RAWCONTENT byte-for-byte), not window_log,
  not CDict parameter override — all three ruled out in §3. Inherent to zstd's
  `dictMatchState` path.
- **Requesting a multi-MiB dictionary from COVER/fastCover. REJECTED, silently
  broken.** Asking for 4 MiB returned **23 KiB**; 16 MiB returned **18 KiB**,
  both *worse than a 16 KiB dictionary* (§2.4). Always check the returned length.
- **`train_dictionary`/`--train` optimization mode in a pack pipeline. REJECTED,
  13.6× too slow *and* worse.** MEASURED: 107.2 s for one 110 KiB dictionary vs
  **7.9 s** with `d=8 k=200 accel=10`, and optimization mode's dictionary was the
  *weaker* of the two (78.9% vs 78.1%). Also rejected: **training on the whole
  corpus** — a 5% sample (3.4 MiB, **1.2 s**) captures ~94% of the benefit of a
  74 MiB, 24.7 s training run (§2.12).
- **LZMA2 preset dictionaries via `.xz`, `.7z`, `xz` CLI or CPython `lzma`.
  REJECTED, not available.** liblzma's `preset_dict` "works correctly only with
  raw encoding and decoding"; no liblzma container can decode with one; `xz` has
  no CLI option; `.7z` has no field for it; CPython rejects the filter key
  outright (verified). Only viable via raw LZMA2 — which narc can do because
  `lzma-rust2` 0.19 supports it on both sides in narc's own container (§2.10).
- **The LZMA2 multi-threaded writer with a preset dictionary. REJECTED, silently
  drops it.** `enc/lzma2_writer_mt.rs:76` sets `preset_dict = None`.
- **Dictionaries as a fix for the many-small-files gap vs 7-Zip. REJECTED,
  wrong target.** Best dictionary gain measured anywhere: 16%, and 0.6% at narc's
  unit size.
- **"The 7-Zip gap is the many-small-files path / small-file block geometry."
  REFUTED BY MEASUREMENT (§6.3).** narc's realized solid blocks sit at the 64 MiB
  hard cap, and that geometry costs **+1.3%** versus one solid stream — the
  small-file path is essentially optimal. The whole gap is the **large-file
  path**: 12 files ≥ 1 MiB cost 5.495 MiB compressed individually versus
  2.229 MiB solid (+146.5%), which is 3.27 MiB against a ~3.1 MiB total gap.
  The earlier "≈ 25% unexplained" figure came from a mismatched file-size split
  and was never a measurement. Two of the four original candidate causes
  (per-unit LZMA2 `dict_size`, extension-group flushing) are also disproven by
  reading the code, and a third (block sizes below target) by `narc info`.
- **`zstd --patch-from`-style chaining to an old file version for edits.
  REJECTED for v0.3.** Makes chunk decodability depend on a specific other
  chunk: breaks `compact`'s unreferenced-chunk pruning, breaks bounded-memory
  extraction, and grows unbounded chains over repeated edits (§6.4).
- **Fear that dictionaries break dedup. REJECTED as a concern — it does not
  apply.** narc hashes *uncompressed* bytes, so dedup and integrity are
  unaffected. The real hazard is immutability: a dictionary is decode-critical
  shared state with a huge blast radius, and `compact` will delete it unless the
  reachability walk is extended (§2.11).
- **One global dictionary per archive. REJECTED in favour of per-class.**
  Per-extension dictionaries are 10.4% better and still net −405 KiB after
  storing eight instead of one (§2.8). "There is no universal dictionary" —
  zstd README.
- **Raw sample bytes instead of a trained dictionary. REJECTED, ~⅓ the gain.**
  At 32 MiB blocks: COVER-trained 94.7% vs raw sample 98.1% (§2.7).
- **MTF as a preprocessing stage. REJECTED, catastrophic.** +202% (zstd) /
  +196% (LZMA2) standalone (§4.3). Only meaningful after BWT.
- **Frequency remapping / alphabet reduction / symbol renumbering. REJECTED,
  measured zero.** ±0.0% for zstd on source text (§4.3). The literature agrees
  and quantifies the trend: +7.08 pp for zlib but +0.76 pp for zstd and
  **−0.44 pp for PPMd** (§4.4) — negative for narc's strongest text codec.
- **Word-level / BPE tokenization + frequency-ordered ids. REJECTED for narc.**
  Published gains are for zlib-class back-ends; tokenization *alone* is negative
  for zstd (−0.38 pp), LZMA (−0.26 pp) and PPMd (−1.64 pp). Also needs a
  language-specific vocabulary shipped and versioned (§4.4).
- **BWT / bzip2-style pipeline. REJECTED.** Dominated by LZMA2 and PPMd on both
  ratio and speed; and BWT+MTF already does internally what alphabet transforms
  attempt, which is why bz2 gains −0.06 pp from them (§4.4).
- **RLE preprocessing. REJECTED, ~1% and sometimes negative** (§4.3). Both
  codecs encode runs as matches already.
- **Hex-dump decoding filter. REJECTED, not worth the code.** −0.7% for LZMA2,
  −3.9% for zstd: hex is nibble-aligned so the entropy coder already recovers
  it (§4.1).
- **Sparse-alphabet bit packing as a general filter. REJECTED (kept as a
  narrow LZMA2-only option).** zstd's FSE already reaches 2.007 bits/symbol on a
  4-symbol alphabet against a 2.000 floor — 0.4% left on the table. Worth 4-8%
  to LZMA2 only, requires a whole unit with ≤16 distinct values, and destroys
  byte alignment of repeats (§4.2). Real benefit is speed, not ratio.
- **Bijective base conversion / numeral-system transforms. REJECTED,
  information-theoretically empty.** A bijection preserves entropy; the only
  recoverable slack is representational, and §4.2 shows the entropy coder has
  already taken it (§4.5).
- **Replacing an entropy coder with rANS/tANS for ratio. REJECTED, wrong
  axis.** ANS's design goal is Huffman-like speed at arithmetic-coding rate; tANS
  carries a non-zero excess rate (the "~0.001-0.01 bits/symbol" magnitude is
  unsourced — see §4.5). Speed technology, slight ratio loss.
- **"10× on small files" as a reason to adopt dictionaries. REJECTED as
  inapplicable.** Reproduced the documented *curve shape* exactly but not the
  magnitude: 1.6× at `<1 KiB`, not 5×. The "10% at 64 KB / 5× at 1 KB" curve has
  **no upstream source** (not in `zstd(1)`, checked twice — §2.3); the ~10× that
  is real comes from the README's ~10K near-duplicate 1 KB JSON API records, and
  for such data concatenation alone beats the dictionary (§2.3).
- **Brotli as narc's main small-file codec. REJECTED (noted as an option).**
  Its free built-in 122 KiB dictionary is genuinely useful — −12.1% vs zstd-19
  per file — but that is only half of a trained dictionary's −22.2%, and brotli
  loses badly to LZMA2/PPMd on large units (§2.9).

---

## 8. Primary sources

**Dictionaries**
- [RFC 8878 — Zstandard Compression and the 'application/zstd' Media Type](https://www.rfc-editor.org/rfc/rfc8878.html) (obsoletes RFC 8478; dictionary format §5)
- [zstd `zdict.h`](https://github.com/facebook/zstd/blob/dev/lib/zdict.h) — training API, sizing rules, "no benefit past 100KB"
- [zstd manual page source](https://github.com/facebook/zstd/blob/dev/programs/zstd.1.md) — `--train`, `--train-cover`, `--train-fastcover`, `--maxdict`, `--dictID`
- [zstd README, "The case for Small Data compression"](https://github.com/facebook/zstd#the-case-for-small-data-compression) — "gains are mostly effective in the first few KB"; "no universal dictionary"
- [facebook/zstd#3783](https://github.com/facebook/zstd/issues/3783) — Yann Collet on why a dictionary cannot beat concatenation
- [facebook/zstd#1654](https://github.com/facebook/zstd/issues/1654) — COVER vs fastCover
- [restic#3775](https://github.com/restic/restic/issues/3775) — dictionaries considered and rejected; dictionary ≠ `--rsyncable`
- [restic#3666](https://github.com/restic/restic/pull/3666) / [restic 0.14.0 notes](https://restic.net/blog/2022-08-25/restic-0.14.0-released/) — per-blob compression, format-version gating
- [RFC 9842 — Compression Dictionary Transport](https://www.rfc-editor.org/rfc/rfc9842.html) — SHA-256 dictionary identity, immutability
- [Chrome: shared dictionary compression](https://developer.chrome.com/blog/shared-dictionary-compression) — Shared Brotli / Shared Zstandard deployment
- [liblzma `lzma_options_lzma`](https://tukaani.org/xz/liblzma-api/structlzma__options__lzma.html) — `preset_dict` is raw-only
- [xz-utils discussion: Preset dictionary](https://sourceforge.net/p/lzmautils/discussion/708858/thread/e40fbf99/) — upstream guidance, tail-ordering and sizing rules
- [femtozip: How it works](https://github.com/gtoubassi/femtozip/wiki/How-femtozip-works) and [LinkedIn SDCH](https://engineering.linkedin.com/shared-dictionary-compression-http-linkedin) — pre-zstd dictionary builders, ~7 h training cost
- Liao, Petri, Moffat, Wirth, "Effective Construction of Relative Lempel-Ziv Dictionaries" — the paper COVER is based on

**Alphabet / representation transforms**
- [Kalcher, "Frequency-Ordered Tokenization for Better Text Compression", arXiv:2602.22958](https://arxiv.org/abs/2602.22958) — Tables I & II, the definitive per-codec numbers
- [Duda, "Asymmetric numeral systems…", arXiv:1311.2540](https://arxiv.org/abs/1311.2540) — ANS is speed, not ratio
- [htscodecs BENCHMARKS.md](https://github.com/samtools/htscodecs/blob/master/BENCHMARKS.md) and [CRAM 3.1 supplement](https://academic.oup.com/bioinformatics/article-pdf/38/6/1497/49008384/btac010.pdf) — `PACK`/`STRIPE`, cardinality gating
- Skibiński, Grabowski, Deorowicz, "Revisiting dictionary-based compression", SP&E 35(15) 2005 — WRT, 3-14%
- [XWRT](https://github.com/inikep/XWRT) — WRT's XML successor, retuned per back-end
- [Large Text Compression Benchmark](https://mattmahoney.net/dc/text.html) — enwik8 reference ratios
- [Sequence Compression Benchmark](http://kirr.dyndns.org/sequence-compression-benchmark/) and [Kryukov et al.](https://www.biorxiv.org/content/10.1101/642553v1.full.pdf) — specialized vs general-purpose on FASTA

**narc source read during this study**
- `lzma-rust2` 0.19: `src/enc/lzma2_writer.rs:31,227-228`; `src/enc/lzma2_writer_mt.rs:76`;
  `src/lzma2_reader.rs:95-97`; `src/lzma_reader.rs:98,166-182`; `src/lz/lz_encoder.rs:224,255-265`

---

## 9. Verification pass — what was checked, what was corrected

An adversarial re-check of this report. Every citation below was fetched from
the primary source; every measurement was re-run on this machine.

**Confirmed, no change needed:**

- Tool versions in §1.2 are exact: zstd 1.5.7 via python-zstandard 0.25.0,
  CPython 3.14.5, brotli 1.2.0, 7-Zip 26.02 (2026-06-25). CPython's `lzma` does
  reject `preset_dict` with exactly the quoted `ValueError`.
- The §1.4 7-Zip anchors reproduce **to the byte** (5.684 / 5.684 / 8.120 /
  15.563 MiB); only the times differ (~15% faster on re-run).
- Every arithmetic total, percentage and bucket sum in §§2.1-2.9, 2.12, 4.1-4.3
  recomputes correctly from `test/dict-{A,B,C,D,F,G,T}.log`.
- Collet's two quotes in §2.2 are verbatim from facebook/zstd#3783; `greatroar`'s
  quote in §2.11 is verbatim from restic#3775.
- RFC 8878 §5: magic `0xEC30A437` LE, the four-field layout, Content-as-history,
  and the ≥ 8-byte raw-content rule all check out. RFC 9842 is indeed
  "Compression Dictionary Transport" (Standards Track, Sept 2025) with SHA-256
  identity, `Use-As-Dictionary` / `Available-Dictionary` / `Dictionary-ID`, and
  the quoted security sentence.
- arXiv:2602.22958 (Kalcher, ETH Zurich, 26 Feb 2026) exists and **Tables I and
  II match this report row for row**, including the load-bearing PPMd-16
  (22.83 → 23.27, −0.44 pp) and bz2 (−0.06 pp) rows. This is the strongest
  citation in the report and it holds.
- `zdict.h` quotes: "~100KB reasonable dictionary size", "~100x the size of the
  dictionary in samples", "about 6 MB" memory. `zstd(1)`: `--maxdict` default
  112640, "only the first 128 KiB of these samples will be used", "> 100"
  samples, "100x the target dictionary size".
- `lzma-rust2` **0.19.0** is the exact version `Cargo.toml` pins; **Apache-2.0**;
  upstream `hasenbanck/lzma-rust2`; released 2026-08-16, actively maintained.
  All five source claims in §2.10 verified line by line, including the MT writer
  dropping `preset_dict` and `set_preset_dict` keeping the **tail**.
- Python's `brotli` 1.2.0 exposes no custom-dictionary API (verified by
  introspection), as §2.9 states.

**Corrected in place (search for "CORRECTED"):**

| § | correction |
|---|---|
| 0, 6.3, 6.5, 7 | The 7-Zip gap is the **large-file** path (3.27 MiB), not small-file geometry. Realized blocks are 64 MiB and cost +1.3%. The "≈25% unexplained" figure and the "measure the histogram" next step are both retired. |
| 0, 2.6, 7 | "Every dictionary size is a net loss / the dictionary never wins" is **false against `dict-G.log`**: an omitted 34 KiB row wins by 0.1%. Correct claim: break-even within noise (−0.1%…+0.8%). Also the 183 KiB row was mislabelled "16 MiB requested" (it was 4 MiB). |
| 1.1, 6.3 | The corpus was cut at 256 KiB (`solid_max_file` for fast/normal). At the **max** tier it is **1 MiB**, so the real solid set is 5739 files / 86.50 MiB. The ROADMAP's "(256 KiB) … (8 MiB)" is stale since 871403e. |
| 1.3 | The `C(dict‖data) − C(dict)` proxy is **optimistic**, not neutral: `set_preset_dict` primes the LZ window only, the proxy primes the probability model too. |
| 1.4 | "zstd-19, one stream = 5.943 MiB" was the 64 MiB / 2-block row; the true one-stream figure is 5.816 MiB. |
| 2.1, 6.1, 6.2 | "One blob serves zstd, LZMA2 and PPMd alike" — **`ppmd-rust` 1.4 has no priming API.** Dictionaries are LZMA2/zstd-only, making the max-tier tournament asymmetric. |
| 2.3, 7 | The "10% at 64 KB to 5× at under 1 KB" quote is **not in `zstd(1)`** (checked in-repo and on manpages.debian.org). |
| 2.4 | `zdict.h`'s "not effective past 100KB" is about the **file being compressed**, not the dictionary size. §2.3 confirms it rather than contradicting it. |
| 2.5 | The 128 MiB row was omitted; it breaks the monotone story (110 KiB penalty falls to 106.6%, 1 MiB becomes a 1.5% gain). |
| 2.11, 5 | "restic considered dictionaries and rejected them" — the closing line is the **requester's**, not a maintainer decision. |
| 4.1, 6.5 | A base64 filter is per-**unit**; at the max tier every file < 1 MiB is inside a solid block with one plan for the whole block, so it can only fire on standalone units ≥ 1 MiB. |
| 4.5, 7 | The tANS excess-rate magnitudes are unsourced (not in arXiv:1311.2540). |
| 6.1 | The −21% append-path gain applies to `add <a few files>`, **not** to tree re-saves (which rebuild full-size blocks), and it is a **zstd** number with no LZMA2 equivalent measured. |
| 6.2 | Added the `lzma2_dict_size()` invariant: `dict_size` is *derived* from `unpacked_len` on both sides and never stored, so a preset dictionary silently changes it — the derivation must be conditional on `dict_id` or every existing archive becomes undecodable. |
| 6.3 | Candidate causes (3) per-unit `dict_size` and (4) extension-group flushing are disproven by reading `codec.rs` / `archive.rs`. |

**Verification scripts added:** `test/verify10.py` (LZMA2 floor for the real
max-tier solid set at 1 MiB cutoff), `test/verify10b.py` (large-file path and
7-Zip whole-corpus anchor).
