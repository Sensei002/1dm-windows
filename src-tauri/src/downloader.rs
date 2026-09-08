//! Tauri commands + the download queue manager.
//!
//! Responsibilities: task bookkeeping, concurrency limiting, scheduling,
//! pause/resume/cancel, persistence across restarts, progress forwarding,
//! and HLS/DASH assembly (concat + optional ffmpeg MP4 remux).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use engine::model::{DownloadTask, FinishedEvent, ProgressEvent};

const HLS_SEGMENT_CONCURRENCY: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub max_concurrent: usize,
    pub clipboard_enabled: bool,
    pub mp4_output: bool,
    pub default_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_concurrent: 3,
            clipboard_enabled: true,
            mp4_output: true,
            default_dir: String::new(),
        }
    }
}

struct TaskEntry {
    task: DownloadTask,
    headers: HashMap<String, String>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    tasks: HashMap<u64, TaskEntry>,
    max_concurrent: usize,
    clipboard_enabled: bool,
    mp4_output: bool,
    default_dir: String,
}

#[derive(Serialize, Deserialize)]
struct Persisted {
    next_id: u64,
    tasks: Vec<DownloadTask>,
    max_concurrent: usize,
    clipboard_enabled: bool,
    mp4_output: bool,
    default_dir: String,
}

pub struct DownloadManager {
    inner: Mutex<Inner>,
}

impl DownloadManager {
    pub fn new() -> Self {
        let settings = Settings::default();
        Self {
            inner: Mutex::new(Inner {
                next_id: 0,
                tasks: HashMap::new(),
                max_concurrent: settings.max_concurrent,
                clipboard_enabled: settings.clipboard_enabled,
                mp4_output: settings.mp4_output,
                default_dir: settings.default_dir,
            }),
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Detects the task kind from the URL shape.
pub fn detect_kind(url: &str) -> &'static str {
    let lower = url.to_lowercase();
    if lower.starts_with("magnet:") || lower.ends_with(".torrent") {
        "torrent"
    } else if lower.contains(".m3u8") {
        "hls"
    } else if lower.contains(".mpd") {
        "dash"
    } else {
        "http"
    }
}

fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "download".into()
    } else {
        trimmed
    }
}

/// Derives a filename from the URL tail.
fn file_name_for(url: &str, kind: &str) -> String {
    if kind == "torrent" && url.starts_with("magnet:") {
        return "torrent-download".into();
    }
    let raw = url.split(['?', '#']).next().unwrap_or(url);
    let name = raw.rsplit(['/', '\\']).next().unwrap_or("");
    sanitize_name(name)
}

fn with_ext(name: &str, ext: &str) -> String {
    if name.to_lowercase().ends_with(&format!(".{ext}")) {
        name.to_string()
    } else {
        format!("{name}.{ext}")
    }
}

pub fn settings_of(app: &AppHandle) -> Settings {
    let state = app.state::<DownloadManager>();
    settings_snapshot(&state)
}

pub fn settings_snapshot(state: &State<'_, DownloadManager>) -> Settings {
    let inner = lock(&state.inner);
    Settings {
        max_concurrent: inner.max_concurrent,
        clipboard_enabled: inner.clipboard_enabled,
        mp4_output: inner.mp4_output,
        default_dir: inner.default_dir.clone(),
    }
}

fn persist_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("queue.json"))
}

