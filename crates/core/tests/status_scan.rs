//! Integration tests for [`git_clawler_core::scan`].
//!
//! Each test creates a real (in-memory / temp-dir) git repository using the
//! [`RepoBuilder`] helper and then calls `scan()` to verify the returned
//! [`RepoStatus`].

use std::path::Path;
use std::process::Command;

use git_clawler_core::scan;

// ===========================================================================
// Test helper
// ===========================================================================

/// A thin wrapper around a temporary directory and a `git2::Repository` that
/// provides ergonomic helpers for building test fixtures.
///
/// All git operations use an explicit signature so the tests are independent of
/// the global `~/.gitconfig`.
struct RepoBuilder {
    /// The temporary directory that owns the repository files.
    ///
    /// Keeping it alive prevents the directory from being deleted while the
    /// test is running.
    _dir: tempfile::TempDir,
    repo: git2::Repository,
}

impl RepoBuilder {
    /// Initialise a new, non-bare repository in a fresh temp directory.
    ///
    /// Uses `RepositoryInitOptions::initial_head("main")` so the default branch
    /// is always `main` regardless of the caller's `init.defaultBranch` setting
    /// (fixes environment-dependent test failures on machines configured with
    /// `master` as the default).
    fn init() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = git2::Repository::init_opts(dir.path(), &opts).expect("init repo");
        // Set local config so tests never depend on the global gitconfig.
        {
            let mut cfg = repo.config().expect("repo config");
            cfg.set_str("user.name", "test").unwrap();
            cfg.set_str("user.email", "test@example.com").unwrap();
        }
        Self { _dir: dir, repo }
    }

    /// Return the absolute path to the repository root.
    fn path(&self) -> &Path {
        // SAFETY: the TempDir lives as long as `self`.
        self._dir.path()
    }

    /// Write `content` to a file named `name` inside the repository.
    fn write_file(&self, name: &str, content: &str) {
        std::fs::write(self.path().join(name), content).expect("write file");
    }

    /// Stage the file `name` (equivalent to `git add <name>`).
    fn stage(&self, name: &str) {
        let mut index = self.repo.index().expect("index");
        index.add_path(Path::new(name)).expect("add path");
        index.write().expect("write index");
    }

    /// Create a commit on HEAD with the given message.
    ///
    /// Uses an explicit signature — global gitconfig is not consulted.
    fn commit(&self, message: &str) -> git2::Oid {
        let sig = git2::Signature::now("test", "test@example.com").expect("sig");
        let mut index = self.repo.index().expect("index");
        let tree_id = index.write_tree().expect("write tree");
        let tree = self.repo.find_tree(tree_id).expect("find tree");

        let parent_commit = self.repo.head().ok().and_then(|h| {
            let oid = h.target()?;
            self.repo.find_commit(oid).ok()
        });
        let parents: Vec<&git2::Commit> = parent_commit.iter().collect();

        self.repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .expect("commit")
    }

    /// Create a new local branch named `name` pointing at the current HEAD.
    fn create_branch(&self, name: &str) {
        let head = self.repo.head().expect("head");
        let oid = head.target().expect("head oid");
        let commit = self.repo.find_commit(oid).expect("find commit");
        self.repo
            .branch(name, &commit, false)
            .expect("create branch");
    }

    /// Make HEAD point to the local branch `name` (does NOT touch the work
    /// tree — fine for status-only tests).
    fn checkout(&self, name: &str) {
        let refname = format!("refs/heads/{name}");
        self.repo.set_head(&refname).expect("set head");
    }

    /// Detach HEAD at the current commit.
    fn detach_head(&self) {
        let oid = self.repo.head().expect("head").target().expect("head oid");
        self.repo.set_head_detached(oid).expect("detach head");
    }

    /// Manually create (or update) a remote-tracking ref
    /// `refs/remotes/{remote}/{branch}` pointing at `oid`.
    ///
    /// This simulates "the remote was at this commit" without actually pushing.
    /// Also registers the remote in the repo config (with a placeholder URL) if
    /// it does not yet exist, so that `git2::Branch::upstream()` can resolve
    /// the fetch refspecs when looking up the upstream ref.
    fn set_remote_tracking(&self, remote: &str, branch: &str, oid: git2::Oid) {
        // Ensure the remote entry exists — git2's upstream resolution needs the
        // fetch refspec that comes with a registered remote.
        if self.repo.find_remote(remote).is_err() {
            self.repo
                .remote(remote, "https://placeholder.example.invalid/repo.git")
                .expect("add placeholder remote");
        }
        let refname = format!("refs/remotes/{remote}/{branch}");
        self.repo
            .reference(&refname, oid, true, "set remote tracking")
            .expect("set remote tracking ref");
    }

    /// Configure `branch.{local}.remote` and `branch.{local}.merge` so that
    /// `git2` considers `{remote}/{remote_branch}` to be the upstream of
    /// `local`.
    ///
    /// Writes directly to the local repository config — avoids the need to
    /// register a real `git remote` entry.
    ///
    /// Assumes that the remote-tracking ref already exists (created via
    /// [`Self::set_remote_tracking`]).
    fn set_branch_upstream(&self, local: &str, remote: &str, remote_branch: &str) {
        let mut cfg = self.repo.config().expect("repo config");
        cfg.set_str(&format!("branch.{local}.remote"), remote)
            .expect("set branch.remote");
        cfg.set_str(
            &format!("branch.{local}.merge"),
            &format!("refs/heads/{remote_branch}"),
        )
        .expect("set branch.merge");
    }

    /// Return the OID that HEAD (or any symbolic ref) currently resolves to.
    #[allow(dead_code)]
    fn head_oid(&self) -> git2::Oid {
        self.repo
            .head()
            .expect("head")
            .target()
            .expect("head target")
    }
}

