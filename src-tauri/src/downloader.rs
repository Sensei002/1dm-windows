//! Tauri commands + in-memory download manager state.

use std::collections::HashMap;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, State};

use engine::model::{DownloadTask, FinishedEvent, ProgressEvent};

#[derive(Default)]
pub struct DownloadManager {
    tasks: Mutex<HashMap<u64, DownloadTask>>,
    next_id: Mutex<u64>,
}

/// Starts a multi-connection download. Returns the task id immediately;
/// progress arrives via `download://progress`, completion via `download://finished`.
#[tauri::command]
pub async fn start_download(
    app: AppHandle,
    state: State<'_, DownloadManager>,
    url: String,
    destination: String,
) -> Result<u64, String> {
    let id = {
        let mut next = state.next_id.lock().map_err(|e| e.to_string())?;
        *next += 1;
        *next
    };
    {
        let mut tasks = state.tasks.lock().map_err(|e| e.to_string())?;
        tasks.insert(
            id,
            DownloadTask {
                id,
                url: url.clone(),
                destination: destination.clone(),
                status: "downloading".into(),
                done_bytes: 0,
                total_bytes: 0,
            },
        );
    }

    let client = engine::http::build_client(&Default::default()).map_err(|e| e.to_string())?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64)>();
    let app_events = app.clone();

    // Forward engine progress to the frontend.
    tokio::spawn(async move {
        while let Some((done, total)) = rx.recv().await {
            let _ = app_events.emit("download://progress", ProgressEvent { id, done, total });
        }
    });

    // Run the download in the background.
    tokio::spawn(async move {
        let result = engine::download::download_file(
            &client,
            &url,
            std::path::Path::new(&destination),
            engine::download::DEFAULT_CONNECTIONS,
            tx,
        )
        .await;
        let ok = result.is_ok();
        let error = result.err().map(|e| e.to_string());
        let _ = app.emit("download://finished", FinishedEvent { id, ok, error });
    });

    Ok(id)
}

#[tauri::command]
pub fn cancel_download(state: State<'_, DownloadManager>, id: u64) -> Result<(), String> {
    let mut tasks = state.tasks.lock().map_err(|e| e.to_string())?;
    if let Some(task) = tasks.get_mut(&id) {
        task.status = "cancelled".into();
    }
    Ok(())
}

#[tauri::command]
pub fn list_downloads(state: State<'_, DownloadManager>) -> Result<Vec<DownloadTask>, String> {
    let tasks = state.tasks.lock().map_err(|e| e.to_string())?;
    Ok(tasks.values().cloned().collect())
}