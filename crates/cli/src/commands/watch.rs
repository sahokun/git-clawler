//! `watch` subcommand — interactive real-time repository monitor.
//!
//! # Event sources → coalesce → render pipeline
//!
//! Three concurrent event sources feed a single [`WatchState`] state machine:
//!
//! 1. **`core::Watcher` channel** (`WatchEvent`):
//!    - `Tick` — periodic poll timer (`general.refresh_interval_secs`, default 30 s)
//!    - `RepoChanged` — debounced filesystem event from `notify`
//!    - `ConfigChanged` — config file edited; triggers watcher rebuild via
//!      [`handle_config_changed`]
//!    - `WatcherError` — inotify / notify failure; logged and ignored
//!
//! 2. **`crossterm` key events** (raw-mode `EventStream`):
//!    - `r` → [`set_refresh_pending`]
//!    - `q` / `Ctrl-C` → break loop
//!
//! 3. **OS signals** (Unix only):
//!    - `SIGTERM` → break loop (via [`sigterm_future`])
//!
//! Any event that arrives while a refresh is already in-progress is collapsed
//! to a single `refresh_pending` flag ("coalesce").  After the current refresh
//! completes the loop drains any further events before executing at most one
//! additional refresh.
//!
//! # Signal handling
//!
//! `tokio::signal::ctrl_c()` covers `SIGINT` on all platforms.
//! `sigterm_future()` additionally handles `SIGTERM` on Unix.
//! Both paths simply break the main loop; cleanup is handled by `TerminalGuard`'s `Drop`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures::StreamExt;

use crate::scan;
use crate::term::TerminalGuard;
use git_clawler_core::notifications::{diff, CooldownTracker, Snapshot};
use git_clawler_core::{WatchEvent, Watcher, WatcherConfig};

pub(crate) struct WatchState {
    pub(crate) active_config: git_clawler_core::AppConfig,
    pub(crate) poll_interval: Duration,
    pub(crate) last_rows: Vec<crate::scan::ListRow>,
    pub(crate) last_refresh_at: chrono::DateTime<chrono::Utc>,
    pub(crate) refresh_in_progress: bool,
    pub(crate) refresh_pending: bool,
    pub(crate) force_github: bool,
    pub(crate) show_github: bool,
    pub(crate) prev_snapshot: Option<Snapshot>,
    pub(crate) notif_cooldown: CooldownTracker,
}

/// Applies a config-load result to the watch state.
/// On error, keeps the current config and logs a warning.
pub(crate) fn apply_config_change(
    state: &mut WatchState,
    result: Result<git_clawler_core::AppConfig, anyhow::Error>,
) {
    match result {
        Ok(new_cfg) => {
            state.poll_interval = Duration::from_secs(new_cfg.general.refresh_interval_secs);
            state.notif_cooldown = CooldownTracker::new(new_cfg.notifications.cooldown_secs);
            state.show_github = state.force_github || new_cfg.github.enabled;
            state.active_config = new_cfg;
        }
        Err(e) => {
            tracing::warn!("config reload failed, keeping current config: {}", e);
        }
    }
}

/// Marks a refresh as pending in the state machine.
/// The loop top dispatches the refresh when no refresh is already in progress.
/// Callers never need to drive `do_refresh()` themselves.
pub(crate) fn set_refresh_pending(state: &mut WatchState) {
    state.refresh_pending = true;
}

fn repos_from_config(config: &git_clawler_core::AppConfig) -> Vec<PathBuf> {
    git_clawler_core::Registry::from_config(config)
        .list()
        .iter()
        .map(|r| r.path().to_path_buf())
        .collect()
}

fn handle_config_changed(
    state: &mut WatchState,
    watcher: &mut Watcher,
    config_path: &Option<PathBuf>,
) {
    // Load the new config first; on failure keep current state unchanged.
    let new_cfg = match git_clawler_core::config::load_or_default() {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!("config reload failed, keeping current config: {}", e);
            return;
        }
    };

    // Build the new watcher config from the prospective new app config.
    let new_poll_interval = Duration::from_secs(new_cfg.general.refresh_interval_secs);
    let repos = repos_from_config(&new_cfg);
    let watcher_cfg = WatcherConfig {
        repos,
        config_path: config_path.clone(),
        poll_interval: new_poll_interval,
        debounce: Duration::from_millis(500),
    };

    // Reload the watcher *before* committing state so that a reload failure
    // cannot leave WatchState and the live watcher in divergent configurations.
    if let Err(e) = watcher.reload(watcher_cfg) {
        tracing::warn!("watcher reload failed, keeping current config: {}", e);
        return;
    }

    // Watcher reload succeeded; now it is safe to commit the new config.
    apply_config_change(state, Ok(new_cfg));
}

