//! Clipboard watcher: polls the clipboard for media URLs and offers them to
//! the user via a `clipboard://url` event (auto-enqueue would be surprising).

use arboard::Clipboard;
use tauri::{AppHandle, Emitter, Manager};

use crate::downloader;

/// URL shapes worth offering (media extensions, manifests, magnets).
pub fn is_interesting(url: &str) -> bool {
    let lower = url.to_lowercase();
    if !(lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("magnet:"))
    {
        return false;
    }
    if lower.starts_with("magnet:") {
        return true;
    }
    matches!(
        lower
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .rsplit('.')
            .next()
            .unwrap_or(""),
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "ts" | "mp3" | "flac" | "m4a" | "wav"
            | "ogg" | "opus" | "zip" | "rar" | "7z" | "pdf" | "apk" | "iso"
    ) || lower.contains(".m3u8")
        || lower.contains(".mpd")
        || lower.ends_with(".torrent")
}

/// Extracts the first URL-looking token from clipboard text.
fn extract_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|token| token.trim_matches(|c: char| "\"'()[]<>{},".contains(c)))
        .find(|token| is_interesting(token))
        .map(|token| token.to_string())
}

/// Starts the background clipboard poller (every 1.2s).
pub fn start_clipboard_watcher(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut last: Option<String> = None;
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(1200));
        loop {
            tick.tick().await;
            let enabled = {
                let state = app.state::<downloader::DownloadManager>();
                downloader::settings_snapshot(&state).clipboard_enabled
            };
            if !enabled {
                continue;
            }
            let Ok(text) = Clipboard::new().and_then(|mut c| c.get_text()) else {
                continue;
            };
            if text.len() > 4096 {
                continue;
            }
            let Some(url) = extract_url(&text) else { continue };
            if last.as_deref() == Some(url.as_str()) {
                continue;
            }
            last = Some(url.clone());
            let _ = app.emit("clipboard://url", url);
        }
    });
}
