//! Git repository status scanning.
//!
//! This module provides [`scan`], a **synchronous** function that inspects a
//! single git repository and returns a [`RepoStatus`] describing uncommitted
//! changes and unpushed branches.
//!
//! Async callers (CLI / GUI) should wrap the call with
//! `tokio::task::spawn_blocking` to keep the runtime responsive.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::model::{RecentCommit, RepoStatus, UnpushedBranch};

/// Number of newest `HEAD` commits included in [`RepoStatus::recent_commits`].
pub const DEFAULT_RECENT_COMMITS_LIMIT: usize = 5;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur while scanning a repository.
#[derive(Debug, Error)]
pub enum StatusError {
    /// The supplied path is not absolute.
    #[error("repository path must be absolute: {0}")]
    RelativePath(PathBuf),

    /// The path does not exist.
    #[error("path not found: {0}")]
    PathNotFound(PathBuf),

    /// The path exists but is not a git repository.
    #[error("not a git repository: {0}")]
    NotARepository(PathBuf),

    /// Bare repositories are not supported.
    #[error("bare repositories are not supported: {0}")]
    BareRepository(PathBuf),

    /// `Repository::open` failed for a reason other than "not found".
    #[error("failed to open repository at {path}: {source}")]
    OpenRepository { path: PathBuf, source: git2::Error },

    /// A git operation failed.
    #[error("git operation failed ({context}): {source}")]
    Git {
        context: String,
        source: git2::Error,
    },
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build a closure that wraps a [`git2::Error`] with the given context string.
///
/// Usage: `.map_err(git_ctx("listing branches"))`
fn git_ctx(context: &str) -> impl FnOnce(git2::Error) -> StatusError + '_ {
    move |source| StatusError::Git {
        context: context.to_string(),
        source,
    }
}

/// Internal representation of the HEAD state.
enum HeadState {
    /// HEAD points to a named branch.
    Branch { name: String },
    /// HEAD is detached.
    Detached,
    /// The repository has no commits yet (unborn branch).
    Unborn,
}