#[derive(Debug, PartialEq, Eq)]
struct GitSubprocessStatus {
    uncommitted_files: u32,
    current_branch: Option<String>,
    ahead: u32,
    behind: u32,
}

fn git_available() -> bool {
    git_command()
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn git_command() -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_NAMESPACE");
    command
}

fn git_output(path: &Path, args: &[&str]) -> Vec<u8> {
    let output = git_command()
        .args(args)
        .current_dir(path)
        .output()
        .expect("run git subprocess");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn subprocess_status_poc(path: &Path) -> GitSubprocessStatus {
    let status_stdout = git_output(
        path,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    );
    let uncommitted_files = count_porcelain_z_entries(&status_stdout);

    let branch_stdout = git_output(path, &["branch", "--show-current"]);
    let current_branch = String::from_utf8(branch_stdout)
        .expect("branch output utf8")
        .trim()
        .to_string();
    let current_branch = (!current_branch.is_empty()).then_some(current_branch);

    let (ahead, behind) = if current_branch.is_some() {
        let upstream = git_command()
            .args([
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ])
            .current_dir(path)
            .output()
            .expect("run git rev-parse upstream");
        if upstream.status.success() {
            let counts = git_output(
                path,
                &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
            );
            parse_rev_list_counts(&counts)
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };

    GitSubprocessStatus {
        uncommitted_files,
        current_branch,
        ahead,
        behind,
    }
}

fn count_porcelain_z_entries(stdout: &[u8]) -> u32 {
    let mut count = 0u32;
    let mut skip_next_path = false;
    for record in stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        if skip_next_path {
            skip_next_path = false;
            continue;
        }
        if record.len() < 3 {
            continue;
        }
        let x = record[0];
        let y = record[1];
        if x == b'!' && y == b'!' {
            continue;
        }
        count += 1;
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            skip_next_path = true;
        }
    }
    count
}

fn parse_rev_list_counts(stdout: &[u8]) -> (u32, u32) {
    let text = String::from_utf8(stdout.to_vec()).expect("rev-list output utf8");
    let mut parts = text.split_whitespace();
    let behind = parts
        .next()
        .expect("behind count")
        .parse()
        .expect("parse behind count");
    let ahead = parts
        .next()
        .expect("ahead count")
        .parse()
        .expect("parse ahead count");
    (ahead, behind)
}

// ===========================================================================
// Integration tests
// ===========================================================================

