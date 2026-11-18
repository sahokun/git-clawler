//! git-clawler core library.
//!
//! Provides shared logic for the CLI and GUI frontends:
//! - [`model`] — Runtime data types (`Repository`, `RepoStatus`, etc.)
//! - [`config`] — TOML configuration management
//! - [`registry`] — In-memory repository management
//! - [`scanner`] — Recursive `.git` directory discovery
//! - [`status`] — Synchronous git repository status scanning

pub mod config;
pub mod discovery;
pub mod fetch_worker;
pub mod github;
pub mod list;
pub mod model;
pub mod notifications;
pub mod path_key;
pub mod registry;
pub mod scanner;
pub mod status;
pub mod watcher;

// Convenience re-exports
pub use config::{AppConfig, ConfigError, NotificationsConfig};
pub use github::{GhCounts, GhError};
pub use list::{
    build_targets, enrich_with_github, find_current_repo, scan_all, scan_all_with_github, ListRow,
    ListTarget, ScanOutcome, GITHUB_CONCURRENCY,
};
pub use model::{GithubInfo, RecentCommit, RepoStatus, Repository, UnpushedBranch};
pub use registry::{Registry, RegistryError};
pub use scanner::{default_excludes, scan_for_repos, ScanError, ScanOptions};
pub use status::{scan, StatusError};
pub use watcher::{WatchEvent, Watcher, WatcherConfig};

pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_version_is_set() {
        assert!(!CRATE_VERSION.is_empty());
    }
}
