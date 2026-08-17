//! Turning files into compression units.
//!
//! A **unit** is one independently decodable compressed stream. It may hold
//! many small files, a run of one large file's chunks, or exactly one file —
//! the packer does not care which, and that is the point: unit size is the
//! biggest single ratio lever there is. Measured on a 5739-file source tree
//! with LZMA2 `-9e`, splitting the same bytes into 4 MiB units costs **50%**
//! more than one solid stream, while 32 MiB units cost only **5%**. Before
//! units existed, small files were grouped but large files were compressed in
//! ~4 MiB chunks, and that alone made narc 2.3x worse than 7-Zip on a set of
//! executables (5.2 MiB vs 2.3 MiB).
//!
//! Unit boundaries are content-defined, decided from the hash and length of
//! the item just added and nothing else. A rule that looked at the accumulated
//! size would move every later boundary as soon as one file changed length, so
//! re-saving a tree after a one-line edit rewrote every unit — measured at
//! 17 MiB of growth. Now only the edited file's own unit changes and the rest
//! deduplicate away.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result};
use fastcdc::v2020::StreamCDC;

use crate::analyze::{self, Tier};
use crate::codec::Codec;
use crate::archive::{AddStats, HEAD_SAMPLE};
use crate::manifest::{Extent, FileEntry, Geometry};
use crate::paths;
use crate::pipeline::Submitter;

/// Below this size a file's content class is guesswork, so it is not allowed
/// to decide anything: it simply joins whatever unit is open.
const CLASSIFIABLE: u64 = 4096;

/// Builds units and the file entries that point into them.
pub(crate) struct Packer {
    tier: Tier,
    geom: Geometry,
    /// Index the next new unit will get in the manifest.
    base: u32,
    /// Unit content hash -> unit index, for deduplication.
    dedup: HashMap<[u8; 16], u32>,
    /// Case-folded path -> the path that claimed it, to warn about entries
    /// that Windows and macOS cannot both extract.
    by_ci: HashMap<String, String>,
    /// Entries produced so far.
    files: Vec<FileEntry>,
    /// Bytes of the unit being assembled.
    buf: Vec<u8>,
    /// What kind of data the current unit holds.
    kind: Option<crate::analyze::Kind>,
    /// Extents waiting for their unit to be flushed: (file index, offset, len).
    pending: Vec<(usize, u64, u64)>,
}

impl Packer {
    pub(crate) fn new(
        tier: Tier,
        geom: Geometry,
        base: u32,
        dedup: HashMap<[u8; 16], u32>,
        by_ci: HashMap<String, String>,
    ) -> Self {
        Packer {
            tier,
            geom,
            base,
            dedup,
            by_ci,
            files: Vec::new(),
            buf: Vec::new(),
            kind: None,
            pending: Vec::new(),
        }
    }

    pub(crate) fn into_files(self) -> Vec<FileEntry> {
        debug_assert!(self.buf.is_empty(), "flush before taking the entries");
        self.files
    }

    fn target(&self) -> usize {
        self.geom.unit as usize
    }