// I1 ─────────────────────────────────────────────────────────────────────────
/// A freshly initialised, empty repository (no commits, no files) must return
/// the zero-value status without error.
#[test]
fn empty_repo_returns_default_status() {
    let rb = RepoBuilder::init();
    let status = scan(rb.path()).expect("scan");

    assert_eq!(status.uncommitted_files, 0);
    assert!(status.unpushed_branches.is_empty());
    assert!(status.current_branch.is_none()); // HEAD is unborn
    assert_eq!(status.ahead, 0);
    assert_eq!(status.behind, 0);
    assert!(status.recent_commits.is_empty());
}

// I2 ─────────────────────────────────────────────────────────────────────────
/// Two untracked files must both appear in `uncommitted_files`.
#[test]
fn untracked_files_are_counted() {
    let rb = RepoBuilder::init();
    rb.write_file("alpha.txt", "a");
    rb.write_file("beta.txt", "b");

    let status = scan(rb.path()).expect("scan");
    assert_eq!(status.uncommitted_files, 2);
}

// I3 ─────────────────────────────────────────────────────────────────────────
/// A file that is staged AND has subsequent working-tree modifications must be
/// counted as a single entry (entry-level counting, not bit-level).
#[test]
fn staged_and_modified_files_counted_once() {
    let rb = RepoBuilder::init();
    rb.write_file("file.txt", "v1");
    rb.stage("file.txt");
    // Modify again after staging so the entry carries both INDEX and WT bits.
    rb.write_file("file.txt", "v2");

    let status = scan(rb.path()).expect("scan");
    assert_eq!(status.uncommitted_files, 1);
}

// I4 ─────────────────────────────────────────────────────────────────────────
/// A repository whose only commit contains all files, with no further changes,
/// must report zero uncommitted files.
#[test]
fn clean_repo_returns_zero_uncommitted() {
    let rb = RepoBuilder::init();
    rb.write_file("README.md", "hello");
    rb.stage("README.md");
    rb.commit("initial");

    let status = scan(rb.path()).expect("scan");
    assert_eq!(status.uncommitted_files, 0);
}

#[test]
fn recent_commits_returns_newest_five_head_commits() {
    let rb = RepoBuilder::init();
    let mut oids = Vec::new();
    for idx in 0..7 {
        let filename = format!("file-{idx}.txt");
        rb.write_file(&filename, &format!("content {idx}"));
        rb.stage(&filename);
        oids.push(rb.commit(&format!("commit {idx}")));
    }

    let status = scan(rb.path()).expect("scan");

    assert_eq!(status.recent_commits.len(), 5);
    let summaries: Vec<_> = status
        .recent_commits
        .iter()
        .map(|commit| commit.summary.as_str())
        .collect();
    assert_eq!(
        summaries,
        vec!["commit 6", "commit 5", "commit 4", "commit 3", "commit 2"]
    );
    assert_eq!(
        status.recent_commits[0].short_hash,
        oids[6].to_string()[..7]
    );
    assert_eq!(status.recent_commits[0].author_name, "test");
    assert!(status.recent_commits[0].committed_at.ends_with('Z'));
}

// I5 ─────────────────────────────────────────────────────────────────────────
/// A branch with **no upstream** configured must appear in `unpushed_branches`
/// with `has_upstream = false` and `ahead` equal to the total number of
/// commits reachable from its tip.
#[test]
fn branch_without_upstream_is_unpushed_with_total_commits() {
    let rb = RepoBuilder::init();

    // Two commits on main (no upstream ever configured).
    rb.write_file("a.txt", "1");
    rb.stage("a.txt");
    rb.commit("first");

    rb.write_file("b.txt", "2");
    rb.stage("b.txt");
    rb.commit("second");

    let status = scan(rb.path()).expect("scan");

    assert_eq!(status.unpushed_branches.len(), 1);
    let branch = &status.unpushed_branches[0];
    assert!(!branch.has_upstream, "expected has_upstream=false");
    assert_eq!(branch.ahead, 2, "expected 2 total commits");
}

