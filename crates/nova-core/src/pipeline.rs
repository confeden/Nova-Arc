//! Multi-threaded, memory-bounded compression pipeline.
//!
//! Shape: one reader (the calling thread) splits files into content-defined
//! chunks, hashes them and resolves dedup; unique chunks go to N compression
//! workers; a single writer thread appends payloads to the archive **in
//! submission order**, so chunk indices are predictable while the reader is
//! still running.
//!
//! Two properties matter more than raw speed:
//! - **Bounded memory.** A byte budget is acquired before a chunk enters the
//!   pipeline and released once it has been written, so an archive of any
//!   size runs in the same RAM. Backpressure falls out of it: the reader
//!   blocks instead of queueing gigabytes.
//! - **Deterministic layout.** The writer emits chunks in submission order,
//!   so the reader can predict each chunk's index and build file entries
//!   without waiting for compression.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use anyhow::{anyhow, bail, Result};
use crossbeam_channel::{bounded, Receiver, Sender};

use crate::analyze::Tier;
use crate::codec::{self, Codec};
use crate::filters::Applied;
use crate::manifest::ChunkRec;

/// Memory the process needs before any packing work: manifest, buffers,
/// runtime. Reserved off the top of the budget.
const BASE_BYTES: u64 = 32 * 1024 * 1024;

/// Chunk buffers a worker holds while compressing (input + output).
const WORKER_CHUNK_BYTES: u64 = 2 * crate::archive::MAX_CHUNK as u64;

/// PPMd7's suballocator pool, allocated by the DECODER as well as the encoder.
///
/// This must be `codec::PPMD7_MEM_MAX`, not a smaller guess. `ppmd7_mem_size`
/// asks for 32x the unit and clamps at 256 MiB, so every max-tier unit of 8 MiB
/// or more takes the full ceiling. It said 64 MiB, which let
/// `extract_workers` spawn four times as many workers as the memory budget
/// actually allows — measured, extracting a max-tier archive peaked at 960 MiB
/// against a model that expected a quarter of that.
const PPMD_POOL_BYTES: u64 = crate::codec::PPMD7_MEM_MAX as u64;

#[derive(Clone, Copy, Debug)]
pub struct PackOptions {
    pub tier: Tier,
    /// 0 = all logical cores.
    pub threads: usize,
    /// 0 = auto (see nova_platform::memory_budget).
    pub memory_budget: u64,
}

impl PackOptions {
    pub fn new(tier: Tier) -> Self {
        PackOptions {
            tier,
            threads: 0,
            memory_budget: 0,
        }
    }

    /// Resolve "auto" into concrete numbers: how many workers, and how many
    /// bytes may be in flight at once.
    ///
    /// Workers are capped by the memory budget (a small budget means fewer
    /// threads, never swapping). The in-flight limit is then capped by what
    /// those workers can actually use, so a machine with lots of free RAM
    /// does not get gigabytes of queued chunks it has no use for.
    pub fn resolve(&self) -> (usize, u64) {
        let budget = nova_platform::memory_budget(if self.memory_budget == 0 {
            None
        } else {
            Some(self.memory_budget)
        });
        let cores = if self.threads == 0 {
            nova_platform::logical_cores()
        } else {
            self.threads
        };
        // budget ≈ BASE + workers × (compressor + chunk buffers) + queued chunks
        let per_worker = self.tier.worker_memory() + WORKER_CHUNK_BYTES;
        let for_workers = budget.saturating_sub(BASE_BYTES).max(per_worker);
        let workers = cores.min((for_workers / per_worker).max(1) as usize).max(1);
        let in_flight = budget
            .saturating_sub(BASE_BYTES + workers as u64 * self.tier.worker_memory())
            .clamp(
                WORKER_CHUNK_BYTES,
                workers as u64 * 4 * crate::archive::MAX_CHUNK as u64,
            );
        (workers, in_flight)
    }

    /// Workers for extraction, which depends entirely on what the archive is
    /// made of — hence `slow_codecs`, meaning it contains LZMA2 or PPMd7
    /// chunks.
    ///
    /// With zstd/store only, unpacking is bound by file creation and disk
    /// I/O, and extra threads make it *slower*: 5751 small files take 1.0 s
    /// on one thread and 2.5 s on eight, because parallel writers fight over
    /// NTFS directory metadata and turn sequential reads into seeks. So the
    /// default there is a single worker.
    ///
    /// PPMd7 decodes at ~20 MB/s per thread, which makes extraction CPU-bound
    /// instead: the same corpus at the max tier takes 19.6 s on one thread and
    /// 7.5 s on eight. So slow archives get every core the memory budget
    /// allows — PPMd7's model pool is 64 MiB per worker, which is the term
    /// that matters on a small budget.
    /// `slow_codecs` means "decode is CPU-bound", which decides both the thread
    /// count and the per-worker memory. zstd and store leave extraction
    /// I/O-bound and measured SLOWER on more threads.
    ///
    /// libbsc belongs in the slow set, and the first version of this had it
    /// wrong. The `bsc` command line decodes 100 MB in 0.9 s, which reads like
    /// 110 MB/s — but that figure is multithreaded across blocks, and nova
    /// disables libbsc's own threads. Single-threaded it is ~25 MB/s, and
    /// treating it as fast made a normal-tier Silesia archive take 8.7 s to
    /// extract where it had taken 0.5 s. Measured, not reasoned.
    ///
    /// `hungry_codecs` is kept separate because the inverse BWT holds an index
    /// four bytes wide per byte of block: a future codec could be hungry
    /// without being slow.
    pub fn extract_workers(&self, slow_codecs: bool, hungry_codecs: bool) -> usize {
        let cores = if self.threads == 0 {
            nova_platform::logical_cores()
        } else {
            self.threads
        };
        if !slow_codecs && self.threads == 0 {
            return 1;
        }
        let budget = nova_platform::memory_budget(if self.memory_budget == 0 {
            None
        } else {
            Some(self.memory_budget)
        });
        let per_worker = if slow_codecs || hungry_codecs {
            PPMD_POOL_BYTES + WORKER_CHUNK_BYTES
        } else {
            WORKER_CHUNK_BYTES
        };
        let by_mem = (budget.saturating_sub(BASE_BYTES) / per_worker).max(1) as usize;
        cores.min(by_mem).max(1)
    }
}

