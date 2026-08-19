# Research 09 — Resource scheduling: all cores, zero lag, bounded memory (Windows-first)

*Nova Prism research report, 2026-08-16. All API semantics, pitfalls and product behaviors below were
verified against live sources (Microsoft Learn, MS DevBlogs, 7-Zip/WinRAR docs & changelogs,
crates.io/lib.rs, community measurement reports) — links inline and in §12.*

Scope: how nova v1 saturates every core while Windows stays responsive, and how its memory stays
bounded and configurable. Concludes with an exact scheduling+memory policy (§11) with Windows API
calls (all present in `windows-sys`) and default numbers.

---

## 1. Executive summary

- **CPU: `SetPriorityClass(BELOW_NORMAL_PRIORITY_CLASS)` is the correct default.** All logical
  cores, full throughput when the machine is idle, near-total yielding to any normal-priority
  foreground work. `IDLE_PRIORITY_CLASS` is the *eco* option, not the default (it loses to every
  background updater on the box).
- **Never use `PROCESS_MODE_BACKGROUND_BEGIN`.** It silently drops I/O priority to Very Low *and*
  memory priority to Very Low; community investigations (Mozilla bug 1476365) report an effective
  ~32 MiB working-set squeeze and up to 250× slowdowns for memory-hungry processes. It is
  current-process-only and all-or-nothing. Compose the same effect from its ingredients instead
  (CPU class + per-handle I/O hint + memory priority), each tuned separately.
- **EcoQoS (`SetProcessInformation` + `PROCESS_POWER_THROTTLING`) is opt-in only (`--eco`).** On
  Intel big.LITTLE it parks work on E-cores — great. On plain AMD 6-core (no E-cores) it clamps
  frequency instead; users report severe slowdowns (down to hundreds of MHz in bad interactions).
  Wrong default for a throughput tool, excellent explicit "laptop/overnight" mode.
- **I/O: per-handle `SetFileInformationByHandle(FileIoPriorityHintInfo)` = Low** on bulk data
  handles. This is the middle level background mode can't give you (background mode = Very Low,
  measured at 1–3 % of disk when anything else is active — the archiver would starve itself).
- **Memory bound = self-imposed budget, not OS working-set caps.** Job-object *commit* limit is a
  fine crash-guardrail; working-set caps just convert your RAM use into page-file I/O storms.
  Because .nva compresses independent CDC chunks ≤ 4 MiB, extraction memory is *structurally*
  bounded (~12 MiB per worker + queues) — a real differentiator vs. RAR 64 GiB dictionaries and
  LZMA2 256 MiB defaults which need dictionary-sized RAM to extract.
- **Defaults:** threads = `available_parallelism()` (all logical cores); compression budget =
  `clamp(min(50 % of available phys RAM, 25 % of total), 512 MiB, 8 GiB)`; workers additionally
  capped by `budget / per_worker_cost`; bounded channels everywhere (cap ≈ 2 chunks/worker).
- **Proving "no lag" is measurable:** replicate Microsoft's own Efficiency-Mode methodology
  (saturate all cores with the archiver, measure scripted foreground actions — app launch, typing,
  Start menu — via ETW/UIforETW input events; p95/p99, not means). Targets in §10.

---

## 2. CPU priority: the toolbox and the right default

### 2.1 Priority classes (verified against MS Learn "Scheduling Priorities" / `SetPriorityClass`)

| Class | Base | Behavior for nova |
|---|---|---|
| `REALTIME_PRIORITY_CLASS` | 24 | Forbidden territory (can hang mouse/disk flush). |
| `HIGH_PRIORITY_CLASS` | 13 | No. |
| `ABOVE_NORMAL_PRIORITY_CLASS` | 10 | No. |
| `NORMAL_PRIORITY_CLASS` | 8 | nova `--full` mode (benchmarks, dedicated boxes). |
| `BELOW_NORMAL_PRIORITY_CLASS` | 6 | **nova default.** Loses every contested slice to normal apps, wins vs. idle-class junk. |
| `IDLE_PRIORITY_CLASS` | 4 | nova `--eco` component. Starved by *anything* ≥ normal, incl. other background tools. |