/// Open and validate the repository at `path`.
///
/// Rejects relative paths, non-git directories, and bare repositories.
fn open_repo(path: &Path) -> Result<git2::Repository, StatusError> {
    if !path.is_absolute() {
        return Err(StatusError::RelativePath(path.to_path_buf()));
    }
    // `try_exists` distinguishes a missing path from an unreadable one; only a
    // definite "does not exist" is reported as PathNotFound.
    if matches!(path.try_exists(), Ok(false)) {
        return Err(StatusError::PathNotFound(path.to_path_buf()));
    }

    match git2::Repository::open(path) {
        Ok(repo) => {
            if repo.is_bare() {
                Err(StatusError::BareRepository(path.to_path_buf()))
            } else {
                Ok(repo)
            }
        }
        Err(e)
            if e.code() == git2::ErrorCode::NotFound
                && matches!(
                    e.class(),
                    git2::ErrorClass::Repository | git2::ErrorClass::Os
                ) =>
        {
            // Covers "path does not exist" (Os) and "path is not a git repo"
            // (Repository).  Other NotFound classes (e.g. Odb, Ref) are not
            // expected here and are forwarded as OpenRepository errors.
            Err(StatusError::NotARepository(path.to_path_buf()))
        }
        Err(e) => Err(StatusError::OpenRepository {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

/// Count files with uncommitted changes (staged, unstaged, untracked).
///
/// Each file is counted once even if it has both index and work-tree changes.
fn count_uncommitted(repo: &git2::Repository) -> Result<u32, StatusError> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .exclude_submodules(true)
        .renames_head_to_index(false)
        .renames_index_to_workdir(false);

    let statuses = repo
        .statuses(Some(&mut opts))
        .map_err(git_ctx("getting repository status"))?;

    use git2::Status;
    let relevant = Status::INDEX_NEW
        | Status::INDEX_MODIFIED
        | Status::INDEX_DELETED
        | Status::INDEX_RENAMED
        | Status::INDEX_TYPECHANGE
        | Status::WT_NEW
        | Status::WT_MODIFIED
        | Status::WT_DELETED
        | Status::WT_TYPECHANGE
        | Status::WT_RENAMED
        | Status::CONFLICTED;

    let count = statuses
        .iter()
        .filter(|e| e.status().intersects(relevant))
        .count();

    Ok(count as u32)
}

/// Determine the current HEAD state.
fn read_head_state(repo: &git2::Repository) -> Result<HeadState, StatusError> {
    let head = match repo.head() {
        Ok(h) => h,
        Err(e)
            if e.code() == git2::ErrorCode::UnbornBranch
                || e.code() == git2::ErrorCode::NotFound =>
        {
            return Ok(HeadState::Unborn);
        }
        Err(e) => return Err(git_ctx("reading HEAD")(e)),
    };

    if head.is_branch() {
        let name = head
            .shorthand()
            .map_err(|source| StatusError::Git {
                context: "branch name is not valid UTF-8".to_string(),
                source,
            })?
            .to_string();
        Ok(HeadState::Branch { name })
    } else {
        Ok(HeadState::Detached)
    }
}

/// Enumerate all local branches and collect those that are ahead of their
/// upstream (or, when there is no upstream, ahead of nothing — i.e. all
/// commits are considered unpushed).
///
/// When `is_linked` is `true` (linked worktree), only the current branch
/// (identified via `head_state`) is evaluated — other branches are skipped to
/// avoid duplicating entries that are already reported by the main worktree.
///
/// Returns `(unpushed_branches, head_ahead, head_behind)`.
fn collect_unpushed(
    repo: &git2::Repository,
    head_state: &HeadState,
    is_linked: bool,
) -> Result<(Vec<UnpushedBranch>, u32, u32), StatusError> {
    let mut unpushed: Vec<UnpushedBranch> = Vec::new();
    let mut head_ahead = 0u32;
    let mut head_behind = 0u32;

    let cfg = repo
        .config()
        .map_err(git_ctx("reading config to check upstream"))?;

    let branches = repo
        .branches(Some(git2::BranchType::Local))
        .map_err(git_ctx("listing branches"))?;

    for branch_result in branches {
        let (branch, _) = branch_result.map_err(git_ctx("iterating branches"))?;

        let branch_name = match branch.name() {
            Ok(Some(n)) => n.to_string(),
            _ => continue,
        };

        // In a linked worktree, skip all branches except the current one.
        // Other branches are reported by the main worktree scan to avoid
        // duplicate entries.
        let is_head = matches!(head_state, HeadState::Branch { name, .. } if *name == branch_name);
        if is_linked && !is_head {
            continue;
        }

        let branch_oid = match branch.get().target() {
            Some(oid) => oid,
            None => continue,
        };

        // Determine whether an upstream is configured for this branch by
        // checking BOTH `branch.<name>.remote` AND `branch.<name>.merge` in
        // the repository config. Git considers an upstream to be configured
        // only when both keys are present; treating "remote-only" or
        // "merge-only" as configured would either cause spurious errors or
        // silently mis-classify the branch.
        //
        // This also avoids silently treating a *broken* upstream (both keys
        // present but the remote-tracking ref is missing/gone) as "no
        // upstream", which would produce a misleading ahead-count equal to
        // all reachable commits rather than a proper error.
        let has_remote = cfg
            .get_string(&format!("branch.{branch_name}.remote"))
            .is_ok();
        let has_merge = cfg
            .get_string(&format!("branch.{branch_name}.merge"))
            .is_ok();
        let upstream_configured = has_remote && has_merge;

        let upstream_oid: Option<git2::Oid> = if !upstream_configured {
            // No upstream configured — all reachable commits are unpushed.
            None
        } else {
            // Upstream is configured; try to resolve the remote-tracking ref.
            // If resolution fails (e.g. the ref has been deleted / never
            // fetched) we return a hard error rather than silently treating
            // the branch as having no upstream.
            match branch.upstream() {
                Ok(up) => up.get().target(),
                Err(e) => {
                    return Err(StatusError::Git {
                        context: format!("resolving upstream of branch '{branch_name}'"),
                        source: e,
                    });
                }
            }
        };

        let (ahead, behind, has_upstream) = if let Some(up_oid) = upstream_oid {
            let (a, b) = repo
                .graph_ahead_behind(branch_oid, up_oid)
                .map_err(git_ctx("computing ahead/behind"))?;
            (a as u32, b as u32, true)
        } else {
            // No upstream — count all reachable commits as "unpushed".
            let mut walk = repo.revwalk().map_err(git_ctx("creating revwalk"))?;
            walk.push(branch_oid)
                .map_err(git_ctx("pushing branch OID to revwalk"))?;
            let total = walk.count() as u32;
            (total, 0, false)
        };

        // Update HEAD summary when we're looking at the current branch.
        // `is_head` was computed above (near the early-skip logic).
        if is_head {
            head_ahead = ahead;
            head_behind = behind;
        }

        if ahead > 0 {
            unpushed.push(UnpushedBranch {
                name: branch_name,
                ahead,
                has_upstream,
            });
        }
    }

    Ok((unpushed, head_ahead, head_behind))
}

fn commit_time_to_rfc3339(time: git2::Time) -> String {
    chrono::DateTime::from_timestamp(time.seconds(), 0)
        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn commit_summary(commit: &git2::Commit<'_>) -> String {
    let summary = commit.summary().ok().flatten().unwrap_or("").trim();
    if !summary.is_empty() {
        return summary.to_string();
    }

    let first_line = commit
        .message_bytes()
        .split(|b| *b == b'\n')
        .next()
        .unwrap_or_default();
    let lossy = String::from_utf8_lossy(first_line).trim().to_string();
    if lossy.is_empty() {
        "(no message)".to_string()
    } else {
        lossy
    }
}

fn author_name(commit: &git2::Commit<'_>) -> String {
    let name = String::from_utf8_lossy(commit.author().name_bytes())
        .trim()
        .to_string();
    if name.is_empty() {
        "unknown".to_string()
    } else {
        name
    }
}

fn collect_recent_commits(
    repo: &git2::Repository,
    limit: usize,
) -> Result<Vec<RecentCommit>, StatusError> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut walk = repo
        .revwalk()
        .map_err(git_ctx("creating recent-commits revwalk"))?;
    match walk.push_head() {
        Ok(()) => {}
        Err(e)
            if e.code() == git2::ErrorCode::UnbornBranch
                || e.code() == git2::ErrorCode::NotFound =>
        {
            return Ok(Vec::new());
        }
        Err(e) => return Err(git_ctx("pushing HEAD to recent-commits revwalk")(e)),
    }
    walk.set_sorting(git2::Sort::TIME | git2::Sort::TOPOLOGICAL)
        .map_err(git_ctx("sorting recent-commits revwalk"))?;

    let mut commits = Vec::with_capacity(limit);
    for oid_result in walk.take(limit) {
        let oid = oid_result.map_err(git_ctx("reading recent commit oid"))?;
        let commit = repo
            .find_commit(oid)
            .map_err(git_ctx("reading recent commit"))?;
        commits.push(RecentCommit {
            short_hash: oid.to_string().chars().take(7).collect(),
            summary: commit_summary(&commit),
            author_name: author_name(&commit),
            committed_at: commit_time_to_rfc3339(commit.time()),
        });
    }
    Ok(commits)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Options controlling how [`scan_with_options`] behaves.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusScanOptions {
    /// If `true`, run a best-effort `git fetch` of the current branch's upstream
    /// remote before computing ahead/behind and the conflict prediction.
    /// Failures are logged at `warn` level and do not abort the scan
    /// (the existing remote-tracking cache is used instead).
    pub fetch: bool,
}

/// Scan the git repository at `path` and return its current status.
///
/// Equivalent to [`scan_with_options`] with [`StatusScanOptions::default()`].
///
/// # Errors
///
/// - [`StatusError::RelativePath`] — `path` is not absolute.
/// - [`StatusError::NotARepository`] — `path` does not exist or is not a git
///   repository.
/// - [`StatusError::BareRepository`] — the repository is bare.
/// - [`StatusError::OpenRepository`] — opening the repository failed for
///   another reason.
/// - [`StatusError::Git`] — a git operation failed after the repository was
///   opened.
///
/// # Linked worktrees
///
/// When `path` points to a *linked* worktree (`git worktree add`), the
/// returned `unpushed_branches` is restricted to the worktree's current
/// branch only. Other branches are intentionally omitted because they are
/// reported by the main worktree's `RepoStatus`, which would otherwise
/// produce duplicate entries. `uncommitted_files`, `current_branch`,
/// `ahead`, and `behind` are computed against the worktree's own HEAD.
pub fn scan(path: &Path) -> Result<RepoStatus, StatusError> {
    scan_with_options(path, StatusScanOptions::default())
}

/// Scan the git repository at `path` with the given [`StatusScanOptions`].
///
/// When `opts.fetch` is `true`, the current branch's upstream remote is
/// fetched (best-effort) before computing ahead/behind and the conflict
/// prediction. Fetch failures are logged at `warn` level and do not abort
/// the scan.
pub fn scan_with_options(path: &Path, opts: StatusScanOptions) -> Result<RepoStatus, StatusError> {
    scan_cancellable(path, opts, &std::sync::atomic::AtomicBool::new(false))
}

pub(crate) fn scan_cancellable(
    path: &Path,
    opts: StatusScanOptions,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<RepoStatus, StatusError> {
    let repo = open_repo(path)?;
    let is_linked = repo.is_worktree();

    let fetch_warning = if opts.fetch {
        if let Err(e) = try_fetch_current_upstream(&repo, cancelled) {
            tracing::warn!(path = %path.display(), error = %e, "fetch failed, using stale cache");
            Some(e.message().to_string())
        } else {
            None
        }
    } else {
        None
    };

    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return Err(StatusError::Git {
            context: "cancelled".into(),
            source: git2::Error::from_str("scan cancelled"),
        });
    }
    let uncommitted_files = count_uncommitted(&repo)?;
    let head_state = read_head_state(&repo)?;

    let (unpushed_branches, ahead, behind) = match (&head_state, is_linked) {
        (HeadState::Unborn, _) => (Vec::new(), 0, 0),
        // Linked worktree with detached HEAD: nothing to report.
        // The main worktree will still enumerate all named branches.
        (HeadState::Detached, true) => (Vec::new(), 0, 0),
        (state, _) => collect_unpushed(&repo, state, is_linked)?,
    };

    let current_branch = match head_state {
        HeadState::Branch { name, .. } => Some(name),
        _ => None,
    };

    let conflict_predicted = match predict_conflict(&repo) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "predict_conflict failed; reporting None");
            None
        }
    };

    let recent_commits = match collect_recent_commits(&repo, DEFAULT_RECENT_COMMITS_LIMIT) {
        Ok(commits) => commits,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "recent commits scan failed; reporting empty list");
            Vec::new()
        }
    };

    Ok(RepoStatus {
        fetch_warning,
        uncommitted_files,
        unpushed_branches,
        current_branch,
        ahead,
        behind,
        github: None,
        conflict_predicted,
        recent_commits,
        worktree_parent: if is_linked {
            worktree_parent_of(&repo)
        } else {
            None
        },
    })
}

