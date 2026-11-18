//! WSL helper bootstrap: lazy install and version-handshake registry.
//!
//! `WslHelperRegistry` manages per-distro installation of `git-clawler-wsl-helper`
//! into `~/.local/bin/` inside the target WSL distro. It uses `tokio::sync::OnceCell`
//! per distro key to serialise concurrent first-install races while allowing
//! parallel bootstraps across different distros.
//!
//! # Bootstrap flow
//! ```text
//! ensure(distro) →
//!   [env override?] → validate handshake and return env value
//!   [OnceCell initialized?] → return cached path
//!   pre-check: run `<helper> --protocol`, parse HelperHandshake
//!   [installed & version compatible?] → cache & return path
//!   install: run bash script (wslpath + cp + chmod + mv)
//!   post-check: run `<installed-path> --protocol`, verify
//!   cache & return path
//! ```

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use super::executor::{WslCommandRunner, WslError};
use git_clawler_wsl_helper::protocol::HelperHandshake;
use git_clawler_wsl_helper::HELPER_VERSION;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::OnceCell;

const HELPER_TARGET_TRIPLE: &str = "x86_64-unknown-linux-musl";
const HELPER_BINARY_NAME: &str = "git-clawler-wsl-helper";

/// Exit code emitted by the install-path probe when the helper binary is
/// absent or non-executable. Only this status is treated as "not installed"
/// and triggers the install flow; any other non-zero status is a hard probe
/// error and is propagated up by `run_probe`.
const PROBE_NOT_FOUND_EXIT: i32 = 42;

/// Bash snippet that resolves `$HOME/.local/bin/git-clawler-wsl-helper`
/// inside the target distro and prints the absolute path on success. Emits
/// [`PROBE_NOT_FOUND_EXIT`] when the helper is missing so the caller can
/// proceed straight to install.
const PROBE_SCRIPT: &str = r#"set -eu; p="$HOME/.local/bin/git-clawler-wsl-helper"; if [ -x "$p" ]; then printf "%s" "$p"; else exit 42; fi"#;

/// Result of the in-distro install-path probe.
///
/// "Not installed" is a *normal* branch of the bootstrap flow (it triggers
/// the install path), so it is modelled as an `Ok` variant rather than an
/// error. Any other failure (spawn error, unexpected non-zero exit, empty
/// stdout despite status=0) propagates as a hard `BootstrapError`.
#[derive(Debug)]
enum ProbeOutcome {
    /// Helper exists and the probe printed its absolute path.
    Found(String),
    /// Helper is absent: probe exited with `PROBE_NOT_FOUND_EXIT` (42).
    NotInstalled,
}

