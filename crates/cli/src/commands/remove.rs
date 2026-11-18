//! `gclawl remove <path>` — remove a repository entry from config only.

use std::path::{Path, PathBuf};

use anyhow::Context;
use git_clawler_core::{config, AppConfig, Registry, RegistryError};

/// Run `gclawl remove` against the user's default config file.
pub async fn run(path: PathBuf) -> anyhow::Result<()> {
    let config_path = config::default_config_path()?;
    run_with_config_path(path, config_path).await
}

/// Run `gclawl remove` against an explicit config path.
///
/// Exposed for tests so the user's real config is never touched.
pub(crate) async fn run_with_config_path(
    path: PathBuf,
    config_path: PathBuf,
) -> anyhow::Result<()> {
    let mut config = load_or_default_from(&config_path)?;
    let mut registry = Registry::from_config(&config);

    let removed = registry
        .remove_by_path_unambiguous(&path)
        .map_err(|e| match e {
            RegistryError::NotFoundByPath(p) => {
                anyhow::anyhow!("repository path is not registered: {}", p.display())
            }
            RegistryError::AmbiguousPath { path, matches } => anyhow::anyhow!(
                "repository path is ambiguous and was not removed: {}\nmatching entries:\n{}",
                path.display(),
                matches
                    .into_iter()
                    .map(|m| format!("  - {m}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            other => anyhow::Error::new(other),
        })?;

    config.repositories = registry.to_config_entries();
    let removed_entry = git_clawler_core::config::RepoEntry {
        name: None,
        path: removed.path().to_path_buf(),
        fetch: None,
        distro: removed.distro.clone(),
        worktree_of: removed.worktree_of.clone(),
    };
    let exclusion = git_clawler_core::discovery::record_exclusion(&mut config, &removed_entry);
    config::save_to(&config, &config_path)
        .with_context(|| format!("failed to save config to {}", config_path.display()))?;

    println!("Removed from config: {}", removed.path().display());
    if let Some((root, pattern)) = exclusion {
        println!(
            "Excluded from automatic discovery under {}: {pattern}",
            root.display()
        );
    }
    Ok(())
}

fn load_or_default_from(path: &Path) -> anyhow::Result<AppConfig> {
    match path.try_exists() {
        Ok(true) => Ok(config::load_from(path)?),
        Ok(false) => Ok(AppConfig::default()),
        Err(e) => Err(anyhow::Error::new(e)
            .context(format!("failed to probe config path {}", path.display()))),
    }
}
