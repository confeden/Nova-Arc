//! Can a single oversized deflate stream be cut, and what is it worth?
//!
//! ROADMAP's Plans 1 recorded that it cannot: "preflate-rs only returns
//! reconstruction parameters for its FIRST chunk, so one lone stream cannot be
//! cut". That is wrong — `PreflateStreamProcessor` is a chunked API and
//! `PlainText::text()` returns only the bytes past the retained dictionary, so
//! the pieces concatenate into the original plaintext with no duplication.
//!
//! This measures the thing that decides the design. For a range of plaintext
//! budgets it reports: how much of the compressed stream a budget reaches, what
//! the transformed form costs, and — the number that matters — what LZMA2 makes
//! of the transformed prefix plus the untransformed tail, against storing the
//! whole file as it is today.
//!
//!     cargo run --release --bin gzchunk -- <file.gz> [budget_mib ...]

use std::io::Cursor;
use std::time::Instant;

use preflate_rs::{PreflateConfig, PreflateStreamProcessor, RecreateStreamProcessor};

/// Locate the deflate body with NOVA'S OWN SCANNER rather than a hand-rolled
/// gzip parse. Two reasons: the real header is not always ten bytes (binutils
/// carries FNAME, which is why the first version of this probe asserted its way
/// out), and a probe that measures a different stream than the packer would
/// find is measuring the wrong thing.
fn deflate_body(gz: &[u8]) -> (usize, usize) {
    let streams = nova_core::deflate::find_streams(gz);
    let s = streams
        .iter()
        .filter(|s| s.kind == nova_core::deflate::Kind::Deflate)
        .max_by_key(|s| s.pieces.iter().map(|p| p.1).sum::<usize>())
        .expect("no deflate stream found");
    assert_eq!(s.pieces.len(), 1, "this probe wants one contiguous stream");
    let (off, len) = s.pieces[0];
    (off, off + len)
}

struct Chunk {
    plain: Vec<u8>,
    corrections: Vec<u8>,
}

/// Walk the stream with a bounded plaintext per call, stopping when `budget`
/// total plaintext is reached. Returns the chunks and how many COMPRESSED bytes
/// they cover — the tail past that is left for the container to store verbatim.
fn split(body: &[u8], per_call: usize, budget: usize) -> (Vec<Chunk>, usize, bool) {
    let cfg = PreflateConfig {
        verify_compression: false,
        plain_text_limit: per_call,
        ..Default::default()
    };
    let mut proc = PreflateStreamProcessor::new(&cfg);
    let (mut consumed, mut plain_total) = (0usize, 0usize);
    let mut chunks = Vec::new();
    loop {
        let r = match proc.decompress(&body[consumed..]) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("    decompress stopped: {e:?}");
                break;
            }
        };
        // No forward progress means the next call would loop forever.
        if r.compressed_size == 0 {
            break;
        }
        let plain = proc.plain_text().text().to_vec();
        plain_total += plain.len();
        consumed += r.compressed_size;
        chunks.push(Chunk {
            plain,
            corrections: r.corrections,
        });
        if proc.is_done() {
            return (chunks, consumed, true);
        }
        if plain_total >= budget {
            break;
        }
        proc.shrink_to_dictionary();
    }
    (chunks, consumed, false)
}

fn lzma2(data: &[u8]) -> usize {
    use lzma_rust2::{Lzma2Options, Lzma2Writer};
    use std::io::Write;
    let mut opts = Lzma2Options::with_preset(6);
    opts.lzma_options.dict_size = 64 << 20;
    let mut w = Lzma2Writer::new(Vec::new(), opts);
    w.write_all(data).unwrap();
    w.finish().unwrap().len()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: gzchunk <file.gz> [budget_mib ...]");
    let budgets: Vec<usize> = args.filter_map(|a| a.parse().ok()).collect();
    let budgets = if budgets.is_empty() {
        vec![64usize, 128, 192, 200, 256, 4096]
    } else {
        budgets
    };

    let gz = std::fs::read(&path).expect("cannot read");
    let (body_at, body_end) = deflate_body(&gz);
    let body = &gz[body_at..body_end];
    println!(
        "{path}: {} B, deflate body {} B",
        gz.len(),
        body.len()
    );

    // What today's pipeline does with it: no transform at all, straight LZMA2.
    let t = Instant::now();
    let stored = lzma2(&gz);
    println!(
        "\nBASELINE (what ships today): lzma2 over the .gz as-is = {stored} B \
         ({:.1}% of {}) in {:.1} s\n",
        stored as f64 * 100.0 / gz.len() as f64,
        gz.len(),
        t.elapsed().as_secs_f64()
    );

    println!(
        "{:>8}  {:>6}  {:>12}  {:>12}  {:>6}  {:>13}  {:>7}  {:>6}",
        "budget", "chunks", "compressed", "plaintext", "reach", "lzma2(all)", "vs base", "secs"
    );
    for mib in budgets {
        let budget = mib << 20;
        let t = Instant::now();
        // 32 MiB per call keeps peak plaintext bounded while giving the
        // predictor long runs; the budget is what stops the walk.
        let pass: usize = std::env::var("PASS_KIB")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(|k: usize| k << 10)
            .unwrap_or(32 << 20);
        let (chunks, consumed, done) = split(body, pass, budget);
        if chunks.is_empty() {
            println!("{mib:>6} Mi  (nothing modelled)");
            continue;
        }
        let plain: usize = chunks.iter().map(|c| c.plain.len()).sum();
        let corr: usize = chunks.iter().map(|c| c.corrections.len()).sum();

        // Prove the prefix round-trips before believing any number from it.
        let mut rec = RecreateStreamProcessor::new();
        let mut back = Vec::with_capacity(consumed);
        for c in &chunks {
            let (bytes, _) = rec
                .recompress(&mut Cursor::new(&c.plain), &c.corrections)
                .expect("recompress failed");
            back.extend_from_slice(&bytes);
        }
        assert_eq!(
            back.as_slice(),
            &body[..consumed],
            "chunked round trip is not byte-exact at {mib} MiB"
        );

        // The transformed form, laid out the way the real filter does it:
        // container bytes (gzip wrapper + the untransformed compressed tail),
        // then every plaintext, then every correction record.
        let mut form = Vec::with_capacity(gz.len() + plain + corr);
        form.extend_from_slice(&gz[..body_at]);
        form.extend_from_slice(&body[consumed..]);
        form.extend_from_slice(&gz[body_end..]);
        for c in &chunks {
            form.extend_from_slice(&c.plain);
        }
        for c in &chunks {
            form.extend_from_slice(&c.corrections);
        }
        let packed = lzma2(&form);
        println!(
            "{mib:>6} Mi  {:>6}  {:>12}  {:>12}  {:>5.1}%  {:>13}  {:>+6.1}%  {:>6.1}{}",
            chunks.len(),
            consumed,
            plain,
            consumed as f64 * 100.0 / body.len() as f64,
            packed,
            (packed as f64 - stored as f64) * 100.0 / stored as f64,
            t.elapsed().as_secs_f64(),
            if done { "  [whole stream]" } else { "" }
        );
    }
}