Windows schedules strictly by priority level with round-robin within a level: lower-priority
threads run only when no higher-priority thread is runnable — that is exactly the "use idle cores,
vanish under load" semantics we want, with two refinements the OS adds for us: dynamic priority
boosts for foreground/input-owning windows keep the UI ahead of us, and background-mode threads
"may not be scheduled promptly, but will never be starved" (MS wording), so no deadlock-by-starvation.

Key subtlety: **CPU priority alone is *not* sufficient for responsiveness.** Microsoft's own docs
say it verbatim: "even an idle CPU priority thread can easily interfere with system responsiveness
when it uses the disk and memory." Hence §4 (I/O) and §5 (memory priority) are mandatory parts of
the policy, not nice-to-haves.

### 2.2 BELOW_NORMAL vs IDLE — why BELOW_NORMAL wins as default

- IDLE (base 4) is below the working priority of many services and updater/indexer processes;
  a long archive job on a busy consumer box can crawl for hours while accomplishing nothing the
  user asked for. BELOW_NORMAL yields to the *user*, not to every daemon.
- Foreground apps get boost + larger quantum anyway; measured UI impact of BELOW_NORMAL all-core
  load is already near-zero (Microsoft's Efficiency-Mode study used *low* priority as the fix for
  a 100 % all-core normal-priority antagonist and got 14–76 % foreground completion-time
  improvements; we start from the polite side).
- Thread-level: worker threads stay `THREAD_PRIORITY_NORMAL` *within* the class (class already
  moved the base). The GUI/progress thread of a future nova GUI gets
  `THREAD_PRIORITY_ABOVE_NORMAL` relative to workers instead of raising the class.

---

## 3. Background processing mode — known trap, do not use process-wide

`SetPriorityClass(GetCurrentProcess(), PROCESS_MODE_BACKGROUND_BEGIN)` (and the per-thread
`THREAD_MODE_BACKGROUND_BEGIN` in `SetThreadPriority`) lowers CPU **and I/O and memory priority**
in one shot. Verified facts and pitfalls:

1. **I/O drops to Very Low** — the lowest of the five kernel I/O levels. A PowerShell measurement
   showed a background-mode process getting **1–3 % of disk throughput** while other I/O was
   active. Good for an indexer, catastrophic for an archiver whose *job* is bulk I/O.
2. **Memory priority drops to Very Low (1)** — its pages are first in line for trimming; combined
   with reports (Mozilla bug 1476365, Chromium/SO analyses) of an effective **~32 MiB working-set
   squeeze**, memory-heavy processes have been observed running **~250× slower**. Chromium ships
   zero non-test uses of this API — that is negative knowledge from two browser vendors.
