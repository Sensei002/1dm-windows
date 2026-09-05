mod downloader;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(downloader::DownloadManager::default())
        .invoke_handler(tauri::generate_handler![
            downloader::start_download,
            downloader::cancel_download,
            downloader::list_downloads
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}