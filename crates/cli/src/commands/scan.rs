//! `gclawl scan <root>` — recursively discover git repositories.
//!
//! Lists every `.git`-bearing directory under `<root>` (bounded by `--depth`).
//! With `--add`, any newly discovered repository is appended to the user's
//! config file. Already-registered repositories are surfaced with an
//! `[already registered]` marker but never duplicated.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context;

use git_clawler_core::{
    config, default_excludes, scan_for_repos, AppConfig, Registry, ScanOptions,
};

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Run `gclawl scan` against the user's default config file.
pub async fn run(root: PathBuf, add: bool, depth: Option<u32>) -> anyhow::Result<()> {
    let config_path = config::default_config_path()?;
    run_with_config_path(root, add, depth, config_path).await
}

/// Run `gclawl scan` writing/reading the registry from `config_path`.
///
/// Exposed for unit tests so the user's real config is never touched.
pub(crate) async fn run_with_config_path(
    root: PathBuf,
    add: bool,
    depth: Option<u32>,
    config_path: PathBuf,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let abs_root = if root.is_absolute() {
        root
    } else {
        cwd.join(&root)
    };
    let canonical = std::fs::canonicalize(&abs_root)
        .with_context(|| format!("path not found: {}", abs_root.display()))?;
    let depth = depth.unwrap_or(3);

    println!("Scanning {} (depth={})...", canonical.display(), depth);

    let opts = ScanOptions {
        max_depth: depth,
        excludes: default_excludes(),
        follow_symlinks: false,
    };
    let scan_root = canonical.clone();
    let found = tokio::task::spawn_blocking(move || scan_for_repos(&scan_root, &opts))
        .await
        .context("scanner task panicked")??;

    let mut config = load_or_default_from(&config_path)?;
    let mut registry = Registry::from_config(&config);

    let width = compute_name_width(&found);
    let mut new_count: usize = 0;

    // Build a set of canonicalized paths from the existing config so that
    // symlink-containing or `..`-containing config paths are deduped correctly.
    let canonical_registered: HashSet<PathBuf> = registry
        .list()
        .iter()
        .filter_map(|repo| {
            repo.path()
                .canonicalize()
                .map_err(|e| {
                    tracing::debug!(
                        path = %repo.path().display(),
                        error = %e,
                        "failed to canonicalize registered path; skipping for dup-check"
                    );
                    e
                })
                .ok()
        })
        .collect();

    for path in &found {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let registered = canonical_registered.contains(path);

        if registered {
            println!("{}", format_row(&name, path, true, width));
        } else if add {
            registry
                .add_from_path(path.clone())
                .with_context(|| format!("failed to register {}", path.display()))?;
            new_count += 1;
            println!("{}", format_row(&name, path, false, width));
        } else {
            println!("{}", format_row(&name, path, false, width));
        }
    }

    if add && new_count > 0 {
        config.repositories = registry.to_config_entries();
        config::save_to(&config, &config_path)
            .with_context(|| format!("failed to save config to {}", config_path.display()))?;
    }
    if add {
        println!("Added {new_count} repositories.");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_or_default_from(path: &Path) -> anyhow::Result<AppConfig> {
    match path.try_exists() {
        Ok(true) => Ok(config::load_from(path)?),
        Ok(false) => Ok(AppConfig::default()),
        Err(e) => Err(anyhow::Error::new(e)
            .context(format!("failed to probe config path {}", path.display()))),
    }
}

/// Column width for the name column: `max(8, longest basename)`.
pub(crate) fn compute_name_width(paths: &[PathBuf]) -> usize {
    let longest = paths
        .iter()
        .filter_map(|p| p.file_name().and_then(|s| s.to_str()).map(str::len))
        .max()
        .unwrap_or(0);
    longest.max(8)
}

/// Render a single row in the discovery listing.
pub(crate) fn format_row(name: &str, path: &Path, registered: bool, width: usize) -> String {
    if registered {
        format!(
            "  - {name:<width$}  ({path}) [already registered]",
            name = name,
            width = width,
            path = path.display()
        )
    } else {
        format!(
            "  ✓ {name:<width$}  ({path})",
            name = name,
            width = width,
            path = path.display()
        )
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn format_row_new() {
        let row = format_row("foo", Path::new("/tmp/foo"), false, 8);
        assert_eq!(row, "  ✓ foo       (/tmp/foo)");
    }

    #[test]
    fn format_row_registered() {
        let row = format_row("foo", Path::new("/tmp/foo"), true, 8);
        assert_eq!(row, "  - foo       (/tmp/foo) [already registered]");
    }

    #[test]
    fn compute_name_width_min_eight() {
        assert_eq!(compute_name_width(&[]), 8);
        assert_eq!(compute_name_width(&[PathBuf::from("/a/x")]), 8);
    }

    #[test]
    fn compute_name_width_uses_max() {
        let paths = vec![
            PathBuf::from("/a/short"),
            PathBuf::from("/a/much-longer-name"),
            PathBuf::from("/a/mid"),
        ];
        assert_eq!(compute_name_width(&paths), "much-longer-name".len());
    }

    #[tokio::test]
    async fn run_with_config_path_no_add_does_not_create_config() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("proj");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();

        let cfg_dir = tempdir().unwrap();
        let cfg_path = cfg_dir.path().join("config.toml");

        run_with_config_path(dir.path().to_path_buf(), false, Some(2), cfg_path.clone())
            .await
            .expect("scan run failed");

        assert!(
            !cfg_path.exists(),
            "config must not be written when --add is not passed"
        );
    }

    #[tokio::test]
    async fn run_with_config_path_add_persists_new_entries() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("proj");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();

        let cfg_dir = tempdir().unwrap();
        let cfg_path = cfg_dir.path().join("config.toml");

        run_with_config_path(dir.path().to_path_buf(), true, Some(2), cfg_path.clone())
            .await
            .expect("scan run failed");

        assert!(cfg_path.exists(), "config must be saved when --add is set");
        let saved = config::load_from(&cfg_path).unwrap();
        assert_eq!(saved.repositories.len(), 1);
        assert!(saved.repositories[0].path.ends_with("proj"));

        // Re-running must be idempotent (already registered → 0 new entries).
        run_with_config_path(dir.path().to_path_buf(), true, Some(2), cfg_path.clone())
            .await
            .expect("scan run failed (2nd)");
        let saved2 = config::load_from(&cfg_path).unwrap();
        assert_eq!(saved2.repositories.len(), 1);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn run_with_config_path_symlink_dedup() {
        let dir = tempdir().unwrap();
        // Create the real repo
        let real = dir.path().join("real");
        fs::create_dir_all(real.join(".git")).unwrap();

        // Create a symlink alias -> real
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        // Pre-populate config with the SYMLINK path (alias), not the canonical real path
        let cfg_dir = tempdir().unwrap();
        let cfg_path = cfg_dir.path().join("config.toml");
        let initial_config = git_clawler_core::AppConfig {
            repositories: vec![git_clawler_core::config::RepoEntry {
                worktree_of: None,
                name: Some("real".into()),
                path: alias.clone(),
                fetch: None,
                distro: None,
            }],
            ..Default::default()
        };
        config::save_to(&initial_config, &cfg_path).unwrap();

        // Scan with --add; the canonical real/ should be detected as already registered
        // via the symlink alias in config.
        run_with_config_path(dir.path().to_path_buf(), true, Some(2), cfg_path.clone())
            .await
            .expect("scan run failed");

        // Config must NOT have gained an extra entry for the canonical path.
        let saved = config::load_from(&cfg_path).unwrap();
        assert_eq!(
            saved.repositories.len(),
            1,
            "symlink alias in config should prevent duplicate canonical registration; got {:?}",
            saved.repositories
        );
    }
}