3. **Current process/thread only**, fails if already in background mode, and mixing process-mode
   with thread-mode is explicitly unsupported (process `..._END` resets all threads' states).
4. Not available for other processes → irrelevant for a GUI driving a worker exe anyway.

**Verdict:** rejected. nova composes the equivalent from the à-la-carte APIs (class + I/O hint +
memory priority), which are each reversible and tunable. `THREAD_MODE_BACKGROUND_BEGIN` remains
acceptable *only* inside `--eco` for the checksum/housekeeping threads, never for pipeline workers.

---

## 4. EcoQoS / power throttling (Windows 11 "Efficiency mode")

API (Windows 11; no-ops harmlessly on Win10 pre-21H2 — call must tolerate failure):

```text
PROCESS_POWER_THROTTLING_STATE s = {
    .Version     = PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    .ControlMask = PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    .StateMask   = PROCESS_POWER_THROTTLING_EXECUTION_SPEED,  // on. 0 = force HighQoS off
};
SetProcessInformation(h, ProcessPowerThrottling, &s, sizeof s);
```

Verified behavior:

- EcoQoS = "run this at the most efficient frequency / on the most efficient cores". Microsoft
  measured up to 90 % CPU package power reduction and — for a *background* antagonist load —
  14–76 % better foreground responsiveness. Task Manager's "Efficiency mode" = base priority Low
  **+** EcoQoS (green leaf).
- **Intel hybrid (12th gen+ big.LITTLE):** EcoQoS threads are scheduled onto E-cores; P-cores stay
  free for the user. This is the honest "archiver invisible to the user" mode on such CPUs.
- **AMD plain 6-core / non-hybrid Intel:** there are no E-cores to migrate to, so the scheduler's
  only lever is **frequency clamping**. Community reports (Ryzen 5 2600 browsing lag; Ryzen bugs
  pinning cores at ~0.5 GHz) show it can cost far more throughput than the ~2× you'd expect —
  workers may run at a fraction of base clock. Unacceptable as default on the owner's own
  hardware mix ("various CPUs, incl. plain 6-core AMD").
- Per-thread variant exists (`SetThreadInformation(ThreadPowerThrottling)`); also
  `PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION` opts out of timer-resolution coalescing
  (irrelevant for nova — we have no high-res timers).
- If nova does nothing, Windows heuristics may *still* green-leaf it. Explicitly setting
  `StateMask = 0` (throttling off) is the documented way to pin HighQoS — nova's `--full` mode
  should do this; default mode leaves the field untouched.

**Verdict:** `--eco` flag = `IDLE_PRIORITY_CLASS` + EcoQoS on + I/O Very Low + memory priority
Low. Default mode: no EcoQoS. On hybrid CPUs `--eco` is nearly free (E-cores are genuinely fast);
on non-hybrid it is a deliberate "I don't care how long it takes" mode — document that.

Core-topology awareness: `GetLogicalProcessorInformationEx(RelationProcessorCore)` exposes
`PROCESSOR_RELATIONSHIP.EfficiencyClass` (all-equal on non-hybrid; higher = faster core on
hybrid). v1 only needs it for *reporting* (thread-count UI like "8P+16E"); scheduling placement is
better left to Windows' QoS machinery + Thread Director than to manual affinities (rejected: CPU
affinity pinning — fights the scheduler, breaks on the next CPU generation).

---

## 5. I/O priority

Kernel has 5 levels (Critical/High/Normal/Low/Very Low); user mode can *lower* only, via:

```text
FILE_IO_PRIORITY_HINT_INFO hint = { .PriorityHint = IoPriorityHintLow };
SetFileInformationByHandle(hFile, FileIoPriorityHintInfo, &hint, sizeof hint);
```

