//! Shared types between the engine and the app layer.

use serde::{Deserialize, Serialize};

/// A download tracked by the app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTask {
    pub id: u64,
    pub url: String,
    pub destination: String,
    /// One of: scheduled, queued, downloading, paused, done, failed, cancelled.
    pub status: String,
    /// Progress units: bytes for HTTP, segments for HLS/DASH.
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// One of: http, hls, dash, torrent.
    pub kind: String,
    /// Parallel connections for HTTP downloads.
    pub connections: usize,
    /// Unix ms when a scheduled download should start.
    #[serde(default)]
    pub start_at: Option<i64>,
    /// DASH representation id (`#rep=` fragment of a quality selection).
    #[serde(default)]
    pub rep_id: Option<String>,
    /// Last error message when status is failed.
    #[serde(default)]
    pub error: Option<String>,
}

/// Emitted periodically while a download runs.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    pub id: u64,
    pub done: u64,
    pub total: u64,
}

/// Emitted when a download finishes (success or failure).
#[derive(Debug, Clone, Serialize)]
pub struct FinishedEvent {
    pub id: u64,
    pub ok: bool,
    pub error: Option<String>,
}

/// One selectable quality variant of a detected stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quality {
    pub label: String,
    /// URL to download for this quality. For DASH this is the manifest URL
    /// with a `#rep=<id>` fragment identifying the representation.
    pub url: String,
}

/// Metadata about a detected stream (from browser traffic sniffing).
#[derive(Debug, Clone, Serialize)]
pub struct StreamInfo {
    pub id: u64,
    /// One of: hls, dash.
    pub kind: String,
    /// Page that produced the stream (Referer), used as session origin.
    pub page_url: String,
    pub url: String,
    pub title: String,
    /// True when the stream is Widevine/PlayReady/FairPlay protected.
    pub is_drm: bool,
    pub qualities: Vec<Quality>,
}
