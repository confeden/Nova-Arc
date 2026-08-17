# Research 15 — The non-neural algorithmic frontier

**Question**: what has lossless compression learned since LZMA (2001) and PPMd var.H (2002)
that a format designed in 2026 should use? Neural methods are explicitly out of scope
(covered elsewhere); this report is about deterministic, shippable algorithms.

**Compiled**: 2026-08-17. All web sources dated. All "MEASURED HERE" numbers were produced
on the owner's machine on 2026-08-17 (8 logical cores, Windows 11, MSYS2 `xz`,
`--format=raw --lzma2=preset=9e -T1`).

**Reviewed**: 2026-08-17 by a second (sceptic) reviewer. Verdicts #4 and #6 downgraded
ship-now → prototype, #5 prototype → watch; §1 re-measured with a real `preset_dict` in
`lzma-rust2` (bench kept at `test/presetbench`, Appendix B); missed items in §11.
Everything the reviewer changed or added is tagged **[REVIEW]**.

---

## 0. Verdict table

> **Sceptic's review pass, 2026-08-17 (second reviewer).** Every ship-now/prototype
> verdict was re-attacked: benchmark reproducibility, project liveness and licence,
> survival under *our* constraints, decode cost. §1's mechanism was re-measured with a
> **real `preset_dict` call in narc's own codec** (`lzma-rust2`, not liblzma) and the
> author's proxy held to 0.3% — but all three of the actionable verdicts moved down, and
> §1's *value proposition* changed even though its mechanism survived. Corrections are marked **[REVIEW]** throughout;
> the reviewer's own numbers are marked **RE-MEASURED**.

| # | Idea | Realistic gain for narc | Cost | Verdict |
|---|------|------------------------|------|---------|
| 4 | **Cross-unit references (preset dictionaries / RLZ)** | mechanism verified; **−12% to −13% in bounded form**, same ratio a 32-64 MiB independent unit already gets — the real win is *keeping 4-16 MiB edit granularity at that ratio* | ~2-3 weeks *plus* an unsolved `compact` liveness problem; encoder RAM ~2×; decode RAM 2-3× unit | **prototype** (was ship-now) |
| 6 | **Similarity ordering of files before packing** (simhash/nilsimsa) | unmeasured *over extension-sort*, which we already do; the cited −10.2% IS extension-sort | ~3 days to measure, and it must be measured before it is believed | **prototype** (was ship-now) |
| 1 | Better optimal parsing (N-best arrivals in the LZ parser) | −1% to −2.7% *if* we ever write our own LZ | We do not own an LZ encoder; liblzma's parser is already good | **watch** |
| 2 | rANS/tANS/FSE entropy backend | ~0% ratio vs LZMA's binary range coder; only speed | Whole new codec | **reject** (as a ratio play) |
| 5 | BWT backend (libbsc-class) as a third tournament codec | LTCB margin is measured at **1000 MB blocks on pure XML**; on a real many-file tree the DwarFS author measured bzip3 **63% worse than lzma** | Rust binding to write from scratch; 5-6× block decode RAM × all extract workers | **watch** (was prototype) |
| 7 | Delta compression between similar chunks (first-class) | −5% to −40% on versioned/near-duplicate data, ~0% otherwise | 3-4 weeks; resemblance index; patch-chain depth limits | **prototype** (deferred) |
| 3 | OpenZL graph model | ~0% on our corpora (source, text, binaries); big on columnar/CSV/telemetry | Vendoring a C/C++ lib with an unstable format | **watch** |
| 8 | Succinct index for the file list | Only matters >10M entries; we are fine to ~1 TB today | 1-2 weeks | **watch** |
| — | ROLZ engine (Razor-class) | Unverifiable; no open reference implementation with published numbers | Whole new codec | **reject** |

---

## 1. THE central result: where narc's 36% source-tree gap actually comes from

The ROADMAP currently attributes the gap to "our compression unit is capped at 32 MiB".
**That attribution is wrong, and the correct one changes the plan.** I separated the two
confounded variables — *window size* and *unit independence* — by direct measurement.

### 1.1 Setup

`test/corpus` (5751 files, 113.41 MiB, avg 20.2 KiB) concatenated in `(extension, path)`
order — i.e. exactly 7-Zip's `-mqs=on` solid ordering. Compressor held constant at
`xz --format=raw --lzma2=preset=9e -T1` throughout, so only geometry varies.

Anchor: owner's earlier measurement on this same tree — 7-Zip 26.02 `-mx9` = **8.8 MiB**,
narc max = **12 MiB** (36% worse). My one-stream/16 MiB-dict figure below (8.824 MiB)
reproduces the 7-Zip anchor to within 0.3%, which validates the methodology.

### 1.2 Experiment A — one solid stream, sweep the *window*

| LZMA2 dictionary | Compressed | % of raw | vs 4 MiB dict |
|---|---|---|---|
| 4 MiB | 11.411 MiB | 10.06% | — |
| 16 MiB | **8.824 MiB** | 7.78% | −22.7% |
| 64 MiB | 8.549 MiB | 7.54% | −25.1% |
| 192 MiB | 8.534 MiB | 7.52% | −25.2% |