- Per-handle, must be set before/independently of reads; it is a **hint** honored by the storage
  stack (NTFS + volsnap + most disk drivers do; some filter drivers don't).
- `IoPriorityHintLow` is the sweet spot **unreachable via background mode** (which only offers
  Very Low). Low keeps the archiver progressing while foreground I/O gets serviced first.
- Very Low = the "1–3 % of disk" starvation level → only in `--eco`.
- Set it on: source-file read handles, archive write handle. Not on: the tiny index/manifest
  handles at finalize time (finish fast, release locks).
- Windows also offers `SetFileBandwidthReservation` (guaranteed *minimum* bandwidth) — rejected:
  nova wants a ceiling-less floor-less polite mode, not reservations, and support is spotty.

CPU priority does not imply I/O priority (7-Zip feature request #1632 documents exactly this
complaint about 7-Zip's own "Background" button: idle CPU class still issues Normal-priority I/O
that trashes HDD seek time for the foreground).

---

## 6. Memory priority

`MEMORY_PRIORITY_INFORMATION` (Win 8+), set via `SetProcessInformation(ProcessMemoryPriority)` or
per-thread `SetThreadInformation(ThreadMemoryPriority)`. Levels 1 (`VERY_LOW`) … 5 (`NORMAL`,
default). Semantics: a *hint* to the memory manager — lower-priority pages are trimmed from the
working set first, other factors equal. This is how you make a 2-GiB archiver not evict the
user's browser tabs, without any hard cap or thrashing:

- Default mode: process memory priority = `MEMORY_PRIORITY_BELOW_NORMAL` (4).
- `--eco`: `MEMORY_PRIORITY_LOW` (2). (Very Low (1) reserved for a future "prefetch/readahead"
  thread whose pages are truly disposable.)
- Cost: when the system is *not* under memory pressure, zero. Under pressure nova pays the page
  faults instead of the user — which is the contract we advertise.

---

## 7. Bounding memory

### 7.1 OS mechanisms — what to use and what to reject

| Mechanism | Semantics | Verdict for nova |
|---|---|---|
| Job object `JOB_OBJECT_LIMIT_PROCESS_MEMORY` (`ProcessMemoryLimit`) | Hard cap on **committed** virtual memory; further commits fail (allocation returns NULL / Rust alloc error) | **Use as guardrail** at 1.5–2× the self-budget: converts a leak/bug into a clean `nova: out of budget` error instead of system-wide swap death. Self-assign: `CreateJobObjectW` → `SetInformationJobObject` → `AssignProcessToJobObject(GetCurrentProcess())`. |
| Job `JOB_OBJECT_LIMIT_WORKINGSET` / `SetProcessWorkingSetSize` | Hard working-set cap; excess pages force-trimmed to standby/pagefile | **Rejected.** Doesn't reduce allocations, just converts them into page-file I/O churn — the process still "uses" the memory, now with disk amplification. Classic mistake (same failure mode as the `PROCESS_MODE_BACKGROUND_BEGIN` 32 MiB squeeze). |
| Job CPU rate control (`JOBOBJECT_CPU_RATE_CONTROL_INFORMATION`) | Cap CPU % of the job | **Rejected.** Caps us even when all cores are idle — violates the "use everything that's free" requirement. Priorities already implement "yield only under contention". |
| Self-imposed budget (allocator-side accounting) | nova decides worker count/queue depths from a byte budget | **Primary mechanism** (below). Only the app knows that N workers × cctx + queues = X. |

### 7.2 Codec memory — the real numbers (why budgets must be per-worker-aware)

**zstd** (compression ≈ window + hash + chain tables; per zstd manual):

| Setting | Window | Approx. compressor state | Decompressor need |
|---|---|---|---|
| L3 (default) | 8 MiB max for big inputs | ~10–20 MiB | ≤ window |
| L19 (`wlog=23,clog=23,hlog=22`) | 8 MiB | ~56 MiB core (8 + 32 + 16) | ≤ 8 MiB |
| L20–22 `--ultra` | up to 128 MiB (wlog 27) | hundreds of MiB | up to 128 MiB |
| `--long[=27]` | 128 MiB default, ≤ 2 GiB | + LDM table | **> wlog 27 requires receiver opt-in** (`ZSTD_d_windowLogMax` default = 27) |

- libzstd multithreading (`ZSTD_c_nbWorkers`) buffers ~`jobSize (≈ 4×window) + overlap` per
  worker → memory scales linearly with workers; and `ZSTD_estimateCCtxSize*()` **refuses** to
  estimate when `nbWorkers ≥ 1`.
- **nova dodges almost all of this structurally:** we compress independent CDC chunks of
  256 KiB–4 MiB, one plain single-threaded `ZSTD_CCtx` per worker, window auto-clamped to input
  size. Per-worker cost ≈ in-chunk 4 MiB + out-buffer ~4.1 MiB + cctx (a few MiB at L3, tens of
  MiB at L19 for a 4 MiB window) → **budget ~64 MiB/worker worst-case, ~16 MiB typical**. Compute
  exactly at startup with `ZSTD_estimateCCtxSize(level)` (available via `zstd-sys`) instead of
  hardcoding.
- Extraction: `DCtx` + window ≤ 4 MiB → **~12 MiB/worker**. This is the "weak PC can always
  extract" guarantee, and it holds *by format construction*, not by configuration.

**LZMA/LZMA2** (7-Zip `-m` switch docs; if nova adds an LZMA2 max-tier):

| Match finder | Compression RAM | Decompression RAM |
|---|---|---|
| BT4 | 11.5 × dict + 4 MiB (dict ≤ 48 MiB); 10.5 × dict above | ≈ dict |
| HC4 | 7.5 × dict + 4 MiB; 6.5 × dict above | ≈ dict |

7-Zip 24.09 raised 64-bit defaults to dict = 32 MiB (x5) … **256 MiB (x9)** → x9 ≈ **2.9 GiB per
compressing stream** (×2 threads per LZMA2 chunk in x5+ modes; N chunks in parallel multiply it) and
256 MiB just to *extract*. WinRAR 7 allows dictionaries to **64 GiB**. Lesson: if nova ever offers
big-window/solid modes, the required extraction memory **must be recorded in the archive header**
and checked against the extractor's budget with a clear error — both competitors landed on exactly
this design (below).

### 7.3 How 7-Zip / WinRAR expose thread & memory limits (UI patterns worth copying)

| Product | Threads UI | Memory UI / switches | Extraction safety |
|---|---|---|---|
| 7-Zip (26.x) | "Number of CPU threads" dropdown (default = all logical); GUI shows live "Memory usage for Compressing/Decompressing" estimate | GUI "Memory usage" dropdown, default **80 % of RAM**; CLI `-mmemuse=p{N}` (percent) / `={N}g`; it *reduces dict/threads to fit* | `-smemx{N}g` limit for RAR unpack; GUI asks permission when RAR dict > 4 GiB |
| WinRAR (7.x) | threads implicit; CLI `-ri<p>[:<s>]` sets priority 1–15 **and sleep-injection** ms per I/O | dict chosen vs. "physically available memory"; non-power-of-2 dicts > 4 GiB | Settings → "Maximum dictionary size allowed to extract" → GUI prompt; CLI **refuses > 4 GiB by default**, override `-md/-mdx` |

nova copies: live memory estimate next to the level picker; percent-or-absolute `--memory`;
header-recorded extraction requirement + default refusal above budget. nova rejects: WinRAR-style
sleep-injection (`-ri1:100`) as primary politeness tool — it wastes idle cores; priorities/QoS do
this correctly (kept only as a last-resort hidden knob if some exotic driver ignores I/O hints).

---

## 8. Bounded-queue pipeline (backpressure by construction)

```
reader (1) ──► chunker/CDC (1) ──► [bounded ch A] ──► compress worker × W ──► [bounded ch B] ──► reorder+write (1)
   │ I/O hint Low                     cap: 2·W chunks       blake3 + zstd         cap: 2·W results      I/O hint Low
   └──────────────── read-ahead ≤ max(32 MiB, 8 MiB·W)  ← single global in-flight-bytes semaphore ─────────────────┘
```

Rules (each one exists to make memory a *closed formula*):

1. Every channel bounded (`crossbeam-channel` `bounded(2·W)` / `std::sync::mpsc::sync_channel`).
   Full channel ⇒ sender blocks ⇒ reader stops ⇒ read-ahead stops. No unbounded `Vec` anywhere.
2. A global byte-semaphore caps *in-flight chunk bytes* (acquired at read, released after write):
   `inflight_max = clamp(8 MiB · W, 32 MiB, budget / 3)`. This is what actually bounds RAM when
   chunks skew large or a worker stalls.
3. Reorder buffer (results arrive out of order, .nva is append-only → writer needs sequence
   order) is bounded by the same semaphore; a stalled chunk N blocks acceptance of N+2·W — that's
   the designed behavior, not a bug.
4. Writer is a single thread; fsync policy and I/O priority hint live only there.
5. Worst-case total = `inflight_max + W · cctx_estimate + constants` → checked against budget at
   startup; if over, reduce W, then queue caps, in that order.

**rayon** (v1.12.0, Apr 2026, MIT/Apache-2.0, active): fine for *pure-CPU inner parallelism*
(blake3 already uses its own; future intra-chunk match finding), wrong as the pipeline backbone —
`par_bridge`/`par_iter` have no backpressure or ordering guarantees and buffer unboundedly in the
worst case, and rayon has no task priorities (own FAQ; the standard workaround is *separate
pools*). If nova uses a rayon pool at all:

```rust
rayon::ThreadPoolBuilder::new()
    .num_threads(w)
    .stack_size(2 * 1024 * 1024)
    .thread_name(|i| format!("nova-worker-{i}"))
    .start_handler(|_| unsafe {
        // runs ON each new worker thread:
        // per-thread memory priority (process-wide setting covers this too)
        let mp = MEMORY_PRIORITY_INFORMATION { MemoryPriority: MEMORY_PRIORITY_BELOW_NORMAL };
        SetThreadInformation(GetCurrentThread(), ThreadMemoryPriority,
                             &mp as *const _ as _, size_of::<MEMORY_PRIORITY_INFORMATION>() as u32);
    })
    .build()
```

Since the process priority class already covers CPU priority for all threads, per-thread
`SetThreadPriority` calls are unnecessary in the default mode. The `thread-priority` crate
(v3.1.1, Jun 2026, MIT, active, Windows supported) is a fine alternative to raw `windows-sys`
here, but nova needs `windows-sys` anyway — one dependency fewer.

For v1 the recommendation is a **hand-rolled fixed pool of `std::thread`s + crossbeam bounded
channels** for the pipeline (deterministic memory, explicit shutdown, trivial to reason about),
rayon reserved for later CPU-only stages.

---

## 9. Thread-count policy on heterogeneous CPUs

- `std::thread::available_parallelism()` → all logical processors (respects process affinity and,
  on Windows 11, processor groups). That is the default W *before* the memory cap.
- SMT: compression is compute+memory-bound; HT typically adds 10–25 % throughput. At
  BELOW_NORMAL priority the responsiveness cost of using logical vs physical cores is handled by
  the scheduler, so **use all logical cores** (owner requirement) — rejected the "N−1 cores"
  folk remedy: it permanently donates ~4–17 % of the machine even when idle, while priority does
  the same job only when needed.
- big.LITTLE: default mode lets Windows spread workers over P+E cores (they are HighQoS,
  BELOW_NORMAL); `--eco` shifts them to E-cores via EcoQoS. No manual affinity.
- Weak-PC extraction: W scales down via the same budget formula; at `--memory=256M` extraction
  still runs ≥ 8 workers (8 × ~12 MiB + queues ≈ 130 MiB + inflight 64 MiB ≈ within budget).

---

## 10. Benchmarking "no system lag" (methodology nova CI can actually run)

Microsoft's Efficiency-Mode study is the template: antagonist load on all cores at 100 %, then
measure *foreground activity completion time* (app launch, Start-menu open) vs. the no-load
baseline; they report 14–76 % improvements from Low priority + EcoQoS. nova inverts it: nova *is*
the antagonist; the acceptance criterion is that foreground metrics stay at baseline.

Harness design (Windows box, admin once, then scripted):

1. **Scenarios:** (a) nova compressing a 30 GiB mixed corpus from/to the system NVMe, default
   mode; (b) same in `--eco`; (c) same in `--full` (expected-fail control); (d) idle baseline.
2. **Foreground probes, scripted and repeated ≥ 20×:**
   - App-launch time (cold-ish Notepad/Explorer window creation → first paint; measured via ETW).
   - Typing/input latency: UIforETW's input logger inserts ETW events per keystroke/click; WPA
     computes input-event → next-present gap. Report p50/p95/p99, not means (jitter is what users
     feel).
   - Browser scroll jank on a fixed page (PresentMon frame times, p99 + dropped frames), because
     "game/video while archiving" is the owner's realistic scenario for the RTX 5060 Ti box.
   - `Microsoft-Windows-Win32k` `UIUnresponsiveness` events = free "message pump stalled" counter.
   - Audio: LatencyMon session must stay green (no DPC/hard-pagefault regressions — catches the
     memory-priority mistakes, not just CPU).
3. **Disk-contention probe:** copy a 10 GiB file on the *same* volume during scenario (a); require
   copy-time degradation ≤ 15 % with I/O hint Low, and record nova's own throughput loss (expected
   large — that's the contract).
4. **Acceptance targets v1:** foreground app-launch and input p99 within **10 %** of idle
   baseline in default mode; zero `UIUnresponsiveness` events attributable to system-wide stalls;
   nova throughput in default mode ≥ **90 %** of `--full` on an otherwise-idle machine (i.e.,
   politeness must be ~free when nobody is looking — this is the headline claim vs. WinRAR's
   sleep-injection approach, which cannot achieve it).
5. Publish the harness (`nova-bench-responsiveness`) — no mainstream archiver documents this;
   cheap credibility.

---

## 11. The nova v1 policy (concrete)

### 11.1 Modes

| | default | `--eco` | `--full` |
|---|---|---|---|
| Priority class | `BELOW_NORMAL_PRIORITY_CLASS` | `IDLE_PRIORITY_CLASS` | `NORMAL_PRIORITY_CLASS` |
| EcoQoS | untouched | on (`EXECUTION_SPEED`) | forced **off** (`StateMask=0`) |
| I/O hint (bulk handles) | `IoPriorityHintLow` | `IoPriorityHintVeryLow` | none (Normal) |
| Memory priority | `BELOW_NORMAL` (4) | `LOW` (2) | default (5) |
| Threads W₀ | all logical | all logical (E-cores will absorb) | all logical |

### 11.2 Numbers

```
T             = available_parallelism()                      // e.g. 24 on 12c/24t
budget        = user --memory
                else clamp( min(0.50 · avail_phys, 0.25 · total_phys), 512 MiB, 8 GiB )
                // GlobalMemoryStatusEx: ullAvailPhys / ullTotalPhys
                // 32 GiB half-used box → min(8 GiB, 8 GiB) = 8 GiB
                // 8 GiB box, 3 GiB free → min(1.5, 2) = 1.5 GiB
per_worker    = chunk_max(4 MiB) + out_bound(≈4.1 MiB) + ZSTD_estimateCCtxSize(level)   // runtime call
W             = clamp( min(T, (budget − inflight_max − 64 MiB) / per_worker), 1, T )
inflight_max  = clamp( 8 MiB · W, 32 MiB, budget / 3 )
channel_cap   = 2 · W  (chunks, each stage)
job_guardrail = 2 · budget   (job-object ProcessMemoryLimit; abort cleanly on breach)
extract       = same formulas; per_worker ≈ 12 MiB; floor guarantee: works at --memory=256M
```

### 11.3 Startup sequence (all in `windows-sys`, features
`Win32_System_Threading`, `Win32_System_JobObjects`, `Win32_Storage_FileSystem`,
`Win32_System_SystemInformation`)

