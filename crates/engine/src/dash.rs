//! MPEG-DASH (mpd) support — planned for Phase 2.
//!
//! Sony Liv serves a DASH manifest (`videoURL.mpd`) alongside the HLS one.
//! The plan: parse with `dash-mpd-rs`, download init + media segments in
//! parallel, and remux to MP4 (via ffmpeg, downloaded on first use).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DashError {
    #[error("not implemented yet")]
    NotImplemented,
}

/// Placeholder — DASH download is planned for a later phase.
pub fn _planned() -> Result<(), DashError> {
    Err(DashError::NotImplemented)
}