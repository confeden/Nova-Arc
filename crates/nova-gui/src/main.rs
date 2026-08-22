//! Nova Prism desktop app.
//!
//! The window is a thin shell: every archive operation runs in a worker
//! thread inside `nova-core`, streams progress back as Tauri events, and the
//! UI never blocks. Nothing here talks to the network — the application has
//! no telemetry, no analytics and no update pings by design.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nova_core::{Archive, Overwrite, PackOptions, Phase, Progress, Tier};
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
    /// A stored folder, not a file. It is sent so that an EMPTY folder still
    /// appears in the tree — the tree is otherwise derived from file paths,
    /// which by definition cannot show a folder with nothing in it.
    dir: bool,
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
    /// "scan" | "work" | "drain" | "commit" | "done" — a byte count cannot
    /// explain the tail of a pack, where no source bytes move at all.
    phase: &'static str,
    files_done: u64,
    files_total: u64,
    /// Source bytes whose work is finished. Reaches the total exactly once.
    bytes_done: u64,
    bytes_total: u64,
    /// Source bytes read ahead; the gap to `bytes_done` is what is inside the
    /// compressors right now, which is how the UI tells work from a hang.
    bytes_read: u64,
    bytes_stored: u64,
    /// Blocks finished / handed to the compressors. At the max tier a whole
    /// tree is a handful of blocks, so this is what explains a long wait.
    units_done: u64,
    units_total: u64,
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

/// Forward one reading to the webview.
///
/// There is no throttle here any more. There used to be one, constructed with a
/// total of `1`, which made its step one byte and passed every event through —
/// all 5751 of them on a source tree. Throttling belongs where the totals are
/// known, so it now lives in nova-core and this side just forwards.
fn emit_progress(app: &AppHandle, op: &str, p: Progress) {
    let _ = app.emit(
        "nova://progress",
        OpProgress {
            op: op.to_string(),
            phase: match p.phase {
                Phase::Scan => "scan",
                Phase::Work => "work",
                Phase::Drain => "drain",
                Phase::Commit => "commit",
                Phase::Done => "done",
            },
            files_done: p.files_done,
            files_total: p.files_total,
            bytes_done: p.bytes_done,
            bytes_total: p.bytes_total,
            bytes_read: p.bytes_read,
            bytes_stored: p.bytes_stored,
            units_done: p.units_done,
            units_total: p.units_total,
        },
    );
}

/// Tell the webview an operation is running but cannot be measured — `compact`
/// and `remove` report nothing, and a bar frozen at zero for the minutes a
/// compact takes is indistinguishable from a hang.
fn emit_indeterminate(app: &AppHandle, op: &str) {
    let _ = app.emit(
        "nova://progress",
        OpProgress {
            op: op.to_string(),
            phase: "work",
            files_done: 0,
            files_total: 0,
            bytes_done: 0,
            bytes_total: 0,
            bytes_read: 0,
            bytes_stored: 0,
            units_done: 0,
            units_total: 0,
        },
    );
}

/// Guarantees the webview hears about the end of an operation even if the
/// worker thread panics.
///
/// `finish` is the only thing that emits `nova://done`, and it is the last
/// statement of each spawned closure — so any panic inside nova-core skipped it
/// entirely, `setBusy(false)` was never called, and every button stayed disabled
/// for the rest of the session with the bar frozen. That is the same picture as
/// the progress bug and no amount of progress reporting fixes it.
struct DoneGuard {
    app: AppHandle,
    op: &'static str,
    armed: bool,
}

