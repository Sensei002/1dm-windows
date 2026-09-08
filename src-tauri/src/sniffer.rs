//! Embedded browser window + stream sniffing.
//!
//! The browser window is a plain Tauri webview pointed at the target site.
//! Every network request it makes passes through
//! `on_web_resource_request`, where we (a) block ad/tracker hosts and
//! (b) capture m3u8/mpd playlist requests — together with their session
//! headers — into the [`Sniffer`] state for the quality picker.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use engine::model::{Quality, StreamInfo};
use tauri::{Emitter, Manager};
use tauri::Url;

/// Detected streams, newest first in [`Sniffer::list`].
#[derive(Default)]
pub struct Sniffer {
    next_id: AtomicU64,
    streams: Mutex<Vec<StreamInfo>>,
}

impl Sniffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one captured playlist request. Returns the new StreamInfo id
    /// when the URL was not seen before.
    fn observe(&self, page_url: &str, url: &str) -> Option<u64> {
        let mut streams = self.streams.lock().unwrap_or_else(|p| p.into_inner());
        if streams.iter().any(|s| s.url == url) {
            return None;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        streams.push(StreamInfo {
            id,
            kind: "hls".into(),
            page_url: page_url.to_string(),
            url: url.to_string(),
            title: title_for(page_url, url),
            is_drm: false,
            qualities: vec![Quality {
                label: "best".into(),
                url: url.to_string(),
            }],
        });
        Some(id)
    }

    /// Fills in the quality list / DRM flag after manifest inspection.
    pub fn update(&self, id: u64, kind: &str, is_drm: bool, qualities: Vec<Quality>) {
        let mut streams = self.streams.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(s) = streams.iter_mut().find(|s| s.id == id) {
            s.kind = kind.into();
            s.is_drm = is_drm;
            if !qualities.is_empty() {
                s.qualities = qualities;
            }
        }
    }

    pub fn list(&self) -> Vec<StreamInfo> {
        let streams = self.streams.lock().unwrap_or_else(|p| p.into_inner());
        streams.iter().rev().cloned().collect()
    }

    pub fn clear(&self) {
        let mut streams = self.streams.lock().unwrap_or_else(|p| p.into_inner());
        streams.clear();
    }
}

fn title_for(page_url: &str, stream_url: &str) -> String {
    let host = tauri::Url::parse(page_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| "stream".into());
    let tail = stream_url
        .split(['?', '#'])
        .next()
        .unwrap_or(stream_url)
        .rsplit('/')
        .next()
        .unwrap_or("stream")
        .to_string();
    format!("{host} — {tail}")
}

/// Host fragments blocked in the embedded browser (ads / trackers).
const ADBLOCK_HOSTS: &[&str] = &[
    "doubleclick.net",
    "googlesyndication.com",
    "googleadservices.com",
    "googletagmanager.com",
    "google-analytics.com",
    "adservice.google.",
    "adnxs.com",
    "adsystem.",
    "amazon-adsystem.com",
    "taboola.com",
    "outbrain.com",
    "criteo.",
    "scorecardresearch.com",
    "quantserve.com",
    "moatads.com",
    "adsrvr.org",
    "pubmatic.com",
    "rubiconproject.com",
    "openx.net",
    "casalemedia.com",
    "smartadserver.com",
    "zeotap.com",
    "branch.io",
    "appsflyer.com",
    "adjust.com",
    "kochava.com",
    "hotjar.com",
    "fullstory.com",
    "chartbeat.",
    "newrelic.com",
    "nr-data.net",
    "bugsnag.com",
    "crashlytics.com",
    "sent.io",
    "bidsxchange",
    "adform.net",
    "yieldmo.com",
    "sharethrough.com",
    "media.net",
    "revcontent.com",
    "mgid.com",
    "propellerads",
    "popads.",
    "adcash.com",
    "exoclick.com",
    "juicyads",
    "trafficjunky",
    "adcolony",
    "vungle.com",
    "unityads",
    "applovin.com",
    "inmobi.com",
    "mopub.com",
    "fyber.com",
    "adtechus",
    "advertising.com",
    "ads.yahoo.com",
    "bing.com/bat.js",
    "clarity.ms",
    "yandex-metrica",
    "mc.yandex.ru",
    "top-fwz1.mail.ru",
    "adfox.ru",
    "an.yandex.ru",
];

/// Headers worth replaying when the engine fetches the stream later.
fn capture_headers(request: &tauri::http::Request<&[u8]>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let interesting = [
        "authorization",
        "cookie",
        "origin",
        "referer",
        "x-requested-with",
        "x-csrf-token",
        "x-playback-session-id",
        "security-token",
    ];
    for (name, value) in request.headers() {
        let key = name.as_str().to_ascii_lowercase();
        if interesting.contains(&key.as_str()) || key.starts_with("x-") {
            if let Ok(v) = value.to_str() {
                out.insert(key, v.to_string());
            }
        }
    }
    out
}

fn is_manifest_url(uri: &str) -> Option<&'static str> {
    let lower = uri.to_lowercase();
    if lower.contains(".m3u8") {
        Some("hls")
    } else if lower.contains(".mpd") {
        Some("dash")
    } else {
        None
    }
}

