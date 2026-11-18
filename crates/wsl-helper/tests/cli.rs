use serde_json::Value;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git-clawler-wsl-helper");

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN).args(args).output().expect("spawn helper");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn t1_protocol_handshake() {
    let (code, stdout, _stderr) = run(&["--protocol"]);
    assert_eq!(code, 0);
    let trimmed = stdout.trim();
    assert_eq!(trimmed.lines().count(), 1, "stdout must be 1 JSON line");
    let v: Value = serde_json::from_str(trimmed).expect("valid JSON");
    assert_eq!(v["protocol_version"], 1);
    assert!(v["helper_version"].is_string());
    assert_eq!(v["capabilities"]["populates_recent_commits"], true);
}

#[test]
fn t2_no_args_yields_empty_results() {
    let (code, stdout, _stderr) = run(&[]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["protocol_version"], 1);
    assert!(v["results"].as_array().unwrap().is_empty());
}

#[test]
fn t3_relative_path_yields_relative_path_err() {
    let (code, stdout, _stderr) = run(&["relative/path"]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    let r = &v["results"][0];
    assert_eq!(r["status"]["kind"], "err");
    assert_eq!(r["status"]["value"]["code"], "relative_path");
}

#[test]
fn t4_nonexistent_dir_yields_path_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does-not-exist");
    let path_str = path.to_string_lossy().into_owned();
    let (code, stdout, _stderr) = run(&[&path_str]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["results"][0]["status"]["kind"], "err");
    assert_eq!(v["results"][0]["status"]["value"]["code"], "path_not_found");
}

#[test]
fn t5_valid_repo_yields_ok() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    let path_str = dir.path().to_string_lossy().into_owned();
    let (code, stdout, _stderr) = run(&[&path_str]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["results"][0]["status"]["kind"], "ok");
}

#[test]
fn t6_stdout_is_single_json_line() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    let path_str = dir.path().to_string_lossy().into_owned();
    let (_code, stdout, _stderr) = run(&[&path_str]);
    assert_eq!(stdout.trim().lines().count(), 1);
    serde_json::from_str::<Value>(stdout.trim()).expect("stdout is valid JSON");
}

#[test]
fn t7_mixed_paths_preserve_index_order() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    let ok_path = dir.path().to_string_lossy().into_owned();
    let (code, stdout, _stderr) = run(&[&ok_path, "rel/path"]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["index"], 0);
    assert_eq!(results[1]["index"], 1);
    assert_eq!(results[0]["status"]["kind"], "ok");
    assert_eq!(results[1]["status"]["kind"], "err");
    assert_eq!(results[1]["status"]["value"]["code"], "relative_path");
}

#[test]
fn t8_clap_error_yields_exit_2() {
    let (code, _stdout, _stderr) = run(&["--bogus"]);
    assert_eq!(code, 2);
}

#[test]
fn t9_version_flag_yields_exit_0_with_binary_name() {
    let (code, stdout, _stderr) = run(&["--version"]);
    assert_eq!(code, 0, "--version should exit 0");
    assert!(
        stdout.contains("git-clawler-wsl-helper"),
        "stdout should contain binary name, got: {stdout:?}"
    );
}

#[test]
fn t10_help_flag_yields_exit_0_with_usage() {
    let (code, stdout, _stderr) = run(&["--help"]);
    assert_eq!(code, 0, "--help should exit 0");
    assert!(
        stdout.contains("Usage"),
        "stdout should contain usage info, got: {stdout:?}"
    );
}

#[test]
fn t12_protocol_handshake_deserializes_to_dto() {
    use git_clawler_wsl_helper::protocol::HelperHandshake;
    let (code, stdout, _stderr) = run(&["--protocol"]);
    assert_eq!(code, 0);
    let trimmed = stdout.trim();
    let handshake: HelperHandshake = serde_json::from_str(trimmed)
        .expect("--protocol output must deserialize to HelperHandshake");
    assert_eq!(handshake.protocol_version, 1);
    assert!(!handshake.helper_version.is_empty());
    assert!(handshake.capabilities.scans_roots);
    assert!(handshake.capabilities.populates_recent_commits);
}

#[test]
fn t13_scan_root_discovers_repositories() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git2::Repository::init(&repo).unwrap();
    let root = dir.path().to_string_lossy().into_owned();

    let (code, stdout, _stderr) = run(&["--scan-root", &root, "--depth", "1"]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["protocol_version"], 1);
    assert_eq!(v["status"]["kind"], "ok");
    let paths = v["status"]["value"].as_array().unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].as_str().unwrap(), repo.to_string_lossy());
}

#[test]
fn t14_scan_root_cannot_combine_with_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().into_owned();
    let (code, _stdout, stderr) = run(&["--scan-root", &root, &root]);
    assert_eq!(code, 2);
    assert!(stderr.contains("mutually exclusive"));
}

#[test]
fn t15_discover_and_worktree_gone_modes() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git2::Repository::init(&repo).unwrap();
    let root = dir.path().to_string_lossy().into_owned();
    let (code, stdout, _stderr) = run(&["--discover", &root, "--depth", "1"]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["status"]["kind"], "ok");
    assert_eq!(
        v["status"]["value"][0]["path"],
        repo.to_string_lossy().as_ref()
    );

    let (code, _stdout, stderr) = run(&["--discover", &root, "--scan-root", &root]);
    assert_eq!(code, 2);
    assert!(stderr.contains("mutually exclusive"));

    let (code, _stdout, stderr) = run(&["--worktree-gone", "--worktree", "/x"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("matching --parent"));
}

#[test]
fn t16_handshake_advertises_discovery_capabilities() {
    let (code, stdout, _stderr) = run(&["--protocol"]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["capabilities"]["discovers_roots"], true);
    assert_eq!(v["capabilities"]["reports_worktrees"], true);
}
#[test]
fn t11_relative_path_emits_warning_to_stderr() {
    let (code, stdout, stderr) = Command::new(BIN)
        .args(["relative/path"])
        .env("RUST_LOG", "warn")
        .output()
        .map(|o| {
            (
                o.status.code().unwrap_or(-1),
                String::from_utf8(o.stdout).unwrap(),
                String::from_utf8(o.stderr).unwrap(),
            )
        })
        .expect("spawn helper");
    assert_eq!(code, 0);
    serde_json::from_str::<Value>(stdout.trim()).expect("stdout is valid JSON");
    assert!(
        !stderr.is_empty(),
        "tracing warning should be written to stderr for failed scan"
    );
}