/// Error type for bootstrap operations.
#[derive(Debug, Error)]
pub enum BootstrapError {
    #[error("runner error during bootstrap: {0}")]
    Runner(#[from] WslError),
    #[error("failed to parse handshake JSON: {0}")]
    HandshakeParse(#[source] serde_json::Error),
    #[error("install script produced no output path")]
    NoInstallPath,
    #[error("install-path probe returned empty path")]
    EmptyProbePath,
    #[error(
        "post-install handshake failed: protocol version mismatch (got {got}, expected {expected})"
    )]
    ProtocolMismatch { got: u32, expected: u32 },
    /// Post-install handshake succeeded but a required capability was not
    /// advertised.  This indicates a build/deploy inconsistency (the newly
    /// installed binary should always advertise all capabilities).
    #[error("installed helper is missing required capability: {name}")]
    MissingCapability { name: &'static str },
    #[error("helper version mismatch (got {got}, expected compatible with {expected})")]
    VersionMismatch { got: String, expected: String },
}

/// Input required to bootstrap a WSL distro.
#[derive(Debug, Clone)]
pub struct BootstrapInput {
    /// WSL distro name (None = system default).
    pub distro: Option<String>,
    /// Path to the helper binary in the MSI resources directory (Windows-side path).
    pub resource_helper_path: PathBuf,
}

/// Resolve the Windows-side helper path from Tauri's resource directory.
///
/// Packaged builds may stage assets below an executable-adjacent `resources`
/// directory or directly in the resource directory. Development
/// builds may resolve `resource_dir` to `target/{debug,release}`; in that case,
/// prefer Tauri's staged resource path, then already-built helper artifacts in
/// the workspace target directory.
pub fn resolve_resource_helper_path(resource_dir: &Path) -> PathBuf {
    let packaged_resource = resource_dir
        .join("wsl-helper")
        .join(HELPER_TARGET_TRIPLE)
        .join(HELPER_BINARY_NAME);

    let staged_resource = resource_dir
        .join("resources")
        .join("wsl-helper")
        .join(HELPER_TARGET_TRIPLE)
        .join(HELPER_BINARY_NAME);
    if path_exists(&staged_resource) {
        return staged_resource;
    }

    let Some(workspace_root) = workspace_root_from_resource_dir(resource_dir) else {
        return packaged_resource;
    };

    for candidate in dev_helper_candidates(workspace_root) {
        if path_exists(&candidate) {
            return candidate;
        }
    }

    staged_resource
}

fn workspace_root_from_resource_dir(resource_dir: &Path) -> Option<&Path> {
    let profile_dir = resource_dir.file_name()?.to_str()?;
    if profile_dir != "debug" && profile_dir != "release" {
        return None;
    }
    let target_dir = resource_dir.parent()?;
    if target_dir.file_name()?.to_str()? != "target" {
        return None;
    }
    target_dir.parent()
}

fn dev_helper_candidates(workspace_root: &Path) -> [PathBuf; 4] {
    [
        workspace_root
            .join("target")
            .join(HELPER_TARGET_TRIPLE)
            .join("debug")
            .join(HELPER_BINARY_NAME),
        workspace_root
            .join("target")
            .join(HELPER_TARGET_TRIPLE)
            .join("release")
            .join(HELPER_BINARY_NAME),
        workspace_root
            .join("target")
            .join("debug")
            .join(HELPER_BINARY_NAME),
        workspace_root
            .join("target")
            .join("release")
            .join(HELPER_BINARY_NAME),
    ]
}

fn path_exists(path: &Path) -> bool {
    path.try_exists().unwrap_or(false)
}

/// Registry that lazily bootstraps the WSL helper once per distro.
pub struct WslHelperRegistry {
    runner: Arc<dyn WslCommandRunner>,
    cells: Mutex<HashMap<String, Arc<OnceCell<String>>>>,
}

impl WslHelperRegistry {
    pub fn new(runner: Arc<dyn WslCommandRunner>) -> Self {
        Self {
            runner,
            cells: Mutex::new(HashMap::new()),
        }
    }

