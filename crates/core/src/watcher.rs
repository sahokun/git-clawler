//! Filesystem watcher that emits coalesced repository-change events.
//!
//! The watcher combines three signals into a single `mpsc` stream consumed by
//! the `watch` UI loops:
//!
//! - **Tick**: a periodic timer for unconditional re-scans.
//! - **RepoChanged**: filesystem events from registered repositories,
//!   debounced to avoid bursts.
//! - **ConfigChanged**: edits to the user's config file.
//!
//! Filesystem noise such as `.git/objects/`, `target/`, and `node_modules/`
//! is filtered out before debouncing so that build/git plumbing churn does
//! not trigger expensive re-scans.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecursiveMode, Watcher as NotifyWatcher};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const CHANNEL_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub enum WatchEvent {
    Tick,
    RepoChanged { root: PathBuf },
    ConfigChanged,
    WatcherError(String),
}

#[derive(Debug, Clone)]
pub struct WatcherConfig {
    pub repos: Vec<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub poll_interval: Duration,
    pub debounce: Duration,
}

pub struct Watcher {
    notify_watcher: notify::RecommendedWatcher,
    tick_handle: JoinHandle<()>,
    debounce_handle: JoinHandle<()>,
    raw_tx: std_mpsc::Sender<RawSignal>,
    out_tx: mpsc::Sender<WatchEvent>,
}

impl Watcher {
    pub fn start(cfg: WatcherConfig) -> anyhow::Result<(Self, mpsc::Receiver<WatchEvent>)> {
        let (out_tx, out_rx) = mpsc::channel::<WatchEvent>(CHANNEL_CAPACITY);
        let (raw_tx, raw_rx) = std_mpsc::channel::<RawSignal>();

        let (watcher, watch_errors) =
            build_notify_watcher(raw_tx.clone(), &cfg.repos, cfg.config_path.as_ref())?;

        // Surface failures registering individual repos/config-dir as
        // WatcherError events so the UI can display them (in addition to the
        // tracing::warn! emitted inside build_notify_watcher).
        for err in watch_errors {
            let _ = out_tx.try_send(WatchEvent::WatcherError(err));
        }

        // Best-effort inotify capacity check on Linux
        #[cfg(target_os = "linux")]
        check_inotify_capacity(cfg.repos.len());

        let debounce = cfg.debounce;
        let out_tx_debounce = out_tx.clone();
        let debounce_handle: JoinHandle<()> = tokio::task::spawn_blocking(move || {
            run_debounce_loop(raw_rx, out_tx_debounce, debounce);
        });

        let tick_handle = spawn_tick_task(out_tx.clone(), cfg.poll_interval);

        Ok((
            Self {
                notify_watcher: watcher,
                tick_handle,
                debounce_handle,
                raw_tx,
                out_tx,
            },
            out_rx,
        ))
    }

    pub fn reload(&mut self, cfg: WatcherConfig) -> anyhow::Result<()> {
        let (new_watcher, watch_errors) =
            build_notify_watcher(self.raw_tx.clone(), &cfg.repos, cfg.config_path.as_ref())?;
        // Replace old watcher; RAII drop unregisters previous file watches.
        let _ = std::mem::replace(&mut self.notify_watcher, new_watcher);
        for err in watch_errors {
            let _ = self.out_tx.try_send(WatchEvent::WatcherError(err));
        }
        self.tick_handle.abort();
        self.tick_handle = spawn_tick_task(self.out_tx.clone(), cfg.poll_interval);
        // NOTE: debounce task is NOT restarted (debounce value changes not
        // supported in this release).
        Ok(())
    }

    pub fn shutdown(self) {
        // Dropping `self` triggers the explicit `Drop` impl below which aborts
        // background tasks; field drop alone (`JoinHandle`) does NOT abort.
        drop(self);
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // tick task: tokio::spawn — abort() is effective and stops it promptly.
        self.tick_handle.abort();
        // debounce task: spawn_blocking — abort() is best-effort only and
        // generally does NOT interrupt a running blocking task. The loop
        // actually exits when `raw_tx` (a field of Self, dropped after this
        // method returns) disconnects and `recv_timeout` returns
        // `Disconnected` within ~100ms.
        self.debounce_handle.abort();
        // notify_watcher: its own Drop unregisters all file watches.
    }
}

