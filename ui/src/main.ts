import { listen } from "@tauri-apps/api/event";
import {
  type DownloadTask,
  type FinishedEvent,
  type ProgressEvent,
  cancelDownload,
  listDownloads,
  startDownload,
} from "./api";
import "./styles.css";

const form = document.querySelector<HTMLFormElement>("#add-form")!;
const urlInput = document.querySelector<HTMLInputElement>("#url")!;
const destInput = document.querySelector<HTMLInputElement>("#dest")!;
const listEl = document.querySelector<HTMLUListElement>("#downloads")!;
const emptyEl = document.querySelector<HTMLParagraphElement>("#empty")!;

const tasks = new Map<number, DownloadTask>();

function formatBytes(n: number): string {
  if (n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** i).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

function render(): void {
  listEl.replaceChildren();
  emptyEl.hidden = tasks.size > 0;

  for (const task of tasks.values()) {
    const li = document.createElement("li");
    li.className = "download";

    const info = document.createElement("div");
    info.className = "download-info";

    const url = document.createElement("div");
    url.className = "download-url";
    url.textContent = task.url;

    const meta = document.createElement("div");
    meta.className = "download-meta";
    const total = task.total_bytes || 0;
    const pct = total > 0 ? Math.round((task.done_bytes / total) * 100) : 0;
    meta.textContent = `${task.status} · ${formatBytes(task.done_bytes)}${total > 0 ? ` / ${formatBytes(total)} (${pct}%)` : ""}`;

    const bar = document.createElement("div");
    bar.className = "progress";
    const fill = document.createElement("div");
    fill.className = "progress-fill";
    fill.style.width = `${total > 0 ? pct : 0}%`;
    bar.appendChild(fill);

    const cancel = document.createElement("button");
    cancel.className = "cancel";
    cancel.textContent = "✕";
    cancel.title = "Cancel";
    cancel.addEventListener("click", () => {
      void cancelDownload(task.id);
    });

    info.appendChild(url);
    info.appendChild(meta);
    li.appendChild(info);
    li.appendChild(bar);
    li.appendChild(cancel);
    listEl.appendChild(li);
  }
}

form.addEventListener("submit", (e) => {
  e.preventDefault();
  const url = urlInput.value.trim();
  if (!url) return;
  const dest = destInput.value.trim() || "downloads";
  void startDownload(url, dest).then((id) => {
    tasks.set(id, {
      id,
      url,
      destination: dest,
      status: "queued",
      done_bytes: 0,
      total_bytes: 0,
    });
    urlInput.value = "";
    render();
  });
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
    task.status = event.payload.ok ? "done" : `failed${event.payload.error ? `: ${event.payload.error}` : ""}`;
    task.done_bytes = task.total_bytes;
    render();
  }
});

void listDownloads().then((list) => {
  tasks.clear();
  for (const task of list) tasks.set(task.id, task);
  render();
});