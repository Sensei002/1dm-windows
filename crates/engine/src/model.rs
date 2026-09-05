//! Shared types between the engine and the app layer.

use serde::Serialize;

/// A download tracked by the app.
#[derive(Debug, Clone, Serialize)]
pub struct DownloadTask {
    pub id: u64,
    pub url: String,
    pub destination: String,
    /// One of: queued, downloading, done, failed, cancelled.
    pub status: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
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

/// Metadata about a detected stream (from the page's player API or manifest).
#[derive(Debug, Clone, Serialize)]
pub struct StreamInfo {
    pub title: String,
    pub url: String,
    /// True when the platform reports the stream as Widevine/DRM encrypted.
    pub is_drm: bool,
    pub qualities: Vec<String>,
}