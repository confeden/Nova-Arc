# Research 11 — Where 7-Zip Is Actually Weak (and where it is not)

**Scope:** the 7z format and 7-Zip implementation as adversary; RAR5 and zpaqfranz on the same axes;
what `.narc` must do to be *strictly* better.
**Date:** 2026-08-17.
**Everything numeric in this report was measured on this machine unless labelled "claimed".**

Test rig: Windows 11 IoT Enterprise LTSC 2024, 8 logical cores, 32 GiB RAM (≈13 GiB free),
NVMe on `D:`. **7-Zip 26.02 (x64), 2026-06-25** (locally installed, `C:\Program Files\7-Zip`).
`narc` = local release build, format v0.2, `--full` (no throttling), tier `max`.
Timings and peak working set captured by a PowerShell harness that polls
`Process.PeakWorkingSet64` every 15 ms (`D:\tmp\7zw\mem.ps1`).

---

## 0. Executive summary — the honest scoreboard

| Axis | Is 7-Zip weak? | Inherent to the format? | Measured gap | narc action |
|---|---|---|---|---|
| 1. Solid-archive update cost | **Yes, on first touch** | Yes (folder = one coder stream) | 18–20 s + 1057 MiB RAM for a 29 KB edit vs narc 0.12 s | Keep; but fix `add <dir>` (§1.4) |
| 2. No deduplication | Yes | **Yes** (substream map is sequential) | Cuts both ways: narc −3.5 % on far-apart dupes, **+30 % worse** on near dupes | Stop selling dedup as ratio |
| 3. No trained dictionaries | **Yes** | Yes (LZMA2 props = 1 byte) | The whole 36.2 % small-files gap | **Biggest available lever** (§3) — but see §3.4 for its real cost |
| 4. Extraction parallelism | **Yes** | No — implementation | 1.00× across solid blocks; 3.6× only inside one folder | Free win, already 2.5× |
| 5. Extraction memory | **Yes** | Yes (LZ77 window must exist) | **1543.7 MiB to extract a 306 KiB archive** at `-md=1536m` | Keep O(chunk); advertise it |
| 6. No recovery record | Yes | No — policy | 1 flipped byte destroys 45.5 % of a solid archive | Optional RS section (§6) |
| 7. Header/metadata | **No — 7-Zip is good here** | — | headers 27.5× compressed, list 17 253 files in 0.064 s | Parity only; don't attack |
| 8. Encryption | **Yes, badly** | No — implementation | **zero-length salt**, verified in bytes and source | Argon2id + AEAD (§8) |
| 9. Random access in solid block | **Yes** | Yes | last file costs 10× the first (1.91 s vs 0.19 s) | Already position-independent |
| 10. Windows fidelity | **Yes** | Yes (no property IDs) | `-sns`/`-sni` → "Not implemented" | Easy differentiator |
| 11. 26.x changes | Nothing structural | — | 26.00–26.02 = bugfix/UX/CVE | No new threat |

Two findings you must internalise before writing any marketing copy:

* **The "7-Zip rewrites the whole archive on update" claim is only half true.** Verified below:
  the *first* edit of a given file costs a full solid-block repack (18–20 s here); every
  subsequent edit of that same file costs **0.6 s**, because 7-Zip migrates it into a small
  private folder and leaves the old copy in place. 7-Zip does *not* accumulate dead bytes
  across repeated edits — it pays CPU once instead of space forever.
* **Content-hash dedup is not a ratio story.** On three identical copies of a 126 MiB source
  tree, `7z -mx9` produced **11.4 MiB** and narc produced **16.4 MiB** — 7-Zip won by 30 %,
  because a 256 MiB LZMA window already covers the duplication. Dedup only wins when duplicates
  are farther apart than the dictionary, or when you cannot afford dictionary-sized RAM.

---

## 1. Update cost of solid archives, and the `-ms` trade

### 1.1 What the format forces

