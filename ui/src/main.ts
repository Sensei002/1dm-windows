import { listen } from "@tauri-apps/api/event";
import {
  type DownloadTask,
  type FinishedEvent,
  type GrabbedLink,
  type ProgressEvent,
  type Settings,
  type StreamInfo,
  cancelDownload,
  clearFinished,
  clearStreams,
  downloadStream,
  getSettings,
  grabSite,
  listDownloads,
  listStreams,
  openBrowser,
  pauseDownload,
  resumeDownload,
  setSettings,
  startDownload,
} from "./api";
import "./styles.css";

// ---------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------

const tabsEl = document.querySelector<HTMLDivElement>("#tabs")!;
tabsEl.addEventListener("click", (e) => {
  const btn = (e.target as HTMLElement).closest<HTMLButtonElement>(".tab");
  if (!btn) return;
  for (const t of document.querySelectorAll(".tab")) t.classList.remove("active");
  btn.classList.add("active");
  for (const p of document.querySelectorAll(".tab-panel")) p.classList.remove("active");
  document.querySelector(`#tab-${btn.dataset.tab}`)?.classList.add("active");
  if (btn.dataset.tab === "browser") void refreshStreams();
});

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatBytes(n: number): string {
  if (n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** i).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

function kindIcon(kind: string): string {
  switch (kind) {
    case "hls": return "📺";
    case "dash": return "🎬";
    case "torrent": return "🧲";
    default: return "⬇";
  }
}

function ghostButton(label: string, title: string): HTMLButtonElement {
  const b = document.createElement("button");
  b.className = "ghost";
  b.textContent = label;
  b.title = title;
  return b;
}

// ---------------------------------------------------------------------------
// Downloads queue
// ---------------------------------------------------------------------------

const form = document.querySelector<HTMLFormElement>("#add-form")!;
const urlInput = document.querySelector<HTMLInputElement>("#url")!;
const destInput = document.querySelector<HTMLInputElement>("#dest")!;
const listEl = document.querySelector<HTMLUListElement>("#downloads")!;
const emptyEl = document.querySelector<HTMLParagraphElement>("#empty")!;
const clearBtn = document.querySelector<HTMLButtonElement>("#clear-finished")!;
const scheduleAt = document.querySelector<HTMLInputElement>("#schedule-at")!;
const scheduleAdd = document.querySelector<HTMLButtonElement>("#schedule-add")!;

let tasks = new Map<number, DownloadTask>();

function render(): void {
  listEl.replaceChildren();
  emptyEl.hidden = tasks.size > 0;

  for (const task of tasks.values()) {
    const li = document.createElement("li");
    li.className = "download";
    if (task.error) li.title = task.error;

    const icon = document.createElement("span");
    icon.className = "dl-icon";
    icon.textContent = kindIcon(task.kind);

    const info = document.createElement("div");
    info.className = "download-info";

    const url = document.createElement("div");
    url.className = "download-url";
    url.textContent = task.url;

    const meta = document.createElement("div");
    meta.className = "download-meta";
    const total = task.total_bytes || 0;
    const pct = total > 0 ? Math.round((task.done_bytes / total) * 100) : 0;
    const size = total > 0 ? ` · ${formatBytes(task.done_bytes)} / ${formatBytes(total)} (${pct}%)` : "";
    const when = task.start_at && task.start_at > Date.now()
      ? ` · scheduled ${new Date(task.start_at).toLocaleString()}`
      : "";
    meta.textContent = `${task.status}${size}${when}`;
    if (task.error) meta.textContent += ` — ${task.error}`;

    const bar = document.createElement("div");
    bar.className = "progress";
    const fill = document.createElement("div");
    fill.className = "progress-fill";
    if (task.status === "paused") fill.classList.add("paused");
    fill.style.width = `${total > 0 && task.status !== "done" ? pct : task.status === "done" ? 100 : 0}%`;
    bar.appendChild(fill);

    const actions = document.createElement("div");
    actions.className = "dl-actions";
    if (task.status === "downloading") {
      const pause = ghostButton("⏸", "Pause");
      pause.addEventListener("click", () => void pauseDownload(task.id));
      actions.appendChild(pause);
    } else if (task.status === "paused") {
      const resume = ghostButton("▶", "Resume");
      resume.addEventListener("click", () => void resumeDownload(task.id));
      actions.appendChild(resume);
    }
    if (task.status !== "done" && task.status !== "cancelled") {
      const cancel = ghostButton("✕", "Cancel");
      cancel.addEventListener("click", () => void cancelDownload(task.id));
      actions.appendChild(cancel);
    }

    info.appendChild(url);
    info.appendChild(meta);
    li.appendChild(icon);
    li.appendChild(info);
    li.appendChild(bar);
    li.appendChild(actions);
    listEl.appendChild(li);
  }
}

form.addEventListener("submit", (e) => {
  e.preventDefault();
  const url = urlInput.value.trim();
  if (!url) return;
  const dest = destInput.value.trim() || undefined;
  void startDownload({ url, destination: dest }).then(refreshQueue);
  urlInput.value = "";
});

scheduleAdd.addEventListener("click", () => {
  const url = urlInput.value.trim();
  const at = scheduleAt.value ? new Date(scheduleAt.value).getTime() : 0;
  if (!url || !at) return;
  void startDownload({ url, destination: destInput.value.trim() || undefined, startAt: at }).then(refreshQueue);
  urlInput.value = "";
});

clearBtn.addEventListener("click", () => {
  void clearFinished().then(refreshQueue);
});

async function refreshQueue(): Promise<void> {
  tasks = new Map((await listDownloads()).map((t) => [t.id, t]));
  render();
}

void listen<DownloadTask[]>("queue://updated", (event) => {
  tasks = new Map(event.payload.map((t) => [t.id, t]));
  render();
});

void listen<ProgressEvent>("download://progress", (event) => {
  const task = tasks.get(event.payload.id);
  if (task) {
    task.status = "downloading";
    task.done_bytes = event.payload.done;
    task.total_bytes = event.payload.total;
    render();
  }
});

void listen<FinishedEvent>("download://finished", (event) => {
  const task = tasks.get(event.payload.id);
  if (task) {
    task.status = event.payload.ok
      ? "done"
      : "failed";
    task.error = event.payload.error;
    render();
  }
});

// ---------------------------------------------------------------------------
// Streams (browser sniffer)
// ---------------------------------------------------------------------------

const streamsEl = document.querySelector<HTMLUListElement>("#streams")!;
const streamsEmpty = document.querySelector<HTMLParagraphElement>("#streams-empty")!;
const clearStreamsBtn = document.querySelector<HTMLButtonElement>("#clear-streams")!;

let streams: StreamInfo[] = [];

function renderStreams(): void {
  streamsEl.replaceChildren();
  streamsEmpty.hidden = streams.length > 0;
  for (const s of streams) {
    const li = document.createElement("li");
    li.className = "stream";

    const head = document.createElement("div");
    head.className = "stream-head";
    const title = document.createElement("span");
    title.className = "stream-title";
    title.textContent = `${s.is_drm ? "🔒 " : ""}${s.title}`;
    const kind = document.createElement("span");
    kind.className = "badge";
    kind.textContent = s.kind;
    head.appendChild(title);
    head.appendChild(kind);

    const url = document.createElement("div");
    url.className = "stream-url";
    url.textContent = s.url;

    const actions = document.createElement("div");
    actions.className = "stream-actions";

    const qSel = document.createElement("select");
    qSel.className = "quality";
    (s.qualities.length ? s.qualities : [{ label: "best", url: s.url }]).forEach((q, i) => {
      const opt = document.createElement("option");
      opt.value = String(i);
      opt.textContent = q.label;
      qSel.appendChild(opt);
    });
    qSel.disabled = s.is_drm;

    const dl = document.createElement("button");
    dl.textContent = s.is_drm ? "DRM 🔒" : "Download";
    dl.disabled = s.is_drm;
    dl.addEventListener("click", () => {
      if (s.is_drm) return;
      void downloadStream({ id: s.id, qualityIndex: Number(qSel.value) }).then(refreshQueue);
    });

    const play = ghostButton("Open page", "Open the source page in the embedded browser");
    play.addEventListener("click", () => void openBrowser(s.page_url));

    actions.appendChild(qSel);
    actions.appendChild(dl);
    actions.appendChild(play);

    li.appendChild(head);
    li.appendChild(url);
    li.appendChild(actions);
    streamsEl.appendChild(li);
  }
}

async function refreshStreams(): Promise<void> {
  streams = await listStreams();
  renderStreams();
}

clearStreamsBtn.addEventListener("click", () => {
  void clearStreams().then(refreshStreams);
});

void listen<StreamInfo>("stream://detected", (event) => {
  const existing = streams.find((s) => s.id === event.payload.id);
  if (existing) {
    Object.assign(existing, event.payload);
  } else {
    streams.unshift(event.payload);
  }
  renderStreams();
});

// ---------------------------------------------------------------------------
// Browser launcher
// ---------------------------------------------------------------------------

const browserForm = document.querySelector<HTMLFormElement>("#browser-form")!;
const browserUrl = document.querySelector<HTMLInputElement>("#browser-url")!;

browserForm.addEventListener("submit", (e) => {
  e.preventDefault();
  const url = browserUrl.value.trim();
  if (!url) return;
  void openBrowser(url);
  browserUrl.value = "";
});

// DRM dialog
const drmModal = document.querySelector<HTMLDivElement>("#drm-modal")!;
document.querySelector<HTMLButtonElement>("#drm-close")!
  .addEventListener("click", () => drmModal.classList.add("hidden"));
document.querySelector<HTMLButtonElement>("#drm-play")!
  .addEventListener("click", () => {
    drmModal.classList.add("hidden");
    if (drmPage) void openBrowser(drmPage);
  });
let drmPage: string | null = null;

streamsEl.addEventListener("click", (e) => {
  const dl = (e.target as HTMLElement).closest("button");
  if (dl && dl.textContent === "DRM 🔒") {
    const li = dl.closest("li");
    if (!li) return;
    const idx = Array.from(streamsEl.children).indexOf(li);
    drmPage = streams[idx]?.page_url ?? null;
    drmModal.classList.remove("hidden");
  }
});

// ---------------------------------------------------------------------------
// Grabber
// ---------------------------------------------------------------------------

const grabForm = document.querySelector<HTMLFormElement>("#grab-form")!;
const grabUrl = document.querySelector<HTMLInputElement>("#grab-url")!;
const grabResults = document.querySelector<HTMLUListElement>("#grab-results")!;
const grabEmpty = document.querySelector<HTMLParagraphElement>("#grab-empty")!;
const grabDownload = document.querySelector<HTMLButtonElement>("#grab-download")!;

let grabbed: GrabbedLink[] = [];

function renderGrab(): void {
  grabResults.replaceChildren();
  grabEmpty.hidden = grabbed.length > 0;
  grabEmpty.textContent = "Scan a page to find downloadable links.";
  for (const link of grabbed) {
    const li = document.createElement("li");
    li.className = "grab-item";

    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.value = link.url;
    cb.className = "grab-check";
    cb.addEventListener("change", updateGrabButton);

    const text = document.createElement("span");
    text.className = "grab-text";
    text.textContent = `${link.text} [${link.ext}]`;

    const url = document.createElement("span");
    url.className = "grab-url";
    url.textContent = link.url;

    li.appendChild(cb);
    li.appendChild(text);
    li.appendChild(url);
    grabResults.appendChild(li);
  }
  updateGrabButton();
}

function updateGrabButton(): void {
  grabDownload.disabled = grabResults.querySelectorAll(".grab-check:checked").length === 0;
}

grabForm.addEventListener("submit", (e) => {
  e.preventDefault();
  const url = grabUrl.value.trim();
  if (!url) return;
  grabEmpty.textContent = "Scanning…";
  grabEmpty.hidden = false;
  void grabSite(url).then((links) => {
    grabbed = links;
    renderGrab();
  });
});

grabDownload.addEventListener("click", () => {
  const checked = Array.from(grabResults.querySelectorAll<HTMLInputElement>(".grab-check:checked"));
  for (const cb of checked) {
    void startDownload({ url: cb.value }).then(refreshQueue);
  }
});

// ---------------------------------------------------------------------------
// Clipboard toast
// ---------------------------------------------------------------------------

const toast = document.querySelector<HTMLDivElement>("#toast")!;
const toastText = document.querySelector<HTMLSpanElement>("#toast-text")!;
let toastUrl: string | null = null;

function showToast(url: string): void {
  toastUrl = url;
  toastText.textContent = url.length > 60 ? `${url.slice(0, 57)}…` : url;
  toast.classList.remove("hidden");
}

document.querySelector<HTMLButtonElement>("#toast-add")!
  .addEventListener("click", () => {
    if (toastUrl) void startDownload({ url: toastUrl }).then(refreshQueue);
    toast.classList.add("hidden");
  });
document.querySelector<HTMLButtonElement>("#toast-close")!
  .addEventListener("click", () => toast.classList.add("hidden"));

void listen<string>("clipboard://url", (event) => showToast(event.payload));

// ---------------------------------------------------------------------------
// Settings + theme
// ---------------------------------------------------------------------------

const setConcurrency = document.querySelector<HTMLInputElement>("#set-concurrency")!;
const setDefaultDir = document.querySelector<HTMLInputElement>("#set-default-dir")!;
const setClipboard = document.querySelector<HTMLInputElement>("#set-clipboard")!;
const setMp4 = document.querySelector<HTMLInputElement>("#set-mp4")!;
const setTheme = document.querySelector<HTMLSelectElement>("#set-theme")!;

function applyTheme(theme: string): void {
  document.documentElement.dataset.theme = theme;
  localStorage.setItem("onedm-theme", theme);
}

async function loadSettings(): Promise<void> {
  const s: Settings = await getSettings();
  setConcurrency.value = String(s.max_concurrent);
  setDefaultDir.value = s.default_dir;
  setClipboard.checked = s.clipboard_enabled;
  setMp4.checked = s.mp4_output;
}

let saveTimer: number | undefined;
function queueSave(): void {
  window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    void setSettings({
      max_concurrent: Math.max(1, Math.min(16, Number(setConcurrency.value) || 3)),
      clipboard_enabled: setClipboard.checked,
      mp4_output: setMp4.checked,
      default_dir: setDefaultDir.value.trim(),
    });
  }, 350);
}

for (const el of [setConcurrency, setDefaultDir, setClipboard, setMp4]) {
  el.addEventListener("change", queueSave);
}

setTheme.addEventListener("change", () => applyTheme(setTheme.value));

const savedTheme = localStorage.getItem("onedm-theme") ?? "dark";
setTheme.value = savedTheme;
applyTheme(savedTheme);

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

void refreshQueue();
void loadSettings();
