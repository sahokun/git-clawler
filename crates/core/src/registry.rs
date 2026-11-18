//! In-memory repository registry for git-clawler.
//!
//! [`Registry`] holds the runtime collection of monitored repositories and
//! provides CRUD operations. It is built from [`crate::config::AppConfig`] at
//! startup and can be converted back for persistence.

use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::{debug, warn};

use crate::config::{is_repo_path_absolute, AppConfig, RepoEntry};
use crate::model::{derive_name_from_path, Repository};
use crate::path_key::{DistroPathKey, PathMatchKey};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur when manipulating the registry.
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("repository already exists at path: {0}")]
    AlreadyExists(PathBuf),
    #[error("no repository found at path: {0}")]
    NotFoundByPath(PathBuf),
    #[error("no repository found with name: {0}")]
    NotFoundByName(String),
    #[error("repository path must be absolute: {0}")]
    RelativePath(PathBuf),
    #[error("no repository found with (distro={distro:?}, path={path})")]
    NotFoundByRepository {
        distro: Option<String>,
        path: PathBuf,
    },
    #[error("multiple repositories match path {path}: {matches:?}")]
    AmbiguousPath { path: PathBuf, matches: Vec<String> },
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// In-memory collection of monitored repositories.
///
/// The registry uses the repository **path** as the primary key — names are
/// display-only and not required to be unique.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    repositories: Vec<Repository>,
}