```rust
// 1. CPU class (default mode)
SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);

// 2. Process memory priority
let mp = MEMORY_PRIORITY_INFORMATION { MemoryPriority: MEMORY_PRIORITY_BELOW_NORMAL };
SetProcessInformation(GetCurrentProcess(), ProcessMemoryPriority,
                      &mp as *const _ as _, size_of_val(&mp) as u32);

// 3. (--eco only) EcoQoS; ignore failure on Win10
let pt = PROCESS_POWER_THROTTLING_STATE {
    Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    StateMask:   PROCESS_POWER_THROTTLING_EXECUTION_SPEED };
SetProcessInformation(GetCurrentProcess(), ProcessPowerThrottling,
                      &pt as *const _ as _, size_of_val(&pt) as u32);

// 4. Job-object commit guardrail (self-assigned)
let job = CreateJobObjectW(null(), null());
let mut eli: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
eli.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY;
eli.ProcessMemoryLimit = 2 * budget;
SetInformationJobObject(job, JobObjectExtendedLimitInformation,
                        &eli as *const _ as _, size_of_val(&eli) as u32);
AssignProcessToJobObject(job, GetCurrentProcess());

// 5. Per bulk file handle, after open:
let hint = FILE_IO_PRIORITY_HINT_INFO { PriorityHint: IoPriorityHintLow };
SetFileInformationByHandle(h, FileIoPriorityHintInfo,
                           &hint as *const _ as _, size_of_val(&hint) as u32);
```

