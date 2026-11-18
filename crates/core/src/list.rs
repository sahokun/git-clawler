//! Shared scan helpers used by both `list` and `watch`.
//!
//! [`build_targets`] converts an [`AppConfig`] into an ordered list of
//! [`ListTarget`]s, marking the entry whose path contains the current working
//! directory.
//!
//! [`scan_all`] runs `core::scan()` for each target in parallel (bounded by a
//! semaphore to `min(available_parallelism, 8)`) and returns [`ListRow`]s
//! ordered by [`ListTarget::index`] ascending.  Callers may pass **any
//! subset** of targets; indices need not be contiguous or zero-based (e.g.
//! the GUI may partition native vs WSL targets and pass each subset
//! independently).  A per-repo scan failure produces a [`ScanOutcome::Err`]
//! row rather than aborting the whole batch.
//!
//! [`enrich_with_github`] enriches rows in-place with GitHub Issue/PR counts
//! using a few batched GraphQL requests (at most [`GITHUB_CONCURRENCY`] = 4
//! in flight), one per group of repositories rather than one per row.  WSL-side
//! `remote_url` is pre-populated by the GUI before calling this function so
//! that `gh api` only needs to run on the Windows host.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Maximum number of batched GitHub requests in flight during enrichment.
///
/// Exposed as a `pub const` so a follow-up can wire it through `[github].concurrency`
/// config without further refactoring.
pub const GITHUB_CONCURRENCY: usize = 4;

#[derive(Debug, Clone)]
pub struct ListTarget {
    pub index: usize,
    pub name: String,
    pub path: PathBuf,
    pub is_current: bool,
    pub distro: Option<String>,
    /// Whether to run `git fetch` before scanning this target. Resolved at
    /// `build_targets` time from the per-repo override (if any) and the
    /// global `general.fetch_on_refresh` default.
    pub fetch: bool,
}

pub enum ScanOutcome {
    Ok(crate::RepoStatus),
    Err(String),
}

pub struct ListRow {
    pub target: ListTarget,
    pub scanned_at: DateTime<Utc>,
    pub outcome: ScanOutcome,
    /// Pre-populated origin remote URL used by the GitHub enrichment step.
    ///
    /// Set by the GUI/CLI before calling [`enrich_with_github`]:
    /// - **Native repos**: left as `None`; enrichment falls back to
    ///   `git remote get-url origin` via the path.
    /// - **WSL repos**: set from the helper's `RepoStatusDto.remote_url` field
    ///   (resolved inside WSL via git2) so that `gh api` can run on the
    ///   Windows host without needing `gh` inside every distro.
    pub remote_url: Option<String>,
}

/// Remove, from each main repository row, unpushed branches that one of its
/// linked worktree rows already reports, so a branch checked out in a
/// worktree is counted once (on the worktree row).
pub fn dedupe_worktree_branches(rows: &mut [ListRow]) {
    use crate::path_key::DistroPathKey;
    use std::collections::{HashMap, HashSet};
    let mut by_parent: HashMap<DistroPathKey, HashSet<String>> = HashMap::new();
    for row in rows.iter() {
        if let ScanOutcome::Ok(status) = &row.outcome {
            if let Some(parent) = &status.worktree_parent {
                let key = DistroPathKey::current_host(row.target.distro.as_deref(), parent);
                by_parent
                    .entry(key)
                    .or_default()
                    .extend(status.unpushed_branches.iter().map(|b| b.name.clone()));
            }
        }
    }
    if by_parent.is_empty() {
        return;
    }
    for row in rows.iter_mut() {
        let key = DistroPathKey::current_host(row.target.distro.as_deref(), &row.target.path);
        if let (ScanOutcome::Ok(status), Some(names)) = (&mut row.outcome, by_parent.get(&key)) {
            if status.worktree_parent.is_none() {
                status
                    .unpushed_branches
                    .retain(|b| !names.contains(&b.name));
            }
        }
    }
}

