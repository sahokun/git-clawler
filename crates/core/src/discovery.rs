//! Automatic registration of repositories found under scan roots.
//!
//! Discovery is split so the WSL helper can reuse the filesystem part:
//! - [`discover_root`] walks one root (on the machine that owns it) and
//!   returns repositories plus linked worktrees.
//! - [`worktree_gone`] decides whether a registered worktree was removed.
//! - [`plan`] compares those results with the configuration, and [`apply`]
//!   updates it idempotently.
//!
//! Only entries carrying `worktree_of` are ever removed automatically; a
//! missing ordinary repository stays registered so the user can decide.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{normalize_distro, AppConfig, RepoEntry, ScanRootEntry};
use crate::path_key::DistroPathKey;
use crate::scanner::{
    default_excludes, identity_key, path_exclude_keys, scan_for_repos, ScanError, ScanOptions,
};
use crate::status::{normalize_workdir, worktree_parent_of};

/// A repository or linked worktree found under a scan root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    pub path: PathBuf,
    /// Main repository working directory when `path` is a linked worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_of: Option<PathBuf>,
}

/// Changes discovery wants to make to the configuration.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Discovery {
    /// New entries to register.
    pub add: Vec<RepoEntry>,
    /// Existing entries (distro, path) that should record their worktree parent.
    pub annotate: Vec<(Option<String>, PathBuf, PathBuf)>,
    /// Existing worktree entries (distro, path) that Git no longer tracks.
    pub prune: Vec<(Option<String>, PathBuf)>,
}

impl Discovery {
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.annotate.is_empty() && self.prune.is_empty()
    }
}

/// Walk one scan root and return repositories plus (optionally) the linked
/// worktrees of each repository, wherever those worktrees live.
pub fn discover_root(root: &ScanRootEntry) -> Result<Vec<Found>, ScanError> {
    let mut excludes = default_excludes();
    excludes.extend(root.exclude.iter().cloned());
    let opts = ScanOptions {
        max_depth: root.depth,
        excludes,
        follow_symlinks: false,
    };
    let excluded = path_exclude_keys(&root.path, &root.exclude);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut push = |found: Found, out: &mut Vec<Found>| {
        if seen.insert(identity_key(&found.path)) {
            out.push(found);
        }
    };
    for path in scan_for_repos(&root.path, &opts)? {
        let repo = match git2::Repository::open(&path) {
            Ok(repo) if !repo.is_bare() => repo,
            // Not a usable repository (e.g. broken .git file): leave it to the user.
            _ => continue,
        };
        if repo.is_worktree() {
            if root.worktrees {
                push(
                    Found {
                        worktree_of: worktree_parent_of(&repo),
                        path,
                    },
                    &mut out,
                );
            }
            continue;
        }
        push(
            Found {
                path: path.clone(),
                worktree_of: None,
            },
            &mut out,
        );
        if !root.worktrees {
            continue;
        }
        for worktree in linked_worktrees(&repo) {
            if !excluded.contains(&identity_key(&worktree)) {
                push(
                    Found {
                        path: worktree,
                        worktree_of: Some(path.clone()),
                    },
                    &mut out,
                );
            }
        }
    }
    Ok(out)
}

/// Valid linked worktrees of a main repository (missing or prunable ones are skipped).
fn linked_worktrees(repo: &git2::Repository) -> Vec<PathBuf> {
    let Ok(names) = repo.worktrees() else {
        return Vec::new();
    };
    names
        .iter()
        .flatten()
        .flatten()
        .filter_map(|name| repo.find_worktree(name).ok())
        .filter(|wt| wt.validate().is_ok())
        .map(|wt| normalize_workdir(wt.path()))
        .collect()
}

/// True when a registered worktree no longer exists and its main repository
/// no longer lists a valid worktree at that path. Unknown situations (for
/// example an unreadable or missing main repository) return `false` so the
/// entry is kept.
pub fn worktree_gone(path: &Path, parent: &Path) -> bool {
    if !matches!(path.try_exists(), Ok(false)) {
        return false;
    }
    let Ok(main) = git2::Repository::open(parent) else {
        return false;
    };
    let key = identity_key(path);
    !linked_worktrees(&main)
        .iter()
        .any(|wt| identity_key(wt) == key)
}

