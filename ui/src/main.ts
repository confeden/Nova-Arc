import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";

type Entry = {
  path: string;
  size: number;
  stored: number;
  mtime: number;
  solid: boolean;
};

type ArchiveInfo = {
  path: string;
  generation: number;
  files: number;
  chunks: number;
  file_len: number;
  live_bytes: number;
  reclaimable: number;
  total_size: number;
  entries: Entry[];
};

type Phase = "scan" | "work" | "drain" | "commit" | "done";

type OpProgress = {
  op: string;
  phase: Phase;
  files_done: number;
  files_total: number;
  /** Source bytes whose work is finished; reaches the total exactly once. */
  bytes_done: number;
  bytes_total: number;
  /** Source bytes read ahead of that — the gap is what is being compressed. */
  bytes_read: number;
  bytes_stored: number;
  /** Blocks finished / submitted. One block is 32-64 MiB at the max tier and
   *  takes tens of seconds, so this is the only honest account of a long wait. */
  units_done: number;
  units_total: number;
};

type OpResult = { op: string; ok: boolean; message: string; details: string[] };

/** Outside the app (plain browser, `npm run dev`) there is no Rust side, so
 *  the UI runs on sample data. That makes layout work a page reload instead of
 *  a full rebuild, and keeps the real code path untouched. */
const IN_APP = "__TAURI_INTERNALS__" in window;

const DEMO: ArchiveInfo = {
  path: "D:/demo/пример.nva",
  generation: 3,
  files: 4,
  chunks: 7,
  file_len: 2_411_235,
  live_bytes: 2_400_000,
  reclaimable: 4_200_000,
  total_size: 21_233_664,
  entries: [
    { path: "проект/src/main.rs", size: 18_233, stored: 3_120, mtime: 1_755_400_000, solid: true },
    { path: "проект/очень/длинный/путь/который/должен/обрезаться/а/не/ломать/таблицу/файл.txt", size: 2_048, stored: 512, mtime: 1_755_300_000, solid: true },
    { path: "фото/DSC_0001.jpg", size: 6_200_000, stored: 6_190_000, mtime: 1_700_000_000, solid: false },
    { path: "bin/tool.exe", size: 15_013_383, stored: 2_400_000, mtime: 0, solid: false },
  ],
};

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (IN_APP) return tauriInvoke<T>(cmd, args);
  if (cmd === "machine_info") return { cores: 8, memory_total: null, budget: 5 << 30 } as T;
  if (cmd === "open_archive" || cmd === "startup_archive") return DEMO as T;
  return null as T;
}

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const el = {
  rows: $<HTMLTableSectionElement>("rows"),
  filter: $<HTMLInputElement>("filter"),
  empty: $<HTMLDivElement>("empty"),
  summary: $<HTMLElement>("summary"),
  status: $<HTMLSpanElement>("status"),
  machine: $<HTMLSpanElement>("machine"),
  progressWrap: $<HTMLDivElement>("progress-wrap"),
  progress: $<HTMLDivElement>("progress"),
  progressRead: $<HTMLDivElement>("progress-read"),
  level: $<HTMLSelectElement>("level"),
  memory: $<HTMLSelectElement>("memory"),
  checkAll: $<HTMLInputElement>("check-all"),
  menu: $<HTMLDivElement>("menu"),
  tree: $<HTMLElement>("tree"),
};

const state = {
  archive: "" as string,
  entries: [] as Entry[],
  filter: "",
  selected: new Set<string>(),
  sortKey: "path" as keyof Entry,
  sortAsc: true,
  busy: false,
  menuTarget: "" as string,
  /** Selected folder, "" for the whole archive. Narrows the table the same way
   *  the filter does, and the two compose. */
  folder: "",
  /** Folders drawn open. A 5751-file tree fully expanded is not navigation, so
   *  only the path down to the selection starts open. */
  expanded: new Set<string>(),
  /** Last reading, and when it arrived. The clock lives here, not in the core:
   *  nova-core reports on data movement, and speed/ETA/"nothing for a while"
   *  all need arrival times, which the webview knows for free. */
  prog: null as OpProgress | null,
  progAt: 0,
  startedAt: 0,
  /** Bytes/s, exponentially smoothed: a deduplicated unit costs nothing and a
   *  unique max-tier one costs three encode passes, so the raw rate is spiky. */
  rate: 0,
  ratePrev: 0,
  /** The bar may never go backwards, whatever the numbers do. */
  lastPct: 0,
  ticker: 0 as ReturnType<typeof setInterval> | 0,
};