fn build_notify_watcher(
    raw_tx: std_mpsc::Sender<RawSignal>,
    repos: &[PathBuf],
    config_path: Option<&PathBuf>,
) -> anyhow::Result<(notify::RecommendedWatcher, Vec<String>)> {
    let raw_tx_inner = raw_tx.clone();
    let config_path_for_cb = config_path.cloned();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| match res {
        Ok(event) => {
            if is_read_only_noise(&event) {
                return;
            }
            for path in event.paths {
                if let Some(cfg_path) = &config_path_for_cb {
                    if path == *cfg_path
                        || path.file_name() == cfg_path.file_name()
                            && path.parent() == cfg_path.parent()
                    {
                        let _ = raw_tx_inner.send(RawSignal::Config);
                        continue;
                    }
                }
                let _ = raw_tx_inner.send(RawSignal::Path(path));
            }
        }
        Err(e) => {
            let _ = raw_tx_inner.send(RawSignal::Error(e.to_string()));
        }
    })?;

    let mut errors: Vec<String> = Vec::new();
    for repo in repos {
        if let Err(e) = watcher.watch(repo, RecursiveMode::Recursive) {
            tracing::warn!(path = %repo.display(), error = %e, "failed to watch repo");
            errors.push(format!("failed to watch {}: {}", repo.display(), e));
        }
    }

    if let Some(cfg_path) = config_path {
        if let Some(parent) = cfg_path.parent() {
            // First launch has no config directory yet. Establish the watch
            // before the first save so the running scan loop reloads it.
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    errors.push(format!(
                        "failed to create config dir {}: {}",
                        parent.display(),
                        e
                    ));
                }
            }
            if parent.exists() {
                if let Err(e) = watcher.watch(parent, RecursiveMode::NonRecursive) {
                    tracing::warn!(path = %parent.display(), error = %e, "failed to watch config dir");
                    errors.push(format!(
                        "failed to watch config dir {}: {}",
                        parent.display(),
                        e
                    ));
                }
            }
        }
    }

    Ok((watcher, errors))
}

fn spawn_tick_task(out_tx: mpsc::Sender<WatchEvent>, interval: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the immediate first tick to avoid double-refresh on start.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if out_tx.send(WatchEvent::Tick).await.is_err() {
                break;
            }
        }
    })
}

#[derive(Debug)]
enum RawSignal {
    Path(PathBuf),
    Config,
    Error(String),
}

/// Returns `true` for events that reading a repository produces without
/// changing it, so scans do not trigger themselves.
///
/// - `Access`: inotify reports opening and closing files and directories.
/// - `Modify(Metadata)`: timestamp and permission updates.
/// - `Modify(Any)` on a directory: Windows reports a directory whose
///   last-access time changed when it is listed. Real changes inside the
///   directory also arrive as events for the affected files.
fn is_read_only_noise(event: &Event) -> bool {
    match event.kind {
        EventKind::Access(_) | EventKind::Modify(notify::event::ModifyKind::Metadata(_)) => true,
        EventKind::Modify(notify::event::ModifyKind::Any) => {
            !event.paths.is_empty() && event.paths.iter().all(|p| p.is_dir())
        }
        _ => false,
    }
}

/// Returns `true` if the path should be ignored (build/git plumbing noise).
fn is_noise_path(p: &Path) -> bool {
    for comp in p.components() {
        let s = comp.as_os_str();
        if s == ".git" {
            // Allow .git/HEAD, .git/refs/... but ignore .git/objects/
            // We make a stricter decision below.
        }
        if s == "target" || s == "node_modules" {
            return true;
        }
    }
    // Ignore .git/objects/ specifically
    let mut iter = p.components().peekable();
    while let Some(c) = iter.next() {
        if c.as_os_str() == ".git" {
            if let Some(next) = iter.peek() {
                if next.as_os_str() == "objects" {
                    return true;
                }
            }
        }
    }
    false
}

