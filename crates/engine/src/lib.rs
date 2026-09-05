//! 1DM for Windows download engine.
//!
//! Modules:
//! - [`http`] — shared HTTP client factory (UA + captured session headers)
//! - [`download`] — multi-connection file downloader with progress events
//! - [`hls`] — HLS (m3u8) playlist parsing + segment downloading with AES-128
//! - [`dash`] — MPEG-DASH (mpd) support (planned)
//! - [`model`] — shared types used by the app layer

pub mod dash;
pub mod download;
pub mod hls;
pub mod http;
pub mod model;

pub use model::{DownloadTask, FinishedEvent, ProgressEvent, StreamInfo};