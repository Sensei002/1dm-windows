import { invoke } from "@tauri-apps/api/core";

export interface DownloadTask {
  id: number;
  url: string;
  destination: string;
  status: string;
  done_bytes: number;
  total_bytes: number;
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

export function startDownload(url: string, destination: string): Promise<number> {
  return invoke<number>("start_download", { url, destination });
}

export function cancelDownload(id: number): Promise<void> {
  return invoke<void>("cancel_download", { id });
}

export function listDownloads(): Promise<DownloadTask[]> {
  return invoke<DownloadTask[]>("list_downloads");
}