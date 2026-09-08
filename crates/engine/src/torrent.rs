//! Torrent downloads via librqbit (magnet links and .torrent sources).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use librqbit::{AddTorrent, AddTorrentOptions, Session, SessionOptions};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TorrentError {
    #[error("torrent error: {0}")]
    Session(String),
    #[error("cancelled")]
    Cancelled,
}

/// Adds a magnet/torrent to a fresh session rooted at `dest_dir` and waits
/// for completion. Cancellation pauses the torrent and stops waiting.
pub async fn download_torrent(
    source: &str,
    dest_dir: &Path,
    cancel: Arc<AtomicBool>,
) -> Result<(), TorrentError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(TorrentError::Cancelled);
    }

    let session = Session::new_with_opts(dest_dir.to_path_buf(), SessionOptions::default())
        .await
        .map_err(|e| TorrentError::Session(e.to_string()))?;

    let handle = session
        .add_torrent(AddTorrent::from_url(source), Some(AddTorrentOptions::default()))
        .await
        .map_err(|e| TorrentError::Session(e.to_string()))?
        .into_handle()
        .ok_or_else(|| TorrentError::Session("torrent could not be added".into()))?;

    tokio::select! {
        _ = poll_cancel(&cancel) => {
            let _ = session.pause(&handle).await;
            Err(TorrentError::Cancelled)
        }
        result = handle.wait_until_completed() => {
            result.map_err(|e| TorrentError::Session(e.to_string()))?;
            if cancel.load(Ordering::Relaxed) {
                Err(TorrentError::Cancelled)
            } else {
                Ok(())
            }
        }
    }
}

async fn poll_cancel(cancel: &AtomicBool) {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}