/// Build [`ListTarget`] entries from an [`AppConfig`], marking the current
/// directory's enclosing repository when applicable.
pub fn build_targets(config: &crate::AppConfig) -> Vec<ListTarget> {
    let general_fetch = config.general.fetch_on_refresh;
    let registry = crate::Registry::from_config(config);
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.canonicalize().ok().or(Some(p)));
    let current_index = cwd
        .as_deref()
        .and_then(|d| find_current_repo(d, registry.list()));

    registry
        .list()
        .iter()
        .enumerate()
        .map(|(i, r)| ListTarget {
            index: i,
            name: r.name.clone(),
            path: r.path().to_path_buf(),
            is_current: Some(i) == current_index,
            distro: r.distro.clone(),
            fetch: r.fetch.unwrap_or(general_fetch),
        })
        .collect()
}

pub fn find_current_repo(cwd: &Path, repos: &[crate::Repository]) -> Option<usize> {
    let cwd_canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    repos
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let rp = r
                .path()
                .canonicalize()
                .unwrap_or_else(|_| r.path().to_path_buf());
            if cwd_canonical == rp || cwd_canonical.starts_with(&rp) {
                Some((i, rp.components().count()))
            } else {
                None
            }
        })
        .max_by_key(|(_, depth)| *depth)
        .map(|(i, _)| i)
}

fn build_join_err_row(target: ListTarget, err: &tokio::task::JoinError) -> (usize, ListRow) {
    let idx = target.index;
    let msg = if err.is_cancelled() {
        "scan task cancelled".to_string()
    } else if err.is_panic() {
        "scan task panicked".to_string()
    } else {
        format!("scan task join error: {err}")
    };

    (
        idx,
        ListRow {
            target,
            scanned_at: Utc::now(),
            outcome: ScanOutcome::Err(msg),
            remote_url: None,
        },
    )
}

struct CancelScans(Arc<std::sync::atomic::AtomicBool>);
impl Drop for CancelScans {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

pub async fn scan_all(targets: Vec<ListTarget>) -> Vec<ListRow> {
    // Dropping this future cancels in-flight blocking fetches as well as tasks
    // waiting for a concurrency permit. A timeout on the future alone cannot.
    let cancellation = CancelScans(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let parallelism = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    let sem = Arc::new(Semaphore::new(parallelism));
    let mut handles: Vec<(ListTarget, tokio::task::JoinHandle<(usize, ListRow)>)> =
        Vec::with_capacity(targets.len());

    for t in targets {
        let sem = sem.clone();
        let cancelled = cancellation.0.clone();
        let t_outer = t.clone();
        handles.push((
            t_outer,
            tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore closed");
                let path = t.path.clone();
                let name = t.name.clone();
                let opts = crate::status::StatusScanOptions { fetch: t.fetch };
                let outcome = tokio::task::spawn_blocking(move || {
                    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                        return Err(crate::status::StatusError::Git {
                            context: "cancelled".into(),
                            source: git2::Error::from_str("scan cancelled"),
                        });
                    }
                    crate::status::scan_cancellable(&path, opts, &cancelled)
                })
                .await;
                let scanned_at = Utc::now();
                let outcome = match outcome {
                    Ok(Ok(status)) => ScanOutcome::Ok(status),
                    Ok(Err(e)) => {
                        tracing::warn!(repo = %name, error = %e, "scan failed");
                        ScanOutcome::Err(e.to_string())
                    }
                    Err(e) => {
                        tracing::warn!(repo = %name, error = %e, "join error");
                        ScanOutcome::Err("task panicked".into())
                    }
                };
                let idx = t.index;
                (
                    idx,
                    ListRow {
                        target: t,
                        scanned_at,
                        outcome,
                        remote_url: None,
                    },
                )
            }),
        ));
    }

    // Collect (index, row) pairs from completed tasks.  Indices may be sparse
    // and non-contiguous (e.g. when the GUI passes a native-only or WSL-only
    // subset after partitioning).  Using a direct-index buffer would panic for
    // any index >= subset length, so we accumulate into a Vec and sort.
    let mut pairs: Vec<(usize, ListRow)> = Vec::with_capacity(handles.len());
    for (target, h) in handles {
        match h.await {
            Ok((idx, row)) => pairs.push((idx, row)),
            Err(e) => {
                tracing::warn!(repo = %target.name, error = %e, "outer join error");
                pairs.push(build_join_err_row(target, &e));
            }
        }
    }
    pairs.sort_by_key(|(idx, _)| *idx);
    pairs.into_iter().map(|(_, row)| row).collect()
}

