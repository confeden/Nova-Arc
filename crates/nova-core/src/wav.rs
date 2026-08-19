//! RIFF/WAVE PCM → FLAC, and back byte for byte.
//!
//! Uncompressed PCM is the one kind of audio every archiver still stores at
//! close to its raw size. LZMA2 with the record-width filter takes 16-bit
//! stereo music to about 60% of itself; FLAC's linear prediction takes the same
//! samples to 52%, because it models the waveform rather than looking for
//! repeats.
//!
//! WHAT MAKES THE ROUND TRIP EXACT is not FLAC — that only promises the
//! *samples* back — but the wrapper. A real .wav carries chunk order, odd
//! sizes with their pad bytes, `LIST`/`INFO` metadata, a `RIFF` size that does
//! not always agree with the file length, and sometimes trailing garbage. So
//! the transform stores THE WHOLE FILE with only the `data` payload cut out,
//! and rebuilds by splicing the decoded samples back into the hole. Nothing
//! about the container is reconstructed from a parse.
//!
//! ID 38 PINS THE DECODER, NOT THE ENCODER — unlike ids 34/35/37. What is
//! stored is a standard FLAC stream, so any conforming decoder reads it and the
//! encoder may be swapped or upgraded without spending a new id. The wrapper
//! format is this module's own and *is* pinned by the id.

use anyhow::{bail, ensure, Context, Result};

/// `NWv1` — the wrapper format, not the FLAC bitstream. See the module note on
/// what id 38 does and does not pin.
const MAGIC: &[u8; 4] = b"NWv1";
const HEADER: usize = 4 + 1 + 1 + 4 + 8 + 8 + 4;

/// WAVE format tags. 0xFFFE carries the real one in a GUID at the end of `fmt `.
const WAVE_PCM: u16 = 1;
const WAVE_EXTENSIBLE: u16 = 0xFFFE;

/// FLAC's own limits. A file outside them is refused, which is a fallback and
/// not an error — the packer stores it the ordinary way.
const MAX_CHANNELS: u16 = 8;
const MAX_SAMPLE_RATE: u32 = 655_350;

struct Fmt {
    channels: u16,
    sample_rate: u32,
    bits: u16,
}

impl Fmt {
    fn frame_bytes(&self) -> usize {
        self.channels as usize * (self.bits / 8) as usize
    }
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Is this the head of a RIFF/WAVE file? Twelve bytes is all it takes, and all
/// the packer has when it decides whether the file needs a unit of its own.
///
/// Deliberately optimistic: whether the audio is PCM this can encode is decided
/// later, from the `fmt ` chunk, and a WAVE that turns out to be ADPCM or float
/// just costs one refused transform.
pub fn is_wav(b: &[u8]) -> bool {
    b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE"
}

/// Walk the RIFF chunk list and return `fmt ` and the extent of `data`.
///
/// Chunks are word-aligned: an odd payload is followed by a pad byte that is
/// NOT counted in the declared size. The walk has to honour that or it drifts
/// one byte per odd chunk and reads the next id from the middle of a payload.
fn parse(data: &[u8]) -> Result<(Fmt, usize, usize)> {
    ensure!(is_wav(data), "not a RIFF/WAVE file");
    let mut fmt: Option<Fmt> = None;
    let mut pcm: Option<(usize, usize)> = None;
    let mut at = 12usize;
    while at + 8 <= data.len() {
        let id = &data[at..at + 4];
        let size = u32le(data, at + 4) as usize;
        let body = at + 8;
        // A declared size that runs past the end is either a truncated file or
        // trailing bytes that belong to no chunk — both are common in the wild.
        // Stop the walk rather than guess; whatever is left stays in the
        // wrapper verbatim, and if `data` was never reached the transform is
        // refused below, which is the truncated case.
        if body.saturating_add(size) > data.len() {
            break;
        }
        if id == b"fmt " && fmt.is_none() {
            ensure!(size >= 16, "fmt chunk is {size} bytes, needs 16");
            let f = &data[body..body + size];
            let mut tag = u16le(f, 0);
            if tag == WAVE_EXTENSIBLE {
                // WAVE_FORMAT_EXTENSIBLE: cbSize, then a 22-byte extension whose
                // last 16 bytes are a GUID beginning with the real format tag.
                ensure!(size >= 40, "extensible fmt is {size} bytes, needs 40");
                tag = u16le(f, 24);
            }
            fmt = Some(Fmt {
                channels: u16le(f, 2),
                sample_rate: u32le(f, 4),
                bits: u16le(f, 14),
            });
            ensure!(tag == WAVE_PCM, "audio format {tag} is not integer PCM");
        } else if id == b"data" && pcm.is_none() {
            pcm = Some((body, size));
        }
        // + the pad byte on an odd payload.
        at = body + size + (size & 1);
    }
    let fmt = fmt.context("no fmt chunk")?;
    let (off, len) = pcm.context("no data chunk")?;
    Ok((fmt, off, len))
}

/// Split interleaved little-endian PCM into the signed samples FLAC wants.
///
/// 8-bit WAV is UNSIGNED and is refused rather than shifted: it is rare, the
/// win on it is small, and a silent off-by-128 would round-trip every test
/// that only checks sizes.
fn to_samples(pcm: &[u8], bits: u16) -> Vec<i32> {
    match bits {
        16 => pcm
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as i32)
            .collect(),
        // Sign-extend the 24-bit two's-complement value into i32.
        _ => pcm
            .chunks_exact(3)
            .map(|c| {
                let v = (c[0] as u32) | ((c[1] as u32) << 8) | ((c[2] as u32) << 16);
                ((v << 8) as i32) >> 8
            })
            .collect(),
    }
}

