import { invoke } from "@tauri-apps/api/core";

export interface DownloadTask {
  id: number;
  url: string;
  destination: string;
  status: string;
  done_bytes: number;
  total_bytes: number;
  kind: string;
  connections: number;
  start_at: number | null;
  rep_id: string | null;
  error: string | null;
}

export interface ProgressEvent {
  id: number;
  done: number;
  total: number;
}

export interface FinishedEvent {
  id: number;
  ok: boolean;
  error: string | null;
}

export interface Quality {
  label: string;
  url: string;
}

export interface StreamInfo {
  id: number;
  kind: string;
  page_url: string;
  url: string;
  title: string;
  is_drm: boolean;
  qualities: Quality[];
}

export interface GrabbedLink {
  url: string;
  text: string;
  ext: string;
}

export interface Settings {
  max_concurrent: number;
  clipboard_enabled: boolean;
  mp4_output: boolean;
  default_dir: string;
}

export function startDownload(opts: {
  url: string;
  destination?: string;
  connections?: number;
  startAt?: number;
  repId?: string;
  headers?: Record<string, string>;
}): Promise<number> {
  return invoke<number>("start_download", {
    url: opts.url,
    destination: opts.destination ?? null,
    connections: opts.connections ?? null,
    startAt: opts.startAt ?? null,
    repId: opts.repId ?? null,
    headers: opts.headers ?? null,
  });
}

export function pauseDownload(id: number): Promise<void> {
  return invoke<void>("pause_download", { id });
}

export function resumeDownload(id: number): Promise<void> {
  return invoke<void>("resume_download", { id });
}

export function cancelDownload(id: number): Promise<void> {
  return invoke<void>("cancel_download", { id });
}

export function listDownloads(): Promise<DownloadTask[]> {
  return invoke<DownloadTask[]>("list_downloads");
}

export function clearFinished(): Promise<void> {
  return invoke<void>("clear_finished");
}

export function getSettings(): Promise<Settings> {
  return invoke<Settings>("get_settings");
}

export function setSettings(patch: Partial<Settings>): Promise<void> {
  return invoke<void>("set_settings", {
    maxConcurrent: patch.max_concurrent ?? null,
    clipboardEnabled: patch.clipboard_enabled ?? null,
    mp4Output: patch.mp4_output ?? null,
    defaultDir: patch.default_dir ?? null,
  });
}

export function grabSite(url: string): Promise<GrabbedLink[]> {
  return invoke<GrabbedLink[]>("grab_site", { url });
}

export function openBrowser(url: string): Promise<string> {
  return invoke<string>("open_browser", { url });
}

export function listStreams(): Promise<StreamInfo[]> {
  return invoke<StreamInfo[]>("list_streams");
}

export function clearStreams(): Promise<void> {
  return invoke<void>("clear_streams");
}

export function downloadStream(opts: {
  id: number;
  qualityIndex?: number;
  destination?: string;
  startAt?: number;
}): Promise<number> {
  return invoke<number>("download_stream", {
    id: opts.id,
    qualityIndex: opts.qualityIndex ?? null,
    destination: opts.destination ?? null,
    startAt: opts.startAt ?? null,
  });
}