impl DoneGuard {
    fn new(app: AppHandle, op: &'static str) -> Self {
        DoneGuard {
            app,
            op,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DoneGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.app.emit(
                "nova://done",
                OpResult {
                    op: self.op.to_string(),
                    ok: false,
                    message: "операция прервана из-за внутренней ошибки".into(),
                    details: Vec::new(),
                },
            );
        }
    }
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
    let _ = app.emit("nova://done", payload);
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
            dir: f.dir,
            // A file that shares its compression unit with other files, or
            // spans several, is worth flagging: extracting it decompresses
            // more than the file itself.
            //
            // Matched on the slice rather than indexed. `extents[0]` panicked
            // on the FIRST entry of any archive that stores folders, because a
            // directory carries no bytes and its extent list is empty -- and
            // folders only started being stored in 0.9.0, so nothing had ever
            // reached this line with an empty list before.
            solid: match f.extents.as_slice() {
                // A folder, or an empty file: no bytes, so nothing is shared.
                [] => false,
                // One unit, shared if it holds more than just this file.
                [e] => a
                    .manifest
                    .chunks
                    .get(e.unit as usize)
                    .is_some_and(|u| u.unpacked != f.size),
                // Several units: extracting it touches every one of them.
                _ => true,
            },
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
        let mut guard = DoneGuard::new(app.clone(), "create");
        let opts = pack_options(&level, threads, memory_mib);
        let inputs: Vec<PathBuf> = inputs.into_iter().map(PathBuf::from).collect();
        let result = (|| -> anyhow::Result<Vec<String>> {
            let path = PathBuf::from(&archive);
            let mut a = if path.exists() {
                Archive::open_rw(&path)?
            } else {
                Archive::create(&path)?
            };
            let handle = app.clone();
            let stats = a.add_paths_with(
                &inputs,
                &opts,
                Some(&move |p: Progress| emit_progress(&handle, "create", p)),
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
        guard.disarm();
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
        let mut guard = DoneGuard::new(app.clone(), "extract");
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
            let handle = app.clone();
            let stats = a.extract_reporting(
                Path::new(&dest),
                sel,
                policy,
                &opts,
                Some(&move |p: Progress| emit_progress(&handle, "extract", p)),
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
            // Damage is listed after what WAS recovered: with a rotting backup
            // the first thing anyone needs is how much of it survived.
            if !stats.failed.is_empty() {
                details.push(format!(
                    "НЕ УДАЛОСЬ восстановить файлов: {}. Остальное распаковано.",
                    stats.failed.len()
                ));
                details.extend(
                    stats
                        .failed
                        .iter()
                        .take(50)
                        .map(|(path, why)| format!("  {path}: {why}")),
                );
                if stats.failed.len() > 50 {
                    details.push(format!("  … и ещё {}", stats.failed.len() - 50));
                }
            }
            Ok(details)
        })();
        guard.disarm();
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
        let dir = std::env::temp_dir().join(format!("nova-prism-{}", std::process::id()));
        let sub = dir.join(format!("{}", temps.0.lock().expect("temp mutex").len()));
        std::fs::create_dir_all(&sub)?;
        a.extract(&sub, Some(std::slice::from_ref(&path)), Overwrite::Force)?;
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
        let mut guard = DoneGuard::new(app.clone(), "compact");
        // Neither operation can report a fraction; say so instead of
        // leaving an empty bar that looks like a hang.
        emit_indeterminate(&app, "compact");
        let result = (|| -> anyhow::Result<Vec<String>> {
            let a = Archive::open_rw(Path::new(&archive))?;
            let (before, after) = a.compact()?;
            Ok(vec![format!(
                "Архив сжат: {} → {}",
                human(before),
                human(after)
            )])
        })();
        guard.disarm();
        finish(&app, "compact", result);
    });
}

/// Read every stored byte back and check it against its checksum.
///
/// The answer a user wants is "is my archive still good", and before this the
/// only way to get it was to extract everything somewhere and look. It writes
/// nothing, so it is safe to run on anything, including an archive that is
/// already known to be damaged.
#[tauri::command]
fn test_archive(app: AppHandle, archive: String) {
    std::thread::spawn(move || {
        let mut guard = DoneGuard::new(app.clone(), "test");
        let result = (|| -> anyhow::Result<Vec<String>> {
            let a = Archive::open_ro(Path::new(&archive))?;
            let opts = PackOptions::new(Tier::Normal);
            let app_for_progress = app.clone();
            let s = a.test(
                &opts,
                Some(&move |p: Progress| emit_progress(&app_for_progress, "test", p)),
            )?;
            if s.bad.is_empty() {
                return Ok(vec![format!(
                    "Проверено {} блок(ов), {} — всё совпадает с контрольными суммами.",
                    s.chunks,
                    human(s.bytes_ok)
                )]);
            }
            // A damaged archive is not an error in the command: the command did
            // exactly what it was asked. The list is the result.
            let mut out = vec![format!(
                "ПОВРЕЖДЁН: {} из {} блок(ов) не прошли проверку. Затронуто файлов: {}.",
                s.bad.len(),
                s.chunks,
                s.damaged.len()
            )];
            out.extend(s.damaged.iter().take(50).map(|p| format!("  {p}")));
            if s.damaged.len() > 50 {
                out.push(format!("  … и ещё {}", s.damaged.len() - 50));
            }
            out.push(String::from(
                "Остальные файлы читаются — их можно распаковать как обычно.",
            ));
            Ok(out)
        })();
        guard.disarm();
        finish(&app, "test", result);
    });
}