/// Working directory of the main repository that owns a linked worktree.
///
/// `commondir()` is the main repository's git directory (normally
/// `<main>/.git/`); opening it yields the main working directory.
pub fn worktree_parent_of(repo: &git2::Repository) -> Option<PathBuf> {
    let common = repo.commondir();
    let main = git2::Repository::open(common).ok()?;
    main.workdir().map(normalize_workdir)
}

/// Strip the trailing separator git2 leaves on working directories.
pub(crate) fn normalize_workdir(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let trimmed = text.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return path.to_path_buf();
    }
    // git reports Windows paths with forward slashes; store native separators.
    if cfg!(windows) {
        PathBuf::from(trimmed.replace('/', "\\"))
    } else {
        PathBuf::from(trimmed)
    }
}

/// Best-effort `git fetch` of the current branch's upstream remote.
///
/// Returns `Ok(())` (no-op) when:
/// - HEAD is unborn or missing
/// - HEAD is detached
/// - The current branch has no upstream configured
///
/// Returns `Err` only when the underlying network/auth operation fails.
/// Configure libgit2 socket deadlines before starting any worker threads.
///
/// # Safety
/// libgit2 changes process-global settings without synchronization. Call this
/// once at process startup, before any thread can access libgit2.
pub unsafe fn configure_network_timeouts() -> Result<(), git2::Error> {
    git2::opts::set_server_connect_timeout_in_milliseconds(30_000)?;
    git2::opts::set_server_timeout_in_milliseconds(30_000)
}

