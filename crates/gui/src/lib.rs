// lib.rs for gui crate — exposes internal modules for integration tests.
// Production entry point is src/main.rs (binary).
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

pub mod commands;
pub mod dto;
pub mod notification;
pub mod realtime;
pub mod shortcut;
pub mod tray;
#[cfg(target_os = "windows")]
pub mod window;
pub mod window_logic;
pub mod window_state;
pub mod wsl;

#[cfg(target_os = "windows")]
pub struct RealtimeShutdown(pub std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>);

#[cfg(target_os = "windows")]
pub struct RealtimeCompletion(pub tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>);

#[cfg(target_os = "windows")]
pub struct MinimizeState(pub std::sync::atomic::AtomicBool);

pub struct GuiPolicyState {
    bits: std::sync::atomic::AtomicU8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuiPolicy {
    pub close_to_tray: bool,
    pub hide_from_taskbar: bool,
}

impl GuiPolicyState {
    const CLOSE_TO_TRAY: u8 = 0b0000_0001;
    const HIDE_FROM_TASKBAR: u8 = 0b0000_0010;

    pub fn new(close_to_tray: bool, hide_from_taskbar: bool) -> Self {
        Self {
            bits: std::sync::atomic::AtomicU8::new(Self::encode(close_to_tray, hide_from_taskbar)),
        }
    }

    pub fn close_to_tray(&self) -> bool {
        self.snapshot().close_to_tray
    }

    pub fn hide_from_taskbar(&self) -> bool {
        self.snapshot().hide_from_taskbar
    }

    pub fn snapshot(&self) -> GuiPolicy {
        Self::decode(self.bits.load(std::sync::atomic::Ordering::Relaxed))
    }

    pub fn update(&self, close_to_tray: bool, hide_from_taskbar: bool) {
        self.bits.store(
            Self::encode(close_to_tray, hide_from_taskbar),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    fn encode(close_to_tray: bool, hide_from_taskbar: bool) -> u8 {
        (if close_to_tray {
            Self::CLOSE_TO_TRAY
        } else {
            0
        }) | (if hide_from_taskbar {
            Self::HIDE_FROM_TASKBAR
        } else {
            0
        })
    }

    fn decode(bits: u8) -> GuiPolicy {
        GuiPolicy {
            close_to_tray: bits & Self::CLOSE_TO_TRAY != 0,
            hide_from_taskbar: bits & Self::HIDE_FROM_TASKBAR != 0,
        }
    }
}

#[cfg(target_os = "windows")]
pub fn current_window_state_snapshot(
    window: &tauri::WebviewWindow,
) -> Option<window_logic::WindowStateSnapshot> {
    if window.is_minimized().unwrap_or(false) {
        return None;
    }

    if window.is_maximized().unwrap_or(false) {
        return Some(window_logic::WindowStateSnapshot::Maximized);
    }

    let position = match window.outer_position() {
        Ok(position) => position,
        Err(e) => {
            tracing::warn!(error=%e, "failed to read window position");
            return None;
        }
    };
    let size = match window.outer_size() {
        Ok(size) => size,
        Err(e) => {
            tracing::warn!(error=%e, "failed to read window size");
            return None;
        }
    };

    Some(window_logic::WindowStateSnapshot::Normal {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    })
}

#[cfg(target_os = "windows")]
pub async fn save_window_state_snapshot(
    app: &tauri::AppHandle,
    snapshot: window_logic::WindowStateSnapshot,
) {
    use tauri::Manager;

    let lock = app.state::<commands::SaveConfigLock>();
    let _guard = lock.0.lock().await;

    match git_clawler_core::config::load_or_default() {
        Ok(mut cfg) => {
            if !window_logic::merge_window_state(&mut cfg.gui, snapshot) {
                return;
            }
            if let Err(e) = git_clawler_core::config::save(&cfg) {
                tracing::warn!(error=%e, "failed to persist window state");
            }
        }
        Err(e) => tracing::warn!(error=%e, "failed to load config for window state save"),
    }
}

#[cfg(target_os = "windows")]
pub async fn save_window_state_now(app: &tauri::AppHandle) {
    use tauri::Manager;

    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Some(snapshot) = current_window_state_snapshot(&window) else {
        return;
    };

    save_window_state_snapshot(app, snapshot).await;
}

/// Cleanly request the app to exit: signal the realtime loop's shutdown
/// oneshot (if registered) and then call `app.exit(0)`. Used by the tray
/// "Exit" menu item and the window CloseRequested handler.
#[cfg(target_os = "windows")]
pub fn request_app_exit(app: &tauri::AppHandle) {
    use tauri::Manager;
    let state = app.state::<RealtimeShutdown>();
    if let Ok(mut guard) = state.0.lock() {
        if let Some(tx) = guard.take() {
            let _ = tx.send(());
        }
    }

    // Take the completion receiver out so the spawned task owns it.
    let completion_rx = {
        let completion_state = app.state::<RealtimeCompletion>();
        // try_lock to avoid blocking the event loop; if contended we still
        // proceed (worst case: no graceful wait).
        completion_state
            .0
            .try_lock()
            .ok()
            .and_then(|mut g| g.take())
    };

    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            save_window_state_now(&app_handle),
        )
        .await;
        if let Some(rx) = completion_rx {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), rx).await;
        }
        app_handle.exit(0);
    });
}

#[cfg(test)]
mod tests {
    use super::GuiPolicyState;

    #[test]
    fn gui_policy_state_reads_initial_values() {
        let state = GuiPolicyState::new(true, false);

        assert_eq!(
            state.snapshot(),
            super::GuiPolicy {
                close_to_tray: true,
                hide_from_taskbar: false,
            }
        );
    }

    #[test]
    fn gui_policy_state_update_replaces_values() {
        let state = GuiPolicyState::new(false, true);

        state.update(true, false);

        assert_eq!(
            state.snapshot(),
            super::GuiPolicy {
                close_to_tray: true,
                hide_from_taskbar: false,
            }
        );
    }
}
