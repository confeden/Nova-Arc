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
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use fastcdc::v2020::StreamCDC;

use crate::analyze::{self, Tier};
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

pub(crate) const HEAD_SAMPLE: usize = 64 * 1024;

/// Upper bound on a stored chunk. Compression can expand incompressible data
/// slightly, hence the headroom; anything beyond is a corrupt manifest.
const MAX_STORED_CHUNK: u64 = MAX_CHUNK as u64 * 2;

/// Upper bound on a solid block, for the same reason.
const MAX_STORED_BLOCK: u64 = MAX_CHUNK as u64 * 2;

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
}

/// Progress of a long operation, for a UI that must stay alive while the max
/// tier grinds through a large archive.
#[derive(Clone, Copy, Debug, Default)]
pub struct Progress {
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// Callback invoked as work completes. Called from the thread driving the
/// operation, so it must be cheap and must not block.
pub type ProgressFn<'a> = &'a (dyn Fn(Progress) + Send + Sync);

#[derive(Debug, Default)]
pub struct AddStats {
    pub files: usize,
    pub bytes_in: u64,
    pub bytes_stored: u64,
    pub bytes_deduped: u64,
    pub symlinks_skipped: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ExtractStats {
    pub files: usize,
    pub bytes: u64,
    pub skipped_existing: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct InfoStats {
    pub generation: u64,
    pub files: usize,
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
        let mut limit = file.metadata()?.len();
        let mut last_err = None;
        let mut found = None;
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

        // Drop uncommitted trailing garbage (e.g. after a crash mid-update)
        // so new appends land right after the committed footer.
        let committed_end = ftr_off + FOOTER_LEN;
        if writable && file.metadata()?.len() > committed_end {
            file.set_len(committed_end)?;
        }
        Ok(Archive {
            file,
            path: path.to_path_buf(),
            manifest,
            last_manifest_packed: ftr.manifest_packed,
            writable,
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

        // Collect the work list first and sort it by extension: neighbouring
        // files of the same type share a unit, which is where the compressor
        // finds what they have in common.
        let mut work: Vec<(PathBuf, String)> = Vec::new();
        for input in inputs {
            let meta = fs::symlink_metadata(input)
                .with_context(|| format!("cannot access {}", input.display()))?;
            if meta.file_type().is_symlink() {
                stats.symlinks_skipped += 1;
                continue;
            }
            if meta.is_file() {
                let rel = match input.file_name() {
                    Some(n) => paths::normalize_rel(Path::new(n))?,
                    None => bail!("cannot determine archive name for {}", input.display()),
                };
                work.push((input.clone(), rel));
            } else {
                let root_name = input.file_name().map(|n| n.to_owned());
                for entry in walkdir::WalkDir::new(input)
                    .sort_by_file_name()
                    .follow_links(false)
                {
                    let entry = entry?;
                    if entry.file_type().is_symlink() {
                        stats.symlinks_skipped += 1;
                        continue;
                    }
                    if !entry.file_type().is_file() {
                        continue;
                    }
                    let inner = entry
                        .path()
                        .strip_prefix(input)
                        .expect("walkdir yields children of its root");
                    let mut relp = PathBuf::new();
                    if let Some(ref n) = root_name {
                        relp.push(n);
                    }
                    relp.push(inner);
                    work.push((entry.path().to_path_buf(), paths::normalize_rel(&relp)?));
                }
            }
        }
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

        let files_total = work.len() as u64;
        let bytes_total: u64 = work
            .iter()
            .filter_map(|(disk, _)| fs::metadata(disk).ok().map(|m| m.len()))
            .sum();
        let out = pipeline::pack_with(&mut self.file, opts, |sub| {
            for (disk, rel) in &work {
                packer.add_file(sub, disk, rel.clone(), &mut stats)?;
                if let Some(cb) = progress {
                    cb(Progress {
                        files_done: stats.files as u64,
                        files_total,
                        bytes_done: stats.bytes_in,
                        bytes_total,
                    });
                }
            }
            packer.flush(sub, &mut stats)?;
            Ok(())
        })?;

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
        for entry in packer.into_files() {
            match by_path.get(&entry.path) {
                Some(&i) => self.manifest.files[i] = entry,
                None => {
                    by_path.insert(entry.path.clone(), self.manifest.files.len());
                    self.manifest.files.push(entry);
                }
            }
        }
        self.commit()?;
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

        // LZMA2 and PPMd7 make extraction CPU-bound; zstd/store leave it
        // I/O-bound. Which one this archive is decides the thread count.
        let slow_codecs = self
            .manifest
            .chunks
            .iter()
            .any(|c| matches!(c.codec, 2 | 3));
        let workers = opts.extract_workers(slow_codecs).min(work.len().max(1));
        let bytes_total: u64 = work.iter().map(|(e, _)| e.size).sum();
        let done_files = AtomicUsize::new(0);
        let done_bytes = std::sync::atomic::AtomicU64::new(0);
        let counters = Mutex::new((0usize, 0u64, 0usize)); // files, bytes, skipped
        let failure: Mutex<Option<anyhow::Error>> = Mutex::new(None);
        let stop = AtomicBool::new(false);
        let next = AtomicUsize::new(0);

        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    // A private handle per worker: cloned handles share a file
                    // position on Windows, which would corrupt concurrent reads.
                    let mut reader = match File::open(&self.path) {
                        Ok(f) => f,
                        Err(e) => {
                            *failure.lock().expect("mutex") = Some(anyhow::Error::new(e));
                            stop.store(true, Ordering::Relaxed);
                            return;
                        }
                    };
                    narc_platform::lower_io_priority(&reader);
                    let mut cache = UnitCache::default();
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
                            &mut reader,
                            &self.manifest,
                            entry,
                            target,
                            overwrite,
                            file_len,
                            &mut cache,
                        ) {
                            Ok(Some(bytes)) => {
                                local.0 += 1;
                                local.1 += bytes;
                                if let Some(cb) = progress {
                                    let done = done_files.fetch_add(1, Ordering::Relaxed) + 1;
                                    let b = done_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes;
                                    cb(Progress {
                                        files_done: done as u64,
                                        files_total: work.len() as u64,
                                        bytes_done: b,
                                        bytes_total,
                                    });
                                }
                            }
                            Ok(None) => local.2 += 1,
                            Err(e) => {
                                stop.store(true, Ordering::Relaxed);
                                let mut slot = failure.lock().expect("mutex");
                                if slot.is_none() {
                                    *slot = Some(e);
                                }
                                break;
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
        let c = counters.into_inner().expect("mutex");
        stats.files = c.0;
        stats.bytes = c.1;
        stats.skipped_existing = c.2;
        Ok(stats)
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

    pub fn info(&self) -> InfoStats {
        let file_len = self.file.metadata().map(|m| m.len()).unwrap_or(0);
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
            files: self.manifest.files.len(),
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
        let swapped = narc_platform::replace_file(&tmp_path, &path);
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

/// Write one entry to `target`. Returns the number of bytes written, or
/// `None` when an existing file was kept per the overwrite policy.
fn extract_one(
    reader: &mut File,
    manifest: &Manifest,
    entry: &FileEntry,
    target: &Path,
    overwrite: Overwrite,
    file_len: u64,
    cache: &mut UnitCache,
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
    narc_platform::lower_io_priority(&out);

    let mut written = 0u64;
    for e in &entry.extents {
        let unit = cache
            .load(reader, manifest, e.unit, file_len)
            .with_context(|| format!("in {}", entry.path))?;
        let start = usize::try_from(e.off).context("extent offset out of range")?;
        let end = start
            .checked_add(usize::try_from(e.len).context("extent length out of range")?)
            .filter(|end| *end <= unit.len())
            .context("corrupt manifest: extent outside its unit")?;
        out.write_all(&unit[start..end])?;
        written += e.len;
    }
    if written != entry.size {
        bail!("size mismatch extracting {}", entry.path);
    }
    drop(out);
    if entry.mtime != 0 {
        let _ =
            filetime::set_file_mtime(target, filetime::FileTime::from_unix_time(entry.mtime, 0));
    }
    Ok(Some(written))
}

/// Grouping key for units: the extension, lowercased. Sorting files by it puts
/// .rs next to .rs and .png next to .png, which is what makes a shared unit
/// compress well.
fn group_key(rel: &str) -> String {
    match rel.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() && ext.len() <= 16 && !ext.contains('/') => {
            ext.to_lowercase()
        }
        _ => String::new(),
    }
}

fn lock_exclusive(file: &File, path: &Path) -> Result<()> {
    match narc_platform::try_lock_exclusive(file) {
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
    let mut data = codec::decompress(
        Codec::from_id(rec.codec)?,
        packed,
        rec.unpacked as usize,
        rec.param,
    )?;
    // Undo the pre-compression transform before checking the hash: the hash
    // was taken over the original bytes, so it also proves the filter round
    // -tripped exactly.
    crate::filters::Filter::from_id(rec.filter)?.unapply(&mut data);
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