**The window saturates at 64 MiB.** Going 16 → 192 MiB buys 3.3%. There is no hidden
prize behind a giant dictionary on a source tree; this corroborates 7-Zip's own doc note
that `-mqs` and `-md` are substitutes ([7-Zip `-m` switch docs](https://documentation.help/7-Zip-18.0/method.htm)).

### 1.3 Experiment B — independent units (narc's actual constraint)

Same blob, cut into fixed units, each compressed **alone** with `dict = unit size`:

| Unit size | # units | Compressed | vs one stream @192 MiB (8.534) |
|---|---|---|---|
| 4 MiB | 29 | 14.988 MiB | **+75.6%** |
| 16 MiB | 8 | 10.723 MiB | **+25.6%** |
| 32 MiB | 4 | 9.411 MiB | +10.3% |
| 64 MiB | 2 | 9.262 MiB | +8.5% |

Compare the two tables at the same number, 16 MiB:

- 16 MiB **window** on one stream: 8.824 MiB (+3.4% over the 192 MiB window)
- 16 MiB **independent units**: 10.723 MiB (+25.6%)

**The penalty is not the window. It is the cold start.** Each unit must re-learn the
range-coder probabilities and cannot reference any earlier unit. That is 21.5% of ratio
(10.723 / 8.824) thrown away purely by unit independence, and it is the whole gap.

**[REVIEW] That paragraph is two-thirds wrong, and the error matters.**

1. *"Re-learn the range-coder probabilities"* is not the mechanism. A preset dictionary
   fills the match window and does **nothing** to the probability model — LZMA2 still
   resets state and props at the unit boundary. If probability warm-up were the cost,
   preset dicts could not fix it. RE-MEASURED below, they fix ~95% of it, so the cost is
   almost entirely **missing back-references**, i.e. window *content*.
2. *"The penalty is not the window"* is a false dichotomy. For an **independent** unit,
   window size and unit size are the same knob — the 16 MiB-window-on-one-stream row is
   not a design narc can reach. The two reachable designs are (a) bigger independent
   units and (b) cross-referencing units. Table 1.3 is therefore the relevant table, and
   it says the ROADMAP's attribution ("our compression unit is capped") is **correct**:
   4 MiB → 14.99, 32 MiB → 9.41, 64 MiB → 9.26. Enlarging the unit is a real lever worth
   12 → ~9.4 MiB with **zero format change**, and it is already ROADMAP plan item 5
   ("blocks > 16 MiB, which means not routing them through FastCDC").
3. What is genuinely new in §1 is not "the gap is cold start" but: *preset dictionaries
   decouple window size from unit size*, so we can have a 64 MiB-unit ratio at a 4-16 MiB
   edit granularity. That is the honest, and still valuable, claim.

### 1.4 Experiment C — buy the cold start back with preset dictionaries

LZMA2 supports a *preset dictionary*: bytes pre-loaded into the match window before
encoding, not emitted in the output. liblzma exposes `preset_dict` / `preset_dict_size` on
`lzma_options_lzma`; it works **only with raw LZMA1/LZMA2 streams, not inside the .xz
container** ([liblzma API docs](https://tukaani.org/xz/liblzma-api/structlzma__options__lzma.html);
confirmed by the XZ Utils author on
[SourceForge](https://sourceforge.net/p/lzmautils/discussion/708858/thread/e40fbf99/)).
narc already writes raw LZMA2 payloads, so this is available to us today.

I estimated per-unit warm cost as `C(D‖U) − C(D)` (standard proxy; see caveat below),
units fixed at 16 MiB:

| Scheme | Compressed | vs cold (10.723) | Selective-extract cost |
|---|---|---|---|
| Cold, independent (today) | 10.723 MiB | — | 1 unit decode |
| Depth-1, one global root (all units ← unit 0) | 9.751 MiB | **−9.1%** | 2 unit decodes |
| Depth ≤ 4 (a fresh root every 4 units) | 9.366 MiB | **−12.6%** | ≤ 4 unit decodes |
| Adjacent chain (unit *i* ← unit *i−1*, unbounded) | **8.793 MiB** | **−18.0%** | ≤ N unit decodes |
| Shared 16 MiB *strided-sample* dictionary | 7.582 + 2.202 stored = 9.784 MiB | −8.8% | 2 unit decodes |
| *(reference)* one solid stream, 192 MiB dict | 8.534 MiB | −20.4% | whole stream |
| *(reference)* 7-Zip 26.02 `-mx9` | 8.8 MiB | — | whole solid block |

Per-unit detail for the adjacent chain (MiB): 1.385, 0.907, 1.517, 1.388, 1.306, 1.457,
0.735, 0.098. Unit 4 cold costs 1.879 MiB vs 1.306 MiB warm — a 30% swing on one unit.

**Readings:**

1. An **unbounded adjacent chain reaches 8.793 MiB — it beats 7-Zip `-mx9` (8.8 MiB) with
   16 MiB compression units.** The differentiator (cheap edits) survives intact.
2. A **depth-capped chain is the shippable form.** Depth ≤ 4 gives −12.6% and bounds
   selective extraction at 4 unit decodes; full extraction pays *nothing* extra because
   units are decoded in order anyway and the previous plaintext is already in hand.
3. A **synthetic sampled dictionary is the worst option**: it does compress the units best
   (7.582 MiB) but you must store the 16 MiB sample (2.202 MiB compressed), and the sample
   duplicates content that is already in the archive. Net −8.8%, worse than depth-≤4 and
   with an extra invariant to maintain. **Do not build a dictionary trainer for this.**
   Use existing archive units as dictionaries — they cost zero bytes.
4. Because the window saturates at 64 MiB (§1.2), the dictionary need never exceed
   ~48-64 MiB, so extraction memory stays bounded: `dict buffer + unit buffer`.

**Caveats on Experiment C (be honest about these):**

- `C(D‖U) − C(D)` is a proxy for a real `preset_dict` run. It is close but not identical:
  a true preset dict resets the range coder and LZMA2 state at the unit boundary, which a
  concatenation does not. Expect the real number to land 1-3% *worse* than the proxy.
  **Re-measure with an actual `preset_dict` call before committing format bytes.**
  → **[REVIEW] Done. The proxy holds; see §1.4b.**
- Ordering is 7-Zip's `-mqs` (extension, path). narc's current solid grouping is similar
  but not identical, and narc routes big files through FastCDC, so its unit boundaries are
  content-defined rather than aligned. The gains transfer directionally, not exactly.
- The corpus is one source tree. Silesia already sits at ratio parity with 7-Zip, so this
  lever will show little there; that is expected and fine.

### 1.4b [REVIEW] RE-MEASURED with a real `preset_dict`, in narc's own codec

**First correction, and it invalidates a source, not a number: narc does not link
liblzma.** It uses `lzma-rust2` 0.19 (pure Rust, Apache-2.0) — see
`crates/narc-core/Cargo.toml`. The §1.4 argument "liblzma exposes `preset_dict`, narc
already writes raw LZMA2, so the mechanism is available today" is right by luck: the
mechanism does exist, but the evidence for it is `lzma-rust2`'s own API
(`LzmaOptions::preset_dict` on `Lzma2Writer`, `Lzma2Reader::new(inner, dict_size,
preset_dict)`), not the tukaani docs. Verified by reading the vendored crate.

Bench: `test/presetbench` (scratch crate, gitignored playground), same blob as §1
(5751 files, 118,921,271 bytes in `(ext, path)` order — byte-identical to the author's),
`lzma-rust2` preset 9 + `nice_len = 273`, one encoder per unit, warm runs assert a
**bit-exact roundtrip through `Lzma2Reader` with the same preset dict** (`[roundtrip OK]`).

| Geometry | Compressed | vs 7-Zip 8.8 | Selective-extract cost |
|---|---|---|---|
| 4 MiB units, cold *(closest to today's max tier)* | 14.949 MiB | +70% | 1 decode |
| 16 MiB units, cold | 10.690 MiB | +21% | 1 decode |
| **32 MiB independent units — no format change** | **9.389 MiB** | +6.7% | 1 decode |
| **64 MiB independent units — no format change** | **9.246 MiB** | +5.1% | 1 decode |
| 4 MiB units, dict = 1 ancestor, groups of K=4 | 10.701 MiB | +21% | ≤4 decodes |
| 4 MiB units, cumulative dict, groups of K=8 | 9.404 MiB | +6.9% | ≤8 decodes |
| **16 MiB units, cumulative dict, groups of K=4** | **9.248 MiB** | +5.1% | ≤4 decodes |
| 4 MiB units, dict = 1 ancestor, *unbounded* chain | 9.962 MiB | +13% | ≤29 decodes |
| 4 MiB units, cumulative 4-ancestor dict, *unbounded* | 8.805 MiB | +0.1% | ≤29 decodes |
| 16 MiB units, dict = 1 ancestor, *unbounded* chain | **8.781 MiB** | **−0.2%** | ≤8 decodes |

Per-unit, 16 MiB: cold `1.381 1.596 1.523 1.867 1.874 1.566 0.763 0.121`;
warm `1.381 0.905 1.512 1.385 1.304 1.462 0.734 0.098`.

**Readings — three of the author's four change:**

1. **The proxy was accurate, in the optimistic direction.** Real cold = 10.690 vs proxy
   10.723; real unbounded chain = 8.781 vs proxy 8.793. The author's "expect 1-3% worse"
   caveat was itself wrong; `C(D‖U) − C(D)` is a *conservative* estimator here. The
   mechanism works and is exactly reversible. This is the one claim that fully survives.
2. **Every *bounded* form lands at 9.2-9.4 MiB — exactly where a 32-64 MiB independent
   unit already lands, with no format change, no chain bookkeeping, no corruption
   amplification.** The "beats 7-Zip `-mx9`" headline requires an **unbounded** chain,
   which throws away the bounded-selective-extraction property the report claims to
   preserve. The defensible pitch is therefore *not* "−18%, closes the gap"; it is
   **"the ratio of a 64 MiB unit at a 4-16 MiB edit granularity"** — which is still the
   single best idea in this report, because cheap edits are the product.
3. **The gain is a function of dictionary BYTES, not of chain depth**, and the author's
   proposed format field cannot express that. At narc's real max-tier geometry (FastCDC
   1/4/16 MiB, average ~4 MiB) a single-ancestor dictionary is only ~4 MiB and yields
   9.962; reaching 8.805 needs ~16 MiB of ancestors, i.e. a **cumulative multi-ancestor**
   dictionary. `dict: Option<ChunkId>` + `dict_depth: u8` describes a *parent pointer*;
   the thing that pays is a *dictionary of the last N ancestors*, whose RAM and decode
   count scale with N. Note also that depth K=4 at 4 MiB units is worth almost nothing
   (10.701 ≈ cold-16 MiB): one cold root per 4 units eats the whole gain.
4. The author's §1.4 reading 3 ("do not build a dictionary trainer") over-generalises from
   one strided-sample run — see the RLZ literature in §1.4c, where dictionary construction
   as a set-cover problem beats pruning-based samplers by up to 27%. Keep the conclusion
   (use existing units — they cost zero bytes) but drop the certainty.

### 1.4c [REVIEW] Prior art: this idea is called Relative Lempel-Ziv

§1 presents cross-unit dictionaries as a fresh observation. It is a 15-year-old named
technique with a literature that already answers §1's open question ("which unit should be
the dictionary?"):

- Kuruppu, Puglisi, Zobel, *Relative Lempel-Ziv Compression of Genomes for Large-Scale
  Storage and Retrieval*, SPIRE 2010 — store one reference in plain text, parse every other
  string into phrases that occur **in the reference**; independent per-document decode.
- Kuruppu et al., *RLZ Factorization for Efficient Storage and Retrieval of Web
  Collections*, [arXiv:1106.2587](https://arxiv.org/pdf/1106.2587) — sampled dictionaries
  work at collection scale, but "parts of it are never used".
- Liao, Petri, Moffat, Wirth, *Effective Construction of Relative Lempel-Ziv Dictionaries*,
  [WWW 2016](https://dl.acm.org/doi/10.1145/2872427.2883042) — reference construction as a
  **string covering / greedy set-cover** problem; **up to 27% better than the best pruning
  method (CARE)** on multi-GB collections. This is the algorithm to steal for "pick the
  dictionary", instead of inventing a heuristic.
- *Hierarchical Relative Lempel-Ziv*,
  [SEA 2023](https://drops.dagstuhl.de/entities/document/10.4230/LIPIcs.SEA.2023.18) —
  different references for different subsets of the collection. That is precisely the
  "fresh root every K units" design, already studied.
- Documented weakness, directly aimed at us: RLZ as described uses a **static** dictionary,
  "against which encoding of new data may be inefficient" — i.e. an append-only archive
  whose later sessions keep referencing early roots will decay. Nothing in §1 addresses
  what happens on the tenth incremental `add`.

### 1.5 What this means for the format

A unit record needs one new optional field: `dict: Option<ChunkId>` plus a
`dict_depth: u8` so readers can refuse pathological chains. Rules that keep every existing
invariant:

- The dictionary must be an **already-committed** unit (append-only preserved: we never
  rewrite bytes, we only reference backwards).
- `dict_depth ≤ K` (K = 4 proposed) — enforced at write time, verified at read time.
  Guarantees selective extraction ≤ K decodes and ≤ K·unit_size transient bytes.
- Editing a file rewrites only its own unit; the dictionary unit is untouched and its
  other referrers are unaffected. **Cheap-edit invariant holds.**
- `compact` must not drop a unit that is still referenced as a dictionary — same liveness
  rule already used for dedup sources, one extra edge in the reachability set.
- Dedup/integrity unaffected: the chunk hash still covers original plaintext, and the
  dictionary is not part of the hashed input.

**Risk to name explicitly**: the dictionary reference makes a unit non-self-contained. A
corrupted dictionary unit now damages every unit that references it. Mitigation: cap K, and
prefer roots that are themselves small and well-replicated; optionally allow a unit to
record a "cold size" so a repair tool knows the loss is bounded.

**[REVIEW] Four things §1.5 gets wrong or omits — all of them cost format bytes or RAM.**

1. **`compact` cannot stay a pure garbage collector.** "One extra edge in the reachability
   set" understates it: after editing a file, its *old* unit is dead data that is now
   **pinned alive as a dictionary** for every later unit that referenced it. Today
   `compact` reclaims that space; with dictionary edges it must either keep dead plaintext
   forever (archives stop shrinking — the feature quietly leaks) or **re-encode every
   referrer**, which is a partial repack, i.e. the exact cost narc exists to avoid.
   Neither branch is written down in §1. This is the single unsolved design problem and
   the reason the verdict moved to *prototype*: it must be answered before format bytes.
2. **The dictionary-size invariant breaks.** `codec.rs::lzma2_dict_size()` deliberately
   does **not** store the window size — both sides derive it from `unpacked_len`, with the
   documented rule "a decoder window may be wider than the encoder's, never narrower". A
   preset dict violates this: the window must hold `dict_len + unit_len`, so a 4 MiB unit
   with a 16 MiB dictionary needs a 20 MiB window that the reader cannot derive. The
   format must now carry the window (or a rule pinning it to `dict_len + unit_len`), and
   the geometry record is the natural place.
3. **Memory: encoder ~2×, decoder 2-3× per worker — not "+1 dict buffer".** bt4 costs
   ~11× the window (already noted in `analyze.rs`), and the window must now cover dict +
   unit, so a 16 MiB unit with a 16 MiB dictionary goes from ~180 MiB to ~350 MiB of match
   tables **per packing worker**; the cumulative-dictionary variant that actually delivers
   8.8 MiB is worse. On the decode side: window (dict+unit) + the ancestor plaintext we
   must hold + the output ≈ 2.5-3× unit, and extraction runs **all cores** for LZMA2
   (`archive.rs`), so the multiplier is per worker. The claim "extraction memory +1
   dictionary buffer (≤16 MiB)" is off by 3-5× against a hard constraint.
4. **The tournament changes shape, and PPMd7 gets nothing.** `ppmd-rust` exposes no preset
   model, so on every unit where PPMd7 wins (prose, wiki, db records — 13-24% per the
   ROADMAP) this lever is worth **zero**. RE-MEASURED on `test/enwik8` at 16 MiB units:
   warm LZMA2 buys only −4.2% on prose (28.18 → 27.01 MiB over the text units), which is
   nowhere near PPMd7's margin, so the tournament outcome does not even flip. The −18%
   number is a *source-tree* number, on units where LZMA2 already wins. Also: every
   tournament entrant must now be run **with** its dictionary, so pack cost per unit rises
   with the dictionary bytes, on top of the memory above.

---

## 2. Optimal parsing — how much is actually left?

### 2.1 Theory

The bit-optimal LZ77 parse is a shortest-path problem on a DAG whose edges are candidate
phrases weighted by their encoded bit cost. Ferragina, Nitto & Venturini,
*Bit-Optimal Lempel-Ziv compression* ([arXiv:0802.0835](https://arxiv.org/abs/0802.0835),
2008-02-06; SODA '09 pp. 768-777) gave the first algorithm computing the bit-optimal parse
in `O(n log n)` time and `O(n)` words for the usual variable-length integer encoders.
Greedy LZ77 is optimal in the *number* of phrases, never in bits.

How much can greedy lose? Kosolobov, *Relations Between Greedy and Bit-Optimal LZ77
Encodings* ([arXiv:1707.09789](https://arxiv.org/abs/1707.09789), STACS 2018,
[DOI 10.4230/LIPIcs.STACS.2018.46](https://drops.dagstuhl.de/entities/document/10.4230/LIPIcs.STACS.2018.46)):
greedy is within `O(log n / log log log n)` of bit-optimal on constant alphabets, and the
bound is **tight** — adversarial strings exist where greedy is asymptotically much worse.
That is a worst-case result with no bearing on real files.

### 2.2 Practice — the reference points

| Data point | Gain | Source |
|---|---|---|
| Zopfli vs zlib `-9` (deflate) | **3-8% smaller**, ~100× slower encode, decode unchanged | [Google Developers Blog, 2013](https://developers.googleblog.com/compress-data-more-densely-with-zopfli/); range corroborated by [HTTP Archive Web Almanac 2021](https://almanac.httparchive.org/en/2021/compression) |
| Zopfli on already-optimized PNG | **1-5 bytes** | practitioner reports; the gain evaporates near the entropy floor |
| Adding REPDIST checks *inside* the optimal parser | **up to +3% on binaries** | [encode.su "[LZ] Optimal parsing"](https://encode.su/threads/1895-LZ-Optimal-parsing) |
| LZMA-style forward parse, 1 arrival → 4 arrivals per position | 9,780,036 → 9,512,780 bytes = **−2.7%** (lzma's own best on that file: 10,319,525) | same thread |
| ECT vs AdvanceCOMP on a JSON sample | 36,657 vs 36,691 bytes = −0.1% | practitioner report |

So the honest range for parser work is **1-3% on top of an already-good optimal parser**,
and **3-8% only if your baseline is a greedy/lazy parser like deflate's**.

### 2.3 What it would take in Rust — and why not to

liblzma's `GetOptimum()` already does forward optimal parsing with rep-slot awareness,
composite candidates (`literal+rep0`, `rep+literal+rep0`, `match+literal+rep0`) and
`prev2` multi-step arrivals. We call liblzma; we do not own that code. The only way to
capture the remaining 1-3% is to write our own LZ encoder with N-best arrivals — several
months, and it would have to beat a 25-year-tuned encoder to break even.

**Verdict: watch.** The 1-3% here is 6-18× smaller than the 18% sitting in §1, for 10× the
work. Revisit only if narc ever grows its own LZ backend for other reasons.

*One cheap exception worth 30 minutes*: liblzma exposes `nice_len` (2-273, default 64) and
`depth`. The ROADMAP says max tier already raises `nice_len`. Confirm we are at 273 with
`mf=bt4` and `mode=normal` on the max tier — that is free parser quality.

---

## 3. Entropy coding — rANS/tANS/FSE, and what zstd/LZMA leave on the table

### 3.1 The actual accounting

There are two different questions, and conflating them is the usual error.

**(a) Coder precision.** rANS introduces no approximation beyond probability quantization;
Giesen's design claim is explicit that the decoder is division-free "without introducing
any approximations that hurt coding efficiency"
([ryg blog, 2015-12-21](https://fgiesen.wordpress.com/2015/12/21/rans-in-practice/)).
tANS adds a small extra loss from symbol placement in the table. Kosolobov,
*Efficiency of ANS Entropy Encoders* ([arXiv:2201.02514](https://arxiv.org/pdf/2201.02514))
formalizes this. **LZMA's adaptive binary range coder has essentially zero quantization
loss already** — it is 11-bit probabilities updated per bit. There is nothing to win.

**(b) Model adaptivity.** This is where zstd loses, not LZMA. zstd uses *block-static*
FSE/Huffman tables: it must transmit tables and cannot adapt within a block. Bloom's
matched-configuration benchmark (12-bit tables for FSE/rANS/arith) shows the three
coders within a few bytes of each other on order-0 data — e.g. book1, `H = 4.527 bpb`,
arith-12 → 435,378 bytes; ryg_rans rans64 → 435,116 bytes; interleaved rANS → 435,120
(4 bytes worse for ~45% more throughput)
([cbloom, 2014-02-01](http://cbloomrants.blogspot.com/2014/02/02-01-14-understanding-ans-3.html);
[ryg_rans README](https://github.com/rygorous/ryg_rans)).
Collet's own comparison concludes FSE gives "equivalent compression performance" to
arithmetic coding without the speed penalty
([fastcompression.blogspot.com, 2014-02](http://fastcompression.blogspot.com/2014/02/a-comparison-of-arithmetic-encoding.html)).
Bloom also notes zstd does **not** use tANS for literals — Huffman is close enough there
that tANS isn't worth the cost.

### 3.2 Conclusion

**ANS is a speed technology, not a ratio technology.** LTCB confirms this from the other
direction: zstd `-22 --ultra` = 215,674,670 bytes on enwik9 vs xz = 197,331,816
([Large Text Compression Benchmark](https://www.mattmahoney.net/dc/text.html), retrieved
2026-08-17). zstd loses 9% to xz and *none* of that is the entropy coder — it is window,
parse effort, and context modelling.

Rust availability, if ever needed: `constriction` (bamler-lab, rANS + range coding +
chain coding, actively released, 0.4.x), `ans` (minimal rANS, `no_std`, zero deps,
explicitly *not* tANS), `rans` (FFI over ryg-rans-sys, has interleaved streams).
**No native Rust tANS/FSE exists** — everyone binds zstd's C implementation.

**Verdict: reject as a ratio play.** narc's max tier is CPU-bound on LZMA2/PPMd, and its
fast tier is already zstd. Replacing an entropy backend buys 0% ratio. The *only*
legitimate future use is if we ever want a GPU-friendly or SIMD-interleaved codec, which is
research 08's territory.

---

## 4. OpenZL (Meta, 2025) — the format-aware compression graph

### 4.1 What it actually is

Collet et al., *OpenZL: A Graph-Based Model for Compression*
([arXiv:2510.03203](https://arxiv.org/abs/2510.03203), v1 2025-10-03, v2 2025-10-30;
announced on the [Meta engineering blog, 2025-10-06](https://engineering.fb.com/2025/10/06/developer-tools/openzl-open-source-format-aware-compression-framework/)).

The idea: express a compressor as a **DAG of typed modular codecs** (tokenize, transpose,
delta, split-by-struct, field-LZ, RLE, BWT, MTF, parse_int, then Huffman/FSE/LZ backends),
serialize *the graph itself* into the frame, and ship **one universal decoder** that
reconstructs the pipeline from the frame. A training pass searches for a good graph on
sample data; runtime "control points" read cheap statistics (run-length, histogram skew,
delta variance) to pick a branch. So a new compressor is a config, not a new binary — the
deployment win is that readers never need updating.

Repo: [facebook/openzl](https://github.com/facebook/openzl), **BSD licensed**, C11 + C++17.
v0.1.0 October 2025; **v0.2.0 May 2026** — new SDDL2 compiler, native LZ codec
`ZL_GRAPH_LZ` claimed 10% faster compression / 70% faster decompression than zstd level 1
on Silesia, and `zli` now auto-chunks multi-GB inputs at ~16 MB
([Phoronix, 2026-05-08](https://www.phoronix.com/news/OpenZL-0.2-Released)).
Community Rust bindings exist: `openzl-sys` (LDeakin, raw FFI, Apache-2.0/MIT) and
`rust-openzl` (vitorpy, safe wrapper, vendors the C lib). Meta's own bindings are Python
and C++, not Rust.

### 4.2 The numbers, and the part everyone omits

Reported wins are on **structured/numeric** data: on `ppmf_person` (US census PPMF) OpenZL
is stated as "55% better than xz-9 and 11 times as fast"; ERA5 pressure/wind grids beat
xz-9 on ratio while running an order of magnitude faster. The paper's own honesty is the
useful part:

> On enwik7, OpenZL's training finds no improvement and **simply uses zstd-6**; the
> specialized text compressor xwrt beats the next-highest ratio by over 30% and OpenZL by
> almost 60%. The authors write that OpenZL "is not a magic bullet for all use cases" and
> note the absence of optimized text-specific codecs.

They also flag that **decompression is much slower** for parse-heavy formats like CSV,
because parsing happens on the decode path too.

### 4.3 Should narc adopt the model?

**No, and here is the precise reason.** narc's three real corpora are source trees, prose,
and binaries — exactly the three cases where OpenZL degrades to "call zstd". Its wins are
in columnar/tabular/numeric-array data (Parquet, telemetry, scientific grids), which is
not what a desktop archiver sees. Adopting it would mean vendoring a C/C++ library whose
"API, compressed format, and set of codecs are all subject to change", inside a
`#![forbid(unsafe_code)]` core, to gain ~0% on our benchmarks.

**What to steal instead — the idea, not the code.** narc's `analyze::plan()` is already a
one-level version of this: content class → codec + filter. OpenZL's contribution is
(i) *typed* transforms composed into a graph, and (ii) *the graph is written into the
frame*, so the decoder is generic. narc's per-chunk `(codec_id, filter_id)` byte pair is a
depth-2 fixed graph. Generalizing it to a short **transform list** (e.g.
`[delta(4), bcj, lzma2]` encoded as a few bytes) is a genuinely good format decision that
costs almost nothing now and prevents a format break later when we add
transpose/split-by-field for numeric data. Do that. Skip the dependency.

**Verdict: watch.** Re-evaluate if narc ever targets database dumps, Parquet, logs, or
scientific arrays. Track the format-stability statement — it is currently a blocker for an
archive format that must read its own files in ten years.

---

## 5. BWT revival — libbsc, bzip3, kanzi

### 5.1 The numbers (LTCB, retrieved 2026-08-17)

From [mattmahoney.net/dc/text.html](https://www.mattmahoney.net/dc/text.html). Columns are
enwik8 / enwik9 bytes, then compression and decompression time in **ns/byte**, then MB of
memory. **Critical caveat: LTCB timings were taken on different machines over ~20 years**
(the default reference machine is an Athlon-64 3500+ under 32-bit Windows XP), so the time
columns are *not* mutually comparable. Sizes are.

| Program | Options | enwik8 | enwik9 | Comp | Decomp | Mem (MB) | Alg |
|---|---|---|---|---|---|---|---|
| kanzi | `-b 1024M -t RLT+TEXT+UTF -e TPAQX` | 19,098,186 | 161,690,495 | 490 | 480 | 3100 | CM |
| bsc 3.25 | `-b1000 -e2` | 20,786,794 | **163,884,462** | 23 | 8 | 5000 | **BWT** |
| bzip3 | `-b 511` | 20,749,611 | 169,990,721 | 175 | 146 | 3700 | **BWT** |
| 7zip 4.46a | `-m0=ppmd:mem=1630m:o=10` | 21,197,559 | 178,965,454 | 503 | 546 | 1630 | PPM |
| ppmd J1 | `-m256 -o10 -r1` | 21,388,296 | 183,964,915 | 880 | 895 | 256 | PPM |
| xz 5.2.1 | `--lzma2=preset=9e,dict=1GiB,lc=4,pb=0` | 24,703,772 | 197,331,816 | 5876 | 20 | 6000 | LZ77 |
| zstd 0.6.0 | `-22 --ultra` | 25,405,601 | 215,674,670 | 701 | 2.2 | 792 | LZ77 |
| brotli | `-q 11 -w 24` | 25,764,698 | 223,597,884 | 3400 | 5.9 | 437 | LZ77 |

**libbsc beats xz by 17% on enwik9 and beats our current best text codec (PPMd) by 11%.**
bzip3 beats xz by 14%. These are large, real, reproducible margins on text.

Third-party corroboration: [MaskRay's 2025-08-31 benchmark](https://maskray.me/blog/2025-08-31-benchmarking-compression-programs)
places kanzi Pareto-superior to xz on enwik8 compression speed but slower to decompress,
and groups LZMA/bzip2/bzip3/bsc/zpaq/kanzi/Oodle Leviathan as the high-ratio/low-speed
cluster. It also documents a real measurement trap: **lzbench 2.0 let kanzi use half the
available threads while other codecs were pinned to one core**; lzbench 2.0.1 fixed that.
Distrust any BWT-vs-LZMA number that does not state thread count and block size.

### 5.2 The memory/latency profile — the part that decides it

| Property | LZMA2 | BWT (libbsc/bzip3) |
|---|---|---|
| Decode memory | ~dict size (+64 KB) | **≈ 5N for the block** (SAIS array + swap buffer) |
| Asymmetry | decode ~10× faster than encode | **symmetric** — decode ≈ encode |
| Random access inside a block | streaming from block start | must inverse-transform the **whole block** |
| Non-text data | strong | weak — "BWT excels in text and only in text" (encode.su) |

This is a direct conflict with narc's hard constraint "extraction must run in bounded
memory on a weak PC (today ~10-80 MiB)". A 16 MiB BWT block needs ~80 MiB to invert. At our
current chunk sizes that is *at the ceiling*, not beyond it — but it removes all headroom,
and it makes decode as slow as encode.

### 5.3 What it would take in Rust

Good news: the plumbing exists and is fresh.

- `libsais` + `libsais-sys` (feldroop, 2025) — safe generic wrapper over Grebnov's
  [libsais](https://github.com/IlyaGrebnov/libsais) (**Apache-2.0**), linear-time SA/BWT/
  unBWT, OpenMP optional, `..64` variants for >2 GB.
  [docs.rs/libsais](https://docs.rs/libsais/latest/libsais/)
- `divsufsort` 2.0 (pure Rust port of libdivsufsort), `libdivsufsort-rs`, `divsufsort-rs`.
- **No Rust binding for libbsc itself.** libbsc is Apache-2.0 and would need a small
  `-sys` crate; note it also ships optional CUDA paths we would not use.
- bzip3 is **LGPL** — a licensing consideration for a not-yet-licensed project; its
  libsais/LZP parts are Apache-2.0 (Grebnov).

Realistic scope: SA-based BWT via `libsais` + LZP pre-pass + a QLFC or order-0 CM entropy
stage is ~3-4 weeks to a working codec and probably 2 months to competitive tuning. Cheaper
alternative: bind libbsc (Apache-2.0) and add codec id 4.

**Verdict: prototype.** Specifically: add libbsc (or libsais+QLFC) as a **fourth entrant in
the max-tier tournament for text-class units only**, with a hard rule that BWT is only
selected when `5 × unit_size ≤ memory_budget`. The tournament architecture already makes
this safe — if BWT loses, nothing changes; if it wins by 11-17% on text units, that is the
second biggest number in this report. Measure decode memory and time before pinning any
format constant.

### 5.4 [REVIEW] Downgraded to **watch**. The LTCB margin does not transfer.

The sizes in §5.1 are correct — I re-checked every row against
[LTCB](https://www.mattmahoney.net/dc/text.html) (retrieved 2026-08-17): bsc 3.25
`-b1000 -e2` = 20,786,794 / 163,884,462; bzip3 `-b 511` = 20,749,611 / 169,990,721;
xz 5.2.1 `--lzma2=preset=9e,dict=1GiB,lc=4,pb=0` = 24,703,772 / 197,331,816. The page also
confirms "Not all tests are done on the same computer", so §5.1's timing caveat is fair.
What does not survive is the *inference* from those rows to narc:

1. **Block size.** Those margins are measured at **1000 MB and 511 MB blocks** on a single
   stream of pure XML text. narc's units are 4-32 MiB — 30-250× smaller. BWT ratio is a
   strong function of block size (bzip3's own manual: bigger block = better compression,
   and it exposes `-b` *instead of* levels for that reason), so the 17% is an upper bound
   measured in a regime we cannot enter. Nobody has published bsc/bzip3 at 16-32 MiB
   blocks vs LZMA2/PPMd at the same size. **That is a one-hour CLI experiment and it must
   come before any binding work.**
2. **Direct counter-evidence on our corpus shape, from the very project §6 leans on.**
   The DwarFS author benchmarked bzip3 as a DwarFS compressor
   ([discussion #110](https://github.com/mhx/dwarfs/discussions/110)): 69 blocks from a
   4.2 GiB image of 1000+ Perl installations (mixed text + binaries) — bzip3 1.1.8
   `-b 511` = **488 MiB / 63.5 s**, zstd-9 = 467 MiB / 14.7 s, **lzma ≈ 300 MiB**. BWT lost
   to LZMA by **63%** on a many-file tree at a huge block size, and his conclusion was
   "I currently don't see `bzip3` outperforming any of the existing options except in edge
   cases". §5 cites DwarFS four times and misses this.
3. **Our text units go to PPMd7, not LZMA2.** So BWT's real opponent on text is PPMd7
   (order 10/16) at 4-32 MiB, where the published 11% margin (bsc at 1000 MB vs 7-Zip PPMd
   at 1630 MB pool) says nothing.
4. **Memory is worse than stated.** §5.2 says a 16 MiB block "needs ~80 MiB to invert…
   at our current ceiling". Extraction uses **all cores** for slow codecs, so the real
   figure is `workers × 5-6 × block` = 8 × 80-96 MiB ≈ **0.6-0.8 GiB**, against a hard
   constraint of 10-80 MiB total. bzip3's manual puts it at ~6× block. A BWT unit is also
   the one thing that *cannot* take a §1 dictionary, so it must beat **warm** LZMA2
   (8.78 MiB-equivalent on the source tree), not the cold 10.69.
5. Licensing/plumbing claims check out: libbsc is Apache-2.0 (README copyright 2009-2025),
   bzip3 is LGPL with Apache-2.0 libsais/LZP parts, and **there is still no libbsc Rust
   binding on crates.io** (the `bsc` crate is an unrelated Beanstalkd client). So the cost
   side of §5.3 (write a `-sys` crate, 3-4 weeks) is if anything understated.

**Revised verdict: watch**, gated on one cheap measurement — run `bsc -b16/-b32` and
`bzip3 -b16` against LZMA2 and PPMd7 on `test/corpus` and on `test/enwik8` slices at
*narc's* unit sizes. If BWT does not win there, the 3-4 weeks are dead, and the DwarFS
data point says it probably will not win on anything but pure prose.

---

## 6. Similarity ordering of files before packing

### 6.1 Published evidence

The strongest real-world system is **DwarFS** ([mhx/dwarfs](https://github.com/mhx/dwarfs),
v0.14.1). It orders inodes by a similarity hash before segmenting and compressing.
`--order` supports `none | path | revpath | similarity | nilsimsa | explicit`. Nilsimsa is
"typically better than `similarity`" but slower, tunable via `max-children` and
`max-cluster-size` (larger clusters compress better but cost quadratically), implemented as
a parallel deterministic divide-and-conquer clustering
([mkdwarfs docs](https://github.com/mhx/dwarfs/blob/main/doc/mkdwarfs.md)).

Its headline run on 1139 Perl installations (47.49 GiB, 1.9M files, 330,733 dirs):
144,675 inodes ordered by nilsimsa in **91.13 s**; final image **430.9 MiB**
(ratio 0.00883). The build log breaks the savings down:

- deduplication: **28.2 GiB** saved (1,782,826 duplicate files)
- **segmentation: 15.19 GiB** saved
- compression: the rest, down to 430.9 MiB

Against competitors on the same corpus: SquashFS-zstd 4.7 GiB (DwarFS 11× smaller),
SquashFS-lzma 3.8 GiB vs DwarFS 315 MiB (12×), `zpaq -m5` 490 MiB in 47m 8s,
`lrzip -L9` 500 MiB in 57m 32s, wimlib 1.0 GiB, EROFS-lzma9 2.3 GiB in 2h 39m.

7-Zip's `-mqs=on` (sort by extension) is the crude version of the same idea. One reported
case: 6,661,786,104 → 6,041,019,462 bytes (90.68%) by name vs 5,427,406,392 (81.47%) by
type — **−10.2%** ([7-Zip forum](https://sourceforge.net/p/sevenzip/discussion/45797/thread/f7b41953/)).
7-Zip's docs are careful to note `-mqs` and a bigger `-md` are substitutes, and that
non-name order costs HDD seek performance.

**Honest caveat**: DwarFS publishes no clean A/B isolating nilsimsa ordering from dedup and
segmentation. Nobody does. The defensible claim is "ordering is a multiplier on cross-unit
matching", not "ordering alone buys X%".

### 6.2 What it would take in Rust

We already sort solid-block members by extension. The upgrade path:

1. Compute a cheap similarity sketch per file. Options in increasing cost/quality:
   simhash over 4-byte shingles (fast, ~64 bits), nilsimsa (256-bit, trigram-based),
   or MinHash/super-features (§7). For files > some threshold, sketch only a sample —
   DwarFS has `--max-similarity-size` for exactly this.
2. Order by recursive clustering, not by full pairwise TSP. DwarFS's divide-and-conquer
   (cluster to ≤ `max-children` centroids, recurse) is deterministic and parallel;
   that determinism matters for us because narc's chunk hashes must be reproducible.
   Note that the naive TSP framing (Xerox US 5,787,420 uses `1 − ρᵢ·ρⱼ` as a TSP distance
   and breaks the cycle at maximum dissimilarity) is unnecessary — clustering suffices.
3. Keep extension as a *tie-break and a group boundary*, not the primary key: BCJ/delta
   filter choice is still per-group, and we cannot mix filters in one unit.

Effort: ~3 days for simhash + recursive clustering. No new dependency needed
(blake3 is already there; simhash is 40 lines).

**Verdict: ship-now, but sequence it after §1.** Ordering alone gives little if units are
still independent — it mostly makes neighbours similar, and that only pays off if a unit
can *reference* its neighbour. Ship §1 first, then ordering, and measure the pair.

### 6.3 [REVIEW] Downgraded to **prototype**: "ship-now" on zero isolated evidence

The evidence problem is worse than §6.1's own caveat admits.

1. **The one quantified number cited is the thing narc already does.** The 7-Zip
   −10.2% is *sort by name* → *sort by extension* (`-mqs=on`). narc's solid blocks are
   already sorted by extension (`archive.rs::solid_group_key`). So the cited −10.2% is the
   baseline, not the upgrade. The delta of **simhash/nilsimsa over extension-sort** — the
   actual proposal — is unmeasured by 7-Zip, by DwarFS, and by this report.
2. **DwarFS publishes no ordering A/B, and its docs confirm it.** Fetched
   [mkdwarfs.md](https://github.com/mhx/dwarfs/blob/main/doc/mkdwarfs.md) 2026-08-17: the
   modes are documented, and there is **no quantitative comparison of ratio across
   ordering modes** anywhere in it. The 47.49 GiB → 430.9 MiB headline is dedup (28.2 GiB)
   + segmentation (15.19 GiB) + compression; ordering's share is not separated. Citing it
   as evidence for a −2% to −8% ordering gain is unsupported. (Good news for us:
   "`nilsimsa` ordering is now completely deterministic", so the determinism requirement is
   satisfiable.)
3. **A cheaper deterministic option is skipped.** DwarFS also ships `revpath` — path
   traversed leaf-to-root — which groups by filename/suffix with **no sketching at all**.
   For a source tree that is most of what similarity ordering would find (`*.h` next to
   `*.h`, `Makefile` next to `Makefile`), and it costs nothing. Measure `revpath` before
   writing a simhash clusterer.
4. **Append-only limits the reachable gain, and §6 never mentions it.** Ordering can only
   be optimised *within one `add` invocation*. An archive grown over ten sessions — the
   differentiator use case — cannot be reordered without a repack, so the ordering win
   applies to the first pass and decays afterwards. This is the same static-reference decay
   the RLZ literature documents (§1.4c).
5. Cost note: "no new dependency, simhash is 40 lines" is true, but the clustering that
   makes it pay is not — DwarFS warns `max-cluster-size` cost grows **quadratically**, and
   its 144,675-inode ordering took 91.13 s. Ours must also be stable under re-`add`.

**Revised verdict: prototype**, and the prototype is one experiment, not a feature: on
`test/corpus`, compress the blob in (a) extension order, (b) revpath order, (c) simhash
cluster order, each cold and warm-with-dictionary, and report four numbers. If the spread
over extension-sort is under ~2% it is not worth a new invariant in the packer.

---

## 7. Delta compression between similar chunks as a first-class feature

### 7.1 The storage literature is mature and directly applicable

This is the "post-deduplication delta compression" line of work: after exact-match dedup,
find *similar* (not identical) chunks and store a delta. The problem is resemblance
detection — building a cheap sketch that groups similar chunks.

| System | Venue / date | Result |
|---|---|---|
| N-Transform super-features | Broder / Douglis-Iyengar (USENIX ATC '03) | baseline; compute-heavy (N linear transforms per Rabin fingerprint) |
| **Finesse** | [USENIX FAST '19](https://www.usenix.org/conference/fast19/presentation/zhang), pp. 121-128 | fixed-size subchunk features grouped into super-features; much faster than N-Transform |
| **Odess** | IEEE ICDE '21 pp. 480-491; extended in [ACM TOS 2023](https://dl.acm.org/doi/full/10.1145/3584663) | content-defined sampling + Gear hash. Detection stage **31.4× faster than N-Transform, 7.9× faster than Finesse**; end-to-end throughput **3.20× / 1.41×** higher; **1.22× better compression ratio than Finesse** at N-Transform's ratio. Sampling rate 1/128. |
| **Palantir** | [ASPLOS '24](https://henryhxu.github.io/share/hongming-asplos24.pdf) | +27.4% detection coverage over N-Transform SF and Odess; +95.8% over Finesse |
| Argus | [ACM TOS 2025](https://dl.acm.org/doi/10.1145/3747839) | more precise resemblance detection |

Known failure modes, worth writing into the ROADMAP as negative knowledge: **Finesse
suffers boundary-shift** (equal-length subchunks cannot absorb insertions/deletions);
**Odess generates useless features**. Both are documented in the follow-up papers.

### 7.2 Delta engines and their measured patch sizes

| Tool | Numbers | Source |
|---|---|---|
| HDiffPatch `-BSD` vs bsdiff4 | 7.72% vs 8.17% of original, ~**12× faster diff**, much less memory | [sisong/HDiffPatch](https://github.com/sisong/HDiffPatch) benchmark (Win 11, R9-7945HX; HDiffPatch 5.0.1, bsdiff 4.3, xdelta 3.1, zstd 1.5.7) |
| HDiffPatch native + zstd | smallest patches overall | same |
| `zstd --patch-from --ultra -21 --long=24` | competitive; **1.5.7 (2025) substantially improved both `--patch-from` ratio and high-level speed** | [zstd releases](https://github.com/facebook/zstd/releases) |
| bidiff (Rust) vs naive zstd-21 | Wine 4.18→4.19 sources: 21.96 MiB / 81.3 s naive → **182.26 KiB / 5.22 s**. Linux 5.3.13→5.4: 106.73 MiB → **6.14 MiB**. Chromium 78 (97→108): 79.73 MiB/117 s → 12.81 MiB/159 s | [divvun/bidiff](https://github.com/divvun/bidiff) |

Rust options: **`qbsdiff`** (bsdiff-4.x *format*-compatible, not byte-identical patches,
≤~4 GiB inputs, crates.io activity April 2025), **`bidiff`** (100% safe Rust, fuzzed,
>2 GiB inputs, pluggable zstd/brotli, uses a hand-ported divsufsort — but lib.rs page dated
September 2020), **`bsdiff`** (plain port, March 2025). No mature Rust HDiffPatch binding.

### 7.3 The archiver reality check

Deduplicating archivers already do fragment-level *exact* dedup and it is the single
biggest lever on versioned data. Mahoney's incremental benchmark
([mingw44 → mingw45](https://mattmahoney.net/dc/mingw.html)): zpaq `-method 5` 36,393,788
→ 73,505,007 bytes across the update, vs `rar -s -m5` 41,796,989 → 112,379,699 and
exdupe `-x3` 50,307,903 → 131,248,915. zpaq's fragment dedup nearly halves the update cost.
On the 10 GB benchmark, exdupe `-x3` = .3671 vs 7zip `-mx` = .3595 vs zpaq `-m5` = .2936
([10gb.html](https://mattmahoney.net/dc/10gb.html)) — but these are old versions
(exdupe 0.5.0b, zpaq 6.49, 7-Zip 9.20); treat as directional only.

**narc already has exact chunk dedup via blake3.** What we lack is *similar*-chunk delta.

### 7.4 Verdict and the trap to avoid

**prototype**, with two hard design constraints:

1. **Patch chains must be depth-capped**, exactly like §1's dictionary chains. Neither
   bsdiff nor Courgette can patch in reverse; if the newest version is stored as a patch
   against the oldest, reading it replays the whole chain
   ([zstd issue #2063](https://github.com/facebook/zstd/issues/2063) is the long-standing
   request for bidirectional deltas). Store recent data as roots, older data as patches, or
   cap depth.
2. **Do not build this before §1.** A preset dictionary *is* a generalized delta:
   compressing unit U with dictionary D and LZMA2 already emits "the difference between U
   and D" and does it with our existing codec, our existing memory model, and no new
   format machinery. §1 captures most of the same redundancy for a fraction of the work.
   Explicit delta only wins where the similar content is *far* apart in the archive
   (found by a resemblance index) rather than adjacent — which is exactly what §6's
   similarity ordering is designed to prevent.

So: **§1 + §6 first; measure what redundancy is still unclaimed; only then decide whether
an Odess-style resemblance index earns its complexity.** If we do build it, Odess is the
right sketch (1/128 content-defined sampling + Gear hash, ACM TOS 2023) and the patch
engine should be `bidiff`-style suffix-sort delta feeding into LZMA2, not a bespoke format.

---

## 8. Succinct / compressed indexes for the file list

### 8.1 Current position

narc keeps the whole manifest in RAM (MessagePack + zstd), which the ROADMAP notes is
"fine ≤ ~1 TB archives". The DwarFS data point for scale: it looks up 1.9M files in 2.8 s
(SquashFS: 5.3 s) and mounts a 1.9M-file image in 0.42 s, with metadata for ~200,000 files
(Ubuntu 20.04.2.0 desktop ISO) compressing from 5.3 MB to 57% with zstd / 49% with LZMA.
So a few million entries is simply not a problem for a plain packed structure.

### 8.2 What the frontier offers, if we ever need it

The standard stack: rank/select bit vectors → wavelet matrix / Quad Wavelet Tree over the
alphabet → FM-index on top, plus Elias-Fano (`SArray`) marking path boundaries. Auxiliary
rank/select indexes reach `O(1)` with ~5% space overhead.

Rust crates, checked 2026-08-17:

| Crate | Notes |
|---|---|
| [`sucds`](https://github.com/kampersanda/sucds) | broad succinct collection, **pure Rust, avoids `unsafe`** (matches `narc-core`'s `forbid(unsafe_code)`), good sparse `SArray` |
| `vers` | praised for performance and minimal overhead over the raw bit vector; better construction design; wavelet matrix |
| QWT | Quad Wavelet Tree (Ceregini, Kurpicz, Venturini 2024), 4-ary, lower latency |
| [`fm-index`](https://github.com/ajalab/fm-index) | count + locate; **no extract-from-arbitrary-position**; updated 2025 |
| `sview-fmindex` | builds the index into one contiguous pre-allocated blob — attractive for mmap'ing a manifest |

### 8.3 Verdict

**watch.** The real win would be *paged, mmap-able* metadata so `narc list` and the GUI's
virtualized list do not need the whole manifest resident — and for that, a sorted packed
array + Elias-Fano offsets (from `sucds`) is enough. An FM-index only pays for itself if we
want substring search over paths inside a huge archive, which is a GUI feature nobody has
asked for. `sucds`'s pure-Rust/no-`unsafe` stance is the deciding factor if we do it.

---

## 9. Things I checked and am rejecting

- **ROLZ / Razor-class engine.** Razor (Christian Martelock) has a real reputation on
  encode.su — "CM ratios with LZMA decompression speeds", 1.66N decode memory, dictionaries
  to 1023M, an lz/rolz hybrid engine, and dedup so huge dictionaries are unnecessary. But
  it is **closed-source with no published reproducible benchmark**, and the peer-reviewed
  ROLZ paper ([IEEE, 2019](https://ieeexplore.ieee.org/document/8801741/)) only claims
  RoLZ beats LZSS and LZP — not LZMA. The general-history framing is "highly optimized ROLZ
  can achieve *nearly the same* ratios as LZMA". Writing a new ROLZ engine to chase an
  unverified claim is exactly the kind of wrong optimism that costs weeks. **Reject.**
- **A giant LZMA2 dictionary (≥128 MiB).** MEASURED HERE: saturates at 64 MiB and buys
  3.3% over 16 MiB on a source tree (§1.2). Confirms and sharpens the existing ROADMAP
  entry. The lever is cross-unit reference, not window size.
- **Training a synthetic dictionary for narc's units.** MEASURED HERE: a 16 MiB strided
  sample compresses the units best (7.582 MiB) but costs 2.202 MiB to store, netting worse
  than a depth-capped chain of existing units (§1.4). Separately, zstd's own documentation
  puts dictionary gains at **~500% for <1 KB files but only ~10% at 64 KiB**
  ([zstd manual, via ROOT I/O paper arXiv:2004.10531](https://arxiv.org/pdf/2004.10531)),
  with gains "mostly effective in the first few KB". Our units are 16 MiB. **Reject** the
  trainer; keep the *mechanism* (preset dict) pointed at real archive units.
- **rzip/lrzip/srep as a separate pre-pass stage.** lrzip on a 646,963,200-byte kernel
  tree: 86,595,597 (`-L 9`) vs lzma -9's 97,704,505 = −11%
  ([ck-hack, 2012](http://ck-hack.blogspot.com/2012/03/lrzip-0612.html)). Ziganshin reports
  rep >1 GB/s and srep ~100 MB/s. But: pcompress testing found LZP/Delta2 have a
  **marginally negative** effect at LZMA `-l 13/-l 14`; DwarFS testing found srep `-m4`
  produced a file the same size as the DwarFS image. The gain exists only when redundancy
  spans distances beyond the back-end window on multi-GB data — and our §1 mechanism
  captures the same redundancy *inside* the codec, without a second pass, temp files as
  large as the input, or srep's history of data-corruption bugs under memory pressure.
  **Reject as a stage**; the underlying insight is adopted in §1.
- **OpenZL as a dependency.** See §4.3.
- **rANS/tANS for ratio.** See §3.2.
- **[REVIEW] Tuning `lc`/`lp`/`pb` per content class.** The obvious cheap idea, since
  LTCB's own xz entry runs `lc=4,pb=0` and narc sets none of these (`codec.rs` only touches
  preset, `nice_len` and `dict_size`, though `lzma-rust2` exposes `lc`/`lp`/`pb`/`mode`/
  `mf`/`depth_limit`). RE-MEASURED 2026-08-17 at 16 MiB units: source tree 10.690 →
  **10.683** MiB (−0.07%) cold and 8.781 → 8.779 warm; `test/enwik8` text units 28.179 →
  **28.120** MiB (−0.21%). LTCB's `lc=4,pb=0` pays off at a 1 GiB dictionary, not at ours.
  **Reject** — it is noise at our unit sizes, and it would cost a per-chunk parameter byte.
- **[REVIEW] Raising `nice_len` on the max tier** is already done (`codec.rs`, `level >= 18`
  → 273), so §2.3's "one cheap exception worth 30 minutes" is closed. Note the code's own
  warning: the deeper search cost **18%** on record-structured data with noisy fields, which
  is exactly the kind of regression an "obviously free parser quality" knob produces.

---

## 10. Recommended sequence

1. **Verify §1 with a real `preset_dict` call** (2 days). Write a throwaway Rust bench that
   calls liblzma raw LZMA2 with `preset_dict` set to a preceding unit, on `test/corpus`.
   Confirm the proxy numbers within 3%. If they hold, this is the plan; if not, stop.
2. **Format: unit → `dict: Option<ChunkId>` + `dict_depth: u8`, K = 4** (1 week).
   Extend `compact` liveness to dictionary edges. Add a reader check that refuses
   `dict_depth > K`. Tests: edit-a-file-with-dict-ref, compact-keeps-dict, forged deep
   chain, dictionary-unit-corrupted.
3. **Packer: pick dictionaries** (1 week). Simplest good rule — within a solid group,
   unit *i* references unit *i−1* until depth K, then a fresh root. Measure. Then try
   "reference the most similar earlier unit within the group" using the §6 sketch.
4. **§6 similarity ordering** (3 days) — simhash + deterministic recursive clustering,
   extension as group boundary. Measure §1+§6 jointly against 7-Zip `-mx9`.
5. **§5 BWT as a fourth tournament entrant, text-class only, gated on
   `5 × unit_size ≤ budget`** (3-4 weeks). Bind libbsc (Apache-2.0) or build on `libsais`.
   Report decode memory and decode time, not just ratio.
6. **Generalize `(codec, filter)` to a short transform list** (2 days) while touching the
   format anyway — the one durable idea worth taking from OpenZL.
7. Defer §7 (explicit delta) and §8 (succinct index) until steps 1-5 are measured.

**Expected outcome if 1-4 land**: source tree 12 MiB → ~9.4 MiB at depth 4, or ~8.8 MiB
with a deeper chain; 7-Zip `-mx9` is 8.8 MiB. Silesia unchanged (already at parity).
Edit-one-file cost unchanged. Extraction memory grows by one dictionary buffer
(≤ 16 MiB today), selective extraction by ≤ K unit decodes, full extraction by nothing.

### [REVIEW] Revised sequence

Step 1 is **done** (§1.4b) and the proxy held, so the question is no longer "does the
mechanism work" but "is it worth the format bytes given the alternative". Reordered by
`measured gain ÷ irreversible commitment`:

0. **Raise the max-tier compression unit first — it needs no format change and it is
   already ROADMAP plan item 5.** Route solid blocks (target 32 MiB at max) around
   FastCDC's 16 MiB cap so one block = one unit. RE-MEASURED value: 12 → ~9.4 MiB, i.e.
   ~70% of the total prize, for days of work and zero new invariants. Cost is a bigger
   edit blast radius, so measure edit-one-file before and after.
1. **Answer the two blockers for §1 on paper** (half a day, no code): what does `compact`
   do with a dead unit pinned as a dictionary, and where does the window size live now that
   it cannot be derived from `unpacked_len`? If the answer to the first is "re-encode
   referrers", the feature costs a partial repack and the whole pitch weakens.
2. **Prototype §1 in the cumulative multi-ancestor form** behind a feature flag, not the
   single-parent form: `dict: ancestors[N]`, roots every K, and measure peak RSS at pack and
   extract with all cores busy — the memory numbers in §1.5 are the ones most likely to
   kill it, and they were never measured.
3. **Measure §6 as an experiment** (extension vs revpath vs simhash, cold and warm).
   Do not add a clusterer before those four numbers exist.
4. **Measure the ceiling before spending more weeks** (§11 item 4): what do `zpaq -m5`,
   `bsc -b32`, `kanzi -e TPAQX` and `rar -m5` get on `test/corpus`? If the floor is ~8.5 MiB
   we are chasing 4% and should stop after step 0; if it is 6.5 MiB the whole plan is
   under-ambitious. One afternoon of CLI runs, and it reframes everything above.
5. §5 (BWT) only if step 4 says a BWT-class codec wins **at our block sizes**. §7, §8 stay
   deferred. §4's transform-list generalisation (2 days) is still worth doing while the
   format is open.

---

## 11. [REVIEW] What this report missed

Ranked by (value to narc) ÷ (effort). Items 1-3 are free: the code is already vendored.

1. **DwarFS "segmentation" — the 15.19 GiB line quoted in §6.1 and never explained.** It is
   not dedup, not ordering, and not a preset dictionary: it is a rolling-hash *cyclic
   lookahead matcher* that finds duplicate byte **ranges across files inside one large
   filesystem block**, emitting references instead of bytes. On DwarFS's own headline corpus
   it is the second-largest saving after exact dedup, larger than anything §1-§8 proposes.
   For narc it is the container-level version of §1: cross-unit reuse expressed as explicit
   references in the manifest rather than as codec state, which means a unit stays decodable
   from data we can name, `compact` can reason about it with the liveness machinery that
   already exists for dedup, and the corruption blast radius is enumerable. This deserved a
   section of its own; it is a genuine alternative to §1, not a footnote.
2. **Seven BCJ filters and BCJ2 are already vendored and unused.** `lzma-rust2` 0.19 ships
   `new_x86 / new_arm / new_arm64 / new_arm_thumb / new_ppc / new_sparc / new_ia64 /
   new_riscv` plus a whole `filter/bcj2` module; narc's `Filter` enum has exactly
   `BcjX86` + `Delta(1..32)`. ARM64 is not exotic in 2026 — Apple Silicon binaries, Linux
   aarch64 packages, every `.so` in an APK, Windows-on-ARM. The ROADMAP measures BCJ x86 at
   **+4.4-5.7%** on real executables; that number is currently unavailable on ARM content.
   Cost: new filter ids and detection in `analyze.rs`, zero new dependencies. BCJ2 (7-Zip's
   default for x86, stream-splitting rather than in-place patching) is the other free one.
3. **PPMd var.I (PPMd8) is already vendored and unused.** `ppmd-rust` 1.4 ships
   `encoder_8`/`decoder_8` alongside 7. The max tier runs a three-way tournament
   (LZMA2, PPMd7 o10, PPMd7 o16); adding PPMd8 is a codec id and a tournament entry, and the
   tournament architecture makes it risk-free — if it loses, nothing changes. The report
   surveys 2001-2026 for a *fourth* codec (§5, 3-4 weeks of BWT bindings) while a fourth
   codec sits in `Cargo.lock`.
4. **No ceiling measurement anywhere in the report.** Every number is relative to 7-Zip
   `-mx9` (8.8 MiB) or to xz on one stream (8.534 MiB). Nobody ran a *stronger* archiver on
   `test/corpus`: `zpaq -m5` (fragment dedup + CM — the closest thing in existence to narc's
   design goal), `kanzi -e TPAQX`, `bsc -b32`, `rar -m5`, `nanozip`. Without that, the
   report cannot say whether 8.5 MiB is the floor (in which case §1's remaining prize is 4%)
   or whether a CM archiver reaches 6.5 MiB (in which case the entire plan is aiming too
   low). A 2026 researcher establishes the ceiling before choosing a technique; this is the
   cheapest missing experiment in the document.
5. **No recovery record, in a report that deliberately breaks self-containment.** §1 and §7
   both introduce cross-unit dependencies and both mitigate with "cap the depth". The
   standard answer since PAR2 and WinRAR is an **optional recovery record**: Reed-Solomon or
   RaptorQ parity over the units, sized by the user. In Rust: `raptorq` (Apache-2.0, RFC
   6330, actively maintained) or `reed-solomon-erasure`. Once a unit's loss can cascade into
   K referrers, the ability to repair *is* part of the feature, and it is also the honest
   answer to "why is a corrupt dictionary unit acceptable".
6. **Big units and parallel decode are not actually coupled.** The report treats unit size
   as the single knob trading ratio against edit cost and decode parallelism.
   `lzma-rust2` ships `Lzma2Writer`'s `chunk_size` (independent LZMA2 chunks inside one
   stream) and `Lzma2ReaderMt`, i.e. the standard mechanism for a large unit that still
   decodes on N cores. That changes the cost side of "just use 64 MiB units" (§1.4b row 4)
   and it is worth measuring, since narc already found parallel extraction *slower* for
   small chunks.
7. **The OpenZL lesson was taken at the wrong altitude.** §4.3 concludes "generalise
   `(codec, filter)` to a transform list" — fine, but the place OpenZL's actual technique
   applies inside narc is the **manifest**: paths are a front-codable sorted string column,
   and sizes/mtimes/permissions/chunk-ids are integer columns that want transpose + delta +
   varint before zstd. §8 dismisses the manifest by looking only at *lookup speed*
   ("1.9M files in 2.8 s"), never at its *size* on a multi-million-file archive. That is the
   one corpus in narc that is genuinely columnar and where §4's rejected library would have
   won.
8. **Opportunity cost against research 02 is never stated.** §10 proposes ~8-10 weeks
   chasing 12-18% on one synthetic source blob. `docs/research/02-recompression.md` already
   validated `preflate-rs` (Apache-2.0, pure Rust, `forbid(unsafe)`, production at Microsoft)
   and `lepton_jpeg` (~22% on JPEG) for **10-60% on the deflate/JPEG payloads users
   actually store** — PDFs, DOCX, APKs, PNGs, photo folders — and it is ROADMAP plan item 7.
   A frontier report that recommends a sequence owes the reader that comparison.
9. Smaller, but free: **PPMd7's order (10) and pool (32× chunk, cap 64 MiB) were tuned for
   4 MiB chunks** and are format constants; the max tier now runs 16-32 MiB units, so the
   tuning is stale in exactly the regime §1 wants to change. Any format break should re-tune
   them in the same pass, since they cannot be changed later.

## Appendix — raw data from the measurements in §1

Corpus `test/corpus`: 5751 files, 113.41 MiB. Blob = concatenation in `(ext, path)` order.
Compressor: `xz --format=raw --lzma2=preset=9e -T1` throughout.

```
A. one stream, dictionary sweep
   dict    4 MiB ->  11.411 MiB (10.06% of raw)  100.0% of 4MiB-dict
   dict   16 MiB ->   8.824 MiB ( 7.78%)          77.3%
   dict   64 MiB ->   8.549 MiB ( 7.54%)          74.9%
   dict  192 MiB ->   8.534 MiB ( 7.52%)          74.8%

B. independent units, dict = unit size
   4 MiB  x29 -> 14.988 MiB
  16 MiB   x8 -> 10.723 MiB
  32 MiB   x4 ->  9.411 MiB
  64 MiB   x2 ->  9.262 MiB

C. 16 MiB units with a preset dictionary; per-unit cost estimated as C(D||U) - C(D)
   adjacent chain (i <- i-1):  1.385 0.907 1.517 1.388 1.306 1.457 0.735 0.098 = 8.793
   depth-1, all <- unit0:      1.385 0.907 1.525 1.817 1.705 1.552 0.747 0.112 = 9.751
   depth<=4, roots 0 and 4:    1.385 0.907 1.517 1.388 1.879 1.457 0.735 0.098 = 9.366
   shared 16 MiB strided sample dict: units 7.582 + dict stored 2.202          = 9.784
   (unit 4 cold = 1.879 MiB vs 1.306 MiB warm against unit 3)
```

Scratch blob deleted after measurement; the scripts are trivial to re-derive from the
tables above (build blob in `(ext, path)` order, then pipe slices through `xz`).

### [REVIEW] Appendix B — raw data from the re-measurement (real `preset_dict`)

Bench kept, so this is reproducible: `test/presetbench` (own tiny crate, `[workspace]`
stanza so it stays out of the main workspace; `cargo run --release -- <dir> <a|b|c|d|g|h>`).
`lzma-rust2` 0.19, preset 9, `nice_len = 273`, `lc/pb` as noted, one encoder per unit,
`dict_size = dict_len + unit_len` when a preset dict is used, warm runs assert a bit-exact
roundtrip through `Lzma2Reader`. Blob = `test/corpus` in `(ext, path)` order,
118,921,271 bytes (matches the author's blob exactly).

```
cold  4 MiB x29                       -> 14.949 MiB      (author's xz proxy: 14.988)
cold 16 MiB  x8                       -> 10.690 MiB      (proxy: 10.723)
cold 32 MiB  x4                       ->  9.389 MiB      (proxy:  9.411)
cold 64 MiB  x2                       ->  9.246 MiB      (proxy:  9.262)
warm 16 MiB, dict = 1 ancestor        ->  8.781 MiB  [roundtrip OK]   (proxy: 8.793)
warm  4 MiB, dict = 1 ancestor        ->  9.962 MiB  [roundtrip OK]
warm  4 MiB, dict = 4 ancestors       ->  8.805 MiB
grouped  4 MiB, K=4 (root every 4)    -> 10.701 MiB
grouped  4 MiB, K=8                   ->  9.404 MiB
grouped 16 MiB, K=4                   ->  9.248 MiB
lc=4,pb=0: cold 16 MiB 10.683 | warm 16 MiB 8.779
test/enwik8 dir, 16 MiB units, text units only (2 incompressible .zip units excluded):
  cold lc3pb2 28.179 | cold lc4pb0 28.120 | warm dict=1 ancestor 27.006
```

Reading: the author's `C(D‖U) − C(D)` proxy is accurate to ≤0.4% everywhere and is
*conservative*, not optimistic. Chain **depth** is not the variable — dictionary **bytes**
are: 4 MiB units need 4 ancestors to reach what 16 MiB units reach with one.

---

## Primary sources

**Optimal parsing**
- Ferragina, Nitto, Venturini, *Bit-Optimal Lempel-Ziv compression*, [arXiv:0802.0835](https://arxiv.org/abs/0802.0835) (2008-02-06); SODA '09 768-777
- Kosolobov, *Relations Between Greedy and Bit-Optimal LZ77 Encodings*, [arXiv:1707.09789](https://arxiv.org/abs/1707.09789), STACS 2018, [DOI](https://drops.dagstuhl.de/entities/document/10.4230/LIPIcs.STACS.2018.46)
- Farruggia, Ferragina, Frangioni, Venturini, *Bicriteria data compression*, [arXiv:1307.3872](https://arxiv.org/pdf/1307.3872)
- Google, *Compress data more densely with Zopfli*, [developers blog](https://developers.googleblog.com/compress-data-more-densely-with-zopfli/) (2013)
- [encode.su, "[LZ] Optimal parsing"](https://encode.su/threads/1895-LZ-Optimal-parsing) — REPDIST +3%, 4-arrivals −2.7%
- [cbloom, LZA New Optimal Parse](http://cbloomrants.blogspot.com/2015/01/01-23-15-lza-new-optimal-parse.html) (2015-01-23)

**Entropy coding**
- Giesen, *rANS in practice*, [ryg blog](https://fgiesen.wordpress.com/2015/12/21/rans-in-practice/) (2015-12-21); [ryg_rans](https://github.com/rygorous/ryg_rans)
- Kosolobov, *Efficiency of ANS Entropy Encoders*, [arXiv:2201.02514](https://arxiv.org/pdf/2201.02514)
- Bloom, *Understanding ANS - 3*, [cbloomrants](http://cbloomrants.blogspot.com/2014/02/02-01-14-understanding-ans-3.html) (2014-02-01)
- Collet, *A comparison of Arithmetic Encoding with FSE*, [fastcompression](http://fastcompression.blogspot.com/2014/02/a-comparison-of-arithmetic-encoding.html) (2014-02)
- Rust: [`constriction`](https://github.com/bamler-lab/constriction), [`ans`](https://docs.rs/ans/latest/ans/), [`rans`](https://github.com/m4tx/rans-rs)

**OpenZL**
- Collet et al., *OpenZL: A Graph-Based Model for Compression*, [arXiv:2510.03203](https://arxiv.org/abs/2510.03203) (2025-10-03, v2 2025-10-30)
- [Meta engineering blog](https://engineering.fb.com/2025/10/06/developer-tools/openzl-open-source-format-aware-compression-framework/) (2025-10-06)
- [facebook/openzl](https://github.com/facebook/openzl) — BSD, C11/C++17, [v0.2.0](https://github.com/facebook/openzl/releases/tag/v0.2.0)
- [Phoronix, OpenZL 0.2 Released](https://www.phoronix.com/news/OpenZL-0.2-Released) (2026-05-08)
- Rust bindings: [`openzl-sys`](https://github.com/LDeakin/openzl-sys), [`rust-openzl`](https://github.com/vitorpy/rust-openzl)

**Long-range matching / dictionaries**
- [liblzma `lzma_options_lzma`](https://tukaani.org/xz/liblzma-api/structlzma__options__lzma.html) — `preset_dict`, raw-only
- [XZ Utils forum: Preset dictionary](https://sourceforge.net/p/lzmautils/discussion/708858/thread/e40fbf99/)
- [ck-hack lrzip-0.612](http://ck-hack.blogspot.com/2012/03/lrzip-0612.html) (2012-03) — kernel tree −11% vs lzma -9
- [encode.su FreeArc suite thread](https://encode.su/threads/231-FreeArc-compression-suite-(4x4-Tornado-REP-Delta-Dict-)/page19) — rep/srep throughput claims
- [modern-rzip](https://github.com/iczelia/modern-rzip); [lrzip](https://github.com/ckolivas/lrzip)
- [7-Zip `-m` switch docs](https://documentation.help/7-Zip-18.0/method.htm) — `qs` vs `md` substitution
- Rust: [`lzma-rust2`](https://github.com/hasenbanck/lzma-rust2) 0.16.x Apache-2.0 pure Rust; [`xz2`](https://github.com/alexcrichton/xz2-rs) liblzma bindings

**BWT**
- [Large Text Compression Benchmark](https://www.mattmahoney.net/dc/text.html) (retrieved 2026-08-17)
- [MaskRay, Benchmarking compression programs](https://maskray.me/blog/2025-08-31-benchmarking-compression-programs) (2025-08-31)
- [libbsc](https://github.com/IlyaGrebnov/libbsc) Apache-2.0; [libsais](https://github.com/IlyaGrebnov/libsais) Apache-2.0
- [bzip3](https://github.com/iczelia/bzip3) LGPL (libsais/LZP parts Apache-2.0); [kanzi-cpp](https://github.com/flanglet/kanzi-cpp)
- Rust: [`libsais`](https://docs.rs/libsais/latest/libsais/) / [`libsais-sys`](https://github.com/feldroop/libsais-rs) (2025), [`divsufsort`](https://lib.rs/crates/divsufsort)

**Ordering & delta**
- [mhx/dwarfs](https://github.com/mhx/dwarfs) v0.14.1 + [mkdwarfs docs](https://github.com/mhx/dwarfs/blob/main/doc/mkdwarfs.md)
- Zhang et al., *Finesse*, [USENIX FAST '19](https://www.usenix.org/conference/fast19/presentation/zhang)
- Xia, Pu et al., *Odess* / *Fast and Lightweight Resemblance Detection*, [ACM TOS 2023](https://dl.acm.org/doi/full/10.1145/3584663) (conf. IEEE ICDE '21, 480-491)
- *Palantir*, [ASPLOS '24](https://henryhxu.github.io/share/hongming-asplos24.pdf); *Argus*, [ACM TOS 2025](https://dl.acm.org/doi/10.1145/3747839)
- [sisong/HDiffPatch](https://github.com/sisong/HDiffPatch) benchmarks; [zstd releases](https://github.com/facebook/zstd/releases) 1.5.7
- Rust: [`qbsdiff`](https://crates.io/crates/qbsdiff), [`bidiff`](https://github.com/divvun/bidiff), [`bsdiff`](https://crates.io/crates/bsdiff)
- [Mahoney incremental benchmark](https://mattmahoney.net/dc/mingw.html); [10 GB benchmark](https://mattmahoney.net/dc/10gb.html)

**Succinct indexes**
- [`sucds`](https://github.com/kampersanda/sucds), [`fm-index`](https://github.com/ajalab/fm-index), [`sview-fmindex`](https://crates.io/crates/sview-fmindex)
- [FM-index implementations survey](https://curiouscoding.nl/posts/fm-index-implementations/)

**[REVIEW] Added by the review pass**
- Kuruppu, Puglisi, Zobel, *Relative Lempel-Ziv Compression of Genomes*, [SPIRE 2010](https://link.springer.com/chapter/10.1007/978-3-642-16321-0_20); *RLZ Factorization for Web Collections*, [arXiv:1106.2587](https://arxiv.org/pdf/1106.2587)
- Liao, Petri, Moffat, Wirth, *Effective Construction of Relative Lempel-Ziv Dictionaries*, [WWW 2016](https://dl.acm.org/doi/10.1145/2872427.2883042) — reference selection as set cover, +27% over CARE
- *Hierarchical Relative Lempel-Ziv*, [SEA 2023](https://drops.dagstuhl.de/entities/document/10.4230/LIPIcs.SEA.2023.18); Navarro et al., *Practical Indexing of Repetitive Collections Using RLZ*, [DCC/IEEE 2019](https://ieeexplore.ieee.org/document/8712748/)
- DwarFS author's bzip3 measurement, [mhx/dwarfs discussion #110](https://github.com/mhx/dwarfs/discussions/110) — bzip3 488 MiB vs lzma ~300 MiB on 4.2 GiB of Perl installs
- [mkdwarfs.md](https://github.com/mhx/dwarfs/blob/main/doc/mkdwarfs.md) (fetched 2026-08-17) — `--order` modes incl. `revpath`; nilsimsa now deterministic; **no ratio A/B published**
- Vendored-but-unused code, verified in the local registry: `lzma-rust2` 0.19 `filter::bcj` (x86/arm/arm64/arm_thumb/ppc/sparc/ia64/riscv) + `filter::bcj2`, `LzmaOptions::preset_dict`, `Lzma2ReaderMt`; `ppmd-rust` 1.4 `encoder_8`/`decoder_8` (PPMd var.I)
- Recovery records: [`raptorq`](https://crates.io/crates/raptorq) (RFC 6330, Apache-2.0), `reed-solomon-erasure`
- Recompression baseline this report should have been sequenced against: `docs/research/02-recompression.md` — [preflate-rs](https://github.com/microsoft/preflate-rs) (Apache-2.0, pure Rust), [lepton_jpeg_rust](https://github.com/microsoft/lepton_jpeg_rust) (~22% on JPEG)
- [OpenZL v0.2.0 release notes](https://github.com/facebook/openzl/releases/tag/v0.2.0) — verified: `ZL_GRAPH_LZ` is "the equivalent of zstd level 1 with a 64K window", 10%/70% faster comp/decomp on Silesia. §4's framing is fair; note it is a *speed* codec, not a ratio one.

**Competitions**
- Ribeiro, Pratas et al., *The 2026 Algorithmic Information Theory Data Compression Challenge*, [arXiv:2606.17712](https://arxiv.org/pdf/2606.17712) (2026-06-17) — 117 compressors, 8 GB memory / <1 MB decompressor limits, hidden test partition; finding: no single compressor dominates, zstd-1/brotli-1 win when runtime counts
- [GDCC 4th edition results](https://gdcc.tech/results/) (2025, winners announced 2025-06-13)