#[tauri::command]
fn remove_entries(app: AppHandle, archive: String, paths: Vec<String>) {
    std::thread::spawn(move || {
        let mut guard = DoneGuard::new(app.clone(), "remove");
        // Neither operation can report a fraction; say so instead of
        // leaving an empty bar that looks like a hang.
        emit_indeterminate(&app, "remove");
        let result = (|| -> anyhow::Result<Vec<String>> {
            let mut a = Archive::open_rw(Path::new(&archive))?;
            let n = a.remove(&paths)?;
            Ok(vec![format!(
                "Удалено записей: {n}. Освободить место — «Уплотнить»."
            )])
        })();
        guard.disarm();
        finish(&app, "remove", result);
    });
}

/// What the command line asked for, if anything.
///
/// Explorer's context menu is the only caller that passes a verb; a bare path
/// is what a double-click sends, and that stays the default.
#[derive(Serialize, Clone, Default)]
struct StartupTask {
    /// "open" | "compress" | "test" | "extract-here" | "extract-into"
    verb: String,
    paths: Vec<String>,
}

struct Startup(Option<StartupTask>);

/// Pick the archive out of the command line.
///
/// A single quoted path is the normal case, but callers routinely forget the
/// quotes and a path like `D:\Nova Prism\a.nva` then arrives as two arguments,
/// so the whole tail is tried as one path before giving up.
fn task_from_args(args: Vec<String>) -> Option<StartupTask> {
    let verb = match args.first().map(String::as_str) {
        Some("--compress") => "compress",
        Some("--test") => "test",
        Some("--extract-here") => "extract-here",
        Some("--extract-into") => "extract-into",
        _ => "open",
    };
    let rest: Vec<String> = if verb == "open" {
        args
    } else {
        args.into_iter().skip(1).collect()
    };
    let existing: Vec<String> = rest
        .iter()
        .filter(|a| Path::new(a.as_str()).exists())
        .cloned()
        .collect();
    let paths = if existing.is_empty() {
        let joined = rest.join(" ");
        if Path::new(&joined).exists() {
            vec![joined]
        } else {
            Vec::new()
        }
    } else {
        existing
    };
    if paths.is_empty() {
        return None;
    }
    // A bare path that is not an archive is nothing to open, and guessing that
    // it must have meant "compress" would be a surprise, not a convenience.
    if verb == "open" && !paths[0].to_lowercase().ends_with(".nva") {
        return None;
    }
    Some(StartupTask {
        verb: verb.to_string(),
        paths,
    })
}

#[tauri::command]
fn startup_task(startup: State<'_, Startup>) -> Option<StartupTask> {
    startup.0.clone()
}

#[tauri::command]
fn machine_info() -> serde_json::Value {
    let mem = nova_platform::memory_status();
    serde_json::json!({
        "cores": nova_platform::logical_cores(),
        "memory_total": mem.map(|m| m.total),
        "memory_available": mem.map(|m| m.available),
        "budget": nova_platform::memory_budget(None),
    })
}

