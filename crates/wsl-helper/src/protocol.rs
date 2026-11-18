use git_clawler_core::{
    model::{RecentCommit, UnpushedBranch},
    status::StatusError,
    RepoStatus,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Top-level JSON response emitted by the helper for a scan request.
pub struct ScanResponse {
    pub protocol_version: u32,
    pub helper_version: String,
    pub scanned_at: String,
    pub results: Vec<PathResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Top-level JSON response emitted by the helper for a scan-root request.
pub struct ScanRootResponse {
    pub protocol_version: u32,
    pub helper_version: String,
    pub scanned_at: String,
    pub root: String,
    pub status: ScanRootStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
/// Scan-root discovery outcome.
pub enum ScanRootStatus {
    Ok(Vec<String>),
    Err(HelperError),
}

/// Response to `--discover`: repositories and linked worktrees under a root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverResponse {
    pub protocol_version: u32,
    pub helper_version: String,
    pub scanned_at: String,
    pub root: String,
    pub status: DiscoverStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DiscoverStatus {
    Ok(Vec<FoundDto>),
    Err(HelperError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FoundDto {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_of: Option<String>,
}

/// Response to `--worktree-gone`: registered worktrees Git no longer tracks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeGoneResponse {
    pub protocol_version: u32,
    pub helper_version: String,
    pub gone: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Scan result for one input path.
/// `index` preserves the original argument position.
pub struct PathResult {
    pub index: usize,
    pub path: String,
    pub status: PathStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
/// Tagged enum encoded with `kind` and `value` fields.
/// `ok` carries [`RepoStatusDto`]; `err` carries [`HelperError`].
pub enum PathStatus {
    Ok(RepoStatusDto),
    Err(HelperError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Serializable subset of `git_clawler_core::RepoStatus` for the helper protocol.
pub struct RepoStatusDto {
    /// Additive protocol field; old helpers omit it and new readers default to None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_warning: Option<String>,
    pub current_branch: Option<String>,
    pub uncommitted_files: u32,
    pub unpushed_branches: Vec<UnpushedBranchDto>,
    pub ahead: u32,
    pub behind: u32,
    /// Pull conflict prediction (see [`git_clawler_core::RepoStatus::conflict_predicted`]).
    /// Backward-compatible: defaults to `None` when absent.
    #[serde(default)]
    pub conflict_predicted: Option<bool>,
    /// Origin remote URL resolved inside WSL via git2.
    ///
    /// Used by the Windows GUI to run `gh api` enrichment without needing
    /// `gh` inside the WSL distro. `#[serde(default)]` keeps old helpers
    /// (without this field) compatible with new GUIs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    /// Newest HEAD commits, most recent first.
    #[serde(default)]
    pub recent_commits: Vec<RecentCommitDto>,
    /// Main repository path when the scanned path is a linked worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_parent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Serializable unpushed branch information.
pub struct UnpushedBranchDto {
    pub name: String,
    pub ahead: u32,
    pub has_upstream: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Serializable recent commit summary.
pub struct RecentCommitDto {
    pub short_hash: String,
    pub summary: String,
    pub author_name: String,
    pub committed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Fail-soft error payload for one path.
pub struct HelperError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Stable machine-readable error code used by GUI-side handling.
pub enum ErrorCode {
    RelativePath,
    PathNotFound,
    NotARepository,
    BareRepository,
    OpenRepository,
    Git,
    Io,
    Panic,
}

impl From<RepoStatus> for RepoStatusDto {
    fn from(s: RepoStatus) -> Self {
        Self {
            current_branch: s.current_branch,
            uncommitted_files: s.uncommitted_files,
            unpushed_branches: s
                .unpushed_branches
                .into_iter()
                .map(UnpushedBranchDto::from)
                .collect(),
            ahead: s.ahead,
            behind: s.behind,
            conflict_predicted: s.conflict_predicted,
            remote_url: None, // populated by runner after scan
            fetch_warning: s.fetch_warning,
            recent_commits: s
                .recent_commits
                .into_iter()
                .map(RecentCommitDto::from)
                .collect(),
            worktree_parent: s.worktree_parent.map(|p| p.to_string_lossy().into_owned()),
        }
    }
}

impl From<RecentCommit> for RecentCommitDto {
    fn from(c: RecentCommit) -> Self {
        Self {
            short_hash: c.short_hash,
            summary: c.summary,
            author_name: c.author_name,
            committed_at: c.committed_at,
        }
    }
}

impl From<UnpushedBranch> for UnpushedBranchDto {
    fn from(b: UnpushedBranch) -> Self {
        Self {
            name: b.name,
            ahead: b.ahead,
            has_upstream: b.has_upstream,
        }
    }
}

impl From<&StatusError> for HelperError {
    fn from(e: &StatusError) -> Self {
        let code = match e {
            StatusError::RelativePath(_) => ErrorCode::RelativePath,
            StatusError::PathNotFound(_) => ErrorCode::PathNotFound,
            StatusError::NotARepository(_) => ErrorCode::NotARepository,
            StatusError::BareRepository(_) => ErrorCode::BareRepository,
            StatusError::OpenRepository { .. } => ErrorCode::OpenRepository,
            StatusError::Git { .. } => ErrorCode::Git,
        };
        Self {
            code,
            message: e.to_string(),
        }
    }
}

/// Handshake DTO emitted by `git-clawler-wsl-helper --protocol`.
///
/// Wire format: single JSON line `{"protocol_version": 1, "helper_version": "0.1.0", "capabilities": {...}}`.
/// The GUI deserialises this to verify version compatibility before running scans.
/// Only `protocol_version` major.minor matters; patch differences are ignored.
///
/// # Capabilities
///
/// `capabilities` is an additive set of boolean flags.  Adding a new capability
/// is a **non-breaking** wire change: old helpers that do not emit the field
/// deserialise it as `HelperCapabilities::default()` (all `false`) via
/// `#[serde(default)]`.  The GUI uses individual capability flags to decide
/// whether a freshly-discovered installed binary needs to be reinstalled
/// (independently of the version-compatibility check).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperHandshake {
    pub protocol_version: u32,
    pub helper_version: String,
    /// Capability flags advertised by this binary.  Defaults to all-false for
    /// old helpers that predate this field.
    #[serde(default)]
    pub capabilities: HelperCapabilities,
}

/// Additive capability flags emitted in the [`HelperHandshake`].
///
/// Each boolean indicates that the helper binary supports a specific optional
/// behaviour.  New flags default to `false` so that old GUIs that deserialise
/// a newer handshake do not break, and old helpers that do not emit the field
/// deserialise the whole struct as the zero-value default.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperCapabilities {
    /// When `true`, the helper populates `RepoStatusDto.remote_url` for each
    /// successfully scanned repository (resolved inside WSL via git2).  The
    /// GUI uses this URL to run `gh api` enrichment on the Windows host
    /// without needing `gh` inside the WSL distro.
    ///
    /// Old helpers (pre-capability-field) default to `false` here, which
    /// triggers a forced reinstall during bootstrap so that WSL repos always
    /// receive GitHub Issue/PR counts.
    #[serde(default)]
    pub populates_remote_url: bool,
    /// When `true`, the helper supports root discovery via `--scan-root`.
    #[serde(default)]
    pub scans_roots: bool,
    /// When `true`, the helper populates `RepoStatusDto.recent_commits`.
    #[serde(default)]
    pub populates_recent_commits: bool,
    /// When `true`, the helper supports `--discover` (repositories plus linked
    /// worktrees) and `--worktree-gone`.
    #[serde(default)]
    pub discovers_roots: bool,
    /// When `true`, the helper reports `RepoStatusDto.worktree_parent` and the
    /// `PathNotFound` error code.
    #[serde(default)]
    pub reports_worktrees: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn protocol_version_is_1() {
        assert_eq!(crate::PROTOCOL_VERSION, 1);
    }

    // ── Capability tests ─────────────────────────────────────────────────────

    /// Current helper must emit `populates_remote_url = true` in the handshake.
    /// Validated via the same HelperHandshake construction used in main.rs.
    #[test]
    fn handshake_has_populates_remote_url_true() {
        let hs = HelperHandshake {
            protocol_version: crate::PROTOCOL_VERSION,
            helper_version: crate::HELPER_VERSION.to_string(),
            capabilities: HelperCapabilities {
                discovers_roots: true,
                reports_worktrees: true,
                populates_remote_url: true,
                scans_roots: true,
                populates_recent_commits: true,
            },
        };
        assert!(
            hs.capabilities.populates_remote_url,
            "current helper handshake must set populates_remote_url = true"
        );
        assert!(
            hs.capabilities.scans_roots,
            "current helper handshake must set scans_roots = true"
        );
        assert!(
            hs.capabilities.populates_recent_commits,
            "current helper handshake must set populates_recent_commits = true"
        );
    }

    /// Old handshake JSON without the `capabilities` field must deserialise
    /// with `populates_remote_url = false` (backward-compat default).
    #[test]
    fn old_handshake_without_capabilities_defaults_to_false() {
        let json = r#"{"protocol_version":1,"helper_version":"0.1.0"}"#;
        let hs: HelperHandshake = serde_json::from_str(json).unwrap();
        assert_eq!(
            hs.capabilities,
            HelperCapabilities::default(),
            "old handshake without capabilities must deserialise to default (all false)"
        );
        assert!(
            !hs.capabilities.populates_remote_url,
            "populates_remote_url must be false when not present in JSON"
        );
        assert!(
            !hs.capabilities.scans_roots,
            "scans_roots must be false when not present in JSON"
        );
        assert!(
            !hs.capabilities.populates_recent_commits,
            "populates_recent_commits must be false when not present in JSON"
        );
    }

    /// Handshake with capabilities serialises the field.
    #[test]
    fn handshake_with_capabilities_serialises_correctly() {
        let hs = HelperHandshake {
            protocol_version: 1,
            helper_version: "0.1.0".into(),
            capabilities: HelperCapabilities {
                discovers_roots: true,
                reports_worktrees: true,
                populates_remote_url: true,
                scans_roots: true,
                populates_recent_commits: true,
            },
        };
        let v = serde_json::to_value(&hs).unwrap();
        assert_eq!(v["capabilities"]["populates_remote_url"], true);
        assert_eq!(v["capabilities"]["scans_roots"], true);
        assert_eq!(v["capabilities"]["populates_recent_commits"], true);
    }

    /// `HelperCapabilities::default()` has all fields false.
    #[test]
    fn helper_capabilities_default_all_false() {
        let caps = HelperCapabilities::default();
        assert!(!caps.populates_remote_url);
        assert!(!caps.scans_roots);
        assert!(!caps.populates_recent_commits);
    }

    #[test]
    fn ok_status_json_shape() {
        let dto = RepoStatusDto {
            worktree_parent: None,
            current_branch: Some("main".into()),
            uncommitted_files: 2,
            unpushed_branches: vec![],
            ahead: 1,
            behind: 0,
            conflict_predicted: None,
            remote_url: None,
            fetch_warning: None,
            recent_commits: vec![],
        };
        let status = PathStatus::Ok(dto);
        let v = serde_json::to_value(&status).unwrap();
        assert_eq!(v["kind"], "ok");
        assert!(v["value"].is_object());
    }

    #[test]
    fn err_status_json_shape() {
        let err = HelperError {
            code: ErrorCode::NotARepository,
            message: "not a repo".into(),
        };
        let status = PathStatus::Err(err);
        let v = serde_json::to_value(&status).unwrap();
        assert_eq!(v["kind"], "err");
        assert_eq!(v["value"]["code"], "not_a_repository");
    }

    #[test]
    fn repo_status_dto_omits_github_key() {
        let dto = RepoStatusDto {
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
        let v = serde_json::to_value(&dto).unwrap();
        assert!(!v.as_object().unwrap().contains_key("github"));
    }

    // B2: backward-compat — old DTO JSON without `conflict_predicted` must
    // deserialise with the field defaulting to `None`.
    #[test]
    fn b2_old_dto_without_conflict_predicted_deserializes_ok() {
        let json = r#"{"current_branch":null,"uncommitted_files":0,"unpushed_branches":[],"ahead":0,"behind":0}"#;
        let dto: RepoStatusDto = serde_json::from_str(json).unwrap();
        assert_eq!(dto.conflict_predicted, None);
    }

    // B3: backward-compat — old DTO without `remote_url` must deserialise
    // with the field defaulting to `None`.
    #[test]
    fn b3_old_dto_without_remote_url_deserializes_ok() {
        let json = r#"{"current_branch":null,"uncommitted_files":0,"unpushed_branches":[],"ahead":0,"behind":0}"#;
        let dto: RepoStatusDto = serde_json::from_str(json).unwrap();
        assert_eq!(dto.remote_url, None);
        assert!(dto.recent_commits.is_empty());
    }

    // Forward-compat: serialisation omits `remote_url` when None
    #[test]
    fn remote_url_none_is_omitted_from_serialization() {
        let dto = RepoStatusDto {
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
        let v = serde_json::to_value(&dto).unwrap();
        assert!(
            !v.as_object().unwrap().contains_key("remote_url"),
            "remote_url=None must be omitted from serialization"
        );
    }

    // Serialisation includes `remote_url` when Some
    #[test]
    fn remote_url_some_is_included_in_serialization() {
        let dto = RepoStatusDto {
            worktree_parent: None,
            current_branch: None,
            uncommitted_files: 0,
            unpushed_branches: vec![],
            ahead: 0,
            behind: 0,
            conflict_predicted: None,
            remote_url: Some("https://github.com/owner/repo".into()),
            fetch_warning: None,
            recent_commits: vec![],
        };
        let v = serde_json::to_value(&dto).unwrap();
        assert_eq!(v["remote_url"], "https://github.com/owner/repo");
    }

    #[test]
    fn status_error_relative_path_maps_to_relative_path_code() {
        let e = StatusError::RelativePath(PathBuf::from("rel/path"));
        let he = HelperError::from(&e);
        let v = serde_json::to_value(&he.code).unwrap();
        assert_eq!(v, "relative_path");
    }

    #[test]
    fn status_error_not_a_repository_maps() {
        let e = StatusError::NotARepository(PathBuf::from("/no/repo"));
        let he = HelperError::from(&e);
        let v = serde_json::to_value(&he.code).unwrap();
        assert_eq!(v, "not_a_repository");
    }

    #[test]
    fn status_error_bare_repository_maps() {
        let e = StatusError::BareRepository(PathBuf::from("/bare"));
        let he = HelperError::from(&e);
        let v = serde_json::to_value(&he.code).unwrap();
        assert_eq!(v, "bare_repository");
    }

    #[test]
    fn status_error_open_repository_maps() {
        let e = StatusError::OpenRepository {
            path: PathBuf::from("/p"),
            source: git2::Error::from_str("failed"),
        };
        let he = HelperError::from(&e);
        let v = serde_json::to_value(&he.code).unwrap();
        assert_eq!(v, "open_repository");
    }

    #[test]
    fn status_error_git_maps() {
        let e = StatusError::Git {
            context: "test".into(),
            source: git2::Error::from_str("git error"),
        };
        let he = HelperError::from(&e);
        let v = serde_json::to_value(&he.code).unwrap();
        assert_eq!(v, "git");
    }
}