pub fn save_state(app: &AppHandle) {
    let Some(path) = persist_path(app) else { return };
    let state = app.state::<DownloadManager>();
    let data = {
        let inner = lock(&state.inner);
        let mut tasks: Vec<DownloadTask> = inner.tasks.values().map(|e| e.task.clone()).collect();
        tasks.sort_by_key(|t| t.id);
        Persisted {
            next_id: inner.next_id,
            tasks,
            max_concurrent: inner.max_concurrent,
            clipboard_enabled: inner.clipboard_enabled,
            mp4_output: inner.mp4_output,
            default_dir: inner.default_dir.clone(),
        }
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(&data) {
        let _ = std::fs::write(&path, json);
    }
}

pub fn load_state(app: &AppHandle) {
    let Some(path) = persist_path(app) else { return };
    let Ok(json) = std::fs::read_to_string(&path) else { return };
    let Ok(data) = serde_json::from_str::<Persisted>(&json) else { return };
    let state = app.state::<DownloadManager>();
    let mut inner = lock(&state.inner);
    inner.next_id = data.next_id;
    inner.max_concurrent = data.max_concurrent.max(1);
    inner.clipboard_enabled = data.clipboard_enabled;
    inner.mp4_output = data.mp4_output;
    if !data.default_dir.is_empty() {
        inner.default_dir = data.default_dir;
    }
    for mut task in data.tasks {
        // Anything mid-flight when the app closed goes back to the queue.
        if task.status == "downloading" || task.status == "starting" {
            task.status = "queued".into();
        }
        inner.tasks.insert(
            task.id,
            TaskEntry {
                cancel: Arc::new(AtomicBool::new(false)),
                headers: HashMap::new(),
                task,
            },
        );
    }
}

pub fn emit_queue(app: &AppHandle) {
    let state = app.state::<DownloadManager>();
    let mut tasks: Vec<DownloadTask> = {
        let inner = lock(&state.inner);
        inner.tasks.values().map(|e| e.task.clone()).collect()
    };
    tasks.sort_by_key(|t| t.id);
    let _ = app.emit("queue://updated", tasks);
}

/// Resolves the destination directory and filename for a new task.
fn resolve_destination(
    app: &AppHandle,
    destination: Option<String>,
    url: &str,
    kind: &str,
) -> Result<PathBuf, String> {
    let dir = match destination.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => {
            let configured = settings_of(app).default_dir;
            if configured.is_empty() {
                app.path()
                    .download_dir()
                    .map_err(|e| format!("no download dir: {e}"))?
            } else {
                PathBuf::from(configured)
            }
        }
    };
    if kind == "torrent" {
        return Ok(dir);
    }
    let name = file_name_for(url, kind);
    let name = match kind {
        "hls" => with_ext(&name, "ts"),
        "dash" => with_ext(&name, "mp4"),
        _ => name,
    };
    Ok(dir.join(name))
}

/// Creates a task and puts it in the queue. Shared by every entry point.
pub fn enqueue(
    app: &AppHandle,
    url: String,
    destination: Option<String>,
    connections: Option<usize>,
    start_at: Option<i64>,
    rep_id: Option<String>,
    headers: HashMap<String, String>,
) -> Result<u64, String> {
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("URL is empty".into());
    }
    let kind = detect_kind(&url);
    let dest = resolve_destination(app, destination, &url, kind)?;
    let state = app.state::<DownloadManager>();
    let id = {
        let mut inner = lock(&state.inner);
        inner.next_id += 1;
        let id = inner.next_id;
        let scheduled = start_at.map(|t| t > now_ms()).unwrap_or(false);
        inner.tasks.insert(
            id,
            TaskEntry {
                task: DownloadTask {
                    id,
                    url: url.clone(),
                    destination: dest.to_string_lossy().to_string(),
                    status: if scheduled { "scheduled".into() } else { "queued".into() },
                    done_bytes: 0,
                    total_bytes: 0,
                    kind: kind.into(),
                    connections: connections.unwrap_or(engine::download::DEFAULT_CONNECTIONS),
                    start_at,
                    rep_id,
                    error: None,
                },
                headers,
                cancel: Arc::new(AtomicBool::new(false)),
            },
        );
        id
    };
    emit_queue(app);
    save_state(app);
    Ok(id)
}

/// The queue ticker: starts queued (and due) tasks while slots are free.
pub fn start_queue_worker(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(400));
        loop {
            tick.tick().await;
            pump_queue(&app).await;
        }
    });
}

