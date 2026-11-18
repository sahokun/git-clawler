//! GUI-side executor that invokes the WSL helper via `wsl.exe`.
//!
//! WSL-resident repositories are scanned by an in-distro
//! `git-clawler-wsl-helper` binary which the Windows GUI launches with
//! `wsl.exe -d <distro> --exec <helper> <paths…>`. This module abstracts the
//! command invocation behind a [`WslCommandRunner`] trait so the executor is
//! unit-testable on any host.

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use git_clawler_core::{
    model::{RecentCommit, UnpushedBranch},
    RepoStatus,
};
use git_clawler_wsl_helper::protocol::{
    HelperError, PathResult, PathStatus, RecentCommitDto, RepoStatusDto, ScanResponse,
    ScanRootResponse, ScanRootStatus, UnpushedBranchDto,
};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[cfg(target_os = "windows")]
pub const WSL_CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(target_os = "windows")]
pub fn hide_wsl_console_window(command: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::windows::process::CommandExt;
    command.creation_flags(WSL_CREATE_NO_WINDOW)
}

#[derive(Debug, Error)]
pub enum WslError {
    #[error("WSL helper path is not configured (set GIT_CLAWLER_WSL_HELPER)")]
    HelperNotConfigured,
    #[error("failed to spawn wsl.exe: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("wsl.exe exited with status {status:?}: {stderr}")]
    NonZeroExit { status: Option<i32>, stderr: String },
    #[error("WSL command timed out after {seconds} seconds; retry the operation")]
    Timeout { seconds: u64 },
    #[error("failed to parse helper JSON: {0}")]
    ParseJson(#[source] serde_json::Error),
    #[error("unsupported protocol version: got {got}, expected {expected}")]
    ProtocolMismatch { got: u32, expected: u32 },
    #[error("helper response missing path: {0}")]
    MissingPath(PathBuf),
    #[error("helper response path does not match expected: got {got:?}, expected {expected:?}")]
    PathMismatch { got: String, expected: PathBuf },
    #[error("WSL helper reported an error: {message}")]
    HelperReportedError { message: String },
}

/// Re-export of [`git_clawler_core::config::normalize_distro`] for GUI use.
pub use git_clawler_core::config::normalize_distro;

/// Heuristic: a repository path that starts with `/` is treated as a WSL path
/// **on Windows builds only**. On Linux, `/`-rooted paths are native, so this
/// always returns `false` there. The explicit `distro` field works on every OS.
pub fn is_wsl_path(path: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        path.to_str().map(|s| s.starts_with('/')).unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        false
    }
}

/// Resolve the helper binary path from `GIT_CLAWLER_WSL_HELPER`.
///
/// We deliberately do **not** fall back to `~/.local/bin/...` — `wsl.exe`'s
/// non-shell exec does not expand `~`, and a misleading default would silently
/// fail. This optional override requires an absolute path inside each distro;
/// normal startup obtains the installed path from the bootstrap process.
pub fn resolve_helper_path() -> Result<String, WslError> {
    std::env::var("GIT_CLAWLER_WSL_HELPER").map_err(|_| WslError::HelperNotConfigured)
}

#[async_trait::async_trait]
pub trait WslCommandRunner: Send + Sync {
    async fn run(&self, args: &[String]) -> Result<RunOutput, WslError>;
}

