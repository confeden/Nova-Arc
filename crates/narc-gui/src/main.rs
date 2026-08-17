//! Nova Arc desktop app.
//!
//! The window is a thin shell: every archive operation runs in a worker
//! thread inside `narc-core`, streams progress back as Tauri events, and the
//! UI never blocks. Nothing here talks to the network — the application has
//! no telemetry, no analytics and no update pings by design.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use narc_core::{Archive, Overwrite, PackOptions, Progress, Tier};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

/// Temp directories created for "open file from archive"; removed when the
/// app exits so nothing is left behind.
#[derive(Default)]
struct TempDirs(Mutex<Vec<PathBuf>>);

#[derive(Serialize, Clone)]
struct Entry {
    path: String,
    size: u64,
    stored: u64,
    mtime: i64,
    solid: bool,
}

#[derive(Serialize, Clone)]
struct ArchiveInfo {
    path: String,
    generation: u64,
    files: usize,
    chunks: usize,
    file_len: u64,
    live_bytes: u64,
    reclaimable: u64,
    total_size: u64,
    entries: Vec<Entry>,
}

#[derive(Serialize, Clone)]
struct OpProgress {
    op: String,
    files_done: u64,
    files_total: u64,
    bytes_done: u64,
    bytes_total: u64,
}

#[derive(Serialize, Clone)]
struct OpResult {
    op: String,
    ok: bool,
    message: String,
    /// Human-readable detail lines for the status area.
    details: Vec<String>,
}

fn tier_from(level: &str) -> Tier {
    match level {
        "fast" => Tier::Fast,
        "max" => Tier::Max,
        _ => Tier::Normal,
    }
}

fn pack_options(level: &str, threads: Option<usize>, memory_mib: Option<u64>) -> PackOptions {
    PackOptions {
        tier: tier_from(level),
        threads: threads.unwrap_or(0),
        memory_budget: memory_mib.unwrap_or(0) * 1024 * 1024,
    }
}

/// Progress events are cheap but not free: at one event per small file a big
/// tree would flood the webview, so they are throttled to ~20/second worth of
/// work by only emitting when a percent of the total has moved.
struct Throttle {
    last: AtomicU64,
    step: u64,
}

impl Throttle {
    fn new(total: u64) -> Self {
        Throttle {
            last: AtomicU64::new(0),
            step: (total / 200).max(1),
        }
    }

