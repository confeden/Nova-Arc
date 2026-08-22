//! Archive operations. The file layout after N updates is:
//!
//! ```text
//! [header][manifest g1][footer g1]                      <- created empty
//!         [chunks...][manifest g2][footer g2]           <- first add
//!         [chunks...][manifest g3][footer g3]           <- update
//! ```
//!
//! Every update appends chunks plus a fresh manifest+footer; earlier
//! manifests/footers and unreferenced chunks become dead space that `compact`
//! reclaims. Readers locate the newest footer at EOF (with a backward scan as
//! crash recovery), so a torn update can never corrupt committed data.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};

use crate::analyze::Tier;
use crate::codec::{self, Codec};
use crate::footer::{self, Footer, FOOTER_LEN, HEADER_LEN};
use crate::manifest::{ChunkRec, Extent, FileEntry, Manifest};
use crate::pack;
use crate::paths;
use crate::pipeline::{self, PackOptions};

/// Largest compression unit any tier produces (see `Tier::cdc` and
/// `Tier::solid_block`). Used for the plausibility checks that keep a hostile
/// manifest from driving allocations, and for the memory model.
pub const MAX_CHUNK: u32 = 32 * 1024 * 1024;

/// How much of a file the analyzer gets to look at.
///
/// 1 MiB, not 64 KiB, because one of the questions it answers needs range: a
/// zip written at deflate level 1 shows **+0.02%** on a 64 KiB zstd sample —
/// indistinguishable from random noise — and **−25.6%** on a 1 MiB one. The
/// bytes are free: `add_file` reads them anyway and chains them in front of the
/// file, so nothing is read twice. Tests that only need a class keep their own
/// smaller caps (see `analyze`).
pub(crate) const HEAD_SAMPLE: usize = 1024 * 1024;

/// Upper bound on a stored chunk. A max-tier unit is capped at twice
/// `MAX_CHUNK`, and the check below is a strict `>`, so units of exactly 64 MiB
/// are admitted. This may be raised, never lowered — and raising it is a FORMAT
/// change, because an older reader refuses what a newer writer would emit.
///
/// `Packer` derives its solo-unit cap from this, and `Packer::flush` asserts
/// against it: a unit above this bound is an archive nova writes, lists, and
/// then cannot extract.
pub(crate) const MAX_STORED_CHUNK: u64 = MAX_CHUNK as u64 * 2;

/// Upper bound on the buffer a codec is asked to produce. A length-changing
/// filter makes this bigger than the unit — undoing deflate turns a zip into
/// several times its size — and it is what bounds extraction memory, so it is
/// checked before anything is allocated. The attacker controls the manifest.
pub(crate) const MAX_CODED_CHUNK: u64 = MAX_STORED_CHUNK * 4;

/// How many footer candidates to try before declaring an archive corrupt.
/// Each retry means "this footer's manifest did not verify"; a handful covers
/// torn writes and embedded footer images, while bounding scan cost on
/// hostile input.
const MAX_FOOTER_CANDIDATES: usize = 64;

/// What to do when an extracted file already exists on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overwrite {
    /// Refuse and abort (default: never destroy data silently).
    #[default]
    Fail,
    /// Leave the existing file alone, count it as skipped.
    Skip,
    /// Truncate and rewrite.
    Force,
}

pub struct Archive {
    file: File,
    pub path: PathBuf,
    pub manifest: Manifest,
    last_manifest_packed: u64,
    writable: bool,
    /// Set when opening had to fall back over a COMMITTED generation. `None` on
    /// every healthy archive; see [`Damage`].
    pub damage: Option<Damage>,
}

/// What [`Archive::open`] had to skip past to find a manifest it could read.
///
/// The distinction this records is the difference between dropping garbage and
/// destroying an archive, so it is a value and not a log line.
///
/// A footer's self-hash covers its own absolute offset, so `find_footer_before`
/// only ever returns a footer that was genuinely committed at that position.
/// The commit order is manifest → fsync → footer → fsync, so a crash leaves the
/// tail with NO valid footer in it — which is why dropping that tail is safe and
/// why `crash_recovery_ignores_trailing_garbage` passes. A footer that verified
/// its own hash but whose MANIFEST will not decode is therefore not a crash: it
/// is damage to bytes that were already committed, and everything behind it
/// belongs to a generation this build cannot read.
///
/// Getting that backwards cost the archive. Measured on 12,583,583 B: one
/// flipped bit inside the newest manifest made `list` print "0 file(s)" and
/// exit 0, `info` print "Reclaimable: 12.0 MiB (run 'nova compact')", and then
/// `compact` reduce the file to 133 B and `add` to 429 B — both reporting
/// success. The tool recommended the command that destroyed the data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Damage {
    /// Generation of the newest committed footer whose manifest would not read.
    pub lost_generation: u64,
    /// Generation actually opened, the newest one that still reads.
    pub opened_generation: u64,
    /// Committed bytes sitting past the opened footer. These are real data that
    /// the opened manifest knows nothing about, so every size the opened
    /// manifest reports — including "reclaimable" — is wrong about them.
    pub stranded_bytes: u64,
}

impl std::fmt::Display for Damage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "generation {} is damaged: its manifest does not decode, so only generation {} \
             could be opened and {} byte(s) of committed data are not accounted for by it",
            self.lost_generation, self.opened_generation, self.stranded_bytes
        )
    }
}

/// Which part of an operation is running. A byte count alone cannot explain a
/// wait: the tail of a pack — draining the compressors, then writing and
/// fsyncing the manifest — moves no source bytes at all, and on a source tree
/// at the max tier it is most of the wall clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Walking the inputs; totals are not known yet.
    #[default]
    Scan,
    /// Reading and compressing, or extracting.
    Work,
    /// Inputs are consumed; the compressors are finishing what is in flight.
    Drain,
    /// Writing the manifest and the footer, with the fsync barrier between.
    Commit,
    /// Finished. The only reading where `bytes_done == bytes_total`.
    Done,
}

/// Progress of a long operation, for a UI that must stay alive while the max
/// tier grinds through a large archive.
///
/// `bytes_done` counts source bytes whose work is FINISHED — for a pack, bytes
/// whose unit has been written to the archive or deduplicated away; for an
/// extraction, bytes written or deliberately skipped. It never runs ahead of
/// the work, never goes backwards, and equals `bytes_total` exactly once, in
/// the single reading with [`Phase::Done`].
///
/// It deliberately is NOT the same thing as `bytes_read`. Measured on a 113 MiB
/// source tree at the max tier, a reader-side count reached 100% after 1.85 s of
/// a 38.59 s operation — the reader can be a full in-flight budget ahead of the
/// archive. `bytes_read` is kept because the gap between the two is the useful
/// part: it is the work currently inside the compressors, which is what tells a
/// UI that a long silence is progress rather than a hang.
#[derive(Clone, Copy, Debug, Default)]
pub struct Progress {
    pub phase: Phase,
    pub files_done: u64,
    pub files_total: u64,
    /// Source bytes whose work is finished. See the type docs.
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Source bytes taken off disk so far; runs ahead of `bytes_done`.
    pub bytes_read: u64,
    /// Compressed bytes appended to the archive so far.
    pub bytes_stored: u64,
    /// Compression units finished, and handed to the pipeline so far. At the
    /// max tier a 113 MiB tree is about six units compressed in parallel, so
    /// this is what explains a long wait that no byte count can.
    pub units_done: u64,
    pub units_total: u64,
}

/// Callback invoked as work completes. Must be cheap and must not block: it is
/// called from the thread driving the operation AND, during packing, from the
/// pipeline's writer thread, which is the serialization point of the whole run.
pub type ProgressFn<'a> = &'a (dyn Fn(Progress) + Send + Sync);

/// Owns the progress reading and the decision of when to report it.
///
/// Two jobs, both of which have to live in one place. The reading is assembled
/// from counters owned by different threads — the reader's files and bytes, the
/// writer's committed units — so it needs one lock, or a UI receives halves of
/// two different moments. And the throttling has to be here too, because only
/// this side knows the totals: the GUI's own throttle was built with a total of
/// `1`, which made its step one byte and let all 5751 events through.
struct Reporter<'a> {
    cb: Option<ProgressFn<'a>>,
    state: Mutex<Snap>,
}

