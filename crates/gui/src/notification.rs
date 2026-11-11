#[cfg(target_os = "windows")]
pub fn notify(handle: &tauri::AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    if let Err(e) = handle
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show()
    {
        tracing::warn!(error = %e, "show notification failed");
    }
}