/// A chunk on its way to being stored.
struct Job {
    seq: u64,
    data: Vec<u8>,
    hash: [u8; 16],
    /// Codecs to try, with their per-codec parameter. More than one means the
    /// max tier's tournament: compress with each, keep the smallest.
    candidates: Vec<(Codec, u8)>,
    /// What the analyzer said this unit is. Carried for diagnostics only —
    /// the entrant list was already chosen from it — so that a trace can
    /// answer "which codec wins on which kind of data" from a real pack.
    kind: Option<crate::analyze::Class>,
    /// Reversible transform to apply before compressing (0 = none).
    filter: u8,
    /// Set when this unit is one piece of a .wav that was too large to be
    /// transformed whole. A middle piece is bare PCM with no `fmt ` chunk in
    /// it, so the format cannot be re-parsed from the bytes and has to arrive
    /// with the job. The stored record carries it onward, which is why the
    /// decode side needs nothing extra.
    wav: Option<crate::wav::Piece>,
    /// Bytes charged to the memory budget for this chunk.
    charge: u64,
}

struct Done {
    seq: u64,
    payload: Vec<u8>,
    codec: Codec,
    param: u8,
    filter: u8,
    /// Original bytes, before any filter. What the hash covers.
    unpacked: u64,
    /// What the codec actually saw; 0 when the filter preserved length.
    filtered: u64,
    hash: [u8; 16],
    charge: u64,
}

/// Idle worker slots, which a job may borrow to run its own entrants side by
/// side instead of one after another.
///
/// The max tier makes FEW units — enwik8 is 2, a source tree 5 — so on eight
/// cores most of the machine is idle while one worker runs a unit's entrants in
/// sequence. Borrowing turns that sequence into a parallel one at no cost in
/// bytes: which entrant wins is decided by size and by position in the field,
/// never by which finished first.
///
/// The cap is the worker count, and that is the whole point of counting rather
/// than just spawning: `Tier::worker_memory` budgets ONE codec's tables per
/// worker, so as long as concurrent codec runs never exceed the worker count,
/// the memory model is exactly the one that was already measured. A job that
/// finds nothing free simply runs its entrants in sequence.
struct Slots {
    workers: usize,
    /// Units submitted and not yet finished — waiting in the channel and
    /// running in a worker count THE SAME. Two counters would leave a window
    /// between a worker taking a job and marking itself busy, in which that
    /// job's own slot looks free and can be lent away.
    outstanding: AtomicUsize,
    /// Slots lent out to jobs running entrants in parallel.
    lent: AtomicUsize,
    /// Set once the reader has submitted its last unit.
    sealed: AtomicBool,
}

impl Slots {
    fn new(workers: usize) -> Self {
        Slots {
            workers,
            outstanding: AtomicUsize::new(0),
            lent: AtomicUsize::new(0),
            sealed: AtomicBool::new(false),
        }
    }

    fn submitted(&self) {
        self.outstanding.fetch_add(1, Ordering::AcqRel);
    }

    fn finished(&self) {
        // Saturating, not wrapping: an unbalanced count would go to usize::MAX
        // and quietly switch lending off for the rest of the run.
        let _ = self
            .outstanding
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                Some(n.saturating_sub(1))
            });
    }

    /// No more units are coming, so an idle worker will stay idle.
    fn seal(&self) {
        self.sealed.store(true, Ordering::Release);
    }

    /// Take up to `want` idle slots; returns how many were granted.
    ///
    /// NOTHING IS LENT WHILE THE READER IS STILL PRODUCING, and that is not
    /// caution, it is measured: an idle worker mid-run is usually a worker
    /// between jobs, so lending its slot oversubscribes the machine a moment
    /// later. Doing it on the free count alone cost 6-8% of wall clock on the
    /// corpora with many units (Silesia 29.1 → 31.0 s, Firefox 45.0 → 48.2)
    /// while helping the few-unit ones. Once the reader is done the count is
    /// honest — nothing new can arrive — and the tail is exactly where units
    /// run out before cores do.
    fn borrow(&self, want: usize) -> usize {
        if want == 0 || !self.sealed.load(Ordering::Acquire) {
            return 0;
        }
        let mut lent = self.lent.load(Ordering::Acquire);
        loop {
            let free = self
                .workers
                .saturating_sub(self.outstanding.load(Ordering::Acquire) + lent);
            let take = want.min(free);
            if take == 0 {
                return 0;
            }
            match self.lent.compare_exchange_weak(
                lent,
                lent + take,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return take,
                Err(seen) => lent = seen,
            }
        }
    }

    fn give_back(&self, n: usize) {
        if n > 0 {
            self.lent.fetch_sub(n, Ordering::AcqRel);
        }
    }
}

/// How often the governor re-reads how much memory the machine has left.
const GOVERNOR_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);

/// Counting semaphore over bytes in flight, with an abort path so a failing
/// writer can never leave the reader blocked forever.
///
/// The limit is not fixed. Packing starts with a fraction of the budget and
/// the governor raises it while the machine has memory to spare, lowering it
/// when free memory runs short. Claiming the whole budget up front would make
/// the archiver the reason another program starts swapping; growing into it
/// gradually keeps us out of the way while still using what is there.
struct Budget {
    /// Current cap on bytes in flight.
    limit: AtomicU64,
    /// Ceiling the governor may never exceed (the resolved budget).
    hard: u64,
    /// Floor, so at least one chunk always fits and progress is guaranteed.
    floor: u64,
    state: Mutex<BudgetState>,
    cv: Condvar,
}

struct BudgetState {
    used: u64,
    aborted: bool,
}

