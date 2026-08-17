use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use narc_core::{AddStats, Archive, Overwrite, Tier};

#[derive(Parser)]
#[command(
    name = "narc",
    version,
    about = "Nova Arc - the NARC archive format (v0 prototype)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(ValueEnum, Clone, Copy)]
enum Level {
    Fast,
    Normal,
    Max,
}

impl From<Level> for Tier {
    fn from(l: Level) -> Tier {
        match l {
            Level::Fast => Tier::Fast,
            Level::Normal => Tier::Normal,
            Level::Max => Tier::Max,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new archive from files/directories
    #[command(visible_alias = "c")]
    Create {
        archive: PathBuf,
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        #[arg(short, long, value_enum, default_value_t = Level::Normal)]
        level: Level,
    },
    /// Add files to an archive (append-only; same-path entries are replaced)
    #[command(visible_alias = "a")]
    Add {
        archive: PathBuf,
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        #[arg(short, long, value_enum, default_value_t = Level::Normal)]
        level: Level,
    },
    /// Extract files (all, or only the given archive paths / dir prefixes)
    #[command(visible_alias = "x")]
    Extract {
        archive: PathBuf,
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
        paths: Vec<String>,
        /// Overwrite files that already exist in the output directory
        #[arg(short, long, conflicts_with = "skip_existing")]
        force: bool,
        /// Keep files that already exist in the output directory
        #[arg(long)]
        skip_existing: bool,
    },
    /// List archive contents
    #[command(visible_alias = "l")]
    List { archive: PathBuf },
    /// Remove entries (append-only; run 'compact' to reclaim space)
    #[command(visible_alias = "rm")]
    Remove {
        archive: PathBuf,
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Rewrite the archive dropping dead data
    Compact { archive: PathBuf },
    /// Show archive statistics
    Info { archive: PathBuf },
}

/// Run below normal priority so heavy (de)compression never makes the
/// system laggy. Applies to the whole process and all worker threads.
fn lower_process_priority() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
        };
        unsafe {
            SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
        }
    }
}

fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn report_add(s: &AddStats, t: Instant) {
    println!(
        "Added {} file(s): {} in, {} stored ({} deduplicated) in {:.1}s",
        s.files,
        human(s.bytes_in),
        human(s.bytes_stored),
        human(s.bytes_deduped),
        t.elapsed().as_secs_f64()
    );
    if s.bytes_in > 0 {
        println!(
            "Ratio: {:.1}%",
            100.0 * s.bytes_stored as f64 / s.bytes_in as f64
        );
    }
    if s.symlinks_skipped > 0 {
        println!("Skipped {} symlink(s)", s.symlinks_skipped);
    }
    for w in &s.warnings {
        eprintln!("warning: {w}");
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    lower_process_priority();
    match cli.cmd {
        Cmd::Create {
            archive,
            inputs,
            level,
        } => {
            let t = Instant::now();
            let mut a = Archive::create(&archive)?;
            let s = a.add_paths(&inputs, level.into())?;
            report_add(&s, t);
        }
        Cmd::Add {
            archive,
            inputs,
            level,
        } => {
            let t = Instant::now();
            let mut a = Archive::open_rw(&archive)?;
            let s = a.add_paths(&inputs, level.into())?;
            report_add(&s, t);
        }
        Cmd::Extract {
            archive,
            output,
            paths,
            force,
            skip_existing,
        } => {
            let t = Instant::now();
            let a = Archive::open_ro(&archive)?;
            let sel = if paths.is_empty() {
                None
            } else {
                Some(paths.as_slice())
            };
            let policy = match (force, skip_existing) {
                (true, _) => Overwrite::Force,
                (_, true) => Overwrite::Skip,
                _ => Overwrite::Fail,
            };
            let s = a.extract(&output, sel, policy)?;
            println!(
                "Extracted {} file(s), {} in {:.1}s",
                s.files,
                human(s.bytes),
                t.elapsed().as_secs_f64()
            );
            if s.skipped_existing > 0 {
                println!("Kept {} existing file(s)", s.skipped_existing);
            }
            for w in &s.warnings {
                eprintln!("warning: {w}");
            }
        }
        Cmd::List { archive } => {
            let a = Archive::open_ro(&archive)?;
            let mut total = 0u64;
            let mut stored = 0u64;
            println!("{:>12}  {:>12}  Path", "Size", "Stored");
            for f in &a.manifest.files {
                let mut st: u64 = 0;
                for &i in &f.chunks {
                    st += a
                        .manifest
                        .chunks
                        .get(i as usize)
                        .context("corrupt manifest: chunk index out of range")?
                        .packed;
                }
                println!("{:>12}  {:>12}  {}", human(f.size), human(st), f.path);
                total += f.size;
                stored += st;
            }
            println!(
                "{:>12}  {:>12}  {} file(s)",
                human(total),
                human(stored),
                a.manifest.files.len()
            );
        }
        Cmd::Remove { archive, paths } => {
            let mut a = Archive::open_rw(&archive)?;
            let n = a.remove(&paths)?;
            if n == 0 {
                println!("Nothing matched.");
            } else {
                println!("Removed {n} item(s). Run 'narc compact' to reclaim space.");
            }
        }
        Cmd::Compact { archive } => {
            let a = Archive::open_rw(&archive)?;
            let (before, after) = a.compact()?;
            println!("Compacted: {} -> {}", human(before), human(after));
        }
        Cmd::Info { archive } => {
            let a = Archive::open_ro(&archive)?;
            let i = a.info();
            println!("Generation:   {}", i.generation);
            println!("Files:        {}", i.files);
            println!("Chunks:       {}", i.chunks);
            println!("Archive size: {}", human(i.file_len));
            println!("Live data:    {}", human(i.live_bytes));
            println!(
                "Reclaimable:  {} (run 'narc compact')",
                human(i.reclaimable)
            );
        }
    }
    Ok(())
}
