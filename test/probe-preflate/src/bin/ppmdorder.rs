//! Does PPMd7 have anything left above order 16?
//!
//! The tournament tries orders 10 and 16. The order travels in the chunk
//! record's `param` byte and the decoder already clamps it to 2..=64, so adding
//! orders costs no format change and no new id — the cheapest possible
//! experiment on the text side, where nova is weakest (+9.6% on enwik8).
//!
//! What it must not hide: PPMd7 is symmetric, so a higher order costs the same
//! again on DECODE, and this project defends decode speed explicitly. Both
//! times are reported.
//!
//!     cargo run --release --bin ppmdorder -- <file> [orders...]

use std::io::{Read, Write};
use std::time::Instant;

use ppmd_rust::{Ppmd7Decoder, Ppmd7Encoder};

/// The pool nova derives, copied here so the probe measures what the archiver
/// would actually do rather than a more generous configuration.
fn pool(len: usize) -> u32 {
    (len as u64).saturating_mul(32).clamp(1 << 20, 256 << 20) as u32
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: ppmdorder <file> [orders...]");
    let orders: Vec<u32> = args.filter_map(|a| a.parse().ok()).collect();
    let orders = if orders.is_empty() {
        vec![10, 16, 20, 24, 32, 40, 64]
    } else {
        orders
    };

    let data = std::fs::read(&path).expect("cannot read");
    // A unit, not a whole corpus: the tournament runs per unit and the model is
    // reset for each, so measuring a 100 MB file would answer a question nova
    // never asks.
    let unit = 32 << 20;
    let data = &data[..data.len().min(unit)];
    println!(
        "{path}: measuring the first {} B (one max-tier unit), pool {} B\n",
        data.len(),
        pool(data.len())
    );

    let mut best = (0u32, usize::MAX);
    println!(
        "{:>6}  {:>11}  {:>8}  {:>9}  {:>9}",
        "order", "bytes", "vs o16", "enc s", "dec s"
    );
    let mut o16 = 0usize;
    for &o in &orders {
        let t = Instant::now();
        let mut enc = Ppmd7Encoder::new(Vec::new(), o, pool(data.len())).expect("encoder");
        enc.write_all(data).unwrap();
        let packed = enc.finish(false).unwrap();
        let enc_s = t.elapsed().as_secs_f64();

        let t = Instant::now();
        let mut out = vec![0u8; data.len()];
        let mut dec = Ppmd7Decoder::new(packed.as_slice(), o, pool(data.len())).expect("decoder");
        dec.read_exact(&mut out).unwrap();
        let dec_s = t.elapsed().as_secs_f64();
        assert_eq!(out, data, "order {o} did not round-trip");

        if o == 16 {
            o16 = packed.len();
        }
        let delta = if o16 > 0 {
            format!("{:+.2}%", (packed.len() as f64 - o16 as f64) * 100.0 / o16 as f64)
        } else {
            "-".to_string()
        };
        println!(
            "{o:>6}  {:>11}  {delta:>8}  {enc_s:>9.1}  {dec_s:>9.1}",
            packed.len()
        );
        if packed.len() < best.1 {
            best = (o, packed.len());
        }
    }
    println!("\nbest: order {} at {} B", best.0, best.1);
}
