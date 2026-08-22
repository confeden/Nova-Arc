use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand, ValueEnum};
use nova_core::{AddStats, Archive, Overwrite, PackOptions, Tier};
use nova_platform::PriorityMode;

#[derive(Parser)]
#[command(
    name = "nova",
    version,
    author = "Brent - t.me/nova_txt",
    about = "Nova Prism - the NOVA archive format (v0, beta)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,

    /// Worker threads for packing (default: all logical cores). Extraction
    /// is I/O bound and stays single-threaded unless this is set.
    #[arg(short = 'j', long, global = true)]
    threads: Option<usize>,

    /// Memory budget, e.g. 512M or 2G (default: adapts to free RAM)
    #[arg(long, global = true, value_parser = parse_size)]
    memory: Option<u64>,

    /// Idle priority and EcoQoS: slowest, gentlest on laptops
    #[arg(long, global = true, conflicts_with = "full")]
    eco: bool,

    /// Normal priority, no throttling: benchmarks and idle machines
    #[arg(long, global = true)]
    full: bool,
}

/// Parse a size with an optional K/M/G suffix.
fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let (num, mult) = match t.chars().last() {
        Some('k') | Some('K') => (&t[..t.len() - 1], 1024),
        Some('m') | Some('M') => (&t[..t.len() - 1], 1024 * 1024),
        Some('g') | Some('G') => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        _ => (t, 1),
    };
    num.trim()
        .parse::<u64>()
        .map(|v| v * mult)
        .map_err(|_| format!("invalid size {s:?} (try 512M or 2G)"))
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
    /// Rename or move an entry or a whole folder inside the archive.
    /// Touches only the index - the data is never re-read or re-compressed.
    #[command(visible_alias = "mv")]
    Rename {
        archive: PathBuf,
        from: String,
        to: String,
    },
    /// Verify every stored byte against its checksum. Writes nothing
    #[command(visible_alias = "t")]
    Test { archive: PathBuf },
    /// Rewrite the archive dropping dead data
    Compact { archive: PathBuf },
    /// Show archive statistics
    Info {
        archive: PathBuf,
        /// Also dump one line per compression unit: size, codec, file types.
        /// Unit size and codec choice are what an archive's ratio is made of.
        #[arg(long)]
        units: bool,
    },
}