fn run_debounce_loop(
    rx: std_mpsc::Receiver<RawSignal>,
    out: mpsc::Sender<WatchEvent>,
    debounce: Duration,
) {
    // Map root -> last seen instant. We map raw paths to their containing
    // repo root if possible by truncating; for simplicity we use the raw
    // path itself as the key and emit `RepoChanged { root: path }`.
    // The consumer treats RepoChanged as "something changed somewhere".
    let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
    let mut config_pending: Option<Instant> = None;

    loop {
        // Block for the next signal, then drain and debounce
        let first = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(s) => Some(s),
            Err(std_mpsc::RecvTimeoutError::Timeout) => None,
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
        };

        if let Some(s) = first {
            handle_signal(s, &mut pending, &mut config_pending, &out);
        }

        // Drain any additional pending signals without blocking
        while let Ok(s) = rx.try_recv() {
            handle_signal(s, &mut pending, &mut config_pending, &out);
        }

        let now = Instant::now();

        // Flush entries older than debounce
        let mut to_emit: Vec<PathBuf> = Vec::new();
        pending.retain(|path, when| {
            if now.duration_since(*when) >= debounce {
                to_emit.push(path.clone());
                false
            } else {
                true
            }
        });
        for path in to_emit {
            if out
                .blocking_send(WatchEvent::RepoChanged { root: path })
                .is_err()
            {
                return;
            }
        }

        if let Some(when) = config_pending {
            if now.duration_since(when) >= debounce {
                config_pending = None;
                if out.blocking_send(WatchEvent::ConfigChanged).is_err() {
                    return;
                }
            }
        }
    }
}

fn handle_signal(
    s: RawSignal,
    pending: &mut HashMap<PathBuf, Instant>,
    config_pending: &mut Option<Instant>,
    out: &mpsc::Sender<WatchEvent>,
) {
    match s {
        RawSignal::Path(p) => {
            if is_noise_path(&p) {
                return;
            }
            pending.insert(p, Instant::now());
        }
        RawSignal::Config => {
            *config_pending = Some(Instant::now());
        }
        RawSignal::Error(e) => {
            let _ = out.blocking_send(WatchEvent::WatcherError(e));
        }
    }
}