pub struct RunOutput {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[cfg(target_os = "windows")]
pub struct TokioWslCommandRunner;

#[cfg(target_os = "windows")]
#[async_trait::async_trait]
impl WslCommandRunner for TokioWslCommandRunner {
    async fn run(&self, args: &[String]) -> Result<RunOutput, WslError> {
        let mut command = tokio::process::Command::new("wsl.exe");
        command.args(args).creation_flags(WSL_CREATE_NO_WINDOW);
        let duration = command_timeout(args);
        let out = run_bounded_command(&mut command, duration).await?;
        Ok(RunOutput {
            status: out.status.code(),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
}

fn command_timeout(args: &[String]) -> std::time::Duration {
    // Only options before the first path can enable fetch.
    let fetch = args
        .iter()
        .position(|arg| arg == "--exec")
        .and_then(|index| args.get(index + 2))
        .is_some_and(|arg| arg == "--fetch");
    std::time::Duration::from_secs(if fetch { 120 } else { 60 })
}

async fn run_bounded_command(
    command: &mut tokio::process::Command,
    duration: std::time::Duration,
) -> Result<std::process::Output, WslError> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    let mut child = command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(WslError::Spawn)?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let result = tokio::time::timeout(duration, async {
        let (status, out, err) = tokio::join!(
            child.wait(),
            stdout.read_to_end(&mut stdout_bytes),
            stderr.read_to_end(&mut stderr_bytes)
        );
        out.map_err(WslError::Spawn)?;
        err.map_err(WslError::Spawn)?;
        status.map_err(WslError::Spawn)
    })
    .await;
    match result {
        Ok(status) => Ok(std::process::Output {
            status: status?,
            stdout: stdout_bytes,
            stderr: stderr_bytes,
        }),
        Err(_) => {
            // Killing and reaping the launcher releases the update worker.
            // Do not terminate the user's entire WSL distro.
            child.kill().await.map_err(WslError::Spawn)?;
            child.wait().await.map_err(WslError::Spawn)?;
            Err(WslError::Timeout {
                seconds: duration.as_secs(),
            })
        }
    }
}

pub struct WslExecutor<R: WslCommandRunner> {
    runner: R,
    helper_path: String,
}

impl<R: WslCommandRunner> WslExecutor<R> {
    pub fn new(runner: R, helper_path: String) -> Self {
        Self {
            runner,
            helper_path,
        }
    }

    /// Scan a batch of paths under (optional) `distro`, returning per-path
    /// outcomes in input order.
    ///
    /// When `fetch` is `true`, `--fetch` is forwarded to the helper so that
    /// each repository's current-branch upstream is fetched before scanning.
    pub async fn scan_paths(
        &self,
        distro: Option<&str>,
        paths: &[PathBuf],
        fetch: bool,
    ) -> Result<Vec<(PathBuf, Result<(RepoStatus, Option<String>), HelperError>)>, WslError> {
        let mut args: Vec<String> = Vec::with_capacity(paths.len() + 6);
        if let Some(d) = distro {
            args.push("-d".into());
            args.push(d.into());
        }
        args.push("--exec".into());
        args.push(self.helper_path.clone());
        if fetch {
            args.push("--fetch".into());
        }
        for p in paths {
            args.push(p.to_string_lossy().into_owned());
        }

        let out = self.runner.run(&args).await?;
        if out.status != Some(0) {
            return Err(WslError::NonZeroExit {
                status: out.status,
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }

        let resp: ScanResponse =
            serde_json::from_slice(&out.stdout).map_err(WslError::ParseJson)?;
        if resp.protocol_version != git_clawler_wsl_helper::PROTOCOL_VERSION {
            return Err(WslError::ProtocolMismatch {
                got: resp.protocol_version,
                expected: git_clawler_wsl_helper::PROTOCOL_VERSION,
            });
        }

        let mut by_index: Vec<Option<PathResult>> = (0..paths.len()).map(|_| None).collect();
        for r in resp.results {
            let idx = r.index;
            if idx < by_index.len() {
                by_index[idx] = Some(r);
            }
        }

        let mut out_vec = Vec::with_capacity(paths.len());
        for (i, slot) in by_index.into_iter().enumerate() {
            let r = slot.ok_or_else(|| WslError::MissingPath(paths[i].clone()))?;
            if Path::new(&r.path) != paths[i].as_path() {
                return Err(WslError::PathMismatch {
                    got: r.path.clone(),
                    expected: paths[i].clone(),
                });
            }
            let mapped = match r.status {
                PathStatus::Ok(dto) => {
                    let remote_url = dto.remote_url.clone();
                    Ok((repo_status_from_dto(dto), remote_url))
                }
                PathStatus::Err(e) => Err(e),
            };
            out_vec.push((paths[i].clone(), mapped));
        }
        Ok(out_vec)
    }

    /// Discover repositories below a WSL scan root.
    pub async fn scan_root(
        &self,
        distro: Option<&str>,
        root: &Path,
        depth: u32,
        exclude: &[String],
    ) -> Result<Vec<PathBuf>, WslError> {
        let mut args: Vec<String> = Vec::with_capacity(exclude.len() * 2 + 8);
        if let Some(d) = distro {
            args.push("-d".into());
            args.push(d.into());
        }
        args.push("--exec".into());
        args.push(self.helper_path.clone());
        args.push("--scan-root".into());
        args.push(root.to_string_lossy().into_owned());
        args.push("--depth".into());
        args.push(depth.to_string());
        for item in exclude {
            args.push("--exclude".into());
            args.push(item.clone());
        }

        let out = self.runner.run(&args).await?;
        if out.status != Some(0) {
            return Err(WslError::NonZeroExit {
                status: out.status,
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }

        let resp: ScanRootResponse =
            serde_json::from_slice(&out.stdout).map_err(WslError::ParseJson)?;
        if resp.protocol_version != git_clawler_wsl_helper::PROTOCOL_VERSION {
            return Err(WslError::ProtocolMismatch {
                got: resp.protocol_version,
                expected: git_clawler_wsl_helper::PROTOCOL_VERSION,
            });
        }
        match resp.status {
            ScanRootStatus::Ok(paths) => Ok(paths.into_iter().map(PathBuf::from).collect()),
            ScanRootStatus::Err(err) => Err(WslError::HelperReportedError {
                message: helper_error_message(&err),
            }),
        }
    }
}

impl<R: WslCommandRunner> WslExecutor<R> {
    fn base_args(&self, distro: Option<&str>) -> Vec<String> {
        let mut args = Vec::with_capacity(16);
        if let Some(d) = distro {
            args.push("-d".into());
            args.push(d.into());
        }
        args.push("--exec".into());
        args.push(self.helper_path.clone());
        args
    }

    async fn run_json<T: serde::de::DeserializeOwned>(
        &self,
        args: &[String],
    ) -> Result<T, WslError> {
        let out = self.runner.run(args).await?;
        if out.status != Some(0) {
            return Err(WslError::NonZeroExit {
                status: out.status,
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        serde_json::from_slice(&out.stdout).map_err(WslError::ParseJson)
    }

    /// Repositories and linked worktrees under a WSL scan root (`--discover`).
    pub async fn discover(
        &self,
        distro: Option<&str>,
        root: &Path,
        depth: u32,
        exclude: &[String],
        worktrees: bool,
    ) -> Result<Vec<git_clawler_core::discovery::Found>, WslError> {
        let mut args = self.base_args(distro);
        args.push("--discover".into());
        args.push(root.to_string_lossy().into_owned());
        args.push("--depth".into());
        args.push(depth.to_string());
        for item in exclude {
            args.push("--exclude".into());
            args.push(item.clone());
        }
        if !worktrees {
            args.push("--no-worktrees".into());
        }
        let resp: git_clawler_wsl_helper::protocol::DiscoverResponse = self.run_json(&args).await?;
        if resp.protocol_version != git_clawler_wsl_helper::PROTOCOL_VERSION {
            return Err(WslError::ProtocolMismatch {
                got: resp.protocol_version,
                expected: git_clawler_wsl_helper::PROTOCOL_VERSION,
            });
        }
        match resp.status {
            git_clawler_wsl_helper::protocol::DiscoverStatus::Ok(found) => Ok(found
                .into_iter()
                .map(|f| git_clawler_core::discovery::Found {
                    path: PathBuf::from(f.path),
                    worktree_of: f.worktree_of.map(PathBuf::from),
                })
                .collect()),
            git_clawler_wsl_helper::protocol::DiscoverStatus::Err(err) => {
                Err(WslError::HelperReportedError {
                    message: helper_error_message(&err),
                })
            }
        }
    }

    /// Which of the `(worktree, main repository)` pairs Git no longer tracks.
    pub async fn worktrees_gone(
        &self,
        distro: Option<&str>,
        pairs: &[(PathBuf, PathBuf)],
    ) -> Result<Vec<PathBuf>, WslError> {
        if pairs.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = self.base_args(distro);
        args.push("--worktree-gone".into());
        for (path, parent) in pairs {
            args.push("--worktree".into());
            args.push(path.to_string_lossy().into_owned());
            args.push("--parent".into());
            args.push(parent.to_string_lossy().into_owned());
        }
        let resp: git_clawler_wsl_helper::protocol::WorktreeGoneResponse =
            self.run_json(&args).await?;
        if resp.protocol_version != git_clawler_wsl_helper::PROTOCOL_VERSION {
            return Err(WslError::ProtocolMismatch {
                got: resp.protocol_version,
                expected: git_clawler_wsl_helper::PROTOCOL_VERSION,
            });
        }
        Ok(resp.gone.into_iter().map(PathBuf::from).collect())
    }
}

fn repo_status_from_dto(d: RepoStatusDto) -> RepoStatus {
    RepoStatus {
        current_branch: d.current_branch,
        uncommitted_files: d.uncommitted_files,
        unpushed_branches: d
            .unpushed_branches
            .into_iter()
            .map(unpushed_from_dto)
            .collect(),
        ahead: d.ahead,
        behind: d.behind,
        github: None,
        conflict_predicted: d.conflict_predicted,
        fetch_warning: d.fetch_warning,
        recent_commits: d.recent_commits.into_iter().map(recent_from_dto).collect(),
        worktree_parent: d.worktree_parent.map(PathBuf::from),
    }
}

fn recent_from_dto(c: RecentCommitDto) -> RecentCommit {
    RecentCommit {
        short_hash: c.short_hash,
        summary: c.summary,
        author_name: c.author_name,
        committed_at: c.committed_at,
    }
}

fn unpushed_from_dto(b: UnpushedBranchDto) -> UnpushedBranch {
    UnpushedBranch {
        name: b.name,
        ahead: b.ahead,
        has_upstream: b.has_upstream,
    }
}

/// Format a [`HelperError`] into a single-line GUI-friendly message.
pub fn helper_error_message(e: &HelperError) -> String {
    format!("[{:?}] {}", e.code, e.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_clawler_wsl_helper::protocol::{
        ErrorCode, HelperError, PathResult, PathStatus, RepoStatusDto, ScanResponse,
    };

    #[test]
    fn timeout_selects_fetch_budget() {
        assert_eq!(
            command_timeout(&["--exec".into(), "helper".into(), "--fetch".into()]).as_secs(),
            120
        );
        assert_eq!(
            command_timeout(&["--exec".into(), "helper".into(), "/repo".into()]).as_secs(),
            60
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stalled_process_is_killed_and_can_be_retried() {
        let mut command = tokio::process::Command::new("sleep");
        command.arg("30");
        let start = std::time::Instant::now();
        let error = run_bounded_command(&mut command, std::time::Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(error, WslError::Timeout { .. }));
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        let mut retry = tokio::process::Command::new("printf");
        retry.arg("recovered");
        let output = run_bounded_command(&mut retry, std::time::Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(output.stdout, b"recovered");
    }

    struct MockRunner {
        captured_args: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        response: std::sync::Arc<dyn Fn() -> Result<RunOutput, WslError> + Send + Sync>,
    }

    impl MockRunner {
        fn new(
            f: impl Fn() -> Result<RunOutput, WslError> + Send + Sync + 'static,
        ) -> (Self, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
            let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let runner = MockRunner {
                captured_args: captured.clone(),
                response: std::sync::Arc::new(f),
            };
            (runner, captured)
        }
    }

    #[async_trait::async_trait]
    impl WslCommandRunner for MockRunner {
        async fn run(&self, args: &[String]) -> Result<RunOutput, WslError> {
            *self.captured_args.lock().unwrap() = args.to_vec();
            (self.response)()
        }
    }

    fn make_scan_response(paths: &[(&str, PathStatus)]) -> Vec<u8> {
        let results: Vec<PathResult> = paths
            .iter()
            .enumerate()
            .map(|(i, (p, s))| PathResult {
                index: i,
                path: (*p).into(),
                status: s.clone(),
            })
            .collect();
        let resp = ScanResponse {
            protocol_version: git_clawler_wsl_helper::PROTOCOL_VERSION,
            helper_version: "0.0.1".into(),
            scanned_at: "2024-01-01T00:00:00Z".into(),
            results,
        };
        serde_json::to_vec(&resp).unwrap()
    }

    fn make_scan_root_response(root: &str, paths: Vec<&str>) -> Vec<u8> {
        let resp = ScanRootResponse {
            protocol_version: git_clawler_wsl_helper::PROTOCOL_VERSION,
            helper_version: "0.0.1".into(),
            scanned_at: "2024-01-01T00:00:00Z".into(),
            root: root.into(),
            status: ScanRootStatus::Ok(paths.into_iter().map(String::from).collect()),
        };
        serde_json::to_vec(&resp).unwrap()
    }

    fn ok_status() -> PathStatus {
        PathStatus::Ok(RepoStatusDto {
            worktree_parent: None,
            current_branch: Some("main".into()),
            uncommitted_files: 0,
            unpushed_branches: vec![],
            ahead: 0,
            behind: 0,
            conflict_predicted: None,
            remote_url: None,
            fetch_warning: None,
            recent_commits: vec![],
        })
    }

    fn err_status() -> PathStatus {
        PathStatus::Err(HelperError {
            code: ErrorCode::NotARepository,
            message: "not a repo".into(),
        })
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn is_wsl_path_unix_absolute_true() {
        assert!(is_wsl_path(Path::new("/home/user/proj")));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn is_wsl_path_unix_absolute_false_on_linux() {
        // On Linux, `/`-rooted paths are native, so they're not WSL paths.
        assert!(!is_wsl_path(Path::new("/home/user/proj")));
    }

    #[test]
    fn is_wsl_path_windows_native_false() {
        assert!(!is_wsl_path(Path::new(r"C:\Users\foo")));
    }

    #[test]
    fn is_wsl_path_relative_false() {
        assert!(!is_wsl_path(Path::new("relative/path")));
    }

    #[test]
    fn wsl_error_display_for_helper_not_configured() {
        let err = WslError::HelperNotConfigured;
        let msg = err.to_string();
        assert!(msg.contains("GIT_CLAWLER_WSL_HELPER"));
    }

    #[tokio::test]
    async fn scan_paths_builds_args_with_distro() {
        let (runner, captured) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: make_scan_response(&[("/home/user/proj", ok_status())]),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        executor
            .scan_paths(Some("Ubuntu"), &[PathBuf::from("/home/user/proj")], false)
            .await
            .unwrap();
        let args = captured.lock().unwrap();
        assert_eq!(args[0], "-d");
        assert_eq!(args[1], "Ubuntu");
        assert_eq!(args[2], "--exec");
        assert_eq!(args[3], "/usr/bin/helper");
        assert_eq!(args[4], "/home/user/proj");
    }

    #[tokio::test]
    async fn scan_paths_builds_args_without_distro_omits_dash_d() {
        let (runner, captured) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: make_scan_response(&[("/home/user/proj", ok_status())]),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        executor
            .scan_paths(None, &[PathBuf::from("/home/user/proj")], false)
            .await
            .unwrap();
        let args = captured.lock().unwrap();
        assert_eq!(args[0], "--exec");
        assert_eq!(args[1], "/usr/bin/helper");
    }

    #[tokio::test]
    async fn scan_root_builds_args_with_distro_depth_and_excludes() {
        let (runner, captured) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: make_scan_root_response("/home/user", vec!["/home/user/repo"]),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let paths = executor
            .scan_root(
                Some("Ubuntu"),
                Path::new("/home/user"),
                2,
                &["target".into(), "node_modules".into()],
            )
            .await
            .unwrap();
        assert_eq!(paths, vec![PathBuf::from("/home/user/repo")]);

        let args = captured.lock().unwrap();
        assert_eq!(args[0], "-d");
        assert_eq!(args[1], "Ubuntu");
        assert_eq!(args[2], "--exec");
        assert_eq!(args[3], "/usr/bin/helper");
        assert_eq!(args[4], "--scan-root");
        assert_eq!(args[5], "/home/user");
        assert_eq!(args[6], "--depth");
        assert_eq!(args[7], "2");
        assert_eq!(args[8], "--exclude");
        assert_eq!(args[9], "target");
        assert_eq!(args[10], "--exclude");
        assert_eq!(args[11], "node_modules");
    }

    #[tokio::test]
    async fn discover_builds_args_and_decodes_worktrees() {
        let stdout = serde_json::to_vec(&git_clawler_wsl_helper::protocol::DiscoverResponse {
            protocol_version: git_clawler_wsl_helper::PROTOCOL_VERSION,
            helper_version: "0.0.1".into(),
            scanned_at: "2026-11-16T00:00:00Z".into(),
            root: "/home/u/p".into(),
            status: git_clawler_wsl_helper::protocol::DiscoverStatus::Ok(vec![
                git_clawler_wsl_helper::protocol::FoundDto {
                    path: "/home/u/p/app".into(),
                    worktree_of: None,
                },
                git_clawler_wsl_helper::protocol::FoundDto {
                    path: "/tmp/app-wt".into(),
                    worktree_of: Some("/home/u/p/app".into()),
                },
            ]),
        })
        .unwrap();
        let (runner, captured) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: stdout.clone(),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let found = executor
            .discover(
                Some("Ubuntu"),
                Path::new("/home/u/p"),
                1,
                &["old".into()],
                false,
            )
            .await
            .unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].worktree_of, Some(PathBuf::from("/home/u/p/app")));
        let args = captured.lock().unwrap();
        assert_eq!(
            &args[..],
            [
                "-d",
                "Ubuntu",
                "--exec",
                "/usr/bin/helper",
                "--discover",
                "/home/u/p",
                "--depth",
                "1",
                "--exclude",
                "old",
                "--no-worktrees"
            ]
        );
    }

    #[tokio::test]
    async fn worktrees_gone_pairs_paths_with_parents() {
        let stdout = serde_json::to_vec(&git_clawler_wsl_helper::protocol::WorktreeGoneResponse {
            protocol_version: git_clawler_wsl_helper::PROTOCOL_VERSION,
            helper_version: "0.0.1".into(),
            gone: vec!["/tmp/wt".into()],
        })
        .unwrap();
        let (runner, captured) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: stdout.clone(),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let gone = executor
            .worktrees_gone(
                None,
                &[(PathBuf::from("/tmp/wt"), PathBuf::from("/home/u/app"))],
            )
            .await
            .unwrap();
        assert_eq!(gone, vec![PathBuf::from("/tmp/wt")]);
        let args = captured.lock().unwrap();
        assert_eq!(
            &args[..],
            [
                "--exec",
                "/usr/bin/helper",
                "--worktree-gone",
                "--worktree",
                "/tmp/wt",
                "--parent",
                "/home/u/app"
            ]
        );
    }

    #[test]
    fn worktree_parent_maps_from_helper_dto() {
        let dto: RepoStatusDto = serde_json::from_str(
            r#"{"current_branch":"f","uncommitted_files":0,"unpushed_branches":[],"ahead":0,"behind":0,"worktree_parent":"/home/u/app"}"#,
        )
        .unwrap();
        assert_eq!(
            repo_status_from_dto(dto).worktree_parent,
            Some(PathBuf::from("/home/u/app"))
        );
    }

    #[tokio::test]
    async fn scan_root_decodes_helper_error() {
        let stdout = {
            let resp = ScanRootResponse {
                protocol_version: git_clawler_wsl_helper::PROTOCOL_VERSION,
                helper_version: "0.0.1".into(),
                scanned_at: "2024-01-01T00:00:00Z".into(),
                root: "/missing".into(),
                status: ScanRootStatus::Err(HelperError {
                    code: ErrorCode::NotARepository,
                    message: "missing".into(),
                }),
            };
            serde_json::to_vec(&resp).unwrap()
        };
        let (runner, _) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: stdout.clone(),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let result = executor
            .scan_root(None, Path::new("/missing"), 1, &[])
            .await;
        assert!(matches!(
            result.unwrap_err(),
            WslError::HelperReportedError { .. }
        ));
    }

    #[tokio::test]
    async fn scan_paths_decodes_ok_status_into_repo_status() {
        let (runner, _) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: make_scan_response(&[("/home/user/proj", ok_status())]),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let results = executor
            .scan_paths(None, &[PathBuf::from("/home/user/proj")], false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_ok());
        assert_eq!(
            results[0].1.as_ref().unwrap().0.current_branch,
            Some("main".into())
        );
    }

    #[tokio::test]
    async fn scan_paths_decodes_err_status_into_helper_error() {
        let (runner, _) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: make_scan_response(&[("/home/user/proj", err_status())]),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let results = executor
            .scan_paths(None, &[PathBuf::from("/home/user/proj")], false)
            .await
            .unwrap();
        assert!(results[0].1.is_err());
        assert_eq!(results[0].1.as_ref().unwrap_err().message, "not a repo");
    }

    #[tokio::test]
    async fn scan_paths_preserves_input_order_when_helper_returns_reversed() {
        let stdout = {
            let resp = ScanResponse {
                protocol_version: git_clawler_wsl_helper::PROTOCOL_VERSION,
                helper_version: "0.0.1".into(),
                scanned_at: "2024-01-01T00:00:00Z".into(),
                results: vec![
                    PathResult {
                        index: 1,
                        path: "/b".into(),
                        status: ok_status(),
                    },
                    PathResult {
                        index: 0,
                        path: "/a".into(),
                        status: ok_status(),
                    },
                ],
            };
            serde_json::to_vec(&resp).unwrap()
        };
        let (runner, _) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: stdout.clone(),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let paths = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let results = executor.scan_paths(None, &paths, false).await.unwrap();
        assert_eq!(results[0].0, PathBuf::from("/a"));
        assert_eq!(results[1].0, PathBuf::from("/b"));
    }

    #[tokio::test]
    async fn scan_paths_returns_missing_path_when_helper_omits_index() {
        let stdout = {
            let resp = ScanResponse {
                protocol_version: git_clawler_wsl_helper::PROTOCOL_VERSION,
                helper_version: "0.0.1".into(),
                scanned_at: "2024-01-01T00:00:00Z".into(),
                results: vec![PathResult {
                    index: 1,
                    path: "/b".into(),
                    status: ok_status(),
                }],
            };
            serde_json::to_vec(&resp).unwrap()
        };
        let (runner, _) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: stdout.clone(),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let paths = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let result = executor.scan_paths(None, &paths, false).await;
        assert!(matches!(result.unwrap_err(), WslError::MissingPath(_)));
    }

    #[tokio::test]
    async fn scan_paths_returns_protocol_mismatch_when_version_differs() {
        let stdout = {
            let resp = ScanResponse {
                protocol_version: 999,
                helper_version: "0.0.1".into(),
                scanned_at: "2024-01-01T00:00:00Z".into(),
                results: vec![],
            };
            serde_json::to_vec(&resp).unwrap()
        };
        let (runner, _) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: stdout.clone(),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let result = executor.scan_paths(None, &[], false).await;
        assert!(matches!(
            result.unwrap_err(),
            WslError::ProtocolMismatch { .. }
        ));
    }

    #[tokio::test]
    async fn scan_paths_returns_parse_json_when_stdout_invalid() {
        let (runner, _) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: b"not valid json".to_vec(),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let result = executor
            .scan_paths(None, &[PathBuf::from("/foo")], false)
            .await;
        assert!(matches!(result.unwrap_err(), WslError::ParseJson(_)));
    }

    #[tokio::test]
    async fn scan_paths_returns_non_zero_exit_when_status_nonzero() {
        let (runner, _) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(1),
                stdout: vec![],
                stderr: b"some error".to_vec(),
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let result = executor
            .scan_paths(None, &[PathBuf::from("/foo")], false)
            .await;
        match result.unwrap_err() {
            WslError::NonZeroExit { status, stderr } => {
                assert_eq!(status, Some(1));
                assert_eq!(stderr, "some error");
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn scan_paths_propagates_runner_io_error() {
        let (runner, _) = MockRunner::new(move || {
            Err(WslError::Spawn(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "wsl.exe not found",
            )))
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let result = executor
            .scan_paths(None, &[PathBuf::from("/foo")], false)
            .await;
        assert!(matches!(result.unwrap_err(), WslError::Spawn(_)));
    }

    #[tokio::test]
    async fn scan_paths_handles_empty_paths_input() {
        let (runner, captured) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: {
                    let resp = ScanResponse {
                        protocol_version: git_clawler_wsl_helper::PROTOCOL_VERSION,
                        helper_version: "0.0.1".into(),
                        scanned_at: "2024-01-01T00:00:00Z".into(),
                        results: vec![],
                    };
                    serde_json::to_vec(&resp).unwrap()
                },
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let results = executor.scan_paths(None, &[], false).await.unwrap();
        assert!(results.is_empty());
        let _ = captured;
    }

    #[tokio::test]
    async fn scan_paths_single_path_args_count_correct() {
        let (runner, captured) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: make_scan_response(&[("/home/user/proj", ok_status())]),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        executor
            .scan_paths(Some("Ubuntu"), &[PathBuf::from("/home/user/proj")], false)
            .await
            .unwrap();
        let args = captured.lock().unwrap();
        // Layout: -d <distro> --exec <helper> <path>
        assert_eq!(args.len(), 5);
        assert_eq!(args[0], "-d");
        assert_eq!(args[1], "Ubuntu");
        assert_eq!(args[2], "--exec");
        assert_eq!(args[3], "/usr/bin/helper");
        assert_eq!(args[4], "/home/user/proj");
    }

    #[tokio::test]
    async fn scan_paths_returns_path_mismatch_when_response_path_differs() {
        let stdout = {
            let resp = ScanResponse {
                protocol_version: git_clawler_wsl_helper::PROTOCOL_VERSION,
                helper_version: "0.0.1".into(),
                scanned_at: "2024-01-01T00:00:00Z".into(),
                results: vec![PathResult {
                    index: 0,
                    path: "/wrong".into(),
                    status: ok_status(),
                }],
            };
            serde_json::to_vec(&resp).unwrap()
        };
        let (runner, _) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: stdout.clone(),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        let result = executor
            .scan_paths(None, &[PathBuf::from("/a")], false)
            .await;
        assert!(matches!(result.unwrap_err(), WslError::PathMismatch { .. }));
    }

    #[test]
    fn normalize_distro_none_stays_none() {
        assert_eq!(normalize_distro(None), None);
    }
    #[test]
    fn normalize_distro_empty_string_becomes_none() {
        assert_eq!(normalize_distro(Some("".into())), None);
    }
    #[test]
    fn normalize_distro_blank_string_becomes_none() {
        assert_eq!(normalize_distro(Some(" ".into())), None);
    }
    #[test]
    fn normalize_distro_trims_whitespace() {
        assert_eq!(
            normalize_distro(Some(" Ubuntu ".into())),
            Some("Ubuntu".into())
        );
    }

    #[test]
    fn helper_error_message_includes_code_and_message() {
        let e = HelperError {
            code: ErrorCode::NotARepository,
            message: "not a repo".into(),
        };
        let msg = helper_error_message(&e);
        assert!(msg.contains("not a repo"));
        assert!(msg.contains("NotARepository"));
    }

    // G1: scan_paths(_, _, true) must include "--fetch" in the runner args
    // (placed after the helper path / before the path arguments).
    #[tokio::test]
    async fn g1_scan_paths_with_fetch_true_includes_fetch_arg() {
        let (runner, captured) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: make_scan_response(&[("/home/user/proj", ok_status())]),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        executor
            .scan_paths(None, &[PathBuf::from("/home/user/proj")], true)
            .await
            .unwrap();
        let args = captured.lock().unwrap();
        assert!(
            args.contains(&"--fetch".to_string()),
            "expected --fetch in args: {:?}",
            *args
        );
    }

    // G2: scan_paths(_, _, false) must NOT include "--fetch".
    #[tokio::test]
    async fn g2_scan_paths_with_fetch_false_omits_fetch_arg() {
        let (runner, captured) = MockRunner::new(move || {
            Ok(RunOutput {
                status: Some(0),
                stdout: make_scan_response(&[("/home/user/proj", ok_status())]),
                stderr: vec![],
            })
        });
        let executor = WslExecutor::new(runner, "/usr/bin/helper".into());
        executor
            .scan_paths(None, &[PathBuf::from("/home/user/proj")], false)
            .await
            .unwrap();
        let args = captured.lock().unwrap();
        assert!(
            !args.contains(&"--fetch".to_string()),
            "--fetch must NOT be in args: {:?}",
            *args
        );
    }

    // G3: repo_status_from_dto must preserve the conflict_predicted field
    // from the protocol DTO into the runtime model.
    #[test]
    fn g3_repo_status_from_dto_preserves_conflict_predicted() {
        let dto = RepoStatusDto {
            worktree_parent: None,
            current_branch: Some("main".into()),
            uncommitted_files: 0,
            unpushed_branches: vec![],
            ahead: 0,
            behind: 0,
            conflict_predicted: Some(true),
            remote_url: None,
            fetch_warning: Some("fetch connection failed; using cached remote refs".into()),
            recent_commits: vec![],
        };
        let status = repo_status_from_dto(dto);
        assert_eq!(status.conflict_predicted, Some(true));
        assert!(status
            .fetch_warning
            .as_deref()
            .unwrap()
            .contains("cached remote refs"));

        let dto_none = RepoStatusDto {
            worktree_parent: None,
            current_branch: None,
            uncommitted_files: 0,
            unpushed_branches: vec![],
            ahead: 0,
            behind: 0,
            conflict_predicted: None,
            remote_url: None,
            fetch_warning: None,
            recent_commits: vec![],
        };
        assert_eq!(repo_status_from_dto(dto_none).conflict_predicted, None);
    }
}
