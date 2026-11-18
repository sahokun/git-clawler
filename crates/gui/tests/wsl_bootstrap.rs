//! Integration tests for WslHelperRegistry (bootstrap module).
//!
//! These tests run on Linux (no Windows API required) using a MockRunner
//! that simulates wsl.exe responses.

#![cfg_attr(not(target_os = "windows"), allow(unused_imports))]

use git_clawler_gui::wsl::bootstrap::{BootstrapInput, WslHelperRegistry};
use git_clawler_gui::wsl::executor::{RunOutput, WslCommandRunner, WslError};
use git_clawler_wsl_helper::HELPER_VERSION;
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};

static HELPER_ENV_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

async fn lock_helper_env() -> AsyncMutexGuard<'static, ()> {
    let guard = HELPER_ENV_LOCK.lock().await;
    std::env::remove_var("GIT_CLAWLER_WSL_HELPER");
    guard
}

// ── Mock runner ──────────────────────────────────────────────────────────────

struct MockRunner {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    responses: Arc<Mutex<std::collections::VecDeque<Result<RunOutput, WslError>>>>,
}

impl MockRunner {
    fn new(responses: Vec<Result<RunOutput, WslError>>) -> (Self, Arc<Mutex<Vec<Vec<String>>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = MockRunner {
            calls: calls.clone(),
            responses: Arc::new(Mutex::new(responses.into())),
        };
        (runner, calls)
    }
}

#[async_trait::async_trait]
impl WslCommandRunner for MockRunner {
    async fn run(&self, args: &[String]) -> Result<RunOutput, WslError> {
        self.calls.lock().unwrap().push(args.to_vec());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(WslError::Spawn(std::io::Error::other(
                "no more responses",
            ))))
    }
}

// ── Helper response builders ─────────────────────────────────────────────────

fn handshake_response(protocol_version: u32, helper_version: &str) -> RunOutput {
    RunOutput {
        status: Some(0),
        stdout: format!(
            r#"{{"protocol_version":{protocol_version},"helper_version":"{helper_version}","capabilities":{{"populates_remote_url":true,"scans_roots":true,"populates_recent_commits":true,"discovers_roots":true,"reports_worktrees":true}}}}"#
        )
        .into_bytes(),
        stderr: vec![],
    }
}

fn install_response(installed_path: &str) -> RunOutput {
    RunOutput {
        status: Some(0),
        stdout: format!("{installed_path}\n").into_bytes(),
        stderr: vec![],
    }
}

/// Successful probe response: `bash -lc` printed the resolved helper path.
fn probe_ok_response(path: &str) -> RunOutput {
    RunOutput {
        status: Some(0),
        stdout: path.as_bytes().to_vec(),
        stderr: vec![],
    }
}

/// Probe failure: helper missing → exit 42.
fn probe_missing_response() -> RunOutput {
    RunOutput {
        status: Some(42),
        stdout: vec![],
        stderr: vec![],
    }
}

/// Probe failure: unexpected non-zero exit (e.g. permission denied).
fn probe_unexpected_failure_response(status: i32, stderr: &str) -> RunOutput {
    RunOutput {
        status: Some(status),
        stdout: vec![],
        stderr: stderr.as_bytes().to_vec(),
    }
}

// ── BootstrapInput builder ────────────────────────────────────────────────────

