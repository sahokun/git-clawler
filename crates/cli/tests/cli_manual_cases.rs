//! CLI E2E tests for registration, status, configuration and errors.
//!
//! This is hermetic coverage for the automatable, non-interactive CLI scope.
//! It does not fully cover manual conditions that require a real TTY, real
//! GitHub API success, OS notifications, or GUI interaction.
//!
//! Covers: CLI-BUILD-005/006, CLI-SETUP-003/004/005/006,
//!         CLI-LIST-001..009, CLI-SCAN-001..004, CLI-ERR-001..004.
//!
//! ## Design constraints
//!
//! - Each test gets its own [`Sandbox`] backed by a [`tempfile::TempDir`].
//! - The CLI binary is isolated via `XDG_CONFIG_HOME` passed as `.env(...)` on
//!   every [`std::process::Command`] — `std::env::set_var` is never called.
//! - Git fixtures are built with the `git` CLI so the test binary stays
//!   dependency-free beyond the existing dev-dep `tempfile`.
//! - Assertions use **partial string matching** because comfy-table output
//!   width and border characters depend on the terminal width.

use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ============================================================================
// Sandbox helper
// ============================================================================

/// Isolated test environment.
///
/// Each field is a directory under a single [`TempDir`] root so they are all
/// cleaned up together when `Sandbox` is dropped.
struct Sandbox {
    /// Keeps the temp-dir alive; must not be renamed to `_`.
    _root: TempDir,
    /// Path used as `XDG_CONFIG_HOME` for every CLI invocation.
    config_home: PathBuf,
    /// Scratch workspace for creating git repos.
    workspace: PathBuf,
    /// Isolated GitHub CLI config directory.
    gh_config_dir: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let root = TempDir::new().expect("create TempDir");
        let config_home = root.path().join("config_home");
        let workspace = root.path().join("workspace");
        let gh_config_dir = root.path().join("gh-config");
        std::fs::create_dir_all(&config_home).expect("mkdir config_home");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        std::fs::create_dir_all(&gh_config_dir).expect("mkdir gh_config_dir");
        Self {
            _root: root,
            config_home,
            workspace,
            gh_config_dir,
        }
    }

    fn root(&self) -> &Path {
        self._root.path()
    }

    /// Path of the config file (`<config_home>/git-clawler/config.toml`).
    fn config_path(&self) -> PathBuf {
        self.config_home.join("git-clawler").join("config.toml")
    }

    /// Write `content` to the config file, creating parent dirs as needed.
    fn write_config(&self, content: &str) {
        let p = self.config_path();
        std::fs::create_dir_all(p.parent().unwrap()).expect("mkdir config dir");
        std::fs::write(&p, content).expect("write config");
    }

    /// Read the config file contents.
    fn read_config(&self) -> String {
        std::fs::read_to_string(self.config_path()).expect("read config")
    }
}

// ============================================================================
// CLI runner
// ============================================================================

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_gclawl")
}

struct CliOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Run the CLI binary with the given `args` and the sandbox's `XDG_CONFIG_HOME`.
fn run_cli(args: &[&str], sandbox: &Sandbox) -> CliOutput {
    run_cli_in(args, sandbox, sandbox.workspace.as_path())
}

