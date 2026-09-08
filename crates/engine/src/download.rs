//! Multi-connection HTTP file downloader (the 1DM-style "16 parts" engine).

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use reqwest::header::{CONTENT_LENGTH, RANGE};
use reqwest::Client;
use thiserror::Error;
use tokio::io::AsyncSeekExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("cancelled")]
    Cancelled,
}

pub const DEFAULT_CONNECTIONS: usize = 16;
const MAX_CONNECTIONS: usize = 32;

/// Downloads `url` to `dest` using up to `connections` parallel range
/// requests. Progress `(done, total)` is sent through `progress`. The
/// download aborts with [`DownloadError::Cancelled`] once `cancel` is set.
pub async fn download_file(
    client: &Client,
    url: &str,
    dest: &Path,
    connections: usize,
    progress: UnboundedSender<(u64, u64)>,
    cancel: Arc<AtomicBool>,
) -> Result<(), DownloadError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(DownloadError::Cancelled);
    }
    let total = probe_size(client, url).await?;
    if total == 0 {
        return stream_single(client, url, dest, progress, cancel).await;
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(DownloadError::Cancelled);
    }

    // Preallocate so each worker writes at its own offset without coordination.
    let file = tokio::fs::File::create(dest).await?;
    file.set_len(total).await?;
    drop(file);

    let done = Arc::new(AtomicU64::new(0));
    let conns = connections.clamp(1, MAX_CONNECTIONS);
    let chunk = total / conns as u64;
    let mut set = tokio::task::JoinSet::new();

    for i in 0..conns {
        let start = i as u64 * chunk;
        let end = if i + 1 == conns {
            total.saturating_sub(1)
        } else {
            start + chunk - 1
        };
        if start > end {
            continue;
        }
        let client = client.clone();
        let url = url.to_string();
        let dest = dest.to_path_buf();
        let done = done.clone();
        let cancel = cancel.clone();
        set.spawn(async move {
            download_range(&client, &url, &dest, start, end, done, &cancel).await
        });
    }

    // Report progress on a timer while workers run.
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(300));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let _ = progress.send((done.load(Ordering::Relaxed), total));
            }
            _ = poll_cancel(&cancel), if cancel.load(Ordering::Relaxed) => {
                set.abort_all();
                return Err(DownloadError::Cancelled);
            }
            res = set.join_next(), if !set.is_empty() => {
                match res {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(e))) => {
                        set.abort_all();
                        return Err(e);
                    }
                    Some(Err(e)) => {
                        set.abort_all();
                        return Err(DownloadError::Io(std::io::Error::other(e.to_string())));
                    }
                    None => break,
                }
            }
            else => break,
        }
    }
    let _ = progress.send((total, total));
    Ok(())
}

/// Fetches one byte range and writes it at the correct offset.
async fn download_range(
    client: &Client,
    url: &str,
    dest: &Path,
    start: u64,
    end: u64,
    done: Arc<AtomicU64>,
    cancel: &AtomicBool,
) -> Result<(), DownloadError> {
    let mut resp = client
        .get(url)
        .header(RANGE, format!("bytes={start}-{end}"))
        .send()
        .await?;
    resp.error_for_status_ref()?;

    // A fresh handle per worker; Rust std opens with full sharing on Windows,
    // so concurrent handles to the same file are fine.
    let mut file = tokio::fs::OpenOptions::new().write(true).open(dest).await?;
    // Seek once; sequential write_all keeps the cursor advancing.
    file.seek(std::io::SeekFrom::Start(start)).await?;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        let Some(chunk) = resp.chunk().await? else { break };
        let n = chunk.len() as u64;
        if n == 0 {
            continue;
        }
        file.write_all(&chunk).await?;
        let _ = done.fetch_add(n, Ordering::Relaxed);
    }
    Ok(())
}

/// Yields only when `cancel` flips — used in `select!` arms.
async fn poll_cancel(cancel: &AtomicBool) {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Best-effort size probe: HEAD first, fall back to a `bytes=0-0` range GET.
/// Returns 0 when the server doesn't expose a length (no multi-connection).
async fn probe_size(client: &Client, url: &str) -> Result<u64, DownloadError> {
    if let Ok(resp) = client.head(url).send().await {
        if let Some(len) = resp
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
        {
            return Ok(len);
        }
    }
    let resp = client.get(url).header(RANGE, "bytes=0-0").send().await?;
    if let Some(cr) = resp
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
    {
        if let Some(total) = cr.rsplit('/').next().and_then(|s| s.parse::<u64>().ok()) {
            return Ok(total);
        }
    }
    Ok(0)
}

/// Fallback: plain single-connection stream when no length is available.
async fn stream_single(
    client: &Client,
    url: &str,
    dest: &Path,
    progress: UnboundedSender<(u64, u64)>,
    cancel: Arc<AtomicBool>,
) -> Result<(), DownloadError> {
    let mut resp = client.get(url).send().await?;
    resp.error_for_status_ref()?;
    let total = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(dest).await?;
    let mut done: u64 = 0;
    while let Some(chunk) = resp.chunk().await? {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        let _ = progress.send((done, total));
    }
    let _ = progress.send((done, total));
    Ok(())
}