fn test_input(distro: Option<&str>) -> BootstrapInput {
    BootstrapInput {
        distro: distro.map(String::from),
        resource_helper_path: std::path::PathBuf::from(
            "/fake/resources/wsl-helper/x86_64-unknown-linux-musl/git-clawler-wsl-helper",
        ),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// T-BS-01: install command contains wslpath + cp + chmod + mv script
#[test]
fn install_command_shape() {
    let args = git_clawler_gui::wsl::bootstrap::build_install_args(
        Some("Ubuntu"),
        "/mnt/c/Program Files/git-clawler/resources/wsl-helper/x86_64-unknown-linux-musl/git-clawler-wsl-helper",
    );
    assert_eq!(args[0], "-d");
    assert_eq!(args[1], "Ubuntu");
    assert_eq!(args[2], "--exec");
    assert_eq!(args[3], "bash");
    assert_eq!(args[4], "-lc");
    let script = &args[5];
    assert!(script.contains("wslpath"), "script must call wslpath");
    assert!(
        script.contains(".local/bin/git-clawler-wsl-helper"),
        "script must set dst"
    );
    assert!(
        script.contains("mktemp"),
        "script must use mktemp for atomic replace"
    );
    assert!(
        script.contains("chmod 0755"),
        "script must set executable bit"
    );
    assert!(script.contains("mv -f"), "script must do atomic mv");
    assert_eq!(args[6], "_");
    assert_eq!(
        args[7],
        "/mnt/c/Program Files/git-clawler/resources/wsl-helper/x86_64-unknown-linux-musl/git-clawler-wsl-helper"
    );
}

/// T-BS-02: install command without distro omits -d flag
#[test]
fn install_command_no_distro() {
    let args = git_clawler_gui::wsl::bootstrap::build_install_args(None, "/fake/path/helper");
    assert_eq!(args[0], "--exec");
    assert_eq!(args[1], "bash");
}

/// T-BS-03: handshake response parses to HelperHandshake DTO
#[test]
fn handshake_parse_ok() {
    use git_clawler_wsl_helper::protocol::HelperHandshake;
    let json = r#"{"protocol_version":1,"helper_version":"0.0.1"}"#;
    let h: HelperHandshake = serde_json::from_str(json).unwrap();
    assert_eq!(h.protocol_version, 1);
    assert_eq!(h.helper_version, "0.0.1");
}

/// T-BS-04: handshake parse fails on malformed JSON
#[test]
fn handshake_parse_invalid() {
    use git_clawler_wsl_helper::protocol::HelperHandshake;
    let json = r#"not json"#;
    assert!(serde_json::from_str::<HelperHandshake>(json).is_err());
}

/// T-BS-05: version_compatible returns true when major.minor match
#[test]
fn version_compatible_patch_ignored() {
    assert!(git_clawler_gui::wsl::bootstrap::version_compatible(
        "1.2.3", "1.2.99"
    ));
    assert!(git_clawler_gui::wsl::bootstrap::version_compatible(
        "0.0.1", "0.0.99"
    ));
}

/// T-BS-06: version_compatible returns false when minor differs
#[test]
fn version_compatible_minor_differs() {
    assert!(!git_clawler_gui::wsl::bootstrap::version_compatible(
        "1.2.3", "1.3.0"
    ));
}

/// T-BS-07: version_compatible returns false when major differs
#[test]
fn version_compatible_major_differs() {
    assert!(!git_clawler_gui::wsl::bootstrap::version_compatible(
        "1.0.0", "2.0.0"
    ));
}

/// T-BS-08: env override validates helper handshake and skips install/probe.
#[tokio::test]
async fn env_override_validates_handshake() {
    let _env = lock_helper_env().await;
    let (runner, calls) = MockRunner::new(vec![Ok(handshake_response(1, HELPER_VERSION))]);
    let registry = WslHelperRegistry::new(Arc::new(runner));

    std::env::set_var("GIT_CLAWLER_WSL_HELPER", "/custom/helper");
    let result = registry.ensure(test_input(Some("Ubuntu"))).await;
    std::env::remove_var("GIT_CLAWLER_WSL_HELPER");

    assert!(result.is_ok(), "env override should succeed: {result:?}");
    let path = result.unwrap();
    assert_eq!(path, "/custom/helper");
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "runner must be called once to validate env override"
    );
    let call = &calls.lock().unwrap()[0];
    assert_eq!(call[0], "-d");
    assert_eq!(call[1], "Ubuntu");
    assert_eq!(call[2], "--exec");
    assert_eq!(call[3], "/custom/helper");
    assert_eq!(call[4], "--protocol");
}

/// T-BS-09: fresh install path — probe fails (not installed), then install, then handshake succeeds
#[tokio::test]
async fn fresh_install_installs_and_caches() {
    let _env = lock_helper_env().await;
    let (runner, calls) = MockRunner::new(vec![
        Ok(probe_missing_response()),
        Ok(install_response(
            "/home/user/.local/bin/git-clawler-wsl-helper",
        )),
        Ok(handshake_response(1, HELPER_VERSION)),
    ]);
    let registry = WslHelperRegistry::new(Arc::new(runner));

    let result = registry.ensure(test_input(Some("Ubuntu"))).await;
    assert!(result.is_ok(), "fresh install should succeed: {result:?}");
    let path = result.unwrap();
    assert_eq!(path, "/home/user/.local/bin/git-clawler-wsl-helper");

    let n = calls.lock().unwrap().len();
    assert_eq!(n, 3, "expected 3 runner calls, got {n}");

    // First call must be the bash probe (HIGH #3 fix): regular users live
    // outside `/root/.local/bin` so we resolve `$HOME` dynamically.
    let first = &calls.lock().unwrap()[0];
    assert_eq!(first[0], "-d");
    assert_eq!(first[1], "Ubuntu");
    assert_eq!(first[3], "bash");
    assert_eq!(first[4], "-lc");
    assert!(first[5].contains("$HOME/.local/bin/git-clawler-wsl-helper"));
}

/// T-BS-10: already-installed and compatible version — no reinstall
#[tokio::test]
async fn already_installed_no_reinstall() {
    let _env = lock_helper_env().await;
    let (runner, calls) = MockRunner::new(vec![
        Ok(probe_ok_response(
            "/home/ubuntu/.local/bin/git-clawler-wsl-helper",
        )),
        Ok(handshake_response(1, HELPER_VERSION)),
    ]);
    let registry = WslHelperRegistry::new(Arc::new(runner));

    let result = registry.ensure(test_input(Some("Ubuntu"))).await;
    assert!(
        result.is_ok(),
        "already-installed should succeed: {result:?}"
    );
    assert_eq!(
        result.unwrap(),
        "/home/ubuntu/.local/bin/git-clawler-wsl-helper",
        "must return the path resolved from the probe (regular user, not /root)"
    );

    let n = calls.lock().unwrap().len();
    assert_eq!(n, 2, "expected 2 runner calls (probe + handshake), got {n}");
}

/// T-BS-10b: probe success → handshake against the *probed* path (HIGH #3).
/// Confirms the second call uses the path returned from the probe instead of
/// the obsolete hard-coded `/root/.local/bin/...`.
#[tokio::test]
async fn probe_success_uses_resolved_path_for_handshake() {
    let _env = lock_helper_env().await;
    let resolved = "/home/ubuntu/.local/bin/git-clawler-wsl-helper";
    let (runner, calls) = MockRunner::new(vec![
        Ok(probe_ok_response(resolved)),
        Ok(handshake_response(1, HELPER_VERSION)),
    ]);
    let registry = WslHelperRegistry::new(Arc::new(runner));

    let result = registry.ensure(test_input(Some("Ubuntu"))).await.unwrap();
    assert_eq!(result, resolved);

    let calls_guard = calls.lock().unwrap();
    assert_eq!(calls_guard.len(), 2);
    let handshake_args = &calls_guard[1];
    // ["-d", "Ubuntu", "--exec", <helper_path>, "--protocol"]
    assert_eq!(handshake_args[3], resolved);
    assert_eq!(handshake_args[4], "--protocol");
}

/// T-BS-10c: probe exit 42 (helper missing) → install path runs.
#[tokio::test]
async fn probe_missing_triggers_install() {
    let _env = lock_helper_env().await;
    let (runner, calls) = MockRunner::new(vec![
        Ok(probe_missing_response()),
        Ok(install_response(
            "/home/ubuntu/.local/bin/git-clawler-wsl-helper",
        )),
        Ok(handshake_response(1, HELPER_VERSION)),
    ]);
    let registry = WslHelperRegistry::new(Arc::new(runner));

    let result = registry.ensure(test_input(Some("Ubuntu"))).await.unwrap();
    assert_eq!(result, "/home/ubuntu/.local/bin/git-clawler-wsl-helper");

    let calls_guard = calls.lock().unwrap();
    assert_eq!(calls_guard.len(), 3);
    // Second call must be the install script.
    assert!(calls_guard[1].iter().any(|a| a.contains("wslpath")));
}

/// T-BS-11: concurrent ensure for same distro installs only once (OnceCell semantics)
#[tokio::test]
async fn concurrent_ensure_installs_once() {
    let _env = lock_helper_env().await;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    let install_count = StdArc::new(AtomicUsize::new(0));

    struct CountingRunner {
        count: StdArc<AtomicUsize>,
        calls: Arc<Mutex<Vec<Vec<String>>>>,
    }

    #[async_trait::async_trait]
    impl WslCommandRunner for CountingRunner {
        async fn run(&self, args: &[String]) -> Result<RunOutput, WslError> {
            self.calls.lock().unwrap().push(args.to_vec());
            let n = self.count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // Probe: helper missing.
                Ok(probe_missing_response())
            } else if n == 1 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                Ok(install_response(
                    "/home/user/.local/bin/git-clawler-wsl-helper",
                ))
            } else {
                Ok(handshake_response(1, HELPER_VERSION))
            }
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let runner = CountingRunner {
        count: install_count.clone(),
        calls: calls.clone(),
    };
    let registry = Arc::new(WslHelperRegistry::new(Arc::new(runner)));

    let mut handles = vec![];
    for _ in 0..5 {
        let r = registry.clone();
        let inp = test_input(Some("Ubuntu"));
        handles.push(tokio::spawn(async move { r.ensure(inp).await }));
    }

    let results: Vec<_> = futures::future::join_all(handles).await;
    for r in &results {
        let inner = r.as_ref().unwrap();
        assert!(inner.is_ok(), "all concurrent ensures should succeed");
    }

    let total_calls = calls.lock().unwrap().len();
    assert_eq!(
        total_calls, 3,
        "install should run exactly once, got {total_calls} calls"
    );
}

/// T-BS-12: different distros run independently (no cross-distro blocking)
#[tokio::test]
async fn different_distros_independent() {
    let _env = lock_helper_env().await;
    let (runner, calls) = MockRunner::new(vec![
        Ok(probe_ok_response(
            "/home/ubuntu/.local/bin/git-clawler-wsl-helper",
        )),
        Ok(handshake_response(1, HELPER_VERSION)),
        Ok(probe_ok_response(
            "/home/debian/.local/bin/git-clawler-wsl-helper",
        )),
        Ok(handshake_response(1, HELPER_VERSION)),
    ]);
    let registry = Arc::new(WslHelperRegistry::new(Arc::new(runner)));

    let r1 = registry.clone();
    let r2 = registry.clone();
    let (res1, res2) = tokio::join!(
        r1.ensure(test_input(Some("Ubuntu"))),
        r2.ensure(test_input(Some("Debian"))),
    );
    assert!(res1.is_ok());
    assert!(res2.is_ok());
    assert_eq!(calls.lock().unwrap().len(), 4);
}

/// T-BS-13: probe returns unexpected non-zero exit → hard error (no install)
#[tokio::test]
async fn probe_unexpected_exit_is_hard_error() {
    let _env = lock_helper_env().await;
    use git_clawler_gui::wsl::executor::WslError;

    let (runner, calls) = MockRunner::new(vec![Ok(probe_unexpected_failure_response(
        1,
        "permission denied",
    ))]);
    let registry = WslHelperRegistry::new(Arc::new(runner));

    let result = registry.ensure(test_input(Some("Ubuntu"))).await;
    assert!(
        result.is_err(),
        "unexpected probe exit should be a hard error"
    );
    match result.unwrap_err() {
        git_clawler_gui::wsl::bootstrap::BootstrapError::Runner(WslError::NonZeroExit {
            status: Some(1),
            ..
        }) => {}
        other => panic!("expected Runner(NonZeroExit {{ status: Some(1), .. }}), got {other:?}"),
    }
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "runner must be called exactly once (probe only)"
    );
}

