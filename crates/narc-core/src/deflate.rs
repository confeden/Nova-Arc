//! Finding and undoing the deflate streams inside a container.
//!
//! Deflate is the one already-compressed format worth reversing: it is what
//! zip, PNG, gzip, docx/xlsx, PDF, jar and apk are made of, and its output
//! still holds most of the redundancy of the original text — the encoder simply
//! could not see far enough. Undoing it, compressing the plaintext with LZMA2
//! and keeping a small record of how to rebuild the exact original bitstream
//! measured **−52.4%** on a corpus of zips, PNGs and gzips, against a figure
//! where 7-Zip and narc were tied because neither can do anything at all.
//!
//! Two rules govern everything here:
//! - **Bit-exact or nothing.** The rebuilt bytes must equal the original bytes.
//!   Every caller verifies before keeping the result, and falls back to storing
//!   the data as it came.
//! - **A stream is a list of pieces, not a range.** PNG splits one zlib stream
//!   across its IDAT chunks, so the deflate data is not contiguous in the file.

use anyhow::{bail, Result};

/// One embedded deflate stream: where its bytes live, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    pub pieces: Vec<(usize, usize)>,
}

impl Stream {
    /// The stream's bytes, gathered out of the container.
    pub fn gather(&self, file: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len());
        for &(off, len) in &self.pieces {
            out.extend_from_slice(&file[off..off + len]);
        }
        out
    }

    pub fn len(&self) -> usize {
        self.pieces.iter().map(|p| p.1).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Smallest stream worth transforming, MEASURED rather than guessed — the first
/// guess was 4096 and it cost 15 percentage points.
///
/// The intuition that a correction record cannot pay for itself on a 2 KB file
/// is right *per file* (a lone 2 KB .gz came out +2 to +7% worse) and wrong
/// inside a unit: there the plaintexts of a thousand small streams compress
/// against each other, and only the correction record — 0.45% of plaintext — is
/// overhead. On the deflate corpus, against what narc stores today:
/// floor 4096 −23.6% · 1024 −34.9% · 256 −38.4% · 64 −38.6% · 0 −38.6%.
const MIN_STREAM: usize = 64;

/// Locate the deflate streams in one buffer, which may hold several
/// concatenated files (a unit groups whole small files of the same kind).
pub fn find_streams(buf: &[u8]) -> Vec<Stream> {
    let mut out = Vec::new();
    let mut at = 0usize;
    // A unit is a concatenation of whole files, so keep scanning from wherever
    // the last container ended.
    while at < buf.len() {
        let rest = &buf[at..];
        let (streams, consumed) = if rest.starts_with(&[0x1F, 0x8B]) {
            gzip(rest)
        } else if rest.starts_with(b"\x89PNG\r\n\x1A\n") {
            png(rest)
        } else if rest.starts_with(b"PK\x03\x04") {
            zip(rest)
        } else if rest.starts_with(b"%PDF-") {
            pdf(rest)
        } else {
            (Vec::new(), 0)
        };
        if consumed == 0 {
            break;
        }
        out.extend(streams.into_iter().map(|s| Stream {
            pieces: s.pieces.iter().map(|&(o, l)| (o + at, l)).collect(),
        }));
        at += consumed;
    }
    out.retain(|s| s.len() >= MIN_STREAM);
    out
}

/// gzip: header, optional extra fields, bare deflate, then CRC32 + ISIZE.
fn gzip(f: &[u8]) -> (Vec<Stream>, usize) {
    if f.len() < 18 || f[2] != 8 {
        return (Vec::new(), 0);
    }
    let flags = f[3];
    let mut p = 10usize;
    if flags & 0x04 != 0 {
        // FEXTRA
        if p + 2 > f.len() {
            return (Vec::new(), 0);
        }
        let n = u16::from_le_bytes([f[p], f[p + 1]]) as usize;
        match p.checked_add(2 + n).filter(|&e| e <= f.len()) {
            Some(e) => p = e,
            None => return (Vec::new(), 0),
        }
    }
    for bit in [0x08u8, 0x10] {
        // FNAME, FCOMMENT: NUL-terminated
        if flags & bit != 0 {
            match f.get(p..).and_then(|r| r.iter().position(|&c| c == 0)) {
                Some(z) => p += z + 1,
                None => return (Vec::new(), 0),
            }
        }
    }
    if flags & 0x02 != 0 {
        // FHCRC
        match p.checked_add(2).filter(|&e| e <= f.len()) {
            Some(e) => p = e,
            None => return (Vec::new(), 0),
        }
    }
    let Some(end) = f.len().checked_sub(8) else {
        return (Vec::new(), 0);
    };
    if p >= end {
        return (Vec::new(), 0);
    }
    // A gzip member's length is only known by decoding it, so a concatenation
    // of gzip members is not scanned past the first one.
    (
        vec![Stream {
            pieces: vec![(p, end - p)],
        }],
        f.len(),
    )
}

/// PNG: the IDAT chunks concatenate into ONE zlib stream — 2 header bytes,
/// deflate, then a 4-byte adler32.
fn png(f: &[u8]) -> (Vec<Stream>, usize) {
    let mut pieces: Vec<(usize, usize)> = Vec::new();
    let mut p = 8usize;
    let mut end = f.len();
    while p + 12 <= f.len() {
        let len = u32::from_be_bytes([f[p], f[p + 1], f[p + 2], f[p + 3]]) as usize;
        let kind = &f[p + 4..p + 8];
        let data = p + 8;
        let Some(next) = data.checked_add(len).and_then(|e| e.checked_add(4)) else {
            return (Vec::new(), 0);
        };
        if next > f.len() {
            return (Vec::new(), 0);
        }
        if kind == b"IDAT" {
            pieces.push((data, len));
        }
        p = next;
        if kind == b"IEND" {
            end = p;
            break;
        }
    }
    if pieces.is_empty() {
        return (Vec::new(), end);
    }
    trim(&mut pieces, 2, 4);
    (vec![Stream { pieces }], end)
}

/// Drop `front` bytes from the start of a piece list and `back` from its end.
fn trim(pieces: &mut Vec<(usize, usize)>, front: usize, back: usize) {
    let mut left = front;
    while left > 0 && !pieces.is_empty() {
        let take = left.min(pieces[0].1);
        pieces[0].0 += take;
        pieces[0].1 -= take;
        left -= take;
        if pieces[0].1 == 0 {
            pieces.remove(0);
        }
    }
    let mut right = back;
    while right > 0 && !pieces.is_empty() {
        let last = pieces.len() - 1;
        let take = right.min(pieces[last].1);
        pieces[last].1 -= take;
        right -= take;
        if pieces[last].1 == 0 {
            pieces.pop();
        }
    }
}

/// zip: read the central directory rather than scanning for local headers, so
/// that an entry written with a data descriptor still gives a reliable size.
fn zip(f: &[u8]) -> (Vec<Stream>, usize) {
    // End-of-central-directory, searched from the back (a comment may follow).
    let Some(eocd) = (0..f.len().saturating_sub(21))
        .rev()
        .find(|&i| &f[i..i + 4] == b"PK\x05\x06")
    else {
        return (Vec::new(), 0);
    };
    let comment = u16::from_le_bytes([f[eocd + 20], f[eocd + 21]]) as usize;
    let end = (eocd + 22 + comment).min(f.len());
    let count = u16::from_le_bytes([f[eocd + 10], f[eocd + 11]]) as usize;
    let mut cd =
        u32::from_le_bytes([f[eocd + 16], f[eocd + 17], f[eocd + 18], f[eocd + 19]]) as usize;
    let mut out = Vec::new();
    for _ in 0..count {
        if cd + 46 > f.len() || &f[cd..cd + 4] != b"PK\x01\x02" {
            break;
        }
        let method = u16::from_le_bytes([f[cd + 10], f[cd + 11]]);
        let csize = u32::from_le_bytes([f[cd + 20], f[cd + 21], f[cd + 22], f[cd + 23]]) as usize;
        let name_len = u16::from_le_bytes([f[cd + 28], f[cd + 29]]) as usize;
        let extra_len = u16::from_le_bytes([f[cd + 30], f[cd + 31]]) as usize;
        let comment_len = u16::from_le_bytes([f[cd + 32], f[cd + 33]]) as usize;
        let lho = u32::from_le_bytes([f[cd + 42], f[cd + 43], f[cd + 44], f[cd + 45]]) as usize;
        cd += 46 + name_len + extra_len + comment_len;
        // 0xFFFFFFFF means the real value is in a ZIP64 extra field; those
        // entries are left alone rather than guessed at.
        if method != 8 || csize == 0 || csize == 0xFFFF_FFFF || lho == 0xFFFF_FFFF {
            continue;
        }
        if lho + 30 > f.len() || &f[lho..lho + 4] != b"PK\x03\x04" {
            continue;
        }
        // The local header repeats the name and extra lengths, and its extra
        // field may differ in length from the central one.
        let ln = u16::from_le_bytes([f[lho + 26], f[lho + 27]]) as usize;
        let le = u16::from_le_bytes([f[lho + 28], f[lho + 29]]) as usize;
        let start = lho + 30 + ln + le;
        if start.checked_add(csize).is_some_and(|e| e <= f.len()) {
            out.push(Stream {
                pieces: vec![(start, csize)],
            });
        }
    }
    (out, end)
}

// -- PDF ---------------------------------------------------------------------

/// PDF whitespace, per ISO 32000-1 §7.2.2. NUL and form feed count, which is
/// why this is not `is_ascii_whitespace`.
fn is_ws(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

fn find_at(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let end = hay.len().checked_sub(needle.len())?;
    (from.min(end + 1)..=end).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let end = hay.len().checked_sub(needle.len())?;
    (0..=end)
        .rev()
        .find(|&i| &hay[i..i + needle.len()] == needle)
}

/// How far back from a `stream` keyword its object dictionary may reach. An
/// image dictionary with a long `/ColorSpace` array is the worst realistic
/// case; missing the `/Filter` only costs the gain on that one stream.
const PDF_DICT_LOOKBACK: usize = 16 * 1024;

/// PDF: every object dictionary that says `/FlateDecode` introduces a deflate
/// stream, and a text-heavy PDF is almost entirely those — page content, the
/// object streams that hold the objects themselves, the cross-reference stream,
/// embedded font programs. 7-Zip and every other archiver leave all of it
/// alone, because to them a PDF is finished data.
///
/// This is a LEXICAL scan, not an object parse, and deliberately so. A real
/// parse means the cross-reference table, object streams, incremental updates
/// and every producer's deviations from the spec — and it would buy nothing,
/// because a stream's bytes are found the same way either way. Getting a range
/// wrong is not a correctness risk: preflate has to consume exactly the bytes
/// handed to it (`compressed_size != raw.len()` is rejected in `filters`), the
/// packer round-trips the whole unit, and either check falls back to storing.
fn pdf(f: &[u8]) -> (Vec<Stream>, usize) {
    let mut out = Vec::new();
    let mut at = 0usize;
    // Earliest byte the NEXT dictionary may start at: wherever the last
    // candidate left off. Without it the lookback is rescanned in full for
    // every candidate, and `%PDF-` followed by `>>stream` repeated makes that
    // ~4096 comparisons per input byte — measured at 2.9 s per MiB, against
    // 45 ms for 16 MiB of real PDF. A file of that shape is a legal
    // recompression candidate up to twice the unit target, so it pinned a
    // packing worker for minutes and produced nothing. With the floor the scan
    // is linear: each candidate only reads bytes no candidate has read before.
    // It is also STRICTER than the fixed window — a dictionary cannot begin
    // inside the stream that precedes it.
    while let Some(i) = find_at(f, at, b"stream") {
        let floor = at;
        at = i + 6;
        // `endstream` ends with the same six bytes.
        if i >= 3 && &f[i - 3..i] == b"end" {
            continue;
        }
        // A stream keyword follows its dictionary, so `>>` must be the last
        // non-space thing before it. This is what separates the keyword from
        // the word "stream" inside a name, a string or a comment.
        let mut j = i;
        while j > 0 && is_ws(f[j - 1]) {
            j -= 1;
        }
        if j < 2 || &f[j - 2..j] != b">>" {
            continue;
        }
        let lo = j.saturating_sub(PDF_DICT_LOOKBACK).max(floor.min(j));
        let mut dict = &f[lo..j];
        // Anchor at the object header so a `/Length` belonging to the previous
        // object cannot be read as this one's. `endobj` also ends in `obj`,
        // which is fine — either match is before this dictionary starts.
        if let Some(p) = rfind(dict, b"obj") {
            dict = &dict[p + 3..];
        }
        if !flate_first(dict) {
            continue;
        }
        // §7.3.8.1: the keyword is followed by CRLF or a single LF, never by a
        // lone CR. Tolerating one would shift every offset by a byte.
        let mut s = i + 6;
        match (f.get(s), f.get(s + 1)) {
            (Some(b'\r'), Some(b'\n')) => s += 2,
            (Some(b'\n'), _) => s += 1,
            _ => continue,
        }
        // `/Length` is authoritative when it is a direct integer and the
        // keyword it predicts really is there. When it is an indirect
        // reference — which resolving would need the whole cross-reference
        // machinery for — fall back to finding `endstream`, and accept that
        // deflate data containing those nine bytes ends up skipped rather than
        // mis-cut.
        let end = match dict_length(dict)
            .and_then(|n| s.checked_add(n))
            .filter(|&e| ends_stream(f, e))
        {
            Some(e) => e,
            None => match find_at(f, s, b"endstream") {
                Some(e) => trim_eol(f, s, e),
                // Nothing after this point ends a stream, so no later candidate
                // can either. Continuing would rescan to the end of the file for
                // every remaining `stream` keyword — quadratic on a file built
                // to be quadratic.
                None => break,
            },
        };
        let (body, body_end) = zlib_body(f, s, end);
        if body_end > body {
            out.push(Stream {
                pieces: vec![(body, body_end - body)],
            });
        }
        at = at.max(end);
    }
    (out, f.len())
}

/// Strip the zlib wrapper a PDF `/FlateDecode` stream carries.
///
/// This is the difference between the feature working and not working at all:
/// PDF's FlateDecode is RFC 1950, so the bytes on disk are two header bytes,
/// the deflate stream, and a four-byte adler32. Handed the wrapper, preflate
/// modelled 0 of 957 streams in a 7.27 MB corpus. The six bytes are not lost —
/// nothing covers them, so the framing keeps them verbatim.
///
/// The test is the spec's own: CM == 8 and the header pair read big-endian is
/// divisible by 31. A preset dictionary (FDICT) is left alone, because the
/// dictionary id sits between the header and the deflate data.
fn zlib_body(f: &[u8], s: usize, end: usize) -> (usize, usize) {
    let wrapped = end >= s + 8
        && f[s] & 0x0F == 8
        && f[s + 1] & 0x20 == 0
        && u16::from_be_bytes([f[s], f[s + 1]]).is_multiple_of(31);
    if wrapped {
        (s + 2, end - 4)
    } else {
        (s, end)
    }
}

/// True when the stream's OUTERMOST filter is FlateDecode — the only case
/// where the bytes on disk are a deflate stream. `/Filter [/ASCII85Decode
/// /FlateDecode]` applies ASCII85 last, so its raw bytes are text.
fn flate_first(dict: &[u8]) -> bool {
    let Some(p) = find_at(dict, 0, b"/Filter") else {
        return false;
    };
    let mut q = p + 7;
    // `/Filter/FlateDecode`, `/Filter [ /FlateDecode ]`, `/Filter[/FlateDecode]`
    while q < dict.len() && (is_ws(dict[q]) || dict[q] == b'[') {
        q += 1;
    }
    dict[q..].starts_with(b"/FlateDecode")
}

/// `/Length N` when it is a direct integer.
///
/// The whitespace is required, not cosmetic: `/Length1`, `/Length2` and
/// `/Length3` are the segment sizes of an embedded Type 1 font, and reading one
/// of those as the stream length would cut every font stream short.
fn dict_length(dict: &[u8]) -> Option<usize> {
    let mut at = 0usize;
    while let Some(p) = find_at(dict, at, b"/Length") {
        at = p + 7;
        let mut q = at;
        while q < dict.len() && is_ws(dict[q]) {
            q += 1;
        }
        if q == at {
            continue; // /Length1 and friends
        }
        let start = q;
        while q < dict.len() && dict[q].is_ascii_digit() {
            q += 1;
        }
        if q == start {
            continue;
        }
        // An indirect reference reads `/Length 12 0 R`: a second number means
        // this one is an object number, not a byte count.
        let mut r = q;
        while r < dict.len() && is_ws(dict[r]) {
            r += 1;
        }
        if r < dict.len() && dict[r].is_ascii_digit() {
            continue;
        }
        return std::str::from_utf8(&dict[start..q]).ok()?.parse().ok();
    }
    None
}

/// Does `endstream` follow this offset? Up to four bytes of whitespace may sit
/// in between; producers write none, one LF, or CRLF.
fn ends_stream(f: &[u8], at: usize) -> bool {
    if at > f.len() {
        return false;
    }
    let mut e = at;
    let limit = (at + 4).min(f.len());
    while e < limit && is_ws(f[e]) {
        e += 1;
    }
    f[e..].starts_with(b"endstream")
}

/// The EOL a producer wrote between the data and `endstream` is framing, not
/// stream data. Trimming a byte that was really the stream's makes preflate
/// report a shorter stream than it was handed, which is rejected — so this can
/// cost a gain but never a mis-decode.
fn trim_eol(f: &[u8], start: usize, mut end: usize) -> usize {
    if end > start && f[end - 1] == b'\n' {
        end -= 1;
    }
    if end > start && f[end - 1] == b'\r' {
        end -= 1;
    }
    end
}

// -- the reversible container ------------------------------------------------

/// Magic for the transformed form. Present so a corrupt or truncated payload is
/// rejected before it can drive an allocation.
const MAGIC: &[u8; 4] = b"NDf1";

fn put(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn get(inp: &mut &[u8]) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let Some((&b, rest)) = inp.split_first() else {
            bail!("truncated recompression header");
        };
        *inp = rest;
        v |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    bail!("malformed varint in recompression header")
}

/// What one transformed stream needs in order to be rebuilt.
pub struct Piece {
    pub pieces: Vec<(usize, usize)>,
    pub plain: Vec<u8>,
    pub corrections: Vec<u8>,
}

/// Lay out the transformed form: header, then the container's own bytes, then
/// every plaintext, then every correction record.
///
/// The grouping is deliberate. Plaintexts are the same kind of data as each
/// other and compress together; correction records are near-random and would
/// otherwise sit between them, cutting every match.
pub fn encode(original: &[u8], parts: &[Piece]) -> Result<Vec<u8>> {
    // Validated rather than assumed. The pieces come from this module's own
    // scanner, but a scanner bug must surface as a refusal and a fallback to
    // storing the data, never as an out-of-range slice inside a library that
    // is also handed hostile archives.
    let mut seen = 0usize;
    for p in parts {
        for &(off, len) in &p.pieces {
            if off < seen || off.checked_add(len).is_none_or(|e| e > original.len()) {
                bail!("recompression piece {off}+{len} is out of order or out of range");
            }
            seen = off + len;
        }
    }
    let mut header = Vec::new();
    header.extend_from_slice(MAGIC);
    put(&mut header, original.len() as u64);
    put(&mut header, parts.len() as u64);
    for p in parts {
        put(&mut header, p.pieces.len() as u64);
        for &(off, len) in &p.pieces {
            put(&mut header, off as u64);
            put(&mut header, len as u64);
        }
        put(&mut header, p.plain.len() as u64);
        put(&mut header, p.corrections.len() as u64);
    }

    let mut out = header;
    // The bytes no stream covers, in order: container headers, file names,
    // central directories, PNG chunk framing.
    let mut covered: Vec<(usize, usize)> = parts.iter().flat_map(|p| p.pieces.clone()).collect();
    covered.sort_unstable();
    let mut at = 0usize;
    for (off, len) in covered {
        if off > at {
            out.extend_from_slice(&original[at..off]);
        }
        at = at.max(off + len);
    }
    out.extend_from_slice(&original[at..]);
    for p in parts {
        out.extend_from_slice(&p.plain);
    }
    for p in parts {
        out.extend_from_slice(&p.corrections);
    }
    Ok(out)
}

/// Parsed transformed form, ready to be rebuilt. Every length is validated
/// against the buffer before anything is allocated, because on the decode path
/// this data is untrusted.
pub struct Decoded<'a> {
    pub original_len: usize,
    pub streams: Vec<Parsed<'a>>,
    pub verbatim: &'a [u8],
}

/// One stream as parsed back out of the transformed form: where its bytes
/// belong, and the two blobs a deflate encoder needs to reproduce them.
pub struct Parsed<'a> {
    pub pieces: Vec<(usize, usize)>,
    pub plain: &'a [u8],
    pub corrections: &'a [u8],
}

