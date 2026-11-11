use crate::protocol::{
    ErrorCode, HelperError, PathResult, PathStatus, RepoStatusDto, ScanResponse, ScanRootResponse,
    ScanRootStatus,
};
use crate::{HELPER_VERSION, PROTOCOL_VERSION};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Scan absolute repository paths and return a fail-soft protocol response.
///
/// Inputs must be absolute paths; relative paths are reported as
/// [`ErrorCode::RelativePath`] without aborting other scans. Concurrency is capped at
/// `available_parallelism().min(8)` using a [`Semaphore`]. Results carry `index`, so
/// callers can reconstruct the input order even if implementation details change.
///
/// When `fetch` is `true`, each repository's current-branch upstream is fetched
/// (best-effort) before its status is computed. Fetch failures are logged and
/// the scan proceeds with the existing remote-tracking cache.
pub async fn run_scan(paths: Vec<PathBuf>, fetch: bool) -> ScanResponse {
    let parallelism = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    let sem = Arc::new(Semaphore::new(parallelism));
    let count = paths.len();
    let mut handles = Vec::with_capacity(count);

    for (index, path) in paths.into_iter().enumerate() {
        let sem = sem.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore closed");
            let path_str = path.to_string_lossy().into_owned();
            let opts = git_clawler_core::status::StatusScanOptions { fetch };
            let result = tokio::task::spawn_blocking(move || {
                let scan = git_clawler_core::status::scan_with_options(&path, opts);
                let remote_url = scan
                    .as_ref()
                    .ok()
                    .and_then(|_| git_clawler_core::github::get_origin_url_sync(&path));
                (scan, remote_url)
            })
            .await;

            let status = match result {
                Ok((Ok(repo_status), remote_url)) => {
                    let mut dto = RepoStatusDto::from(repo_status);
                    // Best-effort: populated inside spawn_blocking using git2
                    // (no subprocess, no `gh` required inside WSL).
                    dto.remote_url = remote_url;
                    PathStatus::Ok(dto)
                }
                Ok((Err(e), _)) => {
                    tracing::warn!(path = %path_str, error = %e, "scan failed");
                    PathStatus::Err(HelperError::from(&e))
                }
                Err(join_err) => {
                    tracing::warn!(path = %path_str, error = %join_err, "task panicked");
                    PathStatus::Err(HelperError {
                        code: ErrorCode::Panic,
                        message: format!("task panicked: {join_err}"),
                    })
                }
            };

            (index, path_str, status)
        }));
    }

    let mut buf: Vec<Option<(usize, String, PathStatus)>> = (0..count).map(|_| None).collect();
    for h in handles {
        if let Ok((idx, path_str, status)) = h.await {
            buf[idx] = Some((idx, path_str, status));
        }
    }

    let results = buf
        .into_iter()
        .flatten()
        .map(|(index, path, status)| PathResult {
            index,
            path,
            status,
        })
        .collect();

    ScanResponse {
        protocol_version: PROTOCOL_VERSION,
        helper_version: HELPER_VERSION.to_string(),
        scanned_at: chrono::Utc::now().to_rfc3339(),
        results,
    }
}

/// Discover git repositories under one absolute scan root.
pub async fn run_scan_root(root: PathBuf, depth: u32, exclude: Vec<String>) -> ScanRootResponse {
    let root_str = root.to_string_lossy().into_owned();
    let status = tokio::task::spawn_blocking(move || {
        let mut excludes = git_clawler_core::scanner::default_excludes();
        excludes.extend(exclude.into_iter().filter(|value| !value.trim().is_empty()));
        excludes.sort();
        excludes.dedup();
        let opts = git_clawler_core::scanner::ScanOptions {
            max_depth: depth,
            excludes,
            follow_symlinks: false,
        };
        git_clawler_core::scanner::scan_for_repos(&root, &opts)
    })
    .await;

    let status = match status {
        Ok(Ok(paths)) => ScanRootStatus::Ok(
            paths
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        ),
        Ok(Err(e)) => ScanRootStatus::Err(scan_error_to_helper_error(&e)),
        Err(join_err) => ScanRootStatus::Err(HelperError {
            code: ErrorCode::Panic,
            message: format!("task panicked: {join_err}"),
        }),
    };

    ScanRootResponse {
        protocol_version: PROTOCOL_VERSION,
        helper_version: HELPER_VERSION.to_string(),
        scanned_at: chrono::Utc::now().to_rfc3339(),
        root: root_str,
        status,
    }
}