fn is_ad(uri: &str) -> bool {
    ADBLOCK_HOSTS.iter().any(|h| uri.to_lowercase().contains(h))
}

/// The `on_web_resource_request` callback for browser windows.
fn intercept(
    app: &tauri::AppHandle,
    request: &tauri::http::Request<Vec<u8>>,
    response: &mut tauri::http::Response<std::borrow::Cow<'static, [u8]>>,
) {
    let uri = request.uri().to_string();
    if !uri.starts_with("http://") && !uri.starts_with("https://") {
        return;
    }

    // 1) adblock: short-circuit the request with an empty 403-style body
    if is_ad(&uri) {
        *response.status_mut() = tauri::http::StatusCode::FORBIDDEN;
        *response.body_mut() = std::borrow::Cow::Borrowed(&b"blocked by 1DM"[..]);
        return;
    }

    // 2) stream sniffing
    let Some(kind) = is_manifest_url(&uri) else { return };
    let referer = request
        .headers()
        .get("referer")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let page_url = if referer.is_empty() { uri.as_str() } else { referer.as_str() };

    let sniffer = app.state::<Sniffer>();
    let Some(id) = sniffer.observe(page_url, &uri) else { return };

    // Analyze the manifest off-thread, then publish quality picks.
    let headers = capture_headers(request);
    let url = uri.clone();
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut qualities = Vec::new();
        let mut is_drm = false;
        let client = engine::http::build_client(&headers).ok();
        if let Some(client) = client {
            match kind {
                "hls" => {
                    if let Ok(variants) = engine::hls::fetch_variants(&client, &url).await {
                        is_drm = variants.is_drm;
                        qualities = variants
                            .variants
                            .into_iter()
                            .map(|v| Quality { label: v.label, url: v.url })
                            .collect();
                    }
                }
                "dash" => {
                    if let Ok(manifest) = engine::dash::fetch_variants(&client, &url).await {
                        is_drm = manifest.is_drm;
                        qualities = manifest
                            .video
                            .iter()
                            .map(|r| Quality {
                                label: r.label.clone(),
                                url: format!("{}#rep={}", url, r.id),
                            })
                            .collect();
                    }
                }
                _ => {}
            }
        }
        app2.state::<Sniffer>().update(id, kind, is_drm, qualities);
        if let Some(info) = app2.state::<Sniffer>().list().into_iter().find(|s| s.id == id) {
            let _ = app2.emit("stream://detected", info);
        }
    });
}

/// Opens (or focuses) the embedded browser window at `url`.
pub fn open_browser_impl(app: &tauri::AppHandle, url: &str) -> Result<String, String> {
    let parsed = tauri::Url::parse(url.trim()).map_err(|e| format!("invalid url: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        _ => return Err("only http(s) URLs can be opened".into()),
    }

    if let Some(existing) = app.get_webview_window("browser") {
        let js = format!(
            "window.location.href = {};",
            serde_json::to_string(url.trim()).unwrap_or_else(|_| "\"about:blank\"".into())
        );
        existing
            .eval(&js)
            .map_err(|e| format!("navigate failed: {e}"))?;
        let _ = existing.set_focus();
        return Ok("browser".into());
    }

    tauri::WebviewWindowBuilder::new(
        app,
        "browser",
        tauri::WebviewUrl::External(parsed),
    )
    .title("1DM Browser")
    .inner_size(1150.0, 800.0)
    .on_web_resource_request(|request, response| {
        intercept(&app, request, response);
    })
    .build()
    .map_err(|e| e.to_string())?;
    Ok("browser".into())
}

/// Helper for tests / callers that poll flags.
pub fn cancelled(flag: &AtomicBool) -> bool {
    flag.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// Opens (or focuses) the embedded browser window at `url`.
#[tauri::command]
pub fn open_browser(app: tauri::AppHandle, url: String) -> Result<String, String> {
    open_browser_impl(&app, &url)
}

/// Lists detected streams (newest first).
#[tauri::command]
pub fn list_streams(state: tauri::State<'_, Sniffer>) -> Result<Vec<StreamInfo>, String> {
    Ok(state.list())
}

/// Clears detected streams.
#[tauri::command]
pub fn clear_streams(state: tauri::State<'_, Sniffer>) -> Result<(), String> {
    state.clear();
    Ok(())
}

/// Queues a download for a detected stream, optionally at a specific quality.
#[tauri::command]
pub fn download_stream(
    app: tauri::AppHandle,
    streams: tauri::State<'_, Sniffer>,
    id: u64,
    quality_index: Option<usize>,
    destination: Option<String>,
    start_at: Option<i64>,
) -> Result<u64, String> {
    let info = streams
        .list()
        .into_iter()
        .find(|s| s.id == id)
        .ok_or("stream not found")?;
    if info.is_drm {
        return Err("DRM-protected streams cannot be downloaded — they can only be played".into());
    }
    let url = match quality_index {
        Some(i) => info
            .qualities
            .get(i)
            .map(|q| q.url.clone())
            .unwrap_or(info.url),
        None => info.url.clone(),
    };
    crate::downloader::enqueue(
        &app,
        url,
        destination,
        None,
        start_at,
        None,
        Default::default(),
    )
}