impl Budget {
    fn new(hard: u64) -> Self {
        let hard = hard.max(WORKER_CHUNK_BYTES);
        Budget {
            // Start at a quarter: enough to fill the workers, small enough that
            // a short job never grabs the whole budget.
            limit: AtomicU64::new((hard / 4).max(WORKER_CHUNK_BYTES)),
            hard,
            floor: WORKER_CHUNK_BYTES,
            state: Mutex::new(BudgetState {
                used: 0,
                aborted: false,
            }),
            cv: Condvar::new(),
        }
    }

    fn limit(&self) -> u64 {
        self.limit.load(Ordering::Relaxed)
    }

    /// Reserve `n` bytes, blocking while the pipeline is full. A single chunk
    /// larger than the whole budget is still admitted when nothing else is in
    /// flight, so progress is always possible. Returns false once aborted.
    fn acquire(&self, n: u64) -> bool {
        let mut st = self.state.lock().expect("budget mutex poisoned");
        loop {
            if st.aborted {
                return false;
            }
            if st.used == 0 || st.used + n <= self.limit() {
                st.used += n;
                return true;
            }
            st = self.cv.wait(st).expect("budget mutex poisoned");
        }
    }

    fn release(&self, n: u64) {
        let mut st = self.state.lock().expect("budget mutex poisoned");
        st.used = st.used.saturating_sub(n);
        self.cv.notify_all();
    }

    fn abort(&self) {
        let mut st = self.state.lock().expect("budget mutex poisoned");
        st.aborted = true;
        st.used = 0;
        self.cv.notify_all();
    }

    /// Adjust the cap from how much memory the machine has free. Growth is
    /// gradual (an eighth of the ceiling at a time) and retreat is immediate
    /// (halving), because being slow to give memory back is what users feel.
    fn govern(&self) {
        let Some(m) = nova_platform::memory_status() else {
            // Without a reading, open up to the ceiling rather than stay
            // throttled forever.
            self.limit.store(self.hard, Ordering::Relaxed);
            self.cv.notify_all();
            return;
        };
        let tight = (m.total / 10).max(1 << 30); // 10% of RAM, at least 1 GiB
        let roomy = (m.total / 5).max(2 << 30); // 20% of RAM, at least 2 GiB
        let cur = self.limit();
        let next = if m.available < tight {
            (cur / 2).max(self.floor)
        } else if m.available > roomy {
            (cur + self.hard / 8).min(self.hard)
        } else {
            cur
        };
        if next != cur {
            self.limit.store(next, Ordering::Relaxed);
            // Waiters blocked against the old, smaller cap must re-check.
            let _guard = self.state.lock().expect("budget mutex poisoned");
            self.cv.notify_all();
        }
    }
}

/// Result of writing one packing run.
pub struct PackOutput {
    pub chunks: Vec<ChunkRec>,
    pub bytes_stored: u64,
}

/// Running totals of what has actually reached the archive, reported by the
/// writer as units are committed.
///
/// This is the only honest measure of packing progress. The reader runs ahead
/// by the whole in-flight budget — up to 1 GiB of charge, so ~512 MiB of source
/// at the max tier — and then sleeps in [`Budget::acquire`], so anything the
/// reader can count says "read", not "done". The writer is also the one thread
/// still working while the reader sleeps, which is exactly when a UI needs to
/// hear something.
#[derive(Clone, Copy, Debug, Default)]
pub struct StoredTick {
    /// Original bytes of every unit committed so far.
    pub unpacked: u64,
    /// Compressed bytes appended to the archive so far.
    pub packed: u64,
    /// Units committed so far. On a source tree the max tier makes only ~6
    /// units and compresses them in parallel, so nothing lands for tens of
    /// seconds — a count of blocks is the only thing that explains the silence.
    pub units: u64,
}

/// Called by the writer thread after each batch of units is committed. Must be
/// cheap: the writer is the pipeline's serialization point, so a slow callback
/// throttles the whole run.
pub type StoredFn<'a> = &'a (dyn Fn(StoredTick) + Send + Sync);

/// Runs the worker/writer threads for the duration of `produce`.
///
/// `produce` is called on the current thread with a submitter; every
/// `submit` returns the index the chunk will have in `PackOutput::chunks`
/// (relative to this run), available immediately even though the chunk has
/// not been compressed or written yet.
pub fn pack_with<F>(
    file: &mut File,
    opts: &PackOptions,
    on_stored: Option<StoredFn>,
    produce: F,
) -> Result<PackOutput>
where
    F: FnOnce(&mut Submitter) -> Result<()>,
{
    let (workers, budget_bytes) = opts.resolve();
    let level = opts.tier.zstd_level();
    let budget = Budget::new(budget_bytes);
    let (job_tx, job_rx) = bounded::<Job>(workers * 2);
    let (done_tx, done_rx) = bounded::<Result<Done>>(workers * 2);

    // The governor lives as long as the packing run and is asked to stop
    // through this flag, so it cannot outlive the borrowed budget.
    let running = AtomicBool::new(true);
    let slots = Slots::new(workers);

    std::thread::scope(|scope| -> Result<PackOutput> {
        for _ in 0..workers {
            let job_rx: Receiver<Job> = job_rx.clone();
            let done_tx: Sender<Result<Done>> = done_tx.clone();
            let slots = &slots;
            scope.spawn(move || worker_loop(job_rx, done_tx, level, slots));
        }
        drop(job_rx);
        drop(done_tx);

        scope.spawn({
            let (budget, running) = (&budget, &running);
            move || {
                while running.load(Ordering::Relaxed) {
                    std::thread::sleep(GOVERNOR_INTERVAL);
                    budget.govern();
                }
            }
        });

        let writer = scope.spawn({
            let budget = &budget;
            move || writer_loop(file, done_rx, budget, on_stored)
        });

        let mut submitter = Submitter {
            tx: Some(job_tx),
            budget: &budget,
            slots: &slots,
            next_seq: 0,
        };
        let produced = produce(&mut submitter);
        // Closing the job channel drains the workers, which closes the done
        // channel, which lets the writer finish. From here an idle worker is
        // idle for good, so the jobs still running may take its slot.
        submitter.tx = None;
        slots.seal();
        if produced.is_err() {
            budget.abort();
        }

        let written = writer
            .join()
            .map_err(|_| anyhow!("writer thread panicked"))?;
        running.store(false, Ordering::Relaxed);
        produced?;
        written
    })
}

