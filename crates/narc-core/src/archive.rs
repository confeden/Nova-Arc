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

use anyhow::{bail, Context, Result};
use fastcdc::v2020::StreamCDC;

use crate::analyze::{pick_codec, Tier};
use crate::codec::{self, Codec};
use crate::footer::{self, Footer, FOOTER_LEN, HEADER_LEN};
use crate::manifest::{ChunkRec, FileEntry, Manifest};
use crate::paths;

pub const MIN_CHUNK: u32 = 256 * 1024;
pub const AVG_CHUNK: u32 = 1024 * 1024;
pub const MAX_CHUNK: u32 = 4 * 1024 * 1024;

const HEAD_SAMPLE: usize = 64 * 1024;

/// Upper bound on a stored chunk. Compression can expand incompressible data
/// slightly, hence the headroom; anything beyond is a corrupt manifest.
const MAX_STORED_CHUNK: u64 = MAX_CHUNK as u64 * 2;

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
    pub fn add_paths(&mut self, inputs: &[PathBuf], tier: Tier) -> Result<AddStats> {
        if !self.writable {
            bail!("archive opened read-only");
        }
        let mut stats = AddStats::default();
        let mut dedup: HashMap<[u8; 16], u32> = self
            .manifest
            .chunks
            .iter()
            .enumerate()
            .map(|(i, c)| (c.hash, i as u32))
            .collect();
        let mut by_path: HashMap<String, usize> = self
            .manifest
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), i))
            .collect();
        let mut by_ci: HashMap<String, String> = self
            .manifest
            .files
            .iter()
            .map(|f| (paths::collision_key(&f.path), f.path.clone()))
            .collect();
        let level = tier.zstd_level();

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
                self.add_one(
                    input,
                    rel,
                    level,
                    &mut dedup,
                    &mut by_path,
                    &mut by_ci,
                    &mut stats,
                )?;
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
                    self.add_one(
                        entry.path(),
                        rel,
                        level,
                        &mut dedup,
                        &mut by_path,
                        &mut by_ci,
                        &mut stats,
                    )?;
                }
            }
        }
        self.commit()?;
        Ok(stats)
    }

    #[allow(clippy::too_many_arguments)]
    fn add_one(
        &mut self,
        disk: &Path,
        rel: String,
        level: i32,
        dedup: &mut HashMap<[u8; 16], u32>,
        by_path: &mut HashMap<String, usize>,
        by_ci: &mut HashMap<String, String>,
        stats: &mut AddStats,
    ) -> Result<()> {
        let meta = fs::metadata(disk)?;
        let mtime = match meta.modified() {
            Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
                Ok(d) => d.as_secs() as i64,
                // Pre-1970 timestamps are representable and worth keeping.
                Err(e) => -(e.duration().as_secs() as i64),
            },
            Err(_) => 0,
        };

        let mut chunk_ids: Vec<u32> = Vec::new();
        let mut actual_size = 0u64;
        if meta.len() > 0 {
            let mut f =
                File::open(disk).with_context(|| format!("cannot read {}", disk.display()))?;
            // Phase 1: analysis — sample the head, pick the storage method.
            let head_len = HEAD_SAMPLE.min(meta.len() as usize);
            let mut head = Vec::with_capacity(head_len);
            (&mut f).take(head_len as u64).read_to_end(&mut head)?;
            let file_codec = pick_codec(&head);
            // Phase 2: chunk + compress, streaming; memory stays bounded by
            // MAX_CHUNK regardless of file size.
            let reader = std::io::Cursor::new(head).chain(BufReader::new(f));
            for result in StreamCDC::new(reader, MIN_CHUNK, AVG_CHUNK, MAX_CHUNK) {
                let chunk = result.with_context(|| format!("reading {}", disk.display()))?;
                let data = chunk.data;
                let unpacked_len = data.len() as u64;
                stats.bytes_in += unpacked_len;
                actual_size += unpacked_len;
                let mut key = [0u8; 16];
                key.copy_from_slice(&blake3::hash(&data).as_bytes()[..16]);
                if let Some(&idx) = dedup.get(&key) {
                    stats.bytes_deduped += unpacked_len;
                    chunk_ids.push(idx);
                    continue;
                }
                let (codec_used, payload) = match file_codec {
                    Codec::Store => (Codec::Store, data),
                    Codec::Zstd => {
                        let c = codec::compress(Codec::Zstd, level, &data)?;
                        if c.len() >= data.len() {
                            (Codec::Store, data)
                        } else {
                            (Codec::Zstd, c)
                        }
                    }
                };
                let offset = self.file.seek(SeekFrom::End(0))?;
                self.file.write_all(&payload)?;
                let idx = u32::try_from(self.manifest.chunks.len())
                    .context("archive chunk count exceeds format limit")?;
                self.manifest.chunks.push(ChunkRec {
                    offset,
                    packed: payload.len() as u64,
                    unpacked: unpacked_len,
                    codec: codec_used.id(),
                    hash: key,
                });
                stats.bytes_stored += payload.len() as u64;
                dedup.insert(key, idx);
                chunk_ids.push(idx);
            }
        }

        let entry = FileEntry {
            path: rel.clone(),
            size: actual_size,
            mtime,
            chunks: chunk_ids,
        };
        match by_path.get(&rel) {
            Some(&i) => self.manifest.files[i] = entry,
            None => {
                // Two entries differing only in case cannot both be extracted
                // on Windows/macOS - warn while the user can still react.
                let ck = paths::collision_key(&rel);
                if let Some(other) = by_ci.get(&ck) {
                    if other != &rel {
                        stats.warnings.push(format!(
                            "{rel:?} differs only in letter case from {other:?}; \
                             they cannot both be extracted on Windows"
                        ));
                    }
                } else {
                    by_ci.insert(ck, rel.clone());
                }
                by_path.insert(rel, self.manifest.files.len());
                self.manifest.files.push(entry);
            }
        }
        stats.files += 1;
        Ok(())
    }

    /// Extract everything, or only entries matching the given archive paths
    /// (exact file path or directory prefix). Selectors are normalized, and
    /// a selector matching nothing is an error rather than a silent no-op.
    pub fn extract(
        &self,
        dest: &Path,
        select: Option<&[String]>,
        overwrite: Overwrite,
    ) -> Result<ExtractStats> {
        let mut stats = ExtractStats::default();
        let selectors: Option<Vec<String>> = select.map(|s| {
            s.iter()
                .map(|x| paths::normalize_selector(x))
                .collect::<Vec<_>>()
        });
        let mut used = vec![false; selectors.as_ref().map_or(0, |s| s.len())];
        let file_len = self.file.metadata()?.len();
        let mut seen: HashSet<String> = HashSet::new();
        let mut reader = &self.file;

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
            let target = dest.join(&safe);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
            {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match overwrite {
                    Overwrite::Fail => bail!(
                        "{} already exists - use --force to overwrite or --skip-existing",
                        target.display()
                    ),
                    Overwrite::Skip => {
                        stats.skipped_existing += 1;
                        continue;
                    }
                    Overwrite::Force => File::create(&target)
                        .with_context(|| format!("cannot write {}", target.display()))?,
                },
                Err(e) => {
                    return Err(e).with_context(|| format!("cannot write {}", target.display()))
                }
            };
            let mut written = 0u64;
            for &idx in &entry.chunks {
                let rec = self
                    .manifest
                    .chunks
                    .get(idx as usize)
                    .context("corrupt manifest: chunk index out of range")?;
                let packed = read_packed(&mut reader, rec, file_len)?;
                let data =
                    verify_chunk(rec, &packed).with_context(|| format!("in {}", entry.path))?;
                out.write_all(&data)?;
                written += data.len() as u64;
            }
            if written != entry.size {
                bail!("size mismatch extracting {}", entry.path);
            }
            drop(out);
            if entry.mtime != 0 {
                let _ = filetime::set_file_mtime(
                    &target,
                    filetime::FileTime::from_unix_time(entry.mtime, 0),
                );
            }
            stats.files += 1;
            stats.bytes += written;
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

    pub fn info(&self) -> InfoStats {
        let file_len = self.file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut live: HashSet<u32> = HashSet::new();
        for f in &self.manifest.files {
            live.extend(f.chunks.iter().copied());
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
        };
        let mut remap: HashMap<u32, u32> = HashMap::new();
        let mut reader = &file;
        for f in &old.files {
            let mut ids = Vec::with_capacity(f.chunks.len());
            for &i in &f.chunks {
                if let Some(&n) = remap.get(&i) {
                    ids.push(n);
                    continue;
                }
                let rec = old
                    .chunks
                    .get(i as usize)
                    .context("corrupt manifest: chunk index out of range")?;
                let buf = read_packed(&mut reader, rec, file_len)?;
                // Never carry corruption into the compacted archive.
                verify_chunk(rec, &buf).with_context(|| format!("in {}", f.path))?;
                let off = tmp.as_file_mut().seek(SeekFrom::End(0))?;
                tmp.as_file_mut().write_all(&buf)?;
                let n = new.chunks.len() as u32;
                new.chunks.push(ChunkRec {
                    offset: off,
                    ..rec.clone()
                });
                remap.insert(i, n);
                ids.push(n);
            }
            new.files.push(FileEntry {
                chunks: ids,
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

/// Take an exclusive advisory lock so two writers cannot append at the same
/// stale EOF and corrupt each other's chunks.
fn lock_exclusive(file: &File, path: &Path) -> Result<()> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(fs::TryLockError::WouldBlock) => bail!(
            "{} is already open for writing by another process",
            path.display()
        ),
        Err(fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("cannot lock {}", path.display()))
        }
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
    let data = codec::decompress(Codec::from_id(rec.codec)?, packed, rec.unpacked as usize)?;
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