async fn pump_queue(app: &AppHandle) {
    let mut to_start: Vec<u64> = Vec::new();
    {
        let state = app.state::<DownloadManager>();
        let mut inner = lock(&state.inner);
        let active = inner
            .tasks
            .values()
            .filter(|e| e.task.status == "downloading")
            .count();
        let mut slots = inner.max_concurrent.saturating_sub(active);
        if slots > 0 {
            let mut ids: Vec<u64> = inner.tasks.keys().copied().collect();
            ids.sort_unstable();
            for id in ids {
                if slots == 0 {
                    break;
                }
                if let Some(entry) = inner.tasks.get_mut(&id) {
                    if entry.task.status != "queued" && entry.task.status != "scheduled" {
                        continue;
                    }
                    let due = entry
                        .task
                        .start_at
                        .map(|t| t <= now_ms())
                        .unwrap_or(true);
                    if due {
                        entry.task.status = "downloading".into();
                        to_start.push(id);
                        slots -= 1;
                    }
                }
            }
        }
    }
    if !to_start.is_empty() {
        for id in to_start {
            spawn_task(app.clone(), id);
        }
        emit_queue(app);
    }
}

fn finalize(app: &AppHandle, id: u64, status: &str, error: Option<String>) {
    {
        let state = app.state::<DownloadManager>();
        let mut inner = lock(&state.inner);
        if let Some(entry) = inner.tasks.get_mut(&id) {
            entry.task.status = status.into();
            entry.task.error = error.clone();
        }
    }
    let _ = app.emit(
        "download://finished",
        FinishedEvent {
            id,
            ok: status == "done",
            error,
        },
    );
    emit_queue(app);
    save_state(app);
}

struct Snapshot {
    id: u64,
    url: String,
    dest: PathBuf,
    kind: String,
    connections: usize,
    rep_id: Option<String>,
    headers: HashMap<String, String>,
    cancel: Arc<AtomicBool>,
}

fn spawn_task(app: AppHandle, id: u64) {
    let snap = {
        let state = app.state::<DownloadManager>();
        let inner = lock(&state.inner);
        let Some(entry) = inner.tasks.get(&id) else { return };
        Snapshot {
            id,
            url: entry.task.url.clone(),
            dest: PathBuf::from(&entry.task.destination),
            kind: entry.task.kind.clone(),
            connections: entry.task.connections,
            rep_id: entry.task.rep_id.clone(),
            headers: entry.headers.clone(),
            cancel: entry.cancel.clone(),
        }
    };
    tauri::async_runtime::spawn(async move {
        execute(app, snap).await;
    });
}

async fn execute(app: AppHandle, snap: Snapshot) {
    let id = snap.id;
    let result = run_task(&app, &snap).await;
    match result {
        Ok(()) => finalize(&app, id, "done", None),
        Err(_) if snap.cancel.load(Ordering::Relaxed) => {
            // pause/cancel command already set the final status
        }
        Err(e) => {
            let msg = drm_friendly(&e);
            finalize(&app, id, "failed", Some(msg));
        }
    }
}

fn drm_friendly(err: &str) -> String {
    let lower = err.to_lowercase();
    if lower.contains("unsupported encryption") || lower.contains("sample-aes") {
        "DRM-protected stream cannot be downloaded".into()
    } else {
        err.to_string()
    }
}

async fn run_task(app: &AppHandle, snap: &Snapshot) -> Result<(), String> {
    let client = engine::http::build_client(&snap.headers).map_err(|e| e.to_string())?;
    match snap.kind.as_str() {
        "http" => run_http_impl(&client, app, snap).await,
        "hls" => run_hls(app, &client, snap).await,
        "dash" => run_dash(app, &client, snap).await,
        "torrent" => engine::torrent::download_torrent(&snap.url, &snap.dest, snap.cancel.clone())
            .await
            .map_err(|e| e.to_string()),
        other => Err(format!("unknown kind: {other}")),
    }
}

/// Forwards engine progress events to the frontend.
async fn forward_progress_loop(
    app: AppHandle,
    id: u64,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<(u64, u64)>,
) {
    while let Some((done, total)) = rx.recv().await {
        let _ = app.emit("download://progress", ProgressEvent { id, done, total });
    }
}