/// T-BS-14: probe spawn error → hard error (no install)
#[tokio::test]
async fn probe_spawn_error_is_hard_error() {
    let _env = lock_helper_env().await;
    use git_clawler_gui::wsl::executor::WslError;

    let (runner, calls) = MockRunner::new(vec![Err(WslError::Spawn(std::io::Error::other(
        "wsl.exe missing",
    )))]);
    let registry = WslHelperRegistry::new(Arc::new(runner));

    let result = registry.ensure(test_input(Some("Ubuntu"))).await;
    assert!(result.is_err(), "spawn error should be a hard error");
    match result.unwrap_err() {
        git_clawler_gui::wsl::bootstrap::BootstrapError::Runner(WslError::Spawn(_)) => {}
        other => panic!("expected Runner(Spawn(_)), got {other:?}"),
    }
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "runner must be called exactly once (probe only)"
    );
}

/// T-BS-15: probe returns status=0 with empty stdout → EmptyProbePath hard error
#[tokio::test]
async fn probe_empty_stdout_is_hard_error() {
    let _env = lock_helper_env().await;
    let (runner, calls) = MockRunner::new(vec![Ok(RunOutput {
        status: Some(0),
        stdout: vec![],
        stderr: vec![],
    })]);
    let registry = WslHelperRegistry::new(Arc::new(runner));

    let result = registry.ensure(test_input(Some("Ubuntu"))).await;
    assert!(result.is_err(), "empty probe stdout should be a hard error");
    match result.unwrap_err() {
        git_clawler_gui::wsl::bootstrap::BootstrapError::EmptyProbePath => {}
        other => panic!("expected EmptyProbePath, got {other:?}"),
    }
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "runner must be called exactly once (probe only)"
    );
}