#[derive(Default)]
struct Snap {
    p: Progress,
    /// Source bytes behind committed units, and bytes deduplicated away. Kept
    /// apart because they arrive from different threads.
    stored_unpacked: u64,
    deduped: u64,
    /// Watermarks: the last reported values, so a step can be measured.
    sent_done: u64,
    sent_files: u64,
    /// Highest `files_done` ever assembled, so the reading cannot regress.
    peak_files: u64,
    /// The scan's own watermark. It cannot share `sent_files`: `emit` rewrites
    /// that one from `files_done`, which is zero during a scan, so the scan's
    /// step reset to zero on every report and every file got an event.
    sent_scan: u64,
    step_bytes: u64,
    step_files: u64,
}

/// Roughly this many readings per operation. 500 is fine for a 200 px bar and
/// leaves the webview time to repaint; the point of a step at all is that one
/// event per file means 5751 IPC round trips on a source tree.
const REPORTS_PER_OP: u64 = 500;

/// While walking the inputs there is no total to divide, so the scan reports
/// every this many files just to prove it is moving.
const SCAN_STEP_FILES: u64 = 500;

impl<'a> Reporter<'a> {
    fn new(cb: Option<ProgressFn<'a>>) -> Self {
        Reporter {
            cb,
            // The steps must start large, not at zero: before `totals` is known
            // a zero step means "any movement is enough", and the scan of a
            // 5751-file tree emitted one event per file.
            state: Mutex::new(Snap {
                step_bytes: u64::MAX,
                step_files: SCAN_STEP_FILES,
                ..Snap::default()
            }),
        }
    }

    /// True when nothing is listening, so every call site can return early and
    /// the CLI pays nothing at all.
    fn idle(&self) -> bool {
        self.cb.is_none()
    }

    fn totals(&self, files_total: u64, bytes_total: u64) {
        if self.idle() {
            return;
        }
        let mut s = self.state.lock().expect("progress mutex poisoned");
        s.p.files_total = files_total;
        s.p.bytes_total = bytes_total;
        s.step_bytes = (bytes_total / REPORTS_PER_OP).max(1);
        s.step_files = (files_total / REPORTS_PER_OP).max(1);
        // The scan counted discoveries against this watermark; the work phase
        // measures finished files against it instead.
        s.sent_files = 0;
        self.emit(s, true);
    }

    fn phase(&self, phase: Phase) {
        if self.idle() {
            return;
        }
        let mut s = self.state.lock().expect("progress mutex poisoned");
        s.p.phase = phase;
        self.emit(s, true);
    }

    /// Files seen while walking the inputs.
    ///
    /// This grows `files_total`, not `files_done`: during a scan the total is
    /// what is being discovered and nothing is finished yet. Reporting it as
    /// `files_done` made the reading jump from 5751 back to 1 when packing
    /// started, which is the one thing a progress reading may never do.
    fn scanned(&self, files: u64) {
        if self.idle() {
            return;
        }
        let mut s = self.state.lock().expect("progress mutex poisoned");
        s.p.files_total = files;
        let force = files >= s.sent_scan.saturating_add(SCAN_STEP_FILES);
        if force {
            s.sent_scan = files;
        }
        self.emit(s, force);
    }

    /// Reader side of a pack.
    fn read(&self, files_done: u64, bytes_read: u64, deduped: u64, submitted: u64) {
        if self.idle() {
            return;
        }
        let mut s = self.state.lock().expect("progress mutex poisoned");
        s.p.files_done = files_done;
        s.p.bytes_read = bytes_read;
        s.p.units_total = submitted;
        s.deduped = deduped;
        self.emit(s, false);
    }

    /// Writer side of a pack: the honest counter.
    fn stored(&self, t: pipeline::StoredTick) {
        if self.idle() {
            return;
        }
        let mut s = self.state.lock().expect("progress mutex poisoned");
        s.stored_unpacked = t.unpacked;
        s.p.bytes_stored = t.packed;
        s.p.units_done = t.units;
        s.p.units_total = s.p.units_total.max(t.units);
        // Always report a finished unit: at the max tier there are only a few,
        // and each one is the only news for tens of seconds.
        self.emit(s, true);
    }

    /// Extraction, where finished work is per file and includes files the
    /// overwrite policy deliberately skipped.
    fn extracted(&self, files: u64, bytes: u64) {
        if self.idle() {
            return;
        }
        let mut s = self.state.lock().expect("progress mutex poisoned");
        s.stored_unpacked += bytes;
        s.p.files_done += files;
        self.emit(s, false);
    }

    /// The one reading that is allowed to say 100%.
    fn finish(&self) {
        if self.idle() {
            return;
        }
        let mut s = self.state.lock().expect("progress mutex poisoned");
        s.p.phase = Phase::Done;
        s.p.files_done = s.p.files_total;
        s.p.bytes_done = s.p.bytes_total;
        s.p.bytes_read = s.p.bytes_read.max(s.p.bytes_total);
        let p = s.p;
        drop(s);
        if let Some(cb) = self.cb {
            cb(p);
        }
    }

    /// Assemble the reading, enforce its guarantees, and hand it over if it has
    /// moved far enough. Takes the guard so the lock is released before the
    /// callback runs — a UI callback must never be able to stall the writer
    /// while holding this lock.
    fn emit(&self, mut s: std::sync::MutexGuard<'_, Snap>, force: bool) {
        // A file can grow between the stat and the read, so the denominator has
        // to be able to follow, or the reading exceeds 100%.
        s.p.bytes_total = s.p.bytes_total.max(s.p.bytes_read);
        let done = s
            .stored_unpacked
            .saturating_add(s.deduped)
            .min(s.p.bytes_total);
        // Held one byte short on purpose: it makes "100%" mean Done and nothing
        // else, structurally, instead of asking every consumer to check Phase.
        let ceiling = s.p.bytes_total.saturating_sub(1);
        s.p.bytes_done = done.min(ceiling).max(s.p.bytes_done);
        // Both counters are clamped monotone here rather than trusted to be:
        // they are fed by different threads, and extraction's workers each
        // report their own completions.
        s.p.files_done = s.p.files_done.max(s.peak_files);
        s.peak_files = s.p.files_done;
        let moved = s.p.bytes_done >= s.sent_done.saturating_add(s.step_bytes)
            || s.p.files_done >= s.sent_files.saturating_add(s.step_files);
        if !force && !moved {
            return;
        }
        s.sent_done = s.p.bytes_done;
        s.sent_files = s.p.files_done;
        let p = s.p;
        drop(s);
        if let Some(cb) = self.cb {
            cb(p);
        }
    }
}