impl Registry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry from an [`AppConfig`].
    ///
    /// Each [`RepoEntry`] is converted to a [`Repository`], deriving the name
    /// from the path if not explicitly set. Duplicate paths are skipped with a
    /// warning (the first entry wins).
    pub fn from_config(config: &AppConfig) -> Self {
        let mut reg = Self::new();
        for entry in &config.repositories {
            let name = entry
                .name
                .clone()
                .unwrap_or_else(|| derive_name_from_path(&entry.path));
            let repo = Repository::new(name, &entry.path)
                .with_fetch(entry.fetch)
                .with_distro(entry.distro.clone())
                .with_worktree_of(entry.worktree_of.clone());
            if let Err(e) = reg.add(repo) {
                match &e {
                    RegistryError::AlreadyExists(_) => {
                        warn!(%e, "skipping duplicate repository in config");
                    }
                    _ => {
                        warn!(%e, "skipping invalid repository in config");
                    }
                }
            }
        }
        debug!(count = reg.len(), "registry built from config");
        reg
    }

    /// Return a slice of all repositories.
    pub fn list(&self) -> &[Repository] {
        &self.repositories
    }

    /// Return the number of repositories.
    pub fn len(&self) -> usize {
        self.repositories.len()
    }

    /// Return `true` if the registry contains no repositories.
    pub fn is_empty(&self) -> bool {
        self.repositories.is_empty()
    }

    /// Find a repository by its path.
    ///
    /// **Note**: If multiple repositories share the same path but different
    /// `distro` values, this returns the *first* match. Prefer
    /// [`Self::find_by_distro_path`] when the distro is known.
    pub fn find_by_path(&self, path: &Path) -> Option<&Repository> {
        self.repositories.iter().find(|r| r.path == path)
    }

    /// Find a repository by its path (mutable).
    ///
    /// **Note**: If multiple repositories share the same path but different
    /// `distro` values, this returns the *first* match. Prefer
    /// [`Self::find_by_distro_path_mut`] when the distro is known.
    pub fn find_by_path_mut(&mut self, path: &Path) -> Option<&mut Repository> {
        self.repositories.iter_mut().find(|r| r.path == path)
    }

    /// Find repositories by name.
    ///
    /// Returns all matches because names are **not** unique.
    pub fn find_by_name(&self, name: &str) -> Vec<&Repository> {
        self.repositories
            .iter()
            .filter(|r| r.name == name)
            .collect()
    }

    /// Find a repository by `(distro, path)` identity.
    pub fn find_by_distro_path(&self, distro: Option<&str>, path: &Path) -> Option<&Repository> {
        self.repositories
            .iter()
            .find(|r| repository_key(r) == DistroPathKey::current_host(distro, path))
    }

    /// Find a repository by `(distro, path)` identity (mutable).
    pub fn find_by_distro_path_mut(
        &mut self,
        distro: Option<&str>,
        path: &Path,
    ) -> Option<&mut Repository> {
        self.repositories
            .iter_mut()
            .find(|r| repository_key(r) == DistroPathKey::current_host(distro, path))
    }

    /// Add a repository to the registry.
    ///
    /// Returns [`RegistryError::AlreadyExists`] if a repository with the same
    /// `(distro, path)` pair is already registered, or
    /// [`RegistryError::RelativePath`] if the path is not absolute.
    pub fn add(&mut self, repo: Repository) -> Result<(), RegistryError> {
        if !is_repo_path_absolute(&repo.path) {
            return Err(RegistryError::RelativePath(repo.path));
        }
        if self
            .find_by_distro_path(repo.distro.as_deref(), &repo.path)
            .is_some()
        {
            return Err(RegistryError::AlreadyExists(repo.path));
        }
        debug!(name = %repo.name, path = %repo.path.display(), distro = ?repo.distro, "repository added");
        self.repositories.push(repo);
        Ok(())
    }

    /// Add a repository from a path, deriving the name automatically.
    ///
    /// This is a convenience wrapper around [`Self::add`].
    pub fn add_from_path(&mut self, path: impl Into<PathBuf>) -> Result<(), RegistryError> {
        let path = path.into();
        let name = derive_name_from_path(&path);
        self.add(Repository::new(name, path))
    }

    /// Remove a repository by its path.
    ///
    /// **Note**: If multiple repositories share the same path but different
    /// `distro` values, this removes the *first* match. Prefer
    /// [`Self::remove_by_distro_path`] when the distro is known.
    ///
    /// Returns the removed [`Repository`], or [`RegistryError::NotFoundByPath`]
    /// if no repository matches.
    pub fn remove_by_path(&mut self, path: &Path) -> Result<Repository, RegistryError> {
        let idx = self
            .repositories
            .iter()
            .position(|r| r.path == path)
            .ok_or_else(|| RegistryError::NotFoundByPath(path.to_path_buf()))?;
        let removed = self.repositories.remove(idx);
        debug!(name = %removed.name, path = %removed.path.display(), "repository removed");
        Ok(removed)
    }

    /// Remove exactly one repository matching `path` after canonical/lexical
    /// absolute-path normalisation.
    ///
    /// This is intended for path-only user interfaces. If multiple registry
    /// entries match the same path (for example the same Linux path in multiple
    /// WSL distros), no entry is removed and [`RegistryError::AmbiguousPath`] is
    /// returned.
    pub fn remove_by_path_unambiguous(&mut self, path: &Path) -> Result<Repository, RegistryError> {
        let input_key = PathMatchKey::new(path);
        let matches: Vec<usize> = self
            .repositories
            .iter()
            .enumerate()
            .filter_map(|(idx, repo)| (PathMatchKey::new(&repo.path) == input_key).then_some(idx))
            .collect();

        match matches.as_slice() {
            [] => Err(RegistryError::NotFoundByPath(input_key.into_path_buf())),
            [idx] => {
                let removed = self.repositories.remove(*idx);
                debug!(name = %removed.name, path = %removed.path.display(), distro = ?removed.distro, "repository removed by unambiguous path");
                Ok(removed)
            }
            _ => {
                let matches = matches
                    .into_iter()
                    .map(|idx| {
                        let repo = &self.repositories[idx];
                        format!(
                            "name={}, distro={}, path={}",
                            repo.name,
                            repo.distro.as_deref().unwrap_or("<native>"),
                            repo.path.display()
                        )
                    })
                    .collect();
                Err(RegistryError::AmbiguousPath {
                    path: input_key.into_path_buf(),
                    matches,
                })
            }
        }
    }

    /// Remove a repository by `(distro, path)` identity.
    ///
    /// Returns the removed [`Repository`], or
    /// [`RegistryError::NotFoundByRepository`] if no match found.
    pub fn remove_by_distro_path(
        &mut self,
        distro: Option<&str>,
        path: &Path,
    ) -> Result<Repository, RegistryError> {
        let idx = self
            .repositories
            .iter()
            .position(|r| repository_key(r) == DistroPathKey::current_host(distro, path))
            .ok_or_else(|| RegistryError::NotFoundByRepository {
                distro: distro.map(str::to_owned),
                path: path.to_path_buf(),
            })?;
        let removed = self.repositories.remove(idx);
        debug!(
            name = %removed.name,
            path = %removed.path.display(),
            distro = ?removed.distro,
            "repository removed by distro+path"
        );
        Ok(removed)
    }

    /// Convert the registry back to a list of [`RepoEntry`] for config
    /// persistence.
    pub fn to_config_entries(&self) -> Vec<RepoEntry> {
        self.repositories
            .iter()
            .map(|r| RepoEntry {
                name: Some(r.name.clone()),
                path: r.path.clone(),
                fetch: r.fetch,
                distro: r.distro.clone(),
                worktree_of: r.worktree_of.clone(),
            })
            .collect()
    }
}

