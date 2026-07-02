use serde_json::Value;

/// GET /health from the local daemon.
#[tauri::command]
pub async fn daemon_health() -> Result<Value, String> {
    let daemon_url = "http://127.0.0.1:47129/health";
    let resp = reqwest::get(daemon_url)
        .await
        .map_err(|e| format!("Failed to connect to daemon: {e}"))?;

    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {e}"))?;

    Ok(body)
}

/// Return the desktop app version.
#[tauri::command]
pub fn get_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