#[derive(Debug, Default)]
pub struct AddStats {
    pub files: usize,
    /// Directory entries recorded. Counted apart from files because a caller
    /// that says "5751 files" should not start saying 7300 the day folders
    /// began to be stored.
    pub dirs: usize,
    pub bytes_in: u64,
    pub bytes_stored: u64,
    pub bytes_deduped: u64,
    pub symlinks_skipped: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ExtractStats {
    pub files: usize,
    /// Directories created, including the empty ones that used to disappear.
    pub dirs: usize,
    /// Files that could not be recovered, with the reason, sorted by path.
    ///
    /// Extraction does NOT stop at the first one. A damaged archive is exactly
    /// when the rest of the files matter most, and an archiver that refuses the
    /// whole tree because one block rotted is worse than useless — it is the
    /// difference between losing three files and losing nine thousand.
    pub failed: Vec<(String, String)>,
    pub bytes: u64,
    pub skipped_existing: usize,
    pub warnings: Vec<String>,
}

/// What `Archive::test` found. A pass is `bad.is_empty()`, and nothing else.
#[derive(Debug, Default)]
pub struct TestStats {
    /// Live chunks, meaning the ones some file still references.
    pub chunks: usize,
    pub chunks_ok: usize,
    /// Original bytes behind the chunks that verified.
    pub bytes_ok: u64,
    /// `(chunk index, what went wrong)`, in index order so two runs read the
    /// same however the workers were scheduled.
    pub bad: Vec<(u32, String)>,
    /// Paths that touch at least one bad chunk, in manifest order. These are
    /// the files that cannot be recovered; everything else in the archive can.
    pub damaged: Vec<String>,
}

#[derive(Debug)]
pub struct InfoStats {
    pub generation: u64,
    /// Files only. Directories are entries too but they are not files, and a
    /// count that quietly grew the day folders started being stored would be
    /// the kind of number nobody can reconcile with anything.
    pub files: usize,
    pub dirs: usize,
    pub chunks: usize,
    pub file_len: u64,
    pub live_bytes: u64,
    pub reclaimable: u64,
    /// Unit geometry as realized, not as configured. Unit size drives the
    /// ratio directly (a 4 MiB unit costs ~50% more than one solid stream, a
    /// 32 MiB unit only ~5%), so these are worth showing rather than guessing.
    pub units: usize,
    pub unit_min: u64,
    pub unit_median: u64,
    pub unit_max: u64,
    /// Bytes stored per codec id, so it is visible which codec actually won.
    pub by_codec: Vec<(u8, u64)>,
}

/// One compression unit as it was actually stored.
///
/// The two numbers that explain an archive's ratio are how big its units came
/// out and which codec won each one, and neither is visible from the summary in
/// [`InfoStats`]: an average hides a bimodal distribution, and "stored by
/// lzma2 6 MiB, ppmd7 2 MiB" hides *which* data went where. This is the
/// per-unit form, for measuring rather than reporting.
#[derive(Clone, Debug)]
pub struct UnitInfo {
    pub idx: u32,
    pub unpacked: u64,
    pub packed: u64,
    pub codec: u8,
    /// PPMd7's model order, 0 for other codecs.
    pub param: u8,
    pub filter: u8,
    /// How many file entries have bytes in this unit.
    pub files: usize,
    /// The most common extension among them, and how many distinct ones there
    /// are — a unit holding one extension is the extension sort working.
    pub top_ext: String,
    pub distinct_exts: usize,
}

impl Archive {
    /// Create a new, empty archive. Fails if the file already exists.
    pub fn create(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("cannot create {}", path.display()))?;
        lock_exclusive(&file, path)?;
        let mut a = Archive {
            file,
            path: path.to_path_buf(),
            manifest: Manifest::default(),
            last_manifest_packed: 0,
            writable: true,
            damage: None,
        };
        a.file.write_all(&footer::header_bytes())?;
        a.commit()?;
        Ok(a)
    }

    pub fn open_ro(path: &Path) -> Result<Self> {
        Self::open(path, false)
    }

    pub fn open_rw(path: &Path) -> Result<Self> {
        Self::open(path, true)
    }

    fn open(path: &Path, writable: bool) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        // A second writer would append at a stale EOF and truncate the first
        // writer's uncommitted bytes, so writers are mutually exclusive.
        if writable {
            lock_exclusive(&file, path)?;
        }
        footer::check_header(&mut file)?;

        // A footer's self-check only proves the footer survived; the manifest
        // it points at may be torn (crash between the two writes), and a
        // stored chunk may even contain a valid-looking footer image. So walk
        // candidates from EOF backwards until one yields a good manifest.
        let file_len = file.metadata()?.len();
        let mut limit = file_len;
        let mut last_err = None;
        let mut found = None;
        // The newest footer that verified its own hash and whose manifest then
        // refused to decode. That footer is a COMMITTED record, so this is the
        // one thing that separates damage from a crash.
        let mut skipped_commit: Option<u64> = None;
        for _ in 0..MAX_FOOTER_CANDIDATES {
            let (ftr, off) = match footer::find_footer_before(&mut file, limit) {
                Ok(v) => v,
                Err(e) => {
                    last_err = Some(e);
                    break;
                }
            };
            match read_manifest(&mut file, &ftr, off) {
                Ok(m) => {
                    found = Some((m, ftr, off));
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    skipped_commit.get_or_insert(ftr.generation);
                    if off <= HEADER_LEN {
                        break;
                    }
                    limit = off;
                }
            }
        }
        let (manifest, ftr, ftr_off) = found.ok_or_else(|| {
            last_err.unwrap_or_else(|| anyhow::anyhow!("archive is corrupt: no usable footer"))
        })?;

        let committed_end = ftr_off + FOOTER_LEN;
        let damage = skipped_commit.map(|lost| Damage {
            lost_generation: lost,
            opened_generation: manifest.generation,
            stranded_bytes: file_len.saturating_sub(committed_end),
        });

        // A DAMAGED ARCHIVE IS NEVER OPENED FOR WRITING. Every write path ends
        // in a commit whose footer describes only what the opened manifest
        // knows, so `add` would strand the newer generation's bytes and
        // `compact` would rewrite the file without them — and both would report
        // success. Refusing here is the whole fix: extraction still works, so
        // whatever the older generation can reach is still recoverable.
        if writable {
            if let Some(d) = damage {
                bail!(
                    "{}\n  Writing to it would destroy those bytes, so this is refused. \
                     Recover what is readable with `nova extract`, then build a new archive.",
                    d
                );
            }
        }