async fn run_http_impl(client: &reqwest::Client, app: &AppHandle, snap: &Snapshot) -> Result<(), String> {
    let part = PathBuf::from(format!("{}.part", snap.dest.display()));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64)>();
    let fwd = tauri::async_runtime::spawn(forward_progress_loop(
        app.clone(),
        snap.id,
        rx,
    ));
    let result = engine::download::download_file(
        client,
        &snap.url,
        &part,
        snap.connections,
        tx,
        snap.cancel.clone(),
    )
    .await;
    let _ = fwd.abort();
    result.map_err(|e| e.to_string())?;
    if snap.dest.exists() {
        tokio::fs::remove_file(&snap.dest).await.map_err(|e| e.to_string())?;
    }
    tokio::fs::rename(&part, &snap.dest).await.map_err(|e| e.to_string())?;
    Ok(())
}

async fn run_hls(app: &AppHandle, client: &reqwest::Client, snap: &Snapshot) -> Result<(), String> {
    let playlist = engine::hls::fetch_playlist(client, &snap.url)
        .await
        .map_err(|e| e.to_string())?;
    if playlist.is_live {
        return Err("live streams are not supported".into());
    }
    let count = playlist.segments.len();
    let tmp = PathBuf::from(format!("{}.segtmp", snap.dest.display()));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64)>();
    let fwd = tauri::async_runtime::spawn(forward_progress_loop(app.clone(), snap.id, rx));
    let result = engine::hls::download_segments(
        client,
        &playlist,
        &tmp,
        HLS_SEGMENT_CONCURRENCY,
        tx,
        snap.cancel.clone(),
    )
    .await;
    let _ = fwd.abort();
    result.map_err(|e| e.to_string())?;

    let mp4_wanted = settings_of(app).mp4_output;
    let ts_tmp = PathBuf::from(format!("{}.concat.ts", snap.dest.display()));
    let ts_tmp_clone = ts_tmp.clone();
    let tmp_clone = tmp.clone();
    tokio::task::spawn_blocking(move || {
        engine::hls::concat_segments(&tmp_clone, count, &ts_tmp_clone)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;

    if mp4_wanted {
        let dest = PathBuf::from(with_ext(
            &snap.dest.to_string_lossy(),
            "mp4",
        ));
        let inputs = vec![ts_tmp.clone()];
        let dest_clone = dest.clone();
        tokio::task::spawn_blocking(move || remux(inputs, dest_clone))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let _ = tokio::fs::remove_file(&ts_tmp).await;
        // Keep the resolved path visible in the task.
        update_destination(app, snap.id, &dest);
    } else {
        let dest = PathBuf::from(with_ext(&snap.dest.to_string_lossy(), "ts"));
        let _ = tokio::fs::remove_file(&dest).await;
        tokio::fs::rename(&ts_tmp, &dest).await.map_err(|e| e.to_string())?;
        update_destination(app, snap.id, &dest);
    }
    let _ = tokio::fs::remove_dir_all(&tmp).await;
    Ok(())
}

async fn run_dash(app: &AppHandle, client: &reqwest::Client, snap: &Snapshot) -> Result<(), String> {
    let (manifest_url, rep_sel) = split_fragment(&snap.url);
    let manifest = engine::dash::fetch_variants(client, &manifest_url)
        .await
        .map_err(|e| e.to_string())?;
    if manifest.is_drm {
        return Err("DRM-protected stream cannot be downloaded".into());
    }
    let rep_id = rep_sel
        .or_else(|| snap.rep_id.clone())
        .or_else(|| manifest.video.first().map(|r| r.id.clone()))
        .ok_or("no video representation")?;

    let xml = fetch_text(client, &manifest_url).await?;
    let (init, segments) =
        engine::dash::segment_urls(&manifest_url, &xml, &rep_id, manifest.duration_secs)
            .map_err(|e| e.to_string())?;

    let tmp = PathBuf::from(format!("{}.segtmp", snap.dest.display()));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64)>();
    let fwd = tauri::async_runtime::spawn(forward_progress_loop(app.clone(), snap.id, rx));
    let result = engine::dash::download_segments(
        client,
        &init,
        segments,
        &tmp,
        HLS_SEGMENT_CONCURRENCY,
        tx,
        snap.cancel.clone(),
    )
    .await;
    let _ = fwd.abort();
    result.map_err(|e| e.to_string())?;

    let seg_count = segment_count_in(&tmp);
    let video_tmp = tmp.join("video.m4s");
    let tmp_v = tmp.clone();
    let video_tmp_c = video_tmp.clone();
    tokio::task::spawn_blocking(move || {
        engine::dash::concat_segments(&tmp_v, seg_count, &video_tmp_c)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;

    let dest = PathBuf::from(with_ext(&snap.dest.to_string_lossy(), "mp4"));
    if settings_of(app).mp4_output {
        let inputs = vec![video_tmp.clone()];
        let dest_clone = dest.clone();
        let remux_res = tokio::task::spawn_blocking(move || remux(inputs, dest_clone))
            .await
            .map_err(|e| e.to_string());
        match remux_res {
            Ok(Ok(())) => {}
            _ => {
                // No ffmpeg: fMP4 concat is still a playable MP4.
                let _ = tokio::fs::remove_file(&dest).await;
                tokio::fs::rename(&video_tmp, &dest).await.map_err(|e| e.to_string())?;
            }
        }
    } else {
        let _ = tokio::fs::remove_file(&dest).await;
        tokio::fs::rename(&video_tmp, &dest).await.map_err(|e| e.to_string())?;
    }
    update_destination(app, snap.id, &dest);
    let _ = tokio::fs::remove_dir_all(&tmp).await;
    Ok(())
}

fn update_destination(app: &AppHandle, id: u64, dest: &Path) {
    let state = app.state::<DownloadManager>();
    let mut inner = lock(&state.inner);
    if let Some(entry) = inner.tasks.get_mut(&id) {
        entry.task.destination = dest.to_string_lossy().to_string();
    }
}

fn segment_count_in(dir: &Path) -> usize {
    let mut n = 0;
    while dir.join(format!("segment_{:05}.m4s", n + 1)).exists() {
        n += 1;
    }
    n
}

fn split_fragment(url: &str) -> (String, Option<String>) {
    match url.split_once('#') {
        Some((base, frag)) => {
            let rep = frag.strip_prefix("rep=").map(|s| s.to_string());
            (base.to_string(), rep)
        }
        None => (url.to_string(), None),
    }
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())
}

/// MP4 remux (stream copy) via ffmpeg, downloading a portable ffmpeg the
/// first time if none is installed. Blocking.
fn remux(inputs: Vec<PathBuf>, out: PathBuf) -> Result<(), String> {
    let ffmpeg = ensure_ffmpeg()?;
    let mut cmd = std::process::Command::new(ffmpeg);
    for input in &inputs {
        cmd.arg("-i").arg(input);
    }
    cmd.args(["-y", "-c", "copy", "-movflags", "+faststart"])
        .arg(&out);
    let output = cmd.output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

fn ensure_ffmpeg() -> Result<std::path::PathBuf, String> {
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Ok(PathBuf::from("ffmpeg"));
    }
    ffmpeg_sidecar::download::auto_download()
        .map_err(|e| format!("ffmpeg unavailable: {e}"))?;
    let path = ffmpeg_sidecar::paths::ffmpeg_path();
    if path.exists() {
        Ok(path)
    } else {
        Err("ffmpeg binary not found after download".into())
    }
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// Starts a download. Returns the task id immediately; progress arrives via
/// `download://progress`, queue state via `queue://updated`.
#[tauri::command]
pub async fn start_download(
    app: AppHandle,
    url: String,
    destination: Option<String>,
    connections: Option<usize>,
    start_at: Option<i64>,
    rep_id: Option<String>,
    headers: Option<HashMap<String, String>>,
) -> Result<u64, String> {
    enqueue(
        &app,
        url,
        destination,
        connections,
        start_at,
        rep_id,
        headers.unwrap_or_default(),
    )
}

#[tauri::command]
pub fn pause_download(app: AppHandle, id: u64) -> Result<(), String> {
    let state = app.state::<DownloadManager>();
    {
        let mut inner = lock(&state.inner);
        let Some(entry) = inner.tasks.get_mut(&id) else {
            return Err("no such task".into());
        };
        if entry.task.status != "downloading" {
            return Ok(());
        }
        entry.cancel.store(true, Ordering::Relaxed);
        entry.task.status = "paused".into();
        entry.task.error = None;
    }
    emit_queue(&app);
    save_state(&app);
    Ok(())
}

#[tauri::command]
pub fn resume_download(app: AppHandle, id: u64) -> Result<(), String> {
    let state = app.state::<DownloadManager>();
    let mut inner = lock(&state.inner);
    let Some(entry) = inner.tasks.get_mut(&id) else {
        return Err("no such task".into());
    };
    if entry.task.status != "paused" {
        return Ok(());
    }
    entry.cancel = Arc::new(AtomicBool::new(false));
    entry.task.status = "queued".into();
    entry.task.error = None;
    drop(inner);
    emit_queue(&app);
    save_state(&app);
    Ok(())
}

#[tauri::command]
pub fn cancel_download(app: AppHandle, id: u64) -> Result<(), String> {
    let state = app.state::<DownloadManager>();
    {
        let mut inner = lock(&state.inner);
        let Some(entry) = inner.tasks.get_mut(&id) else {
            return Err("no such task".into());
        };
        entry.cancel.store(true, Ordering::Relaxed);
        entry.task.status = "cancelled".into();
        entry.task.error = None;
    }
    emit_queue(&app);
    save_state(&app);
    Ok(())
}

#[tauri::command]
pub fn list_downloads(app: AppHandle) -> Result<Vec<DownloadTask>, String> {
    let state = app.state::<DownloadManager>();
    let inner = lock(&state.inner);
    let mut list: Vec<DownloadTask> = inner.tasks.values().map(|e| e.task.clone()).collect();
    list.sort_by_key(|t| t.id);
    Ok(list)
}

#[tauri::command]
pub fn clear_finished(app: AppHandle) -> Result<(), String> {
    let state = app.state::<DownloadManager>();
    {
        let mut inner = lock(&state.inner);
        inner
            .tasks
            .retain(|_, e| !matches!(e.task.status.as_str(), "done" | "failed" | "cancelled"));
    }
    emit_queue(&app);
    save_state(&app);
    Ok(())
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Settings {
    settings_of(&app)
}

#[tauri::command]
pub fn set_settings(
    app: AppHandle,
    max_concurrent: Option<usize>,
    clipboard_enabled: Option<bool>,
    mp4_output: Option<bool>,
    default_dir: Option<String>,
) -> Result<(), String> {
    let state = app.state::<DownloadManager>();
    {
        let mut inner = lock(&state.inner);
        if let Some(n) = max_concurrent {
            inner.max_concurrent = n.clamp(1, 16);
        }
        if let Some(v) = clipboard_enabled {
            inner.clipboard_enabled = v;
        }
        if let Some(v) = mp4_output {
            inner.mp4_output = v;
        }
        if let Some(d) = default_dir {
            inner.default_dir = d;
        }
    }
    save_state(&app);
    Ok(())
}

/// Website grabber: collects media/document links from a page.
#[tauri::command]
pub async fn grab_site(url: String) -> Result<Vec<engine::grabber::GrabbedLink>, String> {
    let client = engine::http::build_client(&Default::default()).map_err(|e| e.to_string())?;
    engine::grabber::grab_site(&client, url.trim(), "", 1)
        .await
        .map_err(|e| e.to_string())
}