pub struct Submitter<'a> {
    tx: Option<Sender<Job>>,
    budget: &'a Budget,
    slots: &'a Slots,
    next_seq: u64,
}

impl Submitter<'_> {
    /// How many units have been handed to the pipeline so far. Together with
    /// the writer's committed count this is the honest shape of the work: at
    /// the max tier a whole tree is a handful of units.
    pub fn submitted(&self) -> u64 {
        self.next_seq
    }

    /// Hand a chunk to the pipeline; returns its index within this run.
    pub fn submit(&mut self, data: Vec<u8>, hash: [u8; 16], codec: Codec) -> Result<u32> {
        self.submit_filtered(data, hash, vec![(codec, 0)], 0, None)
    }

    /// As [`Self::submit`], but with a reversible filter (see
    /// `crate::filters`) and a list of codecs to try.
    pub fn submit_filtered(
        &mut self,
        data: Vec<u8>,
        hash: [u8; 16],
        candidates: Vec<(Codec, u8)>,
        filter: u8,
        kind: Option<crate::analyze::Class>,
    ) -> Result<u32> {
        self.submit_job(data, hash, candidates, filter, None, kind)
    }

    /// As [`Self::submit_filtered`], for one piece of a split .wav.
    pub fn submit_wav(
        &mut self,
        data: Vec<u8>,
        hash: [u8; 16],
        candidates: Vec<(Codec, u8)>,
        piece: crate::wav::Piece,
    ) -> Result<u32> {
        self.submit_job(
            data,
            hash,
            candidates,
            crate::filters::Filter::Wav.id(),
            Some(piece),
            Some(crate::analyze::Class::Wav),
        )
    }

    fn submit_job(
        &mut self,
        data: Vec<u8>,
        hash: [u8; 16],
        candidates: Vec<(Codec, u8)>,
        filter: u8,
        wav: Option<crate::wav::Piece>,
        kind: Option<crate::analyze::Class>,
    ) -> Result<u32> {
        // Input plus a worst-case output buffer of the same size.
        let charge = data.len() as u64 * 2;
        if !self.budget.acquire(charge) {
            bail!("compression pipeline stopped");
        }
        let seq = self.next_seq;
        let job = Job {
            seq,
            data,
            hash,
            candidates,
            kind,
            filter,
            wav,
            charge,
        };
        match self.tx.as_ref().expect("submitter closed").send(job) {
            Ok(()) => {
                self.slots.submitted();
                self.next_seq += 1;
                u32::try_from(seq).map_err(|_| anyhow!("too many chunks in one operation"))
            }
            Err(_) => {
                self.budget.release(charge);
                bail!("compression pipeline stopped")
            }
        }
    }
}

fn worker_loop(rx: Receiver<Job>, tx: Sender<Result<Done>>, level: i32, slots: &Slots) {
    // One compressor per worker: allocating a zstd context per chunk would
    // dominate the cost for small chunks.
    let mut comp = zstd::bulk::Compressor::new(level).ok();
    if let Some(c) = comp.as_mut() {
        // A chunk never exceeds MAX_CHUNK, so a larger match window costs
        // memory (tens of MB per worker at high levels) and buys nothing.
        let _ = c.set_parameter(zstd::zstd_safe::CParameter::WindowLog(
            crate::archive::MAX_CHUNK.trailing_zeros(),
        ));
    }
    while let Ok(job) = rx.recv() {
        // The job stops counting only when it is done, so the slot this
        // worker is using can never be lent to another job's entrants.
        let out = compress_job(comp.as_mut(), job, level, slots);
        slots.finished();
        if tx.send(out).is_err() {
            break; // writer is gone
        }
    }
}

/// One entrant's result on one unit, for `NOVA_TOURNEY_TRACE`.
struct TraceCandidate {
    seq: u64,
    kind: Option<crate::analyze::Class>,
    filter: u8,
    /// Original length, before any filter — what the ratio should be read against.
    unpacked: u64,
    /// What the codec was actually handed, which a rebuilding filter changes.
    fed: u64,
    codec: Codec,
    param: u8,
    coded: u64,
    elapsed: std::time::Duration,
}

/// Where the tournament trace goes, opened once. `None` — the normal case —
/// when `NOVA_TOURNEY_TRACE` is unset or the file cannot be created.
static TOURNEY_TRACE: std::sync::OnceLock<Option<Mutex<File>>> = std::sync::OnceLock::new();

/// Record what one entrant produced and what it cost.
///
/// The tournament is the max tier's dominant cost: every unit is compressed
/// once per entrant and all but one result is thrown away. Whether an entrant
/// earns that CPU is a question about real units — a per-file probe answers a
/// different one and has been wrong before — so the answer has to come out of
/// an ordinary pack. Diagnostics only: the rows change nothing that is written,
/// and nothing is opened unless the variable is set.
fn trace_candidate(c: TraceCandidate) {
    let sink = TOURNEY_TRACE.get_or_init(|| {
        let path = std::env::var_os("NOVA_TOURNEY_TRACE")?;
        let mut f = File::create(path).ok()?;
        writeln!(
            f,
            "seq\tkind\tfilter\tunpacked\tfed\tcodec\tparam\tcoded\tms"
        )
        .ok()?;
        Some(Mutex::new(f))
    });
    let Some(sink) = sink.as_ref() else { return };
    let kind = match c.kind {
        Some(k) => format!("{k:?}"),
        None => "-".to_string(),
    };
    let Ok(mut f) = sink.lock() else { return };
    let _ = writeln!(
        f,
        "{}\t{}\t{}\t{}\t{}\t{:?}\t{}\t{}\t{:.1}",
        c.seq,
        kind,
        c.filter,
        c.unpacked,
        c.fed,
        c.codec,
        c.param,
        c.coded,
        c.elapsed.as_secs_f64() * 1000.0
    );
}

/// Units below this run the whole field: the tournament on a few megabytes
/// costs about a second, which is less than a qualifier could save, and a
/// sample of something this small says little.
const QUALIFY_FROM: usize = 4 * 1024 * 1024;