pub async fn scan_all_with_github(
    targets: Vec<ListTarget>,
    github_cfg: &crate::config::GithubConfig,
) -> Vec<ListRow> {
    let mut rows = scan_all(targets).await;
    enrich_with_github(&mut rows, github_cfg).await;
    rows
}

/// Type alias for the boxed batch fetch-function used by [`do_enrich`].
///
/// Takes a chunk of `(owner, name)` slugs and a clone of the GitHub config
/// and returns one result per slug, in order. The future owns its data so it
/// is `Send + 'static` and can be spawned into a [`tokio::task::JoinSet`].
pub(crate) type FetchFn = dyn Fn(
        Vec<crate::github::RepoSlug>,
        crate::config::GithubConfig,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Vec<Option<crate::GithubInfo>>> + Send + 'static>,
    > + Send
    + Sync;

/// Inner implementation of GitHub enrichment; parameterised over the fetch
/// function to allow injection of a mock in unit tests.
///
/// Callers must have already ensured `cfg.enabled == true` before calling
/// this function (the public [`enrich_with_github`] handles the guard).
pub(crate) async fn do_enrich(
    rows: &mut [ListRow],
    cfg: &crate::config::GithubConfig,
    fetch_fn: &FetchFn,
) {
    // ── Step 1: Resolve each row's GitHub slug ─────────────────────────────
    // Prefer `row.remote_url` (pre-populated from WSL helper or CLI); fall
    // back to git2 (no subprocess) via spawn_blocking for native rows only.
    let mut row_slugs: Vec<Option<crate::github::RepoSlug>> = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let url = match &row.outcome {
            ScanOutcome::Ok(_) => {
                if let Some(u) = row.remote_url.clone() {
                    Some(u)
                } else {
                    let path = row.target.path.clone();
                    match tokio::task::spawn_blocking(move || {
                        crate::github::get_origin_url_sync(&path)
                    })
                    .await
                    {
                        Ok(url) => url,
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "spawn_blocking for origin URL lookup failed; skipping GitHub enrichment for this repo"
                            );
                            None
                        }
                    }
                }
            }
            ScanOutcome::Err(_) => None,
        };
        row_slugs.push(url.as_deref().and_then(crate::github::parse_owner_repo));
    }

    // ── Step 2: Unique slugs (worktrees share their main repository's remote)
    let mut unique: Vec<crate::github::RepoSlug> = Vec::new();
    let mut slot: HashMap<crate::github::RepoSlug, usize> = HashMap::new();
    for slug in row_slugs.iter().flatten() {
        if !slot.contains_key(slug) {
            slot.insert(slug.clone(), unique.len());
            unique.push(slug.clone());
        }
    }
    if unique.is_empty() {
        return;
    }

    // ── Step 3: Batched GraphQL requests ───────────────────────────────────
    // One request carries many repositories; split into up to
    // GITHUB_CONCURRENCY parallel requests (measured ~4x faster than one
    // process per repository), each at most GITHUB_BATCH_MAX repositories.
    let chunk_size = unique
        .len()
        .div_ceil(GITHUB_CONCURRENCY)
        .clamp(1, crate::github::GITHUB_BATCH_MAX);
    let sem = Arc::new(Semaphore::new(GITHUB_CONCURRENCY));
    let mut join_set: JoinSet<(usize, Vec<Option<crate::GithubInfo>>)> = JoinSet::new();
    for (chunk_index, chunk) in unique.chunks(chunk_size).enumerate() {
        let sem_clone = sem.clone();
        let fut = fetch_fn(chunk.to_vec(), cfg.clone());
        join_set.spawn(async move {
            let _permit = sem_clone.acquire_owned().await.expect("semaphore closed");
            (chunk_index * chunk_size, fut.await)
        });
    }

    // ── Step 4: Collect results by unique-slug index ───────────────────────
    let mut infos: Vec<Option<crate::GithubInfo>> = vec![None; unique.len()];
    while let Some(task_result) = join_set.join_next().await {
        match task_result {
            Ok((offset, chunk_infos)) => {
                for (i, info) in chunk_infos.into_iter().enumerate() {
                    if let Some(target) = infos.get_mut(offset + i) {
                        *target = info;
                    }
                }
            }
            Err(e) => tracing::debug!(error = %e, "github enrichment task panicked"),
        }
    }

    // ── Step 5: Write back in a single synchronous pass ────────────────────
    let (mut enriched, mut total) = (0usize, 0usize);
    for (row, slug) in rows.iter_mut().zip(row_slugs) {
        let ScanOutcome::Ok(status) = &mut row.outcome else {
            continue;
        };
        let Some(slug) = slug else { continue };
        total += 1;
        if let Some(info) = infos[slot[&slug]].clone() {
            status.github = Some(info);
            enriched += 1;
        }
    }
    tracing::info!(
        enriched,
        total,
        requests = unique.len().div_ceil(chunk_size),
        "GitHub enrichment complete"
    );
}