    fn should_emit(&self, done: u64) -> bool {
        let last = self.last.load(Ordering::Relaxed);
        if done >= last + self.step {
            self.last.store(done, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}

fn emit_progress(app: &AppHandle, op: &str, p: Progress, throttle: &Throttle) {
    if !throttle.should_emit(p.bytes_done) {
        return;
    }
    let _ = app.emit(
        "narc://progress",
        OpProgress {
            op: op.to_string(),
            files_done: p.files_done,
            files_total: p.files_total,
            bytes_done: p.bytes_done,
            bytes_total: p.bytes_total,
        },
    );
}

fn finish(app: &AppHandle, op: &str, result: anyhow::Result<Vec<String>>) {
    let payload = match result {
        Ok(details) => OpResult {
            op: op.to_string(),
            ok: true,
            message: String::new(),
            details,
        },
        Err(e) => OpResult {
            op: op.to_string(),
            ok: false,
            // anyhow chains the context; the UI shows the whole chain because
            // "cannot write X" alone rarely explains why.
            message: format!("{e:#}"),
            details: Vec::new(),
        },
    };
    let _ = app.emit("narc://done", payload);
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

#[tauri::command]
fn open_archive(path: String) -> Result<ArchiveInfo, String> {
    let p = PathBuf::from(&path);
    let a = Archive::open_ro(&p).map_err(|e| format!("{e:#}"))?;
    let info = a.info();
    let entries: Vec<Entry> = a
        .manifest
        .files
        .iter()
        .map(|f| Entry {
            path: f.path.clone(),
            size: f.size,
            stored: a.stored_size(f),
            mtime: f.mtime,
            solid: f.block.is_some(),
        })
        .collect();
    Ok(ArchiveInfo {
        path,
        generation: info.generation,
        files: info.files,
        chunks: info.chunks,
        file_len: info.file_len,
        live_bytes: info.live_bytes,
        reclaimable: info.reclaimable,
        total_size: entries.iter().map(|e| e.size).sum(),
        entries,
    })
}

#[tauri::command]
fn create_archive(
    app: AppHandle,
    archive: String,
    inputs: Vec<String>,
    level: String,
    threads: Option<usize>,
    memory_mib: Option<u64>,
) {
    std::thread::spawn(move || {
        let opts = pack_options(&level, threads, memory_mib);
        let inputs: Vec<PathBuf> = inputs.into_iter().map(PathBuf::from).collect();
        let result = (|| -> anyhow::Result<Vec<String>> {
            let path = PathBuf::from(&archive);
            let mut a = if path.exists() {
                Archive::open_rw(&path)?
            } else {
                Archive::create(&path)?
            };
            let throttle = Throttle::new(1);
            let handle = app.clone();
            let stats = a.add_paths_with(
                &inputs,
                &opts,
                Some(&move |p: Progress| emit_progress(&handle, "create", p, &throttle)),
            )?;
            let mut details = vec![
                format!("Файлов: {}", stats.files),
                format!(
                    "Данных: {} → {} ({:.1}%)",
                    human(stats.bytes_in),
                    human(stats.bytes_stored),
                    if stats.bytes_in > 0 {
                        100.0 * stats.bytes_stored as f64 / stats.bytes_in as f64
                    } else {
                        0.0
                    }
                ),
            ];
            if stats.bytes_deduped > 0 {
                details.push(format!(
                    "Повторов не сохранено: {}",
                    human(stats.bytes_deduped)
                ));
            }
            details.extend(stats.warnings.iter().cloned());
            Ok(details)
        })();
        finish(&app, "create", result);
    });
}

#[tauri::command]
fn extract_archive(
    app: AppHandle,
    archive: String,
    dest: String,
    paths: Vec<String>,
    overwrite: String,
    threads: Option<usize>,
    memory_mib: Option<u64>,
) {
    std::thread::spawn(move || {
        let opts = pack_options("normal", threads, memory_mib);
        let policy = match overwrite.as_str() {
            "force" => Overwrite::Force,
            "skip" => Overwrite::Skip,
            _ => Overwrite::Fail,
        };
        let result = (|| -> anyhow::Result<Vec<String>> {
            let a = Archive::open_ro(Path::new(&archive))?;
            let sel = if paths.is_empty() {
                None
            } else {
                Some(paths.as_slice())
            };
            let throttle = Throttle::new(1);
            let handle = app.clone();
            let stats = a.extract_reporting(
                Path::new(&dest),
                sel,
                policy,
                &opts,
                Some(&move |p: Progress| emit_progress(&handle, "extract", p, &throttle)),
            )?;
            let mut details = vec![format!(
                "Распаковано {} файл(ов), {}",
                stats.files,
                human(stats.bytes)
            )];
            if stats.skipped_existing > 0 {
                details.push(format!(
                    "Пропущено существующих: {}",
                    stats.skipped_existing
                ));
            }
            details.extend(stats.warnings.iter().cloned());
            Ok(details)
        })();
        finish(&app, "extract", result);
    });
}

/// Extract one entry into a temp directory and hand it to the shell. The
/// directory is remembered so it can be removed when the app closes — the
/// user should never have to clean up after previewing a file.
#[tauri::command]
fn open_entry(
    app: AppHandle,
    temps: State<'_, Arc<TempDirs>>,
    archive: String,
    path: String,
) -> Result<String, String> {
    let run = || -> anyhow::Result<PathBuf> {
        let a = Archive::open_ro(Path::new(&archive))?;
        let dir = std::env::temp_dir().join(format!("nova-arc-{}", std::process::id()));
        let sub = dir.join(format!("{}", temps.0.lock().expect("temp mutex").len()));
        std::fs::create_dir_all(&sub)?;
        a.extract(&sub, Some(&[path.clone()]), Overwrite::Force)?;
        temps.0.lock().expect("temp mutex").push(sub.clone());
        Ok(sub.join(path.replace('/', std::path::MAIN_SEPARATOR_STR)))
    };
    let file = run().map_err(|e| format!("{e:#}"))?;
    tauri_plugin_opener::open_path(&file, None::<&str>).map_err(|e| e.to_string())?;
    let _ = app;
    Ok(file.to_string_lossy().to_string())
}

#[tauri::command]
fn compact_archive(app: AppHandle, archive: String) {
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<Vec<String>> {
            let a = Archive::open_rw(Path::new(&archive))?;
            let (before, after) = a.compact()?;
            Ok(vec![format!(
                "Архив сжат: {} → {}",
                human(before),
                human(after)
            )])
        })();
        finish(&app, "compact", result);
    });
}

#[tauri::command]
fn remove_entries(app: AppHandle, archive: String, paths: Vec<String>) {
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<Vec<String>> {
            let mut a = Archive::open_rw(Path::new(&archive))?;
            let n = a.remove(&paths)?;
            Ok(vec![format!(
                "Удалено записей: {n}. Освободить место — «Уплотнить»."
            )])
        })();
        finish(&app, "remove", result);
    });
}

#[tauri::command]
fn machine_info() -> serde_json::Value {
    let mem = narc_platform::memory_status();
    serde_json::json!({
        "cores": narc_platform::logical_cores(),
        "memory_total": mem.map(|m| m.total),
        "memory_available": mem.map(|m| m.available),
        "budget": narc_platform::memory_budget(None),
    })
}

fn main() {
    // Same policy as the CLI: all cores, but below-normal priority so the
    // desktop stays responsive while an archive is being built.
    narc_platform::apply_process_policy(narc_platform::PriorityMode::Background);

    let temps = Arc::new(TempDirs::default());
    let cleanup = temps.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(temps)
        .invoke_handler(tauri::generate_handler![
            open_archive,
            create_archive,
            extract_archive,
            open_entry,
            compact_archive,
            remove_entries,
            machine_info,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start Nova Arc")
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                for dir in cleanup.0.lock().expect("temp mutex").drain(..) {
                    let _ = std::fs::remove_dir_all(dir);
                }
            }
        });
}
