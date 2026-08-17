# 07 — Windows Integration & Packaging for Nova Arc

Research date: 2026-08-16. All liveness/version/pricing facts below were verified against live sources on this date (links inline). Scope: file associations, Explorer context menu (Win10 vs Win11), shell extensions in Rust, installers, auto-update without telemetry, code signing for open source, portable mode, temp-extraction hygiene, and "viewer closed" detection — ending in a concrete v1 plan.

---

## 1. Executive summary (the v1 plan in one paragraph)

Ship a **classic Win32 app installed by Inno Setup**, which (a) registers ProgIDs + `OpenWithProgids` + Default Programs `Capabilities` for `.narc/.zip/.7z/.rar` (never touching `UserChoice`), (b) registers a **classic `IContextMenu`/registry context menu** that serves Windows 10 and the Win11 "Show more options" menu, and (c) on Windows 11 additionally registers a **signed sparse MSIX ("package with external location")** that exposes an **`IExplorerCommand` COM DLL written in Rust (windows-rs)** for the modern right-click menu — exactly the architecture VS Code and WinRAR ≥6.10 use, and the thing official 7-Zip *still* does not do as of v26.02 (June 2026), which is a genuine differentiator. Sign everything with **Azure Artifact Signing (~$9.99/mo)** or free **SignPath Foundation** OSS signing; accept that SmartScreen reputation takes weeks regardless. Distribute via GitHub Releases + **winget manifest**; in-app update = one anonymous HTTPS GET against a static JSON (or adopt Velopack later). Portable mode = `portable` marker file next to the exe. Temp extraction goes to `%LOCALAPPDATA%\Temp\NovaArc\<run-id>\`, with a startup janitor that reaps orphaned run-dirs whose owning PID is dead; "viewer closed" detection = wait on the `ShellExecuteEx` process handle, falling back to Restart Manager (`RmGetList`) lock polling, plus a file-change watcher to offer "update archive?" (which `.narc` makes nearly free).

---

## 2. File associations: Win10 vs Win11

### 2.1 What an app can and cannot do (2024–2026 reality)

| Mechanism | Works? | Notes |
|---|---|---|
| Register ProgID under `HKLM\Software\Classes` (`NovaArc.narc`, `NovaArc.zip`, …) with `DefaultIcon`, `shell\open\command` | Yes, fully supported | The baseline. For brand-new extensions (`.narc`) this alone makes double-click work — no competition, no UserChoice needed. |
| Add ProgID to `HKCR\.zip\OpenWithProgids` | Yes | Puts Nova Arc in the "Open with" list for `.zip/.7z/.rar` without stealing the default. |
| Default Programs registration: `HKLM\SOFTWARE\NovaArc\Capabilities\FileAssociations` + entry in `HKLM\SOFTWARE\RegisteredApplications` | Yes | Required for the app to appear properly in Settings → Default apps. Call `SHChangeNotify(SHCNE_ASSOCCHANGED)` after registration. |
| Programmatically **setting yourself as default** (writing `UserChoice`) | **No** — blocked since Windows 8 | `HKCU\...\Explorer\FileExts\.ext\UserChoice` is protected by a per-user, per-app **cryptographic hash**; invalid hashes are discarded and logged (`Shell-Core/AppDefaults` event log). See [kolbi.cz / SetUserFTA](https://kolbi.cz/blog/2017/10/25/setuserfta-userchoice-hash-defeated-set-file-type-associations-per-user/). |
| Reverse-engineered hash writers (SetUserFTA et al.) | Fragile and getting worse | In **April 2025** Microsoft added **"UserChoiceLatest" + a UserChoice Protection Driver** on Windows 11 that re-protects associations and reverts tampering; workarounds now involve ViVeTool feature toggles and still don't survive reboots reliably ([setuserfta.com](https://setuserfta.com/), AskWoody/ElevenForum threads, 2025). **Do not build on this.** |

**Conclusion:** an archiver in 2026 *registers* as a handler and then **asks the user** to set defaults, deep-linking to `ms-settings:defaultapps` (optionally `ms-settings:defaultapps?registeredAppUser=Nova Arc` to land on the app's own page on Win11). Any attempt to silently become the default is both technically doomed (UserChoiceLatest) and reputationally toxic for a "zero telemetry, user-respecting" brand.

### 2.2 Win10 vs Win11 UX differences that matter

- **Win10:** Settings → Apps → Default apps → "Choose default apps by file type"; also the classic "How do you want to open this file?" prompt with "Always use this app" appears when a new handler is registered.
- **Win11:** defaults are managed per-app or per-extension in Settings; the "always use" flow is more buried (per-extension "Set default" button). Same registration mechanics, more clicks for the user — the in-app "Make Nova Arc the default for…" screen with a deep link is effectively mandatory UX.
- **Win11 23H2+ ships a native competitor:** File Explorer gained **libarchive-based read support for .rar, .7z, .tar.\*** (11 formats, no encrypted archives) via [KB5031455, Oct 2023](https://www.bleepingcomputer.com/news/microsoft/windows-11-adds-support-for-11-file-archives-including-7-zip-and-rar/), and **24H2 added creation** of zip/7z/tar from Explorer. It is slow ([Neowin measured ~9 min vs ~1 min for WinRAR/NanaZip on the same 7z](https://www.neowin.net/news/windows-11-gets-native-rar-support-here-is-how-it-compares-to-winrar-and-other-apps/)) and feature-poor (no passwords), but it means **on a fresh Win11 box, `.7z` and `.rar` default to Explorer**, not to "nothing". Nova Arc must expect to win the default *from* Explorer via user consent, not to inherit a vacant extension.

### 2.3 Per-extension plan

| Extension | Registration | Default strategy |
|---|---|---|
| `.narc` | Own ProgID, icon, `open` verb, `friendlyTypeName` | Auto-becomes default (no incumbent); still registered via Capabilities for cleanliness |
| `.zip` | ProgID + OpenWithProgids + Capabilities | Never seized silently; offer in first-run UI (Explorer is the incumbent) |
| `.7z`, `.rar` | Same | Same; on Win10 these are often unowned → the "new app installed" prompt appears naturally |
| Optional later: `.tar`, `.gz`, `.zst`, `.xz`, … | Same pattern | Register handlers only if the format is actually supported end-to-end |

Uninstaller must remove ProgIDs, `OpenWithProgids` entries, `RegisteredApplications` entry, and fire `SHCNE_ASSOCCHANGED` again. Leaving orphans is the #1 archiver-uninstall complaint.

---

## 3. Explorer context menu: classic vs Win11 modern

### 3.1 The two worlds

| | Classic (`IContextMenu` + registry) | Win11 modern (`IExplorerCommand` + package identity) |
|---|---|---|
| Where it shows | Win10 main menu; Win11 only under **"Show more options"** (Shift+F10) | Win11 top-level right-click menu |
| Registration | `HKCR\*\shellex\ContextMenuHandlers`, `Directory\...`, per-ProgID | `AppxManifest.xml` (`desktop4:FileExplorerContextMenus` / `com.microsoft.explorer.command`) inside an MSIX or **sparse MSIX** |
| Identity/signing | None needed | **Package identity required**; sparse package **must be Authenticode-signed** and manifest `Publisher` must equal the cert Subject ([MS docs](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/grant-identity-to-nonpackaged-apps)) |
| Min OS | Everything | Sparse packages: Win10 2004 (build 19041)+; the modern menu itself: Win11 only; **drive (volume) context menus: Win11 22H2+** (NanaZip README) |
| Submenus | Arbitrary nesting | **One cascade level only**; effectively ~**16 items max per handler** ([Kenji Mouri's NanaZip write-up](https://mouri.moe/en/2021/12/25/Share-my-experience-of-implementing-context-menu-support-for-File-Explorer-in-Windows-11/)) |
| Drag-and-drop handlers | Supported (`DragDropHandlers`) | **No modern equivalent** — a known gap Mouri explicitly calls out |
| Known quirks | — | Explorer often needs a restart before a newly-registered packaged handler appears; Microsoft has been tightening how sparse packages may register COM CLSIDs, breaking hacky implementations ([shellex.info guide](https://shellex.info/guide/shell-extensions-vs-windows-11-menu)) |

### 3.2 What the competition actually shipped (verified 2026-08)

- **7-Zip (official), v26.02, 2026-06-25:** still **no** Win11 modern menu; bug [#2311](https://sourceforge.net/p/sevenzip/bugs/2311/) / [#2399](https://sourceforge.net/p/sevenzip/bugs/2399/) open for years; Igor Pavlov wants a no-AppX path that doesn't exist. Users live under "Show more options". → **Nova Arc shipping a proper Win11 menu on day one is a visible advantage over 7-Zip.**
- **WinRAR ≥6.10 (2022):** rewrote shell integration as `IExplorerCommand` wrapped in a sparse package; commands appear as one cascaded top-level submenu.
- **NanaZip (M2Team, 15.1k stars, actively maintained):** full-MSIX 7-Zip fork; context menu on Win10/11; distributed via Microsoft Store/winget; min OS Win10 2004. Its constraints (cascade-only, merged items, Explorer-restart issues) are the best public documentation of the modern menu's limits ([repo](https://github.com/M2Team/NanaZip)).
- **VS Code:** classic Win32 + Inno Setup + a **signed sparse package** containing an `IExplorerCommand` DLL — the canonical open-source reference implementation ([microsoft/vscode-explorer-command](https://github.com/microsoft/vscode-explorer-command), MIT). Step-by-step third-party walkthrough: [xplorer² blog](https://www.zabkat.com/blog/win11-explorer-menu-package.htm) (gotcha: manifest `Publisher`/names must match the signing cert exactly or Explorer silently refuses to load the extension). Minimal modern C++ samples: [cjee21/IExplorerCommand-Examples](https://github.com/cjee21/IExplorerCommand-Examples) (MIT; C++/WinRT and WRL).

### 3.3 Sparse package mechanics (condensed from [MS docs](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/grant-identity-to-nonpackaged-apps), updated 2026-04)

1. Author `AppxManifest.xml`: `Identity Publisher` == signing cert Subject; `uap10:AllowExternalContent=true`; `TargetDeviceFamily MinVersion=10.0.19041.0`; capabilities `runFullTrust` + `unvirtualizedResources`; `AppListEntry="none"` so it doesn't show as an "app".
2. `MakeAppx.exe pack /o /d <dir> /nv /p NovaArcId.msix` → `SignTool sign /fd SHA256 …` (same cert as the main binaries; Azure Artifact Signing works here).
3. Add a fusion (side-by-side) manifest to `nova-arc.exe` with `<msix publisher=… packageName=… applicationId=…/>` matching the package.
4. Installer registers it: per-user `Add-AppxPackage -Path … -ExternalLocation <install dir>`; per-machine `Add-AppxPackage -Stage … ; Add-AppxProvisionedPackage -Online` (or the `PackageManager` WinRT API — callable from Rust via windows-rs).
5. Uninstaller **must** `Remove-AppxPackage` (and deprovision for machine installs). Same-version re-register fails with `0x80073CF9` — bump the package version every release or unregister first.
6. Skip registration entirely on Win10 (<Win11) — the classic menu is the main menu there anyway; this also sidesteps sparse-package bugs on older builds.

Common failure codes worth baking into installer logs: `0x800B0109` (cert not trusted), `0x80073D54` (fusion-manifest mismatch), `0x80073CF6` (manifest invalid). Diagnostics: Event Viewer → `AppxDeployment-Server`.

### 3.4 Menu content budget (fits the 16-item / one-cascade limit)

Top-level cascade "Nova Arc" → Open · Extract Here · Extract to "<name>\" · Compress to .narc · Compress to .zip · Compress to .7z · Compress with options… · Test · Checksums. (9 items — room to grow, but the cascade cap is real; don't design a two-level menu, it cannot exist on Win11.)

---

## 4. Shell extensions in Rust — feasibility: **proven**

- **No framework crate exists** for shell extensions (crates.io search 2026-08: nothing maintained; Microsoft's `com-rs` is superseded). The working pattern is a `cdylib` crate using **`windows` / `windows-core`** with the `#[implement]` macro ([windows-implement](https://docs.rs/windows-implement)); `IExplorerCommand`, `IContextMenu`, `IShellItemArray`, `IEnumExplorerCommand` etc. are all generated in the `windows` crate ([IExplorerCommand binding](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/UI/Shell/struct.IExplorerCommand.html)). You hand-write `DllGetClassObject`/`DllCanUnloadNow` and a class factory (~100 lines of boilerplate).
- **Real, current Rust shell extensions (all verified alive 2026-08):**
  - [ArcThumb](https://github.com/citrussoda-com/ArcThumb) — Apache-2.0, pushed 2026-08-11; thumbnail + preview-pane provider **for archives/ebooks** (zip/CBZ/RAR/7z…), built on windows-rs. Closest prior art to Nova Arc's needs; also proves `IThumbnailProvider`/`IPreviewHandler` in Rust for a later milestone.
  - [SageThumbs-2k](https://github.com/LunarWerxs/SageThumbs-2k) — 136 stars, pushed 2026-08-16; Win11 thumbnail extension in Rust for 316 formats, "crash-isolated".
  - [LSENext](https://github.com/SunnyYYLin/LSENext) — Rust **Win11 modern context-menu** extension (symlinks/junctions) that registers "as a Windows 11 native Explorer context menu through package identity"; small (v0.2.x) but a direct existence proof of the exact Nova Arc architecture (Rust `IExplorerCommand` + sparse identity).
  - Plus glimpse (glTF thumbnails), ThumbsUp (EPUB), ai-psd-thumbnails — a healthy pattern, not a one-off.
- **In-proc DLL rules for Rust:** never unwind across the COM boundary (`catch_unwind` in every entry point, `panic = "abort"` is *not* acceptable inside Explorer — return HRESULTs); keep the DLL tiny (no tokio, no GUI toolkit — Explorer loads it into every process that shows a file dialog); do real work by launching `nova-arc.exe` with arguments; static CRT to avoid VC++ redist; test with `regsvr32`-free registration (sparse package does COM registration declaratively).
- **Fallback insurance:** if the Rust DLL ever misbehaves in the field, the vscode-explorer-command C++ DLL is MIT-licensed and swappable behind the same manifest — near-zero architectural risk.

---

## 5. Installers for an open-source archiver

### 5.1 Landscape (verified versions/licenses, 2026-08)

| Tool | Latest | License / cost reality | Fit for Nova Arc |
|---|---|---|---|
| **Inno Setup** | **7.1.0, 2026-08-12** ([jrsoftware.org](https://jrsoftware.org/isdl.php)) | Free for non-commercial/OSS; since v7 **commercial users must buy a perpetual license** (doesn't affect a free OSS app) | **Best fit.** Pascal scripting handles sparse-package registration, per-user installs, ARM64; used by VS Code & Git for Windows; produces one signed `setup.exe` |
| **WiX Toolset** | v6.0 2025-04-05, v7 current | Source open, but **Open Source Maintenance Fee** since v6 ($10–60/mo via GitHub Sponsors for revenue-generating consumers; v7 blocks builds until EULA accepted — `WIX7015`) ([FireGiant](https://www.firegiant.com/blog/2025/4/7/wix-v600-available/), [issue #8974](https://github.com/wixtoolset/issues/issues/8974), [Podman re-evaluating](https://github.com/containers/podman/issues/27042)); WiX v5 (2024) remains fee-free | MSI only matters for enterprise GPO deployment — not a v1 audience. Skip; revisit WiX v5 if enterprises ask for MSI |
| **MSIX (full)** | — | Free tooling; **requires signing**; Store re-signs for free | Great as a **second** channel (Microsoft Store, like NanaZip) — but full-MSIX-only would sacrifice portable mode, classic Win10 menu control, and non-Store users on LTSC/Server without App Installer |
| **NSIS** | 3.x | zlib license, free | Viable but Inno has better docs/momentum and the VS Code sparse-package precedent |

### 5.2 Auto-update without telemetry

Principle: an update check must be **one unauthenticated HTTPS GET for a static document, no unique IDs, no cookies**, opt-out-able, and clearly documented. Options, all verified active:

| Mechanism | Latest | Notes |
|---|---|---|
| **winget** community repo | Windows 10 1809+ built-in | Zero code in-app; users update via `winget upgrade`. Submit a manifest per release (automatable in CI with wingetcreate). No telemetry from Nova Arc's side |
| **In-app check against GitHub Releases / static JSON** | — | ~50 lines of Rust (`ureq` + semver compare). GitHub sees a normal anonymous request; self-hostable later. Recommended v1 |
| **[Velopack](https://github.com/velopack/velopack)** | 1.2.0, 2026-06-03; MIT; **written in Rust, first-class Rust SDK** ([docs](https://docs.velopack.io/getting-started/rust)) | Full install+delta-update framework (Squirrel successor). Delta updates are attractive for a 20–40 MB app; but it owns the install layout (app-per-version dirs), which conflicts with Inno + sparse-package registration. Candidate for v2 if update friction matters |
| **WinSparkle** | 0.9.4, 2026-07-21; MIT ([repo](https://github.com/vslavik/winsparkle)) | C DLL, appcast XML; solid but adds a C dependency where 50 lines of Rust suffice |
| **MSIX `.appinstaller`** | — | Auto-update built into the platform; only for the Store/MSIX channel |

v1 choice: **winget + in-app anonymous check that opens the browser to the release page** (no silent self-replacement → no updater-security surface, no SmartScreen re-warning loop on the updater stub).

---

## 6. Code signing reality for open source (2026)

Verified against [MS "Code signing options"](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options) (updated 2026-04-20):

| Option | Cost | Availability | SmartScreen |
|---|---|---|---|
| Microsoft Store (MSIX) — Store re-signs | Free | Worldwide | No warnings ever — **fastest zero-warning channel** |
| **Azure Artifact Signing** (ex-"Trusted Signing"; GA) | **~$9.99/mo** | Orgs: US/CA/EU/UK; **individuals: US/CA only** | Reputation builds over time; initial warnings expected. Certs rotate daily; reputation attaches to the validated identity, and per [draw.io's migration experience](https://github.com/jgraph/drawio-desktop/discussions/2415) publisher reputation accrues in days-to-weeks once downloads flow |
| OV certificate | $150–300/yr + **HSM/token mandatory since June 2023** | Worldwide | Same reputation model |
| EV certificate | $400+/yr | Worldwide | **EV's instant-reputation bypass was removed in 2024** — no longer worth the premium |
| **[SignPath Foundation](https://signpath.org/)** | **Free for OSS** | OSS projects meeting their criteria | OV-level cert via their managed CI pipeline (they verify binaries come from the public repo). Used by Stellarium, Flameshot, Git Extensions |
| Self-signed / unsigned | Free | — | Blocked/strongly warned; also **sparse package registration fails** without a trusted cert → not an option, signing is a hard requirement for the Win11 menu |

Practical plan: start with **SignPath Foundation** (free, OSS-native) or Azure Artifact Signing if the maintainer is US/CA/EU/UK-eligible; sign *everything* (exe, DLLs, installer, sparse MSIX) with the same identity; expect SmartScreen warnings for the first releases and pre-empt with a Store listing (free re-signing, instant trust). If warnings persist, submit binaries via the Microsoft Security Intelligence portal (software-developer path) — the only semi-official reputation channel.

Key structural point: **the sparse package must be signed with a cert whose Subject equals the manifest `Publisher`** — so the signing identity must be chosen *before* the first Win11-menu release, and changing it later invalidates the package identity (users get a "different publisher" upgrade break). Pick the long-term publisher name now.

---

## 7. Portable mode conventions

Surveyed conventions ([Notepad++](https://www.npp-user-manual.org/docs/config-files/), KeePass, [qBittorrent](https://github.com/qbittorrent/qBittorrent/wiki/How-to-use-portable-mode), [PortableApps.com format](https://portableapps.com/development/portableapps.com_format)):

| App | Trigger | Storage |
|---|---|---|
| Notepad++ | zero-byte `doLocalConf.xml` beside exe | exe dir; **silently ignored if exe is under `%ProgramFiles%`** |
| KeePass | `KeePass.config.xml` beside exe (+`PreferUserConfiguration=false`) | exe dir, falls back if unwritable |
| qBittorrent | folder `profile` beside exe or `--profile=` | the profile folder |

Nova Arc convention (composite of best practices):

- Trigger: file **`portable.txt`** (any content) *or* existing **`data\`** directory next to `nova-arc.exe`; CLI override `--portable[=<dir>]`.
- All config/state/cache under `<exedir>\data\`; **no registry writes, no shell integration, no sparse package, no file associations** in portable mode (integration requires an installer by definition; offer "install integration…" menu item that runs the real installer).
- If `data\` is not writable (e.g., copied into Program Files), show a one-time warning and fall back to `%APPDATA%\NovaArc` rather than silently losing settings (Notepad++'s silent mode-switch is a recurring user trap).
- Ship the portable build as a plain `.zip` (and later `.narc`, self-hosted dogfooding) — never a self-extractor, which re-triggers SmartScreen.
- Temp extraction in portable mode still goes to `%TEMP%` (see §8) — writing temp data next to the exe on a USB stick is slow and wears flash; PortableApps.com launchers do redirect TEMP, but that's their job, not the app's.

---

## 8. Temp extraction dir: placement and crash cleanup

### 8.1 Prior art — 7-Zip

7-Zip extracts double-clicked archive members to `%TEMP%\7zO<random>\` (open/view) and `%TEMP%\7zE<random>\` (edit / staged extraction), deletes them when the archive window closes, prompts "update archive?" if the file changed — and **leaks the folders whenever 7-Zip or the viewer crashes**; leftover `7zO*` dirs in `%TEMP%` are a well-known artifact ([SourceForge discussion](https://sourceforge.net/p/sevenzip/discussion/45797/thread/208a2bd611/)). Also a documented privacy issue: files from encrypted archives sit unencrypted in `%TEMP%`.

### 8.2 What the OS does (and doesn't) clean

- **Storage Sense** ("Delete temporary files that my apps aren't using", on by default on Win10/11) deletes from `%LOCALAPPDATA%\Temp` **based on file modified date** with a user-configurable threshold (1–60 days); Microsoft documents no precise contract, and files can vanish while an app still logically needs them ([MS Q&A investigation](https://answers.microsoft.com/en-us/windows/forum/all/storage-sense-configuration-for-deleting-temporary/321616d1-e6a7-413a-8246-28f88f5ecc4e)). Two consequences: (1) the OS *will eventually* mop up leaked dirs — a safety net; (2) Nova Arc must tolerate its temp files disappearing mid-session (reopen → re-extract).
- Nothing cleans on a schedule shorter than days; **the app owns prompt cleanup.**

### 8.3 Nova Arc design

1. Root: `GetTempPath2()` → `%LOCALAPPDATA%\Temp\NovaArc\` (per-user, usually SSD, ACL'd to the user). `GetTempPath2` (Win11+/Win10 via fallback to `GetTempPath`) avoids the SYSTEM-account `C:\Windows\Temp` pitfall.
2. Per-run directory `NovaArc\r-<pid>-<random>\`, containing a `lock` file that is held **open with exclusive share mode** for the whole session, plus a small `owner.json` (PID + process start time — the pair uniquely identifies a process incarnation and defeats PID reuse).
3. Per-opened-file subdir `r-…\o-<n>\filename.ext` — real filename preserved (apps show it in title bars), collisions isolated per subdir (7-Zip's approach).
4. **Startup janitor:** on every launch (and optionally a "Clean temp" button), scan `NovaArc\*`: for each run-dir, try to open `lock` exclusively — success means the owner is dead → verify owner.json PID+start-time is not a live process → delete recursively with retries (files may still be lock-held by a leaked viewer; skip those, retry next launch). This converges after any crash without background services or scheduled tasks.
5. Encrypted archives: offer (later) an opt-in "secure view" that refuses temp extraction of encrypted content or wipes-on-close; at minimum document the same leak 7-Zip has.
6. Never use the archive's own directory for temp staging (may be read-only/network/removable); for "Extract Here" staging of `.narc` compaction, write to the destination volume (same-volume rename is atomic) under a `.novaarc-tmp` name, cleaned by the same janitor logic.

---

## 9. Detecting "the app that opened the temp file has closed"

Three known techniques; each fails alone, so v1 uses them layered:

| Technique | How | Fails when |
|---|---|---|
| **Process handle wait** | `ShellExecuteEx(SEE_MASK_NOCLOSEPROCESS)` → `WaitForSingleObject`/`RegisterWaitForSingleObject` on `hProcess` | Single-instance apps (Word, Photos, modern browsers): the launched process forwards to an existing instance and exits in milliseconds; DDE-style launches return no handle at all |
| **Restart Manager lock query** | `RmStartSession` → `RmRegisterResources(file)` → `RmGetList` → list of PIDs currently holding the file — the documented, supported way to answer "who has this file open" ([Raymond Chen](https://devblogs.microsoft.com/oldnewthing/20120217-00/?p=8283), [RmGetList docs](https://learn.microsoft.com/en-us/windows/win32/api/restartmanager/nf-restartmanager-rmgetlist), [CrowdStrike deep-dive](https://www.crowdstrike.com/en-us/blog/windows-restart-manager-part-1/)) | Apps that read the file into memory and close the handle immediately (Notepad, most image viewers) hold no lock at all |
| **Exclusive-open probe / change watcher** | Periodically try `CreateFile` with share-mode 0; watch dir via `ReadDirectoryChangesW` for writes | Same "no lock held" apps; watcher only detects *modification*, not "done viewing" |

**v1 algorithm (mirrors what 7-Zip/WinRAR converge on, made explicit):**

1. Launch viewer via `ShellExecuteEx` with `SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI`.
2. If `hProcess` obtained → async-wait on it. When it fires (or if no handle), run `RmGetList` on the temp file: if other processes hold it, re-arm a wait on those PIDs (open their process handles and wait). This exactly covers the "stub forwarded to existing instance" case.
3. In parallel, `ReadDirectoryChangesW` on the run-dir: on modification of an extracted file, mark it dirty; when its holders reach zero (or on archive-window close / Nova Arc exit), prompt **"file changed — update archive?"** — and here `.narc`'s append-only log turns the update into a near-instant append+commit, vs 7-Zip's full repack. This is a marquee demo scenario, worth wiring properly.
4. If neither wait nor lock ever resolves (pure memory-reader app), fall back to cleanup at archive-window close, guarded by delete-retry (locked files survive one round and are reaped by the startup janitor).
5. Never delete a dirty file without either updating the archive or explicit user consent.

All of this is plain Win32, fully expressible in Rust via windows-rs (`Threading`, `RestartManager`, `FileSystem` feature gates).

---

## 10. Concrete v1 integration plan

**Deliverables (in build order):**

1. **`nova-arc-shellext` crate** (`cdylib`, windows-rs): `IExplorerCommand` cascade (9 verbs above) + `IEnumExplorerCommand`; every entry point `catch_unwind` + HRESULT; no async runtime; spawns `nova-arc.exe`. Same DLL also exports the classic `IContextMenu` handler (one DLL, two interfaces) for Win10 / "Show more options".
2. **Sparse package pipeline** in CI: `AppxManifest.xml` (Publisher = signing identity, MinVersion 10.0.19041.0, `AllowExternalContent`), `MakeAppx /nv`, `SignTool`, artifact `NovaArcId.msix`; package version = app version (fixes the `0x80073CF9` re-register trap).
3. **Inno Setup 7 installer** (free for OSS): installs per-machine to `Program Files` (per-user option later); registers ProgIDs/OpenWithProgids/Capabilities; registers classic context-menu handler; **on Win11 only** runs `Add-AppxPackage -ExternalLocation`; uninstall reverses everything incl. `Remove-AppxPackage` and `SHCNE_ASSOCCHANGED`.
4. **Signing**: apply to SignPath Foundation now (weeks of lead time); fallback Azure Artifact Signing $9.99/mo if eligible. One publisher identity forever — it is baked into package identity.
5. **Distribution**: GitHub Releases (setup.exe + portable zip) + winget manifest per release (CI-automated). Microsoft Store MSIX submission as milestone 2 (free re-signing kills SmartScreen for that channel; NanaZip precedent).
6. **Default-apps UX**: first-run screen listing `.narc/.zip/.7z/.rar` with per-extension "Set default" → `ms-settings:defaultapps` deep link; explicit "we can't and won't set this silently" copy (on-brand for a no-telemetry app).
7. **Portable mode**: `portable.txt` / `data\` trigger; no integration in portable mode; warning when unwritable.
8. **Temp subsystem**: `%LOCALAPPDATA%\Temp\NovaArc\r-<pid>-<rand>\` + lock-file + owner.json + startup janitor; layered viewer-close detection (process wait → RmGetList → watcher) feeding the "update archive?" prompt (near-free for `.narc`).
9. **Update check**: `winget` + in-app anonymous GET of `https://…/latest.json`, "open release page" button; document the exact request in PRIVACY.md.

**Explicit non-goals for v1:** MSI/WiX (enterprise pull only, OSMF friction), full-MSIX-only packaging (loses portable + Win10 classic menu control), silent self-update, UserChoice manipulation, drag-drop handler on the Win11 modern menu (API doesn't exist — classic menu covers it), preview/thumbnail handlers for archives (great v2 — ArcThumb proves Rust can; `.narc` cover thumbnails would be a lovely showcase).

**Test matrix:** Win10 22H2 (classic menu, no sparse pkg), Win11 23H2 & 24H2+ (modern menu, libarchive incumbent defaults, Explorer-restart quirk), per-machine vs per-user install, portable-on-USB, crash-during-view temp recovery, uninstall residue audit (Autoruns + registry diff).

---

## 11. Sources

- Win11 menu/identity: [MS: grant identity via external location](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/grant-identity-to-nonpackaged-apps) · [vscode-explorer-command](https://github.com/microsoft/vscode-explorer-command) · [xplorer² walkthrough](https://www.zabkat.com/blog/win11-explorer-menu-package.htm) · [Kenji Mouri on NanaZip](https://mouri.moe/en/2021/12/25/Share-my-experience-of-implementing-context-menu-support-for-File-Explorer-in-Windows-11/) · [cjee21 examples](https://github.com/cjee21/IExplorerCommand-Examples) · [shellex.info](https://shellex.info/guide/shell-extensions-vs-windows-11-menu)
- Competitors: [NanaZip](https://github.com/M2Team/NanaZip) · [7-Zip bug #2311](https://sourceforge.net/p/sevenzip/bugs/2311/) · [7-Zip history (26.02)](https://www.7-zip.org/history.txt)
- Rust shell ext: [ArcThumb](https://github.com/citrussoda-com/ArcThumb) · [LSENext](https://github.com/SunnyYYLin/LSENext) · [SageThumbs-2k](https://github.com/LunarWerxs/SageThumbs-2k) · [windows-rs IExplorerCommand](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/UI/Shell/struct.IExplorerCommand.html) · [windows-implement](https://docs.rs/windows-implement)
- Associations: [SetUserFTA / kolbi.cz](https://kolbi.cz/blog/2017/10/25/setuserfta-userchoice-hash-defeated-set-file-type-associations-per-user/) · [setuserfta.com](https://setuserfta.com/) · [Win11 libarchive formats](https://www.bleepingcomputer.com/news/microsoft/windows-11-adds-support-for-11-file-archives-including-7-zip-and-rar/) · [Neowin perf comparison](https://www.neowin.net/news/windows-11-gets-native-rar-support-here-is-how-it-compares-to-winrar-and-other-apps/)
- Installers/update: [Inno Setup](https://jrsoftware.org/isdl.php) · [WiX v6 + OSMF](https://www.firegiant.com/blog/2025/4/7/wix-v600-available/) · [OSMF issue](https://github.com/wixtoolset/issues/issues/8974) · [Rob Mensching, 2 months in](https://robmensching.com/blog/posts/2025/05/12/open-source-maintenance-fee-two-months-in/) · [Podman evaluation](https://github.com/containers/podman/issues/27042) · [Velopack](https://github.com/velopack/velopack) · [Velopack Rust guide](https://docs.velopack.io/getting-started/rust) · [WinSparkle](https://github.com/vslavik/winsparkle) · [winget](https://learn.microsoft.com/en-us/windows/package-manager/winget/) · [.appinstaller updates](https://learn.microsoft.com/en-us/windows/msix/app-installer/install-update-app-installer)
- Signing: [MS code-signing options (2026-04)](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options) · [SmartScreen reputation](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation) · [draw.io ATS migration](https://github.com/jgraph/drawio-desktop/discussions/2415) · [SignPath Foundation](https://signpath.org/)
- Portable: [Notepad++ config docs](https://www.npp-user-manual.org/docs/config-files/) · [qBittorrent portable wiki](https://github.com/qbittorrent/qBittorrent/wiki/How-to-use-portable-mode) · [PortableApps.com format](https://portableapps.com/development/portableapps.com_format)
- Temp/locks: [7-Zip temp discussion](https://sourceforge.net/p/sevenzip/discussion/45797/thread/208a2bd611/) · [Storage Sense behavior Q&A](https://answers.microsoft.com/en-us/windows/forum/all/storage-sense-configuration-for-deleting-temporary/321616d1-e6a7-413a-8246-28f88f5ecc4e) · [Old New Thing: who has the file open](https://devblogs.microsoft.com/oldnewthing/20120217-00/?p=8283) · [RmGetList](https://learn.microsoft.com/en-us/windows/win32/api/restartmanager/nf-restartmanager-rmgetlist) · [CrowdStrike Restart Manager pt.1](https://www.crowdstrike.com/en-us/blog/windows-restart-manager-part-1/)