/// Enrich [`ListRow`]s with GitHub Issue/PR counts using batched requests.
///
/// No-op when `cfg.enabled` is `false`. Per-repo errors (including non-GitHub
/// remotes) degrade gracefully to `status.github = None`.
///
/// Repositories are deduplicated by `owner/name` and looked up in batched
/// GraphQL requests, at most [`GITHUB_CONCURRENCY`] (= 4) at a time.  The
/// `remote_url` field on each row is used when present (WSL repos); otherwise
/// the origin URL is resolved from the repo path via `git remote get-url
/// origin`.
pub async fn enrich_with_github(rows: &mut [ListRow], cfg: &crate::config::GithubConfig) {
    if !cfg.enabled {
        return;
    }
    do_enrich(rows, cfg, &|slugs, config| {
        Box::pin(async move { crate::github::fetch_infos_batch(&slugs, &config).await })
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GithubConfig;
    use crate::{GithubInfo, Repository};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn make_repo(path: &str) -> Repository {
        Repository::new(path.rsplit('/').next().unwrap_or("repo"), path)
    }

    fn make_ok_row(index: usize, path: &str, remote_url: Option<String>) -> ListRow {
        ListRow {
            target: ListTarget {
                index,
                name: path.rsplit('/').next().unwrap_or("repo").to_string(),
                path: PathBuf::from(path),
                is_current: false,
                distro: None,
                fetch: false,
            },
            scanned_at: Utc::now(),
            outcome: ScanOutcome::Ok(crate::RepoStatus::default()),
            remote_url,
        }
    }

    fn make_err_row(index: usize, path: &str) -> ListRow {
        ListRow {
            target: ListTarget {
                index,
                name: path.rsplit('/').next().unwrap_or("repo").to_string(),
                path: PathBuf::from(path),
                is_current: false,
                distro: None,
                fetch: false,
            },
            scanned_at: Utc::now(),
            outcome: ScanOutcome::Err("scan failed".into()),
            remote_url: None,
        }
    }

    // ── enrich_with_github tests ─────────────────────────────────────────

    #[tokio::test]
    async fn enrich_disabled_is_noop() {
        let cfg = GithubConfig {
            enabled: false,
            ..Default::default()
        };
        let mut rows = vec![make_ok_row(
            0,
            "/a",
            Some("https://github.com/owner/repo".into()),
        )];
        enrich_with_github(&mut rows, &cfg).await;
        if let ScanOutcome::Ok(status) = &rows[0].outcome {
            assert!(
                status.github.is_none(),
                "github must remain None when disabled"
            );
        }
    }

    fn info(issues: u64) -> GithubInfo {
        GithubInfo {
            open_issues: Some(issues),
            open_prs: Some(0),
        }
    }

    fn issues_of(row: &ListRow) -> Option<u64> {
        match &row.outcome {
            ScanOutcome::Ok(status) => status.github.as_ref().and_then(|g| g.open_issues),
            ScanOutcome::Err(_) => None,
        }
    }

    fn enabled() -> GithubConfig {
        GithubConfig {
            enabled: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn enrich_skips_err_rows_and_non_github_remotes() {
        let called = Arc::new(AtomicUsize::new(0));
        let called_c = called.clone();
        let mut rows = vec![
            make_err_row(0, "/a"),
            make_ok_row(1, "/b", Some("https://gitlab.com/owner/repo".into())),
        ];
        do_enrich(&mut rows, &enabled(), &move |slugs, _cfg| {
            called_c.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { vec![None; slugs.len()] })
        })
        .await;
        assert_eq!(called.load(Ordering::SeqCst), 0, "nothing to look up");
    }

    #[tokio::test]
    async fn enrich_uses_remote_url_override_when_present() {
        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_c = received.clone();
        let mut rows = vec![make_ok_row(
            0,
            "/nonexistent/path",
            Some("https://github.com/owner/repo".into()),
        )];
        do_enrich(&mut rows, &enabled(), &move |slugs, _cfg| {
            received_c.lock().unwrap().extend(slugs.clone());
            Box::pin(async move {
                vec![Some(GithubInfo {
                    open_issues: Some(5),
                    open_prs: Some(2),
                })]
            })
        })
        .await;
        // The pre-populated remote_url was used (path fallback would fail for /nonexistent)
        assert_eq!(
            *received.lock().unwrap(),
            vec![("owner".to_string(), "repo".to_string())]
        );
        let ScanOutcome::Ok(status) = &rows[0].outcome else {
            panic!("expected Ok row");
        };
        assert_eq!(status.github.as_ref().unwrap().open_issues, Some(5));
        assert_eq!(status.github.as_ref().unwrap().open_prs, Some(2));
    }

    #[tokio::test]
    async fn enrich_degrades_to_none_per_repository() {
        let mut rows = vec![
            make_ok_row(0, "/a", Some("https://github.com/owner/a".into())),
            make_ok_row(1, "/b", Some("https://github.com/owner/gone".into())),
        ];
        do_enrich(&mut rows, &enabled(), &|slugs, _cfg| {
            Box::pin(async move {
                slugs
                    .iter()
                    .map(|(_, name)| (name != "gone").then(|| info(1)))
                    .collect()
            })
        })
        .await;
        assert_eq!(issues_of(&rows[0]), Some(1));
        assert_eq!(issues_of(&rows[1]), None);
    }

    #[tokio::test]
    async fn enrich_short_batch_result_leaves_rest_none() {
        let mut rows = vec![
            make_ok_row(0, "/a", Some("https://github.com/owner/a".into())),
            make_ok_row(1, "/b", Some("https://github.com/owner/b".into())),
        ];
        do_enrich(&mut rows, &enabled(), &|_slugs, _cfg| {
            Box::pin(async { vec![] })
        })
        .await;
        assert_eq!(issues_of(&rows[0]), None);
        assert_eq!(issues_of(&rows[1]), None);
    }

    #[tokio::test]
    async fn enrich_preserves_row_order_across_chunks() {
        // 10 distinct repositories → 4 chunks (3/3/3/1); each repo's issue
        // count encodes its number so misassignment is detectable.
        let mut rows: Vec<ListRow> = (0..10)
            .map(|i| {
                make_ok_row(
                    i,
                    &format!("/repo{i}"),
                    Some(format!("https://github.com/owner/repo{i}")),
                )
            })
            .collect();
        let chunks = Arc::new(std::sync::Mutex::new(Vec::new()));
        let chunks_c = chunks.clone();
        do_enrich(&mut rows, &enabled(), &move |slugs, _cfg| {
            chunks_c.lock().unwrap().push(slugs.len());
            Box::pin(async move {
                slugs
                    .iter()
                    .map(|(_, name)| Some(info(name.trim_start_matches("repo").parse().unwrap())))
                    .collect()
            })
        })
        .await;
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(issues_of(row), Some(i as u64), "row {i}");
        }
        let mut sizes = chunks.lock().unwrap().clone();
        sizes.sort_unstable();
        assert_eq!(sizes, vec![1, 3, 3, 3]);
    }

    #[tokio::test]
    async fn enrich_looks_up_a_shared_remote_once() {
        // A linked worktree shares its main repository's remote.
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_c = seen.clone();
        let mut rows = vec![
            make_ok_row(0, "/app", Some("git@github.com:owner/app.git".into())),
            make_ok_row(1, "/app-wt", Some("https://github.com/owner/app".into())),
        ];
        do_enrich(&mut rows, &enabled(), &move |slugs, _cfg| {
            seen_c.lock().unwrap().extend(slugs.clone());
            Box::pin(async move { vec![Some(info(7)); slugs.len()] })
        })
        .await;
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(issues_of(&rows[0]), Some(7));
        assert_eq!(issues_of(&rows[1]), Some(7));
    }

    #[tokio::test]
    async fn enrich_bounded_concurrency_holds_at_4() {
        // More repositories than GITHUB_CONCURRENCY × GITHUB_BATCH_MAX so
        // there are more chunks than permits.
        let count = GITHUB_CONCURRENCY * crate::github::GITHUB_BATCH_MAX + 7;
        let mut rows: Vec<ListRow> = (0..count)
            .map(|i| {
                make_ok_row(
                    i,
                    &format!("/repo{i}"),
                    Some(format!("https://github.com/owner/repo{i}")),
                )
            })
            .collect();

        let current = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let (current_c, peak_c, calls_c) = (current.clone(), peak.clone(), calls.clone());

        do_enrich(&mut rows, &enabled(), &move |slugs, _cfg| {
            assert!(slugs.len() <= crate::github::GITHUB_BATCH_MAX);
            calls_c.fetch_add(1, Ordering::SeqCst);
            let cur = current_c.clone();
            let pk = peak_c.clone();
            Box::pin(async move {
                let n = cur.fetch_add(1, Ordering::SeqCst) + 1;
                pk.fetch_max(n, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                cur.fetch_sub(1, Ordering::SeqCst);
                vec![None; slugs.len()]
            })
        })
        .await;

        let observed = peak.load(Ordering::SeqCst);
        assert!(
            observed <= GITHUB_CONCURRENCY,
            "peak concurrency {observed} exceeded cap {GITHUB_CONCURRENCY}"
        );
        assert!(observed > 0, "no concurrent executions observed");
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    // ── find_current_repo / build_targets tests (preserved) ─────────────

    #[test]
    fn find_current_repo_exact_match() {
        let repos = vec![make_repo("/home/user/project")];
        let cwd = PathBuf::from("/home/user/project");
        assert_eq!(find_current_repo(&cwd, &repos), Some(0));
    }

    #[test]
    fn find_current_repo_subdirectory() {
        let repos = vec![make_repo("/home/user/project")];
        let cwd = PathBuf::from("/home/user/project/src/main");
        assert_eq!(find_current_repo(&cwd, &repos), Some(0));
    }

    #[test]
    fn find_current_repo_no_match() {
        let repos = vec![make_repo("/home/user/project")];
        let cwd = PathBuf::from("/home/other/stuff");
        assert_eq!(find_current_repo(&cwd, &repos), None);
    }

    #[test]
    fn find_current_repo_nested_prefers_deepest() {
        let repos = vec![make_repo("/a"), make_repo("/a/b")];
        let cwd = PathBuf::from("/a/b/sub");
        assert_eq!(find_current_repo(&cwd, &repos), Some(1));
    }

    #[test]
    #[cfg(unix)]
    fn find_current_repo_symlink_duplicates_returns_single_index() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real-repo");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("alias-repo");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let repos = vec![
            crate::Repository::new("real", real.clone()),
            crate::Repository::new("alias", link),
        ];

        let chosen = find_current_repo(&real, &repos).expect("must find a repo");
        assert!(chosen < repos.len());

        let current_index = Some(chosen);
        let current_count = repos
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) == current_index)
            .count();
        assert_eq!(
            current_count, 1,
            "exactly one registry entry must be marked current"
        );
    }

    #[test]
    fn build_targets_empty_config() {
        let config = crate::AppConfig::default();
        let targets = build_targets(&config);
        assert!(targets.is_empty());
    }

    #[test]
    fn build_targets_assigns_sequential_indices() {
        let config = crate::AppConfig {
            repositories: vec![
                crate::config::RepoEntry {
                    worktree_of: None,
                    name: Some("a".into()),
                    path: PathBuf::from("/tmp/a"),
                    fetch: None,
                    distro: None,
                },
                crate::config::RepoEntry {
                    worktree_of: None,
                    name: Some("b".into()),
                    path: PathBuf::from("/tmp/b"),
                    fetch: None,
                    distro: None,
                },
            ],
            ..Default::default()
        };
        let targets = build_targets(&config);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].index, 0);
        assert_eq!(targets[1].index, 1);
        assert_eq!(targets[0].name, "a");
        assert_eq!(targets[1].name, "b");
    }

    #[test]
    fn build_targets_propagates_distro() {
        let config = crate::AppConfig {
            repositories: vec![crate::config::RepoEntry {
                worktree_of: None,
                name: Some("wsl-repo".into()),
                path: PathBuf::from("/home/user/proj"),
                fetch: None,
                distro: Some("Ubuntu".into()),
            }],
            ..Default::default()
        };
        let targets = build_targets(&config);
        assert_eq!(targets[0].distro, Some("Ubuntu".into()));
    }

    // ── scan_all sparse-index tests ─────────────────────────────────────────

    fn make_target(index: usize, path: &std::path::Path) -> ListTarget {
        ListTarget {
            index,
            name: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("repo")
                .to_string(),
            path: path.to_path_buf(),
            is_current: false,
            distro: None,
            fetch: false,
        }
    }

    /// scan_all must not panic when target indices are sparse / non-contiguous
    /// (e.g. GUI passes the WSL subset after partitioning native targets away).
    /// Output must be ordered by target.index ascending.
    #[tokio::test]
    async fn scan_all_with_sparse_indices() {
        // Three real git repos; assign out-of-order indices 2, 5, 0.
        let d0 = tempfile::tempdir().unwrap();
        let d2 = tempfile::tempdir().unwrap();
        let d5 = tempfile::tempdir().unwrap();
        git2::Repository::init(d0.path()).unwrap();
        git2::Repository::init(d2.path()).unwrap();
        git2::Repository::init(d5.path()).unwrap();

        let targets = vec![
            make_target(2, d2.path()),
            make_target(5, d5.path()),
            make_target(0, d0.path()),
        ];

        let rows = scan_all(targets).await;

        assert_eq!(rows.len(), 3, "all three repos must be returned");
        // Output must be ordered by index ascending: 0, 2, 5
        assert_eq!(rows[0].target.index, 0);
        assert_eq!(rows[1].target.index, 2);
        assert_eq!(rows[2].target.index, 5);
    }

    /// A single target with a very high index must not panic.
    #[tokio::test]
    async fn scan_all_with_single_high_index() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();

        let targets = vec![make_target(42, dir.path())];
        let rows = scan_all(targets).await;

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.index, 42);
    }

    /// scan_all on an empty subset must return empty vec without panicking.
    #[tokio::test]
    async fn scan_all_empty_subset_returns_empty() {
        let rows = scan_all(vec![]).await;
        assert!(rows.is_empty());
    }

    /// `build_join_err_row` must produce a row with `ScanOutcome::Err` containing
    /// "panicked" when constructed from a panic `JoinError`.
    #[tokio::test]
    async fn build_join_err_row_from_panic_yields_err_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let target = make_target(7, dir.path());
        let target_clone = target.clone();

        let h = tokio::spawn(async {
            panic!("intentional panic for test");
            #[allow(unreachable_code)]
            (0usize, ())
        });
        let join_err = h.await.unwrap_err();
        assert!(join_err.is_panic());

        let (idx, row) = build_join_err_row(target_clone, &join_err);
        assert_eq!(idx, 7);
        assert_eq!(row.target.index, 7);
        match row.outcome {
            ScanOutcome::Err(msg) => {
                assert!(msg.contains("panicked"), "expected 'panicked' in '{msg}'")
            }
            ScanOutcome::Ok(_) => panic!("expected Err outcome"),
        }
    }

    /// `build_join_err_row` must produce a row with `ScanOutcome::Err` containing
    /// "cancelled" when constructed from a cancelled task `JoinError`.
    #[tokio::test]
    async fn build_join_err_row_from_cancel_yields_err_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let target = make_target(3, dir.path());
        let target_clone = target.clone();

        let h = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        });
        h.abort();
        let join_err = h.await.unwrap_err();
        assert!(join_err.is_cancelled());

        let (idx, row) = build_join_err_row(target_clone, &join_err);
        assert_eq!(idx, 3);
        assert_eq!(row.target.index, 3);
        match row.outcome {
            ScanOutcome::Err(msg) => {
                assert!(msg.contains("cancelled"), "expected 'cancelled' in '{msg}'")
            }
            ScanOutcome::Ok(_) => panic!("expected Err outcome"),
        }
    }
}