fn from_samples(samples: &[i32], bits: u16, out: &mut Vec<u8>) {
    let n = (bits / 8) as usize;
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes()[..n]);
    }
}

/// Replace a .wav with `[header][wrapper][flac]`, where the wrapper is the file
/// with its `data` payload cut out.
pub fn encode(data: &[u8]) -> Result<Vec<u8>> {
    let (fmt, off, len) = parse(data)?;
    ensure!(
        fmt.bits == 16 || fmt.bits == 24,
        "{}-bit PCM is not transformed",
        fmt.bits
    );
    ensure!(
        (1..=MAX_CHANNELS).contains(&fmt.channels),
        "{} channels is outside FLAC's range",
        fmt.channels
    );
    ensure!(
        (1..=MAX_SAMPLE_RATE).contains(&fmt.sample_rate),
        "sample rate {} is outside FLAC's range",
        fmt.sample_rate
    );
    let frame = fmt.frame_bytes();
    ensure!(frame > 0, "fmt describes a zero-byte frame");
    ensure!(len > 0, "empty data chunk");
    // A partial frame at the end cannot become FLAC samples. Refusing keeps the
    // wrapper honest: everything it holds is either a whole frame or verbatim.
    ensure!(
        len.is_multiple_of(frame),
        "data chunk is {len} bytes, not a whole number of {frame}-byte frames"
    );

    let samples = to_samples(&data[off..off + len], fmt.bits);
    let flac = encode_flac(&samples, &fmt)?;

    let mut out = Vec::with_capacity(HEADER + (data.len() - len) + flac.len());
    out.extend_from_slice(MAGIC);
    out.push(fmt.bits as u8);
    out.push(fmt.channels as u8);
    out.extend_from_slice(&fmt.sample_rate.to_le_bytes());
    out.extend_from_slice(&(off as u64).to_le_bytes());
    out.extend_from_slice(&(len as u64).to_le_bytes());
    out.extend_from_slice(&((data.len() - len) as u32).to_le_bytes());
    out.extend_from_slice(&data[..off]);
    out.extend_from_slice(&data[off + len..]);
    out.extend_from_slice(&flac);
    // The transform only earns its id if it is smaller. `compress_job` would
    // fall back anyway, but refusing here saves the codec the trip.
    ensure!(out.len() < data.len(), "flac form is not smaller");
    Ok(out)
}