        // Drop uncommitted trailing garbage (e.g. after a crash mid-update) so
        // new appends land right after the committed footer. Only reachable
        // when `damage` is None, i.e. nothing committed lives in that tail.
        if writable && file_len > committed_end {
            file.set_len(committed_end)?;
        }
        Ok(Archive {
            file,
            path: path.to_path_buf(),
            manifest,
            last_manifest_packed: ftr.manifest_packed,
            writable,
            damage,
        })
    }

    /// Append the current manifest and a footer, then fsync. This is the
    /// commit point: an update is durable iff its footer hit the disk.
    fn commit(&mut self) -> Result<()> {
        self.manifest.generation += 1;
        self.last_manifest_packed = write_tail(&mut self.file, &self.manifest)?;
        Ok(())
    }

    /// Add files or directory trees. Entries with the same archive path are
    /// replaced. Unchanged content costs nothing thanks to chunk dedup.
    ///
    /// Chunking and hashing happen here; compression runs on a worker pool
    /// and writing on a dedicated thread, all inside a fixed memory budget
    /// (see [`crate::pipeline`]).
    pub fn add_paths(&mut self, inputs: &[PathBuf], opts: &PackOptions) -> Result<AddStats> {
        self.add_paths_with(inputs, opts, None)
    }

    /// As [`Self::add_paths`], reporting progress as files are consumed.
    pub fn add_paths_with(
        &mut self,
        inputs: &[PathBuf],
        opts: &PackOptions,
        progress: Option<ProgressFn>,
    ) -> Result<AddStats> {
        if !self.writable {
            bail!("archive opened read-only");
        }
        let mut stats = AddStats::default();
        // Unit boundaries must stay compatible with what is already in the
        // archive, or nothing deduplicates; only the compression method
        // follows the tier the user asked for now.
        let geom = *self
            .manifest
            .geometry
            .get_or_insert_with(|| opts.tier.geometry());
        if geom != opts.tier.geometry() {
            stats.warnings.push(format!(
                "archive was created with {} MiB units; keeping that geometry \
so unchanged data still deduplicates",
                geom.unit / (1024 * 1024)
            ));
        }

        let reporter = Reporter::new(progress);
        reporter.phase(Phase::Scan);

        // Collect the work list first and sort it by extension: neighbouring
        // files of the same type share a unit, which is where the compressor
        // finds what they have in common.
        //
        // The size is taken here, from metadata the walk has already read, and
        // carried along. A separate `fs::metadata` pass over the work list used
        // to build the total, and its `filter_map` dropped stat failures
        // silently — those files were still read, so the reading could exceed
        // 100% with nothing to explain it.
        let walk = paths::walk_inputs(inputs, |n| reporter.scanned(n))?;
        stats.symlinks_skipped += walk.symlinks_skipped;
        // Directories are entries too, carrying no bytes. Without them an empty
        // folder simply vanishes and a folder's own attributes and timestamp go
        // with it — a restored tree that LOOKS right and is not.
        let dirs: Vec<FileEntry> = walk
            .dirs
            .iter()
            .filter_map(|d| {
                let meta = std::fs::metadata(&d.disk).ok()?;
                Some(FileEntry {
                    path: d.rel.clone(),
                    size: 0,
                    mtime: pack::mtime_of(&meta),
                    mtime_nanos: pack::mtime_nanos_of(&meta),
                    attrs: nova_platform::file_attributes(&meta),
                    dir: true,
                    extents: Vec::new(),
                })
            })
            .collect();
        stats.dirs += dirs.len();
        let mut work: Vec<(PathBuf, String, u64)> = walk
            .files
            .into_iter()
            .map(|f| (f.disk, f.rel, f.size))
            .collect();
        work.sort_by(|a, b| {
            group_key(&a.1)
                .cmp(&group_key(&b.1))
                .then_with(|| a.1.cmp(&b.1))
        });

        let base = u32::try_from(self.manifest.chunks.len())
            .context("archive unit count exceeds format limit")?;
        let mut packer = pack::Packer::new(
            opts.tier,
            geom,
            base,
            self.manifest
                .chunks
                .iter()
                .enumerate()
                .map(|(i, c)| (c.hash, i as u32))
                .collect(),
            self.manifest
                .files
                .iter()
                .map(|f| (paths::collision_key(&f.path), f.path.clone()))
                .collect(),
        );

        reporter.totals(work.len() as u64, work.iter().map(|(_, _, len)| len).sum());
        reporter.phase(Phase::Work);
        // The writer reports what has actually landed in the archive; the
        // reader only reports how far ahead it has read. Both feed one snapshot.
        let sink = |t: pipeline::StoredTick| reporter.stored(t);
        let out = pipeline::pack_with(
            &mut self.file,
            opts,
            (!reporter.idle()).then_some(&sink as pipeline::StoredFn),
            |sub| {
                for (disk, rel, _) in &work {
                    packer.add_file(sub, disk, rel.clone(), &mut stats)?;
                    reporter.read(
                        stats.files as u64,
                        stats.bytes_in,
                        stats.bytes_deduped,
                        sub.submitted(),
                    );
                }
                packer.flush(sub, &mut stats, pack::Cut::End)?;
                // Everything is submitted; what remains is compressors emptying
                // out. Without this the tail of a max-tier pack is silent for
                // most of the run.
                reporter.phase(Phase::Drain);
                Ok(())
            },
        )?;
        reporter.phase(Phase::Commit);

        // The writer emitted units in submission order, so the indices the
        // packer predicted (base + submission index) are now correct.
        stats.bytes_stored = out.bytes_stored;
        self.manifest.chunks.extend(out.chunks);
        let mut by_path: HashMap<String, usize> = self
            .manifest
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), i))
            .collect();
        // Directory entries first, so a file that shares a path with one (which
        // only a hostile archive can arrange) is the version that survives.
        for entry in dirs.into_iter().chain(packer.into_files()) {
            match by_path.get(&entry.path) {
                Some(&i) => self.manifest.files[i] = entry,
                None => {
                    by_path.insert(entry.path.clone(), self.manifest.files.len());
                    self.manifest.files.push(entry);
                }
            }
        }
        self.commit()?;
        reporter.finish();
        Ok(stats)
    }

    /// Extract everything, or only entries matching the given archive paths
    /// (exact file path or directory prefix). Selectors are normalized, and
    /// a selector matching nothing is an error rather than a silent no-op.
    ///
    /// Files are independent, so extraction runs on a worker pool; each
    /// worker opens its own read handle and holds at most one chunk, keeping
    /// extraction memory in the low tens of MB no matter how big the archive
    /// is. `opts` only supplies the thread and memory limits here.
    pub fn extract(
        &self,
        dest: &Path,
        select: Option<&[String]>,
        overwrite: Overwrite,
    ) -> Result<ExtractStats> {
        self.extract_with(dest, select, overwrite, &PackOptions::new(Tier::Normal))
    }

    pub fn extract_with(
        &self,
        dest: &Path,
        select: Option<&[String]>,
        overwrite: Overwrite,
        opts: &PackOptions,
    ) -> Result<ExtractStats> {
        self.extract_reporting(dest, select, overwrite, opts, None)
    }

    /// As [`Self::extract_with`], reporting progress as files are written.
    pub fn extract_reporting(
        &self,
        dest: &Path,
        select: Option<&[String]>,
        overwrite: Overwrite,
        opts: &PackOptions,
        progress: Option<ProgressFn>,
    ) -> Result<ExtractStats> {
        let mut stats = ExtractStats::default();
        let reporter = Reporter::new(progress);
        reporter.phase(Phase::Scan);
        let selectors: Option<Vec<String>> = select.map(|s| {
            s.iter()
                .map(|x| paths::normalize_selector(x))
                .collect::<Vec<_>>()
        });
        let mut used = vec![false; selectors.as_ref().map_or(0, |s| s.len())];
        let file_len = self.file.metadata()?.len();

        // Selection, path safety and collision checks run once, in order, so
        // the warnings a user sees do not depend on thread scheduling.
        let mut seen: HashSet<String> = HashSet::new();
        let mut work: Vec<(&FileEntry, PathBuf)> = Vec::new();
        for entry in &self.manifest.files {
            if let Some(sel) = &selectors {
                let mut hit = false;
                for (i, s) in sel.iter().enumerate() {
                    if entry.path == *s || entry.path.starts_with(&format!("{s}/")) {
                        used[i] = true;
                        hit = true;
                    }
                }
                if !hit {
                    continue;
                }
            }
            // A hostile or damaged entry must not abort the whole extraction.
            let safe = match paths::sanitize(&entry.path) {
                Ok(p) => p,
                Err(e) => {
                    stats.warnings.push(format!("skipped: {e}"));
                    continue;
                }
            };
            if !seen.insert(paths::collision_key(&entry.path)) {
                stats.warnings.push(format!(
                    "skipped {:?}: another entry maps to the same file name",
                    entry.path
                ));
                continue;
            }
            work.push((entry, dest.join(&safe)));
        }

        if let Some(sel) = &selectors {
            let missing: Vec<&str> = sel
                .iter()
                .zip(&used)
                .filter(|(_, u)| !**u)
                .map(|(s, _)| s.as_str())
                .collect();
            if !missing.is_empty() {
                bail!("not found in archive: {}", missing.join(", "));
            }
        }

        // Directories are entries with no bytes. They come out of the work
        // list because the file path below writes files, and they are created
        // FIRST so an empty one exists even though nothing will ever be written
        // into it. Their timestamps go on at the very end, because writing a
        // file into a directory moves that directory's clock.
        let (dir_entries, files): (Vec<_>, Vec<_>) =
            work.into_iter().partition(|(e, _)| e.dir);
        let work = files;
        for (_, target) in &dir_entries {
            if let Err(e) = std::fs::create_dir_all(target) {
                stats
                    .warnings
                    .push(format!("cannot create {}: {e}", target.display()));
            }
        }
        stats.dirs = dir_entries.len();

        // Fail fast, before writing anything: a refused extraction should not
        // leave half a tree behind.
        if overwrite == Overwrite::Fail {
            if let Some((_, target)) = work.iter().find(|(_, t)| t.exists()) {
                bail!(
                    "{} already exists - use --force to overwrite or --skip-existing",
                    target.display()
                );
            }
        }

        // LZMA2, PPMd7 and bsc make extraction CPU-bound; zstd/store leave it
        // I/O-bound. Which one this archive is decides the thread count.
        // bsc is in the slow set because nova runs it WITHOUT libbsc's internal
        // threads, where it decodes at ~25 MB/s — not the ~110 MB/s the `bsc`
        // command line shows, which is that same work spread over blocks.
        let slow_codecs = self
            .manifest
            .chunks
            .iter()
            .any(|c| matches!(c.codec, 2..=4));
        // The inverse BWT's index is four bytes per byte of block, so a bsc
        // worker needs the large allowance even where LZMA2 would not.
        let hungry_codecs = self.manifest.chunks.iter().any(|c| c.codec == 4);
        let budget = opts.extract_workers(slow_codecs, hungry_codecs);
        let workers = budget.min(work.len().max(1));
        // Threads the budget allows but the file count cannot use. An archive of
        // one large file left seven of eight cores idle; those become decode
        // lanes INSIDE each file instead. Total concurrency is unchanged, so the
        // memory `extract_workers` sized still holds.
        let lanes_per_worker = (budget / workers.max(1)).max(1);
        reporter.totals(work.len() as u64, work.iter().map(|(e, _)| e.size).sum());
        reporter.phase(Phase::Work);
        let counters = Mutex::new((0usize, 0u64, 0usize)); // files, bytes, skipped
        // Only a failure that makes further work impossible — not being able to
        // open the archive at all. Everything else is per file.
        let failure: Mutex<Option<anyhow::Error>> = Mutex::new(None);
        let failed: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
        let stop = AtomicBool::new(false);
        let next = AtomicUsize::new(0);

        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    // A private handle per worker: cloned handles share a file
                    // position on Windows, which would corrupt concurrent reads.
                    let reader = match File::open(&self.path) {
                        Ok(f) => f,
                        Err(e) => {
                            *failure.lock().expect("mutex") = Some(anyhow::Error::new(e));
                            stop.store(true, Ordering::Relaxed);
                            return;
                        }
                    };
                    nova_platform::lower_io_priority(&reader);
                    // Each lane needs its OWN handle: a cloned one shares the
                    // file position on Windows, which would corrupt concurrent
                    // reads. Opened once per worker, not per window.
                    let mut lanes: Vec<(File, UnitCache)> = Vec::new();
                    if lanes_per_worker > 1 {
                        for _ in 0..lanes_per_worker {
                            match File::open(&self.path) {
                                Ok(f) => {
                                    nova_platform::lower_io_priority(&f);
                                    lanes.push((f, UnitCache::default()));
                                }
                                // Running short of handles is not fatal: fewer
                                // lanes simply means less parallelism.
                                Err(_) => break,
                            }
                        }
                    }
                    let mut dec = Decoder {
                        reader,
                        cache: UnitCache::default(),
                        lanes,
                    };
                    let mut local = (0usize, 0u64, 0usize);
                    loop {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some((entry, target)) = work.get(i) else {
                            break;
                        };
                        match extract_one(
                            &mut dec,
                            &self.manifest,
                            entry,
                            target,
                            overwrite,
                            file_len,
                        ) {
                            Ok(Some(bytes)) => {
                                local.0 += 1;
                                local.1 += bytes;
                                reporter.extracted(1, bytes);
                            }
                            // A file kept per the overwrite policy is decided
                            // work, and it is in the denominator. Not counting
                            // it left the GUI — which extracts with `skip` —
                            // showing 0% for an entire re-extraction.
                            Ok(None) => {
                                local.2 += 1;
                                reporter.extracted(1, entry.size);
                            }
                            // One file lost, not the extraction. The partial
                            // file goes with it: a short file that looks whole
                            // is worse than no file, because nothing later will
                            // ever tell the user it was truncated.
                            Err(e) => {
                                let _ = std::fs::remove_file(target);
                                failed
                                    .lock()
                                    .expect("mutex")
                                    .push((entry.path.clone(), format!("{e:#}")));
                                reporter.extracted(1, entry.size);
                            }
                        }
                    }
                    let mut c = counters.lock().expect("mutex");
                    c.0 += local.0;
                    c.1 += local.1;
                    c.2 += local.2;
                });
            }
        });

        if let Some(e) = failure.into_inner().expect("mutex") {
            return Err(e);
        }
        // Sorted, so two runs of the same damaged archive read the same however
        // the workers happened to be scheduled.
        let mut lost = failed.into_inner().expect("mutex");
        lost.sort_by(|a, b| a.0.cmp(&b.0));
        stats.failed = lost;
        // Deepest first: a directory's clock is moved by everything written
        // inside it, so it can only be set once nothing else will be.
        let mut dirs_last: Vec<&(&FileEntry, PathBuf)> = dir_entries.iter().collect();
        dirs_last.sort_by_key(|(e, _)| std::cmp::Reverse(e.path.matches('/').count()));
        for (entry, target) in dirs_last {
            restore_metadata(entry, target);
        }

        reporter.finish();
        let c = counters.into_inner().expect("mutex");
        stats.files = c.0;
        stats.bytes = c.1;
        stats.skipped_existing = c.2;
        Ok(stats)
    }

    /// Rename or move entries, by exact path or by directory prefix.
    ///
    /// This is what the format exists for. A path lives in the manifest and the
    /// bytes live in units, so moving a 4 GiB video into another folder rewrites
    /// a few hundred bytes of manifest and re-reads nothing at all. The same
    /// thing as remove + add would re-read and re-compress the file — which for
    /// an archive of family photos is the difference between instant and
    /// minutes.
    ///
    /// Refuses rather than clobbers: a destination that already exists, or that
    /// only differs in letter case from an existing entry, is an error.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<usize> {
        if !self.writable {
            bail!("archive opened read-only");
        }
        let from = paths::normalize_selector(from);
        let to = paths::normalize_selector(to);
        if from.is_empty() || to.is_empty() {
            bail!("both a source and a destination path are required");
        }
        if from == to {
            return Ok(0);
        }
        // Moving a directory into itself would build paths forever.
        if to.starts_with(&format!("{from}/")) {
            bail!("cannot move {from:?} into itself");
        }
        // The destination must be a path this archive could have been built
        // with, checked by the same rules extraction applies.
        paths::sanitize(&to).with_context(|| format!("unsafe destination {to:?}"))?;

        let prefix = format!("{from}/");
        let mut moves: Vec<(usize, String)> = Vec::new();
        for (i, f) in self.manifest.files.iter().enumerate() {
            let dest = if f.path == from {
                to.clone()
            } else if let Some(rest) = f.path.strip_prefix(&prefix) {
                format!("{to}/{rest}")
            } else {
                continue;
            };
            paths::sanitize(&dest).with_context(|| format!("unsafe destination {dest:?}"))?;
            moves.push((i, dest));
        }
        if moves.is_empty() {
            bail!("not found in archive: {from}");
        }

        // Collisions are checked against the whole archive as it will look
        // afterwards, including entries that only differ in letter case —
        // Windows and macOS cannot hold both, and a rename must not create a
        // pair that a later extraction would silently drop.
        let moving: HashSet<usize> = moves.iter().map(|(i, _)| *i).collect();
        let mut taken: HashMap<String, String> = self
            .manifest
            .files
            .iter()
            .enumerate()
            .filter(|(i, _)| !moving.contains(i))
            .map(|(_, f)| (paths::collision_key(&f.path), f.path.clone()))
            .collect();
        for (_, dest) in &moves {
            let key = paths::collision_key(dest);
            if let Some(other) = taken.get(&key) {
                bail!("{dest:?} would collide with {other:?} already in the archive");
            }
            taken.insert(key, dest.clone());
        }

        let n = moves.len();
        for (i, dest) in moves {
            self.manifest.files[i].path = dest;
        }
        self.commit()?;
        Ok(n)
    }

    /// Remove entries (exact path or directory prefix). Space is reclaimed
    /// later by `compact`.
    pub fn remove(&mut self, patterns: &[String]) -> Result<usize> {
        if !self.writable {
            bail!("archive opened read-only");
        }
        let sel: Vec<String> = patterns
            .iter()
            .map(|s| paths::normalize_selector(s))
            .collect();
        let mut used = vec![false; sel.len()];
        let mut kept = Vec::with_capacity(self.manifest.files.len());
        let mut removed = 0usize;
        for f in std::mem::take(&mut self.manifest.files) {
            let mut hit = false;
            for (i, s) in sel.iter().enumerate() {
                if f.path == *s || f.path.starts_with(&format!("{s}/")) {
                    used[i] = true;
                    hit = true;
                }
            }
            if hit {
                removed += 1;
            } else {
                kept.push(f);
            }
        }
        self.manifest.files = kept;
        let missing: Vec<&str> = sel
            .iter()
            .zip(&used)
            .filter(|(_, u)| !**u)
            .map(|(s, _)| s.as_str())
            .collect();
        if !missing.is_empty() {
            bail!("not found in archive: {}", missing.join(", "));
        }
        if removed > 0 {
            self.commit()?;
        }
        Ok(removed)
    }

    /// Bytes this entry occupies in the archive. For a file inside a solid
    /// block there is no separate answer — the block is one compressed
    /// stream — so its share is prorated by size.
    /// Bytes this entry occupies in the archive. A file inside a shared unit
    /// has no separate answer — the unit is one compressed stream — so its
    /// share is prorated by length.
    pub fn stored_size(&self, entry: &FileEntry) -> u64 {
        let mut total = 0u64;
        for e in &entry.extents {
            let Some(u) = self.manifest.chunks.get(e.unit as usize) else {
                continue;
            };
            total += if u.unpacked == 0 || e.len >= u.unpacked {
                u.packed
            } else {
                (u.packed as u128 * e.len as u128 / u.unpacked as u128) as u64
            };
        }
        total
    }

    /// Read every chunk a file can still reach, decode it and check it against
    /// the hash in the manifest. Writes nothing, anywhere.
    ///
    /// This is the question a user actually has — "is what I stored still
    /// there" — and until now the only way to answer it was to extract the
    /// whole archive somewhere and look. Every competitor has the verb.
    ///
    /// Three things it does deliberately:
    ///
    /// - It DOES NOT STOP at the first bad chunk. The point is the extent of
    ///   the damage, not its existence; stopping early would tell someone with
    ///   a half-readable backup nothing about which half.
    /// - It reports FILES, not chunk numbers. "Chunk 417 failed" is not
    ///   actionable; "these three files cannot be recovered, the other 9,000
    ///   can" is.
    /// - It tests only LIVE chunks — the ones a file references. Dead bytes are
    ///   what `compact` exists to drop, and failing over data nobody can
    ///   extract would be a false alarm.
    ///
    /// The hash covers the ORIGINAL bytes, before any filter (I4), so a pass
    /// also proves every recompression filter round-tripped exactly.
    pub fn test(&self, opts: &PackOptions, progress: Option<ProgressFn>) -> Result<TestStats> {
        let reporter = Reporter::new(progress);
        reporter.phase(Phase::Scan);
        let file_len = self.file.metadata()?.len();

        let mut live: Vec<u32> = self
            .manifest
            .files
            .iter()
            .flat_map(|f| f.extents.iter().map(|e| e.unit))
            .collect();
        live.sort_unstable();
        live.dedup();

        let mut stats = TestStats {
            chunks: live.len(),
            ..Default::default()
        };
        // Same sizing as extraction, and for the same reason: this IS the
        // decode path, so what makes extraction CPU-bound makes a test
        // CPU-bound, and a bsc chunk needs the large per-worker allowance.
        let slow = live
            .iter()
            .any(|&i| matches!(self.chunk_at(i).map(|c| c.codec), Some(2..=4)));
        let hungry = live
            .iter()
            .any(|&i| self.chunk_at(i).map(|c| c.codec) == Some(4));
        let workers = opts
            .extract_workers(slow, hungry)
            .min(live.len().max(1))
            .max(1);
        let total_bytes: u64 = live
            .iter()
            .filter_map(|&i| self.chunk_at(i))
            .map(|c| c.unpacked)
            .sum();
        reporter.totals(live.len() as u64, total_bytes);
        reporter.phase(Phase::Work);

        let next = AtomicUsize::new(0);
        let done = Mutex::new((0usize, 0u64, Vec::<(u32, String)>::new()));
        let opened: Mutex<Option<anyhow::Error>> = Mutex::new(None);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    // A private handle per worker: cloned handles share a file
                    // position on Windows, which would corrupt concurrent reads.
                    let reader = match File::open(&self.path) {
                        Ok(f) => f,
                        Err(e) => {
                            *opened.lock().expect("mutex") = Some(anyhow::Error::new(e));
                            return;
                        }
                    };
                    nova_platform::lower_io_priority(&reader);
                    let mut reader = &reader;
                    loop {
                        let n = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&idx) = live.get(n) else { break };
                        let Some(rec) = self.chunk_at(idx) else {
                            let mut d = done.lock().expect("mutex");
                            d.2.push((idx, "manifest refers to a chunk that is not there".into()));
                            continue;
                        };
                        // A read error is a verdict about this chunk, not a
                        // reason to abandon the archive: a bad sector under one
                        // unit says nothing about the rest of the file.
                        let verdict = read_packed(&mut reader, rec, file_len)
                            .and_then(|packed| verify_chunk(rec, &packed));
                        let mut d = done.lock().expect("mutex");
                        match verdict {
                            Ok(data) => {
                                d.0 += 1;
                                d.1 += data.len() as u64;
                            }
                            Err(e) => d.2.push((idx, format!("{e:#}"))),
                        }
                        let (ok, bytes) = (d.0, d.1);
                        drop(d);
                        // A verified chunk is finished work, the same way an
                        // extracted file is: `extracted` is the counter that
                        // carries that meaning.
                        reporter.extracted(ok as u64, bytes);
                    }
                });
            }
        });
        if let Some(e) = opened.lock().expect("mutex").take() {
            return Err(e);
        }

        let (ok, bytes, mut bad) = done.into_inner().expect("mutex");
        bad.sort_by_key(|(i, _)| *i);
        stats.chunks_ok = ok;
        stats.bytes_ok = bytes;
        let broken: HashSet<u32> = bad.iter().map(|(i, _)| *i).collect();
        stats.bad = bad;
        if !broken.is_empty() {
            for f in &self.manifest.files {
                if f.extents.iter().any(|e| broken.contains(&e.unit)) {
                    stats.damaged.push(f.path.clone());
                }
            }
        }
        reporter.finish();
        Ok(stats)
    }

    /// The chunk record an extent points at, or `None` if the manifest points
    /// past the end of its own table.
    fn chunk_at(&self, index: u32) -> Option<&ChunkRec> {
        self.manifest.chunks.get(index as usize)
    }

    pub fn info(&self) -> InfoStats {
        let file_len = self.file.metadata().map(|m| m.len()).unwrap_or(0);
        let dirs = self.manifest.files.iter().filter(|f| f.dir).count();
        let mut live: HashSet<u32> = HashSet::new();
        for f in &self.manifest.files {
            live.extend(f.extents.iter().map(|e| e.unit));
        }
        let live_bytes: u64 = live
            .iter()
            .filter_map(|&i| self.manifest.chunks.get(i as usize))
            .map(|c| c.packed)
            .sum();
        let overhead = HEADER_LEN + self.last_manifest_packed + FOOTER_LEN;

        let mut sizes: Vec<u64> = live
            .iter()
            .filter_map(|&i| self.manifest.chunks.get(i as usize))
            .map(|c| c.unpacked)
            .collect();
        sizes.sort_unstable();
        let mut by_codec: HashMap<u8, u64> = HashMap::new();
        for i in &live {
            if let Some(c) = self.manifest.chunks.get(*i as usize) {
                *by_codec.entry(c.codec).or_default() += c.packed;
            }
        }
        let mut by_codec: Vec<(u8, u64)> = by_codec.into_iter().collect();
        by_codec.sort_by_key(|(c, _)| *c);

        InfoStats {
            generation: self.manifest.generation,
            files: self.manifest.files.len() - dirs,
            dirs,
            chunks: self.manifest.chunks.len(),
            file_len,
            live_bytes,
            reclaimable: file_len.saturating_sub(live_bytes + overhead),
            units: sizes.len(),
            unit_min: sizes.first().copied().unwrap_or(0),
            unit_median: sizes.get(sizes.len() / 2).copied().unwrap_or(0),
            unit_max: sizes.last().copied().unwrap_or(0),
            by_codec,
        }
    }

    /// Every live unit, in archive order, with the codec that won it and the
    /// file types it holds. See [`UnitInfo`].
    pub fn units(&self) -> Vec<UnitInfo> {
        let mut members: HashMap<u32, HashMap<String, usize>> = HashMap::new();
        let mut files: HashMap<u32, HashSet<usize>> = HashMap::new();
        for (i, f) in self.manifest.files.iter().enumerate() {
            for e in &f.extents {
                if files.entry(e.unit).or_default().insert(i) {
                    *members
                        .entry(e.unit)
                        .or_default()
                        .entry(group_key(&f.path))
                        .or_default() += 1;
                }
            }
        }
        let mut out = Vec::with_capacity(files.len());
        for (idx, rec) in self.manifest.chunks.iter().enumerate() {
            let idx = idx as u32;
            let Some(exts) = members.get(&idx) else {
                continue; // dead unit, kept only as a dedup source
            };
            let top = exts
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                .map(|(e, _)| e.clone())
                .unwrap_or_default();
            out.push(UnitInfo {
                idx,
                unpacked: rec.unpacked,
                packed: rec.packed,
                codec: rec.codec,
                param: rec.param,
                filter: rec.filter,
                files: files.get(&idx).map_or(0, |s| s.len()),
                top_ext: if top.is_empty() { "-".into() } else { top },
                distinct_exts: exts.len(),
            });
        }
        out
    }

    /// Rewrite the archive keeping only live units, verifying every one on the
    /// way, then atomically replace the original. Consumes the archive (the
    /// file handle must be closed for the replace to work on Windows).
    pub fn compact(self) -> Result<(u64, u64)> {
        if !self.writable {
            bail!("archive opened read-only");
        }
        let Archive {
            file,
            path,
            manifest: old,
            ..
        } = self;
        let file_len = file.metadata()?.len();
        let before = file_len;
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
        tmp.as_file_mut().write_all(&footer::header_bytes())?;

        let mut new = Manifest {
            generation: old.generation,
            files: Vec::with_capacity(old.files.len()),
            chunks: Vec::new(),
            geometry: old.geometry,
        };
        let mut remap: HashMap<u32, u32> = HashMap::new();
        let mut reader = &file;
        for f in &old.files {
            let mut extents = Vec::with_capacity(f.extents.len());
            for e in &f.extents {
                let unit = match remap.get(&e.unit) {
                    Some(&n) => n,
                    None => {
                        let rec = old
                            .chunks
                            .get(e.unit as usize)
                            .context("corrupt manifest: unit index out of range")?;
                        let buf = read_packed(&mut reader, rec, file_len)?;
                        // Never carry corruption into the compacted archive.
                        verify_chunk(rec, &buf).with_context(|| format!("in {}", f.path))?;
                        let off = tmp.as_file_mut().seek(SeekFrom::End(0))?;
                        tmp.as_file_mut().write_all(&buf)?;
                        let n = u32::try_from(new.chunks.len())
                            .context("archive unit count exceeds format limit")?;
                        // Cloned wholesale on purpose: `filtered` and every
                        // other coding detail must carry through compact
                        // untouched, only the offset moves.
                        new.chunks.push(ChunkRec {
                            offset: off,
                            ..rec.clone()
                        });
                        remap.insert(e.unit, n);
                        n
                    }
                };
                extents.push(Extent { unit, ..*e });
            }
            new.files.push(FileEntry {
                extents,
                ..f.clone()
            });
        }
        new.generation += 1;
        write_tail(tmp.as_file_mut(), &new)?;
        let after = tmp.as_file_mut().metadata()?.len();

        drop(file); // close the original handle before replacing (Windows)
                    // Keep the archive's own identity: replacing in place preserves its
                    // permissions, attributes and creation time, where persisting the
                    // temporary file over it would carry the temp file's ACL instead.
        let (tmp_file, tmp_path) = tmp.keep().map_err(|e| anyhow::anyhow!("{}", e.error))?;
        drop(tmp_file);
        let swapped = nova_platform::replace_file(&tmp_path, &path);
        if swapped.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        swapped.context("cannot replace the archive with the compacted copy")?;
        #[cfg(unix)]
        {
            // Make the directory entry itself durable.
            let _ = File::open(&dir).and_then(|d| d.sync_all());
        }
        Ok((before, after))
    }
}