pub fn decode(buf: &[u8]) -> Result<Decoded<'_>> {
    let mut inp = buf;
    let Some((magic, rest)) = inp.split_at_checked(4) else {
        bail!("truncated recompression payload");
    };
    if magic != MAGIC {
        bail!("not a recompression payload");
    }
    inp = rest;
    let original_len = get(&mut inp)? as usize;
    // Every count and length here is attacker-controlled, so nothing may size
    // an allocation before it has been checked against what the buffer could
    // possibly describe. A forged header claiming 2^34 pieces asked for 16 GiB
    // and aborted the process — the check has to come first, not the capacity.
    //
    // The transformed form holds the container's own bytes plus every
    // plaintext, and deflate output is never meaningfully larger than its
    // input, so the original cannot be much bigger than this buffer.
    if original_len > buf.len().saturating_mul(2).saturating_add(4096) {
        bail!("implausible original length in recompression header");
    }
    // A stream costs at least three varints of header, a piece two.
    let count = get(&mut inp)? as usize;
    if count > inp.len() / 3 {
        bail!("implausible stream count in recompression header");
    }
    // NOT `with_capacity(count)`, and the distinction is the whole point of the
    // check above being a check rather than a clamp. One `meta` element is 40
    // bytes and its `Parsed` another 56, so a header may legally claim one
    // stream per three payload bytes and ask for ~32x the payload up front —
    // gigabytes, from three bytes of lying. Growing as records are actually
    // parsed ties the allocation to input that was really consumed.
    let mut meta = Vec::new();
    let mut covered = 0usize;
    for _ in 0..count {
        let n = get(&mut inp)? as usize;
        if n > inp.len() / 2 {
            bail!("implausible piece count");
        }
        // Nothing this module emits has a pieceless stream — `find_streams`
        // drops anything under MIN_STREAM — and allowing one would be a stream
        // record that costs three bytes and carries no data.
        if n == 0 {
            bail!("recompression stream with no pieces");
        }
        let mut pieces = Vec::with_capacity(n);
        for _ in 0..n {
            let off = get(&mut inp)? as usize;
            let len = get(&mut inp)? as usize;
            if off.checked_add(len).is_none_or(|e| e > original_len) {
                bail!("recompression piece outside the original");
            }
            covered += len;
            pieces.push((off, len));
        }
        let plain = get(&mut inp)? as usize;
        let corr = get(&mut inp)? as usize;
        meta.push((pieces, plain, corr));
    }
    if covered > original_len {
        bail!("recompression pieces overlap");
    }

    let body = inp;
    let verbatim_len = original_len - covered;
    let mut need = verbatim_len;
    for (_, plain, corr) in &meta {
        need = need
            .checked_add(*plain)
            .and_then(|n| n.checked_add(*corr))
            .ok_or_else(|| anyhow::anyhow!("recompression lengths overflow"))?;
    }
    if need != body.len() {
        bail!(
            "recompression payload is {} bytes, header describes {need}",
            body.len()
        );
    }
    let (verbatim, mut at) = body.split_at(verbatim_len);
    // Sized from what was PARSED, not from what the header claimed. By here
    // every record has been read and its lengths reconciled against the payload
    // size, so `meta.len()` is a fact rather than an assertion.
    let mut plains = Vec::with_capacity(meta.len());
    for (_, plain, _) in &meta {
        let (a, b) = at.split_at(*plain);
        plains.push(a);
        at = b;
    }
    let mut streams = Vec::with_capacity(plains.len());
    for ((pieces, _, corr), plain) in meta.into_iter().zip(plains) {
        let (a, b) = at.split_at(corr);
        at = b;
        streams.push(Parsed {
            pieces,
            plain,
            corrections: a,
        });
    }
    Ok(Decoded {
        original_len,
        streams,
        verbatim,
    })
}