/// Rebuild the original .wav.
///
/// Every length here comes from the payload, so every length is checked before
/// it is used and nothing is reserved from a claimed count — a hostile header
/// must fail, not allocate.
pub fn decode(data: &[u8]) -> Result<Vec<u8>> {
    ensure!(data.len() >= HEADER, "wav record is too short");
    ensure!(&data[0..4] == MAGIC, "not a wav record");
    let bits = data[4] as u16;
    let channels = data[5] as u16;
    let sample_rate = u32le(data, 6);
    let off = u64::from_le_bytes(data[10..18].try_into().unwrap()) as usize;
    let pcm_len = u64::from_le_bytes(data[18..26].try_into().unwrap()) as usize;
    let wrapper_len = u32le(data, 26) as usize;
    ensure!(bits == 16 || bits == 24, "bad depth {bits} in wav record");
    ensure!(
        (1..=MAX_CHANNELS).contains(&channels),
        "bad channel count {channels} in wav record"
    );
    let end = HEADER
        .checked_add(wrapper_len)
        .context("wrapper length overflows")?;
    ensure!(end <= data.len(), "wrapper runs past the record");
    ensure!(off <= wrapper_len, "data offset is outside the wrapper");
    let frame = channels as usize * (bits / 8) as usize;
    ensure!(
        frame > 0 && pcm_len.is_multiple_of(frame),
        "pcm length {pcm_len} is not a whole number of frames"
    );

    let wrapper = &data[HEADER..end];
    let mut pcm = Vec::new();
    decode_flac(&data[end..], bits, channels, sample_rate, &mut pcm)?;
    // The claimed length is only ever CHECKED, never trusted to size anything.
    ensure!(
        pcm.len() == pcm_len,
        "flac stream holds {} bytes of pcm, the record claims {pcm_len}",
        pcm.len()
    );

    let mut out = Vec::with_capacity(wrapper.len() + pcm.len());
    out.extend_from_slice(&wrapper[..off]);
    out.append(&mut pcm);
    out.extend_from_slice(&wrapper[off..]);
    Ok(out)
}

