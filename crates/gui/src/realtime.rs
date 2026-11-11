//! Realtime emitter loop bridging `Watcher` events to Tauri events.
//!
//! This module is platform-agnostic for unit testing; the Tauri-backed
//! `Emitter` implementation and the `bootstrap_and_run` orchestrator are
//! gated to Windows.

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use crate::dto::{CommandError, ListRepositoriesResponse};
use futures::future::BoxFuture;
use git_clawler_core::{AppConfig, WatchEvent, WatcherConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::Instant;

pub const EVENT_REPOS_UPDATED: &str = "repositories://updated";
pub const EVENT_REPOS_ERROR: &str = "repositories://error";
pub const EVENT_SHORTCUT_STATUS: &str = "shortcut://status";

const ERROR_THROTTLE_TTL: Duration = Duration::from_secs(30);

fn config_affects_scan(previous: &AppConfig, next: &AppConfig) -> bool {
    previous.general.fetch_on_refresh != next.general.fetch_on_refresh
        || previous.repositories != next.repositories
        || previous.github != next.github
}

fn watcher_config_changed(
    previous: &AppConfig,
    next: &AppConfig,
    config_path: Option<PathBuf>,
) -> bool {
    let previous_watcher = watcher_config_from_app_config(previous, config_path.clone());
    let next_watcher = watcher_config_from_app_config(next, config_path);
    previous_watcher.repos != next_watcher.repos
        || previous_watcher.config_path != next_watcher.config_path
        || previous_watcher.poll_interval != next_watcher.poll_interval
        || previous_watcher.debounce != next_watcher.debounce
}

/// Payload emitted on `repositories://error`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealtimeErrorPayload {
    pub kind: String,
    pub message: String,
    pub occurred_at: String,
}

/// Payload emitted on `shortcut://status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortcutStatusPayload {
    pub registered: bool,
    pub occurred_at: String,
}

/// Abstraction over Tauri's `emit` so the loop is unit-testable.
pub trait Emitter: Send + Sync + 'static {
    fn emit_repos(&self, payload: &ListRepositoriesResponse);
    fn emit_error(&self, payload: &RealtimeErrorPayload);
    fn emit_shortcut_status(&self, payload: &ShortcutStatusPayload);
}

/// Hook invoked once after a successful `ConfigChanged` reload.
pub type ConfigReloadedHook =
    std::sync::Arc<dyn Fn(&AppConfig) -> Result<(), String> + Send + Sync + 'static>;

/// Deps injected into [`run_realtime_loop`].
pub struct RealtimeDeps<S, L, R>
where
    S: Fn(AppConfig) -> BoxFuture<'static, Result<ListRepositoriesResponse, CommandError>>
        + Send
        + Sync
        + 'static,
    L: Fn() -> Result<AppConfig, anyhow::Error> + Send + Sync + 'static,
    R: FnMut(WatcherConfig) -> anyhow::Result<()> + Send + 'static,
{
    pub scan: S,
    pub load_config: L,
    pub reload_watcher: R,
    /// Optional hook invoked once after a successful `ConfigChanged` reload.
    /// Used by the GUI bootstrap to re-register the global shortcut on the
    /// fly when the user changes `gui.global_shortcut`.
    pub on_config_reloaded: Option<ConfigReloadedHook>,
}

/// Channels and lifecycle signals wired into [`run_realtime_loop`].
///
/// Bundled so the loop entry point stays under clippy's argument-count limit
/// without an `#[allow]`; this is purely structural, not a behaviour change.
pub struct RealtimeChannels {
    /// Filesystem/poll watch events. The loop ends when this closes.
    pub event_rx: tokio::sync::mpsc::Receiver<WatchEvent>,
    /// Manual refresh requests from the UI.
    pub manual_refresh_rx: tokio::sync::mpsc::Receiver<()>,
    /// Resolves to request loop shutdown.
    pub shutdown: tokio::sync::oneshot::Receiver<()>,
    /// Optional signal sent once the loop has exited.
    pub completion_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

struct ErrorThrottle {
    last: HashMap<(String, String), Instant>,
    ttl: Duration,
}

impl ErrorThrottle {
    fn new(ttl: Duration) -> Self {
        Self {
            last: HashMap::new(),
            ttl,
        }
    }