/// Splice rebuilt streams back into the container's own bytes.
pub fn rebuild(d: &Decoded<'_>, rebuilt: &[Vec<u8>]) -> Result<Vec<u8>> {
    let mut spans: Vec<(usize, usize, usize)> = Vec::new(); // (offset, stream, piece)
    for (si, s) in d.streams.iter().enumerate() {
        for (pi, &(off, _)) in s.pieces.iter().enumerate() {
            spans.push((off, si, pi));
        }
    }
    spans.sort_unstable();

    let mut out = Vec::with_capacity(d.original_len);
    let mut verbatim = d.verbatim;
    let mut cursor = 0usize;
    // How much of each rebuilt stream has been placed, since its pieces are
    // written in file order.
    let mut used = vec![0usize; d.streams.len()];
    for (off, si, pi) in spans {
        if off < cursor {
            bail!("recompression pieces are not in order");
        }
        let gap = off - cursor;
        if gap > verbatim.len() {
            bail!("recompression payload is missing container bytes");
        }
        out.extend_from_slice(&verbatim[..gap]);
        verbatim = &verbatim[gap..];
        let len = d.streams[si].pieces[pi].1;
        let src = &rebuilt[si];
        let start = used[si];
        if start + len > src.len() {
            bail!("rebuilt stream is shorter than the original");
        }
        out.extend_from_slice(&src[start..start + len]);
        used[si] = start + len;
        cursor = off + len;
    }
    out.extend_from_slice(verbatim);
    if out.len() != d.original_len {
        bail!("rebuilt {} bytes, expected {}", out.len(), d.original_len);
    }
    for (i, u) in used.iter().enumerate() {
        if *u != rebuilt[i].len() {
            bail!(
                "rebuilt stream {i} has {} unused bytes",
                rebuilt[i].len() - u
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn framed(original: &[u8], parts: Vec<Piece>) -> Vec<u8> {
        encode(original, &parts).expect("valid pieces")
    }

    #[test]
    fn round_trips_the_container_framing() {
        // Two "streams" at known offsets inside a buffer, standing in for the
        // deflate data; the framing must put every byte back where it was.
        let original: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let parts = vec![
            Piece {
                pieces: vec![(10, 100)],
                plain: b"plain-one".to_vec(),
                corrections: b"corr1".to_vec(),
            },
            Piece {
                pieces: vec![(300, 50), (400, 50)],
                plain: b"plain-two".to_vec(),
                corrections: b"c2".to_vec(),
            },
        ];
        let enc = framed(&original, parts);
        let dec = decode(&enc).unwrap();
        assert_eq!(dec.original_len, 1000);
        assert_eq!(dec.streams.len(), 2);
        assert_eq!(dec.streams[0].plain, b"plain-one");
        assert_eq!(dec.streams[1].corrections, b"c2");
        // Pretend the codec rebuilt each stream exactly.
        let rebuilt = vec![original[10..110].to_vec(), {
            let mut v = original[300..350].to_vec();
            v.extend_from_slice(&original[400..450]);
            v
        }];
        assert_eq!(rebuild(&dec, &rebuilt).unwrap(), original);
    }

    #[test]
    fn rejects_a_truncated_or_forged_payload() {
        let original = vec![7u8; 500];
        let enc = framed(
            &original,
            vec![Piece {
                pieces: vec![(0, 100)],
                plain: vec![1, 2, 3],
                corrections: vec![4],
            }],
        );
        assert!(decode(&enc[..enc.len() - 1]).is_err(), "short payload");
        assert!(decode(&enc[..3]).is_err(), "no magic");
        let mut bad = enc.clone();
        bad[0] = b'X';
        assert!(decode(&bad).is_err(), "wrong magic");
        // A piece outside the original is refused at encode time rather than
        // slicing out of range; it used to abort the process trying to
        // allocate 64 GiB.
        assert!(
            encode(
                &original,
                &[Piece {
                    pieces: vec![(400, 200)],
                    plain: vec![],
                    corrections: vec![],
                }]
            )
            .is_err(),
            "piece outside the original"
        );
        // And out-of-order pieces, which `rebuild` could not splice back.
        assert!(
            encode(
                &original,
                &[Piece {
                    pieces: vec![(200, 50), (100, 50)],
                    plain: vec![],
                    corrections: vec![],
                }]
            )
            .is_err(),
            "pieces out of order"
        );
    }

    #[test]
    fn finds_the_deflate_stream_in_a_gzip_member() {
        // gzip header with no optional fields, 4 KiB of "deflate", trailer.
        let mut f = vec![0x1F, 0x8B, 0x08, 0x00, 0, 0, 0, 0, 0, 0xFF];
        f.extend(std::iter::repeat_n(0x5Au8, MIN_STREAM + 10));
        f.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let s = find_streams(&f);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].pieces, vec![(10, MIN_STREAM + 10)]);
    }

    #[test]
    fn ignores_data_with_no_container() {
        assert!(find_streams(b"just some text that is not a container").is_empty());
        assert!(find_streams(&[0u8; 10_000]).is_empty());
    }

    /// Build a PDF object holding one stream. `body` is the raw stream bytes,
    /// written exactly as given, so a test can hand it a zlib wrapper or not.
    fn pdf_obj(num: u32, dict: &str, body: &[u8], length: bool) -> Vec<u8> {
        let mut o = format!("{num} 0 obj\n<< {dict}").into_bytes();
        if length {
            o.extend_from_slice(format!(" /Length {}", body.len()).as_bytes());
        }
        o.extend_from_slice(b" >>\nstream\n");
        o.extend_from_slice(body);
        o.extend_from_slice(b"\nendstream\nendobj\n");
        o
    }

    /// A zlib wrapper around bytes that stand in for deflate output. The scanner
    /// must hand preflate the deflate part alone, so the two header bytes and
    /// the four trailer bytes have to come off.
    fn zlib(body: &[u8]) -> Vec<u8> {
        let mut v = vec![0x78, 0x9C];
        v.extend_from_slice(body);
        v.extend_from_slice(&[1, 2, 3, 4]);
        v
    }

    #[test]
    fn finds_the_flate_streams_in_a_pdf() {
        let payload = vec![0xABu8; MIN_STREAM + 40];
        let wrapped = zlib(&payload);
        let mut f = b"%PDF-1.7\n".to_vec();
        let first = f.len();
        f.extend(pdf_obj(1, "/Filter /FlateDecode", &wrapped, true));
        let second = f.len();
        f.extend(pdf_obj(
            2,
            "/Filter[/FlateDecode]/Type/ObjStm",
            &wrapped,
            true,
        ));
        // Not deflate on disk: ASCII85 is the outermost filter, so these bytes
        // are text and preflate would only waste time on them.
        f.extend(pdf_obj(
            3,
            "/Filter [/ASCII85Decode /FlateDecode]",
            &wrapped,
            true,
        ));
        // No filter at all.
        f.extend(pdf_obj(4, "/Type/Metadata", &wrapped, true));
        f.extend_from_slice(b"%%EOF\n");

        let s = find_streams(&f);
        assert_eq!(s.len(), 2, "only the two FlateDecode streams");
        // The offsets point PAST the zlib header, at the deflate data.
        let head = b"1 0 obj\n<< /Filter /FlateDecode /Length ".len();
        assert_eq!(s[0].len(), payload.len());
        assert_eq!(s[1].len(), payload.len());
        assert!(s[0].pieces[0].0 > first + head);
        assert!(s[1].pieces[0].0 > second);
        assert_eq!(&f[s[0].pieces[0].0..][..payload.len()], &payload[..]);
        assert_eq!(&f[s[1].pieces[0].0..][..payload.len()], &payload[..]);
    }

    /// `/Length1` is the first segment size of an embedded Type 1 font, not the
    /// stream's length. Reading it as one would cut every font stream short —
    /// silently, since a short stream is simply refused and stored.
    #[test]
    fn a_font_segment_size_is_not_the_stream_length() {
        let payload = vec![0x11u8; MIN_STREAM + 100];
        let wrapped = zlib(&payload);
        let mut f = b"%PDF-1.4\n".to_vec();
        f.extend(pdf_obj(
            1,
            &format!(
                "/Filter/FlateDecode /Length1 {} /Length2 0 /Length3 0 /Length {}",
                MIN_STREAM,
                wrapped.len()
            ),
            &wrapped,
            false,
        ));
        let s = find_streams(&f);
        assert_eq!(s.len(), 1);
        assert_eq!(
            s[0].len(),
            payload.len(),
            "read /Length1 instead of /Length"
        );
    }

    /// `/Length 9 0 R` is an object number, not a byte count; resolving it needs
    /// the cross-reference table. The scanner falls back to `endstream`.
    #[test]
    fn an_indirect_length_falls_back_to_the_endstream_keyword() {
        let payload = vec![0x22u8; MIN_STREAM + 7];
        let wrapped = zlib(&payload);
        let mut f = b"%PDF-1.5\n".to_vec();
        f.extend(pdf_obj(
            1,
            "/Filter/FlateDecode /Length 9 0 R",
            &wrapped,
            false,
        ));
        let s = find_streams(&f);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].len(), payload.len());
        assert_eq!(&f[s[0].pieces[0].0..][..payload.len()], &payload[..]);
    }

    /// The word `stream` appears in names, strings and comments; only the one
    /// that follows a dictionary is the keyword.
    /// A forged header may claim one stream per three payload bytes, and each
    /// costs ~96 bytes of parsed structure. Sizing the vectors from that claim
    /// turned three bytes of lying into a multi-gigabyte allocation.
    #[test]
    fn a_forged_stream_count_does_not_size_an_allocation() {
        let mut bad = MAGIC.to_vec();
        put(&mut bad, 1_000_000); // original_len
        put(&mut bad, 40_000_000); // stream count
        bad.resize(bad.len() + 4096, 0);
        // Refused for claiming more streams than the payload could describe —
        // and refused BEFORE anything is reserved.
        assert!(decode(&bad).is_err());
        // A stream with no pieces costs three bytes and carries nothing; it is
        // the cheapest way to buy structure, so it is rejected outright.
        let mut pieceless = MAGIC.to_vec();
        put(&mut pieceless, 100);
        put(&mut pieceless, 1);
        put(&mut pieceless, 0); // pieces
        put(&mut pieceless, 0); // plain
        put(&mut pieceless, 0); // corrections
        assert!(decode(&pieceless).is_err(), "pieceless stream");
    }

    #[test]
    fn only_a_dictionary_introduces_a_stream() {
        let mut f = b"%PDF-1.7\n% a comment about a stream\n".to_vec();
        f.extend_from_slice(b"5 0 obj\n(/Filter /FlateDecode is not a stream here)\nendobj\n");
        f.extend_from_slice(b"6 0 obj\n<< /Name /streamish >>\nendobj\n");
        assert!(find_streams(&f).is_empty());
    }

    #[test]
    fn a_tiny_stream_is_not_worth_a_correction_record() {
        let mut f = vec![0x1F, 0x8B, 0x08, 0x00, 0, 0, 0, 0, 0, 0xFF];
        f.extend(std::iter::repeat_n(0x5Au8, MIN_STREAM - 1));
        f.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(find_streams(&f).is_empty());
    }
}