/// The qualifier judges from this fraction of the unit — measured against 1/8
/// and 1/32 on the same units: 1/16 ranks as well as 1/8 (the widest window a
/// true winner needed is 10.4% against 10.1%) for half the cost, while 1/32
/// starts to blur (11.7%) and saves nothing more.
const QUALIFY_FRACTION: usize = 16;

/// ...taken as this many runs spread across it, never as one prefix. A unit is
/// often thousands of files and its first eighth can be one file type: at equal
/// cost the prefix shape needed a 20% window to lose nothing where the spread
/// shape needed 15%, and it mis-ranked twice as many units at every window
/// below that.
const QUALIFY_SLICES: usize = 8;

/// How far behind the sample's best an entrant may be and still run.
///
/// One window for the whole field is the wrong shape, because a sample does not
/// under-state every entrant equally. MEASURED over the 36 units of at least
/// 4 MiB in `test/tourney` (enwik8, Silesia, a source tree, an installed
/// Firefox, PDFs, the deflate corpus): going from the sample to the whole unit
/// gains LZMA2 14.7% on average and PPMd7 10.5%, and on a source tree — where
/// the win is cross-file duplication that only a full-size dictionary reaches —
/// LZMA2 gains 7 to 16 POINTS more than bsc. A window that ignores that prunes
/// LZMA2 on exactly the data it wins.
///
/// So LZMA2's window is wide. Of the eleven units whose sample winner is not
/// their real winner, the widest gap a true winner had to survive was 10.1%,
/// and 15% leaves half again as much headroom. PPMd7's is narrow because its
/// bias runs the other way — the sample FLATTERS it — and bsc has none at all:
/// it is 6% of the tournament's CPU and wins 41% of the units, so it always
/// runs and four of those eleven cases stop being risks.
const QUALIFY_WINDOW_LZMA: u64 = 15;
const QUALIFY_WINDOW_PPMD: u64 = 2;

/// Split `items` into lanes — one per idle worker slot this job can borrow,
/// plus the worker's own — run `f` over each lane and return the lanes'
/// results in order.
///
/// `f` is handed a slice and the index its first element has in `items`, so a
/// lane can fold as it goes instead of handing back everything it produced.
/// That matters: a tournament that keeps all four coded buffers alive at once
/// holds tens of megabytes per job for nothing, and it measured 10% of the wall
/// clock on Firefox.
///
/// Lane results come back in lane order, so nothing downstream can tell which
/// lane finished first — a tournament's verdict has to be a function of the
/// bytes (I12).
fn race<T, R, F>(items: &[T], slots: &Slots, f: F) -> Result<Vec<R>>
where
    T: Sync,
    R: Send,
    F: Fn(&[T], usize) -> Result<R> + Sync,
{
    let extra = if items.len() > 1 {
        slots.borrow(items.len() - 1)
    } else {
        0
    };
    if extra == 0 {
        return Ok(vec![f(items, 0)?]);
    }
    let per = items.len().div_ceil(extra + 1);
    let f = &f;
    let out: Result<Vec<R>> = std::thread::scope(|scope| {
        let lanes: Vec<_> = items
            .chunks(per)
            .enumerate()
            .skip(1)
            .map(|(n, chunk)| scope.spawn(move || f(chunk, n * per)))
            .collect();
        let mut out = match items.chunks(per).next() {
            Some(chunk) => vec![f(chunk, 0)?],
            None => Vec::new(),
        };
        for lane in lanes {
            out.push(
                lane.join()
                    .map_err(|_| anyhow!("a tournament lane panicked"))??,
            );
        }
        Ok(out)
    });
    slots.give_back(extra);
    out
}

/// Thin the field before it runs, by racing it over a sample of this very unit.
///
/// The max tier's four entrants cost four compressions and keep one result.
/// Measured across 83 real units, LZMA2 alone is 61% of that CPU while winning
/// 41% of the units, and no static rule can drop it: prose and source code are
/// both `Class::Text` and want opposite codecs — bsc beats LZMA2 by 15-25% on
/// enwik8, LZMA2 beats bsc by 9-39% on a source tree.
///
/// So the choice is made from the data rather than from the class, and from
/// THIS unit rather than from a per-file probe — the lesson of N24, where an
/// entrant that won every file won zero units. On the six corpora it was tuned
/// against it prunes an entrant that would have won ZERO times, for a third of
/// the tournament's CPU and a quarter of its critical path.
fn qualify(
    candidates: Vec<(Codec, u8)>,
    data: &[u8],
    level: i32,
    slots: &Slots,
) -> Vec<(Codec, u8)> {
    if candidates.len() < 3 || data.len() < QUALIFY_FROM {
        return candidates;
    }
    let sample = spread_sample(data, data.len() / QUALIFY_FRACTION);
    // A codec that fails on the sample is not evidence about the unit, so the
    // field goes through untouched and the real run decides.
    let Ok(lanes) = race(&candidates, slots, |lane: &[(Codec, u8)], _| {
        lane.iter()
            .map(|&(codec, param)| {
                codec::compress(codec, level, param, &sample).map(|c| c.len() as u64)
            })
            .collect::<Result<Vec<u64>>>()
    }) else {
        return candidates;
    };
    let sizes: Vec<u64> = lanes.concat();
    let Some(&best) = sizes.iter().min() else {
        return candidates;
    };
    // The two PPMd7 orders cost the same and rank almost together, so at most
    // one of them runs: the sample's preference, ties to the lower order.
    let ppmd_pick = candidates
        .iter()
        .zip(&sizes)
        .filter(|((codec, _), _)| *codec == Codec::Ppmd7)
        .min_by_key(|((_, param), &size)| (size, *param))
        .map(|((_, param), _)| *param);
    // Integer arithmetic and the original order, because this decides what goes
    // into the archive: the same unit must field the same entrants in the same
    // sequence on every machine and at every thread count (I8), and the
    // tournament breaks a tie in favour of whoever ran first.
    let kept: Vec<(Codec, u8)> = candidates
        .iter()
        .zip(&sizes)
        .filter(|(&(codec, param), &size)| match codec {
            Codec::Bsc => true,
            Codec::Ppmd7 => {
                Some(param) == ppmd_pick && size * 100 <= best * (100 + QUALIFY_WINDOW_PPMD)
            }
            _ => size * 100 <= best * (100 + QUALIFY_WINDOW_LZMA),
        })
        .map(|(&c, _)| c)
        .collect();
    if kept.is_empty() {
        candidates
    } else {
        kept
    }
}

