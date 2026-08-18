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
//! Unit boundaries are content-defined: *where* a unit ends is decided by the
//! hash and length of the item just added. A rule that cut *at* an accumulated
//! size moved every later boundary as soon as one file changed length, so
//! re-saving a tree after a one-line edit rewrote every unit — measured at
//! 17 MiB of growth. Now only the edited file's own unit changes and the rest
//! deduplicate away. The accumulated size appears in two places only, and both
//! are gates rather than triggers: a hard flush at twice the target, and a
//! refusal to cut below half of it (see `place`).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use fastcdc::v2020::StreamCDC;

use crate::analyze::{self, Kind, Tier};
use crate::archive::{group_key, AddStats, HEAD_SAMPLE};
use crate::codec::Codec;
use crate::manifest::{Extent, FileEntry, Geometry};
use crate::paths;
use crate::pipeline::Submitter;

/// Below this size a file's content class is guesswork, so it is not allowed
/// to decide anything: it simply joins whatever unit is open.
const CLASSIFIABLE: u64 = 4096;

/// Smallest deflate container worth a unit of its own. Recompression needs the
/// container whole and alone, and a unit per 600-byte icon is the failure mode
/// that once shattered a source tree into 246 units and cost 1.8 MiB.
const MIN_CONTAINER: u64 = 64 * 1024;

/// Why a unit ended.
///
/// Unit size is the biggest ratio lever there is, so when a measurement says
/// the units came out too small, the next question is always *which rule cut
/// them* — a content-defined boundary means the geometry is working as
/// designed, while a class change or a solo file means something else is
/// shattering units before they reach the target. Sizes alone cannot tell
/// those apart, so the reason is recorded.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Cut {
    /// Content-defined boundary: the hash of the item just added said stop.
    Hash,
    /// The unit was already at twice the target.
    Cap,
    /// The next item would have pushed it past twice the target.
    Overshoot,
    /// The next file is a different content class, which a unit may not mix.
    Class,
    /// A shared unit closed early because the next file takes one of its own.
    BeforeSolo,
    /// The unit is one large or incompressible file, alone by design.
    Solo,
    /// End of input.
    End,
}

/// Per-unit log, enabled by `NARC_UNIT_TRACE=<path>` and absent otherwise.
///
/// Everything about a finished unit except *why it ended* can be read back out
/// of the manifest; that one field only exists while the packer is running, so
/// this is the only place it can be captured.
struct Trace(File);

impl Trace {
    fn open() -> Option<Trace> {
        let path = std::env::var_os("NARC_UNIT_TRACE")?;
        File::create(path).ok().map(Trace)
    }

    fn unit(&mut self, u: TraceUnit<'_>) {
        let _ = writeln!(
            self.0,
            "{}\t{}\t{}\t{:?}\t{}\t{}\t{}",
            u.idx,
            u.len,
            u.items,
            u.cut,
            u.kind.map_or("-".into(), |k| format!("{k:?}")),
            u.deduped as u8,
            u.exts,
        );
    }
}