/// Compare discovery results with the configuration.
///
/// `found` holds results per scan root (`distro`, items). `gone` reports
/// whether a registered worktree entry should be pruned.
pub fn plan(
    config: &AppConfig,
    found: &[(Option<String>, Vec<Found>)],
    gone: impl Fn(&RepoEntry) -> bool,
) -> Discovery {
    let key = |distro: Option<&str>, path: &Path| DistroPathKey::current_host(distro, path);
    let mut known: HashSet<DistroPathKey> = config
        .repositories
        .iter()
        .map(|r| key(normalize_distro(r.distro.clone()).as_deref(), &r.path))
        .collect();
    let mut result = Discovery::default();
    for (distro, items) in found {
        let distro = normalize_distro(distro.clone());
        for item in items {
            let k = key(distro.as_deref(), &item.path);
            if known.insert(k.clone()) {
                result.add.push(RepoEntry {
                    name: None,
                    path: item.path.clone(),
                    fetch: None,
                    distro: distro.clone(),
                    worktree_of: item.worktree_of.clone(),
                });
                continue;
            }
            let Some(parent) = &item.worktree_of else {
                continue;
            };
            let existing = config
                .repositories
                .iter()
                .find(|r| key(normalize_distro(r.distro.clone()).as_deref(), &r.path) == k);
            if matches!(existing, Some(entry) if entry.worktree_of.is_none()) {
                result
                    .annotate
                    .push((distro.clone(), item.path.clone(), parent.clone()));
            }
        }
    }
    for entry in &config.repositories {
        if entry.worktree_of.is_some() && gone(entry) {
            result
                .prune
                .push((normalize_distro(entry.distro.clone()), entry.path.clone()));
        }
    }
    result
}

/// Native (non-WSL) discovery for every root with `auto = true`.
pub fn discover_native(config: &AppConfig) -> Discovery {
    let mut found = Vec::new();
    for root in config.scan_roots.iter().filter(|r| r.auto) {
        if normalize_distro(root.distro.clone()).is_some() {
            continue;
        }
        match discover_root(root) {
            Ok(items) => found.push((None, items)),
            Err(error) => {
                tracing::warn!(root = %root.path.display(), %error, "scan root discovery failed")
            }
        }
    }
    plan(config, &found, |entry| {
        normalize_distro(entry.distro.clone()).is_none()
            && entry
                .worktree_of
                .as_deref()
                .is_some_and(|parent| worktree_gone(&entry.path, parent))
    })
}

/// Apply discovery changes. Returns whether the configuration changed.
pub fn apply(config: &mut AppConfig, discovery: &Discovery) -> bool {
    let key = |distro: Option<&str>, path: &Path| DistroPathKey::current_host(distro, path);
    let mut changed = false;
    let prune: HashSet<DistroPathKey> = discovery
        .prune
        .iter()
        .map(|(d, p)| key(d.as_deref(), p))
        .collect();
    let before = config.repositories.len();
    config.repositories.retain(|r| {
        !(r.worktree_of.is_some()
            && prune.contains(&key(normalize_distro(r.distro.clone()).as_deref(), &r.path)))
    });
    changed |= config.repositories.len() != before;
    for (distro, path, parent) in &discovery.annotate {
        let k = key(distro.as_deref(), path);
        if let Some(entry) = config.repositories.iter_mut().find(|r| {
            r.worktree_of.is_none()
                && key(normalize_distro(r.distro.clone()).as_deref(), &r.path) == k
        }) {
            entry.worktree_of = Some(parent.clone());
            changed = true;
        }
    }
    let mut known: HashSet<DistroPathKey> = config
        .repositories
        .iter()
        .map(|r| key(normalize_distro(r.distro.clone()).as_deref(), &r.path))
        .collect();
    for entry in &discovery.add {
        if known.insert(key(
            normalize_distro(entry.distro.clone()).as_deref(),
            &entry.path,
        )) {
            config.repositories.push(entry.clone());
            changed = true;
        }
    }
    changed
}

/// After a repository entry is removed by the user, add an exclusion to the
/// auto scan root that would rediscover it. Returns `(root path, pattern)`.
///
/// Paths under the root are excluded by relative path; a worktree outside
/// every root is excluded by absolute path on the root containing its parent.
pub fn record_exclusion(config: &mut AppConfig, removed: &RepoEntry) -> Option<(PathBuf, String)> {
    let distro = normalize_distro(removed.distro.clone());
    let case_insensitive = cfg!(target_os = "windows") && distro.is_none();
    let roots: Vec<usize> = config
        .scan_roots
        .iter()
        .enumerate()
        .filter(|(_, r)| r.auto && normalize_distro(r.distro.clone()) == distro)
        .map(|(i, _)| i)
        .collect();
    let mut target = roots.iter().find_map(|&i| {
        relative_to(&config.scan_roots[i].path, &removed.path, case_insensitive).map(|rel| (i, rel))
    });
    if target.is_none() {
        if let Some(parent) = &removed.worktree_of {
            target = roots.iter().find_map(|&i| {
                relative_to(&config.scan_roots[i].path, parent, case_insensitive)
                    .map(|_| (i, removed.path.to_string_lossy().replace('\\', "/")))
            });
        }
    }
    let (index, pattern) = target?;
    let root = &mut config.scan_roots[index];
    if !root.exclude.iter().any(|e| e == &pattern) {
        root.exclude.push(pattern.clone());
    }
    Some((root.path.clone(), pattern))
}

