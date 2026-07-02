mod commands;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::daemon_health,
            commands::get_version,
        ])
        .setup(|_app| {
            tracing::info!("Qingluan desktop app started");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