/// Resolves when SIGTERM is received (Unix). Never resolves on other platforms.
async fn sigterm_future() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut s) = signal(SignalKind::terminate()) {
            s.recv().await;
            return;
        }
    }
    std::future::pending::<()>().await
}

pub async fn run(with_github: bool) -> anyhow::Result<()> {
    // Acquire terminal first so any subsequent failure path properly restores it.
    let _guard = TerminalGuard::enter()?;

    let config = git_clawler_core::config::load_or_default()?;
    let config_path = git_clawler_core::config::default_config_path().ok();

    let poll_interval = Duration::from_secs(config.general.refresh_interval_secs);
    let repos = repos_from_config(&config);

    let watcher_cfg = WatcherConfig {
        repos,
        config_path: config_path.clone(),
        poll_interval,
        debounce: Duration::from_millis(500),
    };

    let (mut watcher, mut event_rx) = Watcher::start(watcher_cfg)?;

    let notifications_cooldown = config.notifications.cooldown_secs;
    let show_github = config.github.enabled || with_github;
    let mut state = WatchState {
        active_config: config,
        poll_interval,
        last_rows: vec![],
        last_refresh_at: chrono::Utc::now(),
        refresh_in_progress: false,
        refresh_pending: false,
        force_github: with_github,
        show_github,
        prev_snapshot: None,
        notif_cooldown: CooldownTracker::new(notifications_cooldown),
    };

    let mut event_reader = EventStream::new();
    tokio::pin!(let sigterm = sigterm_future(););
    state.refresh_pending = true;

    loop {
        // If a refresh is pending and none is in progress, run one (with
        // drain-and-coalesce after) before blocking on select!.
        if state.refresh_pending && !state.refresh_in_progress {
            state.refresh_pending = false;
            state.refresh_in_progress = true;
            // Keep termination responsive during the initial and subsequent
            // refreshes. Dropping scan_all propagates cancellation to fetch.
            let mut refresh_again = false;
            {
                let refresh = do_refresh(&mut state);
                tokio::pin!(refresh);
                loop {
                    tokio::select! {
                        _ = &mut refresh => break,
                        _ = tokio::signal::ctrl_c() => return Ok(()),
                        _ = &mut sigterm => return Ok(()),
                        event = event_reader.next() => match event {
                            Some(Ok(Event::Key(key))) => match key.code {
                                KeyCode::Char('q') => return Ok(()),
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                                KeyCode::Char('r') => refresh_again = true,
                                _ => {}
                            },
                            None => return Ok(()),
                            _ => {}
                        }
                    }
                }
            }
            state.refresh_in_progress = false;
            state.refresh_pending |= refresh_again;

            // Drain events that arrived during refresh; collapse to a single
            // pending flag so we don't queue back-to-back redundant scans.
            while let Ok(ev) = event_rx.try_recv() {
                match ev {
                    WatchEvent::ConfigChanged => {
                        handle_config_changed(&mut state, &mut watcher, &config_path);
                        state.refresh_pending = true;
                    }
                    WatchEvent::Tick | WatchEvent::RepoChanged { .. } => {
                        state.refresh_pending = true;
                    }
                    WatchEvent::WatcherError(e) => {
                        tracing::warn!("watcher error: {}", e);
                    }
                }
            }

            continue;
        }

        tokio::select! {
            maybe_event = event_rx.recv() => {
                match maybe_event {
                    Some(WatchEvent::ConfigChanged) => {
                        handle_config_changed(&mut state, &mut watcher, &config_path);
                        set_refresh_pending(&mut state);
                    }
                    Some(WatchEvent::Tick) | Some(WatchEvent::RepoChanged { .. }) => {
                        set_refresh_pending(&mut state);
                    }
                    Some(WatchEvent::WatcherError(e)) => {
                        tracing::warn!("watcher error: {}", e);
                    }
                    None => break,
                }
            }
            maybe_key = event_reader.next() => {
                match maybe_key {
                    Some(Ok(Event::Key(key))) => {
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                            KeyCode::Char('r') => {
                                set_refresh_pending(&mut state);
                            }
                            _ => {}
                        }
                    }
                    Some(Err(e)) => {
                        tracing::warn!("input error: {}", e);
                    }
                    None => break,
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = &mut sigterm => break,
        }
    }

    Ok(())
}

async fn do_refresh(state: &mut WatchState) {
    use crossterm::{cursor, execute, terminal};
    use std::io::Write;

    let started = Instant::now();
    let targets = scan::build_targets(&state.active_config);
    let rows = if state.show_github {
        let mut github_cfg = state.active_config.github.clone();
        github_cfg.enabled = true;
        scan::scan_all_with_github(targets, &github_cfg).await
    } else {
        scan::scan_all(targets).await
    };
    state.last_rows = rows;
    state.last_refresh_at = chrono::Utc::now();
    maybe_notify_changes(state).await;

    let mut stdout = std::io::stdout();
    let _ = execute!(
        stdout,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0)
    );

    let table = crate::render::render_list_table(&state.last_rows, true, state.show_github);
    print!("{}\r\n", table.replace('\n', "\r\n"));
    print!(
        "\r\n{}\r\n",
        crate::render::format_watch_footer(state.last_refresh_at, state.poll_interval.as_secs())
    );
    let _ = stdout.flush();

    tracing::debug!(elapsed_ms = started.elapsed().as_millis() as u64, "refresh");
}