/// `n` bytes of `data` as `QUALIFY_SLICES` evenly spread runs.
fn spread_sample(data: &[u8], n: usize) -> Vec<u8> {
    let run = n / QUALIFY_SLICES;
    let step = data.len() / QUALIFY_SLICES;
    let mut out = Vec::with_capacity(n);
    for i in 0..QUALIFY_SLICES {
        let start = i * step;
        out.extend_from_slice(&data[start..(start + run).min(data.len())]);
    }
    out
}

/// Write the bytes the tournament is about to see to `NOVA_UNIT_DUMP=<dir>`,
/// one file per unit, named `<seq>-<kind>.bin`.
///
/// Post-filter on purpose: an entrant sees x86-split or preflate output, not
/// the original file, so a rule tuned on files would be tuned on different
/// data. With the dump, a rule can be tried against real units offline instead
/// of by re-packing a corpus for every threshold. Diagnostics only.
fn dump_unit(seq: u64, kind: Option<crate::analyze::Class>, data: &[u8]) {
    let Some(dir) = std::env::var_os("NOVA_UNIT_DUMP") else {
        return;
    };
    let kind = match kind {
        Some(k) => format!("{k:?}"),
        None => "none".to_string(),
    };
    let dir = std::path::PathBuf::from(dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = std::fs::write(dir.join(format!("{seq:04}-{kind}.bin")), data);
}

fn compress_job(
    comp: Option<&mut zstd::bulk::Compressor<'_>>,
    job: Job,
    level: i32,
    slots: &Slots,
) -> Result<Done> {
    let Job {
        seq,
        mut data,
        hash,
        candidates,
        kind,
        filter,
        wav,
        charge,
    } = job;
    let unpacked = data.len() as u64;
    // Filters are reversible transforms that make the data more compressible;
    // the chunk hash covers the ORIGINAL bytes, so it is computed before this.
    //
    // A filter may change the length, and then two different numbers matter:
    // `unpacked` stays the original length forever (the hash and the extents
    // are expressed in it) while the codec works on, and the decoder must be
    // told, the filtered length.
    let f = crate::filters::Filter::from_id(filter)?;
    let mut original: Option<Vec<u8>> = None;
    if f.changes_length() {
        // A rebuilt buffer cannot be turned back by undoing it in place, so the
        // original has to survive for the store fallback and for the round-trip
        // check below.
        original = Some(data.clone());
    }
    // A filter that cannot do its job is not an error: the analyzer proposes a
    // transform from a file's magic, and a container whose streams preflate
    // cannot model simply gets stored the ordinary way. Aborting the whole
    // operation over one unit would be absurd.
    // A split .wav cannot go through `Filter::apply`, which reads the format out
    // of the buffer it is handed: a middle piece is bare PCM. Same filter id,
    // same record, same decode path — only the encoder needs telling.
    let attempt = match wav {
        Some(p) => crate::wav::encode_piece(&data, p).map(|v| {
            data = v;
            crate::filters::Applied::Rebuilt
        }),
        None => f.apply(&mut data),
    };
    let applied = match attempt {
        Ok(a) => a,
        Err(_) => {
            let data = original.take().unwrap_or(data);
            return plain(
                seq, data, hash, charge, unpacked, candidates, kind, comp, level, slots,
            );
        }
    };
    let filtered = data.len() as u64;
    // The coded length is what a reader must hand the codec, and `verify_chunk`
    // REFUSES anything above `MAX_CODED_CHUNK`. Writing one anyway produces an
    // archive nova cannot extract — the worst outcome an archiver has, because
    // the source may be gone by the time anyone finds out. Each filter bounding
    // its own output is not enough: only this line knows the number that ends
    // up in the manifest, so it is checked here for every filter that exists or
    // ever will. Deflate is the one that can reach it — a PDF recovers ~6x its
    // own size in plaintext.
    if filtered > crate::archive::MAX_CODED_CHUNK {
        // Recovering the original is the same two cases as the store fallback
        // below, and it is spelled out rather than assumed: an in-place filter
        // cannot reach this bound today, but "cannot" is not a thing to store
        // bytes on.
        match applied {
            Applied::InPlace => f.unapply(&mut data)?,
            Applied::Rebuilt => {
                data = original
                    .take()
                    .expect("a rebuilt filter keeps the original")
            }
        }
        return plain(
            seq, data, hash, charge, unpacked, candidates, kind, comp, level, slots,
        );
    }

    // Bit-exact or nothing. A transform that builds a new representation has to
    // prove it can rebuild the original BEFORE the archive keeps it — a
    // recompression bug must never produce an archive that cannot be extracted.
    // The in-place filters skip this: they are their own inverse and the chunk
    // hash already proves it on every extract.
    if applied == Applied::Rebuilt {
        let mut check = data.clone();
        let ok = f.unapply(&mut check).is_ok() && original.as_deref().is_some_and(|o| check == o);
        if !ok {
            let data = original
                .take()
                .expect("a rebuilt filter keeps the original");
            return plain(
                seq, data, hash, charge, unpacked, candidates, kind, comp, level, slots,
            );
        }
    }

    // Try every candidate and keep the smallest result. With one candidate
    // this is just "compress"; with several it is the max tier's tournament,
    // which trades encode time for ratio because no static rule picks the
    // right codec for arbitrary data — but the field is thinned first, by
    // running it over a sample of this very unit, and what survives runs side
    // by side when the machine has idle workers to lend.
    dump_unit(seq, kind, &data);
    let candidates = qualify(candidates, &data, level, slots);
    let stored = candidates.first().is_some_and(|&(c, _)| c == Codec::Store);
    let mut best: Option<(usize, Vec<u8>)> = None;
    if !stored {
        // One lane keeps only its own smallest result: a tie goes to whoever is
        // earlier in the FIELD, never to whoever finished first, so the archive
        // cannot depend on scheduling.
        let lane = |lane: &[(Codec, u8)], base: usize| -> Result<Option<(usize, Vec<u8>)>> {
            let mut won: Option<(usize, Vec<u8>)> = None;
            for (i, &(codec, param)) in lane.iter().enumerate() {
                let started = std::time::Instant::now();
                let packed = codec::compress(codec, level, param, &data)?;
                trace_candidate(TraceCandidate {
                    seq,
                    kind,
                    filter,
                    unpacked,
                    fed: data.len() as u64,
                    codec,
                    param,
                    coded: packed.len() as u64,
                    elapsed: started.elapsed(),
                });
                if won.as_ref().is_none_or(|(_, w)| packed.len() < w.len()) {
                    won = Some((base + i, packed));
                }
            }
            Ok(won)
        };
        // The zstd context is per worker and reused across chunks, because
        // allocating one per chunk would dominate the cost of a small one. It
        // is only ever a lone candidate (the max field has no zstd), so it
        // stays on the sequential path where the worker owns it.
        best = match candidates.as_slice() {
            [(Codec::Zstd, _)] => match comp {
                Some(c) => Some((0, c.compress(&data)?)),
                None => Some((0, codec::compress(Codec::Zstd, level, 0, &data)?)),
            },
            rest => race(rest, slots, lane)?.into_iter().flatten().fold(
                None,
                |won: Option<(usize, Vec<u8>)>, next| match won {
                    Some(w) if w.1.len() <= next.1.len() => Some(w),
                    _ => Some(next),
                },
            ),
        };
    }
    let best = best.map(|(i, payload)| {
        let (codec, param) = candidates[i];
        (codec, param, payload)
    });

    // Three things could be kept, and the smallest wins as long as it beats the
    // ORIGINAL length — not the filtered one, or a filter that expands 2 MB of
    // zip into 20 MB of plaintext and compresses it back to 2.5 MB would "win"
    // while making the archive bigger.
    //
    // The middle candidate — the filtered form stored verbatim — exists for
    // lepton: its output is already entropy-coded, so LZMA2 and PPMd7 spend
    // seconds on it and gain nothing (measured: neither ever beat the lepton
    // blob). A STORED payload that is filtered rather than original is new, and
    // it is safe because the coded length is recorded beside it and a reader too
    // old to know the filter rejects the id outright instead of guessing.
    let coded = best.as_ref().map_or(usize::MAX, |(_, _, p)| p.len());
    let bare = if applied == Applied::Rebuilt {
        data.len()
    } else {
        usize::MAX
    };
    if coded.min(bare) as u64 >= unpacked {
        // Nothing beat the original bytes. Both the filter byte and the coded
        // length must go back to "none", or a decoder sizes a Store payload
        // from a length it does not have.
        match applied {
            Applied::InPlace => f.unapply(&mut data)?,
            Applied::Rebuilt => {
                data = original
                    .take()
                    .expect("a rebuilt filter keeps the original")
            }
        }
        return store(seq, data, hash, charge, unpacked);
    }
    let (codec, param, filter, filtered, payload) = if bare < coded {
        (Codec::Store, 0, filter, filtered, data)
    } else {
        let (c, p, packed) = best.expect("a finite coded length means there was a candidate");
        (c, p, filter, filtered, packed)
    };
    Ok(Done {
        seq,
        payload,
        codec,
        param,
        filter,
        unpacked,
        // Recorded only when it differs, so every chunk nova wrote before this
        // existed — and every chunk with an in-place filter — keeps a manifest
        // byte-for-byte identical to what it had.
        filtered: if filtered == unpacked { 0 } else { filtered },
        hash,
        charge,
    })
}

/// Compress the original bytes with no filter at all — the path taken when a
/// proposed transform does not apply or does not round-trip.
#[allow(clippy::too_many_arguments)]
fn plain(
    seq: u64,
    data: Vec<u8>,
    hash: [u8; 16],
    charge: u64,
    unpacked: u64,
    candidates: Vec<(Codec, u8)>,
    kind: Option<crate::analyze::Class>,
    comp: Option<&mut zstd::bulk::Compressor<'_>>,
    level: i32,
    slots: &Slots,
) -> Result<Done> {
    compress_job(
        comp,
        Job {
            seq,
            data,
            hash,
            candidates,
            kind,
            filter: 0,
            wav: None,
            charge,
        },
        level,
        slots,
    )
    .map(|mut d| {
        d.unpacked = unpacked;
        d
    })
}

/// Store the original bytes verbatim: no codec, no filter, no coded length.
fn store(seq: u64, data: Vec<u8>, hash: [u8; 16], charge: u64, unpacked: u64) -> Result<Done> {
    debug_assert_eq!(
        data.len() as u64,
        unpacked,
        "a stored chunk is the original"
    );
    Ok(Done {
        seq,
        payload: data,
        codec: Codec::Store,
        param: 0,
        filter: 0,
        unpacked,
        filtered: 0,
        hash,
        charge,
    })
}

fn writer_loop(
    file: &mut File,
    rx: Receiver<Result<Done>>,
    budget: &Budget,
    on_stored: Option<StoredFn>,
) -> Result<PackOutput> {
    let mut pending: BTreeMap<u64, Done> = BTreeMap::new();
    let mut next = 0u64;
    let mut chunks = Vec::new();
    let mut bytes_stored = 0u64;
    // Work is finished when a unit has been COMPRESSED, not when its turn to be
    // written comes up. Writing is a memcpy; compressing a 64 MiB unit at the
    // max tier is three encode passes. Counting only written units made a unit
    // that finished early but waited behind a slow predecessor invisible, and
    // its report arrived in a burst with the others — measured as a 36 s silence
    // followed by a jump from 48% to 100%.
    let mut bytes_done = 0u64;
    let mut offset = file.seek(SeekFrom::End(0))?;

    for (received, item) in rx.iter().enumerate() {
        let done = match item {
            Ok(d) => d,
            Err(e) => {
                budget.abort();
                return Err(e);
            }
        };
        bytes_done += done.unpacked;
        pending.insert(done.seq, done);
        // Write everything that has become contiguous: the archive layout
        // must follow submission order, not completion order.
        while let Some(d) = pending.remove(&next) {
            if let Err(e) = file.write_all(&d.payload) {
                budget.abort();
                return Err(e.into());
            }
            chunks.push(ChunkRec {
                offset,
                packed: d.payload.len() as u64,
                unpacked: d.unpacked,
                codec: d.codec.id(),
                param: d.param,
                filter: d.filter,
                filtered: d.filtered,
                hash: d.hash,
            });
            offset += d.payload.len() as u64;
            bytes_stored += d.payload.len() as u64;
            budget.release(d.charge);
            next += 1;
        }
        // Reported after the budget is released, so a slow listener can never
        // delay backpressure. One call per completed unit bounds the rate.
        if let Some(cb) = on_stored {
            cb(StoredTick {
                unpacked: bytes_done,
                packed: bytes_stored,
                units: received as u64 + 1,
            });
        }
    }
    if !pending.is_empty() {
        budget.abort();
        bail!(
            "compression pipeline ended with {} chunks unwritten",
            pending.len()
        );
    }
    Ok(PackOutput {
        chunks,
        bytes_stored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The max tier's field, in the order `Tier::candidates` builds it.
    fn field() -> Vec<(Codec, u8)> {
        vec![
            (Codec::Lzma2, 0),
            (Codec::Ppmd7, 10),
            (Codec::Ppmd7, 16),
            (Codec::Bsc, 0),
        ]
    }

    /// Prose-shaped bytes: enough structure that the entrants disagree, which
    /// is what gives the qualifier something to decide.
    fn wordy(n: usize) -> Vec<u8> {
        let words = [
            "the", "unit", "codec", "sample", "window", "entrant", "archive", "byte",
        ];
        let mut seed = 0x243F_6A88_85A3_08D3u64;
        let mut out = Vec::with_capacity(n + 16);
        while out.len() < n {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            out.extend_from_slice(words[(seed >> 33) as usize % words.len()].as_bytes());
            out.push(b' ');
        }
        out.truncate(n);
        out
    }

    /// Whatever the sample says, bsc runs: it is 6% of the tournament's CPU and
    /// wins 41% of real units, so it is the field's floor rather than a
    /// candidate. And the survivors keep the field's order, because the
    /// tournament breaks a tie in favour of whoever ran first.
    #[test]
    fn bsc_always_survives_and_order_is_kept() {
        let data = wordy(6 * 1024 * 1024);
        let kept = qualify(field(), &data, 19, &Slots::new(1));
        assert!(kept.contains(&(Codec::Bsc, 0)), "bsc was pruned: {kept:?}");
        assert!(kept.len() < 4, "nothing was pruned on prose: {kept:?}");
        let ids: Vec<u8> = kept.iter().map(|(c, _)| c.id()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "the field was reordered: {kept:?}");
    }

    /// At most one PPMd7 order runs: the pair costs the same twice and ranks
    /// almost together.
    #[test]
    fn at_most_one_ppmd_order_runs() {
        let data = wordy(6 * 1024 * 1024);
        let kept = qualify(field(), &data, 19, &Slots::new(1));
        assert!(kept.iter().filter(|(c, _)| *c == Codec::Ppmd7).count() <= 1);
    }

    /// Small units keep the whole field: a tournament on a few megabytes costs
    /// about a second, which is less than a qualifier could save.
    #[test]
    fn a_small_unit_is_not_qualified() {
        let data = wordy(QUALIFY_FROM - 1);
        assert_eq!(qualify(field(), &data, 19, &Slots::new(1)), field());
    }

    /// Lanes must not reorder anything: the tournament breaks a tie by
    /// position in the field, so results have to come back in that order
    /// whether they ran side by side or one after another.
    #[test]
    fn lanes_do_not_reorder_results() {
        let items: Vec<usize> = (0..7).collect();
        let f = |lane: &[usize], _base: usize| {
            Ok(lane
                .iter()
                .map(|i| vec![*i as u8; i + 1])
                .collect::<Vec<_>>())
        };
        let one = Slots::new(1);
        one.seal();
        let many = Slots::new(8);
        many.seal();
        let alone = race(&items, &one, f).expect("no lane fails").concat();
        let borrowed = race(&items, &many, f).expect("no lane fails").concat();
        assert_eq!(alone, borrowed);
        assert!(alone.iter().enumerate().all(|(i, v)| v.len() == i + 1));
    }

    /// A slot a worker is using is not free to lend, and what is lent comes
    /// back — including when a lane fails, or the pool would bleed out and
    /// every later unit would run its entrants in sequence.
    #[test]
    fn slots_are_accounted_for() {
        let slots = Slots::new(4);
        slots.seal();
        slots.submitted();
        assert_eq!(slots.borrow(4), 3, "a running job's own slot was lent");
        slots.give_back(3);
        slots.finished();

        let items: Vec<usize> = (0..4).collect();
        race(&items, &slots, |_: &[usize], _| Ok(Vec::<u8>::new())).expect("no lane fails");
        assert_eq!(slots.borrow(4), 4, "slots were not returned");
        slots.give_back(4);

        let failed = race(&items, &slots, |lane: &[usize], _| {
            if lane.contains(&2) {
                bail!("this lane fails")
            } else {
                Ok(Vec::<u8>::new())
            }
        });
        assert!(failed.is_err());
        assert_eq!(slots.borrow(4), 4, "a failing lane kept its slots");
    }

    /// I12: the same bytes must field the same entrants every time, or I8 goes
    /// with it the moment a thread count changes.
    #[test]
    fn the_verdict_is_a_function_of_the_bytes() {
        let data = wordy(5 * 1024 * 1024);
        let once = qualify(field(), &data, 19, &Slots::new(1));
        assert_eq!(once, qualify(field(), &data, 19, &Slots::new(1)));
        assert!(!once.is_empty());
    }
}