    /// Ensure the helper is installed and return its in-distro path.
    ///
    /// Validates and returns `GIT_CLAWLER_WSL_HELPER` if set (dev override).
    /// Uses a per-distro `OnceCell` to serialise concurrent first-install races.
    pub async fn ensure(&self, input: BootstrapInput) -> Result<String, BootstrapError> {
        if let Ok(path) = std::env::var("GIT_CLAWLER_WSL_HELPER") {
            let handshake = self.run_handshake(input.distro.as_deref(), &path).await?;
            validate_required_handshake(&handshake)?;
            tracing::debug!(
                path = %path,
                version = %handshake.helper_version,
                "wsl.bootstrap.env_override.validated"
            );
            return Ok(path);
        }

        let key = input.distro.clone().unwrap_or_default();
        let cell: Arc<OnceCell<String>> = {
            let mut map = self.cells.lock().unwrap();
            map.entry(key)
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        let path = cell.get_or_try_init(|| self.do_bootstrap(input)).await?;
        Ok(path.clone())
    }

    async fn do_bootstrap(&self, input: BootstrapInput) -> Result<String, BootstrapError> {
        let resource_path_str = input.resource_helper_path.to_string_lossy().into_owned();

        // Step A: dynamically resolve `$HOME/.local/bin/git-clawler-wsl-helper`
        // inside the distro. This avoids hard-coding `/root/...` which only
        // matches the `root` user and forced reinstall for everyone else.
        let probe_outcome = self.run_probe(input.distro.as_deref()).await?;
        //                                                          ^^ hard error は即伝播

        match probe_outcome {
            ProbeOutcome::Found(probe_path) => {
                // Step B: handshake against the probed path to check version
                // *and* capabilities.  Version compatibility and capability
                // presence are independent checks; an installed helper can be
                // version-compatible but pre-date a required capability (e.g.
                // `populates_remote_url` added in a non-version-bumping change).
                match self
                    .run_handshake(input.distro.as_deref(), &probe_path)
                    .await
                {
                    Ok(handshake) if validate_required_handshake(&handshake).is_ok() => {
                        tracing::debug!(
                            distro = ?input.distro,
                            path = %probe_path,
                            version = %handshake.helper_version,
                            "wsl.bootstrap.skipped reason=already_installed"
                        );
                        return Ok(probe_path);
                    }
                    Ok(handshake)
                        if version_compatible(&handshake.helper_version, HELPER_VERSION) =>
                    {
                        // The semantic version matches, but the protocol or a required
                        // capability is incompatible. Reinstall before caching.
                        tracing::info!(
                            distro = ?input.distro,
                            path = %probe_path,
                            version = %handshake.helper_version,
                            "wsl.bootstrap.reinstall reason=protocol_or_capability_mismatch"
                        );
                    }
                    Ok(handshake) => {
                        tracing::info!(
                            distro = ?input.distro,
                            installed = %handshake.helper_version,
                            current = %HELPER_VERSION,
                            "wsl.bootstrap.reinstall reason=version_mismatch"
                        );
                    }
                    Err(_) => {
                        tracing::info!(
                            distro = ?input.distro,
                            path = %probe_path,
                            "wsl.bootstrap.reinstall reason=handshake_failed"
                        );
                    }
                }
            }
            ProbeOutcome::NotInstalled => {
                tracing::info!(
                    distro = ?input.distro,
                    "wsl.bootstrap.install reason=not_found"
                );
            }
        }

        let install_args = build_install_args(input.distro.as_deref(), &resource_path_str);
        let install_out = self.runner.run(&install_args).await?;
        if install_out.status != Some(0) {
            return Err(BootstrapError::Runner(WslError::NonZeroExit {
                status: install_out.status,
                stderr: String::from_utf8_lossy(&install_out.stderr).into_owned(),
            }));
        }
        let installed_path = String::from_utf8_lossy(&install_out.stdout)
            .trim()
            .to_string();
        if installed_path.is_empty() {
            return Err(BootstrapError::NoInstallPath);
        }

        let post_check = self
            .run_handshake(input.distro.as_deref(), &installed_path)
            .await?;
        validate_required_handshake(&post_check)?;

        tracing::info!(
            distro = ?input.distro,
            path = %installed_path,
            version = %post_check.helper_version,
            "wsl.bootstrap.done"
        );

        Ok(installed_path)
    }

    async fn run_handshake(
        &self,
        distro: Option<&str>,
        helper_path: &str,
    ) -> Result<HelperHandshake, BootstrapError> {
        let mut args: Vec<String> = Vec::new();
        if let Some(d) = distro {
            args.push("-d".into());
            args.push(d.into());
        }
        args.push("--exec".into());
        args.push(helper_path.into());
        args.push("--protocol".into());

        let out = self.runner.run(&args).await?;
        if out.status != Some(0) {
            return Err(BootstrapError::Runner(WslError::NonZeroExit {
                status: out.status,
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            }));
        }
        let handshake: HelperHandshake =
            serde_json::from_slice(&out.stdout).map_err(BootstrapError::HandshakeParse)?;
        Ok(handshake)
    }

    /// Probe the in-distro install path by expanding `$HOME` via `bash -lc`.
    /// Returns [`ProbeOutcome::Found`] with the resolved absolute path on success,
    /// or [`ProbeOutcome::NotInstalled`] when the helper is absent (exit 42).
    /// Any other failure (spawn error, unexpected non-zero exit, empty stdout)
    /// propagates as a hard [`BootstrapError`].
    async fn run_probe(&self, distro: Option<&str>) -> Result<ProbeOutcome, BootstrapError> {
        let args = build_probe_args(distro);
        let out = self.runner.run(&args).await?; // spawn error → hard error

        match out.status {
            Some(code) if code == PROBE_NOT_FOUND_EXIT => Ok(ProbeOutcome::NotInstalled),
            Some(0) => {
                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if path.is_empty() {
                    tracing::warn!(
                        distro = ?distro,
                        "wsl.bootstrap.probe_failed reason=empty_stdout"
                    );
                    Err(BootstrapError::EmptyProbePath)
                } else {
                    Ok(ProbeOutcome::Found(path))
                }
            }
            other => {
                let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                tracing::warn!(
                    distro = ?distro,
                    status = ?other,
                    stderr = %stderr,
                    "wsl.bootstrap.probe_failed reason=unexpected_exit"
                );
                Err(BootstrapError::Runner(WslError::NonZeroExit {
                    status: other,
                    stderr,
                }))
            }
        }
    }
}

/// Build the `wsl.exe` argument list for the install-path probe.
///
/// The probe runs [`PROBE_SCRIPT`] which expands `$HOME` inside the target
/// distro and prints the resolved helper path on success, exiting with
/// [`PROBE_NOT_FOUND_EXIT`] otherwise.
pub fn build_probe_args(distro: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if let Some(d) = distro {
        args.push("-d".into());
        args.push(d.into());
    }
    args.push("--exec".into());
    args.push("bash".into());
    args.push("-lc".into());
    args.push(PROBE_SCRIPT.into());
    args
}

/// Build the `wsl.exe` argument list for the install script.
///
/// The script uses `wslpath -a "$1"` to convert the Windows-side resource path
/// to a Linux path inside the distro, then does a `mktemp + cp + chmod + mv`
/// atomic replace into `$HOME/.local/bin/git-clawler-wsl-helper`.
pub fn build_install_args(distro: Option<&str>, windows_helper_path: &str) -> Vec<String> {
    let script = r#"set -euo pipefail; \
src="$(wslpath -a "$1")"; \
dst="$HOME/.local/bin/git-clawler-wsl-helper"; \
mkdir -p "$(dirname "$dst")"; \
tmp="$(mktemp "${dst}.XXXXXX")"; \
cp "$src" "$tmp"; chmod 0755 "$tmp"; mv -f "$tmp" "$dst"; \
printf "%s\n" "$dst""#;

    let mut args: Vec<String> = Vec::new();
    if let Some(d) = distro {
        args.push("-d".into());
        args.push(d.into());
    }
    args.push("--exec".into());
    args.push("bash".into());
    args.push("-lc".into());
    args.push(script.into());
    args.push("_".into());
    args.push(windows_helper_path.into());
    args
}

/// Check if `installed` version is compatible with `current` version.
///
/// Compatibility: same major.minor (patch differences are ignored).
/// Handles semver-style "M.N.P" as well as non-semver strings (returns false).
pub fn version_compatible(installed: &str, current: &str) -> bool {
    fn parse_major_minor(s: &str) -> Option<(u64, u64)> {
        let mut parts = s.splitn(3, '.');
        let major: u64 = parts.next()?.parse().ok()?;
        let minor: u64 = parts.next()?.parse().ok()?;
        Some((major, minor))
    }
    match (parse_major_minor(installed), parse_major_minor(current)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Validate protocol version, helper version compatibility, and required capabilities.
///
/// Shared between the env-override path and can be used by the post-install path.
fn validate_required_handshake(handshake: &HelperHandshake) -> Result<(), BootstrapError> {
    if handshake.protocol_version != git_clawler_wsl_helper::PROTOCOL_VERSION {
        return Err(BootstrapError::ProtocolMismatch {
            got: handshake.protocol_version,
            expected: git_clawler_wsl_helper::PROTOCOL_VERSION,
        });
    }
    if !version_compatible(&handshake.helper_version, HELPER_VERSION) {
        return Err(BootstrapError::VersionMismatch {
            got: handshake.helper_version.clone(),
            expected: HELPER_VERSION.to_string(),
        });
    }
    if !handshake.capabilities.populates_remote_url {
        return Err(BootstrapError::MissingCapability {
            name: "populates_remote_url",
        });
    }
    if !handshake.capabilities.scans_roots {
        return Err(BootstrapError::MissingCapability {
            name: "scans_roots",
        });
    }
    if !handshake.capabilities.populates_recent_commits {
        return Err(BootstrapError::MissingCapability {
            name: "populates_recent_commits",
        });
    }
    if !handshake.capabilities.discovers_roots {
        return Err(BootstrapError::MissingCapability {
            name: "discovers_roots",
        });
    }
    if !handshake.capabilities.reports_worktrees {
        return Err(BootstrapError::MissingCapability {
            name: "reports_worktrees",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wsl::executor::{RunOutput, WslError};
    use async_trait::async_trait;
    use git_clawler_wsl_helper::protocol::HelperCapabilities;
    use std::sync::Mutex;

    // ── Mock runner ─────────────────────────────────────────────────────────

    /// A mock [`WslCommandRunner`] that returns a pre-configured sequence of
    /// [`RunOutput`]s (one per `run()` call in order).
    struct MockRunner {
        responses: Mutex<Vec<RunOutput>>,
    }

    impl MockRunner {
        fn new(responses: Vec<RunOutput>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses),
            })
        }
    }

    #[async_trait]
    impl WslCommandRunner for MockRunner {
        async fn run(&self, _args: &[String]) -> Result<RunOutput, WslError> {
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(WslError::NonZeroExit {
                    status: Some(1),
                    stderr: "mock: no more responses".into(),
                });
            }
            Ok(q.remove(0))
        }
    }

    fn probe_not_found_output() -> RunOutput {
        RunOutput {
            status: Some(PROBE_NOT_FOUND_EXIT),
            stdout: vec![],
            stderr: vec![],
        }
    }

    fn probe_found_output(path: &str) -> RunOutput {
        RunOutput {
            status: Some(0),
            stdout: path.as_bytes().to_vec(),
            stderr: vec![],
        }
    }

    fn handshake_output(caps: &HelperCapabilities) -> RunOutput {
        let hs = HelperHandshake {
            protocol_version: git_clawler_wsl_helper::PROTOCOL_VERSION,
            helper_version: HELPER_VERSION.to_string(),
            capabilities: caps.clone(),
        };
        RunOutput {
            status: Some(0),
            stdout: serde_json::to_vec(&hs).unwrap(),
            stderr: vec![],
        }
    }

    fn install_output(path: &str) -> RunOutput {
        RunOutput {
            status: Some(0),
            stdout: format!("{}\n", path).into_bytes(),
            stderr: vec![],
        }
    }

    fn make_input() -> BootstrapInput {
        BootstrapInput {
            distro: Some("Ubuntu".into()),
            resource_helper_path: std::path::PathBuf::from("/mnt/c/resources/helper"),
        }
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"helper").unwrap();
    }

    // ── Existing unit tests (preserved) ─────────────────────────────────────

    #[test]
    fn resolve_resource_helper_path_uses_packaged_resource_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let resource_dir = dir.path().join("resources");
        let helper = resource_dir
            .join("wsl-helper")
            .join(HELPER_TARGET_TRIPLE)
            .join(HELPER_BINARY_NAME);
        touch(&helper);

        assert_eq!(resolve_resource_helper_path(&resource_dir), helper);
    }

