// git-clawler GUI — Windows-only Tauri application.
// On Linux/macOS, use `git-clawler-cli` instead.

// Suppress the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
struct WindowStateSaveState(std::sync::atomic::AtomicU64);

#[cfg(target_os = "windows")]
fn monitor_rects(window: &tauri::WebviewWindow) -> Vec<git_clawler_gui::window_state::Rect> {
    window
        .available_monitors()
        .map(|monitors| {
            monitors
                .into_iter()
                .map(|monitor| {
                    let position = monitor.position();
                    let size = monitor.size();
                    git_clawler_gui::window_state::Rect {
                        x: position.x,
                        y: position.y,
                        width: size.width,
                        height: size.height,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "windows")]
fn restore_window_state(
    window: &tauri::WebviewWindow,
    cfg: &git_clawler_core::config::GuiConfig,
    first_launch: bool,
) {
    let saved_rect = match (
        cfg.window_x,
        cfg.window_y,
        cfg.window_width,
        cfg.window_height,
    ) {
        (Some(x), Some(y), Some(width), Some(height)) => {
            Some(git_clawler_gui::window_state::Rect {
                x,
                y,
                width,
                height,
            })
        }
        _ => None,
    };

    if let Some(rect) = saved_rect {
        if let Err(e) = window.set_size(tauri::Size::Physical(tauri::PhysicalSize {
            width: rect.width,
            height: rect.height,
        })) {
            tracing::warn!(error=%e, "failed to restore window size");
        }

        let monitors = monitor_rects(window);
        if git_clawler_gui::window_state::should_restore_position(rect, &monitors) {
            if let Err(e) =
                window.set_position(tauri::Position::Physical(tauri::PhysicalPosition {
                    x: rect.x,
                    y: rect.y,
                }))
            {
                tracing::warn!(error=%e, "failed to restore window position");
            }
        } else {
            tracing::warn!(
                ?rect,
                "saved window position is offscreen; skipping restore"
            );
        }
    } else if let (Some(width), Some(height)) = (cfg.window_width, cfg.window_height) {
        if let Err(e) =
            window.set_size(tauri::Size::Physical(tauri::PhysicalSize { width, height }))
        {
            tracing::warn!(error=%e, "failed to restore window size");
        }
    }

    if first_launch || cfg.window_maximized == Some(true) {
        if let Err(e) = window.maximize() {
            tracing::warn!(error=%e, "failed to maximize main window");
        }
    }
}

#[cfg(target_os = "windows")]
fn save_window_state_debounced(app: tauri::AppHandle) {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tauri::Manager;

    let sequence = {
        let state = app.state::<WindowStateSaveState>();
        state.0.fetch_add(1, Ordering::SeqCst) + 1
    };

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let state = app.state::<WindowStateSaveState>();
        if state.0.load(Ordering::SeqCst) != sequence {
            return;
        }

        git_clawler_gui::save_window_state_now(&app).await;
    });
}

#[cfg(target_os = "windows")]
fn invalidate_window_state_debounce(app: &tauri::AppHandle) {
    use std::sync::atomic::Ordering;
    use tauri::Manager;

    let state = app.state::<WindowStateSaveState>();
    state.0.fetch_add(1, Ordering::SeqCst);
}

#[cfg(target_os = "windows")]
fn main() {
    // SAFETY: configure global libgit2 settings before Tauri starts threads.
    unsafe { git_clawler_core::status::configure_network_timeouts() }
        .expect("configure Git socket deadlines");
    git_clawler_core::fetch_worker::run_if_requested();
    use git_clawler_gui::wsl::{TokioWslCommandRunner, WslHelperRegistry};
    use git_clawler_gui::{
        commands, realtime, request_app_exit, shortcut, tray, window as gui_window, GuiPolicyState,
        MinimizeState, RealtimeCompletion, RealtimeShutdown,
    };
    use std::sync::Arc;
    use tauri::{Manager, WindowEvent};
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .manage(commands::SaveConfigLock(tokio::sync::Mutex::new(())))
        .manage(commands::StartupErrorState(std::sync::Mutex::new(
            Vec::new(),
        )))
        .manage(shortcut::CurrentShortcutState(std::sync::Mutex::new(
            shortcut::ShortcutRegistrationState::default(),
        )))
        .manage(MinimizeState(std::sync::atomic::AtomicBool::new(false)))
        .manage(WindowStateSaveState(std::sync::atomic::AtomicU64::new(0)))
        .invoke_handler(tauri::generate_handler![
            commands::list_repositories,
            commands::scan_root,
            commands::get_config,
            commands::save_config,
            commands::save_column_widths,
            commands::request_refresh,
            commands::get_startup_errors,
            commands::open_repository_folder,
            commands::open_github_page,
        ])
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    let policy = window.app_handle().state::<GuiPolicyState>().snapshot();
                    if policy.close_to_tray {
                        api.prevent_close();
                        let snapshot = window.get_webview_window("main").and_then(|webview| {
                            git_clawler_gui::current_window_state_snapshot(&webview)
                        });
                        let app_handle = window.app_handle().clone();
                        invalidate_window_state_debounce(&app_handle);
                        if let Some(snapshot) = snapshot {
                            tauri::async_runtime::spawn(async move {
                                git_clawler_gui::save_window_state_snapshot(&app_handle, snapshot)
                                    .await;
                            });
                        }
                        if let Err(e) = gui_window::hide_window(
                            &window.app_handle().clone(),
                            policy.hide_from_taskbar,
                        ) {
                            tracing::warn!(error=%e, "hide_window on CloseRequested failed");
                        }
                    } else {
                        api.prevent_close();
                        request_app_exit(&window.app_handle().clone());
                    }
                }
                WindowEvent::Resized(size) => {
                    let app_handle = window.app_handle();
                    let minimize_state = app_handle.state::<MinimizeState>();
                    let is_min = window.is_minimized().unwrap_or(false);
                    let prev_is_min = minimize_state
                        .0
                        .swap(is_min, std::sync::atomic::Ordering::SeqCst);
                    if prev_is_min != is_min {
                        let policy = app_handle.state::<GuiPolicyState>().snapshot();
                        if gui_window::should_hide_on_minimize(
                            prev_is_min,
                            is_min,
                            policy.close_to_tray,
                        ) {
                            if let Err(e) = gui_window::hide_window(
                                &app_handle.clone(),
                                policy.hide_from_taskbar,
                            ) {
                                tracing::warn!(error=%e, "hide_window on minimize failed");
                            }
                        }
                    }
                    if !is_min {
                        let _ = size;
                        save_window_state_debounced(app_handle.clone());
                    }
                }
                WindowEvent::Moved(_) => {
                    save_window_state_debounced(window.app_handle().clone());
                }
                _ => {}
            }
        })
        .setup(|app| {
            // Register the lazy WSL helper bootstrap registry plus the
            // bundled-helper resource path. Both are consumed by
            // `commands::list_repositories` and the realtime scan loop.
            let registry: Arc<WslHelperRegistry> =
                Arc::new(WslHelperRegistry::new(Arc::new(TokioWslCommandRunner)));
            app.manage(commands::HelperRegistryState(registry));

            let resource_dir = app
                .path()
                .resource_dir()
                .map_err(|e| format!("failed to resolve resource_dir: {e}"))?;
            let helper_resource_path =
                git_clawler_gui::wsl::resolve_resource_helper_path(&resource_dir);
            app.manage(commands::HelperResourcePath(helper_resource_path));

            // Tray + initial window visibility / taskbar policy + global
            // shortcut registration are all driven from the loaded config.
            let config_file_exists = git_clawler_core::config::default_config_path()
                .ok()
                .and_then(|path| path.try_exists().ok())
                .unwrap_or(false);
            let initial_cfg = git_clawler_core::config::load_or_default().unwrap_or_default();
            app.manage(GuiPolicyState::new(
                initial_cfg.gui.close_to_tray,
                initial_cfg.gui.hide_from_taskbar,
            ));

            if let Some(window) = app.get_webview_window("main") {
                restore_window_state(&window, &initial_cfg.gui, !config_file_exists);
            }

            if let Err(e) = tray::build_tray(app.handle(), tray::TrayStatus::Green) {
                tracing::warn!(error=%e, "build_tray failed");
            }

            // Apply hide_from_taskbar / start_in_tray policy. Only skip the
            // taskbar when both start_in_tray and hide_from_taskbar are
            // enabled; otherwise show the window normally.
            let handle = app.handle().clone();
            if initial_cfg.gui.start_in_tray {
                if initial_cfg.gui.hide_from_taskbar {
                    if let Err(e) = gui_window::apply_taskbar_visibility(&handle, true) {
                        tracing::warn!(error=%e, "apply_taskbar_visibility failed");
                    }
                }
            } else if let Err(e) = gui_window::show_window(&handle) {
                tracing::warn!(error=%e, "initial show_window failed");
            }

            // Register the configured global shortcut (if any).
            let accel = initial_cfg.gui.global_shortcut.trim();
            if !accel.is_empty() {
                if let Err(e) = shortcut::reregister_if_changed(&handle, Some(accel)) {
                    use tauri::Emitter as _;
                    tracing::warn!(error=%e, "initial global shortcut registration failed");
                    let payload = realtime::RealtimeErrorPayload {
                        kind: "shortcut".into(),
                        message: e,
                        occurred_at: chrono::Utc::now().to_rfc3339(),
                    };
                    let startup_state = handle.state::<commands::StartupErrorState>();
                    match startup_state.0.lock() {
                        Ok(mut errors) => errors.push(payload.clone()),
                        Err(poisoned) => {
                            tracing::warn!("startup error state mutex poisoned; recovering");
                            poisoned.into_inner().push(payload.clone());
                        }
                    }
                    if let Err(emit_err) = handle.emit(realtime::EVENT_REPOS_ERROR, payload) {
                        tracing::warn!(error=%emit_err, "emit repositories://error failed");
                    }
                }
            }

            let (tx_shutdown, rx_shutdown) = tokio::sync::oneshot::channel();
            app.manage(RealtimeShutdown(std::sync::Mutex::new(Some(tx_shutdown))));
            let (tx_completion, rx_completion) = tokio::sync::oneshot::channel();
            app.manage(RealtimeCompletion(tokio::sync::Mutex::new(Some(
                rx_completion,
            ))));
            let (manual_refresh_tx, manual_refresh_rx) = tokio::sync::mpsc::channel::<()>(8);
            app.manage(realtime::ManualRefreshState(manual_refresh_tx));
            let realtime_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = realtime::bootstrap_and_run(
                    realtime_handle,
                    rx_shutdown,
                    manual_refresh_rx,
                    Some(tx_completion),
                )
                .await
                {
                    tracing::error!(error=%e, "realtime loop terminated");
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("git-clawler-gui runs on Windows only. Use `git-clawler-cli` on Linux/macOS.");
    std::process::exit(1);
}