fn codec_name(c: u8) -> &'static str {
    match c {
        0 => "store",
        1 => "zstd",
        2 => "lzma2",
        3 => "ppmd7",
        4 => "bsc",
        _ => "unknown",
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

/// RAR extraction is an optional feature (see `foreign_rar`), so the two
/// entry points exist either way: with the feature they call it, without it
/// they say so. Detection itself is always on — the alternative is telling
/// someone their rar is "not a NOVA archive", which sounds like corruption.
#[cfg(feature = "rar")]
fn rar_list(archive: &Path) -> Result<Vec<(String, u64)>> {
    Ok(nova_core::foreign_rar::list(archive)?
        .into_iter()
        .map(|e| (e.path, e.size))
        .collect())
}

#[cfg(not(feature = "rar"))]
fn rar_list(archive: &Path) -> Result<Vec<(String, u64)>> {
    bail!(no_rar(archive))
}

#[cfg(feature = "rar")]
fn rar_extract(
    archive: &Path,
    output: &Path,
    sel: Option<&[String]>,
    policy: Overwrite,
) -> Result<nova_core::ExtractStats> {
    nova_core::foreign_rar::extract(archive, output, sel, policy)
}

#[cfg(not(feature = "rar"))]
fn rar_extract(
    archive: &Path,
    _output: &Path,
    _sel: Option<&[String]>,
    _policy: Overwrite,
) -> Result<nova_core::ExtractStats> {
    bail!(no_rar(archive))
}

/// Whether this build can read a rar at all. `add` needs it too: without it,
/// telling someone nova "can list and extract" their rar would be a lie.
const RAR_READABLE: bool = cfg!(feature = "rar");

#[allow(dead_code)] // used only by the no-feature arms above
fn no_rar(archive: &Path) -> String {
    format!(
        "{} is a RAR archive, and this build has no RAR support \
         (rebuild with --features rar)",
        archive.display()
    )
}

/// Shared by every foreign-format `list`: they carry only a path and a size,
/// unlike native `.nva` listing's extra "Stored" column, which has no
/// foreign-format analogue.
fn print_foreign_list(entries: impl IntoIterator<Item = (String, u64)>) {
    let mut total = 0u64;
    let mut count = 0usize;
    println!("{:>12}  Path", "Size");
    for (path, size) in entries {
        println!("{:>12}  {}", human(size), path);
        total += size;
        count += 1;
    }
    println!("{:>12}  {} file(s)", human(total), count);
}

/// Peak working set, so users (and benchmarks) can see that packing really
/// stays inside its memory budget.
/// Say it, loudly, before printing anything that looks like an inventory.
///
/// A damaged archive opens read-only and reports the last generation whose
/// manifest still decodes. That number can be zero files, and a bare "0
/// file(s)" reads as "this archive is empty" rather than "this archive lost
/// its index" -- which is how a flipped bit ended with the user being advised
/// to run `compact` on twelve megabytes of intact data.
fn warn_if_damaged(a: &Archive) {
    if let Some(d) = a.damage {
        eprintln!("warning: {d}");
        eprintln!(
            "warning: what follows is generation {}, NOT the archive's latest state. \
             Writing is refused; extract what you can and rebuild.",
            d.opened_generation
        );
    }
}

fn report_peak() {
    if let Some(p) = nova_platform::peak_memory() {
        println!("Peak RAM: {}", human(p));
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
    report_peak();
    for w in &s.warnings {
        eprintln!("warning: {w}");
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // All cores, but out of the user's way: below-normal CPU and memory
    // priority, low-priority bulk I/O, and a memory budget that adapts to
    // how loaded the machine already is.
    nova_platform::apply_process_policy(match (cli.eco, cli.full) {
        (true, _) => PriorityMode::Eco,
        (_, true) => PriorityMode::Full,
        _ => PriorityMode::Background,
    });
    let pack = |level: Level| PackOptions {
        tier: level.into(),
        threads: cli.threads.unwrap_or(0),
        memory_budget: cli.memory.unwrap_or(0),
    };
    match cli.cmd {
        Cmd::Create {
            archive,
            inputs,
            level,
        } => {
            let t = Instant::now();
            // The one command that cannot sniff its format: nothing has been
            // written yet, so the name is the only thing that can say what to
            // write. Everywhere else the bytes decide.
            let s = if archive
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
            {
                nova_core::foreign_zip::create(&archive, &inputs, level.into())?
            } else {
                let mut a = Archive::create(&archive)?;
                a.add_paths(&inputs, &pack(level))?
            };
            report_add(&s, t);
        }
        Cmd::Add {
            archive,
            inputs,
            level,
        } => {
            let t = Instant::now();
            // Without this the message would be "not a NOVA archive (bad
            // magic)", which reads as "unreadable" for a file nova lists and
            // extracts perfectly well.
            if nova_core::foreign_rar::sniff(&archive) && !RAR_READABLE {
                bail!(no_rar(&archive));
            }
            if nova_core::foreign_zip::sniff(&archive)
                || nova_core::foreign_7z::sniff(&archive)
                || nova_core::foreign_rar::sniff(&archive)
            {
                bail!(
                    "{} is a foreign archive - nova can list and extract it, \
                     but not add to it",
                    archive.display()
                );
            }
            let mut a = Archive::open_rw(&archive)?;
            let s = a.add_paths(&inputs, &pack(level))?;
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
            let mut damaged = false;
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
            let s = if nova_core::foreign_zip::sniff(&archive) {
                nova_core::foreign_zip::extract(&archive, &output, sel, policy)?
            } else if nova_core::foreign_7z::sniff(&archive) {
                nova_core::foreign_7z::extract(&archive, &output, sel, policy)?
            } else if nova_core::foreign_rar::sniff(&archive) {
                rar_extract(&archive, &output, sel, policy)?
            } else {
                let a = Archive::open_ro(&archive)?;
                // The same trap `list` fell into (G1): a damaged archive opens
                // at its last readable generation, which can be EMPTY, and
                // "Extracted 0 file(s)" then reads as "there was nothing in it"
                // instead of "this archive lost its index". Say it first, and
                // do not exit 0 on it.
                warn_if_damaged(&a);
                damaged = a.damage.is_some();
                a.extract_with(&output, sel, policy, &pack(Level::Normal))?
            };
            println!(
                "Extracted {} file(s), {} in {:.1}s",
                s.files,
                human(s.bytes),
                t.elapsed().as_secs_f64()
            );
            if s.skipped_existing > 0 {
                println!("Kept {} existing file(s)", s.skipped_existing);
            }
            report_peak();
            for w in &s.warnings {
                eprintln!("warning: {w}");
            }
            // Damage is reported after the count of what WAS recovered, because
            // that is the number someone with a rotting backup needs first.
            if !s.failed.is_empty() {
                eprintln!(
                    "
{} file(s) could not be recovered:",
                    s.failed.len()
                );
                for (path, why) in s.failed.iter().take(20) {
                    eprintln!("  {path}: {why}");
                }
                if s.failed.len() > 20 {
                    eprintln!("  ... and {} more", s.failed.len() - 20);
                }
                eprintln!("Run 'nova test' for the full picture.");
                std::process::exit(2);
            }
            if damaged {
                eprintln!(
                    "
The archive is damaged: what came out is its last readable generation, which may hold less than it once did."
                );
                std::process::exit(2);
            }
        }
        Cmd::List { archive } => {
            if nova_core::foreign_zip::sniff(&archive) {
                print_foreign_list(
                    nova_core::foreign_zip::list(&archive)?
                        .into_iter()
                        .map(|e| (e.path, e.size)),
                );
            } else if nova_core::foreign_7z::sniff(&archive) {
                print_foreign_list(
                    nova_core::foreign_7z::list(&archive)?
                        .into_iter()
                        .map(|e| (e.path, e.size)),
                );
            } else if nova_core::foreign_rar::sniff(&archive) {
                print_foreign_list(rar_list(&archive)?);
            } else {
                let a = Archive::open_ro(&archive)?;
                warn_if_damaged(&a);
                let mut total = 0u64;
                let mut stored = 0u64;
                let mut dirs = 0usize;
                println!("{:>12}  {:>12}  Path", "Size", "Stored");
                for f in &a.manifest.files {
                    // A folder is listed because an empty one is part of what
                    // was stored and would otherwise be invisible here.
                    if f.dir {
                        dirs += 1;
                        println!("{:>12}  {:>12}  {}/", "<DIR>", "-", f.path);
                        continue;
                    }
                    let st = a.stored_size(f);
                    println!("{:>12}  {:>12}  {}", human(f.size), human(st), f.path);
                    total += f.size;
                    stored += st;
                }
                println!(
                    "{:>12}  {:>12}  {} file(s), {} folder(s)",
                    human(total),
                    human(stored),
                    a.manifest.files.len() - dirs,
                    dirs
                );
            }
        }
        Cmd::Remove { archive, paths } => {
            let mut a = Archive::open_rw(&archive)?;
            let n = a.remove(&paths)?;
            if n == 0 {
                println!("Nothing matched.");
            } else {
                println!("Removed {n} item(s). Run 'nova compact' to reclaim space.");
            }
        }
        Cmd::Rename { archive, from, to } => {
            let t = Instant::now();
            let mut a = Archive::open_rw(&archive)?;
            let n = a.rename(&from, &to)?;
            println!(
                "Renamed {n} entr{} in {:.3}s (index only, no data rewritten)",
                if n == 1 { "y" } else { "ies" },
                t.elapsed().as_secs_f64()
            );
        }
        Cmd::Test { archive } => {
            let a = Archive::open_ro(&archive)?;
            warn_if_damaged(&a);
            let t = Instant::now();
            let s = a.test(&pack(Level::Normal), None)?;
            println!(
                "Checked {} of {} block(s), {} in {:.1?}",
                s.chunks_ok,
                s.chunks,
                human(s.bytes_ok),
                t.elapsed()
            );
            if s.bad.is_empty() {
                println!("OK - every stored byte matches its checksum.");
            } else {
                // The count first, then the detail: someone staring at a
                // half-readable backup needs the shape of the damage before
                // the reasons for it.
                eprintln!("\nDAMAGED - {} block(s) failed:", s.bad.len());
                for (idx, why) in s.bad.iter().take(10) {
                    eprintln!("  block {idx}: {why}");
                }
                if s.bad.len() > 10 {
                    eprintln!("  ... and {} more", s.bad.len() - 10);
                }
                eprintln!("\n{} file(s) affected:", s.damaged.len());
                for p in s.damaged.iter().take(20) {
                    eprintln!("  {p}");
                }
                if s.damaged.len() > 20 {
                    eprintln!("  ... and {} more", s.damaged.len() - 20);
                }
                // A distinct code, because "the archive is damaged" and "the
                // command could not run" are different things to a script.
                std::process::exit(2);
            }
        }
        Cmd::Compact { archive } => {
            let a = Archive::open_rw(&archive)?;
            let (before, after) = a.compact()?;
            println!("Compacted: {} -> {}", human(before), human(after));
        }
        Cmd::Info { archive, units } => {
            let a = Archive::open_ro(&archive)?;
            warn_if_damaged(&a);
            let i = a.info();
            println!("Generation:   {}", i.generation);
            println!("Files:        {}", i.files);
            println!("Folders:      {}", i.dirs);
            println!("Chunks:       {}", i.chunks);
            println!("Archive size: {}", human(i.file_len));
            println!("Live data:    {}", human(i.live_bytes));
            // "Reclaimable" means "space compact can safely return". On a
            // damaged archive the unreadable generation's bytes land in that
            // number too, and compact is exactly the command that would delete
            // them -- so the advice is withheld rather than printed.
            if a.damage.is_some() {
                println!(
                    "Unaccounted:  {} - NOT reclaimable, see the warning above",
                    human(i.reclaimable)
                );
            } else {
                println!(
                    "Reclaimable:  {} (run 'nova compact')",
                    human(i.reclaimable)
                );
            }
            if i.units > 0 {
                println!(
                    "Units:        {} (min {}, median {}, max {})",
                    i.units,
                    human(i.unit_min),
                    human(i.unit_median),
                    human(i.unit_max)
                );
            }
            if !i.by_codec.is_empty() {
                let parts: Vec<String> = i
                    .by_codec
                    .iter()
                    .map(|(c, b)| format!("{} {}", codec_name(*c), human(*b)))
                    .collect();
                println!("Stored by:    {}", parts.join(", "));
            }
            if units {
                println!("\n#idx\tunpacked\tpacked\tcodec\tparam\tfilter\tfiles\texts\ttop_ext");
                for u in a.units() {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        u.idx,
                        u.unpacked,
                        u.packed,
                        codec_name(u.codec),
                        u.param,
                        u.filter,
                        u.files,
                        u.distinct_exts,
                        u.top_ext
                    );
                }
            }
        }
    }
    Ok(())
}
