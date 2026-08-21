//! Decode FLAC to WAV, so building the public audio corpus needs no ffmpeg.
//!
//! `test/fetch-audio.py` downloads FLAC from Wikimedia Commons — that is what
//! is publicly linkable — but the corpus the WAV benchmark measures is PCM.
//! Every external decoder (ffmpeg, flac, sox) is one more thing a reader has
//! to install before they can check a published number, and claxon is already
//! a dependency here for filter 38.
//!
//! Deliberately NOT part of the library: this produces test input, it is not
//! an archiver feature. Decoding with claxon does not flatter nova either —
//! the WAV is just PCM, and filter 38 re-encodes it with flacenc and has to
//! reproduce the wrapper byte for byte regardless of who made the file.
//!
//! Usage: `cargo run --release --example flac2wav -- in.flac out.wav`

use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let [_, src, dst] = args.as_slice() else {
        eprintln!("usage: flac2wav <in.flac> <out.wav>");
        std::process::exit(2);
    };

    let mut flac = claxon::FlacReader::open(src)?;
    let info = flac.streaminfo();
    if !matches!(info.bits_per_sample, 8 | 16 | 24) {
        return Err(format!(
            "{} bits per sample is not a WAV width",
            info.bits_per_sample
        )
        .into());
    }
    let bytes_per_sample = (info.bits_per_sample / 8) as usize;
    let channels = info.channels as usize;
    let total = info
        .samples
        .ok_or("stream does not declare a sample count")?;
    // Interleaved frames × channels × width. Known up front, so the RIFF
    // header can be written first and the file streamed after it.
    let data_len = total as usize * channels * bytes_per_sample;

    let mut out = BufWriter::new(File::create(dst)?);
    write_header(&mut out, &info, data_len)?;

    let mut written = 0usize;
    for sample in flac.samples() {
        let s = sample?;
        // claxon hands back i32 sign-extended to the source width. WAV stores
        // 8-bit unsigned and everything wider signed little-endian, which is
        // the one asymmetry in the format worth remembering.
        match bytes_per_sample {
            1 => out.write_all(&[(s + 128) as u8])?,
            2 => out.write_all(&(s as i16).to_le_bytes())?,
            _ => out.write_all(&s.to_le_bytes()[..3])?,
        }
        written += bytes_per_sample;
    }
    if written != data_len {
        return Err(format!("declared {data_len} bytes of PCM, decoded {written}").into());
    }
    out.flush()?;
    Ok(())
}

fn write_header(
    out: &mut impl Write,
    info: &claxon::metadata::StreamInfo,
    data_len: usize,
) -> std::io::Result<()> {
    let channels = info.channels as u16;
    let rate = info.sample_rate;
    let bits = info.bits_per_sample as u16;
    let block_align = channels * bits / 8;
    let byte_rate = rate * block_align as u32;

    out.write_all(b"RIFF")?;
    out.write_all(&((36 + data_len) as u32).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&channels.to_le_bytes())?;
    out.write_all(&rate.to_le_bytes())?;
    out.write_all(&byte_rate.to_le_bytes())?;
    out.write_all(&block_align.to_le_bytes())?;
    out.write_all(&bits.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&(data_len as u32).to_le_bytes())?;
    Ok(())
}