/// T-BS-16: version-compatible but capability missing → triggers reinstall
/// Simulates a previously-installed binary that predates `populates_remote_url`.
#[tokio::test]
async fn already_installed_missing_capability_triggers_reinstall() {
    let _env = lock_helper_env().await;

    // Old handshake: version compatible, but no capabilities field
    let old_handshake = RunOutput {
        status: Some(0),
        stdout: format!(r#"{{"protocol_version":1,"helper_version":"{HELPER_VERSION}"}}"#)
            .into_bytes(),
        stderr: vec![],
    };

    let (runner, calls) = MockRunner::new(vec![
        // probe → found
        Ok(probe_ok_response(
            "/home/user/.local/bin/git-clawler-wsl-helper",
        )),
        // handshake → version compatible but capability missing → reinstall
        Ok(old_handshake),
        // install
        Ok(install_response(
            "/home/user/.local/bin/git-clawler-wsl-helper",
        )),
        // post-install handshake → now capability present
        Ok(handshake_response(1, HELPER_VERSION)),
    ]);
    let registry = WslHelperRegistry::new(Arc::new(runner));

    let result = registry.ensure(test_input(Some("Ubuntu"))).await;
    assert!(
        result.is_ok(),
        "reinstall after missing capability should succeed: {result:?}"
    );
    assert_eq!(
        calls.lock().unwrap().len(),
        4,
        "expected 4 calls: probe + pre-handshake + install + post-handshake"
    );
}

/// T-BS-17: after fresh install, if post-install handshake still lacks capability
/// → MissingCapability error is returned.
#[tokio::test]
async fn post_install_missing_capability_is_error() {
    let _env = lock_helper_env().await;
    use git_clawler_gui::wsl::bootstrap::BootstrapError;

    let bad_handshake = RunOutput {
        status: Some(0),
        stdout: format!(
            r#"{{"protocol_version":1,"helper_version":"{HELPER_VERSION}","capabilities":{{"populates_remote_url":false}}}}"#
        )
        .into_bytes(),
        stderr: vec![],
    };

    let (runner, _calls) = MockRunner::new(vec![
        Ok(probe_missing_response()),
        Ok(install_response(
            "/home/user/.local/bin/git-clawler-wsl-helper",
        )),
        Ok(bad_handshake),
    ]);
    let registry = WslHelperRegistry::new(Arc::new(runner));

    let result = registry.ensure(test_input(Some("Ubuntu"))).await;
    assert!(
        matches!(
            result,
            Err(BootstrapError::MissingCapability {
                name: "populates_remote_url"
            })
        ),
        "expected MissingCapability, got: {result:?}"
    );
}