/// `path` relative to `root` using `/` separators, when `path` is strictly inside `root`.
fn relative_to(root: &Path, path: &Path, case_insensitive: bool) -> Option<String> {
    let norm = |p: &Path| {
        let text = p.to_string_lossy().replace('\\', "/");
        let text = text.trim_end_matches('/').to_string();
        if case_insensitive {
            text.to_ascii_lowercase()
        } else {
            text
        }
    };
    let (root_n, path_n) = (norm(root), norm(path));
    let rest = path_n.strip_prefix(&root_n)?.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    // Preserve the original spelling of the relative part.
    let original = path.to_string_lossy().replace('\\', "/");
    let original = original.trim_end_matches('/');
    Some(original[original.len() - rest.len()..].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, worktree_of: Option<&str>) -> RepoEntry {
        RepoEntry {
            name: None,
            path: PathBuf::from(path),
            fetch: None,
            distro: None,
            worktree_of: worktree_of.map(PathBuf::from),
        }
    }

    fn root(path: &str) -> ScanRootEntry {
        ScanRootEntry {
            path: PathBuf::from(path),
            depth: 1,
            exclude: Vec::new(),
            distro: None,
            auto: true,
            worktrees: true,
        }
    }

    #[test]
    fn plan_adds_new_and_annotates_existing_worktrees() {
        let config = AppConfig {
            repositories: vec![entry("/r/a", None), entry("/r/a-wt", None)],
            ..AppConfig::default()
        };
        let found = vec![(
            None,
            vec![
                Found {
                    path: "/r/a".into(),
                    worktree_of: None,
                },
                Found {
                    path: "/r/a-wt".into(),
                    worktree_of: Some("/r/a".into()),
                },
                Found {
                    path: "/r/b".into(),
                    worktree_of: None,
                },
                Found {
                    path: "/tmp/b-wt".into(),
                    worktree_of: Some("/r/b".into()),
                },
            ],
        )];
        let d = plan(&config, &found, |_| false);
        assert_eq!(
            d.add,
            vec![entry("/r/b", None), entry("/tmp/b-wt", Some("/r/b"))]
        );
        assert_eq!(d.annotate, vec![(None, "/r/a-wt".into(), "/r/a".into())]);
        assert!(d.prune.is_empty());
    }

    #[test]
    fn plan_prunes_only_worktree_entries() {
        let config = AppConfig {
            repositories: vec![
                entry("/r/gone-repo", None),
                entry("/r/gone-wt", Some("/r/a")),
            ],
            ..AppConfig::default()
        };
        let d = plan(&config, &[], |_| true);
        assert_eq!(d.prune, vec![(None, PathBuf::from("/r/gone-wt"))]);
    }

    #[test]
    fn apply_is_idempotent() {
        let mut config = AppConfig {
            repositories: vec![entry("/r/a-wt", None), entry("/r/old-wt", Some("/r/a"))],
            ..AppConfig::default()
        };
        let d = Discovery {
            add: vec![entry("/r/b", None)],
            annotate: vec![(None, "/r/a-wt".into(), "/r/a".into())],
            prune: vec![(None, "/r/old-wt".into())],
        };
        assert!(apply(&mut config, &d));
        assert_eq!(
            config.repositories,
            vec![entry("/r/a-wt", Some("/r/a")), entry("/r/b", None)]
        );
        assert!(!apply(&mut config, &d));
    }

    #[test]
    fn record_exclusion_uses_relative_then_absolute_paths() {
        let mut config = AppConfig {
            scan_roots: vec![root("/r")],
            ..AppConfig::default()
        };
        assert_eq!(
            record_exclusion(&mut config, &entry("/r/a/b", None)),
            Some(("/r".into(), "a/b".into()))
        );
        assert_eq!(
            record_exclusion(&mut config, &entry("/tmp/wt", Some("/r/a"))),
            Some(("/r".into(), "/tmp/wt".into()))
        );
        assert_eq!(
            record_exclusion(&mut config, &entry("/elsewhere/x", None)),
            None
        );
        // Recording twice does not duplicate the pattern.
        record_exclusion(&mut config, &entry("/r/a/b", None));
        assert_eq!(config.scan_roots[0].exclude, vec!["a/b", "/tmp/wt"]);
    }

    #[test]
    fn record_exclusion_ignores_manual_roots() {
        let mut manual = root("/r");
        manual.auto = false;
        let mut config = AppConfig {
            scan_roots: vec![manual],
            ..AppConfig::default()
        };
        assert_eq!(record_exclusion(&mut config, &entry("/r/a", None)), None);
    }
}