fn encode_flac(samples: &[i32], fmt: &Fmt) -> Result<Vec<u8>> {
    use flacenc::component::BitRepr;
    use flacenc::config::Encoder as EncCfg;
    use flacenc::error::Verify;
    use flacenc::source::MemSource;

    // The encoder settings are NOT a format constant here — the payload is a
    // standard FLAC stream and the decoder is claxon, so they may change
    // without spending an id. They are fixed only so that output stays
    // byte-identical run to run, which the -j 1 == -j 8 requirement needs.
    let cfg = EncCfg::default()
        .into_verified()
        .map_err(|e| anyhow::anyhow!("flac config rejected: {e:?}"))?;
    let src = MemSource::from_samples(
        samples,
        fmt.channels as usize,
        fmt.bits as usize,
        fmt.sample_rate as usize,
    );
    let stream = flacenc::encode_with_fixed_block_size(&cfg, src, 4096)
        .map_err(|e| anyhow::anyhow!("flac encode failed: {e:?}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| anyhow::anyhow!("flac serialise failed: {e:?}"))?;
    Ok(sink.into_inner())
}

fn decode_flac(
    flac: &[u8],
    bits: u16,
    channels: u16,
    sample_rate: u32,
    out: &mut Vec<u8>,
) -> Result<()> {
    let mut r = claxon::FlacReader::new(std::io::Cursor::new(flac))
        .map_err(|e| anyhow::anyhow!("cannot open the flac stream: {e}"))?;
    let info = r.streaminfo();
    // The stream has to describe the same audio the wrapper does, or the
    // splice would produce a file that is the right length and the wrong sound.
    if info.bits_per_sample as u16 != bits
        || info.channels as u16 != channels
        || info.sample_rate != sample_rate
    {
        bail!(
            "flac stream is {}-bit {}ch {}Hz, the record says {bits}-bit {channels}ch {sample_rate}Hz",
            info.bits_per_sample,
            info.channels,
            info.sample_rate
        );
    }
    let mut buf = Vec::with_capacity(4096);
    for s in r.samples() {
        buf.push(s.map_err(|e| anyhow::anyhow!("corrupt flac stream: {e}"))?);
        if buf.len() == 4096 {
            from_samples(&buf, bits, out);
            buf.clear();
        }
    }
    from_samples(&buf, bits, out);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A .wav with the awkward parts real files have: a `LIST` chunk before
    /// `data`, an odd-sized chunk with its pad byte, an odd number of PCM
    /// bytes' worth of frames, a trailing chunk after `data`, and garbage at
    /// the end that belongs to no chunk at all.
    fn awkward_wav(frames: usize, channels: u16, bits: u16) -> Vec<u8> {
        let frame = channels as usize * (bits / 8) as usize;
        let mut pcm = Vec::with_capacity(frames * frame);
        let mut seed = 0x1234_5678_9ABC_DEF0u64;
        for i in 0..frames {
            for c in 0..channels {
                let t = i as f64 + c as f64 * 0.5;
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let wave = (t * 0.017).sin() * 12000.0 + ((seed >> 48) as i16 % 40) as f64;
                let v = wave as i32;
                pcm.extend_from_slice(&v.to_le_bytes()[..(bits / 8) as usize]);
            }
        }

        let chunk = |id: &[u8; 4], body: &[u8], out: &mut Vec<u8>| {
            out.extend_from_slice(id);
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(body);
            if body.len() % 2 == 1 {
                out.push(0);
            }
        };

        let mut fmt = Vec::new();
        fmt.extend_from_slice(&WAVE_PCM.to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&44100u32.to_le_bytes());
        fmt.extend_from_slice(&(44100 * frame as u32).to_le_bytes());
        fmt.extend_from_slice(&(frame as u16).to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());

        let mut body = Vec::new();
        chunk(b"fmt ", &fmt, &mut body);
        chunk(b"LIST", b"INFOINAModd", &mut body); // 11 bytes: pad byte follows
        chunk(b"data", &pcm, &mut body);
        chunk(b"id3 ", b"tail", &mut body);

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        // Deliberately NOT the true length: real encoders get this wrong and the
        // rebuilt file has to be wrong in exactly the same way.
        out.extend_from_slice(&((body.len() + 4 - 3) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(&body);
        out.extend_from_slice(b"\x00garbage past the last chunk");
        out
    }

    #[test]
    fn round_trips_byte_for_byte() {
        for (ch, bits) in [(2u16, 16u16), (1, 16), (2, 24), (6, 16)] {
            let wav = awkward_wav(9_001, ch, bits);
            let coded = encode(&wav).unwrap_or_else(|e| panic!("{ch}ch {bits}-bit: {e}"));
            assert!(
                coded.len() < wav.len(),
                "{ch}ch {bits}-bit: {} is not smaller than {}",
                coded.len(),
                wav.len()
            );
            let back = decode(&coded).unwrap();
            assert_eq!(back, wav, "{ch}ch {bits}-bit did not round-trip");
        }
    }

    /// Output must not depend on when it ran, or `-j 1` and `-j 8` stop
    /// producing the same archive.
    #[test]
    fn encoding_is_deterministic() {
        let wav = awkward_wav(5_000, 2, 16);
        assert_eq!(encode(&wav).unwrap(), encode(&wav).unwrap());
    }

    /// Every refusal is a fallback, so each of these must be an `Err` and not a
    /// panic or a silently wrong transform.
    #[test]
    fn refuses_what_it_cannot_rebuild() {
        // 8-bit is unsigned in WAV and signed in FLAC.
        assert!(encode(&awkward_wav(2_000, 2, 8)).is_err());
        // Not a RIFF at all.
        assert!(encode(b"not a wav at all, just some bytes").is_err());
        // A truncated file: the data chunk claims more than is there.
        let mut wav = awkward_wav(4_000, 2, 16);
        wav.truncate(wav.len() / 2);
        assert!(encode(&wav).is_err());
        // A WAVE with no PCM in it.
        let mut float = awkward_wav(4_000, 2, 16);
        let at = 12 + 8; // first byte of the fmt body
        float[at] = 3; // WAVE_FORMAT_IEEE_FLOAT
        assert!(encode(&float).is_err());
    }

    /// A corrupt record must fail, and must fail before it allocates from a
    /// length the payload made up.
    #[test]
    fn a_hostile_record_is_rejected() {
        let good = encode(&awkward_wav(3_000, 2, 16)).unwrap();
        assert!(decode(&good[..HEADER - 1]).is_err());
        let mut bad = good.clone();
        bad[0] = b'X';
        assert!(decode(&bad).is_err());
        // A wrapper longer than the record.
        let mut bad = good.clone();
        bad[26..30].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&bad).is_err());
        // A pcm length the flac stream cannot produce.
        let mut bad = good.clone();
        bad[18..26].copy_from_slice(&(1u64 << 40).to_le_bytes());
        assert!(decode(&bad).is_err());
        // A data offset past the end of the wrapper.
        let mut bad = good;
        bad[10..18].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode(&bad).is_err());
    }
}