    /// Add one file. Small files are placed whole; larger ones are cut into
    /// content-defined chunks first, so that changing part of a big file
    /// rewrites only the units that part touched.
    pub(crate) fn add_file(
        &mut self,
        sub: &mut Submitter,
        disk: &Path,
        rel: String,
        stats: &mut AddStats,
    ) -> Result<()> {
        let meta = std::fs::metadata(disk)?;
        let idx = self.files.len();
        self.note_case_collision(&rel, stats);
        self.files.push(FileEntry {
            path: rel,
            size: meta.len(),
            mtime: mtime_of(&meta),
            extents: Vec::new(),
        });

        if meta.len() == 0 {
            stats.files += 1;
            return Ok(());
        }

        // Decide from the head bytes whether this file should share a unit.
        //
        // Grouping only pays when the data actually compresses: sharing lets
        // the compressor reuse what neighbouring files have in common. Data
        // that is already compressed — photos, video, archives — gains nothing
        // from a neighbour and loses two things by sharing: an identical copy
        // elsewhere no longer produces an identical unit, so it stops
        // deduplicating, and replacing one photo would rewrite every other
        // file that shares its unit. Those files get units of their own.
        // Files at least half a unit long are alone for the same reason.
        let mut whole: Option<Vec<u8>> = None;
        let mut handle: Option<File> = None;
        let mut head: Vec<u8> = Vec::new();
        if meta.len() < self.geom.chunked_from {
            let data =
                std::fs::read(disk).with_context(|| format!("cannot read {}", disk.display()))?;
            head.extend_from_slice(&data[..data.len().min(HEAD_SAMPLE)]);
            whole = Some(data);
        } else {
            let mut f =
                File::open(disk).with_context(|| format!("cannot read {}", disk.display()))?;
            narc_platform::lower_io_priority(&f);
            let head_len = HEAD_SAMPLE.min(meta.len() as usize);
            (&mut f).take(head_len as u64).read_to_end(&mut head)?;
            handle = Some(f);
        }
        // Only trust the verdict on a file large enough for it to mean
        // something: a 300-byte file rarely compresses on its own, but that
        // says nothing about how it compresses next to a thousand siblings.
        let plan = analyze::plan(&head, self.tier);
        let incompressible = meta.len() >= self.geom.chunked_from && plan.codec == Codec::Store;
        let alone = incompressible || meta.len() >= self.geom.unit / 2;

        // A unit gets ONE codec and ONE filter, so mixing kinds forfeits the
        // per-file choice the analyzer exists to make: on Silesia, whose files
        // are 5-51 MiB of wildly different data with no extensions to sort by,
        // mixing cost 8 MiB. But the verdict is only trusted for files big
        // enough to classify — judging a 300-byte file by its "kind" and
        // flushing on it shattered a source tree into 246 units of median
        // 1.4 KiB, which cost 1.8 MiB.
        let confident = meta.len() >= CLASSIFIABLE;
        let mixed = confident && self.kind.is_some_and(|k| k != plan.kind);
        if alone || mixed {
            self.flush(sub, stats)?;
        }
        if confident && self.kind.is_none() {
            self.kind = Some(plan.kind);
        }

        if let Some(data) = whole {
            // The size on disk may have changed since the metadata read; the
            // entry must describe what was actually stored.
            self.files[idx].size = data.len() as u64;
            stats.bytes_in += data.len() as u64;
            self.place(sub, idx, data, stats)?;
        } else {
            let f = handle.expect("large files keep their handle");
            let reader = std::io::Cursor::new(head).chain(BufReader::new(f));
            let mut actual = 0u64;
            for chunk in StreamCDC::new(
                reader,
                self.geom.chunk_min,
                self.geom.chunk_avg,
                self.geom.chunk_max,
            ) {
                let data = chunk
                    .with_context(|| format!("reading {}", disk.display()))?
                    .data;
                actual += data.len() as u64;
                stats.bytes_in += data.len() as u64;
                self.place(sub, idx, data, stats)?;
            }
            self.files[idx].size = actual;
        }
        if alone {
            self.flush(sub, stats)?;
        }
        stats.files += 1;
        Ok(())
    }

    /// Append one item (a whole small file, or one chunk of a big one) to the
    /// unit under construction.
    fn place(
        &mut self,
        sub: &mut Submitter,
        file: usize,
        data: Vec<u8>,
        stats: &mut AddStats,
    ) -> Result<()> {
        let target = self.target();
        // Keep units within twice the target: an item that would overshoot
        // starts a new unit instead, and an item larger than that becomes a
        // unit of its own.
        if !self.buf.is_empty() && self.buf.len() + data.len() > target * 2 {
            self.flush(sub, stats)?;
        }
        let off = self.buf.len() as u64;
        let len = data.len() as u64;
        let cut = blake3::hash(&data);
        self.buf.extend_from_slice(&data);
        self.pending.push((file, off, len));

        // End the unit here with probability len/target, so units average the
        // target size while the decision depends only on this item.
        let h = u64::from_le_bytes(cut.as_bytes()[..8].try_into().expect("blake3 is 32 bytes"));
        if h % (target as u64) < len || self.buf.len() >= target * 2 {
            self.flush(sub, stats)?;
        }
        Ok(())
    }

    /// Hand the assembled unit to the compression pipeline (or reuse an
    /// identical one already stored) and attach its extents to the files.
    pub(crate) fn flush(&mut self, sub: &mut Submitter, stats: &mut AddStats) -> Result<()> {
        if self.buf.is_empty() {
            self.pending.clear();
            return Ok(());
        }
        let buf = std::mem::take(&mut self.buf);
        self.kind = None;
        let mut key = [0u8; 16];
        key.copy_from_slice(&blake3::hash(&buf).as_bytes()[..16]);

        let unit = match self.dedup.get(&key) {
            Some(&idx) => {
                stats.bytes_deduped += buf.len() as u64;
                idx
            }
            None => {
                let plan = analyze::plan(&buf[..buf.len().min(HEAD_SAMPLE)], self.tier);
                let local = sub.submit_filtered(
                    buf,
                    key,
                    self.tier.candidates(plan.codec),
                    plan.filter.id(),
                )?;
                let idx = self
                    .base
                    .checked_add(local)
                    .context("archive unit count exceeds format limit")?;
                self.dedup.insert(key, idx);
                idx
            }
        };

        for (file, off, len) in self.pending.drain(..) {
            self.files[file].extents.push(Extent { unit, off, len });
        }
        Ok(())
    }

    /// Warn about entries that differ only in letter case: Windows and macOS
    /// cannot hold both, so one of them would be skipped on extract.
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

pub(crate) fn mtime_of(meta: &std::fs::Metadata) -> i64 {
    match meta.modified() {
        Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            // Pre-1970 timestamps are representable and worth keeping.
            Err(e) => -(e.duration().as_secs() as i64),
        },
        Err(_) => 0,
    }
}
