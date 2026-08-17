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
use crate::manifest::ChunkRec;

/// Memory the process needs before any packing work: manifest, buffers,
/// runtime. Reserved off the top of the budget.
const BASE_BYTES: u64 = 32 * 1024 * 1024;

/// Chunk buffers a worker holds while compressing (input + output).
const WORKER_CHUNK_BYTES: u64 = 2 * crate::archive::MAX_CHUNK as u64;

/// PPMd7's suballocator pool, allocated by the decoder as well as the
/// encoder. It dominates per-worker memory whenever an archive contains PPMd
/// chunks (see `codec::PPMD7_MEM_MAX`).
const PPMD_POOL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct PackOptions {
    pub tier: Tier,
    /// 0 = all logical cores.
    pub threads: usize,
    /// 0 = auto (see narc_platform::memory_budget).
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
        let budget = narc_platform::memory_budget(if self.memory_budget == 0 {
            None
        } else {
            Some(self.memory_budget)
        });
        let cores = if self.threads == 0 {
            narc_platform::logical_cores()
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
    pub fn extract_workers(&self, slow_codecs: bool) -> usize {
        let cores = if self.threads == 0 {
            narc_platform::logical_cores()
        } else {
            self.threads
        };
        if !slow_codecs && self.threads == 0 {
            return 1;
        }
        let budget = narc_platform::memory_budget(if self.memory_budget == 0 {
            None
        } else {
            Some(self.memory_budget)
        });
        let per_worker = if slow_codecs {
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
    unpacked: u64,
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
        let Some(m) = narc_platform::memory_status() else {
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

/// Runs the worker/writer threads for the duration of `produce`.
///
/// `produce` is called on the current thread with a submitter; every
/// `submit` returns the index the chunk will have in `PackOutput::chunks`
/// (relative to this run), available immediately even though the chunk has
/// not been compressed or written yet.
pub fn pack_with<F>(file: &mut File, opts: &PackOptions, produce: F) -> Result<PackOutput>
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
            move || writer_loop(file, done_rx, budget)
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
    crate::filters::Filter::from_id(filter)?.apply(&mut data);

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

    // Never let "compression" grow a chunk, and drop the filter with it: a
    // stored chunk is the original bytes, so no transform must be recorded.
    let (codec, param, filter, payload) = match best {
        Some((c, p, packed)) if packed.len() < data.len() => (c, p, filter, packed),
        _ => {
            crate::filters::Filter::from_id(filter)?.unapply(&mut data);
            (Codec::Store, 0, 0, data)
        }
    };
    Ok(Done {
        seq,
        payload,
        codec,
        param,
        filter,
        unpacked,
        hash,
        charge,
    })
}

fn writer_loop(file: &mut File, rx: Receiver<Result<Done>>, budget: &Budget) -> Result<PackOutput> {
    let mut pending: BTreeMap<u64, Done> = BTreeMap::new();
    let mut next = 0u64;
    let mut chunks = Vec::new();
    let mut bytes_stored = 0u64;
    let mut offset = file.seek(SeekFrom::End(0))?;

    for item in rx.iter() {
        let done = match item {
            Ok(d) => d,
            Err(e) => {
                budget.abort();
                return Err(e);
            }
        };
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
                hash: d.hash,
            });
            offset += d.payload.len() as u64;
            bytes_stored += d.payload.len() as u64;
            budget.release(d.charge);
            next += 1;
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