#[cfg(target_os = "linux")]
fn check_inotify_capacity(repo_count: usize) {
    let max = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok());
    if let Some(max) = max {
        // Conservative estimate: 1000 watches per repo on average
        let est = repo_count * 1000;
        if est * 100 > max * 70 {
            tracing::warn!(
                est_watches = est,
                max_user_watches = max,
                "inotify watch budget may exceed 70% of system limit"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::time::timeout;

    #[tokio::test]
    async fn test_tick_events() {
        let cfg = WatcherConfig {
            repos: vec![],
            config_path: None,
            poll_interval: Duration::from_millis(100),
            debounce: Duration::from_millis(50),
        };
        let (_w, mut rx) = Watcher::start(cfg).expect("start");
        let mut ticks = 0;
        let deadline = Duration::from_millis(500);
        let _ = timeout(deadline, async {
            while let Some(ev) = rx.recv().await {
                if matches!(ev, WatchEvent::Tick) {
                    ticks += 1;
                    if ticks >= 2 {
                        break;
                    }
                }
            }
        })
        .await;
        assert!(ticks >= 2, "expected >=2 ticks, got {}", ticks);
    }

    #[tokio::test]
    async fn test_repo_changed() {
        let dir = tempdir().expect("tempdir");
        let repo_path = dir.path().to_path_buf();
        std::fs::create_dir_all(repo_path.join(".git")).unwrap();

        let cfg = WatcherConfig {
            repos: vec![repo_path.clone()],
            config_path: None,
            poll_interval: Duration::from_secs(60),
            debounce: Duration::from_millis(200),
        };
        let (_w, mut rx) = Watcher::start(cfg).expect("start");

        // Give watcher a moment to register
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::write(repo_path.join("hello.txt"), "world").unwrap();

        let got = timeout(Duration::from_secs(3), async {
            while let Some(ev) = rx.recv().await {
                if matches!(ev, WatchEvent::RepoChanged { .. }) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(got, "expected RepoChanged event");
    }

    #[test]
    fn read_only_noise_ignores_access_metadata_and_directory_modify() {
        use notify::event::{AccessKind, CreateKind, MetadataKind, ModifyKind};
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("HEAD");
        std::fs::write(&file, "x").unwrap();
        let event = |kind, path: &Path| Event::new(kind).add_path(path.to_path_buf());

        assert!(is_read_only_noise(&event(
            EventKind::Access(AccessKind::Any),
            &file
        )));
        assert!(is_read_only_noise(&event(
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)),
            &file
        )));
        assert!(is_read_only_noise(&event(
            EventKind::Modify(ModifyKind::Any),
            dir.path()
        )));

        assert!(!is_read_only_noise(&event(
            EventKind::Modify(ModifyKind::Any),
            &file
        )));
        assert!(!is_read_only_noise(&event(
            EventKind::Create(CreateKind::Any),
            dir.path()
        )));
        // A directory that no longer exists is a real change (e.g. removed worktree).
        assert!(!is_read_only_noise(&event(
            EventKind::Modify(ModifyKind::Any),
            &dir.path().join("gone")
        )));
    }

    #[tokio::test]
    async fn reading_a_repository_does_not_signal_change() {
        let dir = tempdir().expect("tempdir");
        let repo_path = dir.path().to_path_buf();
        std::fs::create_dir_all(repo_path.join(".git/refs")).unwrap();
        std::fs::write(repo_path.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(repo_path.join("README.md"), "x").unwrap();

        let cfg = WatcherConfig {
            repos: vec![repo_path.clone()],
            config_path: None,
            poll_interval: Duration::from_secs(60),
            debounce: Duration::from_millis(200),
        };
        let (_w, mut rx) = Watcher::start(cfg).expect("start");
        tokio::time::sleep(Duration::from_millis(100)).await;
        // What a status scan does: open directories and read files.
        let _ = std::fs::read_dir(&repo_path).unwrap().count();
        let _ = std::fs::read_dir(repo_path.join(".git")).unwrap().count();
        let _ = std::fs::read(repo_path.join(".git/HEAD")).unwrap();
        let _ = std::fs::read(repo_path.join("README.md")).unwrap();

        let got = timeout(Duration::from_millis(600), async {
            while let Some(ev) = rx.recv().await {
                if matches!(ev, WatchEvent::RepoChanged { .. }) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(!got, "read-only access must not signal a change");
    }

    #[tokio::test]
    async fn test_objects_filtered() {
        let dir = tempdir().expect("tempdir");
        let repo_path = dir.path().to_path_buf();
        std::fs::create_dir_all(repo_path.join(".git/objects/aa")).unwrap();

        let cfg = WatcherConfig {
            repos: vec![repo_path.clone()],
            config_path: None,
            poll_interval: Duration::from_secs(60),
            debounce: Duration::from_millis(200),
        };
        let (_w, mut rx) = Watcher::start(cfg).expect("start");
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::write(repo_path.join(".git/objects/aa/bb"), "x").unwrap();

        // Wait for any RepoChanged event for 600ms
        let got = timeout(Duration::from_millis(600), async {
            while let Some(ev) = rx.recv().await {
                if matches!(ev, WatchEvent::RepoChanged { .. }) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(!got, "objects path must be filtered");
    }

    #[tokio::test]
    async fn test_config_changed() {
        let dir = tempdir().expect("tempdir");
        let cfg_path = dir.path().join("config.toml");
        std::fs::write(&cfg_path, "[general]\n").unwrap();

        let cfg = WatcherConfig {
            repos: vec![],
            config_path: Some(cfg_path.clone()),
            poll_interval: Duration::from_secs(60),
            debounce: Duration::from_millis(200),
        };
        let (_w, mut rx) = Watcher::start(cfg).expect("start");
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::write(&cfg_path, "[general]\nrefresh_interval_secs = 60\n").unwrap();

        let got = timeout(Duration::from_secs(3), async {
            while let Some(ev) = rx.recv().await {
                if matches!(ev, WatchEvent::ConfigChanged) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(got, "expected ConfigChanged event");
    }

    #[tokio::test]
    async fn first_config_save_is_observed_when_directory_is_absent() {
        let dir = tempdir().expect("tempdir");
        let cfg_path = dir.path().join("new-profile/git-clawler/config.toml");
        let cfg = WatcherConfig {
            repos: vec![],
            config_path: Some(cfg_path.clone()),
            poll_interval: Duration::from_secs(60),
            debounce: Duration::from_millis(50),
        };
        let (_watcher, mut rx) = Watcher::start(cfg).expect("start");
        assert!(cfg_path.parent().unwrap().is_dir());
        std::fs::write(cfg_path, "[general]\nrefresh_interval_secs = 60\n").unwrap();
        let changed = timeout(Duration::from_secs(3), async {
            while let Some(event) = rx.recv().await {
                if matches!(event, WatchEvent::ConfigChanged) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(changed, "the first config save must trigger a reload");
    }

    #[test]
    fn is_noise_path_filters_objects() {
        assert!(is_noise_path(Path::new("/r/.git/objects/aa/bb")));
        assert!(is_noise_path(Path::new("/r/target/debug/x")));
        assert!(is_noise_path(Path::new("/r/node_modules/foo")));
    }

    #[test]
    fn is_noise_path_allows_normal() {
        assert!(!is_noise_path(Path::new("/r/src/main.rs")));
        assert!(!is_noise_path(Path::new("/r/.git/HEAD")));
    }

    #[tokio::test]
    async fn test_reload_changes_poll_interval() {
        let cfg = WatcherConfig {
            repos: vec![],
            config_path: None,
            poll_interval: Duration::from_secs(10),
            debounce: Duration::from_millis(50),
        };
        let (mut w, mut rx) = Watcher::start(cfg).expect("start");

        let new_cfg = WatcherConfig {
            repos: vec![],
            config_path: None,
            poll_interval: Duration::from_millis(100),
            debounce: Duration::from_millis(50),
        };
        w.reload(new_cfg).expect("reload");

        let mut ticks = 0;
        let _ = timeout(Duration::from_millis(500), async {
            while let Some(ev) = rx.recv().await {
                if matches!(ev, WatchEvent::Tick) {
                    ticks += 1;
                    if ticks >= 2 {
                        break;
                    }
                }
            }
        })
        .await;
        assert!(ticks >= 2, "expected >=2 ticks after reload, got {}", ticks);
    }

    #[tokio::test]
    async fn test_reload_changes_watch_set() {
        let dir = tempdir().expect("tempdir");
        let dir_a = dir.path().join("a");
        let dir_b = dir.path().join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();

        let cfg = WatcherConfig {
            repos: vec![dir_a.clone()],
            config_path: None,
            poll_interval: Duration::from_secs(60),
            debounce: Duration::from_millis(150),
        };
        let (mut w, mut rx) = Watcher::start(cfg).expect("start");
        tokio::time::sleep(Duration::from_millis(100)).await;

        let new_cfg = WatcherConfig {
            repos: vec![dir_b.clone()],
            config_path: None,
            poll_interval: Duration::from_secs(60),
            debounce: Duration::from_millis(150),
        };
        w.reload(new_cfg).expect("reload");
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Drain anything left over from the original registration.
        while rx.try_recv().is_ok() {}

        // Edit dir_a -> should NOT produce RepoChanged
        std::fs::write(dir_a.join("hello.txt"), "world").unwrap();
        let saw_a = timeout(Duration::from_millis(600), async {
            while let Some(ev) = rx.recv().await {
                if matches!(ev, WatchEvent::RepoChanged { .. }) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(!saw_a, "edits to old watch set must not produce events");

        // Edit dir_b -> SHOULD produce RepoChanged
        std::fs::write(dir_b.join("hello.txt"), "world").unwrap();
        let saw_b = timeout(Duration::from_secs(3), async {
            while let Some(ev) = rx.recv().await {
                if matches!(ev, WatchEvent::RepoChanged { .. }) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(saw_b, "edits to new watch set must produce events");
    }

    #[tokio::test]
    async fn test_reload_preserves_event_receiver() {
        let cfg = WatcherConfig {
            repos: vec![],
            config_path: None,
            poll_interval: Duration::from_secs(60),
            debounce: Duration::from_millis(50),
        };
        let (mut w, mut rx) = Watcher::start(cfg).expect("start");

        let new_cfg = WatcherConfig {
            repos: vec![],
            config_path: None,
            poll_interval: Duration::from_millis(100),
            debounce: Duration::from_millis(50),
        };
        w.reload(new_cfg).expect("reload");

        let got = timeout(Duration::from_millis(500), async {
            while let Some(ev) = rx.recv().await {
                if matches!(ev, WatchEvent::Tick) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(got, "rx must still receive events after reload");
    }

    /// Dropping `Watcher` must abort background tasks and release all
    /// senders so the corresponding receiver eventually sees `None`.
    #[tokio::test]
    async fn test_drop_closes_event_channel() {
        let cfg = WatcherConfig {
            repos: vec![],
            config_path: None,
            // Large interval to avoid queued Tick events racing the close.
            poll_interval: Duration::from_secs(60),
            debounce: Duration::from_millis(50),
        };
        let (w, mut rx) = Watcher::start(cfg).expect("start");

        drop(w);

        // After drop:
        //   - tick task is aborted (its out_tx clone dropped)
        //   - raw_tx (field of Self) dropped → debounce loop exits within
        //     ~100ms (recv_timeout) → its out_tx clone dropped
        //   - Self.out_tx dropped
        // → all senders gone, rx.recv() returns None.
        let result = timeout(Duration::from_secs(1), rx.recv()).await;
        match result {
            Ok(None) => {} // expected: channel closed
            Ok(Some(ev)) => panic!("expected channel to close, got event: {ev:?}"),
            Err(_) => panic!("rx.recv() did not return within 1s after drop"),
        }
    }
}
