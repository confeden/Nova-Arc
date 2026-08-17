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
use crate::manifest::{Block, ChunkRec, FileEntry, Manifest};
use crate::paths;
use crate::pipeline::{self, PackOptions};

/// Largest compression unit any tier produces (see `Tier::cdc` and
/// `Tier::solid_block`). Used for the plausibility checks that keep a hostile
/// manifest from driving allocations, and for the memory model.
pub const MAX_CHUNK: u32 = 32 * 1024 * 1024;

const HEAD_SAMPLE: usize = 64 * 1024;

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
        if !self.writable {
            bail!("archive opened read-only");
        }
        let mut stats = AddStats::default();
        // Chunk boundaries must stay compatible with what is already in the
        // archive, or nothing deduplicates; only the compression method
        // follows the tier the user asked for now.
        let geom = *self
            .manifest
            .geometry
            .get_or_insert_with(|| opts.tier.geometry());
        if geom != opts.tier.geometry() {
            stats.warnings.push(format!(
                "archive was created with {} KiB average chunks; keeping that geometry so unchanged data still deduplicates",
                geom.chunk_avg / 1024
            ));
        }

        // Collect the work list first: small files are packed last, grouped
        // by extension, so the compressor sees similar data back to back.
        let mut work: Vec<(PathBuf, String, u64)> = Vec::new();
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
                work.push((input.clone(), rel, meta.len()));
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
                    let rel = paths::normalize_rel(&relp)?;
                    let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    work.push((entry.path().to_path_buf(), rel, len));
                }
            }
        }
        let solid_max = geom.solid_max_file;
        let (mut small, big): (Vec<_>, Vec<_>) =
            work.into_iter().partition(|(_, _, len)| *len < solid_max);
        // Group key = extension: .rs with .rs, .png with .png. Sorting by it
        // is what makes a solid block compress well.
        small.sort_by(|a, b| {
            let ka = solid_group_key(&a.1);
            let kb = solid_group_key(&b.1);
            ka.cmp(&kb).then_with(|| a.1.cmp(&b.1))
        });

        let mut ctx = AddCtx {
            tier: opts.tier,
            geom,
            base: u32::try_from(self.manifest.chunks.len())
                .context("archive chunk count exceeds format limit")?,
            block_base: u32::try_from(self.manifest.blocks.len())
                .context("archive block count exceeds format limit")?,
            dedup: self
                .manifest
                .chunks
                .iter()
                .enumerate()
                .map(|(i, c)| (c.hash, i as u32))
                .collect(),
            by_ci: self
                .manifest
                .files
                .iter()
                .map(|f| (paths::collision_key(&f.path), f.path.clone()))
                .collect(),
            files: Vec::new(),
            blocks: Vec::new(),
            solid: SolidBuilder::default(),
        };

        let out = pipeline::pack_with(&mut self.file, opts, |sub| {
            for (disk, rel, _) in &big {
                ctx.add_file(sub, disk, rel.clone(), &mut stats)?;
            }
            for (disk, rel, _) in &small {
                ctx.add_small_file(sub, disk, rel.clone(), &mut stats)?;
            }
            ctx.flush_solid(sub, &mut stats)?;
            Ok(())
        })?;

        // The writer emitted chunks in submission order, so the indices the
        // reader predicted (base + submission index) are now correct.
        stats.bytes_stored = out.bytes_stored;
        self.manifest.chunks.extend(out.chunks);
        self.manifest.blocks.extend(ctx.blocks);
        let mut by_path: HashMap<String, usize> = self
            .manifest
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), i))
            .collect();
        for entry in ctx.files {
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
                    let mut cache = BlockCache::default();
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
    pub fn stored_size(&self, entry: &FileEntry) -> u64 {
        if let Some((b, _)) = entry.block {
            let Some(block) = self.manifest.blocks.get(b as usize) else {
                return 0;
            };
            if block.size == 0 {
                return 0;
            }
            let packed: u64 = block
                .chunks
                .iter()
                .filter_map(|&i| self.manifest.chunks.get(i as usize))
                .map(|c| c.packed)
                .sum();
            return (packed as u128 * entry.size as u128 / block.size as u128) as u64;
        }
        entry
            .chunks
            .iter()
            .filter_map(|&i| self.manifest.chunks.get(i as usize))
            .map(|c| c.packed)
            .sum()
    }

    pub fn info(&self) -> InfoStats {
        let file_len = self.file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut live: HashSet<u32> = HashSet::new();
        for f in &self.manifest.files {
            live.extend(f.chunks.iter().copied());
            if let Some((b, _)) = f.block {
                if let Some(block) = self.manifest.blocks.get(b as usize) {
                    live.extend(block.chunks.iter().copied());
                }
            }
        }
        let live_bytes: u64 = live
            .iter()
            .filter_map(|&i| self.manifest.chunks.get(i as usize))
            .map(|c| c.packed)
            .sum();
        let overhead = HEADER_LEN + self.last_manifest_packed + FOOTER_LEN;
        InfoStats {
            generation: self.manifest.generation,
            files: self.manifest.files.len(),
            chunks: self.manifest.chunks.len(),
            file_len,
            live_bytes,
            reclaimable: file_len.saturating_sub(live_bytes + overhead),
        }
    }

    /// Rewrite the archive keeping only live chunks, verifying every chunk on
    /// the way, then atomically replace the original. Consumes the archive
    /// (the file handle must be closed for the replace to work on Windows).
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
            blocks: Vec::new(),
            geometry: old.geometry,
        };
        let mut remap: HashMap<u32, u32> = HashMap::new();
        let mut block_remap: HashMap<u32, u32> = HashMap::new();
        let mut reader = &file;
        // Copying a chunk verifies it first, so compaction can never bake
        // corruption into the new archive.
        let mut copy_chunk = |i: u32,
                              remap: &mut HashMap<u32, u32>,
                              new: &mut Manifest,
                              tmp: &mut tempfile::NamedTempFile,
                              what: &str|
         -> Result<u32> {
            if let Some(&n) = remap.get(&i) {
                return Ok(n);
            }
            let rec = old
                .chunks
                .get(i as usize)
                .context("corrupt manifest: chunk index out of range")?;
            let buf = read_packed(&mut reader, rec, file_len)?;
            verify_chunk(rec, &buf).with_context(|| format!("in {what}"))?;
            let off = tmp.as_file_mut().seek(SeekFrom::End(0))?;
            tmp.as_file_mut().write_all(&buf)?;
            let n = new.chunks.len() as u32;
            new.chunks.push(ChunkRec {
                offset: off,
                ..rec.clone()
            });
            remap.insert(i, n);
            Ok(n)
        };

        for f in &old.files {
            let mut ids = Vec::with_capacity(f.chunks.len());
            for &i in &f.chunks {
                ids.push(copy_chunk(i, &mut remap, &mut new, &mut tmp, &f.path)?);
            }
            // A solid block is copied once, when its first surviving member
            // is reached; blocks whose members were all removed are dropped.
            let block = match f.block {
                Some((b, off)) => {
                    let nb = match block_remap.get(&b) {
                        Some(&nb) => nb,
                        None => {
                            let src = old
                                .blocks
                                .get(b as usize)
                                .context("corrupt manifest: block index out of range")?;
                            let mut bids = Vec::with_capacity(src.chunks.len());
                            for &i in &src.chunks {
                                bids.push(copy_chunk(i, &mut remap, &mut new, &mut tmp, &f.path)?);
                            }
                            let nb = new.blocks.len() as u32;
                            new.blocks.push(Block {
                                chunks: bids,
                                size: src.size,
                            });
                            block_remap.insert(b, nb);
                            nb
                        }
                    };
                    Some((nb, off))
                }
                None => None,
            };
            new.files.push(FileEntry {
                chunks: ids,
                block,
                ..f.clone()
            });
        }
        new.generation += 1;
        write_tail(tmp.as_file_mut(), &new)?;
        let after = tmp.as_file_mut().metadata()?.len();

        drop(file); // close the original handle before replacing (Windows)
        tmp.persist(&path)
            .map_err(|e| anyhow::anyhow!("compaction failed: {}", e.error))?;
        #[cfg(unix)]
        {
            // Make the directory entry itself durable.
            let _ = File::open(&dir).and_then(|d| d.sync_all());
        }
        Ok((before, after))
    }
}

