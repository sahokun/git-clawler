//! Window show/hide/toggle helpers (Tauri, Windows-only).
//!
//! These are kept out of `main.rs` so other modules (tray, shortcut) can
//! call them without re-implementing the `get_webview_window("main")`
//! boilerplate.
//!
//! NOTE: The `set_skip_taskbar(true)` ⇒ `set_skip_taskbar(false)`
//! ordering when transitioning between hidden-from-taskbar and shown
//! states cannot be exercised on Linux CI; the `set_skip_taskbar` call
//! is a Windows-only Win32 SHELL_WINDOW operation. Manual verification
//! on Windows is required for this change.

pub use crate::window_logic::should_hide_on_minimize;
use tauri::{AppHandle, Manager};

fn main_window(app: &AppHandle) -> tauri::Result<tauri::WebviewWindow> {
    app.get_webview_window("main")
        .ok_or(tauri::Error::WebviewNotFound)
}

pub fn show_window(app: &AppHandle) -> tauri::Result<()> {
    let window = main_window(app)?;
    // Always restore taskbar entry when showing the window so a previously
    // hidden-from-taskbar state doesn't persist.
    window.set_skip_taskbar(false)?;
    window.unminimize()?;
    window.show()?;
    window.set_focus()?;
    Ok(())
}

pub fn hide_window(app: &AppHandle, hide_from_taskbar: bool) -> tauri::Result<()> {
    let window = main_window(app)?;
    if hide_from_taskbar {
        window.set_skip_taskbar(true)?;
    }
    window.hide()
}

pub fn toggle_window(app: &AppHandle) -> tauri::Result<()> {
    let window = main_window(app)?;
    if window.is_visible()? {
        window.hide()?;
    } else {
        // Always restore taskbar entry on show.
        window.set_skip_taskbar(false)?;
        window.unminimize()?;
        window.show()?;
        window.set_focus()?;
    }
    Ok(())
}

/// Marshal a window toggle onto the Tauri main thread. Safe to call from
/// a global-shortcut callback or tray-icon click handler running on a
/// worker thread.
pub fn toggle_window_from_any_thread(app: &AppHandle) {
    let app_for_main = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Err(e) = toggle_window(&app_for_main) {
            tracing::warn!(error=%e, "toggle_window failed");
        }
    });
}

pub fn apply_taskbar_visibility(app: &AppHandle, hide: bool) -> tauri::Result<()> {
    main_window(app)?.set_skip_taskbar(hide)
}
