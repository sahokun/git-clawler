//! Recursive `.git` repository scanner.
//!
//! [`scan_for_repos`] walks a directory tree breadth-first and returns every
//! discovered git repository (directory containing a `.git` entry, either a
//! directory or a worktree pointer file). The scan is bounded by
//! [`ScanOptions::max_depth`] and skips a configurable list of basename
//! [`ScanOptions::excludes`] (e.g. `node_modules`, `target`).
//!
//! The scanner intentionally uses only `std::fs` (no `git2`) so it stays cheap
//! and side-effect free — verifying that a discovered path is actually a valid
//! repository is the responsibility of [`crate::status::scan`].

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::warn;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced by [`scan_for_repos`].
#[derive(Debug, Error)]
pub enum ScanError {
    #[error("scan root does not exist: {0}")]
    RootNotFound(PathBuf),
    #[error("scan root is not a directory: {0}")]
    RootNotADirectory(PathBuf),
    #[error("scan root must be absolute: {0}")]
    RootNotAbsolute(PathBuf),
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Options controlling [`scan_for_repos`] behaviour.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Maximum directory depth at which a `.git` may be found.
    /// Depth 0 = root itself is examined; depth N means repos may exist up to
    /// N directory levels below root (inclusive). `0` means only root is examined.
    pub max_depth: u32,
    /// Basenames to skip during traversal (e.g. `node_modules`, `target`).
    pub excludes: Vec<String>,
    /// Whether to follow symbolic links when traversing.
    pub follow_symlinks: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_depth: 3,
            excludes: default_excludes(),
            follow_symlinks: false,
        }
    }
}

