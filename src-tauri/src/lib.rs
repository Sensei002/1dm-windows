mod clipboard;
mod downloader;
mod sniffer;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(downloader::DownloadManager::new())
        .manage(sniffer::Sniffer::new())
        .invoke_handler(tauri::generate_handler![
            downloader::start_download,
            downloader::pause_download,
            downloader::resume_download,
            downloader::cancel_download,
            downloader::list_downloads,
            downloader::clear_finished,
            downloader::get_settings,
            downloader::set_settings,
            downloader::grab_site,
            sniffer::open_browser,
            sniffer::list_streams,
            sniffer::clear_streams,
            sniffer::download_stream,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            downloader::load_state(&handle);
            downloader::start_queue_worker(handle.clone());
            clipboard::start_clipboard_watcher(handle);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                downloader::save_state(app);
            }
        });
}