/// Holds the most recently decompressed solid block. Members of a block are
/// adjacent in the manifest, so a single-entry cache turns "decompress the
/// block per file" into "decompress it once".
#[derive(Default)]
struct BlockCache {
    idx: Option<u32>,
    data: Vec<u8>,
}

impl BlockCache {
    fn load(
        &mut self,
        reader: &mut File,
        manifest: &Manifest,
        block_idx: u32,
        file_len: u64,
    ) -> Result<&[u8]> {
        if self.idx != Some(block_idx) {
            let block = manifest
                .blocks
                .get(block_idx as usize)
                .context("corrupt manifest: block index out of range")?;
            if block.size > MAX_STORED_BLOCK {
                bail!("corrupt manifest: implausible solid block size");
            }
            let mut data = Vec::with_capacity(block.size as usize);
            for &idx in &block.chunks {
                let rec = manifest
                    .chunks
                    .get(idx as usize)
                    .context("corrupt manifest: chunk index out of range")?;
                let packed = read_packed(&mut &*reader, rec, file_len)?;
                data.extend_from_slice(&verify_chunk(rec, &packed)?);
            }
            if data.len() as u64 != block.size {
                bail!("corrupt archive: solid block size mismatch");
            }
            self.data = data;
            self.idx = Some(block_idx);
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
    cache: &mut BlockCache,
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
    if let Some((block_idx, offset)) = entry.block {
        let block = cache
            .load(reader, manifest, block_idx, file_len)
            .with_context(|| format!("in {}", entry.path))?;
        let start = usize::try_from(offset).context("solid offset out of range")?;
        let end = start
            .checked_add(usize::try_from(entry.size).context("file size out of range")?)
            .filter(|e| *e <= block.len())
            .context("corrupt manifest: file range outside its solid block")?;
        out.write_all(&block[start..end])?;
        written = entry.size;
    } else {
        for &idx in &entry.chunks {
            let rec = manifest
                .chunks
                .get(idx as usize)
                .context("corrupt manifest: chunk index out of range")?;
            let packed = read_packed(&mut &*reader, rec, file_len)?;
            let data = verify_chunk(rec, &packed).with_context(|| format!("in {}", entry.path))?;
            out.write_all(&data)?;
            written += data.len() as u64;
        }
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

/// Grouping key for solid blocks: the extension, lowercased. Sorting small
/// files by it puts .rs next to .rs and .png next to .png.
fn solid_group_key(rel: &str) -> String {
    match rel.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() && ext.len() <= 16 && !ext.contains('/') => {
            ext.to_lowercase()
        }
        _ => String::new(),
    }
}

/// Accumulates small files into one contiguous buffer that is compressed as
/// a single stream.
#[derive(Default)]
struct SolidBuilder {
    buf: Vec<u8>,
    /// Files placed in the current block, with their offsets in `buf`.
    members: Vec<(FileEntry, u64)>,
}

/// Reader-side state for one `add_paths` run: dedup index, collision
/// detection, and the file entries being built. Chunk indices are predicted
/// as `base + submission index`, which is exact because the pipeline's writer
/// appends chunks in submission order.
struct AddCtx {
    tier: Tier,
    geom: crate::manifest::Geometry,
    base: u32,
    block_base: u32,
    dedup: HashMap<[u8; 16], u32>,
    by_ci: HashMap<String, String>,
    files: Vec<FileEntry>,
    blocks: Vec<Block>,
    solid: SolidBuilder,
}

impl AddCtx {
    /// Append a small file to the pending solid block, flushing first if the
    /// block is full.
    fn add_small_file(
        &mut self,
        sub: &mut pipeline::Submitter,
        disk: &Path,
        rel: String,
        stats: &mut AddStats,
    ) -> Result<()> {
        let data = fs::read(disk).with_context(|| format!("cannot read {}", disk.display()))?;
        let target = self.geom.solid_block as usize;
        if self.solid.buf.len() + data.len() > target * 2 && !self.solid.buf.is_empty() {
            self.flush_solid(sub, stats)?;
        }
        let meta = fs::metadata(disk)?;
        let entry = FileEntry {
            path: rel.clone(),
            size: data.len() as u64,
            mtime: mtime_of(&meta),
            chunks: Vec::new(),
            block: None, // filled in by flush_solid
        };
        let offset = self.solid.buf.len() as u64;
        let cut = blake3::hash(&data);
        self.solid.buf.extend_from_slice(&data);
        self.solid.members.push((entry, offset));
        self.note_case_collision(&rel, stats);
        stats.bytes_in += data.len() as u64;
        stats.files += 1;

        // Content-defined block boundary: end the block here with probability
        // `size / target`, so blocks average the target size while the
        // decision depends on nothing but this file's own hash and length.
        //
        // That independence is the whole point. A boundary rule that looked at
        // the accumulated buffer size would move every later boundary as soon
        // as one file changed length, so re-saving a tree after a one-line
        // edit rewrote every block — measured at 17 MiB of growth. Now only
        // the edited file's own block changes; the rest stay byte-identical
        // and deduplicate away.
        let h = u64::from_le_bytes(cut.as_bytes()[..8].try_into().expect("blake3 is 32 bytes"));
        let size = self.solid.members.last().map_or(0, |(e, _)| e.size);
        if h % (target as u64) < size || self.solid.buf.len() >= target * 2 {
            self.flush_solid(sub, stats)?;
        }
        Ok(())
    }

    /// Compress the pending solid block and record its members.
    fn flush_solid(&mut self, sub: &mut pipeline::Submitter, stats: &mut AddStats) -> Result<()> {
        if self.solid.members.is_empty() {
            return Ok(());
        }
        let buf = std::mem::take(&mut self.solid.buf);
        let members = std::mem::take(&mut self.solid.members);
        let plan = analyze::plan(&buf[..buf.len().min(HEAD_SAMPLE)], self.tier);
        let block_size = buf.len() as u64;

        // The block is one compression unit, not a stream to be re-chunked:
        // splitting it would hide exactly the cross-file redundancy it exists
        // to expose (measured: 8 MiB blocks re-chunked at 4 MiB give 9.9% on
        // a source tree, compressed whole at 32 MiB they give 7.9%).
        let mut chunk_ids = Vec::new();
        let mut key = [0u8; 16];
        key.copy_from_slice(&blake3::hash(&buf).as_bytes()[..16]);
        match self.dedup.get(&key) {
            Some(&idx) => {
                stats.bytes_deduped += block_size;
                chunk_ids.push(idx);
            }
            None => {
                let local = sub.submit_filtered(
                    buf,
                    key,
                    self.tier.candidates(plan.codec),
                    plan.filter.id(),
                )?;
                let idx = self
                    .base
                    .checked_add(local)
                    .context("archive chunk count exceeds format limit")?;
                self.dedup.insert(key, idx);
                chunk_ids.push(idx);
            }
        }

        let block_idx = self
            .block_base
            .checked_add(u32::try_from(self.blocks.len()).unwrap_or(u32::MAX))
            .context("archive block count exceeds format limit")?;
        self.blocks.push(Block {
            chunks: chunk_ids,
            size: block_size,
        });
        for (mut entry, offset) in members {
            entry.block = Some((block_idx, offset));
            self.files.push(entry);
        }
        Ok(())
    }

    /// Warn about entries that differ only in letter case: Windows and macOS
    /// cannot hold both.
    fn note_case_collision(&mut self, rel: &str, stats: &mut AddStats) {
        let ck = paths::collision_key(rel);
        match self.by_ci.get(&ck) {
            Some(other) if other != rel => stats.warnings.push(format!(
                "{rel:?} differs only in letter case from {other:?}; \
                 they cannot both be extracted on Windows"
            )),
            Some(_) => {}
            None => {
                self.by_ci.insert(ck, rel.to_string());
            }
        }
    }
}

fn mtime_of(meta: &fs::Metadata) -> i64 {
    match meta.modified() {
        Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            // Pre-1970 timestamps are representable and worth keeping.
            Err(e) => -(e.duration().as_secs() as i64),
        },
        Err(_) => 0,
    }
}

impl AddCtx {
    fn add_file(
        &mut self,
        sub: &mut pipeline::Submitter,
        disk: &Path,
        rel: String,
        stats: &mut AddStats,
    ) -> Result<()> {
        let meta = fs::metadata(disk)?;
        let mtime = mtime_of(&meta);

        let mut chunk_ids: Vec<u32> = Vec::new();
        let mut actual_size = 0u64;
        if meta.len() > 0 {
            let mut f =
                File::open(disk).with_context(|| format!("cannot read {}", disk.display()))?;
            narc_platform::lower_io_priority(&f);
            // Phase 1: analysis — sample the head, pick the storage method.
            let head_len = HEAD_SAMPLE.min(meta.len() as usize);
            let mut head = Vec::with_capacity(head_len);
            (&mut f).take(head_len as u64).read_to_end(&mut head)?;
            let plan = analyze::plan(&head, self.tier);
            // Phase 2: chunk, hash, dedup, then hand unique chunks to the
            // compression pipeline. Memory stays bounded by the pipeline's
            // budget regardless of file size.
            let reader = std::io::Cursor::new(head).chain(BufReader::new(f));
            let (min, avg, max) = (
                self.geom.chunk_min,
                self.geom.chunk_avg,
                self.geom.chunk_max,
            );
            for result in StreamCDC::new(reader, min, avg, max) {
                let chunk = result.with_context(|| format!("reading {}", disk.display()))?;
                let data = chunk.data;
                let unpacked_len = data.len() as u64;
                stats.bytes_in += unpacked_len;
                actual_size += unpacked_len;
                let mut key = [0u8; 16];
                key.copy_from_slice(&blake3::hash(&data).as_bytes()[..16]);
                if let Some(&idx) = self.dedup.get(&key) {
                    stats.bytes_deduped += unpacked_len;
                    chunk_ids.push(idx);
                    continue;
                }
                let local = sub.submit_filtered(
                    data,
                    key,
                    self.tier.candidates(plan.codec),
                    plan.filter.id(),
                )?;
                let idx = self
                    .base
                    .checked_add(local)
                    .context("archive chunk count exceeds format limit")?;
                self.dedup.insert(key, idx);
                chunk_ids.push(idx);
            }
        }

        let entry = FileEntry {
            path: rel.clone(),
            size: actual_size,
            mtime,
            chunks: chunk_ids,
            block: None,
        };
        self.note_case_collision(&rel, stats);
        self.files.push(entry);
        stats.files += 1;
        Ok(())
    }
}

/// Take an exclusive advisory lock so two writers cannot append at the same
/// stale EOF and corrupt each other's chunks.
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