function human(n: number): string {
  const u = ["B", "КиБ", "МиБ", "ГиБ", "ТиБ"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(1)} ${u[i]}`;
}

function when(unix: number): string {
  if (!unix) return "—";
  const d = new Date(unix * 1000);
  const p = (x: number) => String(x).padStart(2, "0");
  return `${p(d.getDate())}.${p(d.getMonth() + 1)}.${d.getFullYear()} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** File-type icons as inline SVG rather than emoji: emoji depend on the
 *  system emoji font, render at inconsistent sizes and pick up colour
 *  fringing at 14 px, while these are crisp and follow the theme colour. */
const ICONS: Record<string, string> = {
  image: "M3 4h14v12H3zM3 13l4-4 3 3 3-4 4 5",
  audio: "M8 14V5l7-2v9M8 14a2 2 0 1 1-3 1.7A2 2 0 0 1 8 14m7-2a2 2 0 1 1-3 1.7A2 2 0 0 1 15 12",
  video: "M3 5h11v10H3zM14 8l4-2v8l-4-2",
  archive: "M4 3h12v14H4zM10 3v5M8 8h4M9 11h2v3H9z",
  exe: "M10 3l6 3v5c0 3-2.6 5-6 6-3.4-1-6-3-6-6V6zM7 10l2 2 4-4",
  doc: "M5 2h7l4 4v12H5zM12 2v4h4M8 11h6M8 14h6",
  code: "M7 6 3 10l4 4M13 6l4 4-4 4",
  generic: "M4 3h12v14H4zM7 7h6M7 10h6M7 13h4",
};

function iconKind(path: string): keyof typeof ICONS {
  const ext = path.slice(path.lastIndexOf(".") + 1).toLowerCase();
  if (/^(jpg|jpeg|png|gif|bmp|webp|heic|avif|tif|tiff|svg|dds|tga)$/.test(ext)) return "image";
  if (/^(mp3|flac|wav|ogg|m4a|aac|opus|mid)$/.test(ext)) return "audio";
  if (/^(mp4|mkv|avi|mov|webm|wmv|m4v)$/.test(ext)) return "video";
  if (/^(zip|7z|rar|gz|xz|bz2|zst|nva|tar|cab|iso)$/.test(ext)) return "archive";
  if (/^(exe|dll|so|dylib|msi|sys|bin|elf)$/.test(ext)) return "exe";
  if (/^(pdf|doc|docx|odt|rtf|txt|md|log)$/.test(ext)) return "doc";
  if (/^(rs|c|h|hpp|cpp|py|js|ts|tsx|java|go|cs|rb|php|json|toml|yaml|yml|xml|html|css|sh|ps1|bat)$/.test(ext))
    return "code";
  return "generic";
}

function icon(path: string): string {
  const d = ICONS[iconKind(path)];
  return `<svg class="ico" viewBox="0 0 20 20" aria-hidden="true"><path d="${d}"/></svg>`;
}

function setBusy(busy: boolean, text?: string) {
  state.busy = busy;
  document.body.classList.toggle("busy", busy);
  for (const id of ["btn-open", "btn-new", "btn-extract", "btn-add", "btn-remove", "btn-compact"]) {
    const b = $<HTMLButtonElement>(id);
    if (busy) b.disabled = true;
  }
  if (!busy) refreshButtons();
  el.progressWrap.classList.toggle("hidden", !busy);
  if (busy) {
    // Fresh operation: forget everything about the previous one, or its rate
    // and its width bleed into the first frames of this one.
    state.prog = null;
    state.progAt = 0;
    state.startedAt = performance.now();
    state.rate = 0;
    state.ratePrev = 0;
    state.lastPct = 0;
    el.progress.style.width = "0%";
    el.progressRead.style.width = "0%";
    el.progressWrap.classList.remove("indeterminate", "stalled");
    if (!state.ticker) state.ticker = setInterval(renderProgress, 200);
  } else if (state.ticker) {
    clearInterval(state.ticker);
    state.ticker = 0;
  }
  // The width is deliberately NOT reset here: the last frame of a successful
  // operation used to be an ERASED bar, so a full one was never seen.
  if (text) setStatus(text);
}

// At the max tier the reader consumes a 113 MiB tree in under a second and the
// codecs then work for another 38, so "drain" is not a brief tail — it is most
// of the operation. Its label has to say what is happening, not that something
// is finishing up.
const PHASE_TEXT: Record<Phase, (op: string) => string> = {
  scan: () => "Сканирование",
  work: (op) => (op === "extract" ? "Распаковка" : "Упаковка"),
  drain: (op) => (op === "extract" ? "Распаковка" : "Сжатие"),
  commit: () => "Запись оглавления",
  done: () => "Готово",
};

function hhmmss(sec: number): string {
  const s = Math.max(0, Math.round(sec));
  if (s < 60) return `${s} с`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} мин ${String(s % 60).padStart(2, "0")} с`;
  return `${Math.floor(m / 60)} ч ${String(m % 60).padStart(2, "0")} мин`;
}

/** Draw on a timer, not on arrival. One max-tier unit takes tens of seconds and
 *  produces no event, so an event-driven bar simply stops repainting — which is
 *  exactly what "always at 100% with an unknown wait" looked like. */
function renderProgress() {
  const p = state.prog;
  if (!state.busy) return;
  const now = performance.now();
  const elapsed = (now - state.startedAt) / 1000;
  if (!p) {
    setStatus(`Подготовка… ${hhmmss(elapsed)}`);
    return;
  }
  // No denominator: running, cannot say how far.
  const measurable = p.bytes_total > 0;
  el.progressWrap.classList.toggle("indeterminate", !measurable);
  el.progressWrap.classList.toggle(
    "stalled",
    measurable && p.phase !== "done" && now - state.progAt > 1500,
  );
  if (measurable) {
    const raw = (100 * p.bytes_done) / p.bytes_total;
    // Clamped below 100 until the core says Done, so a full bar means finished
    // and nothing else.
    const cap = p.phase === "done" ? 100 : 99;
    state.lastPct = Math.min(cap, Math.max(state.lastPct, raw));
    el.progress.style.width = `${state.lastPct.toFixed(1)}%`;
    el.progressRead.style.width = `${Math.min(100, (100 * p.bytes_read) / p.bytes_total).toFixed(1)}%`;
  }

  const head = PHASE_TEXT[p.phase](p.op);
  const parts: string[] = [];
  if (p.phase === "scan") {
    parts.push(`${p.files_total} файл(ов) найдено`);
  } else if (measurable) {
    parts.push(`${state.lastPct.toFixed(0)}%`);
    parts.push(`${human(p.bytes_done)} из ${human(p.bytes_total)}`);
    if (p.files_total > 0) parts.push(`${p.files_done}/${p.files_total} файлов`);
    // A block is 32-64 MiB at the max tier and yields nothing until it is done,
    // so saying "block 3 of 6" is the difference between a wait and a hang.
    if (p.units_total > 1) parts.push(`блок ${p.units_done}/${p.units_total}`);
  }
  if (state.rate > 0) parts.push(`${human(state.rate)}/с`);
  // An estimate before a few seconds of data is worse than none, and there is
  // nothing to estimate once the source is consumed.
  const left = measurable ? p.bytes_total - p.bytes_done : 0;
  if (state.rate > 0 && elapsed > 3 && left > 0 && (p.phase === "work" || p.phase === "drain")) {
    parts.push(`осталось ~${hhmmss(left / state.rate)}`);
  } else {
    parts.push(hhmmss(elapsed));
  }
  setStatus(`${head}: ${parts.join(" · ")}`);
}

function setStatus(text: string, error = false) {
  el.status.textContent = text;
  el.status.parentElement!.classList.toggle("err", error);
}

function refreshButtons() {
  const has = state.archive !== "";
  const hasSel = state.selected.size > 0;
  $<HTMLButtonElement>("btn-extract").disabled = !has;
  $<HTMLButtonElement>("btn-add").disabled = !has;
  $<HTMLButtonElement>("btn-compact").disabled = !has;
  $<HTMLButtonElement>("btn-remove").disabled = !has || !hasSel;
  $<HTMLButtonElement>("btn-open").disabled = false;
  $<HTMLButtonElement>("btn-new").disabled = false;
}

function renderSummary(info: ArchiveInfo) {
  const ratio = info.total_size > 0 ? (100 * info.file_len) / info.total_size : 0;
  el.summary.classList.remove("hidden");
  el.summary.innerHTML = [
    `<span>Архив: <b>${info.path.replace(/^.*[\\/]/, "")}</b></span>`,
    `<span>Файлов: <b>${info.files}</b></span>`,
    `<span>Содержимое: <b>${human(info.total_size)}</b></span>`,
    `<span>Размер архива: <b>${human(info.file_len)}</b> (${ratio.toFixed(1)}%)</span>`,
    info.reclaimable > 1024 * 1024
      ? `<span>Мусора: <b>${human(info.reclaimable)}</b> — можно «Уплотнить»</span>`
      : "",
    `<span>Версий: <b>${info.generation}</b></span>`,
  ].join("");
}

/** A folder in the archive. Archive paths are always '/'-separated, so the
 *  tree is derived from the entry list rather than stored: the format has no
 *  directory entries at all. */
type Folder = {
  name: string;
  path: string;
  kids: Map<string, Folder>;
  /** Files anywhere below, not just directly inside — the number that tells
   *  you whether a branch is worth opening. */
  files: number;
};

function buildTree(entries: Entry[]): Folder {
  const root: Folder = { name: "", path: "", kids: new Map(), files: entries.length };
  for (const e of entries) {
    const parts = e.path.split("/");
    let at = root;
    // The last part is the file itself and never becomes a folder.
    for (let i = 0; i < parts.length - 1; i++) {
      const name = parts[i];
      let kid = at.kids.get(name);
      if (!kid) {
        kid = { name, path: at.path ? `${at.path}/${name}` : name, kids: new Map(), files: 0 };
        at.kids.set(name, kid);
      }
      kid.files++;
      at = kid;
    }
  }
  return root;
}

function inFolder(path: string, folder: string): boolean {
  return folder === "" || path.startsWith(`${folder}/`);
}

function renderTree() {
  const root = buildTree(state.entries);
  // A flat archive has nothing to navigate; the panel would only take width.
  if (root.kids.size === 0) {
    el.tree.classList.add("hidden");
    el.tree.innerHTML = "";
    return;
  }
  el.tree.classList.remove("hidden");

  const draw = (f: Folder, depth: number): string => {
    const kids = [...f.kids.values()].sort((a, b) => a.name.localeCompare(b.name));
    const open = f.path === "" || state.expanded.has(f.path);
    const twist = kids.length ? (open ? "▾" : "▸") : "";
    const label = f.path === "" ? "Весь архив" : f.name;
    const row = `<div class="node${state.folder === f.path ? " on" : ""}"
        data-folder="${escapeHtml(f.path)}" style="padding-left:${6 + depth * 14}px"
        title="${escapeHtml(f.path || "Весь архив")}">
        <span class="twist${kids.length ? " has" : ""}">${twist}</span>
        <span class="label">${escapeHtml(label)}</span>
        <span class="count">${f.files}</span>
      </div>`;
    return row + (open ? kids.map((k) => draw(k, depth + 1)).join("") : "");
  };
  el.tree.innerHTML = draw(root, 0);
}

function visibleEntries(): Entry[] {
  const q = state.filter.trim().toLowerCase();
  return state.entries.filter(
    (e) => inFolder(e.path, state.folder) && (!q || e.path.toLowerCase().includes(q)),
  );
}

/** How an entry is named in the table: relative to the folder being viewed,
 *  because repeating the selected prefix on every row is noise. Selection and
 *  every command still use the full path. */
function displayPath(path: string): string {
  return state.folder ? path.slice(state.folder.length + 1) : path;
}

function renderRows() {
  const rows = [...visibleEntries()].sort((a, b) => {
    const k = state.sortKey;
    const va = a[k];
    const vb = b[k];
    const c = typeof va === "string" ? (va as string).localeCompare(vb as string) : Number(va) - Number(vb);
    return state.sortAsc ? c : -c;
  });
  const html = rows
    .map((e) => {
      const sel = state.selected.has(e.path) ? " class='sel'" : "";
      const checked = state.selected.has(e.path) ? " checked" : "";
      return `<tr${sel} data-path="${escapeHtml(e.path)}">
        <td><input type="checkbox"${checked} /></td>
        <td title="${escapeHtml(e.path)}"><span class="name">${icon(e.path)}<span class="text">${escapeHtml(
          displayPath(e.path),
        )}</span>${e.solid ? '<span class="solid">в блоке</span>' : ""}</span></td>
        <td class="num">${human(e.size)}</td>
        <td class="num">${human(e.stored)}</td>
        <td class="num">${when(e.mtime)}</td>
      </tr>`;
    })
    .join("");
  el.rows.innerHTML = html;
  el.empty.classList.toggle("hidden", rows.length > 0);
  if (state.entries.length > 0 && rows.length === 0) {
    el.empty.querySelector(".empty-title")!.textContent = "Ничего не найдено";
    el.empty.querySelectorAll("p").forEach((p, i) => {
      p.textContent = i === 0 ? "Под фильтр не подходит ни один файл." : "";
    });
  }
  // Show which column is sorted, so the header is never ambiguous.
  document.querySelectorAll("th.sortable").forEach((th) => {
    const key = (th as HTMLElement).dataset.sort;
    const label = (th as HTMLElement).dataset.label ?? th.textContent!.replace(/[ ↑↓]+$/, "");
    (th as HTMLElement).dataset.label = label;
    th.innerHTML =
      key === state.sortKey
        ? `${label} <span class="arrow">${state.sortAsc ? "↑" : "↓"}</span>`
        : label;
  });
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);
}

function packOpts() {
  const mem = Number(el.memory.value);
  return { level: el.level.value, memoryMib: mem === 0 ? null : mem };
}

async function loadArchive(path: string) {
  try {
    const info = await invoke<ArchiveInfo>("open_archive", { path });
    state.archive = info.path;
    state.entries = info.entries;
    state.selected.clear();
    // A different archive's folder is meaningless here, and after add/remove
    // the old one may not exist any more.
    if (!state.entries.some((e) => inFolder(e.path, state.folder))) state.folder = "";
    el.checkAll.checked = false;
    el.filter.disabled = false;
    renderSummary(info);
    renderTree();
    renderRows();
    refreshButtons();
    setStatus(`Открыт архив: ${info.files} файл(ов)`);
  } catch (e) {
    setStatus(String(e), true);
  }
}

async function pickArchiveToOpen() {
  const path = await openDialog({
    multiple: false,
    filters: [{ name: "Архив Nova Prism", extensions: ["nva"] }],
  });
  if (typeof path === "string") await loadArchive(path);
}

async function createArchive(inputs?: string[]) {
  if (!inputs) {
    const picked = await openDialog({ multiple: true, directory: false });
    const dirs = picked ? null : await openDialog({ multiple: true, directory: true });
    const chosen = (picked ?? dirs) as string | string[] | null;
    if (!chosen) return;
    inputs = Array.isArray(chosen) ? chosen : [chosen];
  }
  const target = await saveDialog({
    defaultPath: "archive.nva",
    filters: [{ name: "Архив Nova Prism", extensions: ["nva"] }],
  });
  if (!target) return;
  const o = packOpts();
  setBusy(true, "Упаковка…");
  await invoke("create_archive", {
    archive: target,
    inputs,
    level: o.level,
    threads: null,
    memoryMib: o.memoryMib,
  });
  state.archive = target;
}

async function addToArchive(inputs?: string[]) {
  if (!state.archive) return createArchive(inputs);
  if (!inputs) {
    const picked = await openDialog({ multiple: true, directory: false });
    if (!picked) return;
    inputs = Array.isArray(picked) ? picked : [picked];
  }
  const o = packOpts();
  setBusy(true, "Добавление…");
  await invoke("create_archive", {
    archive: state.archive,
    inputs,
    level: o.level,
    threads: null,
    memoryMib: o.memoryMib,
  });
}

async function extract(paths: string[]) {
  const dest = await openDialog({ directory: true, multiple: false });
  if (typeof dest !== "string") return;
  const o = packOpts();
  setBusy(true, paths.length ? `Распаковка (${paths.length})…` : "Распаковка…");
  await invoke("extract_archive", {
    archive: state.archive,
    dest,
    paths,
    overwrite: "skip",
    threads: null,
    memoryMib: o.memoryMib,
  });
}

function selectedPaths(): string[] {
  return [...state.selected];
}

// --- wiring ---------------------------------------------------------------

$("btn-open").addEventListener("click", () => void pickArchiveToOpen());
$("btn-new").addEventListener("click", () => void createArchive());
$("btn-add").addEventListener("click", () => void addToArchive());
$("btn-extract").addEventListener("click", () => void extract(selectedPaths()));
$("btn-compact").addEventListener("click", async () => {
  setBusy(true, "Уплотнение…");
  await invoke("compact_archive", { archive: state.archive });
});
$("btn-remove").addEventListener("click", async () => {
  const paths = selectedPaths();
  if (!paths.length) return;
  setBusy(true, "Удаление…");
  await invoke("remove_entries", { archive: state.archive, paths });
});

el.filter.addEventListener("input", () => {
  state.filter = el.filter.value;
  renderRows();
});

el.checkAll.addEventListener("change", () => {
  state.selected.clear();
  // Selecting "all" means all *visible* rows: with a filter on, anything else
  // would silently act on files the user cannot see.
  if (el.checkAll.checked) for (const e of visibleEntries()) state.selected.add(e.path);
  renderRows();
  refreshButtons();
});

el.tree.addEventListener("click", (ev) => {
  const node = (ev.target as HTMLElement).closest<HTMLElement>(".node");
  if (!node) return;
  const folder = node.dataset.folder!;
  // The chevron only opens and closes; anywhere else picks the folder. Mixing
  // the two into one click makes it impossible to peek without navigating.
  if ((ev.target as HTMLElement).classList.contains("twist")) {
    if (state.expanded.has(folder)) state.expanded.delete(folder);
    else state.expanded.add(folder);
  } else {
    state.folder = folder;
    if (folder) state.expanded.add(folder);
    // Selection is by full path and survives, but "select all" now means a
    // different set, so the header box must not claim otherwise.
    el.checkAll.checked = false;
    renderRows();
    refreshButtons();
  }
  renderTree();
});

el.rows.addEventListener("click", (ev) => {
  const tr = (ev.target as HTMLElement).closest("tr");
  if (!tr) return;
  const path = tr.dataset.path!;
  if (state.selected.has(path)) state.selected.delete(path);
  else state.selected.add(path);
  renderRows();
  refreshButtons();
});

el.rows.addEventListener("dblclick", async (ev) => {
  const tr = (ev.target as HTMLElement).closest("tr");
  if (!tr || state.busy) return;
  await openEntry(tr.dataset.path!);
});

async function openEntry(path: string) {
  setStatus("Открываю файл…");
  try {
    await invoke("open_entry", { archive: state.archive, path });
    setStatus("Файл открыт во временной папке (она удалится при выходе)");
  } catch (e) {
    setStatus(String(e), true);
  }
}

document.querySelectorAll("th[data-sort]").forEach((th) =>
  th.addEventListener("click", () => {
    const key = (th as HTMLElement).dataset.sort as keyof Entry;
    state.sortAsc = state.sortKey === key ? !state.sortAsc : true;
    state.sortKey = key;
    renderRows();
  }),
);

el.rows.addEventListener("contextmenu", (ev) => {
  const tr = (ev.target as HTMLElement).closest("tr");
  if (!tr) return;
  ev.preventDefault();
  state.menuTarget = tr.dataset.path!;
  el.menu.style.left = `${ev.clientX}px`;
  el.menu.style.top = `${ev.clientY}px`;
  el.menu.classList.remove("hidden");
});

document.addEventListener("click", () => el.menu.classList.add("hidden"));

el.menu.addEventListener("click", async (ev) => {
  const act = (ev.target as HTMLElement).dataset.act;
  el.menu.classList.add("hidden");
  if (!act || !state.menuTarget) return;
  if (act === "open") await openEntry(state.menuTarget);
  if (act === "extract-one") await extract([state.menuTarget]);
  if (act === "remove-one") {
    setBusy(true, "Удаление…");
    await invoke("remove_entries", { archive: state.archive, paths: [state.menuTarget] });
  }
});

// Everything below reaches into Tauri internals and THROWS outside the app,
// which took the rest of this module with it — including the startup load, so
// the browser demo mode this file opens by promising rendered a blank window.
if (IN_APP) {
  // Drag & drop from Explorer: an archive opens, anything else is packed.
  void getCurrentWebview().onDragDropEvent(async (event) => {
    if (event.payload.type !== "drop" || state.busy) return;
    const paths = event.payload.paths;
    if (paths.length === 1 && paths[0].toLowerCase().endsWith(".nva")) {
      await loadArchive(paths[0]);
    } else if (state.archive) {
      await addToArchive(paths);
    } else {
      await createArchive(paths);
    }
  });

  void listen<OpProgress>("nova://progress", (ev) => {
    const p = ev.payload;
    const now = performance.now();
    // Events arrive up to ~20 times a second and must not each drag three DOM
    // writes with them; they only update the model. renderProgress paints.
    const prev = state.prog;
    if (prev && now > state.progAt) {
      const dt = (now - state.progAt) / 1000;
      const db = p.bytes_done - prev.bytes_done;
      if (db >= 0 && dt > 0) {
        const inst = db / dt;
        state.rate = state.rate > 0 ? state.rate * 0.7 + inst * 0.3 : inst;
      }
    }
    state.prog = p;
    state.progAt = now;
  });

  void listen<OpResult>("nova://done", async (ev) => {
    const r = ev.payload;
    setBusy(false);
    // setBusy hides the track; on success bring it back full for a moment. The
    // whole point of a progress bar is the frame where it is complete, and that
    // frame used to be an erased bar.
    if (r.ok) {
      el.progressWrap.classList.remove("indeterminate", "stalled", "hidden");
      el.progress.style.width = "100%";
      el.progressRead.style.width = "100%";
      setTimeout(() => el.progressWrap.classList.add("hidden"), 600);
    }
    if (!r.ok) {
      setStatus(r.message, true);
      return;
    }
    setStatus(r.details.join(" · ") || "Готово");
    if (state.archive) await loadArchive(state.archive);
  });
}

void (async () => {
  const m = await invoke<{ cores: number; memory_total: number | null; budget: number }>("machine_info");
  el.machine.textContent = `${m.cores} потоков · бюджет памяти ${human(m.budget)}`;
  refreshButtons();
  // Opened with a path argument (double-click a .nva, or the shell
  // association)? Pull it now that listeners and state are ready.
  try {
    const startup = IN_APP ? await invoke<string | null>("startup_archive") : DEMO.path;
    if (startup) await loadArchive(startup);
  } catch (e) {
    setStatus(`Не удалось открыть переданный архив: ${e}`, true);
  }
})();