fn repository_key(repo: &Repository) -> DistroPathKey {
    DistroPathKey::current_host(repo.distro.as_deref(), &repo.path)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    fn sample_repo(name: &str, path: &str) -> Repository {
        Repository::new(name, path)
    }

    #[test]
    fn new_registry_is_empty() {
        let reg = Registry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.list().is_empty());
    }

    #[test]
    fn add_and_list() {
        let mut reg = Registry::new();
        reg.add(sample_repo("a", "/tmp/a")).unwrap();
        reg.add(sample_repo("b", "/tmp/b")).unwrap();
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.list()[0].name, "a");
        assert_eq!(reg.list()[1].name, "b");
    }

    #[test]
    fn add_duplicate_path_returns_error() {
        let mut reg = Registry::new();
        reg.add(sample_repo("a", "/tmp/dup")).unwrap();
        let result = reg.add(sample_repo("b", "/tmp/dup"));
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RegistryError::AlreadyExists(_)
        ));
    }

    #[test]
    fn add_relative_path_returns_error() {
        let mut reg = Registry::new();
        let result = reg.add(sample_repo("a", "relative/path"));
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RegistryError::RelativePath(_)
        ));
    }

    #[test]
    fn add_from_path_derives_name() {
        let mut reg = Registry::new();
        reg.add_from_path("/home/user/my-project").unwrap();
        assert_eq!(reg.list()[0].name, "my-project");
    }

    #[test]
    fn find_by_path() {
        let mut reg = Registry::new();
        reg.add(sample_repo("a", "/tmp/a")).unwrap();
        assert!(reg.find_by_path(Path::new("/tmp/a")).is_some());
        assert!(reg.find_by_path(Path::new("/tmp/x")).is_none());
    }

    #[test]
    fn find_by_path_mut() {
        let mut reg = Registry::new();
        reg.add(sample_repo("a", "/tmp/a")).unwrap();
        let repo = reg.find_by_path_mut(Path::new("/tmp/a")).unwrap();
        repo.name = "renamed".to_string();
        assert_eq!(reg.list()[0].name, "renamed");
    }

    #[test]
    fn find_by_name_returns_multiple() {
        let mut reg = Registry::new();
        reg.add(sample_repo("same", "/tmp/x")).unwrap();
        reg.add(sample_repo("same", "/tmp/y")).unwrap();
        reg.add(sample_repo("other", "/tmp/z")).unwrap();
        let found = reg.find_by_name("same");
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn remove_by_path() {
        let mut reg = Registry::new();
        reg.add(sample_repo("a", "/tmp/a")).unwrap();
        reg.add(sample_repo("b", "/tmp/b")).unwrap();
        let removed = reg.remove_by_path(Path::new("/tmp/a")).unwrap();
        assert_eq!(removed.name, "a");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn remove_nonexistent_returns_error() {
        let mut reg = Registry::new();
        let result = reg.remove_by_path(Path::new("/tmp/nope"));
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RegistryError::NotFoundByPath(_)
        ));
    }

    #[test]
    fn remove_by_path_unambiguous_removes_single_match() {
        let mut reg = Registry::new();
        reg.add(sample_repo("a", "/tmp/a")).unwrap();
        reg.add(sample_repo("b", "/tmp/b")).unwrap();

        let removed = reg.remove_by_path_unambiguous(Path::new("/tmp/a")).unwrap();

        assert_eq!(removed.name, "a");
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.list()[0].name, "b");
    }

    #[test]
    fn remove_by_path_unambiguous_rejects_ambiguous_distros_without_mutating() {
        let mut reg = Registry::new();
        reg.add(Repository::new("native", "/home/user/proj"))
            .unwrap();
        reg.add(Repository::new("ubuntu", "/home/user/proj").with_distro(Some("Ubuntu".into())))
            .unwrap();

        let result = reg.remove_by_path_unambiguous(Path::new("/home/user/proj"));

        assert!(matches!(
            result.unwrap_err(),
            RegistryError::AmbiguousPath { .. }
        ));
        assert_eq!(reg.len(), 2, "ambiguous remove must not mutate registry");
    }

    #[test]
    fn remove_by_path_unambiguous_uses_lexical_relative_path_for_stale_entries() {
        let cwd = std::env::current_dir().unwrap();
        let stale = cwd.join("definitely-missing-gclawl-remove-test");
        let mut reg = Registry::new();
        reg.add(Repository::new("stale", &stale)).unwrap();

        let removed = reg
            .remove_by_path_unambiguous(Path::new("definitely-missing-gclawl-remove-test"))
            .unwrap();

        assert_eq!(removed.name, "stale");
        assert!(reg.is_empty());
    }

    #[test]
    fn from_config_derives_names() {
        let config = AppConfig {
            repositories: vec![
                RepoEntry {
                    worktree_of: None,
                    name: Some("explicit".into()),
                    path: PathBuf::from("/tmp/explicit"),
                    fetch: None,
                    distro: None,
                },
                RepoEntry {
                    worktree_of: None,
                    name: None,
                    path: PathBuf::from("/tmp/auto-name"),
                    fetch: None,
                    distro: None,
                },
            ],
            ..Default::default()
        };
        let reg = Registry::from_config(&config);
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.list()[0].name, "explicit");
        assert_eq!(reg.list()[1].name, "auto-name");
    }

    #[test]
    fn from_config_skips_duplicates() {
        let config = AppConfig {
            repositories: vec![
                RepoEntry {
                    worktree_of: None,
                    name: Some("first".into()),
                    path: PathBuf::from("/tmp/dup"),
                    fetch: None,
                    distro: None,
                },
                RepoEntry {
                    worktree_of: None,
                    name: Some("second".into()),
                    path: PathBuf::from("/tmp/dup"),
                    fetch: None,
                    distro: None,
                },
            ],
            ..Default::default()
        };
        let reg = Registry::from_config(&config);
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.list()[0].name, "first");
    }

    #[test]
    fn to_config_entries_roundtrip() {
        let mut reg = Registry::new();
        reg.add(sample_repo("a", "/tmp/a")).unwrap();
        reg.add(sample_repo("b", "/tmp/b")).unwrap();
        let entries = reg.to_config_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, Some("a".to_string()));
        assert_eq!(entries[0].path, PathBuf::from("/tmp/a"));
    }

    #[test]
    fn from_config_empty() {
        let config = AppConfig::default();
        let reg = Registry::from_config(&config);
        assert!(reg.is_empty());
    }

    #[test]
    fn from_config_preserves_fetch_override() {
        let config = AppConfig {
            repositories: vec![
                RepoEntry {
                    worktree_of: None,
                    name: Some("with-fetch-true".into()),
                    path: PathBuf::from("/tmp/a"),
                    fetch: Some(true),
                    distro: None,
                },
                RepoEntry {
                    worktree_of: None,
                    name: Some("with-fetch-false".into()),
                    path: PathBuf::from("/tmp/b"),
                    fetch: Some(false),
                    distro: None,
                },
                RepoEntry {
                    worktree_of: None,
                    name: Some("inherit".into()),
                    path: PathBuf::from("/tmp/c"),
                    fetch: None,
                    distro: None,
                },
            ],
            ..Default::default()
        };
        let reg = Registry::from_config(&config);
        assert_eq!(reg.list()[0].fetch, Some(true));
        assert_eq!(reg.list()[1].fetch, Some(false));
        assert_eq!(reg.list()[2].fetch, None);
    }

    #[test]
    fn to_config_entries_preserves_fetch_override() {
        // Regression test: persisting via
        // `Registry::to_config_entries` must not silently drop a per-repository
        // `fetch` override that was loaded from config.
        let config = AppConfig {
            repositories: vec![
                RepoEntry {
                    worktree_of: None,
                    name: Some("keep-true".into()),
                    path: PathBuf::from("/tmp/keep-true"),
                    fetch: Some(true),
                    distro: None,
                },
                RepoEntry {
                    worktree_of: None,
                    name: Some("keep-false".into()),
                    path: PathBuf::from("/tmp/keep-false"),
                    fetch: Some(false),
                    distro: None,
                },
                RepoEntry {
                    worktree_of: None,
                    name: Some("keep-inherit".into()),
                    path: PathBuf::from("/tmp/keep-inherit"),
                    fetch: None,
                    distro: None,
                },
            ],
            ..Default::default()
        };
        let reg = Registry::from_config(&config);

        let entries = reg.to_config_entries();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0].fetch,
            Some(true),
            "Some(true) must survive roundtrip"
        );
        assert_eq!(
            entries[1].fetch,
            Some(false),
            "Some(false) must survive roundtrip"
        );
        assert_eq!(entries[2].fetch, None, "None must remain None");
    }

    #[test]
    fn add_propagates_distro() {
        let mut reg = Registry::new();
        let repo = Repository::new("proj", "/home/user/proj").with_distro(Some("Ubuntu".into()));
        reg.add(repo).unwrap();
        assert_eq!(reg.list()[0].distro, Some("Ubuntu".into()));
    }

    #[test]
    fn add_rejects_duplicate_distro_path_pair() {
        let mut reg = Registry::new();
        reg.add(Repository::new("a", "/home/user/proj").with_distro(Some("Ubuntu".into())))
            .unwrap();
        let result =
            reg.add(Repository::new("b", "/home/user/proj").with_distro(Some("Ubuntu".into())));
        assert!(matches!(
            result.unwrap_err(),
            RegistryError::AlreadyExists(_)
        ));
    }

    #[test]
    fn add_allows_same_path_with_different_distros() {
        let mut reg = Registry::new();
        reg.add(Repository::new("a", "/home/user/proj")).unwrap();
        reg.add(Repository::new("b", "/home/user/proj").with_distro(Some("Ubuntu".into())))
            .unwrap();
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn to_config_entries_preserves_distro() {
        let mut reg = Registry::new();
        reg.add(Repository::new("a", "/home/user/proj").with_distro(Some("Ubuntu".into())))
            .unwrap();
        let entries = reg.to_config_entries();
        assert_eq!(entries[0].distro, Some("Ubuntu".into()));
    }

    #[test]
    fn find_by_distro_path_distinguishes_distros() {
        let mut reg = Registry::new();
        reg.add(Repository::new("native", "/home/user/proj"))
            .unwrap();
        reg.add(Repository::new("ubuntu", "/home/user/proj").with_distro(Some("Ubuntu".into())))
            .unwrap();

        let native = reg.find_by_distro_path(None, Path::new("/home/user/proj"));
        assert!(native.is_some());
        assert_eq!(native.unwrap().name, "native");

        let ubuntu = reg.find_by_distro_path(Some("Ubuntu"), Path::new("/home/user/proj"));
        assert!(ubuntu.is_some());
        assert_eq!(ubuntu.unwrap().name, "ubuntu");
    }

    #[test]
    fn find_by_path_returns_first_match_when_multiple_distros() {
        let mut reg = Registry::new();
        reg.add(Repository::new("first", "/home/user/proj"))
            .unwrap();
        reg.add(Repository::new("second", "/home/user/proj").with_distro(Some("Ubuntu".into())))
            .unwrap();

        let found = reg.find_by_path(Path::new("/home/user/proj"));
        assert_eq!(found.unwrap().name, "first");
    }

    #[test]
    fn find_by_distro_path_mut_works() {
        let mut reg = Registry::new();
        reg.add(Repository::new("proj", "/home/user/proj").with_distro(Some("Ubuntu".into())))
            .unwrap();
        let repo = reg
            .find_by_distro_path_mut(Some("Ubuntu"), Path::new("/home/user/proj"))
            .unwrap();
        repo.name = "renamed".to_string();
        assert_eq!(reg.list()[0].name, "renamed");
    }

    #[test]
    fn remove_by_distro_path_removes_correct_entry() {
        let mut reg = Registry::new();
        reg.add(Repository::new("native", "/home/user/proj"))
            .unwrap();
        reg.add(Repository::new("ubuntu", "/home/user/proj").with_distro(Some("Ubuntu".into())))
            .unwrap();

        let removed = reg
            .remove_by_distro_path(Some("Ubuntu"), Path::new("/home/user/proj"))
            .unwrap();
        assert_eq!(removed.name, "ubuntu");
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.list()[0].name, "native");
    }

    #[test]
    fn remove_by_distro_path_not_found_returns_error() {
        let mut reg = Registry::new();
        let result = reg.remove_by_distro_path(None, Path::new("/nonexistent"));
        assert!(matches!(
            result.unwrap_err(),
            RegistryError::NotFoundByRepository { .. }
        ));
    }
}