    #[test]
    fn resolve_resource_helper_path_uses_executable_adjacent_packaged_resources() {
        let tmp = tempfile::tempdir().unwrap();
        let resource_dir = tmp.path().join("Program Files").join("git-clawler");
        let helper = resource_dir
            .join("resources")
            .join("wsl-helper")
            .join(HELPER_TARGET_TRIPLE)
            .join(HELPER_BINARY_NAME);
        std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
        std::fs::write(&helper, b"packaged helper").unwrap();
        assert_eq!(resolve_resource_helper_path(&resource_dir), helper);
    }

    #[test]
    fn resolve_resource_helper_path_uses_development_target_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let resource_dir = workspace.join("target").join("debug");
        let helper = workspace
            .join("target")
            .join(HELPER_TARGET_TRIPLE)
            .join("debug")
            .join(HELPER_BINARY_NAME);
        touch(&helper);

        assert_eq!(resolve_resource_helper_path(&resource_dir), helper);
    }

    #[test]
    fn resolve_resource_helper_path_uses_staged_dev_resource_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let resource_dir = workspace.join("target").join("debug");
        let helper = resource_dir
            .join("resources")
            .join("wsl-helper")
            .join(HELPER_TARGET_TRIPLE)
            .join(HELPER_BINARY_NAME);
        touch(&helper);