// I6 ─────────────────────────────────────────────────────────────────────────
/// A branch whose tip is 3 commits ahead of its upstream must appear in
/// `unpushed_branches` with `has_upstream = true` and `ahead = 3`.
#[test]
fn branch_with_upstream_ahead_is_listed() {
    let rb = RepoBuilder::init();

    // Create a "synced" base commit.
    rb.write_file("base.txt", "base");
    rb.stage("base.txt");
    let base_oid = rb.commit("base");

    // Simulate the remote being at `base`.
    rb.set_remote_tracking("origin", "main", base_oid);
    rb.set_branch_upstream("main", "origin", "main");

    // Three new commits that the remote does not have.
    for i in 1..=3u8 {
        rb.write_file(&format!("f{i}.txt"), "x");
        rb.stage(&format!("f{i}.txt"));
        rb.commit(&format!("commit {i}"));
    }

    let status = scan(rb.path()).expect("scan");

    let branch = status
        .unpushed_branches
        .iter()
        .find(|b| b.name == "main")
        .expect("main should be in unpushed_branches");

    assert!(branch.has_upstream, "expected has_upstream=true");
    assert_eq!(branch.ahead, 3);
}

// I7 ─────────────────────────────────────────────────────────────────────────
/// A branch whose tip is exactly at its upstream (ahead = 0) must NOT appear
/// in `unpushed_branches`.
#[test]
fn branch_with_upstream_synced_is_not_listed() {
    let rb = RepoBuilder::init();

    rb.write_file("README.md", "v1");
    rb.stage("README.md");
    let tip_oid = rb.commit("initial");

    // Remote is at the same commit as local.
    rb.set_remote_tracking("origin", "main", tip_oid);
    rb.set_branch_upstream("main", "origin", "main");

    let status = scan(rb.path()).expect("scan");
    assert!(
        status.unpushed_branches.is_empty(),
        "synced branch must not be in unpushed_branches"
    );
}

// I8 ─────────────────────────────────────────────────────────────────────────
/// When HEAD is detached, `current_branch` must be `None`.
#[test]
fn detached_head_returns_none_branch() {
    let rb = RepoBuilder::init();

    rb.write_file("file.txt", "x");
    rb.stage("file.txt");
    rb.commit("initial");
    rb.detach_head();

    let status = scan(rb.path()).expect("scan");
    assert!(
        status.current_branch.is_none(),
        "detached HEAD must yield current_branch=None"
    );
}

// I9 ─────────────────────────────────────────────────────────────────────────
/// When there are multiple local branches, each is evaluated independently.
/// A synced `main` must not appear; an unpushed `feature` branch must appear.
#[test]
fn multiple_local_branches_each_evaluated() {
    let rb = RepoBuilder::init();

    // Base commit — will be the sync point for main.
    rb.write_file("base.txt", "base");
    rb.stage("base.txt");
    let base_oid = rb.commit("base");

    // Set up main as synced with origin.
    rb.set_remote_tracking("origin", "main", base_oid);
    rb.set_branch_upstream("main", "origin", "main");

    // Create feature branch and add a commit there.
    rb.create_branch("feature");
    rb.checkout("feature");
    rb.write_file("feat.txt", "y");
    rb.stage("feat.txt");
    rb.commit("feat commit");

    // Go back to main (synced).
    rb.checkout("main");

    let status = scan(rb.path()).expect("scan");

    let names: Vec<&str> = status
        .unpushed_branches
        .iter()
        .map(|b| b.name.as_str())
        .collect();

    assert!(
        names.contains(&"feature"),
        "feature branch should be in unpushed_branches; got {names:?}"
    );
    assert!(
        !names.contains(&"main"),
        "synced main must not be in unpushed_branches; got {names:?}"
    );
}

// I10 ────────────────────────────────────────────────────────────────────────
/// When the current branch (HEAD) is itself ahead of its upstream it must
/// appear in `unpushed_branches`, and `RepoStatus.ahead` must match.
#[test]
fn current_branch_with_ahead_appears_in_unpushed() {
    let rb = RepoBuilder::init();

    // One base commit shared with remote.
    rb.write_file("base.txt", "0");
    rb.stage("base.txt");
    let base_oid = rb.commit("base");

    rb.set_remote_tracking("origin", "main", base_oid);
    rb.set_branch_upstream("main", "origin", "main");

    // Two more local commits.
    for i in 1..=2u8 {
        rb.write_file(&format!("extra{i}.txt"), "x");
        rb.stage(&format!("extra{i}.txt"));
        rb.commit(&format!("extra {i}"));
    }

    let status = scan(rb.path()).expect("scan");

    assert_eq!(
        status.current_branch.as_deref(),
        Some("main"),
        "current_branch should be main"
    );
    assert_eq!(status.ahead, 2, "RepoStatus.ahead should be 2");

    let branch = status
        .unpushed_branches
        .iter()
        .find(|b| b.name == "main")
        .expect("main must be in unpushed_branches");

    assert!(branch.has_upstream);
    assert_eq!(branch.ahead, 2);
}

