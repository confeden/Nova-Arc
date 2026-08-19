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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// Reversible transform to apply before compressing (0 = none).
    filter: u8,
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

    std::thread::scope(|scope| -> Result<PackOutput> {
        for _ in 0..workers {
            let job_rx: Receiver<Job> = job_rx.clone();
            let done_tx: Sender<Result<Done>> = done_tx.clone();
            scope.spawn(move || worker_loop(job_rx, done_tx, level));
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
            next_seq: 0,
        };
        let produced = produce(&mut submitter);
        // Closing the job channel drains the workers, which closes the done
        // channel, which lets the writer finish.
        submitter.tx = None;
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
        self.submit_filtered(data, hash, vec![(codec, 0)], 0)
    }

    /// As [`Self::submit`], but with a reversible filter (see
    /// `crate::filters`) and a list of codecs to try.
    pub fn submit_filtered(
        &mut self,
        data: Vec<u8>,
        hash: [u8; 16],
        candidates: Vec<(Codec, u8)>,
        filter: u8,
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
            filter,
            charge,
        };
        match self.tx.as_ref().expect("submitter closed").send(job) {
            Ok(()) => {
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

fn worker_loop(rx: Receiver<Job>, tx: Sender<Result<Done>>, level: i32) {
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
        let out = compress_job(comp.as_mut(), job, level);
        if tx.send(out).is_err() {
            break; // writer is gone
        }
    }
}

fn compress_job(
    mut comp: Option<&mut zstd::bulk::Compressor<'_>>,
    job: Job,
    level: i32,
) -> Result<Done> {
    let Job {
        seq,
        mut data,
        hash,
        candidates,
        filter,
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
    let applied = match f.apply(&mut data) {
        Ok(a) => a,
        Err(_) => {
            let data = original.take().unwrap_or(data);
            return plain(seq, data, hash, charge, unpacked, candidates, comp, level);
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
        return plain(seq, data, hash, charge, unpacked, candidates, comp, level);
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
            return plain(seq, data, hash, charge, unpacked, candidates, comp, level);
        }
    }

    // Try every candidate and keep the smallest result. With one candidate
    // this is just "compress"; with several it is the max tier's tournament,
    // which trades encode time for ratio because no static rule picks the
    // right codec for arbitrary data.
    let mut best: Option<(Codec, u8, Vec<u8>)> = None;
    for (codec, param) in candidates {
        let packed = match codec {
            Codec::Store => break,
            // The per-worker zstd context is reused across chunks; allocating
            // one per chunk would dominate the cost of small chunks.
            Codec::Zstd => match comp.as_deref_mut() {
                Some(c) => c.compress(&data)?,
                None => codec::compress(Codec::Zstd, level, param, &data)?,
            },
            other => codec::compress(other, level, param, &data)?,
        };
        if best.as_ref().is_none_or(|(_, _, b)| packed.len() < b.len()) {
            best = Some((codec, param, packed));
        }
    }

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
    comp: Option<&mut zstd::bulk::Compressor<'_>>,
    level: i32,
) -> Result<Done> {
    compress_job(
        comp,
        Job {
            seq,
            data,
            hash,
            candidates,
            filter: 0,
            charge,
        },
        level,
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