/// Holds the most recently decompressed unit. Files that share a unit are
/// adjacent in the manifest, so a single-entry cache turns "decompress the
/// unit once per file" into "decompress it once".
#[derive(Default)]
struct UnitCache {
    idx: Option<u32>,
    data: Vec<u8>,
}

impl UnitCache {
    fn load(
        &mut self,
        reader: &mut File,
        manifest: &Manifest,
        unit: u32,
        file_len: u64,
    ) -> Result<&[u8]> {
        if self.idx != Some(unit) {
            let rec = manifest
                .chunks
                .get(unit as usize)
                .context("corrupt manifest: unit index out of range")?;
            let packed = read_packed(&mut &*reader, rec, file_len)?;
            self.data = verify_chunk(rec, &packed)?;
            self.idx = Some(unit);
        }
        Ok(&self.data)
    }
}

/// Decode the units one window of extents needs, `lanes` at a time.
///
/// This exists because extraction parallelises across FILES, so an archive of
/// one large file used a single thread however many cores the budget allowed —
/// a bsc unit decodes at ~25 MB/s, so that was 4.8 s for enwik8 with seven
/// cores idle. Splitting the file's own units across lanes fixes it without
/// touching how the file is written: the writer still appends in order, from
/// one handle, so nothing about ordering, the overwrite policy or mtime moves.
///
/// The window is `lanes` extents wide, which bounds the extra memory at
/// `lanes` units — and `lanes` is only above 1 when there are fewer files than
/// workers, so the total in flight is the same as it always was.
fn decode_window(
    lanes: &mut [(File, UnitCache)],
    manifest: &Manifest,
    file_len: u64,
    window: &[u32],
) -> Result<Vec<Vec<u8>>> {
    let n = window.len();
    let mut out: Vec<Vec<u8>> = vec![Vec::new(); n];
    let next = AtomicUsize::new(0);
    let failure: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let slots: Vec<Mutex<&mut Vec<u8>>> = out.iter_mut().map(Mutex::new).collect();

    std::thread::scope(|scope| {
        for (reader, cache) in lanes.iter_mut() {
            let next = &next;
            let failure = &failure;
            let slots = &slots;
            scope.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n || failure.lock().expect("mutex").is_some() {
                    break;
                }
                match cache.load(reader, manifest, window[i], file_len) {
                    Ok(unit) => **slots[i].lock().expect("mutex") = unit.to_vec(),
                    Err(e) => {
                        let mut slot = failure.lock().expect("mutex");
                        if slot.is_none() {
                            *slot = Some(e);
                        }
                        break;
                    }
                }
            });
        }
    });
    drop(slots);
    match failure.into_inner().expect("mutex") {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

/// Everything one extraction worker needs to read units: its own archive
/// handle and cache, plus the extra lanes it may use inside a single file.
/// Grouped because the handles must never be shared — a cloned `File` shares
/// its position on Windows — so the ownership is the invariant.
struct Decoder {
    reader: File,
    cache: UnitCache,
    lanes: Vec<(File, UnitCache)>,
}

/// Write one entry to `target`. Returns the number of bytes written, or
/// `None` when an existing file was kept per the overwrite policy.
fn extract_one(
    dec: &mut Decoder,
    manifest: &Manifest,
    entry: &FileEntry,
    target: &Path,
    overwrite: Overwrite,
    file_len: u64,
) -> Result<Option<u64>> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = match OpenOptions::new().write(true).create_new(true).open(target) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match overwrite {
            Overwrite::Fail => bail!(
                "{} already exists - use --force to overwrite or --skip-existing",
                target.display()
            ),
            Overwrite::Skip => return Ok(None),
            Overwrite::Force => File::create(target)
                .with_context(|| format!("cannot write {}", target.display()))?,
        },
        Err(e) => return Err(e).with_context(|| format!("cannot write {}", target.display())),
    };
    nova_platform::lower_io_priority(&out);

    let mut written = 0u64;
    // One lane means the original path, byte for byte: decode into the shared
    // cache, write, move on. Several lanes only ever appear when this file
    // would otherwise leave cores idle.
    if dec.lanes.len() < 2 || entry.extents.len() < 2 {
        for e in &entry.extents {
            let unit = dec
                .cache
                .load(&mut dec.reader, manifest, e.unit, file_len)
                .with_context(|| format!("in {}", entry.path))?;
            let start = usize::try_from(e.off).context("extent offset out of range")?;
            let end = start
                .checked_add(usize::try_from(e.len).context("extent length out of range")?)
                .filter(|end| *end <= unit.len())
                .context("corrupt manifest: extent outside its unit")?;
            out.write_all(&unit[start..end])?;
            written += e.len;
        }
    } else {
        // Group by DISTINCT unit before splitting the work. Consecutive extents
        // of one file usually share a unit — several chunks land in the same
        // 16 or 32 MiB stream — and the sequential path gets that for free from
        // `UnitCache`. Parallelising per EXTENT threw it away and decoded the
        // same unit once per lane: measured, enwik8 went from 4.8 s to 8.2 s.
        let mut i = 0usize;
        while i < entry.extents.len() {
            let mut units: Vec<u32> = Vec::new();
            let mut j = i;
            while j < entry.extents.len() {
                let u = entry.extents[j].unit;
                if !units.contains(&u) {
                    if units.len() == dec.lanes.len() {
                        break;
                    }
                    units.push(u);
                }
                j += 1;
            }
            let decoded = decode_window(&mut dec.lanes, manifest, file_len, &units)
                .with_context(|| format!("in {}", entry.path))?;
            for e in &entry.extents[i..j] {
                let unit = units
                    .iter()
                    .position(|u| *u == e.unit)
                    .map(|k| &decoded[k])
                    .context("internal: extent unit missing from its window")?;
                let start = usize::try_from(e.off).context("extent offset out of range")?;
                let end = start
                    .checked_add(usize::try_from(e.len).context("extent length out of range")?)
                    .filter(|end| *end <= unit.len())
                    .context("corrupt manifest: extent outside its unit")?;
                out.write_all(&unit[start..end])?;
                written += e.len;
            }
            i = j;
        }
    }
    if written != entry.size {
        bail!("size mismatch extracting {}", entry.path);
    }
    drop(out);
    restore_metadata(entry, target);
    Ok(Some(written))
}