fn try_fetch_current_upstream(
    repo: &git2::Repository,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<(), git2::Error> {
    #[cfg(test)]
    {
        let _ = cancelled;
        fetch_current_upstream_in_worker(repo)
    }
    #[cfg(not(test))]
    {
        crate::fetch_worker::fetch(repo, cancelled)
    }
}

pub(crate) fn fetch_current_upstream_in_worker(repo: &git2::Repository) -> Result<(), git2::Error> {
    let head = match repo.head() {
        Ok(h) => h,
        Err(e)
            if e.code() == git2::ErrorCode::UnbornBranch
                || e.code() == git2::ErrorCode::NotFound =>
        {
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    if !head.is_branch() {
        return Ok(());
    }
    let branch_full_refname = head.name()?.to_string();
    drop(head);
    let remote_name_buf = match repo.branch_upstream_remote(&branch_full_refname) {
        Ok(b) => b,
        Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let remote_name = remote_name_buf.as_str()?;

    let mut remote = repo.find_remote(remote_name)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.transfer_progress(move |_| std::time::Instant::now() < deadline);
    callbacks.credentials(|url, username_from_url, allowed_types| {
        if allowed_types.contains(git2::CredentialType::SSH_KEY) {
            if let Some(user) = username_from_url {
                return git2::Cred::ssh_key_from_agent(user);
            }
        }
        if allowed_types.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
            if let Ok(cfg) = repo.config() {
                return git2::Cred::credential_helper(&cfg, url, username_from_url);
            }
        }
        if allowed_types.contains(git2::CredentialType::DEFAULT) {
            return git2::Cred::default();
        }
        Err(git2::Error::from_str("no supported credential type"))
    });
    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(callbacks);
    fo.download_tags(git2::AutotagOption::None);
    remote.fetch::<&str>(&[], Some(&mut fo), None)?;
    Ok(())
}

/// Predict whether a `pull` (merge of upstream into the current branch) would
/// conflict, by comparing the file paths changed on each side of the
/// merge-base.
///
/// Returns:
/// - `Ok(None)` when prediction is not applicable (no upstream / detached /
///   unborn / no merge-base / both sides at the same commit / pure
///   fast-forward in either direction).
/// - `Ok(Some(true))` when at least one path is modified (or
///   added/deleted/renamed-without-detection) on **both** sides — this is a
///   path-level overlap, treated as a likely-conflict signal.
/// - `Ok(Some(false))` when the changed-path sets are disjoint.
///
/// Limitations (documented as spec):
/// - Rename detection is OFF (performance trade-off). A rename combined with
///   a modification of the old path on the other side will be reported as a
///   conflict because the old path appears in both diffs.
/// - Submodule entries are ignored.
/// - This is a path-level heuristic, not a real 3-way merge simulation.
fn predict_conflict(repo: &git2::Repository) -> Result<Option<bool>, git2::Error> {
    use std::collections::HashSet;

    // HEAD that points at something other than a branch (detached) is a
    // normal "no prediction possible" case → Ok(None). Unborn or genuinely
    // missing HEAD is also Ok(None). Any other libgit2 error (e.g. corrupt
    // refs DB) propagates so the caller can log it with context.
    let head = match repo.head() {
        Ok(h) if h.is_branch() => h,
        Ok(_) => return Ok(None),
        Err(e)
            if e.code() == git2::ErrorCode::UnbornBranch
                || e.code() == git2::ErrorCode::NotFound =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let local_oid = head
        .target()
        .ok_or_else(|| git2::Error::from_str("no head target"))?;
    let branch = git2::Branch::wrap(head);
    // Missing upstream config → Ok(None). Any other error (broken ref,
    // I/O, etc.) propagates rather than being silently swallowed.
    let upstream = match branch.upstream() {
        Ok(u) => u,
        Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let upstream_oid = upstream
        .get()
        .target()
        .ok_or_else(|| git2::Error::from_str("no upstream target"))?;

    if local_oid == upstream_oid {
        return Ok(None);
    }

    let base_oid = match repo.merge_base(local_oid, upstream_oid) {
        Ok(b) => b,
        Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };

    // Pure fast-forward in either direction → nothing to predict.
    if base_oid == local_oid || base_oid == upstream_oid {
        return Ok(None);
    }

    let base_tree = repo.find_commit(base_oid)?.tree()?;
    let local_tree = repo.find_commit(local_oid)?.tree()?;
    let upstream_tree = repo.find_commit(upstream_oid)?.tree()?;

    let mut opts = git2::DiffOptions::new();
    opts.include_untracked(false)
        .ignore_submodules(true)
        .skip_binary_check(true);
    let diff_local =
        repo.diff_tree_to_tree(Some(&base_tree), Some(&local_tree), Some(&mut opts))?;

    let mut opts2 = git2::DiffOptions::new();
    opts2
        .include_untracked(false)
        .ignore_submodules(true)
        .skip_binary_check(true);
    let diff_up =
        repo.diff_tree_to_tree(Some(&base_tree), Some(&upstream_tree), Some(&mut opts2))?;

    let mut local_paths: HashSet<Vec<u8>> = HashSet::new();
    for d in diff_local.deltas() {
        if let Some(p) = d.new_file().path() {
            local_paths.insert(p.as_os_str().as_encoded_bytes().to_vec());
        }
        if let Some(p) = d.old_file().path() {
            local_paths.insert(p.as_os_str().as_encoded_bytes().to_vec());
        }
    }
    for d in diff_up.deltas() {
        if let Some(p) = d.new_file().path() {
            if local_paths.contains(p.as_os_str().as_encoded_bytes()) {
                return Ok(Some(true));
            }
        }
        if let Some(p) = d.old_file().path() {
            if local_paths.contains(p.as_os_str().as_encoded_bytes()) {
                return Ok(Some(true));
            }
        }
    }
    Ok(Some(false))
}

// ===========================================================================
// Unit tests (U1 – U8)
// ===========================================================================

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_libgit2_supports_network_fetch_transports() {
        let version = git2::Version::get();
        assert!(version.https(), "HTTPS fetch support must be compiled in");
        assert!(version.ssh(), "SSH fetch support must be compiled in");
    }

    use super::*;

    // U1 ─────────────────────────────────────────────────────────────────────
    #[test]
    fn scan_rejects_relative_path() {
        let result = scan(Path::new("relative/path"));
        assert!(
            matches!(result, Err(StatusError::RelativePath(_))),
            "expected RelativePath, got {result:?}"
        );
    }

    // U2 ─────────────────────────────────────────────────────────────────────
    #[test]
    fn scan_rejects_nonexistent_path() {
        // Create a tempdir, capture its path, then immediately drop it so the
        // directory is deleted.  This gives us a guaranteed-absolute path that
        // does not exist, without relying on a hardcoded OS-specific string
        // (e.g. `/tmp/...` is not an absolute path on Windows).
        let path = {
            let dir = tempfile::tempdir().expect("tempdir");
            let p = dir.path().to_path_buf();
            drop(dir); // directory is deleted here
            p
        };
        let result = scan(&path);
        assert!(
            matches!(result, Err(StatusError::PathNotFound(_))),
            "expected PathNotFound, got {result:?}"
        );
    }

    // U3 ─────────────────────────────────────────────────────────────────────
    #[test]
    fn scan_rejects_non_git_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = scan(dir.path());
        assert!(
            matches!(result, Err(StatusError::NotARepository(_))),
            "expected NotARepository, got {result:?}"
        );
    }

    // U4 ─────────────────────────────────────────────────────────────────────
    #[test]
    fn scan_rejects_bare_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        git2::Repository::init_bare(dir.path()).expect("init bare");
        let result = scan(dir.path());
        assert!(
            matches!(result, Err(StatusError::BareRepository(_))),
            "expected BareRepository, got {result:?}"
        );
    }

    // -------------------------------------------------------------------
    // C1 – C11: predict_conflict scenarios
    // -------------------------------------------------------------------

    use git2::{Repository, Signature};
    use std::path::PathBuf;

    /// Initialise a non-bare repository and a sibling bare upstream, with
    /// `origin` configured. Performs the initial commit on both sides so the
    /// repo has an HEAD on `main` tracking `origin/main`.
    fn init_repo_with_upstream() -> (tempfile::TempDir, Repository, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = tmp.path().join("work");
        let bare = tmp.path().join("bare.git");
        std::fs::create_dir_all(&work).expect("mkdir work");
        Repository::init_bare(&bare).expect("init bare");
        let repo = Repository::init(&work).expect("init work");
        // initial commit
        commit_file(&repo, "README.md", "init\n", "initial");
        // determine current head branch (may be "main" or "master")
        let head_branch = {
            let head = repo.head().expect("head");
            head.shorthand().unwrap_or("master").to_string()
        };
        // configure origin
        repo.remote("origin", &bare.to_string_lossy())
            .expect("add remote");
        // push initial commit to origin so upstream tracking can be set
        {
            let mut remote = repo.find_remote("origin").expect("find remote");
            remote
                .push(
                    &[&format!("refs/heads/{0}:refs/heads/{0}", head_branch)],
                    None,
                )
                .expect("push initial");
        }
        // set upstream
        {
            let mut br = repo
                .find_branch(&head_branch, git2::BranchType::Local)
                .expect("local branch");
            br.set_upstream(Some(&format!("origin/{}", head_branch)))
                .expect("set upstream");
        }
        (tmp, repo, bare)
    }

    fn commit_file(repo: &Repository, path: &str, content: &str, msg: &str) -> git2::Oid {
        let workdir = repo.workdir().expect("workdir").to_path_buf();
        let full = workdir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parent");
        }
        std::fs::write(&full, content).expect("write file");
        let mut idx = repo.index().expect("index");
        idx.add_path(Path::new(path)).expect("add path");
        idx.write().expect("write index");
        let tree_oid = idx.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_oid).expect("find tree");
        let sig = Signature::now("test", "test@example.com").expect("sig");
        let parents: Vec<git2::Commit> = match repo.head() {
            Ok(h) => h
                .target()
                .and_then(|o| repo.find_commit(o).ok())
                .into_iter()
                .collect(),
            Err(_) => vec![],
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parent_refs)
            .expect("commit")
    }

    /// Make an "upstream-side" commit by pushing a new commit from `repo`
    /// to `origin`, then resetting `repo` back to where it was, then setting
    /// the local `refs/remotes/origin/<branch>` to the new upstream commit.
    /// Returns the new upstream OID. This avoids needing a second working
    /// repo for the upstream's commits.
    fn upstream_commit(repo: &Repository, path: &str, content: &str, msg: &str) -> git2::Oid {
        let head = repo.head().expect("head");
        let branch_name = head.shorthand().expect("shorthand").to_string();
        let saved_oid = head.target().expect("target");

        let new_oid = commit_file(repo, path, content, msg);

        let mut remote = repo.find_remote("origin").expect("origin");
        remote
            .push(
                &[&format!("refs/heads/{0}:refs/heads/{0}", branch_name)],
                None,
            )
            .expect("push upstream commit");

        // Reset the local branch back to the saved oid so the local side
        // stays as it was before the upstream commit.
        let saved_commit = repo.find_commit(saved_oid).expect("saved commit");
        repo.reset(saved_commit.as_object(), git2::ResetType::Hard, None)
            .expect("reset");

        // Update remote-tracking ref to point to new upstream commit.
        let ref_name = format!("refs/remotes/origin/{}", branch_name);
        repo.reference(&ref_name, new_oid, true, "upstream commit")
            .expect("update remote ref");

        new_oid
    }

    fn current_branch_short(repo: &Repository) -> String {
        repo.head()
            .ok()
            .and_then(|h| h.shorthand().ok().map(|s| s.to_string()))
            .unwrap_or_else(|| "main".into())
    }

    // C1: upstream not configured → None
    #[test]
    fn c1_no_upstream_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = Repository::init(dir.path()).expect("init");
        commit_file(&repo, "a.txt", "hi", "c1");
        assert_eq!(predict_conflict(&repo).expect("predict"), None);
    }

    // C2: detached HEAD → None
    #[test]
    fn c2_detached_head_returns_none() {
        let (_tmp, repo, _bare) = init_repo_with_upstream();
        let oid = repo.head().unwrap().target().unwrap();
        repo.set_head_detached(oid).expect("detach");
        assert_eq!(predict_conflict(&repo).expect("predict"), None);
    }

    // C3: unborn branch → None
    #[test]
    fn c3_unborn_branch_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = Repository::init(dir.path()).expect("init");
        // No commits → head() errors → None
        assert_eq!(predict_conflict(&repo).expect("predict"), None);
    }

    // C4: disjoint history (no merge-base) → None
    #[test]
    fn c4_disjoint_history_returns_none() {
        let (_tmp, repo, _bare) = init_repo_with_upstream();
        let branch_name = current_branch_short(&repo);

        // Make a local divergent commit.
        commit_file(&repo, "local.txt", "local\n", "local change");

        // Build a fully-disjoint commit (no parents) and point the
        // remote-tracking ref at it.
        let sig = Signature::now("u", "u@example.com").unwrap();
        let mut idx = repo.index().unwrap();
        idx.clear().unwrap();
        let workdir = repo.workdir().unwrap().to_path_buf();
        std::fs::write(workdir.join("orphan.txt"), "orphan\n").unwrap();
        idx.add_path(Path::new("orphan.txt")).unwrap();
        let tree_oid = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let orphan_oid = repo
            .commit(None, &sig, &sig, "orphan", &tree, &[])
            .expect("orphan commit");
        let ref_name = format!("refs/remotes/origin/{}", branch_name);
        repo.reference(&ref_name, orphan_oid, true, "orphan upstream")
            .expect("update remote ref");

        assert_eq!(predict_conflict(&repo).expect("predict"), None);
    }

    // C5: local == upstream → None
    #[test]
    fn c5_local_equals_upstream_returns_none() {
        let (_tmp, repo, _bare) = init_repo_with_upstream();
        assert_eq!(predict_conflict(&repo).expect("predict"), None);
    }

    // C6: local ahead only → None (upstream == merge-base)
    #[test]
    fn c6_local_ahead_only_returns_none() {
        let (_tmp, repo, _bare) = init_repo_with_upstream();
        commit_file(&repo, "ahead.txt", "ahead\n", "ahead");
        assert_eq!(predict_conflict(&repo).expect("predict"), None);
    }

    // C7: upstream ahead only (pure fast-forward) → None
    #[test]
    fn c7_upstream_ahead_only_returns_none() {
        let (_tmp, repo, _bare) = init_repo_with_upstream();
        upstream_commit(&repo, "u.txt", "u\n", "upstream only");
        assert_eq!(predict_conflict(&repo).expect("predict"), None);
    }

    // C8: divergent, disjoint paths → Some(false)
    #[test]
    fn c8_divergent_disjoint_paths_returns_false() {
        let (_tmp, repo, _bare) = init_repo_with_upstream();
        upstream_commit(&repo, "up.txt", "u\n", "upstream change");
        commit_file(&repo, "loc.txt", "l\n", "local change");
        assert_eq!(predict_conflict(&repo).expect("predict"), Some(false));
    }

    // C9: divergent, same path on both sides → Some(true)
    #[test]
    fn c9_divergent_same_path_returns_true() {
        let (_tmp, repo, _bare) = init_repo_with_upstream();
        upstream_commit(&repo, "shared.txt", "upstream content\n", "u edit");
        commit_file(&repo, "shared.txt", "local content\n", "l edit");
        assert_eq!(predict_conflict(&repo).expect("predict"), Some(true));
    }

    // C10: one side deletes, other modifies same path → Some(true)
    #[test]
    fn c10_delete_vs_modify_returns_true() {
        let (_tmp, repo, _bare) = init_repo_with_upstream();
        // Commit a baseline file on both sides (in main HEAD).
        commit_file(&repo, "victim.txt", "base\n", "baseline");
        // Push baseline to upstream.
        let head_branch = current_branch_short(&repo);
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(
                &[&format!("refs/heads/{0}:refs/heads/{0}", head_branch)],
                None,
            )
            .unwrap();

        // Upstream-side: modify victim.txt
        upstream_commit(&repo, "victim.txt", "upstream mod\n", "u mod");

        // Local-side: delete victim.txt
        let workdir = repo.workdir().unwrap().to_path_buf();
        std::fs::remove_file(workdir.join("victim.txt")).unwrap();
        let mut idx = repo.index().unwrap();
        idx.remove_path(Path::new("victim.txt")).unwrap();
        idx.write().unwrap();
        let tree_oid = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("t", "t@example.com").unwrap();
        let parent = repo
            .find_commit(repo.head().unwrap().target().unwrap())
            .unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "delete victim", &tree, &[&parent])
            .unwrap();

        assert_eq!(predict_conflict(&repo).expect("predict"), Some(true));
    }

    // C11: rename detection OFF: upstream renames, local edits old name.
    // With rename detection off, both sides touch "old.txt" so we report
    // Some(true). Documented as a known limitation of this minimal predictor.
    #[test]
    fn c11_rename_vs_edit_old_name_returns_true_with_rename_off() {
        let (_tmp, repo, _bare) = init_repo_with_upstream();
        // Baseline file on both sides
        commit_file(&repo, "old.txt", "base\n", "baseline");
        let head_branch = current_branch_short(&repo);
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(
                &[&format!("refs/heads/{0}:refs/heads/{0}", head_branch)],
                None,
            )
            .unwrap();

        // Upstream: rename old.txt -> new.txt (touches both old & new paths
        // when rename detection is off — the diff sees a delete of "old.txt"
        // and an add of "new.txt").
        let saved_oid = repo.head().unwrap().target().unwrap();
        let workdir = repo.workdir().unwrap().to_path_buf();
        std::fs::rename(workdir.join("old.txt"), workdir.join("new.txt")).unwrap();
        let mut idx = repo.index().unwrap();
        idx.remove_path(Path::new("old.txt")).unwrap();
        idx.add_path(Path::new("new.txt")).unwrap();
        idx.write().unwrap();
        let tree_oid = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("u", "u@example.com").unwrap();
        let parent = repo.find_commit(saved_oid).unwrap();
        let upstream_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "rename", &tree, &[&parent])
            .unwrap();
        // Push and reset back so this becomes upstream-only.
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(
                &[&format!("refs/heads/{0}:refs/heads/{0}", head_branch)],
                None,
            )
            .unwrap();
        let saved_commit = repo.find_commit(saved_oid).unwrap();
        repo.reset(saved_commit.as_object(), git2::ResetType::Hard, None)
            .unwrap();
        let ref_name = format!("refs/remotes/origin/{}", head_branch);
        repo.reference(&ref_name, upstream_oid, true, "rename upstream")
            .unwrap();

        // Local: edit old.txt
        commit_file(&repo, "old.txt", "local edit\n", "edit old");

        assert_eq!(predict_conflict(&repo).expect("predict"), Some(true));
    }

    // C12: merge_base NotFound → Ok(None) (disjoint history returns None, not Err)
    // This is already verified by C4, but add explicit name for regression guard.
    #[test]
    fn c12_merge_base_not_found_returns_none() {
        use std::path::Path;
        let (_tmp, repo, _bare) = init_repo_with_upstream();
        let branch_name = current_branch_short(&repo);

        commit_file(&repo, "local2.txt", "local\n", "local change");

        let sig = Signature::now("u", "u@example.com").unwrap();
        let mut idx = repo.index().unwrap();
        idx.clear().unwrap();
        let workdir = repo.workdir().unwrap().to_path_buf();
        std::fs::write(workdir.join("orphan2.txt"), "orphan\n").unwrap();
        idx.add_path(Path::new("orphan2.txt")).unwrap();
        let tree_oid = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let orphan_oid = repo
            .commit(None, &sig, &sig, "orphan", &tree, &[])
            .expect("orphan commit");
        let ref_name = format!("refs/remotes/origin/{}", branch_name);
        repo.reference(&ref_name, orphan_oid, true, "orphan upstream")
            .expect("update remote ref");

        let result = predict_conflict(&repo);
        assert_eq!(result.expect("must be Ok, not Err"), None);
    }

    // -------------------------------------------------------------------
    // F1 – F3: fetch behaviour via scan_with_options
    // -------------------------------------------------------------------

    // F1: with fetch=false, scan must not access the remote.
    // Verified by deleting the bare upstream and confirming scan still Ok.
    #[test]
    fn f1_no_fetch_does_not_touch_remote() {
        let (_tmp, repo, bare) = init_repo_with_upstream();
        let workdir = repo.workdir().unwrap().to_path_buf();
        // Drop reference to repo so fs operations are clean
        drop(repo);
        std::fs::remove_dir_all(&bare).expect("rm bare");

        let result = scan_with_options(&workdir, StatusScanOptions { fetch: false });
        assert!(result.is_ok(), "scan failed: {:?}", result.err());
    }

    // F2: with fetch=true and a valid file:// remote, scan succeeds and
    // upstream tracking refs reflect the fetched state.
    #[test]
    fn f2_fetch_true_succeeds_against_local_remote() {
        let (_tmp, repo, _bare) = init_repo_with_upstream();
        let workdir = repo.workdir().unwrap().to_path_buf();
        drop(repo);
        let result = scan_with_options(&workdir, StatusScanOptions { fetch: true });
        assert!(result.is_ok(), "scan failed: {:?}", result.err());
    }

    // F3: fetch=true with a broken remote URL → scan still returns Ok
    // (fetch error is logged as warn, not propagated).
    #[test]
    fn f3_fetch_true_with_bad_remote_still_ok() {
        let (_tmp, repo, _bare) = init_repo_with_upstream();
        // Replace origin URL with a non-existent path.
        repo.remote_set_url("origin", "file:///nonexistent/path-does-not-exist.git")
            .expect("rewrite url");
        let workdir = repo.workdir().unwrap().to_path_buf();
        drop(repo);
        let result = scan_with_options(&workdir, StatusScanOptions { fetch: true });
        assert!(
            result.is_ok(),
            "scan must be Ok even on fetch failure: {:?}",
            result.err()
        );
        let cached = result.unwrap();
        assert!(
            cached.fetch_warning.is_some(),
            "failed fetch must remain visible"
        );
        let retry = scan_with_options(&workdir, StatusScanOptions { fetch: false }).unwrap();
        assert!(retry.fetch_warning.is_none());
        assert_eq!(cached.uncommitted_files, retry.uncommitted_files);
        assert_eq!(cached.unpushed_branches, retry.unpushed_branches);
    }

    // F4: fetch=true recovers when remote-tracking ref is missing
    // upstream config is set, but refs/remotes/origin/<branch> deleted.
    // With the fix, try_fetch_current_upstream no longer bails out on upstream().is_err()
    // → it fetches, restores the remote-tracking ref → collect_unpushed succeeds.
    #[test]
    fn f4_fetch_recovers_missing_remote_tracking_ref() {
        let (_tmp, repo, _bare) = init_repo_with_upstream();
        let branch_name = current_branch_short(&repo);
        let ref_name = format!("refs/remotes/origin/{}", branch_name);
        repo.find_reference(&ref_name)
            .expect("ref should exist before deletion")
            .delete()
            .expect("delete remote-tracking ref");
        let workdir = repo.workdir().unwrap().to_path_buf();
        drop(repo);
        let result = scan_with_options(&workdir, StatusScanOptions { fetch: true });
        assert!(
            result.is_ok(),
            "scan must succeed after fetch recovers missing tracking ref: {:?}",
            result.err()
        );
    }
}