From [`DOC/7zFormat.txt`](https://raw.githubusercontent.com/ip7z/7zip/main/DOC/7zFormat.txt),
a *Folder* is the compression unit: `NumCoders`, coder IDs+properties, `NumBindPairs =
NumOutStreamsTotal - 1`, packed-stream indices. `SubStreamsInfo` then carves the folder's single
output stream into per-file substreams by **sequential sizes** (`kNumUnPackStream 0x0D`,
`kSize 0x09`, `kCRC 0x0A`). Changing any member of a folder therefore requires re-encoding the
whole folder. That is inherent and unfixable without a format break.

### 1.2 Where the default block size comes from (primary source)

From [`CPP/7zip/Archive/7z/7zHandlerOut.cpp`](https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zHandlerOut.cpp):

```cpp
const UInt64 kSolidBytes_Min = 1 << 24;          // 16 MiB
const UInt64 kSolidBytes_Max = (UInt64)1 << 32;  // 4 GiB, for non-LZMA2 methods
...
if (methodFull.Id == k_LZMA2) {
    UInt64 cs = (UInt64)dicSize << 2;
    const UInt32 kMinSize = (UInt32)1 << 20;     // 1 MiB
    const UInt32 kMaxSize = (UInt32)1 << 28;     // 256 MiB  <-- hard cap
    if (cs < kMinSize) cs = kMinSize;
    if (cs > kMaxSize) cs = kMaxSize;
    if (cs < dicSize) cs = dicSize;
    ...
    // we want to use at least 64 chunks (threads) per one solid block.
    numSolidBytes = cs << 6;                     // 64 x chunk
    const UInt64 kSolidBytes_Lzma2_Max = (UInt64)1 << 34;   // 16 GiB
```

So for the default `-mx9` (dict 256 MiB on 64-bit since 24.09) the **default solid block is
16 GiB** — i.e. "everything in one folder" — and the LZMA2 chunk is 256 MiB.

### 1.3 The measured ratio ↔ editability curve

126 MiB / 5751-file source tree, `7z -mx9`, one 29 KB `.c` file edited afterwards:

| `-ms=` | folders | archive | vs best | create | create RAM | **first edit** | edit RAM |
|---|---|---|---|---|---|---|---|
| `off` | 5751 | 23 697 709 B (22.6 MiB) | +159.6 % | 15.35 s | 76 MiB | **0.72 s** | 24 MiB |
| `1m` | 105 | 17 864 430 B (17.0 MiB) | +95.7 % | 12.65 s | 75 MiB | 0.73 s | 32 MiB |
| `4m` | 31 | 16 143 022 B (15.4 MiB) | +76.8 % | 14.29 s | 76 MiB | 1.12 s | 69 MiB |
| `16m` | 8 | 13 543 835 B (12.9 MiB) | +48.4 % | 17.89 s | 227 MiB | 2.89 s | 213 MiB |
| `64m` | 3 | 10 241 470 B (9.8 MiB) | +12.2 % | 23.26 s | 722 MiB | 13.23 s | 724 MiB |
| `on` (default) | 2 | **9 129 445 B (8.7 MiB)** | — | 24.19 s | 965 MiB | **20.25 s** | 1057 MiB |
| **narc max** | — | 12 436 093 B (11.9 MiB) | +36.2 % | 49.43 s | 720 MiB | **0.12 s** | 8.5 MiB |

Read it as: **7-Zip's editability knob costs 2.60× ratio** (22.6 / 8.7 MiB) and buys 28× faster
edits. narc currently sits at 7-Zip's `-ms≈24m` ratio with better-than-`-ms=off` edit cost, at
**1/125 of the edit RAM**. That is the correct positioning claim, and it is defensible.

### 1.4 The update model, precisely (this corrects the project's premise)

Repeated edits on the same solid `-ms=on` archive, each time via `7z u archive.7z <path>`:

| what changed | command form | time | archive delta | folders after |
|---|---|---|---|---|
| file still inside a **big** folder | dir form (`7z u a.7z src`) | **18.29 s / 20.27 s** | −3 618 B / +7 153 B | 4 → 5 → 6 |
| **same** file again | dir form | 0.62 s | +34 B | 6 |
| same file, 5× in a row | explicit path | 0.58–0.61 s each | +43, +20, −16, −2, +6 B | 4 (constant) |

Interpretation: 7-Zip repacks **only the folder containing the changed file**. On a fresh
`-ms=on` archive that folder is the whole archive → 20 s at 1057 MiB. After the repack the file
lives in a tiny folder, so further edits are ~0.6 s. Folder count grows by one per newly touched
file; the archive size stays flat (9.142 MB across ten edits). **7-Zip trades CPU once; narc
trades space until `compact`.**

narc's own weak spot, measured on the same corpus:

| operation | narc | 7-Zip `-ms=on` |
|---|---|---|
| replace **one named file** | **0.12 s**, +84 KB appended | 0.59 s, +7.6 KB |
| re-add the **whole directory** with 1 file changed | **45.01 s**, **+5.4 MiB** | **0.62 s**, +34 B |

`narc add <archive> <dir>` re-reads and re-analyses all 5751 files and re-emits solid blocks;
after 4 generations `narc info` reported `Reclaimable: 5.2 MiB` on a 12 MiB archive. This is the
single most user-visible regression versus 7-Zip and it is an implementation gap, not a format
one. Fix: mtime+size fast path against the manifest, and reuse existing block assignments for
untouched members.

**Verdict:** the *inherent* weakness is real but narrower than assumed — you cannot have a big
window and cheap edits *in the same unit*. The way to win is not "smaller units" (that is the
`-ms=off` column, −2.6× ratio); it is **a big window that lives outside the unit** → §3.

---

## 2. No deduplication — quantified, and it cuts both ways

### 2.1 Inherent, not an oversight

In `FilesInfo`/`SubStreamsInfo` each file maps to exactly one substream at a sequential offset
inside one folder. There is no indirection expressing "file B is file A". Feature request
[#1359](https://sourceforge.net/p/sevenzip/feature-requests/1359/) ("find redundant files within
directory and store them as one") has been open since 2018. **Adding dedup requires a format
break.**

### 2.2 Corpus A — duplicates farther apart than the effective window

`dup1` = 4 byte-identical copies of `enwik8` (400 000 000 B = 381.5 MiB):

| configuration | archive | time | peak RAM | recorded dict |
|---|---|---|---|---|
| `7z -mx9` (default `-mmt=8`) | 49 631 873 B (47.3 MiB) | 215.5 s | 3884 MiB | `LZMA2:28` (256 MiB) |
| `7z -mx9 -mmt=4` | 49 631 873 B | 206.0 s | 3884 MiB | `LZMA2:28` |
| `7z -mx9 -mmt=2` | 24 842 978 B | (not timed) | — | `LZMA2:28` |
| `7z -mx9 -mmt=1` | 24 842 831 B (23.7 MiB) | **458.3 s** | 2694 MiB | `LZMA2:28` |
| `7z -mx9 -md=1536m` | 24 842 887 B | 320.8 s | 3959 MiB | `LZMA2:384m` (reduced) |
| **narc max** | **23 966 015 B (22.9 MiB)** | **25.1 s** | **560 MiB** | — |

narc beats 7-Zip's *best* result by 3.5 % while being **12.8× faster** and using **7× less RAM**;
it beats the *default* by 2.07× and 8.6×.

### 2.3 The reason 7-Zip's default doubled — its own multithreading

From [`C/Lzma2Enc.c`](https://raw.githubusercontent.com/ip7z/7zip/main/C/Lzma2Enc.c), `Lzma2EncProps_Normalize`:

```c
else if (p->blockSize == LZMA2_ENC_PROPS_BLOCK_SIZE_AUTO && t2 <= 1)
{
  /* if there is no block multi-threading, we use SOLID block */
  p->blockSize = LZMA2_ENC_PROPS_BLOCK_SIZE_SOLID;
}
else { ... blockSize = clamp(dictSize << 2, 1 MiB, 256 MiB); if (blockSize < dictSize) blockSize = dictSize; }
```

Effective MT block = `max(min(4·dict, 256 MiB), dict)`, and **every block resets the LZMA2
dictionary**. With the `-mx9` default dict of 256 MiB the block is 256 MiB, so a 381.5 MiB input
becomes 2 independent streams → each compressed to ≈24.8 MB → 49.6 MB total. Exactly what was
measured, and the ratio cliff appears the moment block-MT engages (`-mmt≥4`, because the LZMA
encoder itself consumes 2 threads):

```
-mmt=1 → 24 842 831 B     -mmt=2 → 24 842 978 B     -mmt=4 → 49 631 873 B     -mmt=8 → 49 631 873 B
```

Confirmed at the source level, which is stronger than the curve: `Lzma2EncProps_Normalize` splits
threads as `t2 = t3 / t1n` where `t1n = 2` (the BT match finder takes two threads) and `t2` is the
number of *block* threads. So `-mmt=1` and `-mmt=2` both yield `t2 = 1` → `BLOCK_SIZE_SOLID`
(identical ratio, and they are: 24 842 831 vs 24 842 978, a 147 B difference), while `-mmt=4`
yields `t2 = 2` → `BLOCK_SIZE_AUTO` → the 256 MiB cliff.

**7-Zip's default settings lose 100 % of ratio on this corpus purely to parallelism.** Two
corollaries worth remembering:

* The real redundancy window of a default `-mx9` archive is **256 MiB, not the 16 GiB solid block**,
  and the `Blocks = 1` shown by `7z l -slt` hides this completely.
* Raising `-md` above 256 MiB sets `blockSize = dict`, so you regain the window but lose the
  parallelism *and* pay dict-sized RAM on both ends. That three-way lock (ratio × threads × decode
  RAM) is the structural flaw to attack.
* LZMA2 dictionary sizes are quantised — the props field is a single byte encoding only `2^k` and
  `3·2^k`. `-md=1536m` on a 381.5 MiB input became `LZMA2:384m` (`LzmaEncProps_Normalize` clamps
  `dictSize` to `reduceSize`, then the byte encoding rounds up). Max LZMA2 dict is 4 GiB
  (verified: `-md=4g` accepted, `-md=8g` → `System ERROR`).

### 2.4 Corpus B and C — where dedup loses, and where it wins back

`dup2` = 3 identical copies of the 126 MiB source tree (395 MB, 17 253 files):

| | archive | time | peak RAM |
|---|---|---|---|
| `7z -mx9` | **11 965 720 B (11.4 MiB)** | 68.1 s | 2815 MiB |
| narc max | 17 154 751 B (16.4 MiB) | 50.0 s | 2825 MiB |

**7-Zip wins by 30 %.** The whole corpus fits inside two 256 MiB LZMA2 blocks, so LZ77 matching
does the dedup *and* gets a better base ratio than narc's 32 MiB units.

`dup8` = 8 identical copies (1004 MiB, 46 008 files):

| | archive | time | peak RAM |
|---|---|---|---|
| `7z -mx9` | 26 201 966 B (25.0 MiB) | 114.1 s | **7392 MiB** |
| narc max | **23 304 168 B (22.2 MiB)** | 135.6 s | 4832 MiB |

narc wins ratio by 11 % and loses time by 19 %; 7-Zip needed **7.2 GiB of RAM to compress 1 GiB**.

**Blunt conclusion:** dedup is an *edit-locality and RAM* feature, not a ratio feature. Its ratio
payoff appears only past the dictionary distance. Anyone who benchmarks "3 copies of my repo"
will see 7-Zip win. Do not put dedup in the ratio pitch.

---

## 3. No trained / preset dictionaries — the single biggest lever

### 3.1 7-Zip cannot do it, at format level

LZMA2 coder properties inside `.7z` are **one byte** (dictionary size). There is no field for a
preset dictionary; LZMA1's 5-byte props carry only `lc/lp/pb` + `dictSize`. Nothing in the
7-Zip 26.02 codec list can use a trained dictionary either — the official build offers only
`Copy, LZMA, LZMA2, PPMd, BZip2, Deflate, Deflate64` plus filters
(`BCJ, BCJ2, ARM64, RISCV, PPC, IA64, ARM, ARMT, SPARC, Swap2, Swap4, Delta`) and `7zAES`
(verified via `7z i`). **No zstd/brotli/lz4 encoder for the 7z container at all.**

### 3.2 narc already ships the capability — verify before designing anything else

The LZMA authors describe narc's exact situation in
[`liblzma/api/lzma/lzma12.h`](https://raw.githubusercontent.com/tukaani-project/xz/master/src/liblzma/api/lzma/lzma12.h):

> "It is possible to initialize the LZ77 history window using a preset dictionary. It is useful
> when compressing many similar, relatively small chunks of data independently from each other.
> The preset dictionary should contain typical strings that occur in the files being compressed.
> The most probable strings should be near the end of the preset dictionary."

liblzma only allows this on the **raw** encoder/decoder (not inside `.xz`/`.lzma`) — which is fine,
because `.narc` is our own container. And crucially, **the crate narc already depends on supports
it on both sides** (`lzma-rust2` 0.19.0, vendored at
`~/.cargo/registry/src/index.crates.io-*/lzma-rust2-0.19.0/`):

| side | location | API |
|---|---|---|
| encoder | `src/enc/lzma2_writer.rs:31` | `pub preset_dict: Option<Vec<u8>>` on `LZMA2Options` |
| encoder wiring | `src/enc/lzma2_writer.rs:227-228` | `lzma.lz.set_preset_dict(dict_size, preset_dict)` |
| decoder | `src/lzma2_reader.rs:95` | `LZMA2Reader::new(inner, dict_size, preset_dict: Option<&[u8]>)` |
| LZMA1 | `src/enc/lzma_writer.rs:38-44` | same |
| **gotcha** | `src/enc/lzma2_writer_mt.rs:76` | the crate's own MT writer does `preset_dict = None` |

`zstd` 0.13.3 / `zstd-safe` 7.2.4 are also present and support dictionaries natively
(`ZDICT_optimizeTrainFromBuffer_fastCover` under the hood; upstream guidance: dictionary ≈100 KB,
total samples ≈100× the dictionary size, training memory ≈6 MB).

**PPMd is the exception:** `ppmd-rust` 1.4.0 exposes only `order`, `mem_size`, `RestoreMethod` —
no preset/priming API. Priming a PPMd model means encoding `dict || unit` and discarding the first
`len(dict)` decoded bytes, i.e. paying dictionary-sized work on *every* unit, in both directions.
So the trained-dictionary lever is cleanly available for **LZMA2 and zstd, not PPMd**.

### 3.3 Why this is the right fix for the 36.2 % small-files gap

The gap on the source tree is 11.9 → 8.7 MiB (`+36.2 %` in §1.3's table; closing it means removing
26.6 % of narc's output). 7-Zip closes it with a 256 MiB window, paying
965 MiB to compress, 264 MiB to extract, and 20 s per first edit. A per-extension trained
dictionary of 16–64 MiB, stored once as an immutable archive object and passed to
`set_preset_dict`, gives the *same* cross-file redundancy while:

* extraction RAM stays `dict + unit` — bounded and constant, not proportional to archive size;
* edit granularity stays the unit — the dictionary is never rewritten by an edit;
* decode remains fully parallel across units.

Format consequences that must be settled *now* (same class of mistake as zpaq's `-fragment`):

* the dictionary is **format-critical**: its BLAKE3 hash must be pinned in the manifest, its
  identity recorded per unit, and it must be covered by the recovery record (§6);
* re-adding files after the dictionary changed produces different compressed bytes → plan for
  multiple dictionaries coexisting in one archive, addressed by id;
* a dictionary must never be *required* to read a unit that did not use one.

---

## 4. Extraction parallelism — what `-mmt` actually parallelises

History says the capability arrived in **18.03 (2018-03-04)**:

> "7-Zip now can use multi-threading for 7z/LZMA2 decoding, **if there are multiple independent
> data chunks in LZMA2 stream.**" — [7-zip.org/history.txt](https://www.7-zip.org/history.txt)

Measured on the same 203 MiB Silesia payload, decode-only (`7z t`, no disk writes):

| archive | folders | independent LZMA2 chunks | `-mmt=1` | `-mmt=8` | speed-up | peak RAM 1→8 |
|---|---|---|---|---|---|---|
| `-mx9 -ms=on` (default) | 1 | **1** | 1.60 s | 1.61 s | **1.00×** | 210 → 256 MiB |
| `-mx9 -ms=16m` | **10** | 1 per folder | 1.56 s | 1.57 s | **1.00×** | 57 → 69 MiB |
| `-ms=on -m0=LZMA2:d=192m:c=16m` | 1 | ~13 in one folder | 1.59 s | **0.44 s** | **3.6×** | 24 → 169 MiB |

Full extract to disk confirms it: solid 1.88 s (`-mmt=1`) vs 1.89 s (`-mmt=8`); ten 16 MiB folders
1.95 / 1.90 / 1.84 / 1.76 s at 1/2/4/8 threads = **1.11×**.

Three hard conclusions:

1. **7-Zip never parallelises decoding across solid blocks.** Ten independent folders gave zero
   speed-up. This is an *implementation* limit (it streams output files in archive order), not a
   format one — so it is a gap a competitor can simply take.
2. Intra-folder chunk parallelism works (3.6×) but you must ask for a small `c=`, and
   `7zHandlerOut.cpp` then does `cs = Get_Xz_BlockSize(); if (dicSize > cs) dicSize = cs;` —
   **the dictionary is clamped down to the chunk size.** Verified: the archive recorded
   `LZMA2:24` (16 MiB) instead of `LZMA2:28` (256 MiB), and cost +1.03 % size on Silesia
   (49 191 269 vs 48 688 197 B). On a source tree that clamp would cost far more (§1.3).
3. On a *default* `-mx9` archive, decoding is strictly single-threaded. `-mmt` is a compression
   switch with a decoding side-effect that only materialises when compression already sacrificed
   the window.

narc for contrast (Silesia, tier max): extract `-j1` **16.89 s**, `-j8` **6.67 s** (2.53×).
So narc's parallelism advantage is real *but currently spent covering a much slower codec* — see §12.

---

## 5. Memory to EXTRACT at high dictionary settings

Method: 2 GiB of zeros compressed at each `-md` (so `reduceSize` never clamps the dictionary),
then `7z t` with peak working set polled. Archive is 305 KiB in every case.

| `-md=` | archive | decode time | **decode peak RSS** | RSS − dict |
|---|---|---|---|---|
| `64m` | 305 KiB | 2.66 s | 71.7 MiB | 7.7 MiB |
| `128m` | 305 KiB | 2.64 s | 135.7 MiB | 7.7 MiB |
| `256m` (today's `-mx9` default) | 305 KiB | 2.64 s | **263.7 MiB** | 7.7 MiB |
| `512m` | 305 KiB | 2.64 s | 519.7 MiB | 7.7 MiB |
| `1024m` | 305 KiB | 2.70 s | 1031.7 MiB | 7.7 MiB |
| **`1536m`** | **305 KiB** | 2.75 s | **1543.7 MiB** | 7.7 MiB |

Exactly linear: **peak = dictionary + 7.7 MiB**, independent of archive size and independent of
`-mmt` (256m and 1536m gave byte-identical peaks with 8 threads). A **306 KiB** file therefore
demands **1.5 GiB** of RAM to open. This is inherent: the LZMA decoder must materialise the whole
LZ77 window.

Two aggravating facts:

* **24.09 (2024-11-29) raised the defaults** — `-mx7` 32→128 MB, `-mx8/-mx9` 64→256 MB on 64-bit.
  Every ordinary `-mx9` archive made since then needs 264 MiB to extract, where a 23.01-era one
  needed 72 MiB. A weak-PC user cannot tell from the outside.
* Compression is worse still: `-mx9` on 381.5 MiB needed **3884 MiB**, and on 1004 MiB needed
  **7392 MiB**. 21.04 added a mitigation ("7-Zip now reduces the number of working CPU threads for
  compression, if RAM size is not enough") — i.e. low RAM silently costs you threads *and*,
  per §2.3, changes the ratio.

**narc's counter-claim, measured:** Silesia extraction peaked at 74 MiB for a single 9.7 MiB file
and 123 MiB for the full 203 MiB archive at `-j1`. `narc info`/`list` on a 17 253-file archive
peaked under 10 MiB. Bounded-by-chunk extraction is a genuine, checkable differentiator; keep the
`MAX_CHUNK`-bounded invariant sacred.

---

## 6. No recovery record — and what to do instead

### 6.1 7-Zip's position (primary quotes)

Igor Pavlov, [SourceForge thread "Igor, please, don't add recovery"](https://sourceforge.net/p/sevenzip/discussion/45797/thread/49bd833d/), 2006-04-16:

> "Yes, I don't plan 'recovery feature' for nearest future. Compression is more important for me."
> … "Before implementing 'recovery feature' we need good statistics of types of damages. I have no
> such information."

And in the 7-Zip 22.01 discussion, on the same request: *"I'm not ready to work for that feature."*
Feature request [#1374](https://sourceforge.net/p/sevenzip/feature-requests/1374/) ("Please add
recovery record function for 7z", 2018-08-27) is still open. This is **policy, not a format
limitation** — the format already tolerates trailing data (7-Zip warns "There are data after the
end of archive" rather than failing).

### 6.2 Measured damage radius (one flipped byte at the 50 % mark)

| archive | layout | recovered / 211 938 580 B | files lost |
|---|---|---|---|
| `sil-solid.7z` | 1 folder, 203 MiB | 115 513 230 B = **54.5 %** | reymont, samba, sao, webster, x-ray, xml; osdb truncated |
| `sil-16m.7z` | 10 folders | 206 350 221 B = **97.4 %** | osdb truncated only |
| `sil.narc` | ~16 MiB chunks | 139 322 729 B = **65.7 %** | **and narc aborted (exit 1)** |

Header damage: 10 bytes clobbered near EOF → `7z l` reports `Headers Error`, archive unopenable
(no redundancy for the header at all). narc's footer damaged the same way → `narc list` printed
`0 file(s)` **with no loud error**, which is worse UX than an explicit failure.

**narc's own gap here is embarrassing and cheap to fix:** narc stops at the first bad chunk instead
of skipping it, so four files whose chunks were intact were never written. Skip-and-continue plus a
per-file damage report would already put narc ahead of solid 7z, before any parity data exists.

### 6.3 What a modern format should do

RAR5 is the benchmark: recovery record is **Reed-Solomon based** (RAR4 used 512-byte recovery
sectors capped at 524 288 sectors ≈ 256 MB), sized as a percent of archive size
(`rr` with no argument = **3 %**; 3–10 % recommended; cannot exceed 100 %), roughly matching RAR4
for continuous damage but "significantly more efficient in the case of multiple damaged areas"
([WinRAR docs](https://techshelps.github.io/WinRAR/html/HELPCmdRR.htm),
[win-rar.com explainer](https://www.win-rar.com/recovery-record.html?L=0)). Note the limit:
"the Repair command does not fix broken blocks in the recovery record itself".

Recommended design for `.narc`:

* **Two tiers.** Always protect the footer + manifest (kilobytes; makes the difference between
  "unopenable" and "fully listable"). Optionally protect the chunk log at a user-chosen percent.
* **Detection separate from correction.** Per-shard CRC32c; RS reconstructs only shards you know
  are bad. This is what PAR2/PAR3 do at format level, and what the Rust crates explicitly require.
* **Codec:** [`reed-solomon-simd`](https://docs.rs/reed-solomon-simd/) — fork of `reed-solomon-16`
  (Leopard-RS lineage), GF(2^16), O(n log n), runtime SSSE3/AVX2/Neon selection with a plain-Rust
  fallback, 1–32768 shards, and since 3.0.0 shard sizes need not be multiples of 64.
* **Append-only.** Parity as a separate trailing section referenced by the footer keeps the
  append-only commit protocol intact and lets `compact` regenerate it.

Rejected: targeting PAR3 compatibility (still the **2022-01-28 ALPHA DRAFT** after four years);
`reed-solomon-erasure` (repo header literally reads "*Looking for new owners/maintainers*", SIMD
tuned for Haswell+ only); `reed-solomon-16` (SIMD "planned, not implemented").

---

## 7. Header / metadata — 7-Zip is *good* here; do not attack it

`SignatureHeader` is a fixed 32 bytes at offset 0
([`7zFormat.txt`](https://raw.githubusercontent.com/ip7z/7zip/main/DOC/7zFormat.txt)):

```
BYTE kSignature[6] = {'7','z',0xBC,0xAF,0x27,0x1C};
ArchiveVersion { BYTE Major; BYTE Minor; }      // 0.4
UINT32 StartHeaderCRC;
StartHeader { REAL_UINT64 NextHeaderOffset; REAL_UINT64 NextHeaderSize; UINT32 NextHeaderCRC; }
```

So the file list is located in **O(1)** — no scan — and `kEncodedHeader (0x17)` means the header is
itself a compressed stream. Measured on a 17 253-file archive:

| | value |
|---|---|
| `Headers Size`, default (`-mhc=on`) | **104 082 B** ≈ 6 B/file |
| `Headers Size`, `-mhc=off` | 2 864 598 B ≈ 166 B/file |
| header compression factor | **27.5×** |
| `7z l` wall time (17 253 files) | **0.063–0.064 s** |
| `narc list` wall time, same file count | 0.096–0.102 s |
| `narc info` | 0.037 s |

*(An earlier measurement through the PowerShell harness showed `narc list` at ~1.8 s. That was a
harness artefact — the shell-`time` numbers above are the correct ones. Recording it so nobody
re-derives the wrong conclusion.)*

7-Zip additionally writes `kDummy (0x19)` padding blocks since 9.26 "for faster archive opening".
**There is no listing-cost weakness to exploit.** narc is at parity and should stay there; the only
metadata weakness is the encryption leak in §8.

---

## 8. Encryption — the most serious defect found, and it is fixable-by-them

### 8.1 Verified from real archive bytes

Two archives of the same file, same password, `-mhc=off` so the AES coder props sit in a readable
header. Parsing the `7zAES` coder id `06 F1 07 01` and its properties:

```
e1.7z props = 53 0f b51cdc7a45da4743d7fdb8fac1df1d9d
e2.7z props = 53 0f b644c7273b65343a641499301652c289
             ^^ ^^ ^^ 16-byte IV
             |  |
             |  +-- 0x0f : salt-size nibble absent, ivSize = 0x0f + 1 = 16
             +----- 0x53 = 0b0101_0011 : NumCyclesPower = 19, bit6 IV present, bit7 SALT ABSENT
```

**7z AES-256 uses a zero-length salt.** Confirmed in
[`CPP/7zip/Crypto/7zAes.cpp`](https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Crypto/7zAes.cpp) —
the salt generator is commented out:

```cpp
CEncoder::CEncoder()
{
  // _key.SaltSize = 4; g_RandomGenerator.Generate(_key.Salt, _key.SaltSize);
  // _key.NumCyclesPower = 0x3F;
  _key.NumCyclesPower = 19;
  _aesFilter = new CAesCbcEncoder(kKeySize);
}
```

`Z7_COM7F_IMF(CEncoder::ResetSalt())` — which would have set `SaltSize = 4` — is commented out
wholesale, and `CKeyInfo::ClearProps()` leaves `SaltSize = 0`. The KDF is therefore
`SHA-256` iterated `2^19 = 524 288` times over `password || 8-byte LE counter`, with **no salt at
all**. 7-Zip even keeps a process-global key cache (`g_GlobalKeyCache(32)`), which is only sound
*because* the key depends on nothing but the password.

Consequences: one precomputed table per candidate password is valid for **every 7z archive ever
made**; identical passwords produce identical keys across archives; 2^19 SHA-256 rounds is a
weak work factor by 2026 standards (single-digit hundreds of milliseconds).

### 8.2 The other three problems

| issue | detail |
|---|---|
| No AEAD | AES-256-**CBC** with a random 16-byte IV, no MAC. Integrity is CRC-32 of the plaintext → malleable ciphertext, non-cryptographic check. |
| `-mhe=off` is the default with `-p` | Verified: `7z l -slt` on such an archive prints, **without the password**, every `Path`, `Size`, `Modified` (100 ns precision) **and `CRC`** of the plaintext. The plaintext CRC-32 is a free confirmation oracle for "is file X in this archive". |
| `-mhe=on` is all-or-nothing | It hides names correctly (listing prompts for a password), but you must know to ask for it. |

### 8.3 RAR5 does this properly — copy RAR, not 7-Zip

From the [RAR 5.0 format technote](https://github.com/pmachapman/unrar/wiki/RAR-5.0-archive-format):
encryption version 0 = AES-256; **`Salt: 16 bytes` "used globally for all encrypted archive
headers"**; `KDF count: 1 byte — binary logarithm of iteration number for PBKDF2`;
`Check value: 12 bytes` (8 PBKDF2-derived + 4-byte checksum). Optional `BLAKE2sp` 256-bit file
checksums instead of CRC32, and — the detail 7-Zip missed — *"if archive headers are not
encrypted, file checksums for encrypted RAR 5.0 files are modified using a password-dependent
algorithm so that file contents cannot be guessed from checksums."* Still no AEAD.

### 8.4 narc's requirements (and a trap specific to dedup)

* Argon2id (or scrypt) with a **16-byte random salt** stored in the header; tunable, recorded.
* **AEAD per chunk** — XChaCha20-Poly1305 or AES-256-GCM-SIV — with the chunk index and archive id
  in the AAD so chunks cannot be reordered or transplanted between archives.
* Encrypt the **manifest unconditionally** when encryption is on. There is no defensible reason to
  ship 7-Zip's `-mhe=off` default.
* **The dedup trap:** `blake3(plaintext)[..16]` stored in the manifest is exactly the confirmation
  oracle that RAR5 explicitly defends against, and worse — it is content-addressed, so it works
  across archives. Use **keyed BLAKE3** with a key derived from the archive key. Dedup still works
  within an archive; cross-archive dedup and the oracle both disappear. This must be decided before
  encryption ships, because it changes the manifest schema.

---

## 9. No random access to a file inside a solid block

Silesia, 203 MiB, `-mx9 -ms=on` → one folder (46.4 MiB archive). Extracting exactly one file:

| file extracted | size | position in folder | time | peak RAM |
|---|---|---|---|---|
| `dickens` | 9.7 MiB | 1st (offset 0) | **0.19 s** | 19.9 MiB |
| `mozilla` | 48.8 MiB | 2nd | 0.69 s | 81.5 MiB |
| `webster` | 39.5 MiB | 10th | 1.81 s | 238.1 MiB |
| `xml` | **5.1 MiB** | last (≈206 MB in) | **1.91 s** | **256.0 MiB** |
| *(whole archive, 203 MiB)* | — | — | 1.88 s | 209 MiB |

Extracting the **last 5 MiB file costs more than extracting all 203 MiB**, at the full dictionary's
RAM. Splitting into ten 16 MiB folders fixes it (`dickens` 0.17 s / 19.9 MiB, `xml` 0.25 s /
21.3 MiB) for **+0.14 % archive size on Silesia** — but §1.3 shows the same knob costs 2.6× on a
source tree, so it is not a free default.

And 7-Zip cannot even use LZMA2's own restart points: on the `d=192m:c=16m` archive (one folder,
~13 dictionary-reset chunks) `-mmt=1` extraction of the last file took **1.63 s** — identical to a
full folder decode. The earlier 0.43 s figure was 8 threads decoding *everything* in parallel, not
a seek. **Random-access granularity in 7z is the folder, period**, because intra-folder chunk
offsets are not indexed in the header.

narc for contrast: `dickens` 2.07 s / 74 MiB, `xml` 0.33 s / 37.7 MiB — cost tracks the file's own
size, not its position. That is the right property; the per-byte constant is the problem (§12).

---

## 10. Windows fidelity: timestamps, ACLs, alternate data streams

Test file with an alternate data stream and three distinct sub-second timestamps:

| property | default `7z a -t7z` | with switches | verdict |
|---|---|---|---|
| mtime | **stored, 100 ns FILETIME** (`Modified = 2002-03-04 05:06:07.7654321`) | — | full fidelity |
| ctime (Created) | **dropped** | `-mtc=on` → `2001-02-03 04:05:06.1234567` | opt-in |
| atime (Accessed) | **dropped** | `-mta=on` → but recorded **2026-08-17**, i.e. clobbered by 7-Zip's own read; needs `-ssp` | opt-in and lossy |
| Windows attributes | stored (`Attributes = A`) | — | ok |
| **alternate data streams** | **silently lost, no warning** | `-sns` → **`System ERROR: Not implemented`** | 7z format cannot |
| **NT security / ACLs** | not stored | `-sni` → **`System ERROR: Not implemented`** | 7z format cannot |
| hard links, sparse ranges, EAs, object ids | not stored | — | 7z format cannot |

Both switches work in 7-Zip's **WIM** handler, which is the tell: the capability exists in the
program, the **7z format has no property IDs for it** (the defined range ends at `kDummy 0x19`).
RAR5 by contrast defines service headers `ACL` ("NTFS file permissions") and `STM` ("NTFS
alternate data stream"), plus redirection record types `0x0001` Unix symlink, `0x0002` Windows
symlink, `0x0003` Windows junction, `0x0004` Hard link, `0x0005` File copy.

`0x0005 File copy` deserves a note: **RAR5 has file-level dedup** that 7z lacks — an identical
file can be stored as a redirection instead of data.

This is the cheapest differentiator on the list. narc's ROADMAP already lists empty dirs, symlinks,
NTFS attrs/ADS and ACLs as "not preserved yet"; shipping them makes a factual claim no `.7z` can
match, and matters for real backup use.

---

## 11. What 7-Zip 26.x actually added (and why it changes nothing)

From the locally installed `History.txt` and [7-zip.org/history.txt](https://www.7-zip.org/history.txt):

| version | date | content |
|---|---|---|
| **26.02** | 2026-06-25 | "Some bugs and vulnerabilities were fixed." Nothing else. |
| **26.01** | 2026-04-27 | Linux **huge pages (2 MB)** → "+10 % for 7z/xz/LZMA/LZMA2 compression"; new `-spo[d\|c\|r]` output-path modes; **CVE-2026-48095** heap overflow in the NTFS handler. |
| **26.00** | 2026-02-12 | Improved ZIP/CPIO/RAR/UFD/QCOW/Compound handlers; File Manager sorting secondary key; benchmark supports >64 threads; TAR sparse-file extraction fix. |
| 25.01 | 2025-08-03 | **CVE-2025-55188** symlink handling hardened; `-snld20` to bypass. |
| 25.00 | 2025-07-05 | >64 CPU threads for compression via processor groups; bzip2 +15–40 %; CVE-2025-11001/11002/53816/53817. |
| 24.09 | 2024-11-29 | **Default dictionaries raised** (`-mx9` 64 → 256 MB on 64-bit) — see §5. |

**Nothing in 26.x touches dedup, recovery, random access, decode parallelism across folders,
encryption, or the memory model.** The trajectory is maintenance, format-handler breadth, and CVE
response. Two things to respect rather than dismiss: the CVE cadence is real work (five CVEs in
14 months, mostly in foreign-format parsers — an argument for `#![forbid(unsafe_code)]` in
narc-core), and the huge-pages work shows Pavlov still optimises the hot path.

---

## 12. Cross-check: RAR5 and zpaqfranz on the same axes

| axis | 7z (26.02) | RAR5 (WinRAR 7.x) | zpaqfranz (v64.x, MIT) | narc today |
|---|---|---|---|---|
| dedup | **none** (format-inherent) | file-level via `0x0005 File copy`; hard links | **fragment-level CDC dedup** — closest prior art | chunk-level CDC |
| journaling / versions | none | none | **yes**, append-only, rollback | append-only, single version |
| recovery record | **none** (policy) | **yes**, Reed-Solomon, `rr`=3 % default | **none** | none |
| quick open / list without scan | `StartHeader` at offset 0 → O(1) | **`QO` service header + locator record** in main header | index blocks per transaction | footer + manifest |
| max dictionary | 4 GiB (`-md=8g` errors) | format field says up to 4096 MB; WinRAR 7 goes to 64 GB — 7-Zip 24.03 added `-smemx{size}g` and **defaults to a 4 GB limit** for RAR unpacking, prompting above that | n/a (CM/LZ77 per block) | chunk-bounded (16 MiB) |
| extraction RAM | **dict + 7.7 MiB** | dict-sized, up to 64 GB | model-sized | **chunk-sized** |
| file checksum | CRC-32 | CRC-32 or **BLAKE2sp-256**, masked when encrypted headers are off | SHA-1 upstream; **XXH3/BLAKE3** in franz | BLAKE3-128 |
| KDF / cipher | **no salt**, SHA-256 ×2^19, AES-256-CBC, no MAC | PBKDF2, **16-byte salt**, log-count, 12-byte check value, AES-256, no AEAD | AES-256-**CTR** via `-key` | not implemented |
| ADS / ACL | **"Not implemented"** | `STM` / `ACL` service headers | claimed: does **not** save symlinks/junctions | not yet |
| solid update | folder repack on first touch | solid flag = continue previous dictionary → repack | append-only, cheap | append-only, cheap |
| licence for *writing* | free (LGPL/unRAR mix) | **forbidden** (RARLAB) | MIT | ours |

zpaqfranz caveats worth copying into the design file, from its own docs and the author's own
statements: single-shot ratio is *not* its selling point ("there are faster programs out there and
others that achieve better compression ratios"); `-m5` is extremely slow (a user measured
~850 KB/s on 12 cores ≈ 20 min/GB); caps at 4×10^9 files / 250 TB post-dedup; practical advice is
to restart the archive every 1000–2000 versions on HDD; no recovery record; extracting a single
file from a stored disk image requires a temp extract. The ZPAQL bytecode VM bought forward
compatibility at enormous complexity cost and **nobody copied it** — version the format instead.

### The narc weakness this comparison exposes

| Silesia, 203 MiB | archive | compress | **extract to disk, 1 thr** | **extract to disk, 8 thr** |
|---|---|---|---|---|
| `7z -mx9 -ms=on` | 48 688 197 B (46.4 MiB) | 66.1 s / 2089 MiB | **1.88 s** / 209 MiB | 1.89 s / 256 MiB |
| `narc max` | **45 219 944 B (43.1 MiB)** | **36.5 s** / 701 MiB | **16.89 s** / 123 MiB | 6.67 s / 427 MiB |

narc wins ratio by 7.1 % and compression speed by 1.8×, and **loses decompression by 9.0× single
-threaded and 3.5× at 8 threads.** Root cause: PPMd7 is roughly symmetric, while LZMA2 decodes
~10× faster than it encodes. Archives are written once and read many times; a max tier that is
9× slower to read is a real product problem, and on a weak single-core PC (an explicit project
requirement) it is disqualifying. Options, in order of preference:

1. Make the analyzer's PPMd-vs-LZMA2 tournament **cost-aware**: only take PPMd when it wins by more
   than a threshold (measure it — on the ROADMAP's own prose data PPMd won by 24 %, on binaries
   LZMA2 won by 16 %).
2. Record a per-archive "decode class" so `extract` can auto-enable parallelism (already 2.5×) and
   the GUI can warn.
3. Offer `max` (balanced, LZMA2-leaning) and `max-ratio` (PPMd allowed) rather than one tier.

---

## 13. Reproduction

All artefacts live in `D:\tmp\7zw\` (scratch, outside the repo):

* `mem.ps1` — timing + peak-working-set harness (`-exe`, `-argline`, `-tag`).
* `parse.py` — extracts and decodes the `7zAES` coder properties from a `.7z` (§8.1).
* `corrupt.py` — flips one byte at a given fraction of a file (§6.2).
* `dup1/` 4×enwik8; `dup2/` 3× source tree; `dup8/` 8× source tree; `src/` source tree; `sil/` Silesia.
* `zeros2g.bin` + `z{64m…1536m}.7z` — the decoder-memory ladder (§5).
* `sil-solid.7z`, `sil-16m.7z`, `sil-c16m.7z` — the decode-parallelism triple (§4).

**Harness gotcha that cost real time:** in bash, `"D:\tmp\7zw\$name.7z"` inside double quotes makes
`\$` a literal `$`, so the path silently became `D:\tmp\7zw$name.7z`, 7-Zip found no archive, and
runs "completed" in 0.11 s with a 7 MiB peak. Use forward slashes (`D:/tmp/7zw/...`) or `${name}`.
Any measurement here showing ~0.1 s and ~7 MiB peak is a failed invocation, not a fast one.