/// Put a timestamp and attributes back on something just written.
///
/// ORDER IS LOAD-BEARING: read-only goes on last. Setting it before the
/// contents are written makes the write fail, and setting it before the
/// timestamp makes the timestamp fail — Windows refuses to touch either on a
/// read-only file. Both are advisory: a tree that came back with the right
/// bytes and the wrong clock is still worth having, so a failure here is not
/// allowed to fail the extraction.
fn restore_metadata(entry: &FileEntry, target: &Path) {
    if entry.mtime != 0 || entry.mtime_nanos != 0 {
        let _ = filetime::set_file_mtime(
            target,
            filetime::FileTime::from_unix_time(entry.mtime, entry.mtime_nanos),
        );
    }
    if entry.attrs != 0 {
        let _ = nova_platform::set_file_attributes(target, entry.attrs);
    }
}

/// Grouping key for units: the extension, lowercased. Sorting files by it puts
/// .rs next to .rs and .png next to .png, which is what makes a shared unit
/// compress well.
pub(crate) fn group_key(rel: &str) -> String {
    match rel.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() && ext.len() <= 16 && !ext.contains('/') => {
            ext.to_lowercase()
        }
        _ => String::new(),
    }
}

fn lock_exclusive(file: &File, path: &Path) -> Result<()> {
    match nova_platform::try_lock_exclusive(file) {
        Ok(true) => Ok(()),
        Ok(false) => bail!(
            "{} is already open for writing by another process",
            path.display()
        ),
        Err(e) => Err(e).with_context(|| format!("cannot lock {}", path.display())),
    }
}

