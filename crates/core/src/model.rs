//! Runtime data types for git-clawler.
//!
//! This module defines the core domain models that are shared between the CLI
//! and GUI frontends. Types here represent the **runtime** view of repository
//! data, as opposed to the persistence-oriented types in [`crate::config`].

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Repository
// ---------------------------------------------------------------------------

/// A monitored git repository.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Repository {
    /// Human-readable display name (e.g. `"my-project"`).
    pub name: String,
    /// Absolute path to the repository root.
    pub(crate) path: PathBuf,
    /// Latest scan result. `None` means the repo has never been scanned.
    #[serde(default)]
    pub status: Option<RepoStatus>,
    /// When the repository was last scanned. `None` means never.
    #[serde(default)]
    pub last_scanned: Option<DateTime<Utc>>,
    /// Per-repository override of `general.fetch_on_refresh`.
    ///
    /// Mirrors [`crate::config::RepoEntry::fetch`]: `None` means inherit the
    /// general default, `Some(true)`/`Some(false)` overrides it. Carried on the
    /// runtime model so that round-tripping config through
    /// [`crate::registry::Registry`] does not silently drop the setting.
    #[serde(default)]
    pub fetch: Option<bool>,
    /// Optional WSL distribution name (mirrors [`crate::config::RepoEntry::distro`]).
    ///
    /// `Some(...)` means the repository lives inside that WSL distribution and
    /// must be scanned via the wsl-helper rather than natively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
}

impl Repository {
    /// Create a new `Repository` with the given name and path.
    ///
    /// `status`, `last_scanned`, and `fetch` are initialised to `None`.
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            status: None,
            last_scanned: None,
            fetch: None,
            distro: None,
        }
    }

    /// Builder-style setter for [`Self::fetch`].
    pub fn with_fetch(mut self, fetch: Option<bool>) -> Self {
        self.fetch = fetch;
        self
    }

    /// Builder-style setter for [`Self::distro`].
    pub fn with_distro(mut self, distro: Option<String>) -> Self {
        self.distro = distro;
        self
    }

    /// Return the absolute path to the repository root.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ---------------------------------------------------------------------------
// RepoStatus
// ---------------------------------------------------------------------------

/// Result of scanning a single repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoStatus {
    /// A failed optional fetch; status below still uses cached remote refs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_warning: Option<String>,
    /// Number of files with uncommitted changes (staged + unstaged + untracked).
    pub uncommitted_files: u32,
    /// Branches that have commits not yet pushed to their upstream.
    ///
    /// **Note**: For a *linked* worktree (created by `git worktree add`),
    /// this is restricted to the worktree's current branch to avoid duplicating
    /// entries already reported by the main worktree's `RepoStatus`. See
    /// [`crate::status::scan`] for details.
    #[serde(default)]
    pub unpushed_branches: Vec<UnpushedBranch>,
    /// Name of the currently checked-out branch, if any.
    pub current_branch: Option<String>,
    /// Commits ahead of upstream on the current branch.
    pub ahead: u32,
    /// Commits behind upstream on the current branch.
    pub behind: u32,
    /// Optional GitHub-specific metadata.
    #[serde(default)]
    pub github: Option<GithubInfo>,
    /// Pull コンフリクト予測の結果。
    ///
    /// `None` は判定不能（upstream 無し / detached / unborn / merge-base 無し /
    /// 同一 commit / 片側のみ ahead）。
    /// `Some(true)` は変更パスが両側で重複（コンフリクトの可能性が高い）。
    /// `Some(false)` はクリーンマージ可能と予測。
    #[serde(default)]
    pub conflict_predicted: Option<bool>,
    /// Newest commits reachable from `HEAD`, most recent first.
    #[serde(default)]
    pub recent_commits: Vec<RecentCommit>,
}

// ---------------------------------------------------------------------------
// RecentCommit
// ---------------------------------------------------------------------------

/// A compact commit summary for recent history views.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentCommit {
    /// Short object ID, currently 7 hex characters.
    pub short_hash: String,
    /// First-line commit summary.
    pub summary: String,
    /// Commit author display name.
    pub author_name: String,
    /// Commit time formatted as RFC3339.
    pub committed_at: String,
}

// ---------------------------------------------------------------------------
// UnpushedBranch
// ---------------------------------------------------------------------------

/// A local branch that is ahead of its remote tracking branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnpushedBranch {
    /// Local branch name (e.g. `"feat/new-feature"`).
    pub name: String,
    /// Number of commits ahead of the remote tracking branch.
    ///
    /// When `has_upstream` is `false` this is the total number of commits
    /// reachable from the branch tip (via revwalk), **not** an ahead-of-remote
    /// count.
    pub ahead: u32,
    /// Whether the branch has a configured remote upstream.
    ///
    /// `false` means the branch has never been pushed; the `ahead` field then
    /// contains the total commit count rather than an upstream-relative delta.
    /// Defaults to `true` when deserialising older data that lacks this field.
    #[serde(default = "default_true")]
    pub has_upstream: bool,
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// GithubInfo
// ---------------------------------------------------------------------------