#[test]
fn subprocess_poc_matches_git2_for_uncommitted_and_ahead_behind_subset() {
    if !git_available() {
        return;
    }
    let rb = RepoBuilder::init();

    rb.write_file("base.txt", "0");
    rb.stage("base.txt");
    let base_oid = rb.commit("base");

    rb.set_remote_tracking("origin", "main", base_oid);
    rb.set_branch_upstream("main", "origin", "main");

    rb.write_file("ahead.txt", "ahead");
    rb.stage("ahead.txt");
    rb.commit("ahead");
    rb.write_file("dirty.txt", "dirty");
    rb.write_file("untracked.txt", "untracked");

    let git2_status = scan(rb.path()).expect("scan");
    let subprocess = subprocess_status_poc(rb.path());

    assert_eq!(subprocess.uncommitted_files, git2_status.uncommitted_files);
    assert_eq!(subprocess.current_branch, git2_status.current_branch);
    assert_eq!(subprocess.ahead, git2_status.ahead);
    assert_eq!(subprocess.behind, git2_status.behind);
}

// I11 ────────────────────────────────────────────────────────────────────────
/// A branch with `branch.<name>.remote` set but no `branch.<name>.merge` must
/// be treated as having no upstream — git itself considers an upstream
/// configured only when both keys are present.
#[test]
fn branch_with_only_remote_config_is_treated_as_no_upstream() {
    let rb = RepoBuilder::init();

    rb.write_file("a.txt", "1");
    rb.stage("a.txt");
    rb.commit("first");

    rb.write_file("b.txt", "2");
    rb.stage("b.txt");
    rb.commit("second");

    // Only set `branch.main.remote`, omit `branch.main.merge`.
    {
        let mut cfg = rb.repo.config().expect("repo config");
        cfg.set_str("branch.main.remote", "origin")
            .expect("set branch.main.remote");
    }

    let status = scan(rb.path()).expect("scan should succeed");

    assert_eq!(status.unpushed_branches.len(), 1);
    let branch = &status.unpushed_branches[0];
    assert_eq!(branch.name, "main");
    assert!(
        !branch.has_upstream,
        "remote-only config must be treated as no upstream"
    );
    assert_eq!(branch.ahead, 2, "ahead must equal total commit count");
}

// I12 ────────────────────────────────────────────────────────────────────────
/// A branch with `branch.<name>.merge` set but no `branch.<name>.remote` must
/// likewise be treated as having no upstream.
#[test]
fn branch_with_only_merge_config_is_treated_as_no_upstream() {
    let rb = RepoBuilder::init();

    rb.write_file("a.txt", "1");
    rb.stage("a.txt");
    rb.commit("first");

    rb.write_file("b.txt", "2");
    rb.stage("b.txt");
    rb.commit("second");

    // Only set `branch.main.merge`, omit `branch.main.remote`.
    {
        let mut cfg = rb.repo.config().expect("repo config");
        cfg.set_str("branch.main.merge", "refs/heads/main")
            .expect("set branch.main.merge");
    }

    let status = scan(rb.path()).expect("scan should succeed");

    assert_eq!(status.unpushed_branches.len(), 1);
    let branch = &status.unpushed_branches[0];
    assert_eq!(branch.name, "main");
    assert!(
        !branch.has_upstream,
        "merge-only config must be treated as no upstream"
    );
    assert_eq!(branch.ahead, 2, "ahead must equal total commit count");
}