Cross-platform note: every call above is a no-op-able `#[cfg(windows)]` shim; Linux equivalents
later are `nice(10)` / `sched_setattr(SCHED_BATCH)`, `ioprio_set(IOPRIO_CLASS_IDLE|BE)`,
cgroup v2 `memory.high` — same policy shape, one trait (`ResourcePolicy`) in `nova-core`.

---

## 12. Sources

- SetPriorityClass / background mode: <https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setpriorityclass>
- SetThreadPriority (THREAD_MODE_BACKGROUND, "never starved", priority table): <https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setthreadpriority>
- Mozilla investigation of PROCESS_MODE_BACKGROUND (32 MiB / 250× reports, Chromium non-use): <https://bugzilla.mozilla.org/show_bug.cgi?id=1476365>
- SetProcessInformation / PROCESS_POWER_THROTTLING: <https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setprocessinformation>
- Introducing EcoQoS (90 % power figure, guidance): <https://devblogs.microsoft.com/sustainable-software/introducing-ecoqos/>
- Efficiency Mode methodology & 14–76 % responsiveness data: <https://devblogs.microsoft.com/performance-diagnostics/reduce-process-interference-with-task-manager-efficiency-mode/>
- EcoQoS on non-hybrid CPUs, user reports: <https://forum.vivaldi.net/topic/90809/one-word-turn-off-efficiency-mode>, <https://forums.tomshardware.com/threads/discovered-an-interesting-and-annoying-glitch-on-windows-that-may-go-un-noticed-for-some-people-and-slows-your-cpu-down-to-just-0-5ghz-ryzen-cpus.3685352/>
- FILE_IO_PRIORITY_HINT_INFO: <https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_io_priority_hint_info>
- Windows I/O priorities overview (two app-usable levels): <https://bitsum.com/pl_io_priority.php>
- Very Low I/O = 1–3 % of disk measurement: <https://jakubjares.com/2015/03/06/lower-io-priority/>
- 7-Zip "Background ≠ low I/O" feature request: <https://sourceforge.net/p/sevenzip/feature-requests/1632/>
- MEMORY_PRIORITY_INFORMATION: <https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/ns-processthreadsapi-memory_priority_information>
- JOBOBJECT_EXTENDED_LIMIT_INFORMATION (commit limits): <https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_extended_limit_information>
- zstd manual (window/hash/chain memory, ultra levels, d_windowLogMax, MT params): <https://facebook.github.io/zstd/zstd_manual.html>, <https://github.com/facebook/zstd/blob/dev/programs/zstd.1.md>
- RFC 9659 (8 MiB interop window ceiling): <https://www.ietf.org/rfc/rfc9659.html>
- 7-Zip -m switch (LZMA memory formulas, defaults, mt): <https://7-zip.opensource.jp/chm/cmdline/switches/method.htm>
- 7-Zip 24.09 dictionary default bump: <https://linuxiac.com/7-zip-24-09-file-archiver-enhances-lzma-compression/>; history: <https://www.7-zip.org/history.txt>
- 7-Zip -mmemuse / -smemx / 80 % default: <https://sourceforge.net/p/sevenzip/discussion/45798/thread/5155258a1c/>, <https://github.com/M2Team/NanaZip/issues/389>
- WinRAR 7.0 (64 GiB dicts, extraction refusal/prompt): <https://www.win-rar.com/singlenewsview.html?L=0&tx_ttnews%5Btt_news%5D=251>
- WinRAR -ri priority/sleep switch: <https://documentation.help/WinRAR/HELPSwRI.htm>
- rayon ThreadPoolBuilder (num_threads/start_handler/stack_size): <https://docs.rs/rayon/latest/rayon/struct.ThreadPoolBuilder.html>; two-pool priority workaround: <https://users.rust-lang.org/t/dealing-with-work-priority-and-rayon/30954>
- thread-priority crate (3.1.1, MIT, active): <https://lib.rs/crates/thread-priority>
- rayon 1.12.0 status: <https://lib.rs/crates/rayon>
- UIforETW input logging: <https://randomascii.wordpress.com/2015/04/14/uiforetw-windows-performance-made-easier/>
- MS guidance on measuring interaction responsiveness (TraceLogging + WPR/WPA): <https://learn.microsoft.com/en-us/windows/apps/performance/responsive>
- UI-thread responsiveness monitoring via Win32k UIUnresponsiveness ETW: <https://minidump.net/measuring-ui-responsiveness/>
- LatencyMon (DPC/hard-fault attribution): <https://www.resplendence.com/latencymon>
- windows-sys Threading module (all listed APIs present): <https://docs.rs/windows-sys/latest/windows_sys/Win32/System/Threading/index.html>