        assert_eq!(resolve_resource_helper_path(&resource_dir), helper);
    }

    #[test]
    fn resolve_resource_helper_path_prefers_staged_dev_resource_over_unstaged_profile_resource() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let resource_dir = workspace.join("target").join("debug");
        let unstaged_profile_helper = resource_dir
            .join("wsl-helper")
            .join(HELPER_TARGET_TRIPLE)
            .join(HELPER_BINARY_NAME);
        let staged_helper = resource_dir
            .join("resources")
            .join("wsl-helper")
            .join(HELPER_TARGET_TRIPLE)
            .join(HELPER_BINARY_NAME);
        touch(&unstaged_profile_helper);
        touch(&staged_helper);

        assert_eq!(resolve_resource_helper_path(&resource_dir), staged_helper);
    }

    #[test]
    fn resolve_resource_helper_path_ignores_unstaged_profile_resource_when_staged_missing() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let resource_dir = workspace.join("target").join("debug");
        let unstaged_profile_helper = resource_dir
            .join("wsl-helper")
            .join(HELPER_TARGET_TRIPLE)
            .join(HELPER_BINARY_NAME);
        let staged_helper = resource_dir
            .join("resources")
            .join("wsl-helper")
            .join(HELPER_TARGET_TRIPLE)
            .join(HELPER_BINARY_NAME);
        touch(&unstaged_profile_helper);

        assert_eq!(resolve_resource_helper_path(&resource_dir), staged_helper);
    }

    #[test]
    fn resolve_resource_helper_path_returns_staged_dev_resource_when_no_candidate_exists() {
        let dir = tempfile::tempdir().unwrap();
        let resource_dir = dir.path().join("workspace").join("target").join("debug");
        let expected = resource_dir
            .join("resources")
            .join("wsl-helper")
            .join(HELPER_TARGET_TRIPLE)
            .join(HELPER_BINARY_NAME);

        assert_eq!(resolve_resource_helper_path(&resource_dir), expected);
    }

    #[test]
    fn build_install_args_with_distro() {
        let args = build_install_args(Some("Ubuntu"), "/mnt/c/helper");
        assert_eq!(args[0], "-d");
        assert_eq!(args[1], "Ubuntu");
        assert_eq!(args[2], "--exec");
        assert_eq!(args[3], "bash");
        assert_eq!(args[4], "-lc");
        assert!(args[5].contains("wslpath"));
        assert_eq!(args[6], "_");
        assert_eq!(args[7], "/mnt/c/helper");
    }

    #[test]
    fn build_install_args_without_distro() {
        let args = build_install_args(None, "/mnt/c/helper");
        assert_eq!(args[0], "--exec");
        assert_eq!(args[1], "bash");
    }

    #[test]
    fn version_compatible_same_major_minor() {
        assert!(version_compatible("0.0.1", "0.0.1"));
        assert!(version_compatible("1.2.3", "1.2.99"));
    }

    #[test]
    fn version_compatible_minor_differs() {
        assert!(!version_compatible("1.2.3", "1.3.0"));
    }

    #[test]
    fn version_compatible_major_differs() {
        assert!(!version_compatible("1.0.0", "2.0.0"));
    }

    #[test]
    fn version_compatible_non_semver() {
        assert!(!version_compatible("not-a-version", "1.0.0"));
    }

    #[test]
    fn version_compatible_old_patch_vs_new_minor() {
        // 0.0.x is incompatible with 0.1.x (minor differs)
        assert!(!version_compatible("0.0.1", "0.1.0"));
    }

    #[test]
    fn build_probe_args_with_distro() {
        let args = build_probe_args(Some("Ubuntu"));
        assert_eq!(args[0], "-d");
        assert_eq!(args[1], "Ubuntu");
        assert_eq!(args[2], "--exec");
        assert_eq!(args[3], "bash");
        assert_eq!(args[4], "-lc");
        assert!(args[5].contains("$HOME/.local/bin/git-clawler-wsl-helper"));
        assert!(args[5].contains(&format!("exit {PROBE_NOT_FOUND_EXIT}")));
    }

    #[test]
    fn build_probe_args_without_distro() {
        let args = build_probe_args(None);
        assert_eq!(args[0], "--exec");
        assert_eq!(args[1], "bash");
        assert_eq!(args[2], "-lc");
    }

    // ── Capability-gated reinstall tests ────────────────────────────────────

    /// An already-installed helper with a matching version AND `populates_remote_url = true`
    /// must be reused without triggering a reinstall.
    #[tokio::test]
    async fn bootstrap_skips_when_version_and_capability_present() {
        let _env = lock_helper_env().await;
        let caps = HelperCapabilities {
            discovers_roots: true,
            reports_worktrees: true,
            populates_remote_url: true,
            scans_roots: true,
            populates_recent_commits: true,
        };
        // Call sequence: probe → handshake (both version-compatible + capable)
        let runner = MockRunner::new(vec![
            probe_found_output("/home/user/.local/bin/git-clawler-wsl-helper"),
            handshake_output(&caps),
        ]);
        let registry = WslHelperRegistry::new(runner);
        let result = registry.ensure(make_input()).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert_eq!(
            result.unwrap(),
            "/home/user/.local/bin/git-clawler-wsl-helper"
        );
    }

    /// An already-installed helper that is version-compatible but has
    /// `populates_remote_url = false` must trigger a reinstall.
    #[tokio::test]
    async fn bootstrap_reinstalls_when_capability_missing() {
        let _env = lock_helper_env().await;
        let caps_old = HelperCapabilities {
            discovers_roots: true,
            reports_worktrees: true,
            populates_remote_url: false,
            scans_roots: false,
            populates_recent_commits: false,
        };
        let caps_new = HelperCapabilities {
            discovers_roots: true,
            reports_worktrees: true,
            populates_remote_url: true,
            scans_roots: true,
            populates_recent_commits: true,
        };
        // Call sequence:
        // 1. probe → found at existing path
        // 2. handshake → version OK but capability false  (triggers reinstall)
        // 3. install → returns new path
        // 4. post-install handshake → version OK, capability true
        let runner = MockRunner::new(vec![
            probe_found_output("/home/user/.local/bin/git-clawler-wsl-helper"),
            handshake_output(&caps_old),
            install_output("/home/user/.local/bin/git-clawler-wsl-helper"),
            handshake_output(&caps_new),
        ]);
        let registry = WslHelperRegistry::new(runner);
        let result = registry.ensure(make_input()).await;
        assert!(
            result.is_ok(),
            "expected Ok after reinstall, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn bootstrap_reinstalls_when_scan_root_capability_missing() {
        let _env = lock_helper_env().await;
        let caps_old = HelperCapabilities {
            discovers_roots: true,
            reports_worktrees: true,
            populates_remote_url: true,
            scans_roots: false,
            populates_recent_commits: false,
        };
        let caps_new = HelperCapabilities {
            discovers_roots: true,
            reports_worktrees: true,
            populates_remote_url: true,
            scans_roots: true,
            populates_recent_commits: true,
        };
        let runner = MockRunner::new(vec![
            probe_found_output("/home/user/.local/bin/git-clawler-wsl-helper"),
            handshake_output(&caps_old),
            install_output("/home/user/.local/bin/git-clawler-wsl-helper"),
            handshake_output(&caps_new),
        ]);
        let registry = WslHelperRegistry::new(runner);
        let result = registry.ensure(make_input()).await;
        assert!(
            result.is_ok(),
            "expected Ok after reinstall, got: {:?}",
            result
        );
    }

    /// If the post-install handshake still returns `populates_remote_url = false`
    /// (deploy inconsistency), bootstrap must return `MissingCapability`.
    #[tokio::test]
    async fn bootstrap_errors_when_post_install_capability_missing() {
        let _env = lock_helper_env().await;
        let caps_missing = HelperCapabilities {
            discovers_roots: true,
            reports_worktrees: true,
            populates_remote_url: false,
            scans_roots: false,
            populates_recent_commits: false,
        };
        // Call sequence:
        // 1. probe → not installed
        // 2. install → returns path
        // 3. post-install handshake → capability still false
        let runner = MockRunner::new(vec![
            probe_not_found_output(),
            install_output("/home/user/.local/bin/git-clawler-wsl-helper"),
            handshake_output(&caps_missing),
        ]);
        let registry = WslHelperRegistry::new(runner);
        let result = registry.ensure(make_input()).await;
        assert!(
            matches!(
                result,
                Err(BootstrapError::MissingCapability {
                    name: "populates_remote_url"
                })
            ),
            "expected MissingCapability error, got: {:?}",
            result
        );
    }

    // ── env override tests ──────────────────────────────────────────────────

    // Env-var manipulation must be serialised to avoid test interference when
    // running with the default parallel test executor.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn lock_helper_env() -> tokio::sync::MutexGuard<'static, ()> {
        let guard = ENV_LOCK.lock().await;
        // SAFETY: test-only, serialised by ENV_LOCK.
        unsafe { std::env::remove_var("GIT_CLAWLER_WSL_HELPER") };
        guard
    }

    fn valid_caps() -> HelperCapabilities {
        HelperCapabilities {
            discovers_roots: true,
            reports_worktrees: true,
            populates_remote_url: true,
            scans_roots: true,
            populates_recent_commits: true,
        }
    }

    fn incompatible_version_handshake_output() -> RunOutput {
        let hs = HelperHandshake {
            protocol_version: git_clawler_wsl_helper::PROTOCOL_VERSION,
            // Use a clearly different major.minor to fail version_compatible.
            helper_version: "99.99.0".to_string(),
            capabilities: valid_caps(),
        };
        RunOutput {
            status: Some(0),
            stdout: serde_json::to_vec(&hs).unwrap(),
            stderr: vec![],
        }
    }

    fn wrong_protocol_handshake_output() -> RunOutput {
        let hs = HelperHandshake {
            protocol_version: git_clawler_wsl_helper::PROTOCOL_VERSION.wrapping_add(1),
            helper_version: HELPER_VERSION.to_string(),
            capabilities: valid_caps(),
        };
        RunOutput {
            status: Some(0),
            stdout: serde_json::to_vec(&hs).unwrap(),
            stderr: vec![],
        }
    }

    fn missing_capability_handshake_output() -> RunOutput {
        let hs = HelperHandshake {
            protocol_version: git_clawler_wsl_helper::PROTOCOL_VERSION,
            helper_version: HELPER_VERSION.to_string(),
            capabilities: HelperCapabilities {
                discovers_roots: true,
                reports_worktrees: true,
                populates_remote_url: false,
                scans_roots: false,
                populates_recent_commits: false,
            },
        };
        RunOutput {
            status: Some(0),
            stdout: serde_json::to_vec(&hs).unwrap(),
            stderr: vec![],
        }
    }

    #[tokio::test]
    async fn bootstrap_reinstalls_wrong_protocol_before_caching() {
        let _env = lock_helper_env().await;
        let path = "/home/user/.local/bin/git-clawler-wsl-helper";
        let runner = MockRunner::new(vec![
            probe_found_output(path),
            wrong_protocol_handshake_output(),
            install_output(path),
            handshake_output(&valid_caps()),
        ]);
        let registry = WslHelperRegistry::new(runner);
        assert_eq!(registry.ensure(make_input()).await.unwrap(), path);
        // A cached success must only be reached after the validated reinstall.
        assert_eq!(registry.ensure(make_input()).await.unwrap(), path);
    }

    /// env override with valid handshake → returns the overridden path.
    #[tokio::test]
    async fn env_override_valid_handshake_returns_path() {
        let _lock = lock_helper_env().await;
        // SAFETY: test-only, serialised by ENV_LOCK.
        unsafe { std::env::set_var("GIT_CLAWLER_WSL_HELPER", "/custom/helper") };
        let runner = MockRunner::new(vec![handshake_output(&valid_caps())]);
        let registry = WslHelperRegistry::new(runner);
        let result = registry.ensure(make_input()).await;
        unsafe { std::env::remove_var("GIT_CLAWLER_WSL_HELPER") };
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert_eq!(result.unwrap(), "/custom/helper");
    }

    /// env override with protocol mismatch → ProtocolMismatch error.
    #[tokio::test]
    async fn env_override_protocol_mismatch_returns_error() {
        let _lock = lock_helper_env().await;
        unsafe { std::env::set_var("GIT_CLAWLER_WSL_HELPER", "/custom/helper") };
        let runner = MockRunner::new(vec![wrong_protocol_handshake_output()]);
        let registry = WslHelperRegistry::new(runner);
        let result = registry.ensure(make_input()).await;
        unsafe { std::env::remove_var("GIT_CLAWLER_WSL_HELPER") };
        assert!(
            matches!(result, Err(BootstrapError::ProtocolMismatch { .. })),
            "expected ProtocolMismatch, got: {:?}",
            result
        );
    }

    /// env override with version mismatch → VersionMismatch error.
    #[tokio::test]
    async fn env_override_version_mismatch_returns_error() {
        let _lock = lock_helper_env().await;
        unsafe { std::env::set_var("GIT_CLAWLER_WSL_HELPER", "/custom/helper") };
        let runner = MockRunner::new(vec![incompatible_version_handshake_output()]);
        let registry = WslHelperRegistry::new(runner);
        let result = registry.ensure(make_input()).await;
        unsafe { std::env::remove_var("GIT_CLAWLER_WSL_HELPER") };
        assert!(
            matches!(result, Err(BootstrapError::VersionMismatch { .. })),
            "expected VersionMismatch, got: {:?}",
            result
        );
    }

    /// env override with missing capability → MissingCapability error.
    #[tokio::test]
    async fn env_override_missing_capability_returns_error() {
        let _lock = lock_helper_env().await;
        unsafe { std::env::set_var("GIT_CLAWLER_WSL_HELPER", "/custom/helper") };
        let runner = MockRunner::new(vec![missing_capability_handshake_output()]);
        let registry = WslHelperRegistry::new(runner);
        let result = registry.ensure(make_input()).await;
        unsafe { std::env::remove_var("GIT_CLAWLER_WSL_HELPER") };
        assert!(
            matches!(
                result,
                Err(BootstrapError::MissingCapability {
                    name: "populates_remote_url"
                })
            ),
            "expected MissingCapability, got: {:?}",
            result
        );
    }

    /// env override with handshake parse failure → HandshakeParse error.
    #[tokio::test]
    async fn env_override_handshake_parse_failure_returns_error() {
        let _lock = lock_helper_env().await;
        unsafe { std::env::set_var("GIT_CLAWLER_WSL_HELPER", "/custom/helper") };
        let runner = MockRunner::new(vec![RunOutput {
            status: Some(0),
            stdout: b"not-valid-json{{{".to_vec(),
            stderr: vec![],
        }]);
        let registry = WslHelperRegistry::new(runner);
        let result = registry.ensure(make_input()).await;
        unsafe { std::env::remove_var("GIT_CLAWLER_WSL_HELPER") };
        assert!(
            matches!(result, Err(BootstrapError::HandshakeParse(_))),
            "expected HandshakeParse, got: {:?}",
            result
        );
    }
}