// I13 ────────────────────────────────────────────────────────────────────────
/// A branch with BOTH `branch.<name>.remote` and `branch.<name>.merge`
/// configured but whose upstream cannot actually be resolved (no real
/// `origin` remote registered, no remote-tracking ref) must surface as
/// `StatusError::Git`, with the branch name included in the context.
#[test]
fn branch_with_broken_upstream_returns_error() {
    let rb = RepoBuilder::init();

    rb.write_file("a.txt", "1");
    rb.stage("a.txt");
    rb.commit("first");

    // Set both keys WITHOUT registering a real `origin` remote, so that
    // `git2::Branch::upstream()` fails when the scanner tries to resolve it.
    {
        let mut cfg = rb.repo.config().expect("repo config");
        cfg.set_str("branch.main.remote", "origin")
            .expect("set branch.main.remote");
        cfg.set_str("branch.main.merge", "refs/heads/main")
            .expect("set branch.main.merge");
    }

    let err = scan(rb.path()).expect_err("scan should fail with broken upstream");
    match err {
        git_clawler_core::StatusError::Git { context, .. } => {
            assert!(
                context.contains("main"),
                "context should mention the branch name 'main', got: {context}"
            );
        }
        other => panic!("expected StatusError::Git, got {other:?}"),
    }
}

// ===========================================================================
// Linked worktree helpers
// ===========================================================================

/// Holds the main [`RepoBuilder`] and a linked worktree that was created with
/// `git worktree add`.
///
/// The main worktree lives in `rb._dir`, the linked worktree lives in
/// `wt_dir`.
struct LinkedWorktree {
    /// Main worktree repository / builder.
    rb: RepoBuilder,
    /// Temporary directory that holds the linked worktree's parent.
    wt_dir: tempfile::TempDir,
    /// Subdirectory name within `wt_dir` where the linked worktree lives.
    wt_subdir: std::path::PathBuf,
}

impl LinkedWorktree {
    /// Create a fixture with:
    ///   - main worktree on branch `main` (with one initial commit)
    ///   - linked worktree checked out on branch `feature`
    ///
    /// `main` is left with additional branches `other` and `feature` so that
    /// W2 can verify all three appear in a main-worktree scan.
    fn build() -> Self {
        let rb = RepoBuilder::init();

        // Initial commit on main.
        rb.write_file("README.md", "hello");
        rb.stage("README.md");
        rb.commit("initial");

        // Create `other` branch (stays at initial commit — no upstream).
        rb.create_branch("other");

        // Create `feature` branch (diverges from initial — adds one commit).
        rb.create_branch("feature");
        rb.checkout("feature");
        rb.write_file("feat.txt", "feature work");
        rb.stage("feat.txt");
        rb.commit("feature commit");

        // Return to `main` in the main worktree before creating the linked wt.
        rb.checkout("main");

        // Create a temp dir to *own* the linked worktree's parent, but pass
        // a *subdirectory* path that does NOT yet exist to `repo.worktree()`.
        // git2 requires the target path to be absent; tempdir() pre-creates it.
        let wt_parent = tempfile::tempdir().expect("wt parent tempdir");
        let wt_path = wt_parent.path().join("linked-wt");
        // `wt_path` does not exist yet — git2 will create it.

        let _wt = rb
            .repo
            .worktree("wt-feature", &wt_path, None)
            .expect("create linked worktree");

        // Open the worktree repo and set HEAD to `feature`.
        let wt_repo = git2::Repository::open(&wt_path).expect("open linked worktree repo");
        wt_repo
            .set_head("refs/heads/feature")
            .expect("set linked worktree HEAD to feature");

        // Checkout the feature tree into the linked worktree working directory
        // so the working tree is clean and consistent with the HEAD.
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.force();
        wt_repo
            .checkout_head(Some(&mut checkout))
            .expect("checkout feature in linked worktree");

        // Wrap the parent dir in a TempDir-compatible structure.
        // We re-use `wt_parent` as the directory owner.
        LinkedWorktree {
            rb,
            wt_dir: wt_parent,
            wt_subdir: std::path::PathBuf::from("linked-wt"),
        }
    }

    /// Path to the linked worktree working directory.
    fn wt_path(&self) -> std::path::PathBuf {
        self.wt_dir.path().join(&self.wt_subdir)
    }

    /// Path to the main worktree working directory.
    fn main_path(&self) -> &Path {
        self.rb.path()
    }