/// Run the CLI binary with a custom `current_dir`.
fn run_cli_in(args: &[&str], sandbox: &Sandbox, cwd: &Path) -> CliOutput {
    let out = std::process::Command::new(bin_path())
        .args(args)
        .env("XDG_CONFIG_HOME", &sandbox.config_home)
        .env("GH_CONFIG_DIR", &sandbox.gh_config_dir)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("HOME", sandbox.root())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        // Suppress GITHUB_TOKEN so GitHub-dependent paths get a clear "no token"
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_ENTERPRISE_TOKEN")
        .env_remove("GH_ENTERPRISE_TOKEN")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_TEMPLATE_DIR")
        .env_remove("GIT_WORK_TREE")
        .current_dir(cwd)
        .output()
        .expect("failed to spawn gclawl");
    CliOutput {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// ============================================================================
// Git helpers
// ============================================================================

/// Run `git <args>` in `dir`, panic on failure with a helpful message.
fn git_in(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Manual Test")
        .env("GIT_AUTHOR_EMAIL", "manual-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Manual Test")
        .env("GIT_COMMITTER_EMAIL", "manual-test@example.invalid")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_TEMPLATE_DIR")
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git spawn failed: {e}"));
    if !out.status.success() {
        panic!(
            "git {} in {} failed\nstdout: {}\nstderr: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

/// Initialise a non-bare git repository at `path` with branch `main` and
/// local `user.name` / `user.email` set.
fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap_or_else(|e| panic!("mkdir {}: {e}", path.display()));

    // Try --initial-branch=main (git ≥ 2.28); fall back for older versions.
    let out = std::process::Command::new("git")
        .args(["init", "--initial-branch=main"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_TEMPLATE_DIR")
        .current_dir(path)
        .output()
        .expect("git init");
    if !out.status.success() {
        // Older git: init and rename default branch.
        git_in(path, &["init"]);
        // Rename if possible; ignore errors (branch may already be `main`).
        let _ = std::process::Command::new("git")
            .args(["checkout", "-b", "main"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_TEMPLATE_DIR")
            .current_dir(path)
            .output();
    }

    git_in(path, &["config", "user.name", "Manual Test"]);
    git_in(
        path,
        &["config", "user.email", "manual-test@example.invalid"],
    );
    configure_test_repo(path);
}

/// Initialise a **bare** repository at `path` with branch `main`.
fn init_bare_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap_or_else(|e| panic!("mkdir {}: {e}", path.display()));
    // Try with --initial-branch, fall back silently.
    let out = std::process::Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_TEMPLATE_DIR")
        .current_dir(path)
        .output()
        .expect("git init --bare");
    if !out.status.success() {
        git_in(path, &["init", "--bare"]);
        git_in(path, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    }
}

/// Clone `origin_url` into `dest`, configure local user.
fn clone_repo(origin_url: &str, dest: &Path) {
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let out = std::process::Command::new("git")
        .args(["clone", origin_url, dest.to_str().unwrap()])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Manual Test")
        .env("GIT_AUTHOR_EMAIL", "manual-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Manual Test")
        .env("GIT_COMMITTER_EMAIL", "manual-test@example.invalid")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_TEMPLATE_DIR")
        .output()
        .expect("git clone");
    if !out.status.success() {
        panic!(
            "git clone {} -> {} failed\nstdout: {}\nstderr: {}",
            origin_url,
            dest.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
    git_in(dest, &["config", "user.name", "Manual Test"]);
    git_in(
        dest,
        &["config", "user.email", "manual-test@example.invalid"],
    );
    configure_test_repo(dest);
}

/// Write `content` to `dir/<name>`.
fn write_file(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content)
        .unwrap_or_else(|e| panic!("write {}/{name}: {e}", dir.display()));
}

/// Render a path as a TOML basic string using only std.
fn toml_path(path: &Path) -> String {
    let escaped = path
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Find the first line in `output` that contains `needle`.
fn find_line_containing<'a>(output: &'a str, needle: &str) -> Option<&'a str> {
    output.lines().find(|line| line.contains(needle))
}

/// Configure repo-local settings to prevent GPG signing and hook interference.
fn configure_test_repo(path: &Path) {
    git_in(path, &["config", "commit.gpgsign", "false"]);
    git_in(path, &["config", "tag.gpgsign", "false"]);
    let hooks_dir = path.join(".git").join("hooks-disabled");
    std::fs::create_dir_all(&hooks_dir)
        .unwrap_or_else(|e| panic!("mkdir {}: {e}", hooks_dir.display()));
    let hooks_dir = hooks_dir.to_string_lossy().into_owned();
    git_in(path, &["config", "core.hooksPath", hooks_dir.as_str()]);
}

/// Stage all changes and create a commit in `dir`.
fn commit_all(dir: &Path, msg: &str) {
    git_in(dir, &["add", "--all"]);
    git_in(dir, &["commit", "--no-gpg-sign", "--no-verify", "-m", msg]);
}

// ============================================================================
// Tests: help / version  (CLI-BUILD-005, CLI-BUILD-006)
// ============================================================================

#[test]
fn help_and_version_are_available() {
    let sb = Sandbox::new();

    // Top-level --help
    let out = run_cli(&["--help"], &sb);
    assert!(out.success, "gclawl --help failed:\n{}", out.stderr);
    let txt = out.stdout.to_lowercase();
    assert!(txt.contains("list"), "--help must mention 'list'");
    assert!(txt.contains("scan"), "--help must mention 'scan'");
    assert!(txt.contains("remove"), "--help must mention 'remove'");
    assert!(txt.contains("watch"), "--help must mention 'watch'");
    assert!(txt.contains("tui"), "--help must mention 'tui'");

    // list --help
    let out = run_cli(&["list", "--help"], &sb);
    assert!(out.success, "list --help failed:\n{}", out.stderr);
    assert!(
        out.stdout.contains("--with-github"),
        "list --help must mention --with-github"
    );

    // scan --help
    let out = run_cli(&["scan", "--help"], &sb);
    assert!(out.success, "scan --help failed:\n{}", out.stderr);
    assert!(
        out.stdout.contains("--add"),
        "scan --help must mention --add"
    );
    assert!(
        out.stdout.contains("--depth"),
        "scan --help must mention --depth"
    );

    // watch --help
    let out = run_cli(&["watch", "--help"], &sb);
    assert!(out.success, "watch --help failed:\n{}", out.stderr);
    assert!(
        out.stdout.contains("--with-github"),
        "watch --help must mention --with-github"
    );

    // tui --help
    let out = run_cli(&["tui", "--help"], &sb);
    assert!(out.success, "tui --help failed:\n{}", out.stderr);
    assert!(
        out.stdout.contains("--with-github"),
        "tui --help must mention --with-github"
    );

    // --version
    let out = run_cli(&["--version"], &sb);
    assert!(out.success, "gclawl --version failed:\n{}", out.stderr);
    assert!(
        out.stdout.contains("gclawl"),
        "--version must contain 'gclawl', got: {}",
        out.stdout
    );
}

// ============================================================================
// Tests: remove
// ============================================================================

#[test]
fn remove_deletes_config_entry_without_deleting_directory() {
    let sb = Sandbox::new();
    let repo = sb.workspace.join("remove-me");
    init_repo(&repo);
    sb.write_config(&format!(
        "[[repositories]]\nname = \"remove-me\"\npath = {}\n",
        toml_path(&repo)
    ));

    let out = run_cli(&["remove", repo.to_str().unwrap()], &sb);

    assert!(
        out.success,
        "remove must succeed;\nstdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(repo.exists(), "repository directory must not be deleted");
    assert_eq!(
        out.stdout.trim(),
        format!("Removed from config: {}", repo.display())
    );
    let saved = sb.read_config();
    assert!(
        !saved.contains("[[repositories]]"),
        "repository entry should be removed, got: {saved}"
    );
}

#[test]
fn remove_unregistered_path_fails() {
    let sb = Sandbox::new();
    let repo = sb.workspace.join("registered");
    init_repo(&repo);
    let other = sb.workspace.join("other");
    std::fs::create_dir_all(&other).unwrap();
    sb.write_config(&format!(
        "[[repositories]]\nname = \"registered\"\npath = {}\n",
        toml_path(&repo)
    ));

    let out = run_cli(&["remove", other.to_str().unwrap()], &sb);

    assert!(!out.success, "remove should fail for unregistered path");
    assert!(
        out.stderr.contains("not registered"),
        "stderr should be clear, got: {}",
        out.stderr
    );
    assert!(
        sb.read_config().contains("registered"),
        "failed remove must not alter config"
    );
}

#[test]
fn remove_relative_stale_path_uses_current_directory() {
    let sb = Sandbox::new();
    let stale = sb.workspace.join("stale");
    sb.write_config(&format!(
        "[[repositories]]\nname = \"stale\"\npath = {}\n",
        toml_path(&stale)
    ));

    let out = run_cli_in(&["remove", "stale"], &sb, &sb.workspace);

    assert!(
        out.success,
        "relative stale remove must succeed;\nstdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(!sb.read_config().contains("[[repositories]]"));
}

#[test]
fn remove_ambiguous_path_fails_without_mutating_config() {
    let sb = Sandbox::new();
    let repo = PathBuf::from("/home/user/same");
    let config = format!(
        "[[repositories]]\nname = \"native\"\npath = {}\n\n[[repositories]]\nname = \"ubuntu\"\npath = {}\ndistro = \"Ubuntu\"\n",
        toml_path(&repo),
        toml_path(&repo)
    );
    sb.write_config(&config);

    let out = run_cli(&["remove", repo.to_str().unwrap()], &sb);

    assert!(!out.success, "ambiguous remove should fail");
    assert!(
        out.stderr.contains("ambiguous"),
        "stderr should mention ambiguity, got: {}",
        out.stderr
    );
    assert_eq!(
        sb.read_config(),
        config,
        "ambiguous remove must not mutate config"
    );
}

// ============================================================================
// Tests: scan  (CLI-SETUP-004/005/006, CLI-SCAN-001/002/003/004)
// ============================================================================

#[test]
fn scan_without_add_does_not_create_config() {
    let sb = Sandbox::new();
    let repo = sb.workspace.join("proj");
    init_repo(&repo);
    write_file(&repo, "README.md", "hello");
    commit_all(&repo, "initial");

    let out = run_cli(
        &["scan", sb.workspace.to_str().unwrap(), "--depth", "1"],
        &sb,
    );
    assert!(
        out.success,
        "scan without --add failed:\nstdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(
        out.stdout.contains("proj"),
        "scan must show repo name 'proj'; got:\n{}",
        out.stdout
    );
    assert!(
        !sb.config_path().exists(),
        "config must not be created when --add is not passed"
    );
}

#[test]
fn scan_with_add_registers_repositories_without_duplicates() {
    let sb = Sandbox::new();
    let repo = sb.workspace.join("my-repo");
    init_repo(&repo);
    write_file(&repo, "README.md", "hello");
    commit_all(&repo, "initial");

    // First scan --add
    let out = run_cli(
        &[
            "scan",
            sb.workspace.to_str().unwrap(),
            "--depth",
            "1",
            "--add",
        ],
        &sb,
    );
    assert!(
        out.success,
        "first scan --add failed:\nstdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(
        sb.config_path().exists(),
        "config must be created after --add"
    );

    let cfg = sb.read_config();
    let count1 = cfg.matches("[[repositories]]").count();
    assert_eq!(count1, 1, "expected 1 repository entry after first --add");

    // Second scan --add (duplicate)
    let out2 = run_cli(
        &[
            "scan",
            sb.workspace.to_str().unwrap(),
            "--depth",
            "1",
            "--add",
        ],
        &sb,
    );
    assert!(
        out2.success,
        "second scan --add failed:\nstdout: {}\nstderr: {}",
        out2.stdout, out2.stderr
    );
    assert!(
        out2.stdout.contains("[already registered]"),
        "second scan must show '[already registered]'; got:\n{}",
        out2.stdout
    );

    let cfg2 = sb.read_config();
    let count2 = cfg2.matches("[[repositories]]").count();
    assert_eq!(count2, 1, "no new entries must be added on duplicate scan");
}

#[test]
fn scan_accepts_relative_path() {
    let sb = Sandbox::new();
    let repo = sb.workspace.join("rel-repo");
    init_repo(&repo);
    write_file(&repo, "README.md", "hello");
    commit_all(&repo, "initial");

    // Run from sandbox root with relative path "workspace"
    let out = run_cli_in(&["scan", "workspace", "--depth", "1"], &sb, sb.root());
    assert!(
        out.success,
        "scan with relative path failed:\nstdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(
        out.stdout.contains("rel-repo"),
        "scan must detect 'rel-repo'; got:\n{}",
        out.stdout
    );
}

#[test]
fn scan_respects_depth() {
    let sb = Sandbox::new();

    // Create deep repo: workspace/a/b/deep-repo  (depth 3 from workspace)
    let deep = sb.workspace.join("a").join("b").join("deep-repo");
    init_repo(&deep);
    write_file(&deep, "README.md", "deep");
    commit_all(&deep, "initial");

    let ws = sb.workspace.to_str().unwrap();

    // depth=1 must NOT find deep-repo
    let out1 = run_cli(&["scan", ws, "--depth", "1"], &sb);
    assert!(
        out1.success,
        "scan depth=1 failed:\nstdout: {}\nstderr: {}",
        out1.stdout, out1.stderr
    );
    assert!(
        !out1.stdout.contains("deep-repo"),
        "depth=1 must NOT find deep-repo; got:\n{}",
        out1.stdout
    );

    // depth=3 must find deep-repo
    let out3 = run_cli(&["scan", ws, "--depth", "3"], &sb);
    assert!(
        out3.success,
        "scan depth=3 failed:\nstdout: {}\nstderr: {}",
        out3.stdout, out3.stderr
    );
    assert!(
        out3.stdout.contains("deep-repo"),
        "depth=3 must find deep-repo; got:\n{}",
        out3.stdout
    );
}

#[test]
fn scan_missing_path_reports_error() {
    let sb = Sandbox::new();
    let missing = sb.workspace.join("definitely-missing-zzz99999");
    let out = run_cli(&["scan", missing.to_str().unwrap()], &sb);
    assert!(
        !out.success,
        "scan of nonexistent path must fail; stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(
        out.stderr.contains("path not found") || out.stderr.to_lowercase().contains("not found"),
        "stderr must mention 'path not found'; got:\n{}",
        out.stderr
    );
}

#[cfg(unix)]
#[test]
fn scan_handles_symlink_without_duplicate_registration() {
    let sb = Sandbox::new();

    // Real repo
    let real_repo = sb.workspace.join("real-repo");
    init_repo(&real_repo);
    write_file(&real_repo, "README.md", "real");
    commit_all(&real_repo, "initial");

    // Symlink pointing to the real repo
    let link_path = sb.workspace.join("link-to-real");
    std::os::unix::fs::symlink(&real_repo, &link_path).expect("create symlink");

    // scan --add: only one canonical path should be registered
    let out = run_cli(
        &[
            "scan",
            sb.workspace.to_str().unwrap(),
            "--depth",
            "1",
            "--add",
        ],
        &sb,
    );
    assert!(
        out.success,
        "symlink scan failed:\nstdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );

    let cfg = sb.read_config();
    let count = cfg.matches("[[repositories]]").count();
    assert_eq!(
        count, 1,
        "symlink alias must not cause duplicate registration; config:\n{}",
        cfg
    );
}

#[cfg(unix)]
#[test]
fn scan_handles_symlink_loop_without_crash() {
    let sb = Sandbox::new();

    // Real repo inside workspace
    let real_repo = sb.workspace.join("real-repo");
    init_repo(&real_repo);
    write_file(&real_repo, "README.md", "real");
    commit_all(&real_repo, "initial");

    // Create a circular symlink: workspace/link-to-root -> workspace
    // This forms an infinite directory loop that the scanner must not follow forever.
    let link_path = sb.workspace.join("link-to-root");
    std::os::unix::fs::symlink(&sb.workspace, &link_path).expect("create symlink loop");

    let out = run_cli(
        &[
            "scan",
            sb.workspace.to_str().unwrap(),
            "--depth",
            "3",
            "--add",
        ],
        &sb,
    );
    assert!(
        out.success,
        "scan with symlink loop must not crash;\nstdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );

    let cfg = sb.read_config();
    let count = cfg.matches("[[repositories]]").count();
    assert_eq!(
        count, 1,
        "symlink loop must not cause duplicate registration; config:\n{}",
        cfg
    );
}

// ============================================================================
// Tests: list  (CLI-SETUP-003, CLI-LIST-001..009)
// ============================================================================

#[test]
fn list_empty_config_prints_guidance() {
    // No config file at all → list must print guidance and exit 0
    let sb = Sandbox::new();
    let out = run_cli(&["list"], &sb);
    assert!(
        out.success,
        "list with empty config must succeed; stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("No repositories"),
        "stderr must contain 'No repositories'; got:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("gclawl scan <root> --add"),
        "stderr must contain actionable scan --add guidance; got:\n{}",
        out.stderr
    );
}

#[test]
fn list_existing_empty_config_prints_guidance() {
    let sb = Sandbox::new();
    sb.write_config("");

    let out = run_cli(&["list"], &sb);
    assert!(
        out.success,
        "list with existing empty config must succeed; stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("No repositories"),
        "stderr must contain 'No repositories'; got:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("gclawl scan <root> --add"),
        "stderr must contain actionable scan --add guidance; got:\n{}",
        out.stderr
    );
}

/// Build the multi-repo fixture used by several `list` tests.
///
/// Returns the paths of:
/// `(clean_repo, dirty_repo, unpushed_repo, conflict_repo, behind_repo)`
fn create_list_fixture(sb: &Sandbox) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
    // ---- clean-repo (cloned from bare, fully pushed) ----
    let remotes_dir = sb.workspace.join("remotes");
    std::fs::create_dir_all(&remotes_dir).unwrap();

    let clean_bare = remotes_dir.join("clean.git");
    init_bare_repo(&clean_bare);

    let clean_repo = sb.workspace.join("clean-repo");
    clone_repo(clean_bare.to_str().unwrap(), &clean_repo);
    write_file(&clean_repo, "README.md", "clean");
    commit_all(&clean_repo, "initial");
    git_in(&clean_repo, &["push", "-u", "origin", "main"]);

    // ---- dirty-repo (uncommitted change) ----
    let dirty_repo = sb.workspace.join("dirty-repo");
    init_repo(&dirty_repo);
    write_file(&dirty_repo, "README.md", "dirty initial");
    commit_all(&dirty_repo, "initial");
    // Leave an uncommitted file
    write_file(&dirty_repo, "dirty.txt", "dirty change");

    // ---- unpushed-repo (upstream-tracking clone with one local commit) ----
    let unpushed_bare = remotes_dir.join("unpushed.git");
    init_bare_repo(&unpushed_bare);

    let unpushed_setup = sb.workspace.join("unpushed-setup");
    clone_repo(unpushed_bare.to_str().unwrap(), &unpushed_setup);
    write_file(&unpushed_setup, "README.md", "unpushed initial");
    commit_all(&unpushed_setup, "initial");
    git_in(&unpushed_setup, &["push", "-u", "origin", "main"]);

    let unpushed_repo = sb.workspace.join("unpushed-repo");
    clone_repo(unpushed_bare.to_str().unwrap(), &unpushed_repo);
    write_file(&unpushed_repo, "local.txt", "local commit");
    commit_all(&unpushed_repo, "local unpushed commit");

    // ---- conflict-repo (both local and remote changed the same file) ----
    let conflict_bare = remotes_dir.join("conflict.git");
    init_bare_repo(&conflict_bare);

    // A setup clone to populate the bare remote with an initial commit
    let conflict_setup = sb.workspace.join("conflict-setup");
    clone_repo(conflict_bare.to_str().unwrap(), &conflict_setup);
    write_file(&conflict_setup, "shared.txt", "base content");
    commit_all(&conflict_setup, "initial with shared.txt");
    git_in(&conflict_setup, &["push", "-u", "origin", "main"]);

    // Our conflict-repo (this is what gets registered)
    let conflict_repo = sb.workspace.join("conflict-repo");
    clone_repo(conflict_bare.to_str().unwrap(), &conflict_repo);

    // Now push a "remote" change to shared.txt via the setup clone
    write_file(&conflict_setup, "shared.txt", "remote change");
    commit_all(&conflict_setup, "remote commit on shared.txt");
    git_in(&conflict_setup, &["push"]);

    // Make a local change in conflict-repo on the same file
    write_file(&conflict_repo, "shared.txt", "local change");
    commit_all(&conflict_repo, "local commit on shared.txt");

    // Fetch so the tracking ref is up to date (diverged from common ancestor)
    git_in(&conflict_repo, &["fetch"]);

    // ---- behind-repo (fetched but not pulled: local is behind origin) ----
    let behind_bare = remotes_dir.join("behind.git");
    init_bare_repo(&behind_bare);

    // Setup clone: creates initial commit and pushes to behind.git
    let behind_setup = sb.workspace.join("behind-setup");
    clone_repo(behind_bare.to_str().unwrap(), &behind_setup);
    write_file(&behind_setup, "README.md", "behind initial");
    commit_all(&behind_setup, "initial commit");
    git_in(&behind_setup, &["push", "-u", "origin", "main"]);

    // Our behind-repo clone (gets the initial commit)
    let behind_repo = sb.workspace.join("behind-repo");
    clone_repo(behind_bare.to_str().unwrap(), &behind_repo);

    // Push one more commit to the remote (behind-repo doesn't have it)
    write_file(&behind_setup, "remote-update.txt", "remote update");
    commit_all(&behind_setup, "remote update commit");
    git_in(&behind_setup, &["push"]);

    // Fetch in behind-repo — local main stays at initial commit
    git_in(&behind_repo, &["fetch"]);

    (
        clean_repo,
        dirty_repo,
        unpushed_repo,
        conflict_repo,
        behind_repo,
    )
}

#[test]
fn list_prints_registered_repositories_table() {
    let sb = Sandbox::new();
    let (clean, dirty, unpushed, conflict, behind) = create_list_fixture(&sb);

    // Write config registering all repos + one non-existent "missing" path
    let missing_path = sb.workspace.join("nonexistent-missing-repo-xyz");
    let config = format!(
        r#"
[[repositories]]
path = {clean}

[[repositories]]
path = {dirty}

[[repositories]]
path = {unpushed}

[[repositories]]
path = {conflict}

[[repositories]]
path = {behind}

[[repositories]]
path = {missing}
name = "missing-repo"
"#,
        clean = toml_path(&clean),
        dirty = toml_path(&dirty),
        unpushed = toml_path(&unpushed),
        conflict = toml_path(&conflict),
        behind = toml_path(&behind),
        missing = toml_path(&missing_path),
    );
    sb.write_config(&config);

    let out = run_cli(&["list"], &sb);
    assert!(
        out.success,
        "list must succeed even with one ERROR row;\nstdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );

    // Header columns
    for col in &[
        "Repository",
        "Branch",
        "Uncommitted",
        "Push",
        "A / B",
        "Conflict",
        "Last scan",
    ] {
        assert!(
            out.stdout.contains(col),
            "stdout must contain column '{col}';\n{}",
            out.stdout
        );
    }

    // All repos should appear by name
    for name in &[
        "clean-repo",
        "dirty-repo",
        "unpushed-repo",
        "conflict-repo",
        "behind-repo",
    ] {
        assert!(
            out.stdout.contains(name),
            "stdout must contain '{name}';\n{}",
            out.stdout
        );
    }

    // Missing repo → ERROR row
    assert!(
        out.stdout.contains("ERROR"),
        "stdout must contain 'ERROR' for missing repo;\n{}",
        out.stdout
    );

    // Conflict prediction → ⚠
    assert!(
        out.stdout.contains('⚠'),
        "stdout must contain '⚠' for conflict-repo;\n{}",
        out.stdout
    );

    // behind-repo row: A / B should show 0 / 1 (local is behind by one commit)
    if let Some(behind_line) = find_line_containing(&out.stdout, "behind-repo") {
        assert!(
            behind_line.contains("0 / 1"),
            "behind-repo row must show behind count; got:\n{behind_line}"
        );
    } else {
        panic!("behind-repo not found in stdout:\n{}", out.stdout);
    }

    // unpushed-repo row: A / B should show one local commit ahead of upstream.
    if let Some(unpushed_line) = find_line_containing(&out.stdout, "unpushed-repo") {
        assert!(
            unpushed_line.contains("1 / 0"),
            "unpushed-repo row must show ahead count; got:\n{unpushed_line}"
        );
    } else {
        panic!("unpushed-repo not found in stdout:\n{}", out.stdout);
    }
}

#[test]
fn list_marks_current_repository() {
    let sb = Sandbox::new();
    let repo = sb.workspace.join("current-repo");
    init_repo(&repo);
    write_file(&repo, "README.md", "hello");
    commit_all(&repo, "initial");

    let config = format!("[[repositories]]\npath = {}\n", toml_path(&repo));
    sb.write_config(&config);

    // Run with cwd inside the registered repo
    let out = run_cli_in(&["list"], &sb, &repo);
    assert!(
        out.success,
        "list must succeed;\nstdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    // Repo column now shows full path; the `*` marker must precede the path.
    let repo_path_str = repo.to_string_lossy();
    assert!(
        out.stdout.contains(&format!("* {repo_path_str}")),
        "stdout must contain '* {repo_path_str}' (current marker with full path);\n{}",
        out.stdout
    );
}

#[test]
fn list_with_github_keeps_table_without_real_github_dependency() {
    let sb = Sandbox::new();

    // Set up a repo whose origin is a GitHub URL so the GitHub remote path is exercised.
    // The URL is intentionally nonexistent; the test only verifies fail-soft behaviour.
    let repo = sb.workspace.join("gh-test-repo");
    init_repo(&repo);
    write_file(&repo, "README.md", "hello");
    commit_all(&repo, "initial");
    // Add a GitHub-format origin URL (won't be reachable; triggers GitHub enrichment path)
    git_in(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/git-clawler-test/nonexistent-xyz.git",
        ],
    );

    let config = format!("[[repositories]]\npath = {}\n", toml_path(&repo));
    sb.write_config(&config);

    // `list --with-github` should not crash even without real GitHub auth.
    // It may fail gracefully (non-zero exit) but must not panic or hang.
    let out = run_cli(&["list", "--with-github"], &sb);

    assert!(
        out.success,
        "list --with-github must succeed without real GitHub dependency;\nstdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(
        out.stdout.contains("Issues"),
        "stdout must contain 'Issues' column with --with-github;\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("PRs"),
        "stdout must contain 'PRs' column with --with-github;\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("gh-test-repo"),
        "stdout must contain 'gh-test-repo';\n{}",
        out.stdout
    );
}

// ============================================================================
// Tests: config error / error recovery  (CLI-ERR-001..004)
// ============================================================================

#[test]
fn list_fails_on_invalid_config_toml() {
    let sb = Sandbox::new();
    sb.write_config("invalid toml = [");

    let out = run_cli(&["list"], &sb);
    assert!(
        !out.success,
        "list must fail on invalid TOML;\nstdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(
        !out.stderr.is_empty(),
        "stderr must contain parse error details"
    );
}

#[test]
fn invalid_refresh_interval_is_rejected_at_config_boundary() {
    let sandbox = Sandbox::new();
    sandbox.write_config("[general]\nrefresh_interval_secs = 0\n");
    let output = run_cli(&["list"], &sandbox);
    assert!(!output.success, "zero must fail before starting a watcher");
    assert!(output.stderr.contains("refresh_interval_secs"));
    // Positive CLI intervals retain their existing range; GUI-only bounds
    // must not prevent a CLI user from choosing a long polling interval.
    sandbox.write_config("[general]\nrefresh_interval_secs = 86401\n");
    assert!(run_cli(&["list"], &sandbox).success);
}

#[test]
fn failed_fetch_preserves_cached_status_and_keeps_diagnostics_on_stderr() {
    let sandbox = Sandbox::new();
    let repo = sandbox.workspace.join("cached-repo");
    init_repo(&repo);
    write_file(&repo, "README.md", "example");
    commit_all(&repo, "initial");
    for args in [
        vec![
            "remote",
            "add",
            "origin",
            "file:///does-not-exist/git-clawler-fixture",
        ],
        vec!["update-ref", "refs/remotes/origin/main", "HEAD"],
        vec!["config", "branch.main.remote", "origin"],
        vec!["config", "branch.main.merge", "refs/heads/main"],
    ] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap()
            .status
            .success());
    }
    sandbox.write_config(&format!(
        "[general]\nfetch_on_refresh = true\n[[repositories]]\npath = \"{}\"\n",
        repo.to_string_lossy().replace('\\', "\\\\")
    ));
    let output = run_cli(&["list"], &sandbox);
    assert!(output.success);
    assert!(
        !output.stdout.contains("ERROR"),
        "cached status must remain usable"
    );
    assert!(
        !output.stdout.contains("WARN"),
        "stdout must contain only the table"
    );
    assert!(output.stderr.contains("fetch failed"));
}

#[test]
fn list_fails_on_relative_repo_path_in_config() {
    let sb = Sandbox::new();
    sb.write_config("[[repositories]]\npath = \"relative/path\"\n");

    let out = run_cli(&["list"], &sb);
    assert!(
        !out.success,
        "list must fail on relative repo path;\nstdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    // Error message should mention the path validation failure
    let combined = format!("{}{}", out.stdout, out.stderr);
    assert!(
        combined.to_lowercase().contains("relative")
            || combined.to_lowercase().contains("absolute")
            || combined.to_lowercase().contains("path"),
        "output must mention the path error; got:\nstdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
}

#[test]
fn list_fails_on_duplicate_repo_path_in_config() {
    let sb = Sandbox::new();
    // Same absolute path registered twice
    let duplicate_path = sb.workspace.join("some-repo-xyz");
    sb.write_config(&format!(
        "[[repositories]]\npath = {path}\n\n\
         [[repositories]]\npath = {path}\n",
        path = toml_path(&duplicate_path),
    ));

    let out = run_cli(&["list"], &sb);
    assert!(
        !out.success,
        "list must fail on duplicate repo path;\nstdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    let combined = format!("{}{}", out.stdout, out.stderr);
    assert!(
        combined.to_lowercase().contains("duplicate"),
        "output must mention 'duplicate'; got:\nstdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
}