    fn should_emit(&mut self, kind: &str, message: &str) -> bool {
        let key = (kind.to_string(), message.to_string());
        let now = Instant::now();
        // GC: evict expired entries to prevent unbounded HashMap growth.
        self.last
            .retain(|_, instant| now.duration_since(*instant) < self.ttl);
        match self.last.get(&key) {
            Some(_) => false,
            None => {
                self.last.insert(key, now);
                true
            }
        }
    }
}

pub fn watcher_repos_from_config(config: &AppConfig) -> Vec<PathBuf> {
    git_clawler_core::Registry::from_config(config)
        .list()
        .iter()
        .filter(|r| {
            let is_wsl = r.distro.is_some() || crate::wsl::is_wsl_path(r.path());
            if is_wsl {
                tracing::warn!(
                    path = %r.path().display(),
                    "skipping WSL repository from filesystem watcher; polling tick only"
                );
            }
            !is_wsl
        })
        .map(|r| r.path().to_path_buf())
        .collect()
}

pub fn watcher_config_from_app_config(
    config: &AppConfig,
    config_path: Option<PathBuf>,
) -> WatcherConfig {
    WatcherConfig {
        repos: watcher_repos_from_config(config),
        config_path,
        poll_interval: Duration::from_secs(config.general.refresh_interval_secs),
        debounce: Duration::from_millis(500),
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn make_error(kind: &str, message: impl Into<String>) -> RealtimeErrorPayload {
    RealtimeErrorPayload {
        kind: kind.to_string(),
        message: message.into(),
        occurred_at: now_rfc3339(),
    }
}

/// Realtime loop. Runs until `event_rx` closes or `shutdown` resolves.
pub async fn run_realtime_loop<E, S, L, R>(
    initial_cfg: AppConfig,
    config_path: Option<PathBuf>,
    channels: RealtimeChannels,
    emitter: E,
    mut deps: RealtimeDeps<S, L, R>,
) where
    E: Emitter,
    S: Fn(AppConfig) -> BoxFuture<'static, Result<ListRepositoriesResponse, CommandError>>
        + Send
        + Sync
        + 'static,
    L: Fn() -> Result<AppConfig, anyhow::Error> + Send + Sync + 'static,
    R: FnMut(WatcherConfig) -> anyhow::Result<()> + Send + 'static,
{
    let RealtimeChannels {
        mut event_rx,
        mut manual_refresh_rx,
        mut shutdown,
        completion_tx,
    } = channels;
    let mut current_cfg = initial_cfg;
    let mut current_path = config_path;
    let mut throttle = ErrorThrottle::new(ERROR_THROTTLE_TTL);

    let mut scanning: Option<BoxFuture<'static, Result<ListRepositoriesResponse, CommandError>>> =
        Some((deps.scan)(current_cfg.clone()));
    let mut pending_scan = false;
    let mut pending_manual_scan = false;
    let mut current_scan_is_manual = false;
    let mut manual_refresh_open = true;

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            maybe_refresh = manual_refresh_rx.recv(), if manual_refresh_open => {
                if maybe_refresh.is_some() {
                    // Drain any additional pending signals to coalesce burst refreshes.
                    while manual_refresh_rx.try_recv().is_ok() {}
                    pending_scan = true;
                    pending_manual_scan = true;
                    if scanning.is_none() && pending_scan {
                        pending_scan = false;
                        current_scan_is_manual = pending_manual_scan;
                        pending_manual_scan = false;
                        scanning = Some((deps.scan)(current_cfg.clone()));
                    }
                } else {
                    manual_refresh_open = false;
                }
            }
            maybe_evt = event_rx.recv() => {
                let Some(evt) = maybe_evt else { break };
                handle_event(
                    evt,
                    &mut current_cfg,
                    &mut current_path,
                    &emitter,
                    &mut deps,
                    &mut throttle,
                    &mut pending_scan,
                ).await;
                while let Ok(evt) = event_rx.try_recv() {
                    handle_event(
                        evt,
                        &mut current_cfg,
                        &mut current_path,
                        &emitter,
                        &mut deps,
                        &mut throttle,
                        &mut pending_scan,
                    ).await;
                }
                if scanning.is_none() && pending_scan {
                    pending_scan = false;
                    current_scan_is_manual = pending_manual_scan;
                    pending_manual_scan = false;
                    scanning = Some((deps.scan)(current_cfg.clone()));
                }
            }
            result = async { scanning.as_mut().unwrap().await }, if scanning.is_some() => {
                scanning = None;
                match result {
                    Ok(payload) => emitter.emit_repos(&payload),
                    Err(e) => {
                        if current_scan_is_manual || throttle.should_emit("scan", &e.message) {
                            emitter.emit_error(&make_error("scan", e.message));
                        }
                    }
                }
                current_scan_is_manual = false;
                while let Ok(evt) = event_rx.try_recv() {
                    handle_event(
                        evt,
                        &mut current_cfg,
                        &mut current_path,
                        &emitter,
                        &mut deps,
                        &mut throttle,
                        &mut pending_scan,
                    ).await;
                }
                if pending_scan {
                    pending_scan = false;
                    current_scan_is_manual = pending_manual_scan;
                    pending_manual_scan = false;
                    scanning = Some((deps.scan)(current_cfg.clone()));
                }
            }
        }
    }

    if let Some(tx) = completion_tx {
        let _ = tx.send(());
    }
}

async fn handle_event<E, S, L, R>(
    evt: WatchEvent,
    current_cfg: &mut AppConfig,
    current_path: &mut Option<PathBuf>,
    emitter: &E,
    deps: &mut RealtimeDeps<S, L, R>,
    throttle: &mut ErrorThrottle,
    pending_scan: &mut bool,
) where
    E: Emitter,
    S: Fn(AppConfig) -> BoxFuture<'static, Result<ListRepositoriesResponse, CommandError>>
        + Send
        + Sync
        + 'static,
    L: Fn() -> Result<AppConfig, anyhow::Error> + Send + Sync + 'static,
    R: FnMut(WatcherConfig) -> anyhow::Result<()> + Send + 'static,
{
    match evt {
        WatchEvent::Tick | WatchEvent::RepoChanged { .. } => {
            *pending_scan = true;
        }
        WatchEvent::ConfigChanged => match (deps.load_config)() {
            Ok(new_cfg) => {
                let reload_needed =
                    watcher_config_changed(current_cfg, &new_cfg, current_path.clone());
                let scan_needed = config_affects_scan(current_cfg, &new_cfg);

                if reload_needed {
                    let wc = watcher_config_from_app_config(&new_cfg, current_path.clone());
                    if let Err(e) = (deps.reload_watcher)(wc) {
                        if throttle.should_emit("reload", &e.to_string()) {
                            emitter.emit_error(&make_error("reload", e.to_string()));
                        }
                        return;
                    }
                }

                *current_cfg = new_cfg;
                // Re-apply runtime hooks on every successful config reload,
                // even if the config value is unchanged. This lets users retry
                // transient shortcut registration failures by saving again.
                if let Some(cb) = &deps.on_config_reloaded {
                    match cb(current_cfg) {
                        Ok(()) => emitter.emit_shortcut_status(&ShortcutStatusPayload {
                            registered: !current_cfg.gui.global_shortcut.trim().is_empty(),
                            occurred_at: now_rfc3339(),
                        }),
                        Err(e) => {
                            if throttle.should_emit("shortcut", &e) {
                                emitter.emit_error(&make_error("shortcut", e));
                            }
                        }
                    }
                }
                if scan_needed {
                    *pending_scan = true;
                }
            }
            Err(e) => {
                if throttle.should_emit("config", &e.to_string()) {
                    emitter.emit_error(&make_error("config", e.to_string()));
                }
            }
        },
        WatchEvent::WatcherError(msg) => {
            if throttle.should_emit("watcher", &msg) {
                emitter.emit_error(&make_error("watcher", msg));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri integration (Windows only)
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
pub struct TauriEmitter {
    pub handle: tauri::AppHandle,
}

#[cfg(target_os = "windows")]
impl Emitter for TauriEmitter {
    fn emit_repos(&self, p: &ListRepositoriesResponse) {
        use tauri::Emitter as _;
        if let Err(e) = self.handle.emit(EVENT_REPOS_UPDATED, p) {
            tracing::warn!(error = %e, "emit repositories://updated failed");
        }
    }
    fn emit_error(&self, p: &RealtimeErrorPayload) {
        use tauri::Emitter as _;
        if let Err(e) = self.handle.emit(EVENT_REPOS_ERROR, p) {
            tracing::warn!(error = %e, "emit repositories://error failed");
        }
    }
    fn emit_shortcut_status(&self, p: &ShortcutStatusPayload) {
        use tauri::Emitter as _;
        if let Err(e) = self.handle.emit(EVENT_SHORTCUT_STATUS, p) {
            tracing::warn!(error = %e, "emit shortcut://status failed");
        }
    }
}

/// Managed state holding the sender half of the manual refresh channel.
/// Tray "Refresh" uses this to signal the realtime loop to scan immediately.
#[cfg(target_os = "windows")]
pub struct ManualRefreshState(pub tokio::sync::mpsc::Sender<()>);

/// Wraps `TauriEmitter` to also push tray icon / tooltip updates on every
/// `repositories://updated` event.
#[cfg(target_os = "windows")]
pub struct TrayAwareEmitter {
    pub inner: TauriEmitter,
    pub handle: tauri::AppHandle,
    pub notif_state: std::sync::Arc<std::sync::Mutex<NotifState>>,
}

pub struct NotifState {
    pub prev_snapshot: Option<git_clawler_core::notifications::Snapshot>,
    pub cooldown: git_clawler_core::notifications::CooldownTracker,
    pub cooldown_secs: u64,
    pub enabled: bool,
}

pub fn reload_notification_state(state: &mut NotifState, config: &AppConfig) {
    let was_enabled = state.enabled;
    let cooldown_changed = state.cooldown_secs != config.notifications.cooldown_secs;
    let enabled_changed = was_enabled != config.notifications.enabled;
    state.enabled = config.notifications.enabled;
    if enabled_changed || cooldown_changed {
        state.cooldown = git_clawler_core::notifications::CooldownTracker::new(
            config.notifications.cooldown_secs,
        );
        state.cooldown_secs = config.notifications.cooldown_secs;
    }
    if enabled_changed {
        state.prev_snapshot = None;
    }
}

#[cfg(target_os = "windows")]
impl Emitter for TrayAwareEmitter {
    fn emit_repos(&self, p: &ListRepositoriesResponse) {
        self.inner.emit_repos(p);
        let status = crate::tray::derive_tray_status(&p.rows);
        let tooltip = crate::tray::format_tooltip(&p.rows);
        crate::tray::refresh_status(&self.handle, status, tooltip);
        notify_changes_if_needed(&self.handle, &self.notif_state, p);
    }
    fn emit_error(&self, p: &RealtimeErrorPayload) {
        self.inner.emit_error(p);
    }
    fn emit_shortcut_status(&self, p: &ShortcutStatusPayload) {
        self.inner.emit_shortcut_status(p);
    }
}

#[cfg(target_os = "windows")]
fn notify_changes_if_needed(
    handle: &tauri::AppHandle,
    state: &std::sync::Arc<std::sync::Mutex<NotifState>>,
    payload: &ListRepositoriesResponse,
) {
    let next = snapshot_from_payload(payload);
    let mut guard = match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!("notification state mutex poisoned; recovering");
            poisoned.into_inner()
        }
    };
    let Some(prev) = guard.prev_snapshot.as_ref() else {
        guard.prev_snapshot = Some(next);
        return;
    };
    let changes = git_clawler_core::notifications::diff(prev, &next);
    guard.prev_snapshot = Some(next);
    if !guard.enabled || changes.is_empty() || !guard.cooldown.allow() {
        return;
    }
    let summary = changes.summary();
    guard.cooldown.record();
    drop(guard);
    crate::notification::notify(handle, "git-clawler", &summary);
}

#[cfg(target_os = "windows")]
fn snapshot_from_payload(
    payload: &ListRepositoriesResponse,
) -> git_clawler_core::notifications::Snapshot {
    let mut map = std::collections::BTreeMap::new();
    for row in &payload.rows {
        if let crate::dto::RowStatus::Ok(status) = &row.status {
            map.insert(
                row.path.clone(),
                git_clawler_core::notifications::RepoFingerprint {
                    dirty: status.uncommitted_files > 0,
                    uncommitted_files: status.uncommitted_files,
                    unpushed_branches: status.unpushed_branches.len() as u32,
                    ahead: status.ahead,
                    behind: status.behind,
                    conflict_predicted: status.conflict_predicted,
                },
            );
        }
    }
    git_clawler_core::notifications::Snapshot(map)
}

#[cfg(target_os = "windows")]
pub async fn bootstrap_and_run(
    handle: tauri::AppHandle,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
    manual_refresh_rx: tokio::sync::mpsc::Receiver<()>,
    completion_tx: Option<tokio::sync::oneshot::Sender<()>>,
) -> anyhow::Result<()> {
    use git_clawler_core::config;
    use tauri::Manager as _;

    let config_path = config::default_config_path().ok();
    let initial_cfg = match config::load_or_default() {
        Ok(cfg) => cfg,
        Err(e) => {
            TauriEmitter {
                handle: handle.clone(),
            }
            .emit_error(&make_error("config", e.to_string()));
            AppConfig::default()
        }
    };

    let watcher_cfg = watcher_config_from_app_config(&initial_cfg, config_path.clone());
    let notif_state = std::sync::Arc::new(std::sync::Mutex::new(NotifState {
        prev_snapshot: None,
        cooldown: git_clawler_core::notifications::CooldownTracker::new(
            initial_cfg.notifications.cooldown_secs,
        ),
        cooldown_secs: initial_cfg.notifications.cooldown_secs,
        enabled: initial_cfg.notifications.enabled,
    }));
    let emitter = TrayAwareEmitter {
        inner: TauriEmitter {
            handle: handle.clone(),
        },
        handle: handle.clone(),
        notif_state: notif_state.clone(),
    };

    match git_clawler_core::Watcher::start(watcher_cfg) {
        Ok((mut watcher, event_rx)) => {
            let scan_handle = handle.clone();
            let deps = RealtimeDeps {
                scan: move |cfg| {
                    let handle = scan_handle.clone();
                    Box::pin(async move { scan_with_bootstrap(&handle, cfg).await })
                },
                load_config: || git_clawler_core::config::load_or_default().map_err(Into::into),
                reload_watcher: move |wc| watcher.reload(wc),
                on_config_reloaded: Some(std::sync::Arc::new({
                    let cfg_handle = handle.clone();
                    let notif_state = notif_state.clone();
                    move |cfg: &AppConfig| {
                        let accel = if cfg.gui.global_shortcut.trim().is_empty() {
                            None
                        } else {
                            Some(cfg.gui.global_shortcut.as_str())
                        };
                        let shortcut_result =
                            crate::shortcut::reregister_if_changed(&cfg_handle, accel);
                        cfg_handle
                            .state::<crate::GuiPolicyState>()
                            .update(cfg.gui.close_to_tray, cfg.gui.hide_from_taskbar);
                        let mut state = match notif_state.lock() {
                            Ok(state) => state,
                            Err(poisoned) => {
                                tracing::warn!("notification state mutex poisoned; recovering");
                                poisoned.into_inner()
                            }
                        };
                        reload_notification_state(&mut state, cfg);
                        shortcut_result
                    }
                })),
            };

            run_realtime_loop(
                initial_cfg,
                config_path,
                RealtimeChannels {
                    event_rx,
                    manual_refresh_rx,
                    shutdown,
                    completion_tx,
                },
                emitter,
                deps,
            )
            .await;
            Ok(())
        }
        Err(e) => {
            emitter.emit_error(&make_error("watcher", e.to_string()));

            // Degraded fallback: poll-only loop without filesystem watcher.
            match scan_with_bootstrap(&handle, initial_cfg.clone()).await {
                Ok(payload) => emitter.emit_repos(&payload),
                Err(err) => emitter.emit_error(&make_error("scan", err.message)),
            }

            let poll_interval = Duration::from_secs(initial_cfg.general.refresh_interval_secs);
            let mut interval = tokio::time::interval_at(
                tokio::time::Instant::now() + poll_interval,
                poll_interval,
            );
            let mut manual_refresh_rx = manual_refresh_rx;
            let mut manual_refresh_open = true;

            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown => break,
                    maybe_refresh = manual_refresh_rx.recv(), if manual_refresh_open => {
                        if maybe_refresh.is_some() {
                            while manual_refresh_rx.try_recv().is_ok() {}
                            match scan_with_bootstrap(&handle, initial_cfg.clone()).await {
                                Ok(payload) => emitter.emit_repos(&payload),
                                Err(err) => emitter.emit_error(&make_error("scan", err.message)),
                            }
                        } else {
                            manual_refresh_open = false;
                        }
                    }
                    _ = interval.tick() => {
                        match scan_with_bootstrap(&handle, initial_cfg.clone()).await {
                            Ok(payload) => emitter.emit_repos(&payload),
                            Err(err) => emitter.emit_error(&make_error("scan", err.message)),
                        }
                    }
                }
            }
            if let Some(tx) = completion_tx {
                let _ = tx.send(());
            }
            Ok(())
        }
    }
}

/// Wraps `list_repositories_inner_with_config` with the lazy-bootstrap
/// callback wired from the Tauri-managed `HelperRegistryState` and
/// `HelperResourcePath`. Mirrors the production `list_repositories`
/// command so the realtime loop also benefits from per-distro lazy install.
#[cfg(target_os = "windows")]
async fn scan_with_bootstrap(
    handle: &tauri::AppHandle,
    cfg: AppConfig,
) -> Result<ListRepositoriesResponse, CommandError> {
    use crate::commands::{BootstrapFn, HelperRegistryState, HelperResourcePath};
    use crate::wsl::BootstrapInput;
    use tauri::Manager;

    let registry = handle.state::<HelperRegistryState>().0.clone();
    let resource = handle.state::<HelperResourcePath>().0.clone();

    let bootstrap: Box<BootstrapFn> = Box::new(move |distro: Option<String>| {
        let registry = registry.clone();
        let resource = resource.clone();
        Box::pin(async move {
            registry
                .ensure(BootstrapInput {
                    distro,
                    resource_helper_path: resource,
                })
                .await
        })
    });

    crate::commands::list_repositories_inner_with_config(cfg, Some(&*bootstrap)).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::ListRepositoriesResponse;
    use futures::future::BoxFuture;
    use git_clawler_core::{AppConfig, WatchEvent};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::{mpsc, oneshot};

    #[derive(Default, Clone)]
    struct MockEmitter {
        repos: Arc<Mutex<Vec<ListRepositoriesResponse>>>,
        errors: Arc<Mutex<Vec<RealtimeErrorPayload>>>,
        shortcut_statuses: Arc<Mutex<Vec<ShortcutStatusPayload>>>,
    }

    impl Emitter for MockEmitter {
        fn emit_repos(&self, p: &ListRepositoriesResponse) {
            self.repos.lock().unwrap().push(p.clone());
        }
        fn emit_error(&self, p: &RealtimeErrorPayload) {
            self.errors.lock().unwrap().push(p.clone());
        }
        fn emit_shortcut_status(&self, p: &ShortcutStatusPayload) {
            self.shortcut_statuses.lock().unwrap().push(p.clone());
        }
    }

    fn ok_scanner(
    ) -> impl Fn(AppConfig) -> BoxFuture<'static, Result<ListRepositoriesResponse, CommandError>>
           + Send
           + Sync
           + 'static {
        |_| Box::pin(async { Ok(ListRepositoriesResponse { rows: vec![] }) })
    }

