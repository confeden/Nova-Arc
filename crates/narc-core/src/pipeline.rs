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

    /// Workers for extraction. Default is 1 **on purpose**: with zstd,
    /// unpacking is bound by file creation and disk I/O, not by the CPU
    /// (measured: 5751 small files take 1.0 s on one thread and 2.5 s on
    /// eight, because parallel writers fight over NTFS directory metadata and
    /// turn sequential reads into seeks). An explicit `-j` is still honoured,
    /// and the default should become "all cores" once slow-decoding codecs
    /// (LZMA/PPMd) land and decompression becomes the bottleneck.
    ///
    /// Memory per worker is one packed plus one unpacked chunk, so extraction
    /// stays in the low tens of MB regardless of archive size.
    pub fn extract_workers(&self) -> usize {
        if self.threads == 0 {
            return 1;
        }
        let budget = narc_platform::memory_budget(if self.memory_budget == 0 {
            None
        } else {
            Some(self.memory_budget)
        });
        let by_mem = (budget.saturating_sub(BASE_BYTES) / WORKER_CHUNK_BYTES).max(1) as usize;
        self.threads.min(by_mem).max(1)
    }
}

/// A chunk on its way to being stored.
struct Job {
    seq: u64,
    data: Vec<u8>,
    hash: [u8; 16],
    /// Codec chosen by the analysis phase for the owning file.
    codec: Codec,
    /// Bytes charged to the memory budget for this chunk.
    charge: u64,
}

struct Done {
    seq: u64,
    payload: Vec<u8>,
    codec: Codec,
    unpacked: u64,
    hash: [u8; 16],
    charge: u64,
}

/// Counting semaphore over bytes in flight, with an abort path so a failing
/// writer can never leave the reader blocked forever.
struct Budget {
    limit: u64,
    state: Mutex<BudgetState>,
    cv: Condvar,
}

struct BudgetState {
    used: u64,
    aborted: bool,
}

impl Budget {
    fn new(limit: u64) -> Self {
        Budget {
            limit: limit.max(1),
            state: Mutex::new(BudgetState {
                used: 0,
                aborted: false,
            }),
            cv: Condvar::new(),
        }
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
            if st.used == 0 || st.used + n <= self.limit {
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

    std::thread::scope(|scope| -> Result<PackOutput> {
        for _ in 0..workers {
            let job_rx: Receiver<Job> = job_rx.clone();
            let done_tx: Sender<Result<Done>> = done_tx.clone();
            scope.spawn(move || worker_loop(job_rx, done_tx, level));
        }
        drop(job_rx);
        drop(done_tx);

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
            codec,
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
        let out = compress_job(comp.as_mut(), job);
        if tx.send(out).is_err() {
            break; // writer is gone
        }
    }
}

fn compress_job(comp: Option<&mut zstd::bulk::Compressor<'_>>, job: Job) -> Result<Done> {
    let Job {
        seq,
        data,
        hash,
        codec,
        charge,
    } = job;
    let unpacked = data.len() as u64;
    let (codec, payload) = match codec {
        Codec::Store => (Codec::Store, data),
        Codec::Zstd => {
            let packed = match comp {
                Some(c) => c.compress(&data)?,
                None => codec::compress(Codec::Zstd, 3, &data)?,
            };
            // Never let "compression" grow a chunk.
            if packed.len() >= data.len() {
                (Codec::Store, data)
            } else {
                (Codec::Zstd, packed)
            }
        }
    };
    Ok(Done {
        seq,
        payload,
        codec,
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