/// GitHub-specific metadata for a repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GithubInfo {
    /// Number of open issues.
    #[serde(default)]
    pub open_issues: Option<u64>,
    /// Number of open pull requests.
    #[serde(default)]
    pub open_prs: Option<u64>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Derive a human-readable repository name from its path.
///
/// Uses the last component of the path, or falls back to `"unknown"` if the
/// path has no file-name component.
///
/// This is intentionally `pub(crate)` — external callers should set names
/// explicitly or rely on [`crate::registry::Registry`] which calls this
/// function internally.
pub(crate) fn derive_name_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn repository_new_initialises_none_fields() {
        let repo = Repository::new("test", "/tmp/test");
        assert_eq!(repo.name, "test");
        assert_eq!(repo.path, PathBuf::from("/tmp/test"));
        assert!(repo.status.is_none());
        assert!(repo.last_scanned.is_none());
        assert!(repo.fetch.is_none());
    }

    #[test]
    fn repository_with_fetch_sets_field() {
        let repo = Repository::new("test", "/tmp/test").with_fetch(Some(true));
        assert_eq!(repo.fetch, Some(true));
        let repo = Repository::new("test", "/tmp/test").with_fetch(Some(false));
        assert_eq!(repo.fetch, Some(false));
        let repo = Repository::new("test", "/tmp/test").with_fetch(None);
        assert!(repo.fetch.is_none());
    }

    #[test]
    fn repo_status_default_is_clean() {
        let status = RepoStatus::default();
        assert_eq!(status.uncommitted_files, 0);
        assert!(status.unpushed_branches.is_empty());
        assert!(status.current_branch.is_none());
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
        assert!(status.github.is_none());
        assert!(status.conflict_predicted.is_none());
        assert!(status.recent_commits.is_empty());
    }

    // B1: backward-compat — old JSON without `conflict_predicted` must
    // deserialise with the field defaulting to `None`.
    #[test]
    fn repo_status_b1_old_json_without_conflict_predicted_defaults_to_none() {
        let json = r#"{
            "uncommitted_files": 0,
            "unpushed_branches": [],
            "current_branch": "main",
            "ahead": 0,
            "behind": 0
        }"#;
        let status: RepoStatus = serde_json::from_str(json).expect("deserialize");
        assert_eq!(status.conflict_predicted, None);
    }

    #[test]
    fn derive_name_normal_path() {
        assert_eq!(
            derive_name_from_path(Path::new("/home/user/my-project")),
            "my-project"
        );
    }

    #[test]
    fn derive_name_trailing_slash() {
        // PathBuf normalises trailing slashes
        assert_eq!(
            derive_name_from_path(Path::new("/home/user/my-project/")),
            "my-project"
        );
    }

    #[test]
    fn derive_name_root_path() {
        assert_eq!(derive_name_from_path(Path::new("/")), "unknown");
    }

    #[test]
    fn unpushed_branch_has_upstream_defaults_to_true_on_deserialize() {
        // Old JSON without `has_upstream` must deserialise with has_upstream=true
        let json = r#"{"name":"feat/x","ahead":2}"#;
        let branch: UnpushedBranch = serde_json::from_str(json).expect("deserialize");
        assert!(branch.has_upstream);
    }

    #[test]
    fn repository_serde_roundtrip() {
        let repo = Repository {
            name: "test-repo".to_string(),
            path: PathBuf::from("/tmp/test-repo"),
            status: Some(RepoStatus {
                fetch_warning: None,
                uncommitted_files: 3,
                unpushed_branches: vec![UnpushedBranch {
                    name: "feat/x".to_string(),
                    ahead: 2,
                    has_upstream: true,
                }],
                current_branch: Some("main".to_string()),
                ahead: 1,
                behind: 0,
                github: Some(GithubInfo {
                    open_issues: Some(2),
                    open_prs: Some(1),
                }),
                conflict_predicted: None,
                recent_commits: vec![],
            }),
            last_scanned: Some(Utc::now()),
            fetch: Some(true),
            distro: None,
        };
        let json = serde_json::to_string(&repo).expect("serialize");
        let back: Repository = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(repo.name, back.name);
        assert_eq!(repo.path, back.path);
        assert_eq!(
            repo.status.as_ref().unwrap().uncommitted_files,
            back.status.as_ref().unwrap().uncommitted_files,
        );
    }

    #[test]
    fn repository_with_distro_sets_field() {
        let repo = Repository::new("test", "/tmp/test").with_distro(Some("Ubuntu".into()));
        assert_eq!(repo.distro, Some("Ubuntu".into()));
    }
}
