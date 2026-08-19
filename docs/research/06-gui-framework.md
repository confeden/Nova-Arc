# 06 — GUI Stack for Nova Prism (Windows-first, Android later)

Research date: 2026-08-16. All liveness/version/benchmark claims verified against live sources (linked inline).
Scope: Tauri 2, Slint, egui, iced, Dioxus, native WinUI 3 via windows-rs — evaluated against archiver-specific
requirements: 100k+-row virtualized lists, shell icons/thumbnails, bidirectional Explorer drag-and-drop
(drag-OUT researched in depth), context menus, RU/EN i18n, dark mode, accessibility, binary size, startup,
dev velocity, 2026 maturity, realistic Android path, zero telemetry.

---

## TL;DR

- **Primary recommendation: Tauri 2** (Rust core + TypeScript frontend in WebView2). It is the only stack where
  *every* hard requirement is satisfiable today with maintained, shipping components — including drag-out to
  Explorer (via CrabNebula's `tauri-plugin-drag`, production-proven in the Spacedrive file manager) and
  first-party Android support. WinRAR-class UI complexity (settings dialogs, profiles, progress, archive browser)
  is where web-frontend dev velocity dominates.
- **Runner-up: egui** (pure Rust). Proven at archiver-like data density (Rerun's `egui_table` handles millions of
  rows), AccessKit accessibility on by default, drag-out workable via the same `drag` crate, instant startup.
  Falls short on Android maturity, native look, and i18n tooling — acceptable fallback if the webview approach
  is vetoed.
- **Explicitly rejected**: Slint (no OS-level drag-and-drop at all as of 1.17 — fatal for an archiver today),
  iced (no accessibility, no Android, slow release cadence), Dioxus (same webview stack as Tauri but younger,
  plus opt-out CLI telemetry), WinUI 3 / Windows Reactor (Windows-only, ~3 months old).

---

## 1. Candidate assessments (state as of August 2026)

### 1.1 Tauri 2

- **Liveness**: very active. Tauri 2.0 stable since 2024-10-02; current stable v2.10.1 (2026-03-04)
  ([Wikipedia](https://en.wikipedia.org/wiki/Tauri_(software_framework)), [tauri.app](https://v2.tauri.app/blog/tauri-20/)).
  License MIT/Apache-2.0. Backed by CrabNebula + Tauri working group under the Commons Conservancy.
- **Architecture**: Rust process (all archiver logic) + system webview for UI (WebView2/Chromium on Windows,
  WKWebView on macOS, WebKitGTK on Linux, system WebView on Android/iOS). IPC bridge with a capability/permission
  model ([security docs](https://v2.tauri.app/security/)).
- **Virtualized lists**: solved by the mature web ecosystem (TanStack Virtual, AG Grid, react-window…). 100k rows
  is routine for these libraries; this is the least risky option of all six for the file-list requirement.
- **Drag-and-drop**: drag-IN is native (`tauri://drag-drop` events, incl. hover position). Drag-OUT is **not**
  built in — open feature request [tauri#6664](https://github.com/tauri-apps/tauri/issues/6664) — but is solved by
  [`drag-rs`](https://github.com/crabnebula-dev/drag-rs) / `tauri-plugin-drag` (MIT/Apache-2.0, tested with
  tao/winit/wry/Tauri v2, used by [Spacedrive](https://github.com/spacedriveapp/drag-rs)). Caveat: real file
  paths only (temp-materialization model — the same model WinRAR and 7-Zip use, see §3). No virtual-file
  (`CFSTR_FILEDESCRIPTOR`) support; that would be a custom `windows-rs` extension (§3.3).
- **Context menus**: native menus via the built-in menu API (muda) with `popup()`; or fully custom HTML menus.
- **i18n**: any web i18n stack (fluent, i18next…). RU/EN trivial. Dark mode: `prefers-color-scheme` + Tauri
  window theme API. Both are commodity features here.
- **Accessibility**: best of all candidates — WebView2 exposes the full Chromium accessibility tree to UIA/Narrator;
  semantic HTML gives screen-reader support with near-zero extra work.
- **Size/perf** (measured, sources in §5): 2.5–10 MB installers ([hopp measured 8.6 MiB](https://www.gethopp.app/blog/tauri-vs-electron)
  vs Electron 244 MiB); startup 366–417 ms to final render in the
  [Kalbertodt benchmark](http://lukaskalbertodt.github.io/2023/02/03/tauri-iced-egui-performance-comparison.html)
  (vs 200–300 ms egui) — fine for a GUI archiver; memory is the honest weak spot: WebView2 is Chromium, so
  real-world RAM is Electron-class ([tauri#5889](https://github.com/tauri-apps/tauri/issues/5889); hopp measured
  ~172 MB with 6 windows vs Electron ~409 MB).
- **Android**: first-party since 2.0 (Android + iOS templates, HMR on device). Team is honest that mobile is less
  mature than desktop: not all official plugins ported ([RC blog](https://v2.tauri.app/blog/tauri-2-0-0-release-candidate/),
  [stable blog](https://v2.tauri.app/blog/tauri-20/)). Still the most credible "same codebase on Android" story of
  any candidate except Slint.
- **Telemetry**: Tauri framework/CLI does not phone home. The community-raised concern is WebView2 itself — a
  Microsoft OS component whose telemetry is governed by OS-level settings
  ([discussion #4089](https://github.com/tauri-apps/tauri/discussions/4089)). The *app* ships nothing; document
  this in the privacy policy ("UI rendered by the OS webview; Nova Prism itself sends zero bytes").
- **Risks**: webview engine differences across OSes (WebKitGTK on Linux is the known pain point later);
  UI layer is TypeScript, not Rust (core remains 100% Rust); WebView2 runtime dependency (preinstalled on Win11,
  auto-distributed on Win10; offline installer exists).

### 1.2 Slint

- **Liveness**: very active, VC-backed company (SixtyFPS GmbH). 1.15 (Feb 2026), 1.16 (Apr 2026), 1.17.0
  (2026-06-24), 1.17.1 patch (Jul 2026) ([releases](https://github.com/slint-ui/slint/releases)).
- **License**: triple — GPLv3 **or** Royalty-Free 2.0 (free for desktop/mobile apps but **requires disclosing that
  you use Slint**, e.g. AboutSlint badge) **or** paid commercial ([FAQ](https://github.com/slint-ui/slint/blob/master/FAQ.md),
  [license](https://github.com/slint-ui/slint/blob/master/LICENSE.md)). For a FOSS archiver GPLv3 is fine;
  the attribution requirement only bites if Nova Prism avoids GPL.
- **Virtualized lists**: `ListView` instantiates only visible rows — 100k rows is within design
  ([docs](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/views/listview/)). But real-world reports:
  `StandardTableView` fast-scroll CPU spikes, "10k rows unusable on M1"
  ([discussion #7986](https://github.com/slint-ui/slint/discussions/7986)), artifacts past ~2–3M rows
  ([#3700](https://github.com/slint-ui/slint/issues/3700)). Custom item delegates would be needed.
- **Drag-and-drop — the killer defect**: as of 1.17, `DragArea`/`DropArea` work **only inside the application**.
  "Dragging to other applications and receiving drops from them is in development upstream in winit"
  ([Slint 1.17 blog](https://slint.dev/blog/slint-1.17-released), [#11400](https://github.com/slint-ui/slint/issues/11400)).
  An archiver that can't accept a drop from Explorer, let alone drag out, is dead on arrival. Workarounds
  (raw winit hooks + `drag-rs`) fight the framework.
- **i18n**: best-in-class in pure Rust: `@tr()` macro + gettext toolchain or compile-time bundled translations
  ([docs](https://docs.slint.dev/latest/docs/slint/guide/development/translations/)).
- **Accessibility**: `accessibility` feature (AccessKit-based) on desktop.
- **Android**: the strongest pure-Rust story — official since 1.5 ("the only Rust GUI toolkit to officially support
  Android", [blog](https://slint.dev/blog/slint-1.5-released)); 1.15 added safe-area + virtual-keyboard support.
- **Telemetry**: none in the library.
- **Verdict**: rejected *for now* purely on OS DnD absence + weaker table maturity; re-evaluate if cross-app DnD
  ships (they are actively upstreaming winit work).

### 1.3 egui

- **Liveness**: very active. 0.33 (Oct 2025) → 0.34 → 0.35.0 (2026-06-25, "Inspection, egui_mcp, classes and
  improved IME") ([releases](https://github.com/emilk/egui/releases)). MIT/Apache-2.0. Funded via rerun.io.
- **Virtualized lists**: proven at extreme scale. `ScrollArea::show_rows` renders visible rows only (f32-precision
  jitter only appears past ~2M rows, [#1391](https://github.com/emilk/egui/issues/1391));
  [`egui_table`](https://github.com/rerun-io/egui_table) (Rerun-maintained, v0.9.0 for egui 0.35) explicitly
  supports "millions of rows", sticky columns/headers, heterogeneous heights. 100k rows: no risk.
- **Drag-and-drop**: drag-IN works (winit dropped-files events surface in `RawInput::dropped_files`). Drag-OUT via
  the same [`drag`](https://github.com/crabnebula-dev/drag-rs) crate (winit supported on Windows/macOS; not
  winit-Linux). Temp-materialization model, like Tauri.
- **Accessibility**: AccessKit integrated, enabled by default in eframe on Windows/macOS
  ([README](https://github.com/emilk/egui)); new inspection protocol + `egui_mcp` (0.35) even lets agents drive
  the a11y tree.
- **i18n/dark mode**: dark/light built in; i18n is DIY (`rust-i18n`/fluent + font with Cyrillic — default Ubuntu
  font already covers RU). Context menus: built-in (`response.context_menu`), custom-drawn.
- **Size/perf**: fastest startup of all measured (200–300 ms to final render, window appears immediately;
  2 frames input lag — [Kalbertodt](http://lukaskalbertodt.github.io/2023/02/03/tauri-iced-egui-performance-comparison.html));
  binary ~18 MB in that test (shrinkable with opt-level="z"/strip, but bigger than Tauri's 5 MB exe).
- **Android**: runs via winit/android-activity, but no official mobile polish (soft-keyboard/IME/gesture gaps);
  0.33+ added safe-area work for iOS, so mobile is *moving*, not mature.
- **Telemetry**: none.
- **Risks**: immediate-mode look ("debug UI that grew up" — [2026 landscape](https://wrenlearnsrust.com/posts/2026-03-11-rust-gui-landscape-2026.html));
  every polished-archiver detail (thumbnail grid, inline rename, animated progress) is hand-rolled; continuous
  repaint costs battery if not idle-clamped (egui reactive mode mitigates).

### 1.4 iced

- **Liveness**: active but slow-cadence: 0.14.0 released 2025-12-07 — first stable since Sep 2024 (~15 months)
  ([release](https://github.com/iced-rs/iced/releases/tag/0.14.0)). MIT. Used by System76 COSMIC (via libcosmic fork).
- **0.14 verified against release notes**: new `table`/`grid` widgets, primitive culling in row/column, reactive
  rendering, IME support, headless testing, hot reload. **No accessibility entries whatsoever** in the changelog —
  secondary press claiming "screen reader support out of the box" is wrong; iced still has no AccessKit
  integration. No Android. No true list virtualization (culling ≠ windowed models; the scrollable still lays out
  all children).
- **Drag-out**: nothing; DIY via `drag` crate (winit-based, works on Windows).
- **Verdict**: rejected — accessibility gap alone disqualifies it for a mainstream consumer archiver; no Android
  path ("roadmap hints" only); virtualization story weakest of the pure-Rust trio.

### 1.5 Dioxus

- **Liveness**: very active, VC-funded. 0.7 (late 2025) + 0.7.10 patches; 0.8 series continuing
  ([release](https://github.com/DioxusLabs/dioxus/releases/tag/v0.7.0), [blog](https://dioxuslabs.com/blog/release-070/)).
  MIT/Apache-2.0.
- **Architecture**: desktop/mobile = tao + wry system webview (i.e., **the same rendering stack as Tauri**, with
  Rust instead of TS as the UI language); Dioxus Native (Blitz, wgpu HTML/CSS renderer) is explicitly
  experimental. Android/iOS webview-based with genuinely good tooling (`dx serve --platform android` on real
  devices, unified manifest config).
- **Telemetry — verified**: the `dx` CLI collects anonymized telemetry since 0.7, **opt-out**
  (`dx config set disable-telemetry true` / `TELEMETRY=false` / `disable-telemetry` build flag)
  ([release notes](https://dioxuslabs.com/blog/release-070/)). It's dev-tooling-only — nothing ships in the app —
  but an opt-out default clashes with Nova Prism's zero-telemetry ethos and would need documenting for
  reproducible-build contributors.
- **Verdict**: rejected as primary — if we accept a webview, Tauri is the same engine with a larger plugin
  ecosystem (incl. the drag-out plugin), bigger community, and no CLI telemetry. Rust-in-UI is attractive but
  RSX+hot-patching is a younger toolchain; drag-out plugin story untested with Dioxus's wry windows.

### 1.6 Native WinUI 3 via windows-rs ("Windows Reactor")

- **History**: Microsoft's `windows-app-rs` was archived — WinAppSDK "too heavily tied to .NET and Visual Studio"
  ([windows-rs#2153](https://github.com/microsoft/windows-rs/issues/2153)).
- **New in May 2026 (verified against the primary source)**: the
  [Rust for Windows – May 2026 newsletter](https://github.com/microsoft/windows-rs/issues/4483) announces
  **Windows Reactor** — a React-inspired declarative Rust framework over WinUI 3: hooks, 55+ widgets,
  virtualized lists, theming, accessibility, single ~3 MB binary, requires WinAppSDK 2.0.1+ runtime. Community
  alternative: [`winio-winui3`](https://crates.io/crates/winio-winui3).
- **Verdict**: rejected — Windows-only forever (kills Linux/macOS/Android), ~3 months old, unknown adoption, and
  Microsoft has already abandoned one Rust+WinUI vehicle. Watch it: if it matures, it could someday power a
  Windows-native *shell extension* UI, not the main app.

---

## 2. Comparison matrix

| Criterion | Tauri 2 | egui | Slint | iced | Dioxus | WinUI3/Reactor |
|---|---|---|---|---|---|---|
| 100k+-row virtual list | ✅ web grids (mature) | ✅ egui_table "millions" | ⚠️ ListView ok, table perf reports | ⚠️ culling only | ✅ web-side | ✅ claimed |
| Shell icons/thumbnails | ✅ via Rust + custom protocol | ✅ via Rust + texture | ✅ via Rust + Image | ✅ via Rust | ✅ same as Tauri | ✅ native |
| Drag IN from Explorer | ✅ native events | ✅ winit | ❌ not until winit work lands | ✅ winit | ✅ wry | ✅ |
| **Drag OUT to Explorer** | ✅ tauri-plugin-drag (Spacedrive-proven) | ✅ drag crate (winit/Win) | ❌ in-app only (1.17) | ⚠️ DIY drag crate | ⚠️ untested plugin path | ✅ native OLE |
| Context menus | ✅ native (muda) or HTML | ✅ built-in (custom-drawn) | ✅ ContextMenuArea | ⚠️ DIY overlay | ✅ HTML | ✅ native |
| RU/EN i18n | ✅ web i18n libs | ⚠️ DIY | ✅ @tr + gettext | ⚠️ DIY | ✅ web-ish | ✅ WinRT resources |
| Dark mode | ✅ CSS + window theme | ✅ built-in | ✅ | ✅ | ✅ | ✅ Fluent |
| Accessibility | ✅✅ full Chromium UIA | ✅ AccessKit (default) | ✅ AccessKit feature | ❌ none (verified 0.14) | ✅ webview | ✅ claimed |
| Installer/binary | ~2.5–10 MB installer | ~5–18 MB exe | small (few MB) | ~17 MB | ~5–10 MB | ~3 MB + WinAppSDK |
| Startup (to final render) | 366–417 ms | 200–300 ms (best) | fast (native) | 217–333 ms | ≈Tauri | fast |
| Idle RAM (Windows) | ⚠️ Chromium-class (100 MB+) | ✅ low | ✅ low | ✅ low | ⚠️ Chromium-class | ✅ low |
| Dev velocity (archiver UI) | ✅✅ highest | ⚠️ hand-rolled polish | ⚠️ DSL learning curve | ⚠️ | ✅ | ❌ verbose/new |
| Android path | ✅ official (plugin gaps) | ⚠️ runs, unpolished | ✅ official (best pure-Rust) | ❌ none | ✅ official | ❌ never |
| Telemetry | ✅ none (WebView2 = OS component) | ✅ none | ✅ none | ✅ none | ⚠️ opt-out CLI telemetry | unknown (new) |
| License | MIT/Apache-2.0 | MIT/Apache-2.0 | GPLv3 / RF-with-attribution / paid | MIT | MIT/Apache-2.0 | MIT (bindings) |
| 2026 maturity | ✅✅ v2.10, 2 yrs stable | ✅✅ | ✅ (DnD gap) | ⚠️ pre-1.0 | ✅ younger | ❌ 3 months |

---

## 3. Deep dive: drag-and-drop OUT of the app (the hard one)

### 3.1 The Windows protocol

A drag source calls OLE `DoDragDrop` with an `IDataObject`. Two ways to represent files:

1. **`CF_HDROP`** — a list of *real paths that must exist on disk at drop time*. Universally accepted.
2. **Virtual files** — `CFSTR_FILEDESCRIPTORW` (FILEGROUPDESCRIPTOR: names, sizes, attrs, timestamps) +
   one `CFSTR_FILECONTENTS` per file, streamed on demand via `IStream` (`FORMATETC.lindex` selects the file).
   This is how Outlook drags messages that live in a PST
   ([MS shell data scenarios](https://learn.microsoft.com/en-us/windows/win32/shell/datascenarios),
   [Raymond Chen's canonical walkthrough](https://devblogs.microsoft.com/oldnewthing/20080318-00/?p=23083)).
   Filling in sizes gives Explorer a correct progress bar. Compatibility caveat: Explorer accepts it, but many
   targets (most non-shell apps) only understand `CF_HDROP`
   ([OutlookFileDrag](https://github.com/tonyfederer/OutlookFileDrag) exists precisely to bridge that gap by
   materializing temp files when a `CF_HDROP`-only target asks).

### 3.2 How the incumbents do it (verified)

- **WinRAR**: two-step. Drag-out first extracts to the temp folder, then the drop target copies from temp
  ([official docs: Drag and drop support](https://documentation.help/WinRAR/HELPWinShellDrag.htm)). Temp files
  can't be deleted immediately ("no reliable way to detect if an external program still needs them"), so WinRAR
  deletes temp files **older than 1 hour on the next run**
  ([official FAQ](https://www.win-rar.com/leave-artifacts-temp-folder.html?L=0)). Docs explicitly recommend
  "Extract To" to bypass temp entirely.
- **7-Zip**: same temp-materialization; extracts into `%TEMP%\7zO<hex>\`. Known bug: it ignores the user's custom
  working-directory setting when dragging ([sf bug #2056](https://sourceforge.net/p/sevenzip/bugs/2056/), open
  since ~2007; complaints: SSD wear, plaintext leaks from encrypted archives). Cleanup is inconsistent —
  leftover `7zO*` folders are common ([sf thread](https://sourceforge.net/p/sevenzip/discussion/45797/thread/e23a6931/));
  7-Zip 24.04 added *Tools → Delete Temporary Files* as a band-aid.

Conclusion: **temp-materialization + `CF_HDROP` is the industry-standard mechanism** — neither market leader ships
virtual-file drag. Matching them is table stakes; beating them (virtual files = no temp writes, correct progress,
no plaintext residue from encrypted archives) is a genuine differentiator Nova Prism can add later.

### 3.3 What each framework gives us

- **Tauri 2**: no built-in drag-out ([tauri#6664](https://github.com/tauri-apps/tauri/issues/6664) open).
  Use [`tauri-plugin-drag`](https://github.com/crabnebula-dev/drag-rs) (`startDrag({ item: [paths], icon })` from
  JS): Nova Prism extracts the selection to its managed temp dir, then starts the OS drag with real paths — exactly
  the WinRAR model, and battle-tested by Spacedrive (a file manager, our closest real-world analog).
- **egui/iced**: same `drag` crate at the Rust level (winit windows supported on Windows/macOS; the crate's Linux
  path is GTK-only — winit-Linux unsupported).
- **Slint**: nothing until their winit cross-app DnD work lands.
- **Phase-2 upgrade (framework-independent)**: implement our own `IDataObject` in Rust via `windows-rs` exposing
  *both* `CFSTR_FILEDESCRIPTORW`+`CFSTR_FILECONTENTS` (streamed straight from the .nva chunk store — no temp
  files, exact sizes for the progress bar) *and* delayed-rendered `CF_HDROP` (extract to temp only when a
  legacy target actually requests it), plus `IDataObjectAsyncCapability` so extraction runs off the UI thread.
  This works with any framework that hands us an HWND + mouse-down hook; `drag-rs` is the scaffold to fork.
  Zero out unused FILEGROUPDESCRIPTOR bytes (info-disclosure fix noted by Chen).

---

## 4. Deep dive: open → edit → update-archive UX flow

### 4.1 Incumbent behavior (verified, incl. failure modes)

**WinRAR**: double-click in archive → extract to temp subfolder → `ShellExecute` the associated app → on
return/refocus compares modification time → prompts to update the archive. Cleanup: age-based GC (>1 h) on next
launch, because "files may still be needed by an external app" ([FAQ](https://www.win-rar.com/leave-artifacts-temp-folder.html?L=0)).

**7-Zip**: extract to `%TEMP%\7zO<hex>\` → `ShellExecute` → wait on the spawned process + listen for change
notifications → on change, prompt to update. Documented failure modes (SourceForge, incl. Igor Pavlov's own
comments):

1. **Editor spawns a second process** (single-instance editors, Office): 7-Zip sees the launcher exit, assumes
   "closed", deletes the temp file prematurely ([thread](https://sourceforge.net/p/sevenzip/discussion/45798/thread/6a4d0fa2/)).
2. **Locked temp file at cleanup**: Word still holds the file; deletion fails; Pavlov: "no simple solution"
   ([thread](https://sourceforge.net/p/sevenzip/discussion/45798/thread/9b7c563a/)).
3. **Missed modifications**: Office saves via replace-file (delete + rename of a new file), so mtime tracking on
   the original handle misses it; Excel edits detected only "sometimes"
   ([thread](https://sourceforge.net/p/sevenzip/discussion/45798/thread/fec17482/)).
4. **Security**: contents of *encrypted* archives sit in plaintext in `%TEMP%`; cleanup is unreliable
   ([bug #1448](https://sourceforge.net/p/sevenzip/bugs/1448/)).

### 4.2 Recommended Nova Prism design (do better on every failure mode)

1. **Dedicated temp root**: `%LOCALAPPDATA%\NovaPrism\open\<session>\<archive-hash>\` (never bare `%TEMP%`; honor a
   user-configured override — the thing 7-Zip's bug #2056 gets wrong). Mark files read-only when the archive is
   read-only.
2. **Watch the directory, not the process**: `ReadDirectoryChangesW` (Rust `notify` crate) on the extraction
   folder catches Office-style replace-saves that defeat 7-Zip's mtime check. Debounce, then compare content hash
   (we already have CDC chunk hashes — reuse them) so touch-without-change doesn't nag.
3. **Track "in use" correctly**: don't guess from process exit. Use the **Restart Manager API**
   (`RmStartSession`/`RmGetList`) or a periodic exclusive-open probe to know whether *any* process still holds
   the file — this solves both the two-process-editor and the locked-Word-file cases.
4. **Offer update non-modally**: toast/banner "photo.jpg changed — update archive?" with "always for this
   session". Thanks to .nva append-only updates, applying it is near-instant — this flow becomes a headline
   feature instead of a scary repack.
5. **Locked-file edge**: if the file is still locked when the user closes the archive, keep a pending-update
   journal; retry on next launch (and GC temp entries older than N hours, WinRAR-style, but *per entry* with the
   Restart-Manager check before deletion).
6. **Encrypted archives**: extract-for-open into an encrypted scratch (DPAPI-protected per-user dir at minimum;
   optionally refuse-and-warn), and securely delete on cleanup — direct answer to 7-Zip's plaintext-in-%TEMP%
   complaints.
7. **Menu parity**: provide both "Open" (this flow) and "Extract To…" (no temp involvement), as WinRAR docs
   recommend for temp-averse users.

---

## 5. Numbers (sourced)

| Metric | Tauri 2 | egui | iced | Source |
|---|---|---|---|---|
| Startup → final render | 366–417 ms | 200–300 ms | 217–333 ms | [Kalbertodt benchmark](http://lukaskalbertodt.github.io/2023/02/03/tauri-iced-egui-performance-comparison.html) (Linux/X11, 60 Hz; older but the only controlled 3-way test) |
| Window appears | 100–150 ms | immediate | 33–50 ms | same |
| Input lag (frames @60 Hz) | 2–3 | 2 | 3 | same |
| Binary size (that test) | 5 MB | 18 MB | 17 MB | same |
| Real-app bundle | 8.6 MiB (vs Electron 244 MiB) | — | — | [hopp](https://www.gethopp.app/blog/tauri-vs-electron) |
| RAM, 6 windows | ~172 MB (vs Electron ~409 MB) | — | — | [hopp](https://www.gethopp.app/blog/tauri-vs-electron); WebView2 ≈ Chromium caveat: [tauri#5889](https://github.com/tauri-apps/tauri/issues/5889) |
| Tauri official startup/memory harness | live numbers | — | — | [tauri-apps/benchmark_results](https://github.com/tauri-apps/benchmark_results) |

Interpretation: egui wins raw startup/latency; Tauri's ~0.4 s cold start and Chromium-class RAM are the price of
webview velocity — acceptable for an archiver (7-Zip FM and WinRAR are not sub-100 ms apps either), but keep the
CLI/extraction engine a separate lean binary so shell-integration paths never pay webview costs.

---

## 6. Shell icons & thumbnails (framework-independent, Rust-side)

- Per-extension icons: `SHGetFileInfoW` with `SHGFI_USEFILEATTRIBUTES` — returns the correct icon for a filename
  *that does not exist on disk* (exactly what an archive browser needs). Safe wrapper exists in
  [winsafe](https://docs.rs/winsafe/latest/winsafe/fn.SHGetFileInfo.html); raw in
  [windows crate](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/UI/Shell/fn.SHGetFileInfoA.html).
- Explorer-quality thumbnails: `IShellItemImageFactory::GetImage`
  ([windows crate binding](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/UI/Shell/struct.IShellItemImageFactory.html)) —
  requires a real file, so thumbnails for archived images = extract-to-cache first (or decode ourselves from the
  chunk store, which .nva makes cheap; do that and skip the shell for common image types).
- Convenience crates: [windows-icons](https://crates.io/crates/windows-icons),
  [file_icon_provider](https://crates.io/crates/file_icon_provider) (cross-platform).
- Delivery into the UI: Tauri — custom `icon://` protocol returning PNG (cache by extension); egui/Slint/iced —
  texture upload. Both straightforward; not a differentiator between frameworks.

---

## 7. Telemetry audit (owner requirement: zero, ever)

| Component | Phones home? | Notes |
|---|---|---|
| Tauri framework + CLI | No | Community-audited; concern is WebView2 as an OS component ([#4089](https://github.com/tauri-apps/tauri/discussions/4089)) — outside the app, governed by Windows settings; document it |
| egui / eframe | No | — |
| Slint | No | — |
| iced | No | — |
| **Dioxus `dx` CLI** | **Yes, opt-out** (since 0.7) | Anonymized heartbeats/crash reports; disable via config/env/build flag ([release notes](https://dioxuslabs.com/blog/release-070/)) — dev-time only, but opt-out-by-default conflicts with project ethos |
| Windows Reactor | Unknown | Too new to audit |

---

## 8. Decision

### Primary: **Tauri 2** (Rust core + TypeScript/web frontend)

Decisive reasons:

1. **Every hard requirement is provable today with maintained components**: drag-out via CrabNebula's plugin
   (proven in Spacedrive — a shipping Rust file manager), native drag-in events, 100k-row lists via mature web
   virtualization, full UIA accessibility from Chromium, commodity i18n/dark-mode, native context menus.
   No other candidate passes all gates: Slint fails DnD entirely; iced fails a11y + Android; egui passes but at
   2–3× the UI engineering cost for WinRAR-grade chrome.
2. **Dev velocity where an archiver actually spends UI effort** — settings/profiles dialogs, compression-analysis
   review UI ("phase 1 found 214 JPEGs → recompress losslessly?"), progress/queue management. This is form-heavy
   UI, the web stack's home turf.
3. **Android path is first-party** and shares the frontend; desktop plugins that matter (dialog, fs, drag) are
   desktop-only anyway.
4. **Architecture insurance**: keep 100% of archiver logic in Rust crates behind a thin IPC boundary
   (`nova-core`, `nova-formats`, `nova-shell-integration`). If Tauri ever disappoints, the UI layer is the only
   rewrite; the same crates would back an egui or Slint shell. "Written in Rust" stays true where it matters —
   the engine — and marketing-honest ("Rust core, native webview UI, zero telemetry").

Accepted costs: Chromium-class RAM while the GUI is open; ~0.4 s cold start; TS in the repo; WebView2 runtime
dependency; WebKitGTK pain deferred to the Linux port.

### Runner-up: **egui**

If the owner vetoes any webview (RAM, TS, or WebView2-optics grounds): egui + `egui_table` + AccessKit +
`drag` crate delivers a lean, instant-launch, pure-Rust archiver — the Rerun ecosystem proves the data-density
story. Costs: hand-rolled polish and i18n, utilitarian look, and a genuinely weak Android story (Slint would
re-enter consideration for mobile once its DnD lands, or mobile gets a separate thin UI).

### Trigger points to revisit

- Slint ships cross-application drag-and-drop → re-benchmark `StandardTableView`; strongest pure-Rust Android play.
- Windows Reactor survives 12+ months with adoption → candidate for Windows-native shell-extension surfaces.
- Tauri mobile plugin parity milestone → accelerates the Android port.