    fn ok_loader() -> impl Fn() -> Result<AppConfig, anyhow::Error> + Send + Sync + 'static {
        || Ok(AppConfig::default())
    }

    fn watcher_and_scan_changed_loader(
    ) -> impl Fn() -> Result<AppConfig, anyhow::Error> + Send + Sync + 'static {
        || {
            let mut cfg = AppConfig::default();
            cfg.general.refresh_interval_secs = 45;
            cfg.github.enabled = true;
            Ok(cfg)
        }
    }

    fn shortcut_changed_loader(
    ) -> impl Fn() -> Result<AppConfig, anyhow::Error> + Send + Sync + 'static {
        || {
            let mut cfg = AppConfig::default();
            cfg.gui.global_shortcut = "Ctrl+Alt+G".into();
            Ok(cfg)
        }
    }

    fn layout_changed_loader(
    ) -> impl Fn() -> Result<AppConfig, anyhow::Error> + Send + Sync + 'static {
        || {
            let mut cfg = AppConfig::default();
            cfg.gui.window_width = Some(1024);
            cfg.gui.window_height = Some(768);
            cfg.gui.column_widths.insert("branch".into(), 160);
            Ok(cfg)
        }
    }

    fn policy_changed_loader(
    ) -> impl Fn() -> Result<AppConfig, anyhow::Error> + Send + Sync + 'static {
        || {
            let mut cfg = AppConfig::default();
            cfg.gui.close_to_tray = false;
            cfg.gui.hide_from_taskbar = false;
            Ok(cfg)
        }
    }

    fn noop_reload() -> impl FnMut(WatcherConfig) -> anyhow::Result<()> + Send + 'static {
        |_| Ok(())
    }

    /// Helper to spawn the loop and return shutdown handle + emitter.
    fn spawn_loop<S, L, R>(
        rx: mpsc::Receiver<WatchEvent>,
        scan: S,
        load: L,
        reload: R,
    ) -> (
        tokio::task::JoinHandle<()>,
        oneshot::Sender<()>,
        MockEmitter,
        mpsc::Sender<()>,
    )
    where
        S: Fn(AppConfig) -> BoxFuture<'static, Result<ListRepositoriesResponse, CommandError>>
            + Send
            + Sync
            + 'static,
        L: Fn() -> Result<AppConfig, anyhow::Error> + Send + Sync + 'static,
        R: FnMut(WatcherConfig) -> anyhow::Result<()> + Send + 'static,
    {
        let emitter = MockEmitter::default();
        let emitter_clone = emitter.clone();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (manual_refresh_tx, manual_refresh_rx) = mpsc::channel::<()>(8);
        let deps = RealtimeDeps {
            scan,
            load_config: load,
            reload_watcher: reload,
            on_config_reloaded: None,
        };
        let handle = tokio::spawn(async move {
            run_realtime_loop(
                AppConfig::default(),
                None,
                RealtimeChannels {
                    event_rx: rx,
                    manual_refresh_rx,
                    shutdown: shutdown_rx,
                    completion_tx: None,
                },
                emitter_clone,
                deps,
            )
            .await;
        });
        (handle, shutdown_tx, emitter, manual_refresh_tx)
    }

    #[tokio::test]
    async fn initial_emit_on_start() {
        let (_tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (handle, shutdown, emitter, _refresh) =
            spawn_loop(rx, ok_scanner(), ok_loader(), noop_reload());
        // Allow initial scan to run.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = shutdown.send(());
        handle.await.unwrap();
        assert_eq!(emitter.repos.lock().unwrap().len(), 1);
        assert!(emitter.errors.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tick_triggers_emit() {
        let (tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (handle, shutdown, emitter, _refresh) =
            spawn_loop(rx, ok_scanner(), ok_loader(), noop_reload());
        tokio::time::sleep(Duration::from_millis(30)).await;
        tx.send(WatchEvent::Tick).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = shutdown.send(());
        handle.await.unwrap();
        assert_eq!(emitter.repos.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn repo_changed_triggers_emit() {
        let (tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (handle, shutdown, emitter, _refresh) =
            spawn_loop(rx, ok_scanner(), ok_loader(), noop_reload());
        tokio::time::sleep(Duration::from_millis(30)).await;
        tx.send(WatchEvent::RepoChanged {
            root: PathBuf::from("/x"),
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = shutdown.send(());
        handle.await.unwrap();
        assert_eq!(emitter.repos.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn coalesce_during_inflight_scan() {
        use tokio::sync::Notify;

        let counter = Arc::new(AtomicUsize::new(0));
        let start_notify = Arc::new(Notify::new());
        let release_notify = Arc::new(Notify::new());

        let counter_c = counter.clone();
        let start_c = start_notify.clone();
        let release_c = release_notify.clone();

        let scanner = move |_cfg: AppConfig| {
            let c = counter_c.clone();
            let start = start_c.clone();
            let release = release_c.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
                start.notify_one();
                release.notified().await;
                Ok(ListRepositoriesResponse { rows: vec![] })
            }) as BoxFuture<'static, _>
        };

        let (tx, rx) = mpsc::channel::<WatchEvent>(16);
        let (handle, shutdown, _emitter, _refresh) =
            spawn_loop(rx, scanner, ok_loader(), noop_reload());

        // 1. Wait for initial scan to start
        start_notify.notified().await;

        // 2. Push 5 events while initial scan is blocked (they should be coalesced)
        for _ in 0..5 {
            tx.send(WatchEvent::Tick).await.unwrap();
        }
        tokio::task::yield_now().await;

        // 3. Release initial scan
        release_notify.notify_one();

        // 4. Wait for the single trailing scan to start
        start_notify.notified().await;

        // 5. Release trailing scan
        release_notify.notify_one();

        // 6. Allow trailing scan to complete and loop to settle
        tokio::time::sleep(Duration::from_millis(20)).await;

        // 7. Shutdown
        let _ = shutdown.send(());
        handle.await.unwrap();

        // 8. Exactly initial(1) + trailing(1) = 2 scans total
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "expected initial(1) + trailing(1) = 2 scans, got {}",
            counter.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn config_changed_calls_reload_then_emits() {
        let reload_count = Arc::new(AtomicUsize::new(0));
        let reload_count_clone = reload_count.clone();
        let reload = move |_wc: WatcherConfig| {
            reload_count_clone.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let (tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (handle, shutdown, emitter, _refresh) =
            spawn_loop(rx, ok_scanner(), watcher_and_scan_changed_loader(), reload);
        tokio::time::sleep(Duration::from_millis(30)).await;
        tx.send(WatchEvent::ConfigChanged).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = shutdown.send(());
        handle.await.unwrap();
        assert_eq!(reload_count.load(Ordering::SeqCst), 1);
        // initial + post-config emits.
        assert_eq!(emitter.repos.lock().unwrap().len(), 2);
        assert!(emitter.errors.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn layout_only_config_change_does_not_reload_or_scan() {
        let reload_count = Arc::new(AtomicUsize::new(0));
        let reload_count_clone = reload_count.clone();
        let reload = move |_wc: WatcherConfig| {
            reload_count_clone.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let (tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (handle, shutdown, emitter, _refresh) =
            spawn_loop(rx, ok_scanner(), layout_changed_loader(), reload);
        tokio::time::sleep(Duration::from_millis(30)).await;
        tx.send(WatchEvent::ConfigChanged).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = shutdown.send(());
        handle.await.unwrap();
        assert_eq!(reload_count.load(Ordering::SeqCst), 0);
        assert_eq!(emitter.repos.lock().unwrap().len(), 1);
        assert!(emitter.errors.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn watcher_error_emits_error_event() {
        let (tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (handle, shutdown, emitter, _refresh) =
            spawn_loop(rx, ok_scanner(), ok_loader(), noop_reload());
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(WatchEvent::WatcherError("disk full".into()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = shutdown.send(());
        handle.await.unwrap();
        let errors = emitter.errors.lock().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, "watcher");
        assert_eq!(errors[0].message, "disk full");
    }

    #[tokio::test]
    async fn error_throttled_within_30s() {
        let (tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (handle, shutdown, emitter, _refresh) =
            spawn_loop(rx, ok_scanner(), ok_loader(), noop_reload());
        tokio::time::sleep(Duration::from_millis(20)).await;
        for _ in 0..3 {
            tx.send(WatchEvent::WatcherError("same".into()))
                .await
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = shutdown.send(());
        handle.await.unwrap();
        assert_eq!(emitter.errors.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn error_not_throttled_after_30s() {
        let (tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (handle, shutdown, emitter, _refresh) =
            spawn_loop(rx, ok_scanner(), ok_loader(), noop_reload());
        // Yield to let the loop run initial scan and reach the select.
        tokio::time::advance(Duration::from_millis(10)).await;
        tokio::task::yield_now().await;

        tx.send(WatchEvent::WatcherError("boom".into()))
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(10)).await;
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::task::yield_now().await;

        tx.send(WatchEvent::WatcherError("boom".into()))
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(10)).await;
        tokio::task::yield_now().await;

        let _ = shutdown.send(());
        handle.await.unwrap();
        assert_eq!(emitter.errors.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn shutdown_signal_breaks_loop() {
        let (_tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (handle, shutdown, _emitter, _refresh) =
            spawn_loop(rx, ok_scanner(), ok_loader(), noop_reload());
        let _ = shutdown.send(());
        // Should complete quickly.
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("loop should exit on shutdown")
            .unwrap();
    }

    #[tokio::test]
    async fn scanner_failure_emits_error() {
        let scanner = |_cfg: AppConfig| {
            Box::pin(async {
                Err(CommandError {
                    kind: "internal".into(),
                    message: "oops".into(),
                })
            }) as BoxFuture<'static, _>
        };
        let (_tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (handle, shutdown, emitter, _refresh) =
            spawn_loop(rx, scanner, ok_loader(), noop_reload());
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = shutdown.send(());
        handle.await.unwrap();
        let errors = emitter.errors.lock().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, "scan");
        assert_eq!(errors[0].message, "oops");
    }

    #[tokio::test]
    async fn config_changed_invokes_on_config_reloaded_callback() {
        use std::sync::Arc;
        let cb_count = Arc::new(AtomicUsize::new(0));
        let cb_count_clone = cb_count.clone();

        let emitter = MockEmitter::default();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (_manual_refresh_tx, manual_refresh_rx) = mpsc::channel::<()>(1);
        let deps = RealtimeDeps {
            scan: ok_scanner(),
            load_config: shortcut_changed_loader(),
            reload_watcher: noop_reload(),
            on_config_reloaded: Some(Arc::new(move |_cfg: &AppConfig| {
                cb_count_clone.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
        };
        let emitter_clone = emitter.clone();
        let handle = tokio::spawn(async move {
            run_realtime_loop(
                AppConfig::default(),
                None,
                RealtimeChannels {
                    event_rx: rx,
                    manual_refresh_rx,
                    shutdown: shutdown_rx,
                    completion_tx: None,
                },
                emitter_clone,
                deps,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(WatchEvent::ConfigChanged).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let _ = shutdown_tx.send(());
        handle.await.unwrap();
        assert_eq!(cb_count.load(Ordering::SeqCst), 1);
        let statuses = emitter.shortcut_statuses.lock().unwrap();
        assert_eq!(statuses.len(), 1);
        assert!(statuses[0].registered);
    }

    #[tokio::test]
    async fn config_changed_retries_runtime_hook_even_when_config_unchanged() {
        use std::sync::Arc;
        let cb_count = Arc::new(AtomicUsize::new(0));
        let cb_count_clone = cb_count.clone();

        let emitter = MockEmitter::default();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (_manual_refresh_tx, manual_refresh_rx) = mpsc::channel::<()>(1);
        let deps = RealtimeDeps {
            scan: ok_scanner(),
            load_config: ok_loader(),
            reload_watcher: noop_reload(),
            on_config_reloaded: Some(Arc::new(move |_cfg: &AppConfig| {
                cb_count_clone.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
        };
        let emitter_clone = emitter.clone();
        let handle = tokio::spawn(async move {
            run_realtime_loop(
                AppConfig::default(),
                None,
                RealtimeChannels {
                    event_rx: rx,
                    manual_refresh_rx,
                    shutdown: shutdown_rx,
                    completion_tx: None,
                },
                emitter_clone,
                deps,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(WatchEvent::ConfigChanged).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let _ = shutdown_tx.send(());
        handle.await.unwrap();
        assert_eq!(cb_count.load(Ordering::SeqCst), 1);
        assert_eq!(emitter.shortcut_statuses.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn config_changed_hook_error_emits_shortcut_error_and_continues_scan() {
        use std::sync::Arc;

        let emitter = MockEmitter::default();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (_manual_refresh_tx, manual_refresh_rx) = mpsc::channel::<()>(1);
        let load_config = || {
            let mut cfg = AppConfig::default();
            cfg.github.enabled = true;
            cfg.gui.global_shortcut = "Control+PageUp".into();
            Ok(cfg)
        };
        let deps = RealtimeDeps {
            scan: ok_scanner(),
            load_config,
            reload_watcher: noop_reload(),
            on_config_reloaded: Some(Arc::new(move |_cfg: &AppConfig| {
                Err("global shortcut register failed for Control+PageUp".into())
            })),
        };
        let emitter_clone = emitter.clone();
        let handle = tokio::spawn(async move {
            run_realtime_loop(
                AppConfig::default(),
                None,
                RealtimeChannels {
                    event_rx: rx,
                    manual_refresh_rx,
                    shutdown: shutdown_rx,
                    completion_tx: None,
                },
                emitter_clone,
                deps,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(WatchEvent::ConfigChanged).await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        let _ = shutdown_tx.send(());
        handle.await.unwrap();

        let errors = emitter.errors.lock().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, "shortcut");
        assert!(errors[0].message.contains("Control+PageUp"));
        assert!(emitter.shortcut_statuses.lock().unwrap().is_empty());
        drop(errors);
        assert_eq!(
            emitter.repos.lock().unwrap().len(),
            2,
            "initial scan + config-triggered scan should still complete"
        );
    }

    #[tokio::test]
    async fn config_changed_invokes_callback_for_gui_policy_change() {
        use std::sync::Arc;
        let cb_count = Arc::new(AtomicUsize::new(0));
        let cb_count_clone = cb_count.clone();

        let emitter = MockEmitter::default();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (_manual_refresh_tx, manual_refresh_rx) = mpsc::channel::<()>(1);
        let deps = RealtimeDeps {
            scan: ok_scanner(),
            load_config: policy_changed_loader(),
            reload_watcher: noop_reload(),
            on_config_reloaded: Some(Arc::new(move |_cfg: &AppConfig| {
                cb_count_clone.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
        };
        let emitter_clone = emitter.clone();
        let handle = tokio::spawn(async move {
            run_realtime_loop(
                AppConfig::default(),
                None,
                RealtimeChannels {
                    event_rx: rx,
                    manual_refresh_rx,
                    shutdown: shutdown_rx,
                    completion_tx: None,
                },
                emitter_clone,
                deps,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(WatchEvent::ConfigChanged).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let _ = shutdown_tx.send(());
        handle.await.unwrap();
        assert_eq!(cb_count.load(Ordering::SeqCst), 1);
    }

    fn notification_state(enabled: bool) -> NotifState {
        NotifState {
            prev_snapshot: Some(git_clawler_core::notifications::Snapshot(
                std::collections::BTreeMap::new(),
            )),
            cooldown: git_clawler_core::notifications::CooldownTracker::new(300),
            cooldown_secs: 300,
            enabled,
        }
    }

    #[test]
    fn reload_notification_state_clears_snapshot_when_enabled_turns_on() {
        let mut state = notification_state(false);
        let mut cfg = AppConfig::default();
        cfg.notifications.enabled = true;

        reload_notification_state(&mut state, &cfg);

        assert!(state.enabled);
        assert!(state.prev_snapshot.is_none());
    }

    #[test]
    fn reload_notification_state_clears_snapshot_when_enabled_turns_off() {
        let mut state = notification_state(true);
        let mut cfg = AppConfig::default();
        cfg.notifications.enabled = false;

        reload_notification_state(&mut state, &cfg);

        assert!(!state.enabled);
        assert!(state.prev_snapshot.is_none());
    }

    #[test]
    fn reload_notification_state_preserves_snapshot_when_enabled_unchanged() {
        let mut state = notification_state(true);
        let mut cfg = AppConfig::default();
        cfg.notifications.enabled = true;
        cfg.notifications.cooldown_secs = 60;

        reload_notification_state(&mut state, &cfg);

        assert!(state.enabled);
        assert!(state.prev_snapshot.is_some());
    }

    #[test]
    fn reload_notification_state_preserves_cooldown_when_notifications_unchanged() {
        let mut state = notification_state(true);
        state.cooldown.record();
        let mut cfg = AppConfig::default();
        cfg.notifications.enabled = true;
        cfg.notifications.cooldown_secs = 300;

        reload_notification_state(&mut state, &cfg);

        assert!(!state.cooldown.allow());
        assert_eq!(state.cooldown_secs, 300);
    }

    #[tokio::test]
    async fn config_change_failure_does_not_invoke_callback() {
        use std::sync::Arc;
        let cb_count = Arc::new(AtomicUsize::new(0));
        let cb_count_clone = cb_count.clone();

        let emitter = MockEmitter::default();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (_manual_refresh_tx, manual_refresh_rx) = mpsc::channel::<()>(1);
        let failing_loader =
            || -> Result<AppConfig, anyhow::Error> { Err(anyhow::anyhow!("config parse failed")) };
        let deps = RealtimeDeps {
            scan: ok_scanner(),
            load_config: failing_loader,
            reload_watcher: noop_reload(),
            on_config_reloaded: Some(Arc::new(move |_cfg: &AppConfig| {
                cb_count_clone.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
        };
        let emitter_clone = emitter.clone();
        let handle = tokio::spawn(async move {
            run_realtime_loop(
                AppConfig::default(),
                None,
                RealtimeChannels {
                    event_rx: rx,
                    manual_refresh_rx,
                    shutdown: shutdown_rx,
                    completion_tx: None,
                },
                emitter_clone,
                deps,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(WatchEvent::ConfigChanged).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let _ = shutdown_tx.send(());
        handle.await.unwrap();
        assert_eq!(cb_count.load(Ordering::SeqCst), 0);
        // The error should have been emitted.
        assert_eq!(emitter.errors.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn completion_fires_when_loop_exits() {
        let (_tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (_manual_refresh_tx, manual_refresh_rx) = mpsc::channel::<()>(1);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (completion_tx, completion_rx) = oneshot::channel::<()>();
        let emitter = MockEmitter::default();
        let deps = RealtimeDeps {
            scan: ok_scanner(),
            load_config: ok_loader(),
            reload_watcher: noop_reload(),
            on_config_reloaded: None,
        };
        let handle = tokio::spawn(async move {
            run_realtime_loop(
                AppConfig::default(),
                None,
                RealtimeChannels {
                    event_rx: rx,
                    manual_refresh_rx,
                    shutdown: shutdown_rx,
                    completion_tx: Some(completion_tx),
                },
                emitter,
                deps,
            )
            .await;
        });
        let _ = shutdown_tx.send(());
        tokio::time::timeout(Duration::from_secs(1), completion_rx)
            .await
            .expect("completion should fire within 1 second")
            .expect("completion_rx should not be dropped");
        handle.await.unwrap();
    }

    #[test]
    fn watcher_repos_skips_wsl_entries() {
        use git_clawler_core::config::RepoEntry;
        use std::path::PathBuf;

        let native = tempfile::tempdir().unwrap();
        let cfg = AppConfig {
            repositories: vec![
                RepoEntry {
                    name: Some("native".into()),
                    path: native.path().to_path_buf(),
                    fetch: None,
                    distro: None,
                },
                RepoEntry {
                    name: Some("wsl".into()),
                    path: PathBuf::from("/home/me/proj"),
                    fetch: None,
                    distro: Some("Ubuntu".into()),
                },
            ],
            ..Default::default()
        };

        let watched = watcher_repos_from_config(&cfg);
        assert_eq!(watched.len(), 1);
        assert_eq!(watched[0], native.path().to_path_buf());
    }

    #[tokio::test]
    async fn manual_refresh_triggers_emit() {
        let (_tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (handle, shutdown, emitter, refresh_tx) =
            spawn_loop(rx, ok_scanner(), ok_loader(), noop_reload());
        // Let initial scan complete.
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Send a manual refresh.
        refresh_tx.send(()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = shutdown.send(());
        handle.await.unwrap();
        // initial(1) + manual_refresh(1) = 2
        assert_eq!(
            emitter.repos.lock().unwrap().len(),
            2,
            "manual refresh should trigger a second scan"
        );
    }

    #[tokio::test]
    async fn manual_refresh_scan_errors_are_not_throttled() {
        let scanner = |_cfg: AppConfig| {
            Box::pin(async {
                Err(CommandError {
                    kind: "internal".into(),
                    message: "same manual failure".into(),
                })
            }) as BoxFuture<'static, _>
        };
        let (_tx, rx) = mpsc::channel::<WatchEvent>(8);
        let (handle, shutdown, emitter, refresh_tx) =
            spawn_loop(rx, scanner, ok_loader(), noop_reload());

        tokio::time::sleep(Duration::from_millis(30)).await;
        refresh_tx.send(()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        refresh_tx.send(()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;

        let _ = shutdown.send(());
        handle.await.unwrap();
        let errors = emitter.errors.lock().unwrap();
        assert_eq!(
            errors.len(),
            3,
            "initial failure + two manual failures should all be emitted"
        );
        assert!(errors.iter().all(|e| e.kind == "scan"));
    }

    #[tokio::test]
    async fn manual_refresh_coalesces_during_inflight_scan() {
        use tokio::sync::Notify;

        let counter = Arc::new(AtomicUsize::new(0));
        let start_notify = Arc::new(Notify::new());
        let release_notify = Arc::new(Notify::new());

        let counter_c = counter.clone();
        let start_c = start_notify.clone();
        let release_c = release_notify.clone();

        let scanner = move |_cfg: AppConfig| {
            let c = counter_c.clone();
            let start = start_c.clone();
            let release = release_c.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
                start.notify_one();
                release.notified().await;
                Ok(ListRepositoriesResponse { rows: vec![] })
            }) as BoxFuture<'static, _>
        };

        let (_tx, rx) = mpsc::channel::<WatchEvent>(16);
        let (handle, shutdown, _emitter, refresh_tx) =
            spawn_loop(rx, scanner, ok_loader(), noop_reload());

        // Wait for initial scan to start.
        start_notify.notified().await;

        // Send multiple manual refresh signals while scan is in-flight.
        for _ in 0..4 {
            let _ = refresh_tx.try_send(());
        }
        tokio::task::yield_now().await;

        // Release initial scan.
        release_notify.notify_one();

        // Wait for trailing scan to start.
        start_notify.notified().await;

        // Release trailing scan.
        release_notify.notify_one();

        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = shutdown.send(());
        handle.await.unwrap();

        // initial(1) + trailing(1) = 2; coalesced bursts count as one trailing.
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "coalesced manual refreshes should result in exactly 2 scans total"
        );
    }
}