fn scan_error_to_helper_error(e: &git_clawler_core::scanner::ScanError) -> HelperError {
    let code = match e {
        git_clawler_core::scanner::ScanError::RootNotAbsolute(_) => ErrorCode::RelativePath,
        git_clawler_core::scanner::ScanError::RootNotFound(_)
        | git_clawler_core::scanner::ScanError::RootNotADirectory(_) => ErrorCode::NotARepository,
        git_clawler_core::scanner::ScanError::Io { .. } => ErrorCode::Io,
    };
    HelperError {
        code,
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ErrorCode, PathStatus};

    #[tokio::test]
    async fn empty_input_yields_empty_results() {
        let resp = run_scan(vec![], false).await;
        assert_eq!(resp.results.len(), 0);
        assert_eq!(resp.protocol_version, 1);
    }

    #[tokio::test]
    async fn relative_path_yields_relative_path_error() {
        let resp = run_scan(vec![PathBuf::from("relative/path")], false).await;
        assert_eq!(resp.results.len(), 1);
        match &resp.results[0].status {
            PathStatus::Err(e) => assert!(matches!(e.code, ErrorCode::RelativePath)),
            other => panic!("expected Err(RelativePath), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn nonexistent_absolute_path_yields_not_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist");
        // Drop tempdir so absolute path is gone; path remains absolute
        let resp = run_scan(vec![path], false).await;
        assert_eq!(resp.results.len(), 1);
        match &resp.results[0].status {
            PathStatus::Err(e) => assert!(matches!(e.code, ErrorCode::NotARepository)),
            other => panic!("expected Err(NotARepository), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn valid_repo_yields_ok() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let resp = run_scan(vec![dir.path().to_path_buf()], false).await;
        assert_eq!(resp.results.len(), 1);
        assert!(matches!(resp.results[0].status, PathStatus::Ok(_)));
    }

    #[tokio::test]
    async fn scan_root_discovers_repositories() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git2::Repository::init(&repo).unwrap();

        let resp = run_scan_root(dir.path().to_path_buf(), 1, vec![]).await;
        match resp.status {
            ScanRootStatus::Ok(paths) => {
                assert_eq!(paths.len(), 1);
                assert_eq!(PathBuf::from(&paths[0]), repo);
            }
            ScanRootStatus::Err(err) => panic!("expected ok, got {err:?}"),
        }
    }

    #[tokio::test]
    async fn scan_root_respects_excludes() {
        let dir = tempfile::tempdir().unwrap();
        let ignored = dir.path().join("ignored");
        std::fs::create_dir_all(&ignored).unwrap();
        git2::Repository::init(&ignored).unwrap();

        let resp = run_scan_root(dir.path().to_path_buf(), 1, vec!["ignored".into()]).await;
        match resp.status {
            ScanRootStatus::Ok(paths) => assert!(paths.is_empty()),
            ScanRootStatus::Err(err) => panic!("expected ok, got {err:?}"),
        }
    }

    #[tokio::test]
    async fn scan_root_relative_path_yields_error() {
        let resp = run_scan_root(PathBuf::from("relative"), 1, vec![]).await;
        match resp.status {
            ScanRootStatus::Err(err) => assert!(matches!(err.code, ErrorCode::RelativePath)),
            ScanRootStatus::Ok(paths) => panic!("expected error, got {paths:?}"),
        }
    }

    #[tokio::test]
    async fn multiple_paths_preserve_order() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let p_ok = dir.path().to_path_buf();
        let p_rel = PathBuf::from("rel/path");
        let resp = run_scan(vec![p_ok.clone(), p_rel.clone()], false).await;
        assert_eq!(resp.results.len(), 2);
        assert_eq!(resp.results[0].index, 0);
        assert_eq!(resp.results[1].index, 1);
        assert!(matches!(resp.results[0].status, PathStatus::Ok(_)));
        match &resp.results[1].status {
            PathStatus::Err(e) => assert!(matches!(e.code, ErrorCode::RelativePath)),
            other => panic!("expected Err(RelativePath), got {other:?}"),
        }
    }

    // W2: scan response JSON must include the new `conflict_predicted` field
    // in the `Ok` payload (preserving the protocol DTO contract).
    #[tokio::test]
    async fn w2_json_contains_conflict_predicted_field() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let resp = run_scan(vec![dir.path().to_path_buf()], false).await;
        let json = serde_json::to_value(&resp).unwrap();
        let status_val = &json["results"][0]["status"]["value"];
        assert!(
            status_val
                .as_object()
                .expect("ok status payload object")
                .contains_key("conflict_predicted"),
            "expected conflict_predicted key, got: {status_val:?}"
        );
    }

    #[tokio::test]
    async fn w5_json_contains_recent_commits_field() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let resp = run_scan(vec![dir.path().to_path_buf()], false).await;
        let json = serde_json::to_value(&resp).unwrap();
        let status_val = &json["results"][0]["status"]["value"];
        assert!(
            status_val
                .as_object()
                .expect("ok status payload object")
                .contains_key("recent_commits"),
            "expected recent_commits key, got: {status_val:?}"
        );
    }

    // W3: valid repo without origin remote → remote_url is None
    #[tokio::test]
    async fn w3_repo_without_remote_has_no_remote_url() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let resp = run_scan(vec![dir.path().to_path_buf()], false).await;
        assert_eq!(resp.results.len(), 1);
        match &resp.results[0].status {
            PathStatus::Ok(dto) => assert_eq!(
                dto.remote_url, None,
                "new repo with no remotes should have remote_url=None"
            ),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    // W4: valid repo with origin remote → remote_url is Some(url)
    #[tokio::test]
    async fn w4_repo_with_origin_remote_has_remote_url() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        // Add a fake origin remote
        repo.remote("origin", "https://github.com/owner/test-repo.git")
            .unwrap();
        let resp = run_scan(vec![dir.path().to_path_buf()], false).await;
        assert_eq!(resp.results.len(), 1);
        match &resp.results[0].status {
            PathStatus::Ok(dto) => {
                assert_eq!(
                    dto.remote_url.as_deref(),
                    Some("https://github.com/owner/test-repo.git"),
                    "remote_url should match origin URL"
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }
}