/// Default list of basenames excluded from recursive scans.
pub fn default_excludes() -> Vec<String> {
    [
        "node_modules",
        "target",
        ".venv",
        "venv",
        "__pycache__",
        "dist",
        "build",
        ".cache",
        ".idea",
        ".vscode",
        "vendor",
        ".tox",
        ".gradle",
        ".next",
        ".nuxt",
        ".terraform",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Check whether `name` matches any entry in `excludes`.
///
/// On Windows, comparison is ASCII-case-insensitive (filesystem is
/// case-insensitive); on all other platforms exact equality is used.
pub(crate) fn matches_exclude(excludes: &HashSet<String>, name: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        let lowered = name.to_ascii_lowercase();
        excludes.iter().any(|e| e.to_ascii_lowercase() == lowered)
    }
    #[cfg(not(target_os = "windows"))]
    {
        excludes.contains(name)
    }
}

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

/// Recursively scan `root` for directories that contain a `.git` entry.
///
/// Behaviour:
/// - If `root` itself is a repository (`root/.git` exists), returns `[root]`
///   without recursing.
/// - When a subdirectory is identified as a repository, the scanner records it
///   and does **not** descend into it (nested repos under a repo are skipped).
/// - Directories whose basename appears in `opts.excludes`, or starts with
///   `.` (other than `.git` itself), are skipped.
/// - `read_dir`/metadata errors on individual entries are logged and skipped;
///   only errors on the root itself are returned as [`ScanError`].
pub fn scan_for_repos(root: &Path, opts: &ScanOptions) -> Result<Vec<PathBuf>, ScanError> {
    if !root.is_absolute() {
        return Err(ScanError::RootNotAbsolute(root.to_path_buf()));
    }
    let meta = match std::fs::metadata(root) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(ScanError::RootNotFound(root.to_path_buf()));
        }
        Err(e) => {
            return Err(ScanError::Io {
                path: root.to_path_buf(),
                source: e,
            });
        }
    };
    if !meta.is_dir() {
        return Err(ScanError::RootNotADirectory(root.to_path_buf()));
    }

    if root.join(".git").exists() {
        return Ok(vec![root.to_path_buf()]);
    }

    let exclude_set: HashSet<String> = opts.excludes.iter().cloned().collect();
    let mut results: BTreeSet<PathBuf> = BTreeSet::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    if opts.follow_symlinks {
        if let Ok(c) = root.canonicalize() {
            visited.insert(c);
        }
    }

    let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), 0));

    while let Some((dir, depth)) = queue.pop_front() {
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) => {
                if depth == 0 {
                    return Err(ScanError::Io {
                        path: dir.to_path_buf(),
                        source: e,
                    });
                }
                warn!(path = %dir.display(), error = %e, "skipping unreadable directory");
                continue;
            }
        };

        for entry in read {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!(path = %dir.display(), error = %e, "skipping unreadable entry");
                    continue;
                }
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "skipping entry without file_type");
                    continue;
                }
            };

            let name = match entry.file_name().to_str().map(String::from) {
                Some(s) => s,
                None => continue,
            };

            // `.git` marker (directory OR worktree pointer file): the parent
            // is a repo. We do not descend into `.git` itself.
            if name == ".git" {
                results.insert(dir.clone());
                continue;
            }

            if file_type.is_symlink() && !opts.follow_symlinks {
                continue;
            }

            // We only descend into directories. (Symlinks-to-dir are handled
            // below via the canonical-path check when follow_symlinks is on.)
            let is_dir = if file_type.is_dir() {
                true
            } else if opts.follow_symlinks && file_type.is_symlink() {
                std::fs::metadata(&path)
                    .map(|m| m.is_dir())
                    .unwrap_or(false)
            } else {
                false
            };
            if !is_dir {
                continue;
            }

            // Hidden directories are skipped.
            if name.starts_with('.') {
                continue;
            }
            if matches_exclude(&exclude_set, &name) {
                continue;
            }

            // Loop protection for follow_symlinks.
            if opts.follow_symlinks {
                if let Ok(canon) = path.canonicalize() {
                    if !visited.insert(canon) {
                        continue;
                    }
                }
            }

            let next_depth = depth + 1;
            if next_depth <= opts.max_depth {
                queue.push_back((path, next_depth));
            }
        }

        // If we discovered that `dir` itself is a repo via the `.git` entry,
        // remove any of its children we may have already enqueued so we don't
        // recurse into the repo. (Children enqueued in the same loop above.)
        if results.contains(&dir) {
            queue.retain(|(p, _)| !p.starts_with(&dir) || p == &dir);
        }
    }

    Ok(results.into_iter().collect())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn init_repo(path: &Path) {
        fs::create_dir_all(path).unwrap();
        git2::Repository::init(path).unwrap();
    }

    fn abs(p: &Path) -> PathBuf {
        p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
    }

    #[test]
    fn scan_empty_dir_returns_empty() {
        let dir = tempdir().unwrap();
        let out = scan_for_repos(&abs(dir.path()), &ScanOptions::default()).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn scan_single_repo_in_subdir() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("sub").join("myrepo");
        init_repo(&repo);
        let out = scan_for_repos(&abs(dir.path()), &ScanOptions::default()).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("sub/myrepo"));
    }

    #[test]
    fn scan_root_is_repo() {
        let dir = tempdir().unwrap();
        init_repo(dir.path());
        // Also create a nested repo to confirm we do not descend.
        init_repo(&dir.path().join("nested"));
        let out = scan_for_repos(&abs(dir.path()), &ScanOptions::default()).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], abs(dir.path()));
    }

    #[test]
    fn scan_nested_repos_skips_inside_repo() {
        let dir = tempdir().unwrap();
        let parent = dir.path().join("parent");
        init_repo(&parent);
        init_repo(&parent.join("child"));
        let out = scan_for_repos(&abs(dir.path()), &ScanOptions::default()).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("parent"));
    }

    #[test]
    fn scan_respects_max_depth() {
        let dir = tempdir().unwrap();
        let deep = dir.path().join("a").join("b").join("c").join("repo");
        init_repo(&deep);

        let mut opts = ScanOptions {
            max_depth: 2,
            ..Default::default()
        };
        let out = scan_for_repos(&abs(dir.path()), &opts).unwrap();
        assert!(out.is_empty(), "depth 2 must not reach a/b/c/repo");

        opts.max_depth = 5;
        let out = scan_for_repos(&abs(dir.path()), &opts).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn scan_excludes_default_dirs() {
        let dir = tempdir().unwrap();
        init_repo(&dir.path().join("node_modules").join("foo"));
        init_repo(&dir.path().join("keep"));
        let out = scan_for_repos(&abs(dir.path()), &ScanOptions::default()).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("keep"));
    }

    #[test]
    fn scan_detects_worktree_dotgit_file() {
        let dir = tempdir().unwrap();
        let wt = dir.path().join("worktree");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), "gitdir: /some/path/to/.git/worktrees/x\n").unwrap();
        let out = scan_for_repos(&abs(dir.path()), &ScanOptions::default()).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("worktree"));
    }

    #[test]
    #[cfg(unix)]
    fn scan_symlink_loop_safety() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a");
        fs::create_dir_all(&a).unwrap();
        // Symlink that points back to its own parent: would loop if followed.
        std::os::unix::fs::symlink(dir.path(), a.join("loop")).unwrap();
        // Default: do not follow → must not hang or error.
        let out = scan_for_repos(&abs(dir.path()), &ScanOptions::default()).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn scan_nonexistent_root_errors() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let err = scan_for_repos(&missing, &ScanOptions::default()).unwrap_err();
        assert!(matches!(err, ScanError::RootNotFound(_)));
    }

    #[test]
    fn scan_relative_root_errors() {
        let err = scan_for_repos(Path::new("relative/path"), &ScanOptions::default()).unwrap_err();
        assert!(matches!(err, ScanError::RootNotAbsolute(_)));
    }

    #[test]
    fn scan_root_is_file_errors() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("file.txt");
        fs::write(&f, "x").unwrap();
        let err = scan_for_repos(&abs(&f), &ScanOptions::default()).unwrap_err();
        assert!(matches!(err, ScanError::RootNotADirectory(_)));
    }

    #[test]
    #[cfg(unix)]
    fn scan_unreadable_dir_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let bad = dir.path().join("locked");
        fs::create_dir_all(&bad).unwrap();
        init_repo(&dir.path().join("good"));
        let mut perms = fs::metadata(&bad).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&bad, perms).unwrap();

        let result = scan_for_repos(&abs(dir.path()), &ScanOptions::default());

        // Restore permissions so tempdir cleanup works.
        let mut restore = fs::metadata(&bad).unwrap().permissions();
        restore.set_mode(0o755);
        let _ = fs::set_permissions(&bad, restore);

        let out = result.unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("good"));
    }

    #[test]
    fn scan_depth_boundary_three_levels() {
        let dir = tempdir().unwrap();
        // repo lives at root/a/b/c — exactly 3 levels below root
        let repo = dir.path().join("a").join("b").join("c");
        init_repo(&repo);

        let opts3 = ScanOptions {
            max_depth: 3,
            ..Default::default()
        };
        let out = scan_for_repos(&abs(dir.path()), &opts3).unwrap();
        assert_eq!(out.len(), 1, "max_depth=3 must reach a/b/c");

        let opts2 = ScanOptions {
            max_depth: 2,
            ..Default::default()
        };
        let out2 = scan_for_repos(&abs(dir.path()), &opts2).unwrap();
        assert!(out2.is_empty(), "max_depth=2 must NOT reach a/b/c");
    }

    #[test]
    #[cfg(unix)]
    fn scan_unreadable_root_hard_errors() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();

        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(dir.path(), perms).unwrap();

        // If running as root, chmod has no effect — skip.
        if std::fs::read_dir(dir.path()).is_ok() {
            // Restore and skip.
            let mut restore = std::fs::metadata(dir.path()).unwrap().permissions();
            restore.set_mode(0o755);
            let _ = std::fs::set_permissions(dir.path(), restore);
            return;
        }

        let result = scan_for_repos(&abs(dir.path()), &ScanOptions::default());

        // Restore permissions before tempdir drops.
        let mut restore = std::fs::metadata(dir.path())
            .unwrap_or_else(|_| {
                // If metadata also fails, we can't restore; this is ok in CI as a best effort.
                // In practice metadata on the dir itself should work even with mode 000.
                panic!("could not get metadata to restore permissions")
            })
            .permissions();
        restore.set_mode(0o755);
        let _ = std::fs::set_permissions(dir.path(), restore);

        assert!(
            matches!(result, Err(ScanError::Io { .. })),
            "expected ScanError::Io for unreadable root, got: {:?}",
            result
        );
    }

    #[test]
    fn matches_exclude_exact_on_non_windows() {
        let excludes: HashSet<String> = ["node_modules", "target"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Exact match works.
        assert!(matches_exclude(&excludes, "node_modules"));
        assert!(matches_exclude(&excludes, "target"));

        // Non-exact does NOT match on Linux/macOS.
        #[cfg(not(target_os = "windows"))]
        {
            assert!(!matches_exclude(&excludes, "NODE_MODULES"));
            assert!(!matches_exclude(&excludes, "Target"));
        }
    }
}