    /// Open the linked worktree's `git2::Repository`.
    fn wt_repo(&self) -> git2::Repository {
        git2::Repository::open(self.wt_path()).expect("open linked worktree repo")
    }
}

// ===========================================================================
// Linked worktree integration tests (W1 – W5)
// ===========================================================================

// W1 ─────────────────────────────────────────────────────────────────────────
/// Scanning a **linked** worktree must only include the current branch
/// (`feature`) in `unpushed_branches` — not the other branches from the main
/// worktree (`main`, `other`).
#[test]
fn scan_linked_worktree_only_reports_current_branch_as_unpushed() {
    let lw = LinkedWorktree::build();

    let status = scan(&lw.wt_path()).expect("scan linked worktree");

    let names: Vec<&str> = status
        .unpushed_branches
        .iter()
        .map(|b| b.name.as_str())
        .collect();

    assert!(
        names.contains(&"feature"),
        "expected 'feature' in unpushed_branches; got {names:?}"
    );
    assert!(
        !names.contains(&"main"),
        "'main' must not appear in linked worktree scan; got {names:?}"
    );
    assert!(
        !names.contains(&"other"),
        "'other' must not appear in linked worktree scan; got {names:?}"
    );
    assert_eq!(
        status.current_branch.as_deref(),
        Some("feature"),
        "current_branch should be 'feature'"
    );
}

// W2 ─────────────────────────────────────────────────────────────────────────
/// Scanning the **main** worktree must still enumerate ALL branches
/// (backward-compatible behaviour).
#[test]
fn scan_main_worktree_still_enumerates_all_branches() {
    let lw = LinkedWorktree::build();

    let status = scan(lw.main_path()).expect("scan main worktree");

    let names: Vec<&str> = status
        .unpushed_branches
        .iter()
        .map(|b| b.name.as_str())
        .collect();

    // `main` has only the initial commit and no upstream → appears as unpushed.
    assert!(
        names.contains(&"main"),
        "expected 'main' in main worktree scan; got {names:?}"
    );
    // `other` has the same initial commit and no upstream → appears as unpushed.
    assert!(
        names.contains(&"other"),
        "expected 'other' in main worktree scan; got {names:?}"
    );
    // `feature` is ahead of initial → appears as unpushed.
    assert!(
        names.contains(&"feature"),
        "expected 'feature' in main worktree scan; got {names:?}"
    );
}

// W3 ─────────────────────────────────────────────────────────────────────────
/// Making an uncommitted change inside the **linked** worktree's working
/// directory must be reflected in `uncommitted_files > 0`.
#[test]
fn scan_linked_worktree_counts_uncommitted_files() {
    let lw = LinkedWorktree::build();

    // Baseline: the linked worktree must be clean right after construction.
    let baseline = scan(&lw.wt_path()).expect("scan linked worktree (baseline)");
    assert_eq!(
        baseline.uncommitted_files, 0,
        "expected 0 uncommitted_files before any modification; got {}",
        baseline.uncommitted_files
    );

    // Write an untracked file directly into the linked worktree working tree.
    std::fs::write(lw.wt_path().join("untracked.txt"), "dirty")
        .expect("write untracked file in linked wt");

    let status = scan(&lw.wt_path()).expect("scan linked worktree");

    assert_eq!(
        status.uncommitted_files, 1,
        "expected uncommitted_files == 1 after writing one untracked file; got {}",
        status.uncommitted_files
    );
}

