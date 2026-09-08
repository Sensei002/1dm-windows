//! 1DM for Windows download engine.
//!
//! Modules:
//! - [`http`] — shared HTTP client factory (UA + captured session headers)
//! - [`download`] — multi-connection file downloader with progress events
//! - [`hls`] — HLS (m3u8) playlists, quality variants, AES-128 segments
//! - [`dash`] — MPEG-DASH (mpd) manifest parsing + segment downloading
//! - [`torrent`] — BitTorrent via librqbit
//! - [`grabber`] — website link extraction for batch grabbing
//! - [`model`] — shared types used by the app layer

pub mod dash;
pub mod download;
pub mod grabber;
pub mod hls;
pub mod http;
pub mod model;
pub mod torrent;

pub use model::{DownloadTask, FinishedEvent, ProgressEvent, Quality, StreamInfo};
