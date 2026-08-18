//! What a progress callback actually sees during an operation, on a real
//! timeline.
//!
//! Reading the code tells you which counter is reported; only running it tells
//! you *when*. This example exists because of a measured defect: the reading
//! reached 100% after 1.85 s of a 38.59 s max-tier pack, because it counted
//! bytes the reader had taken off disk rather than bytes that had reached the
//! archive. It is now the acceptance test for the fix — the gap between the last
//! reading and DONE is the thing to watch.
//!
//!   cargo run --release -p narc-core --example progress_probe -- <dir> [tier]

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use narc_core::{Archive, PackOptions, Phase, Progress, Tier};

struct Seen {
    events: u64,
    last_at: f64,
    max_gap: f64,
    gap_at: f64,
    first_full: Option<f64>,
    prev_done: u64,
    prev_files: u64,
    backwards: u64,
    phases: Vec<Phase>,
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().unwrap_or("test/corpus".into()));
    let tier = match args.next().unwrap_or("max".into()).as_str() {
        "fast" => Tier::Fast,
        "normal" => Tier::Normal,
        _ => Tier::Max,
    };
    let out = PathBuf::from(format!("test/progress-probe-{tier:?}.narc").to_lowercase());
    let _ = std::fs::remove_file(&out);

    let start = Instant::now();
    let seen = Mutex::new(Seen {
        events: 0,
        last_at: 0.0,
        max_gap: 0.0,
        gap_at: 0.0,
        first_full: None,
        prev_done: 0,
        prev_files: 0,
        backwards: 0,
        phases: Vec::new(),
    });

    println!(
        "{:>8} {:>7}  {:<7} {:>11} {:>11} {:>11} {:>7}",
        "elapsed", "gap", "phase", "done", "read", "stored", "pct"
    );
    let mut a = Archive::create(&out)?;
    let stats = a.add_paths_with(
        &[dir],
        &PackOptions::new(tier),
        Some(&|p: Progress| {
            let t = start.elapsed().as_secs_f64();
            let mut s = seen.lock().expect("probe mutex");
            let gap = t - s.last_at;
            if gap > s.max_gap {
                s.max_gap = gap;
                s.gap_at = t;
            }
            s.last_at = t;
            s.events += 1;
            if p.bytes_done < s.prev_done || p.files_done < s.prev_files {
                s.backwards += 1;
            }
            s.prev_done = p.bytes_done;
            s.prev_files = p.files_done;
            if p.bytes_total > 0 && p.bytes_done >= p.bytes_total && s.first_full.is_none() {
                s.first_full = Some(t);
            }
            if s.phases.last() != Some(&p.phase) {
                s.phases.push(p.phase);
            }
            let pct = if p.bytes_total > 0 {
                100.0 * p.bytes_done as f64 / p.bytes_total as f64
            } else {
                0.0
            };
            // Only print phase changes, big steps and the tail: a full log of
            // 500 readings hides the shape.
            let interesting = gap > 0.5 || p.phase != Phase::Work || pct >= 99.0;
            if interesting {
                println!(
                    "{t:8.2}s {gap:6.2}s  {:<7} {:>11} {:>11} {:>11} {pct:6.2}%",
                    format!("{:?}", p.phase).to_lowercase(),
                    p.bytes_done,
                    p.bytes_read,
                    p.bytes_stored,
                );
            }
        }),
    )?;
    let total = start.elapsed().as_secs_f64();
    let s = seen.into_inner().expect("probe mutex");
    println!(
        "{total:8.2}s  DONE: {} files, {} in, {} stored",
        stats.files, stats.bytes_in, stats.bytes_stored
    );
    println!("\nevents:            {}", s.events);
    println!("readings backwards: {} (must be 0)", s.backwards);
    println!(
        "first 100% at:     {}",
        s.first_full
            .map(|t| format!("{t:.2}s"))
            .unwrap_or("never".into())
    );
    println!(
        "silence after it:  {:.2}s (must be ~0)",
        s.first_full.map(|t| total - t).unwrap_or(total)
    );
    println!("largest gap:       {:.2}s at {:.2}s", s.max_gap, s.gap_at);
    println!("phases:            {:?}", s.phases);
    Ok(())
}