// W4 ─────────────────────────────────────────────────────────────────────────
/// When the linked worktree's current branch has an upstream configured, the
/// `ahead` / `behind` values must be computed correctly.
#[test]
fn scan_linked_worktree_computes_ahead_behind_for_current_branch() {
    let lw = LinkedWorktree::build();

    // The linked worktree is on `feature`. Its tip is the "feature commit"
    // added in LinkedWorktree::build (1 commit ahead of initial).
    // Simulate the remote at the initial commit (i.e., 1 commit behind).
    let wt_repo = lw.wt_repo();

    // Resolve the tip of `feature` inside the wt_repo.
    let feature_branch = wt_repo
        .find_branch("feature", git2::BranchType::Local)
        .expect("find feature branch");
    let feature_tip = feature_branch.get().target().expect("feature tip oid");

    // The "remote tip" for feature is its parent (the initial commit).
    let feature_commit = wt_repo.find_commit(feature_tip).expect("feature commit");
    let initial_oid = feature_commit.parent_id(0).expect("parent oid");

    // Set up fake remote-tracking ref and upstream config on the wt_repo.
    // Must set the remote-tracking ref on the *linked* worktree's repo,
    // and the upstream config likewise.
    if wt_repo.find_remote("origin").is_err() {
        wt_repo
            .remote("origin", "https://placeholder.example.invalid/repo.git")
            .expect("add placeholder remote to wt_repo");
    }
    wt_repo
        .reference(
            "refs/remotes/origin/feature",
            initial_oid,
            true,
            "set remote tracking for feature",
        )
        .expect("set remote tracking ref");

    {
        let mut cfg = wt_repo.config().expect("wt_repo config");
        cfg.set_str("branch.feature.remote", "origin")
            .expect("set branch.feature.remote");
        cfg.set_str("branch.feature.merge", "refs/heads/feature")
            .expect("set branch.feature.merge");
    }

    let status = scan(&lw.wt_path()).expect("scan linked worktree with upstream");

    assert_eq!(
        status.current_branch.as_deref(),
        Some("feature"),
        "current_branch should be 'feature'"
    );
    assert_eq!(
        status.ahead, 1,
        "expected ahead=1 (feature is 1 commit ahead of fake remote)"
    );
    assert_eq!(status.behind, 0, "expected behind=0");

    let branch = status
        .unpushed_branches
        .iter()
        .find(|b| b.name == "feature")
        .expect("feature must be in unpushed_branches");
    assert!(branch.has_upstream, "has_upstream should be true");
    assert_eq!(branch.ahead, 1);
}

// W5 ─────────────────────────────────────────────────────────────────────────
/// A linked worktree in **detached HEAD** state must report
/// `current_branch = None`, `unpushed_branches = []`, `ahead = 0`,
/// `behind = 0`.
#[test]
fn scan_linked_worktree_with_detached_head_returns_empty() {
    let lw = LinkedWorktree::build();

    // Detach HEAD in the linked worktree.
    let wt_repo = lw.wt_repo();
    let oid = wt_repo
        .head()
        .expect("wt head")
        .target()
        .expect("wt head oid");
    wt_repo.set_head_detached(oid).expect("detach wt head");

    let status = scan(&lw.wt_path()).expect("scan linked worktree with detached HEAD");

    assert!(
        status.current_branch.is_none(),
        "expected current_branch=None for detached HEAD in linked wt"
    );
    assert!(
        status.unpushed_branches.is_empty(),
        "expected empty unpushed_branches for detached HEAD in linked wt; got {:?}",
        status.unpushed_branches
    );
    assert_eq!(status.ahead, 0, "expected ahead=0");
    assert_eq!(status.behind, 0, "expected behind=0");
}

#[test]
fn separate_git_directory_reports_non_head_branches() {
    let rb = RepoBuilder::init();
    rb.commit("initial");
    rb.create_branch("unpublished");
    let path = rb.path().to_path_buf();
    drop(rb.repo);
    let storage = tempfile::tempdir().unwrap();
    let gitdir = storage.path().join("metadata");
    std::fs::rename(path.join(".git"), &gitdir).unwrap();
    std::fs::write(path.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
    let result = scan(&path).unwrap();
    assert!(result
        .unpushed_branches
        .iter()
        .any(|branch| branch.name == "unpublished"));
}

#[test]
fn submodule_reports_non_head_branches() {
    let parent = RepoBuilder::init();
    let child = RepoBuilder::init();
    child.commit("initial");
    let status = Command::new("git")
        .arg("-C")
        .arg(parent.path())
        .args(["-c", "protocol.file.allow=always", "submodule", "add", "--"])
        .arg(child.path())
        .arg("child")
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let path = parent.path().join("child");
    let repo = git2::Repository::open(&path).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("unpublished", &head, false).unwrap();
    assert!(path.join(".git").is_file());
    assert!(scan(&path)
        .unwrap()
        .unpushed_branches
        .iter()
        .any(|b| b.name == "unpublished"));
}
