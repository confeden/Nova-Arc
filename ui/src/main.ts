import { invoke } from "@tauri-apps/api/core";
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

type OpProgress = {
  op: string;
  files_done: number;
  files_total: number;
  bytes_done: number;
  bytes_total: number;
};

type OpResult = { op: string; ok: boolean; message: string; details: string[] };

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const el = {
  rows: $<HTMLTableSectionElement>("rows"),
  empty: $<HTMLDivElement>("empty"),
  summary: $<HTMLElement>("summary"),
  status: $<HTMLSpanElement>("status"),
  machine: $<HTMLSpanElement>("machine"),
  progressWrap: $<HTMLDivElement>("progress-wrap"),
  progress: $<HTMLDivElement>("progress"),
  level: $<HTMLSelectElement>("level"),
  memory: $<HTMLSelectElement>("memory"),
  checkAll: $<HTMLInputElement>("check-all"),
  menu: $<HTMLDivElement>("menu"),
};

const state = {
  archive: "" as string,
  entries: [] as Entry[],
  selected: new Set<string>(),
  sortKey: "path" as keyof Entry,
  sortAsc: true,
  busy: false,
  menuTarget: "" as string,
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

/** A file-type glyph. Deliberately text: shell icons come later, and an
 *  archiver that stalls its list waiting for icon extraction is worse than
 *  one that shows the list instantly. */
function glyph(path: string): string {
  const ext = path.slice(path.lastIndexOf(".") + 1).toLowerCase();
  if (/^(jpg|jpeg|png|gif|bmp|webp|heic|avif|tif|tiff|svg)$/.test(ext)) return "🖼";
  if (/^(mp3|flac|wav|ogg|m4a|aac|opus)$/.test(ext)) return "🎵";
  if (/^(mp4|mkv|avi|mov|webm|wmv)$/.test(ext)) return "🎬";
  if (/^(zip|7z|rar|gz|xz|bz2|zst|narc)$/.test(ext)) return "🗜";
  if (/^(exe|dll|so|dylib|msi|sys)$/.test(ext)) return "⚙";
  if (/^(pdf|doc|docx|odt|rtf|txt|md)$/.test(ext)) return "📄";
  if (/^(rs|c|h|cpp|py|js|ts|java|go|cs|json|toml|yaml|yml|xml|html|css|sh)$/.test(ext)) return "📝";
  return "📦";
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
  if (!busy) el.progress.style.width = "0%";
  if (text) setStatus(text);
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

function renderRows() {
  const rows = [...state.entries].sort((a, b) => {
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
        <td class="col-check"><input type="checkbox"${checked} /></td>
        <td><span class="name"><span class="ico">${glyph(e.path)}</span>${escapeHtml(e.path)}${
          e.solid ? ' <span class="solid">в блоке</span>' : ""
        }</span></td>
        <td class="col-size">${human(e.size)}</td>
        <td class="col-size">${human(e.stored)}</td>
        <td class="col-date">${when(e.mtime)}</td>
      </tr>`;
    })
    .join("");
  el.rows.innerHTML = html;
  el.empty.classList.toggle("hidden", state.entries.length > 0);
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
    el.checkAll.checked = false;
    renderSummary(info);
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
    filters: [{ name: "Архив Nova Arc", extensions: ["narc"] }],
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
    defaultPath: "archive.narc",
    filters: [{ name: "Архив Nova Arc", extensions: ["narc"] }],
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

el.checkAll.addEventListener("change", () => {
  state.selected.clear();
  if (el.checkAll.checked) for (const e of state.entries) state.selected.add(e.path);
  renderRows();
  refreshButtons();
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

// Drag & drop from Explorer: an archive opens, anything else is packed.
void getCurrentWebview().onDragDropEvent(async (event) => {
  if (event.payload.type !== "drop" || state.busy) return;
  const paths = event.payload.paths;
  if (paths.length === 1 && paths[0].toLowerCase().endsWith(".narc")) {
    await loadArchive(paths[0]);
  } else if (state.archive) {
    await addToArchive(paths);
  } else {
    await createArchive(paths);
  }
});

void listen<OpProgress>("narc://progress", (ev) => {
  const p = ev.payload;
  const pct = p.bytes_total > 0 ? (100 * p.bytes_done) / p.bytes_total : 0;
  el.progress.style.width = `${Math.min(100, pct).toFixed(1)}%`;
  setStatus(
    `${p.op === "extract" ? "Распаковка" : "Упаковка"}: ${p.files_done}/${p.files_total} — ${human(
      p.bytes_done,
    )} из ${human(p.bytes_total)}`,
  );
});

void listen<OpResult>("narc://done", async (ev) => {
  const r = ev.payload;
  setBusy(false);
  if (!r.ok) {
    setStatus(r.message, true);
    return;
  }
  setStatus(r.details.join(" · ") || "Готово");
  if (state.archive) await loadArchive(state.archive);
});

void (async () => {
  const m = await invoke<{ cores: number; memory_total: number | null; budget: number }>("machine_info");
  el.machine.textContent = `${m.cores} потоков · бюджет памяти ${human(m.budget)}`;
  refreshButtons();
})();