fn read_manifest(file: &mut File, ftr: &Footer, ftr_off: u64) -> Result<Manifest> {
    if ftr.manifest_offset < HEADER_LEN
        || ftr
            .manifest_offset
            .checked_add(ftr.manifest_packed)
            .is_none_or(|end| end > ftr_off)
    {
        bail!("corrupt footer: manifest location out of bounds");
    }
    let cap = usize::try_from(ftr.manifest_packed).context("manifest too large")?;
    let mut packed = vec![0u8; cap];
    file.seek(SeekFrom::Start(ftr.manifest_offset))?;
    file.read_exact(&mut packed)?;
    if blake3::hash(&packed).as_bytes()[..16] != ftr.manifest_hash {
        bail!("manifest checksum mismatch");
    }
    Manifest::decode(&packed, ftr.manifest_unpacked)
}

/// Read a chunk's packed bytes, with all sizes validated first: the manifest
/// may come from an untrusted archive and must not drive a huge allocation.
fn read_packed(reader: &mut &File, rec: &ChunkRec, file_len: u64) -> Result<Vec<u8>> {
    if rec.packed > MAX_STORED_CHUNK || rec.unpacked > MAX_STORED_CHUNK {
        bail!("corrupt manifest: implausible chunk size");
    }
    if rec.offset < HEADER_LEN
        || rec
            .offset
            .checked_add(rec.packed)
            .is_none_or(|end| end > file_len)
    {
        bail!("corrupt manifest: chunk points outside the archive");
    }
    let mut buf = vec![0u8; rec.packed as usize];
    reader.seek(SeekFrom::Start(rec.offset))?;
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

/// Decompress a chunk and check it against the hash recorded in the manifest.
fn verify_chunk(rec: &ChunkRec, packed: &[u8]) -> Result<Vec<u8>> {
    // The codec is asked for the length it produced, which is NOT the original
    // length once a filter can change it. That number also fixes the LZMA2
    // window and the PPMd7 model pool, neither of which is stored anywhere
    // else: too narrow an LZMA2 window fails only when a match happens to reach
    // past it, and a mismatched PPMd7 pool decodes into garbage of exactly the
    // right length.
    let coded = rec.coded_len();
    if coded > MAX_CODED_CHUNK {
        bail!("corrupt manifest: implausible coded chunk size");
    }
    // The codec's own words for a broken stream are things like "dist
    // overflow", which mean nothing to whoever is looking at a damaged backup;
    // the reason is kept, but it arrives behind a sentence that says what
    // happened.
    let mut data = codec::decompress(
        Codec::from_id(rec.codec)?,
        packed,
        coded as usize,
        rec.param,
    )
    .context("block did not decode - the archive is damaged here")?;
    // Undo the pre-compression transform before checking the hash: the hash
    // was taken over the original bytes, so it also proves the filter round
    // -tripped exactly.
    crate::filters::Filter::from_id(rec.filter)?.unapply(&mut data)?;
    if data.len() as u64 != rec.unpacked {
        bail!(
            "chunk unfiltered to {} bytes, manifest says {}",
            data.len(),
            rec.unpacked
        );
    }
    if blake3::hash(&data).as_bytes()[..16] != rec.hash {
        bail!("chunk checksum mismatch - archive is corrupt");
    }
    Ok(data)
}

/// Serialize the manifest, append it plus a footer to `file`, fsync.
/// Returns the packed manifest length.
fn write_tail(file: &mut File, manifest: &Manifest) -> Result<u64> {
    let (packed, unpacked) = manifest.encode()?;
    let off = file.seek(SeekFrom::End(0))?;
    file.write_all(&packed)?;
    // Barrier: chunk and manifest bytes must be durable *before* the footer
    // that declares them committed, otherwise a crash can leave a valid
    // footer pointing at a half-written manifest.
    file.sync_data()?;
    let mut hash = [0u8; 16];
    hash.copy_from_slice(&blake3::hash(&packed).as_bytes()[..16]);
    let ftr = Footer {
        generation: manifest.generation,
        manifest_offset: off,
        manifest_packed: packed.len() as u64,
        manifest_unpacked: unpacked,
        manifest_hash: hash,
    };
    let ftr_off = file.seek(SeekFrom::End(0))?;
    file.write_all(&ftr.encode(ftr_off))?;
    file.sync_all()?;
    Ok(packed.len() as u64)
}