struct TraceUnit<'a> {
    idx: u32,
    len: usize,
    items: usize,
    cut: Cut,
    kind: Option<Kind>,
    deduped: bool,
    exts: &'a str,
}

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
    /// Bytes behind each per-file verdict in the unit under construction,
    /// keyed by (codec, filter id). See `unit_plan`.
    votes: HashMap<(Codec, u8), u64>,
    /// The verdict for the file being read, so every chunk of it can vote.
    /// A vote cast once per FILE only reached the unit that happened to be open
    /// when the file started: measured on Firefox, 150 MB of a 176 MB xul.dll
    /// landed in units with no BCJ filter at all, because only the first of them
    /// ever heard that the file was an executable.
    current: Option<(Codec, u8)>,
    /// Diagnostics only; `None` unless `NARC_UNIT_TRACE` is set.
    trace: Option<Trace>,
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
            votes: HashMap::new(),
            current: None,
            trace: Trace::open(),
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
        // A deflate container has to reach the filter WHOLE: a FastCDC chunk is
        // a fragment of a deflate stream, and preflate cannot start mid-stream.
        // The magic is enough to decide — a zip's central directory is at the
        // end of the file, so nothing that scans a head could tell.
        let mut peek = [0u8; 8];
        // Same for a JPEG: lepton reads one whole file, not a fragment of one.
        let recompressible = meta.len() >= MIN_CONTAINER
            && meta.len() <= self.geom.unit * 2
            && File::open(disk)
                .and_then(|mut f| f.read(&mut peek))
                .map(|n| {
                    analyze::is_deflate_container(&peek[..n]) || analyze::is_jpeg(&peek[..n])
                })
                .unwrap_or(false);
        let deflate_container = recompressible;

        let mut whole: Option<Vec<u8>> = None;
        let mut handle: Option<File> = None;
        let mut head: Vec<u8> = Vec::new();
        if meta.len() < self.geom.chunked_from || deflate_container {
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
        let mut plan = analyze::plan(&head, self.tier);
        // A container too small to be alone cannot be recompressed: the scanner
        // must see exactly one container, so a shared unit would hand it a
        // concatenation. Put it back on the ordinary already-compressed path.
        if matches!(plan.kind, analyze::Class::Deflate | analyze::Class::Jpeg) && !recompressible {
            plan = analyze::plan_precompressed(&head, self.tier);
        }
        let incompressible = meta.len() >= self.geom.chunked_from && plan.codec == Codec::Store;
        // A deflate container is alone in its unit, and that is not a
        // compromise: the scanner has to see exactly one container, because a
        // zip's central directory is found by searching BACKWARDS from the end
        // of the buffer — in a concatenation it would find the last archive's
        // directory and read it as the first one's. The measured −38.6% was
        // taken per file, so nothing is lost.
        let alone = incompressible || meta.len() >= self.geom.unit / 2 || deflate_container;

        // A unit gets ONE codec and ONE filter, so mixing kinds forfeits the
        // per-file choice the analyzer exists to make: on Silesia, whose files
        // are 5-51 MiB of wildly different data with no extensions to sort by,
        // mixing cost 8 MiB. But the verdict is only trusted for files big
        // enough to classify — judging a 300-byte file by its "kind" and
        // flushing on it shattered a source tree into 246 units of median
        // 1.4 KiB, which cost 1.8 MiB.
        // A class change ends a unit at ANY size, deliberately. Letting it slide
        // until the unit was half the target looked like a 171 KB win when the
        // alternative was measured with one fixed codec — and cost 1.0 MB at max
        // in the real packer, because a mixed unit forfeits both the codec
        // tournament and the filter. See Negative knowledge in ROADMAP.
        let confident = meta.len() >= CLASSIFIABLE;
        let mixed = confident && self.kind.is_some_and(|k| k != plan.kind);
        if alone || mixed {
            self.flush(sub, stats, if alone { Cut::BeforeSolo } else { Cut::Class })?;
        }
        if confident && self.kind.is_none() {
            self.kind = Some(plan.kind);
        }
        // This file's verdict votes for how the unit gets compressed, weighted
        // by length — but the vote is cast per CHUNK, in `place`, so that every
        // unit a large file spans hears it. See `unit_plan`.
        self.current = confident.then_some((plan.codec, plan.filter.id()));

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
            self.flush(sub, stats, Cut::Solo)?;
        }
        self.current = None;
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
            self.flush(sub, stats, Cut::Overshoot)?;
        }
        if let Some(key) = self.current {
            *self.votes.entry(key).or_default() += data.len() as u64;
        }
        let off = self.buf.len() as u64;
        let len = data.len() as u64;
        let cut = blake3::hash(&data);
        self.buf.extend_from_slice(&data);
        self.pending.push((file, off, len));

        // End the unit here with probability len/target, so units average the
        // target size while the decision depends only on this item.
        //
        // That makes unit sizes exponentially distributed, and the low tail of
        // that distribution is pure loss: a unit that ends at half the target
        // had cross-file redundancy still in front of it that it never got to
        // use. MEASURED on test/corpus: one such early cut fell inside a run of
        // 10 near-duplicate .exe builds and split it 16.7 + 7.0 MB; the same
        // bytes as one unit are 2,171,026 B against 2,587,061 B split, so that
        // single cut cost 416 KB — 54% of the whole remaining gap to 7-Zip.
        // Refusing to cut below half the target truncates the tail.
        //
        // This reads the accumulated size, which the content-defined rule
        // exists to avoid, but only as a gate: the cut point is still chosen by
        // the item's own hash, and the firing item is normally far past the
        // gate, so a file changing length moves a boundary only in the rare
        // case where the two nearly coincide. A rule that cut *at* the
        // accumulated target moved every later boundary instead — 17 MiB of
        // growth for a one-line edit.
        let h = u64::from_le_bytes(cut.as_bytes()[..8].try_into().expect("blake3 is 32 bytes"));
        let by_hash = self.buf.len() >= target / 2 && h % (target as u64) < len;
        if by_hash || self.buf.len() >= target * 2 {
            self.flush(sub, stats, if by_hash { Cut::Hash } else { Cut::Cap })?;
        }
        Ok(())
    }

    /// Hand the assembled unit to the compression pipeline (or reuse an
    /// identical one already stored) and attach its extents to the files.
    pub(crate) fn flush(
        &mut self,
        sub: &mut Submitter,
        stats: &mut AddStats,
        cut: Cut,
    ) -> Result<()> {
        if self.buf.is_empty() {
            self.pending.clear();
            return Ok(());
        }
        let buf = std::mem::take(&mut self.buf);
        let kind = self.kind.take();
        let traced = self
            .trace
            .is_some()
            .then(|| (buf.len(), self.pending.len(), self.ext_summary()));
        let mut key = [0u8; 16];
        key.copy_from_slice(&blake3::hash(&buf).as_bytes()[..16]);

        let mut deduped = false;
        let unit = match self.dedup.get(&key) {
            Some(&idx) => {
                stats.bytes_deduped += buf.len() as u64;
                deduped = true;
                idx
            }
            None => {
                let (codec, filter) = self.unit_plan(&buf);
                let local = sub.submit_filtered(buf, key, self.tier.candidates(codec), filter)?;
                let idx = self
                    .base
                    .checked_add(local)
                    .context("archive unit count exceeds format limit")?;
                self.dedup.insert(key, idx);
                idx
            }
        };
        self.votes.clear();

        if let (Some((len, items, exts)), Some(t)) = (traced, self.trace.as_mut()) {
            t.unit(TraceUnit {
                idx: unit,
                len,
                items,
                cut,
                kind,
                deduped,
                exts: &exts,
            });
        }

        for (file, off, len) in self.pending.drain(..) {
            self.files[file].extents.push(Extent { unit, off, len });
        }
        Ok(())
    }

    /// What to compress this unit with: the codec to hand the tournament, and
    /// the filter to apply first.
    ///
    /// The verdict has to come from the bytes that are in the unit, not from
    /// its first 64 KiB. MEASURED: at the fast tier one 8.36 MB unit of 484
    /// files — 235 `.go`, 203 `.h` — opened with a single sub-4 KiB `.flac`,
    /// because the extension sort puts `.flac` before `.go`. The head sample
    /// said "already compressed", so all 8.36 MB of source was stored raw and
    /// the archive grew 5.1 MB. So every file large enough to classify votes
    /// with its length and the majority of bytes wins. Only a unit with no
    /// voters at all — a continuation of one large file, or nothing but
    /// sub-4 KiB files — falls back to reading the head.
    fn unit_plan(&self, buf: &[u8]) -> (Codec, u8) {
        // Ties go to the lower codec id for a stable, reproducible archive.
        let top = self
            .votes
            .iter()
            .max_by_key(|(&(codec, filter), &bytes)| (bytes, !codec.id(), !filter));
        match top {
            Some((&(codec, filter), _)) => (codec, filter),
            None => {
                let plan = analyze::plan(&buf[..buf.len().min(HEAD_SAMPLE)], self.tier);
                (plan.codec, plan.filter.id())
            }
        }
    }

    /// Which file types this unit holds, most-frequent first — the check that
    /// the extension sort is actually keeping like next to like.
    fn ext_summary(&self) -> String {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for &(file, _, _) in &self.pending {
            *counts.entry(group_key(&self.files[file].path)).or_default() += 1;
        }
        let mut by_count: Vec<(String, usize)> = counts.into_iter().collect();
        by_count.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let head: Vec<String> = by_count
            .iter()
            .take(4)
            .map(|(e, n)| format!("{}:{n}", if e.is_empty() { "-" } else { e }))
            .collect();
        format!("{} {}", by_count.len(), head.join(","))
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