fn main() {
    // Same policy as the CLI: all cores, but below-normal priority so the
    // desktop stays responsive while an archive is being built.
    nova_platform::apply_process_policy(nova_platform::PriorityMode::Background);

    let temps = Arc::new(TempDirs::default());
    let cleanup = temps.clone();

    // A path on the command line (double-clicking a .nva, or the shell
    // association) opens that archive on startup. The frontend pulls it once
    // it is ready, rather than us pushing an event that could arrive before
    // its listeners exist.
    let startup = task_from_args(std::env::args().skip(1).collect());

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(temps)
        .manage(Startup(startup))
        .invoke_handler(tauri::generate_handler![
            open_archive,
            create_archive,
            extract_archive,
            open_entry,
            compact_archive,
            test_archive,
            remove_entries,
            machine_info,
            startup_task,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start Nova Prism")
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                for dir in cleanup.0.lock().expect("temp mutex").drain(..) {
                    let _ = std::fs::remove_dir_all(dir);
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::task_from_args;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// Opening an archive that stores a FOLDER used to panic, and a panic in a
    /// Tauri command takes the whole window with it -- the reported symptom was
    /// "it opens and dies two seconds later".
    ///
    /// A folder carries no bytes, so its extent list is empty, and the "is this
    /// file sharing its unit" check indexed `extents[0]` unconditionally.
    /// Nothing had ever hit it, because folders only started being stored in
    /// 0.9.0. The folder is normally the first entry, so this was every archive
    /// packed from a directory.
    #[test]
    fn an_archive_that_stores_a_folder_opens() {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let tree = tmp.path().join("tree");
        std::fs::create_dir_all(tree.join("sub")).expect("a tree");
        std::fs::write(tree.join("sub").join("a.txt"), b"hello").expect("a file");
        // An empty file has no extents either, for the same reason.
        std::fs::write(tree.join("empty.bin"), b"").expect("an empty file");
        std::fs::create_dir(tree.join("nothing-here")).expect("an empty folder");

        let archive = tmp.path().join("t.nva");
        {
            let mut a = nova_core::Archive::create(&archive).expect("a new archive");
            a.add_paths(&[tree], &super::pack_options("normal", Some(1), None))
                .expect("packs");
        }

        let info = super::open_archive(archive.to_string_lossy().into_owned()).expect("opens");
        let dirs: Vec<_> = info.entries.iter().filter(|e| e.dir).collect();
        assert!(!dirs.is_empty(), "the folders are listed");
        assert!(
            dirs.iter().all(|e| !e.solid),
            "a folder holds no bytes, so it shares no unit with anything"
        );
        assert!(
            info.entries.iter().any(|e| e.path.ends_with("a.txt")),
            "and the file inside it survived the trip"
        );
    }

    /// A path that is not there is not a task. Explorer never sends one, but a
    /// stale shortcut does, and opening a window that immediately errors is
    /// worse than not opening one.
    #[test]
    fn nothing_to_do_without_an_existing_path() {
        assert!(task_from_args(args(&[])).is_none());
        assert!(task_from_args(args(&["D:/nope/missing.nva"])).is_none());
        assert!(task_from_args(args(&["--test", "D:/nope/missing.nva"])).is_none());
    }

    /// The verb comes first and the rest is the path list. Checked against a
    /// file that really exists, because that is the rule the parser applies.
    #[test]
    fn a_verb_is_taken_from_the_front() {
        let me = std::env::current_exe().expect("this test has a path");
        let me = me.to_string_lossy().to_string();
        for (verb, expect) in [
            ("--compress", "compress"),
            ("--test", "test"),
            ("--extract-here", "extract-here"),
            ("--extract-into", "extract-into"),
        ] {
            let t = task_from_args(args(&[verb, &me])).expect("a real path");
            assert_eq!(t.verb, expect);
            assert_eq!(t.paths, vec![me.clone()]);
        }
    }

    /// A bare path is "open", and only for an archive: a double-clicked .txt
    /// must not be guessed into meaning anything.
    #[test]
    fn a_bare_path_opens_only_an_archive() {
        let me = std::env::current_exe().expect("this test has a path");
        assert!(task_from_args(args(&[&me.to_string_lossy()])).is_none());

        let dir = std::env::temp_dir().join("nova-startup-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let arc = dir.join("sample.nva");
        std::fs::write(&arc, b"not really an archive, only a name").expect("write");
        let t = task_from_args(args(&[&arc.to_string_lossy()])).expect("an .nva path");
        assert_eq!(t.verb, "open");
        let _ = std::fs::remove_file(&arc);
    }

    /// Several selected files arrive as several arguments and stay a list —
    /// compressing them into one archive is the whole point of selecting them.
    #[test]
    fn several_paths_stay_several() {
        let dir = std::env::temp_dir().join("nova-startup-multi");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
        std::fs::write(&a, b"a").expect("write");
        std::fs::write(&b, b"b").expect("write");
        let t = task_from_args(args(&[
            "--compress",
            &a.to_string_lossy(),
            &b.to_string_lossy(),
        ]))
        .expect("two real paths");
        assert_eq!(t.paths.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