async fn maybe_notify_changes(state: &mut WatchState) {
    let next = Snapshot::from_rows(&state.last_rows);
    let Some(prev) = state.prev_snapshot.as_ref() else {
        state.prev_snapshot = Some(next);
        return;
    };
    let changes = diff(prev, &next);
    state.prev_snapshot = Some(next);
    if !state.active_config.notifications.enabled || changes.is_empty() {
        return;
    }
    if state.notif_cooldown.allow() {
        let summary = changes.summary();
        send_os_notification("git-clawler", &summary).await;
        state.notif_cooldown.record();
    }
}

#[cfg(target_os = "linux")]
async fn send_os_notification(title: &str, body: &str) {
    let mut command = tokio::process::Command::new("notify-send");
    command.arg(title).arg(body);
    let status =
        tokio::time::timeout(Duration::from_secs(5), command.kill_on_drop(true).status()).await;
    let status = match status {
        Ok(status) => status,
        Err(_) => {
            tracing::warn!("notify-send timed out");
            return;
        }
    };
    match status {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(
                "notify-send not found; install libnotify-bin for desktop notifications"
            );
        }
        Err(e) => tracing::warn!(error = %e, "notify-send failed"),
        Ok(status) if !status.success() => {
            tracing::warn!(%status, "notify-send exited unsuccessfully");
        }
        Ok(_) => {}
    }
}

#[cfg(not(target_os = "linux"))]
async fn send_os_notification(_title: &str, body: &str) {
    tracing::warn!(
        body,
        "desktop notifications are not supported on this platform"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_watch_state() -> WatchState {
        WatchState {
            active_config: git_clawler_core::AppConfig::default(),
            poll_interval: Duration::from_secs(30),
            last_rows: vec![],
            last_refresh_at: chrono::Utc::now(),
            refresh_in_progress: false,
            refresh_pending: false,
            force_github: false,
            show_github: false,
            prev_snapshot: None,
            notif_cooldown: CooldownTracker::new(300),
        }
    }

    #[test]
    fn test_refresh_keeps_previous_state_on_broken_config() {
        let mut state = default_watch_state();
        let orig_config = state.active_config.clone();
        let orig_interval = state.poll_interval;
        apply_config_change(&mut state, Err(anyhow::anyhow!("broken TOML")));
        assert_eq!(state.active_config, orig_config);
        assert_eq!(state.poll_interval, orig_interval);
    }

    #[test]
    fn test_set_refresh_pending_marks_state() {
        // set_refresh_pending() on an idle WatchState sets refresh_pending = true
        // and must NOT set refresh_in_progress (that is the loop top's job).
        let mut state = default_watch_state();
        assert!(!state.refresh_pending);
        assert!(!state.refresh_in_progress);
        set_refresh_pending(&mut state);
        assert!(state.refresh_pending, "should mark pending");
        assert!(
            !state.refresh_in_progress,
            "set_refresh_pending must not touch refresh_in_progress"
        );
    }

    #[test]
    fn test_coalesce_multiple_pending_requests() {
        // Multiple set_refresh_pending() calls while a refresh is in progress
        // should all collapse to a single pending flag.
        let mut state = default_watch_state();
        state.refresh_in_progress = true;
        set_refresh_pending(&mut state);
        set_refresh_pending(&mut state);
        set_refresh_pending(&mut state);
        assert!(
            state.refresh_pending,
            "all events should collapse to single pending"
        );
    }
}
