#![cfg_attr(not(target_os = "windows"), allow(dead_code))]
use crate::dto::{
    AppConfigDto, CommandError, ListRepositoriesResponse, RecentCommitDto, RepoEntryDto, RepoRow,
    RepoStatusDto, RowStatus, UnpushedBranchDto,
};
use crate::wsl::BootstrapError;
use chrono::SecondsFormat;
use git_clawler_core::path_key::DistroPathKey;
use git_clawler_core::AppConfig;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
#[cfg(target_os = "windows")]
use std::process::{Command, Stdio};

/// Optional async callback that resolves the WSL helper path for a given
/// distro by lazily bootstrapping it (install + version handshake). Wired in
/// production by `main.rs` via `WslHelperRegistry`; tests pass `None` to
/// fall back to the env-only `resolve_helper_path()` path.
pub type BootstrapFn = dyn Fn(Option<String>) -> Pin<Box<dyn Future<Output = Result<String, BootstrapError>> + Send>>
    + Send
    + Sync;

fn repo_status_to_dto(s: git_clawler_core::RepoStatus) -> RepoStatusDto {
    let (open_issues, open_prs) = s
        .github
        .as_ref()
        .map(|g| (g.open_issues, g.open_prs))
        .unwrap_or((None, None));
    RepoStatusDto {
        current_branch: s.current_branch,
        uncommitted_files: s.uncommitted_files,
        unpushed_branches: s
            .unpushed_branches
            .into_iter()
            .map(|b| UnpushedBranchDto {
                name: b.name,
                ahead: b.ahead,
                has_upstream: b.has_upstream,
            })
            .collect(),
        ahead: s.ahead,
        behind: s.behind,
        open_issues,
        open_prs,
        conflict_predicted: s.conflict_predicted,
        fetch_warning: s.fetch_warning,
        recent_commits: s
            .recent_commits
            .into_iter()
            .map(|c| RecentCommitDto {
                short_hash: c.short_hash,
                summary: c.summary,
                author_name: c.author_name,
                committed_at: c.committed_at,
            })
            .collect(),
    }
}

pub async fn list_repositories_inner_with_config(
    config: AppConfig,
    bootstrap: Option<&BootstrapFn>,
) -> Result<ListRepositoriesResponse, CommandError> {
    #[cfg(target_os = "windows")]
    use crate::wsl::{helper_error_message, WslExecutor};
    use crate::wsl::{is_wsl_path, resolve_helper_path};
    use git_clawler_core::list::{ListRow, ScanOutcome};
    use std::collections::HashMap;

    type WslGroupOutcome = Result<
        Vec<(
            std::path::PathBuf,
            Result<(git_clawler_core::RepoStatus, Option<String>), String>,
        )>,
        String,
    >;

    let all_targets = git_clawler_core::list::build_targets(&config);

    // Partition into native and WSL groups (keyed by `(distro, fetch)` so
    // repositories with different fetch settings produce separate helper
    // invocations).
    let mut native_targets: Vec<git_clawler_core::list::ListTarget> = Vec::new();
    let mut wsl_groups: HashMap<(Option<String>, bool), Vec<git_clawler_core::list::ListTarget>> =
        HashMap::new();
    for target in all_targets {
        let is_wsl = target.distro.is_some() || is_wsl_path(&target.path);
        if is_wsl {
            wsl_groups
                .entry((target.distro.clone(), target.fetch))
                .or_default()
                .push(target);
        } else {
            native_targets.push(target);
        }
    }

    // 1) Native scan — no GitHub enrichment yet (enrichment runs once after
    //    all rows are collected, covering both native and WSL repos).
    let native_rows: Vec<ListRow> = git_clawler_core::list::scan_all(native_targets).await;

    // 2) WSL groups: launch via wsl.exe (Windows) or report HelperNotConfigured.
    let env_helper_path: Option<Result<String, String>> = if bootstrap.is_none() {
        Some(resolve_helper_path().map_err(|e| e.to_string()))
    } else {
        None
    };

    let wsl_futures: Vec<_> = wsl_groups
        .into_iter()
        .map(|((distro, fetch), targets)| {
            let env_helper = env_helper_path.clone();
            async move {
                let group_scanned_at = chrono::Utc::now();
                let paths: Vec<std::path::PathBuf> =
                    targets.iter().map(|t| t.path.clone()).collect();
                let helper_result: Result<String, String> = match bootstrap {
                    Some(boot) => boot(distro.clone()).await.map_err(|e| e.to_string()),
                    None => env_helper.expect("env_helper set when bootstrap is None"),
                };
                let outcome: WslGroupOutcome = match helper_result {
                    Err(e) => Err(e),
                    Ok(helper_path) => {
                        #[cfg(target_os = "windows")]
                        {
                            let runner = crate::wsl::TokioWslCommandRunner;
                            let executor = WslExecutor::new(runner, helper_path.clone());
                            match executor.scan_paths(distro.as_deref(), &paths, fetch).await {
                                Ok(results) => Ok(results
                                    .into_iter()
                                    .map(|(p, r)| (p, r.map_err(|he| helper_error_message(&he))))
                                    .collect()),
                                Err(e) => Err(format!("wsl: {e}")),
                            }
                        }
                        #[cfg(not(target_os = "windows"))]
                        {
                            let _ = &paths;
                            let _ = distro.as_deref();
                            let _ = helper_path;
                            let _ = fetch;
                            Err("wsl.exe unavailable on this platform".into())
                        }
                    }
                };
                (targets, outcome, group_scanned_at)
            }
        })
        .collect();

    let wsl_results = futures::future::join_all(wsl_futures).await;

    // 3) Build unified Vec<ListRow> (native + WSL) for single-pass enrichment.
    let mut all_rows: Vec<ListRow> = native_rows;
    for (targets, outcome, group_scanned_at) in wsl_results {
        match outcome {
            Ok(per_path) => {
                for (target, (_, res)) in targets.into_iter().zip(per_path) {
                    let (scan_outcome, remote_url) = match res {
                        Ok((status, url)) => (ScanOutcome::Ok(status), url),
                        Err(e) => (ScanOutcome::Err(e), None),
                    };
                    all_rows.push(ListRow {
                        target,
                        scanned_at: group_scanned_at,
                        outcome: scan_outcome,
                        remote_url,
                    });
                }
            }
            Err(err_msg) => {
                for target in targets {
                    all_rows.push(ListRow {
                        target,
                        scanned_at: group_scanned_at,
                        outcome: ScanOutcome::Err(err_msg.clone()),
                        remote_url: None,
                    });
                }
            }
        }
    }

    // 4) GitHub enrichment: one bounded-concurrent pass over ALL rows.
    //    WSL rows carry `remote_url` from the helper so `gh api` runs here
    //    (Windows side) without requiring `gh` inside every WSL distro.
    if config.github.enabled {
        git_clawler_core::list::enrich_with_github(&mut all_rows, &config.github).await;
    }

    // 5) Sort by registry index and convert to RepoRow DTOs.
    all_rows.sort_by_key(|r| r.target.index);
    let rows: Vec<RepoRow> = all_rows
        .into_iter()
        .map(|row| {
            let distro = row.target.distro.clone();
            let remote_url = row.remote_url.clone().or_else(|| {
                (!crate::wsl::is_wsl_path(&row.target.path))
                    .then(|| git_clawler_core::github::get_origin_url_sync(&row.target.path))
                    .flatten()
            });
            let status = match row.outcome {
                ScanOutcome::Ok(s) => RowStatus::Ok(repo_status_to_dto(s)),
                ScanOutcome::Err(e) => RowStatus::Err(e),
            };
            RepoRow {
                index: row.target.index,
                name: row.target.name,
                path: row.target.path.to_string_lossy().into_owned(),
                is_current: row.target.is_current,
                scanned_at: row.scanned_at.to_rfc3339_opts(SecondsFormat::Secs, true),
                status,
                distro,
                remote_url,
            }
        })
        .collect();

    Ok(ListRepositoriesResponse { rows })
}

pub async fn list_repositories_inner() -> Result<ListRepositoriesResponse, CommandError> {
    let config = git_clawler_core::config::load_or_default().map_err(|e| CommandError {
        kind: "config".into(),
        message: e.to_string(),
    })?;
    list_repositories_inner_with_config(config, None).await
}

pub async fn scan_root_inner(
    root: String,
    depth: u32,
    exclude: Vec<String>,
) -> Result<Vec<RepoEntryDto>, CommandError> {
    if root.trim().is_empty() {
        return Err(CommandError {
            kind: "validation".into(),
            message: "scan root path must not be empty".into(),
        });
    }
    if depth > 32 {
        return Err(CommandError {
            kind: "validation".into(),
            message: format!("scan root depth must be 0..=32, got {depth}"),
        });
    }

    let root_path = PathBuf::from(root.trim());
    if !root_path.is_absolute() {
        return Err(CommandError {
            kind: "validation".into(),
            message: format!("scan root path must be absolute: {}", root_path.display()),
        });
    }
    let canonical = std::fs::canonicalize(&root_path).map_err(|e| CommandError {
        kind: "validation".into(),
        message: format!("failed to resolve scan root {}: {e}", root_path.display()),
    })?;
    if !canonical.is_dir() {
        return Err(CommandError {
            kind: "validation".into(),
            message: format!(
                "scan root does not exist or is not a directory: {}",
                canonical.display()
            ),
        });
    }

    let mut excludes = git_clawler_core::scanner::default_excludes();
    excludes.extend(exclude.into_iter().filter(|value| !value.trim().is_empty()));
    excludes.sort();
    excludes.dedup();
    let opts = git_clawler_core::scanner::ScanOptions {
        max_depth: depth,
        excludes,
        follow_symlinks: false,
    };
    let found = tokio::task::spawn_blocking(move || {
        git_clawler_core::scanner::scan_for_repos(&canonical, &opts)
    })
    .await
    .map_err(|e| CommandError {
        kind: "internal".into(),
        message: format!("scanner task failed: {e}"),
    })?
    .map_err(|e| CommandError {
        kind: "validation".into(),
        message: e.to_string(),
    })?;

    Ok(found
        .into_iter()
        .map(|path| RepoEntryDto {
            name: None,
            path: displayable_path(&path),
            fetch: None,
            distro: None,
        })
        .collect())
}

#[cfg(target_os = "windows")]
async fn scan_root_wsl_inner(
    root: String,
    depth: u32,
    exclude: Vec<String>,
    distro: Option<String>,
    bootstrap: &BootstrapFn,
) -> Result<Vec<RepoEntryDto>, CommandError> {
    if root.trim().is_empty() {
        return Err(CommandError {
            kind: "validation".into(),
            message: "scan root path must not be empty".into(),
        });
    }
    if depth > 32 {
        return Err(CommandError {
            kind: "validation".into(),
            message: format!("scan root depth must be 0..=32, got {depth}"),
        });
    }
    if !root.starts_with('/') {
        return Err(CommandError {
            kind: "validation".into(),
            message: format!(
                "WSL scan root requires a Linux absolute path (must start with '/'): {root}"
            ),
        });
    }

    let helper_path = bootstrap(distro.clone()).await.map_err(|e| CommandError {
        kind: "internal".into(),
        message: e.to_string(),
    })?;
    let runner = crate::wsl::TokioWslCommandRunner;
    let executor = crate::wsl::WslExecutor::new(runner, helper_path);
    let paths = executor
        .scan_root(distro.as_deref(), Path::new(root.trim()), depth, &exclude)
        .await
        .map_err(|e| CommandError {
            kind: "validation".into(),
            message: format!("wsl scan root failed: {e}"),
        })?;

    Ok(paths
        .into_iter()
        .map(|path| RepoEntryDto {
            name: None,
            path: path.to_string_lossy().into_owned(),
            fetch: None,
            distro: distro.clone(),
        })
        .collect())
}

fn displayable_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    #[cfg(target_os = "windows")]
    {
        if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = raw.strip_prefix(r"\\?\") {
            return rest.to_string();
        }
    }
    raw.into_owned()
}

/// Tauri managed state: lazy WSL helper bootstrap registry. Wired in
/// `main.rs::setup`. The registry installs the helper into
/// `$HOME/.local/bin/git-clawler-wsl-helper` on first use per distro and
/// caches the resolved path for subsequent scans.
#[cfg(target_os = "windows")]
pub struct HelperRegistryState(pub std::sync::Arc<crate::wsl::WslHelperRegistry>);

/// Tauri managed state: absolute Windows-side path to the helper binary
/// shipped in the MSI's `resources/wsl-helper/x86_64-unknown-linux-musl/`
/// directory. Bootstrapping uses `wslpath` to translate this into an
/// in-distro path before copying.
#[cfg(target_os = "windows")]
pub struct HelperResourcePath(pub std::path::PathBuf);

#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn list_repositories(
    registry: tauri::State<'_, HelperRegistryState>,
    resource: tauri::State<'_, HelperResourcePath>,
) -> Result<ListRepositoriesResponse, CommandError> {
    use crate::wsl::BootstrapInput;

    let config = git_clawler_core::config::load_or_default().map_err(|e| CommandError {
        kind: "config".into(),
        message: e.to_string(),
    })?;

    let registry_arc = registry.0.clone();
    let resource_path = resource.0.clone();

    let bootstrap: Box<BootstrapFn> = Box::new(move |distro: Option<String>| {
        let registry = registry_arc.clone();
        let resource = resource_path.clone();
        Box::pin(async move {
            registry
                .ensure(BootstrapInput {
                    distro,
                    resource_helper_path: resource,
                })
                .await
        })
    });

    list_repositories_inner_with_config(config, Some(&*bootstrap)).await
}

#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn scan_root(
    registry: tauri::State<'_, HelperRegistryState>,
    resource: tauri::State<'_, HelperResourcePath>,
    root: String,
    depth: u32,
    exclude: Vec<String>,
    distro: Option<String>,
) -> Result<Vec<RepoEntryDto>, CommandError> {
    let normalized_distro = crate::wsl::normalize_distro(distro);
    let is_wsl_entry = normalized_distro.is_some() || crate::wsl::is_wsl_path(Path::new(&root));
    if is_wsl_entry {
        use crate::wsl::BootstrapInput;

        let registry_arc = registry.0.clone();
        let resource_path = resource.0.clone();
        let bootstrap: Box<BootstrapFn> = Box::new(move |distro: Option<String>| {
            let registry = registry_arc.clone();
            let resource = resource_path.clone();
            Box::pin(async move {
                registry
                    .ensure(BootstrapInput {
                        distro,
                        resource_helper_path: resource,
                    })
                    .await
            })
        });
        scan_root_wsl_inner(root, depth, exclude, normalized_distro, &*bootstrap).await
    } else {
        scan_root_inner(root, depth, exclude).await
    }
}

// ---------------------------------------------------------------------------
// Settings dialog: get_config / save_config
// ---------------------------------------------------------------------------

/// Tauri managed state — `save_config` を直列化するための非同期 mutex。
///
/// debounce すり抜けや並列 invoke 時の TOCTOU を抑制する。
pub struct SaveConfigLock(pub tokio::sync::Mutex<()>);

#[cfg(target_os = "windows")]
pub struct StartupErrorState(pub std::sync::Mutex<Vec<crate::realtime::RealtimeErrorPayload>>);

/// GUI 専用のバリデーション (Rust 側の安全網)。
///
/// 真偽ロジックと数値レンジは `git_clawler_core::config` の共有述語・定数
/// (`wsl_path_requirement_met` / `MIN`/`MAX_*`) に委譲し、core の `validate` と
/// 規則が乖離しないようにしている (整合性は `settings_tests` で検証)。本関数は
/// `AppConfigDto` に対する field-indexed な `kind:"validation"` メッセージ生成を
/// 担うため gui 側に残す。
///
/// **フロントの `settingsValidation.ts` は Rust を呼べないミラー。規則変更時は
/// core / gui / TS の 3 箇所を揃えること。**
fn validate_for_save(dto: &AppConfigDto) -> Result<(), CommandError> {
    // general.refresh_interval_secs (shared range from core)
    if !(git_clawler_core::config::MIN_REFRESH_INTERVAL_SECS
        ..=git_clawler_core::config::MAX_REFRESH_INTERVAL_SECS)
        .contains(&dto.general.refresh_interval_secs)
    {
        return Err(CommandError {
            kind: "validation".into(),
            message: format!(
                "refresh_interval_secs must be {}..={}, got {}",
                git_clawler_core::config::MIN_REFRESH_INTERVAL_SECS,
                git_clawler_core::config::MAX_REFRESH_INTERVAL_SECS,
                dto.general.refresh_interval_secs
            ),
        });
    }

    // notifications.cooldown_secs (shared range from core)
    if !(git_clawler_core::config::MIN_COOLDOWN_SECS..=git_clawler_core::config::MAX_COOLDOWN_SECS)
        .contains(&dto.notifications.cooldown_secs)
    {
        return Err(CommandError {
            kind: "validation".into(),
            message: format!(
                "notifications.cooldown_secs must be {}..={}, got {}",
                git_clawler_core::config::MIN_COOLDOWN_SECS,
                git_clawler_core::config::MAX_COOLDOWN_SECS,
                dto.notifications.cooldown_secs
            ),
        });
    }

    // repositories[*].path: !empty, absolute (Linux paths allowed for WSL entries),
    // exists (skipped for WSL entries since the path lives in another fs), no
    // duplicate `(distro, path)` pair.
    let mut seen_repo_keys = std::collections::HashSet::new();
    for (i, repo) in dto.repositories.iter().enumerate() {
        if repo.path.is_empty() {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!("repositories[{i}].path must not be empty"),
            });
        }
        let p = Path::new(&repo.path);
        let normalized_distro = crate::wsl::normalize_distro(repo.distro.clone());
        let is_wsl_entry = normalized_distro.is_some() || crate::wsl::is_wsl_path(p);
        if !git_clawler_core::config::wsl_path_requirement_met(repo.distro.as_deref(), p) {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!(
                    "repositories[{i}]: WSL entry requires a Linux absolute path (must start with '/'): {}",
                    repo.path
                ),
            });
        }
        if !git_clawler_core::config::is_repo_path_absolute(p) {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!("repositories[{i}].path must be absolute: {}", repo.path),
            });
        }
        if !is_wsl_entry && !p.is_dir() {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!(
                    "repositories[{i}].path does not exist or is not a directory: {}",
                    repo.path
                ),
            });
        }
        let key = DistroPathKey::current_host(normalized_distro.as_deref(), p);
        if !seen_repo_keys.insert(key) {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!(
                    "duplicate repository (distro={:?}, path={})",
                    repo.distro, repo.path
                ),
            });
        }
    }

    // scan_roots[*].path: !empty, absolute, is_dir for native roots, no duplicate. depth: 0..=32
    let mut seen_scan_roots = std::collections::HashSet::new();
    for (i, root) in dto.scan_roots.iter().enumerate() {
        if root.path.is_empty() {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!("scan_roots[{i}].path must not be empty"),
            });
        }
        let p = Path::new(&root.path);
        let normalized_distro = crate::wsl::normalize_distro(root.distro.clone());
        let is_wsl_entry = normalized_distro.is_some() || crate::wsl::is_wsl_path(p);
        if !git_clawler_core::config::wsl_path_requirement_met(root.distro.as_deref(), p) {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!(
                    "scan_roots[{i}]: WSL entry requires a Linux absolute path (must start with '/'): {}",
                    root.path
                ),
            });
        }
        if !git_clawler_core::config::is_repo_path_absolute(p) {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!("scan_roots[{i}].path must be absolute: {}", root.path),
            });
        }
        if !is_wsl_entry && !p.is_dir() {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!(
                    "scan_roots[{i}].path does not exist or is not a directory: {}",
                    root.path
                ),
            });
        }
        let key = DistroPathKey::current_host(normalized_distro.as_deref(), p);
        if !seen_scan_roots.insert(key) {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!(
                    "duplicate scan_roots (distro={:?}) path: {}",
                    root.distro, root.path
                ),
            });
        }
        if root.depth > git_clawler_core::config::MAX_SCAN_DEPTH {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!(
                    "scan_roots[{i}].depth must be 0..={}, got {}",
                    git_clawler_core::config::MAX_SCAN_DEPTH,
                    root.depth
                ),
            });
        }
    }

    // github.token_env: ^[A-Za-z_][A-Za-z0-9_]*$ (Some & non-empty 時のみ)
    if let Some(token_env) = dto.github.token_env.as_deref() {
        if !token_env.is_empty() && !is_valid_env_var_name(token_env) {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!(
                    "github.token_env must match ^[A-Za-z_][A-Za-z0-9_]*$: {token_env}"
                ),
            });
        }
    }

    // gui.global_shortcut: Some の場合 trim 後非空
    if dto.gui.theme != "dark" && dto.gui.theme != "light" {
        return Err(CommandError {
            kind: "validation".into(),
            message: format!("gui.theme must be 'dark' or 'light': {}", dto.gui.theme),
        });
    }
    if let Some(sc) = dto.gui.global_shortcut.as_deref() {
        if sc.trim().is_empty() {
            return Err(CommandError {
                kind: "validation".into(),
                message: "gui.global_shortcut must not be blank".into(),
            });
        }
    }
    if let Some(width) = dto.gui.window_width {
        if !(git_clawler_core::config::GUI_MIN_WINDOW_WIDTH
            ..=git_clawler_core::config::GUI_MAX_WINDOW_WIDTH)
            .contains(&width)
        {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!(
                    "gui.window_width must be {}..={}, got {}",
                    git_clawler_core::config::GUI_MIN_WINDOW_WIDTH,
                    git_clawler_core::config::GUI_MAX_WINDOW_WIDTH,
                    width
                ),
            });
        }
    }
    if let Some(height) = dto.gui.window_height {
        if !(git_clawler_core::config::GUI_MIN_WINDOW_HEIGHT
            ..=git_clawler_core::config::GUI_MAX_WINDOW_HEIGHT)
            .contains(&height)
        {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!(
                    "gui.window_height must be {}..={}, got {}",
                    git_clawler_core::config::GUI_MIN_WINDOW_HEIGHT,
                    git_clawler_core::config::GUI_MAX_WINDOW_HEIGHT,
                    height
                ),
            });
        }
    }
    validate_column_widths(&dto.gui.column_widths)?;

    Ok(())
}

fn validate_column_widths(column_widths: &BTreeMap<String, u32>) -> Result<(), CommandError> {
    for (column, width) in column_widths {
        if !(git_clawler_core::config::GUI_MIN_COLUMN_WIDTH
            ..=git_clawler_core::config::GUI_MAX_COLUMN_WIDTH)
            .contains(width)
        {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!(
                    "gui.column_widths.{column} must be {}..={}, got {}",
                    git_clawler_core::config::GUI_MIN_COLUMN_WIDTH,
                    git_clawler_core::config::GUI_MAX_COLUMN_WIDTH,
                    width
                ),
            });
        }
    }
    Ok(())
}

fn is_valid_env_var_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Test helper: load config from the given path, or default if it does not exist.
pub async fn get_config_inner_from_path(path: &Path) -> Result<AppConfigDto, CommandError> {
    let config = match path.try_exists() {
        Ok(true) => git_clawler_core::config::load_from(path).map_err(|e| CommandError {
            kind: "config".into(),
            message: e.to_string(),
        })?,
        Ok(false) => AppConfig::default(),
        Err(e) => {
            return Err(CommandError {
                kind: "internal".into(),
                message: e.to_string(),
            })
        }
    };
    Ok(AppConfigDto::from(config))
}

/// Test helper: validate, convert, and save to the given path.
pub async fn save_config_inner_to_path(
    path: &Path,
    mut dto: AppConfigDto,
) -> Result<(), CommandError> {
    validate_for_save(&dto)?;
    // Normalise the global shortcut so the on-disk form is canonical and
    // the live re-register path always sees a well-formed accelerator.
    if let Some(sc) = dto.gui.global_shortcut.take() {
        match crate::shortcut::normalize_accelerator(&sc) {
            Ok(normalized) => dto.gui.global_shortcut = Some(normalized),
            Err(e) => {
                return Err(CommandError {
                    kind: "validation".into(),
                    message: format!("gui.global_shortcut: invalid accelerator: {e}"),
                });
            }
        }
    }
    if path.try_exists().map_err(|e| CommandError {
        kind: "internal".into(),
        message: e.to_string(),
    })? {
        let current = git_clawler_core::config::load_from(path).map_err(|e| CommandError {
            kind: "config".into(),
            message: e.to_string(),
        })?;
        dto.gui.window_width = current.gui.window_width;
        dto.gui.window_height = current.gui.window_height;
        dto.gui.window_x = current.gui.window_x;
        dto.gui.window_y = current.gui.window_y;
        dto.gui.window_maximized = current.gui.window_maximized;
        dto.gui.column_widths = current.gui.column_widths;
    }
    // try_from only fails on an invalid gui.theme, which is a validation error.
    let config = AppConfig::try_from(dto).map_err(|e| CommandError {
        kind: "validation".into(),
        message: e,
    })?;
    // save_to re-runs core validation as a safety net; classify its errors so a
    // validation failure surfaces as `kind:"validation"` rather than `internal`.
    git_clawler_core::config::save_to(&config, path).map_err(|e| CommandError {
        kind: git_clawler_core::config::config_error_kind(&e).into(),
        message: e.to_string(),
    })
}

pub async fn get_config_inner() -> Result<AppConfigDto, CommandError> {
    let path = git_clawler_core::config::default_config_path().map_err(|e| CommandError {
        kind: "config".into(),
        message: e.to_string(),
    })?;
    get_config_inner_from_path(&path).await
}

pub async fn save_config_inner(dto: AppConfigDto) -> Result<(), CommandError> {
    let path = git_clawler_core::config::default_config_path().map_err(|e| CommandError {
        kind: "config".into(),
        message: e.to_string(),
    })?;
    save_config_inner_to_path(&path, dto).await
}

pub async fn save_column_widths_inner_to_path(
    path: &Path,
    column_widths: BTreeMap<String, u32>,
) -> Result<AppConfigDto, CommandError> {
    validate_column_widths(&column_widths)?;
    let mut config = match path.try_exists() {
        Ok(true) => git_clawler_core::config::load_from(path).map_err(|e| CommandError {
            kind: "config".into(),
            message: e.to_string(),
        })?,
        Ok(false) => AppConfig::default(),
        Err(e) => {
            return Err(CommandError {
                kind: "internal".into(),
                message: e.to_string(),
            })
        }
    };
    config.gui.column_widths = column_widths;
    git_clawler_core::config::save_to(&config, path).map_err(|e| CommandError {
        kind: "internal".into(),
        message: e.to_string(),
    })?;
    Ok(AppConfigDto::from(config))
}

pub async fn save_column_widths_inner(
    column_widths: BTreeMap<String, u32>,
) -> Result<AppConfigDto, CommandError> {
    let path = git_clawler_core::config::default_config_path().map_err(|e| CommandError {
        kind: "config".into(),
        message: e.to_string(),
    })?;
    save_column_widths_inner_to_path(&path, column_widths).await
}

pub fn request_refresh_inner(tx: &tokio::sync::mpsc::Sender<()>) -> Result<(), CommandError> {
    tx.try_send(()).map_err(|e| CommandError {
        kind: "refresh".into(),
        message: match e {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                "manual refresh queue is full".into()
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                "manual refresh loop is not running".into()
            }
        },
    })
}

pub fn github_page_url(remote_url: &str) -> Result<String, CommandError> {
    let (owner, repo) =
        git_clawler_core::github::parse_owner_repo(remote_url).ok_or_else(|| CommandError {
            kind: "validation".into(),
            message: "repository remote is not a supported GitHub URL".into(),
        })?;
    Ok(format!("https://github.com/{owner}/{repo}"))
}

#[cfg(target_os = "windows")]
fn open_with_explorer(target: &str) -> Result<(), CommandError> {
    Command::new("explorer.exe")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| CommandError {
            kind: "internal".into(),
            message: format!("failed to open Explorer: {e}"),
        })
}

#[cfg(target_os = "windows")]
async fn wsl_path_to_windows(path: &str, distro: Option<String>) -> Result<String, CommandError> {
    use crate::wsl::WslCommandRunner;
    let mut args = Vec::new();
    if let Some(distro) = crate::wsl::normalize_distro(distro) {
        args.extend(["-d".to_string(), distro]);
    }
    args.extend(["--exec".into(), "wslpath".into(), "-w".into(), path.into()]);
    let output = crate::wsl::TokioWslCommandRunner
        .run(&args)
        .await
        .map_err(|error| CommandError {
            kind: "internal".into(),
            message: error.to_string(),
        })?;
    if output.status != Some(0) {
        return Err(CommandError {
            kind: "internal".into(),
            message: format!(
                "wslpath failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }

    let converted = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if converted.is_empty() {
        return Err(CommandError {
            kind: "internal".into(),
            message: "wslpath returned an empty path".into(),
        });
    }
    Ok(converted)
}

#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn open_repository_folder(
    path: String,
    distro: Option<String>,
) -> Result<(), CommandError> {
    let trimmed = path.trim().to_string();
    if trimmed.is_empty() {
        return Err(CommandError {
            kind: "validation".into(),
            message: "repository path must not be empty".into(),
        });
    }

    let path_ref = Path::new(&trimmed);
    let target = if crate::wsl::normalize_distro(distro.clone()).is_some()
        || crate::wsl::is_wsl_path(path_ref)
    {
        wsl_path_to_windows(&trimmed, distro).await?
    } else {
        if !path_ref.is_dir() {
            return Err(CommandError {
                kind: "validation".into(),
                message: format!("repository path is not a directory: {trimmed}"),
            });
        }
        trimmed
    };
    tokio::task::spawn_blocking(move || open_with_explorer(&target))
        .await
        .map_err(|e| CommandError {
            kind: "internal".into(),
            message: format!("open folder task failed: {e}"),
        })?
}

#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn open_github_page(remote_url: String) -> Result<(), CommandError> {
    let page_url = github_page_url(&remote_url)?;
    tokio::task::spawn_blocking(move || open_with_explorer(&page_url))
        .await
        .map_err(|e| CommandError {
            kind: "internal".into(),
            message: format!("open GitHub task failed: {e}"),
        })?
}

#[cfg(not(target_os = "windows"))]
pub async fn open_repository_folder(
    _path: String,
    _distro: Option<String>,
) -> Result<(), CommandError> {
    Err(CommandError {
        kind: "internal".into(),
        message: "opening repository folders is supported only by the Windows GUI".into(),
    })
}

#[cfg(not(target_os = "windows"))]
pub async fn open_github_page(remote_url: String) -> Result<(), CommandError> {
    let _ = github_page_url(&remote_url)?;
    Err(CommandError {
        kind: "internal".into(),
        message: "opening GitHub pages is supported only by the Windows GUI".into(),
    })
}

#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn get_config(
    state: tauri::State<'_, SaveConfigLock>,
) -> Result<AppConfigDto, CommandError> {
    let _guard = state.0.lock().await;
    get_config_inner().await
}

#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn save_config(
    config: AppConfigDto,
    state: tauri::State<'_, SaveConfigLock>,
    policy_state: tauri::State<'_, crate::GuiPolicyState>,
) -> Result<(), CommandError> {
    let close_to_tray = config.gui.close_to_tray;
    let hide_from_taskbar = config.gui.hide_from_taskbar;
    let _guard = state.0.lock().await;
    save_config_inner(config).await?;
    policy_state.update(close_to_tray, hide_from_taskbar);
    Ok(())
}

#[cfg(target_os = "windows")]
#[tauri::command]
pub fn request_refresh(
    state: tauri::State<'_, crate::realtime::ManualRefreshState>,
) -> Result<(), CommandError> {
    request_refresh_inner(&state.0)
}

#[cfg(target_os = "windows")]
#[tauri::command]
pub fn get_startup_errors(
    state: tauri::State<'_, StartupErrorState>,
) -> Vec<crate::realtime::RealtimeErrorPayload> {
    let mut errors = match state.0.lock() {
        Ok(errors) => errors,
        Err(poisoned) => {
            tracing::warn!("startup error state mutex poisoned; recovering");
            poisoned.into_inner()
        }
    };
    std::mem::take(&mut *errors)
}

#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn save_column_widths(
    column_widths: BTreeMap<String, u32>,
    state: tauri::State<'_, SaveConfigLock>,
) -> Result<AppConfigDto, CommandError> {
    let _guard = state.0.lock().await;
    save_column_widths_inner(column_widths).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_clawler_core::config::{AppConfig, RepoEntry};
    use std::path::PathBuf;

    #[tokio::test]
    async fn returns_empty_when_no_repos() {
        let config = AppConfig::default();
        let result = list_repositories_inner_with_config(config, None)
            .await
            .unwrap();
        assert!(result.rows.is_empty());
    }

    #[tokio::test]
    async fn list_repositories_wsl_parallel_groups_preserve_index_order() {
        // Create config with 2 WSL entries (different distros); on Linux they both
        // error with "unavailable", but we just want to verify rows are returned
        // in index order.
        let config = AppConfig {
            repositories: vec![
                RepoEntry {
                    name: Some("first".into()),
                    path: PathBuf::from("/home/a"),
                    fetch: None,
                    distro: Some("Alpine".into()),
                },
                RepoEntry {
                    name: Some("second".into()),
                    path: PathBuf::from("/home/b"),
                    fetch: None,
                    distro: Some("Ubuntu".into()),
                },
            ],
            ..Default::default()
        };
        let result = list_repositories_inner_with_config(config, None)
            .await
            .unwrap();
        assert_eq!(result.rows.len(), 2);
        // Index order must be preserved
        assert_eq!(result.rows[0].name, "first");
        assert_eq!(result.rows[1].name, "second");
    }

    #[tokio::test]
    async fn returns_row_for_temp_git_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        git2::Repository::init(dir.path()).expect("git init");

        let config = AppConfig {
            repositories: vec![RepoEntry {
                name: Some("test-repo".into()),
                path: dir.path().to_path_buf(),
                fetch: None,
                distro: None,
            }],
            ..Default::default()
        };

        let result = list_repositories_inner_with_config(config, None)
            .await
            .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].name, "test-repo");
        assert!(matches!(result.rows[0].status, RowStatus::Ok(_)));
    }

    #[tokio::test]
    async fn err_row_for_nonexistent_path() {
        let config = AppConfig {
            repositories: vec![RepoEntry {
                name: Some("missing".into()),
                path: PathBuf::from("/nonexistent/path/that/does/not/exist"),
                fetch: None,
                distro: None,
            }],
            ..Default::default()
        };

        let result = list_repositories_inner_with_config(config, None)
            .await
            .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert!(matches!(result.rows[0].status, RowStatus::Err(_)));
    }

    #[test]
    fn github_page_url_normalizes_supported_remote_urls() {
        for remote in [
            "https://github.com/sahokun/git-clawler.git",
            "git@github.com:sahokun/git-clawler.git",
            "ssh://git@github.com/sahokun/git-clawler.git",
        ] {
            assert_eq!(
                github_page_url(remote).unwrap(),
                "https://github.com/sahokun/git-clawler"
            );
        }
    }

    #[test]
    fn github_page_url_rejects_unsupported_remote_urls() {
        for remote in [
            "https://gitlab.com/sahokun/git-clawler.git",
            "https://github.com/sahokun/git-clawler/tree/main",
            "https://github.com/-bad/git-clawler.git",
            "not a url",
        ] {
            assert_eq!(github_page_url(remote).unwrap_err().kind, "validation");
        }
    }

    #[tokio::test]
    async fn list_repositories_routes_distro_entries_to_wsl_branch() {
        // Entry with explicit `distro` is routed to the WSL path, regardless of
        // platform. On Linux, the WSL branch returns a clear error; on Windows
        // without GIT_CLAWLER_WSL_HELPER set, it surfaces HelperNotConfigured.
        // Either way: row exists, distro is preserved, status is Err, and the
        // path is *not* stat'd against the local fs (so a fake path is fine).
        let prev = std::env::var("GIT_CLAWLER_WSL_HELPER").ok();
        std::env::remove_var("GIT_CLAWLER_WSL_HELPER");

        let config = AppConfig {
            repositories: vec![RepoEntry {
                name: Some("wsl-repo".into()),
                path: PathBuf::from("/home/me/proj"),
                fetch: None,
                distro: Some("Ubuntu".into()),
            }],
            ..Default::default()
        };

        let result = list_repositories_inner_with_config(config, None)
            .await
            .unwrap();

        if let Some(v) = prev {
            std::env::set_var("GIT_CLAWLER_WSL_HELPER", v);
        }

        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].distro.as_deref(), Some("Ubuntu"));
        assert!(matches!(result.rows[0].status, RowStatus::Err(_)));
    }

    /// Verifies the bootstrap callback (HIGH #1 wiring) is invoked exactly
    /// once per WSL distro group and that the resolved helper path is
    /// forwarded to the WSL scan path. On non-Windows builds the WslExecutor
    /// branch is unavailable so the row ends up as Err, but the callback
    /// invocation is the contract we care about here.
    #[tokio::test]
    async fn bootstrap_callback_is_invoked_once_per_distro_group() {
        use crate::wsl::BootstrapError;
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        let prev = std::env::var("GIT_CLAWLER_WSL_HELPER").ok();
        std::env::remove_var("GIT_CLAWLER_WSL_HELPER");

        let config = AppConfig {
            repositories: vec![
                RepoEntry {
                    name: Some("ubuntu-1".into()),
                    path: PathBuf::from("/home/me/a"),
                    fetch: None,
                    distro: Some("Ubuntu".into()),
                },
                RepoEntry {
                    name: Some("ubuntu-2".into()),
                    path: PathBuf::from("/home/me/b"),
                    fetch: None,
                    distro: Some("Ubuntu".into()),
                },
                RepoEntry {
                    name: Some("debian-1".into()),
                    path: PathBuf::from("/srv/c"),
                    fetch: None,
                    distro: Some("Debian".into()),
                },
            ],
            ..Default::default()
        };

        let calls: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let calls_c = calls.clone();
        let count_c = count.clone();

        let bootstrap: Box<crate::commands::BootstrapFn> =
            Box::new(move |distro: Option<String>| {
                calls_c.lock().unwrap().push(distro.clone());
                count_c.fetch_add(1, Ordering::SeqCst);
                let path = format!(
                    "/home/{}/.local/bin/git-clawler-wsl-helper",
                    distro.as_deref().unwrap_or("default")
                );
                let fut: Pin<Box<dyn Future<Output = Result<String, BootstrapError>> + Send>> =
                    Box::pin(async move { Ok(path) });
                fut
            });

        let result = list_repositories_inner_with_config(config, Some(&*bootstrap))
            .await
            .unwrap();

        if let Some(v) = prev {
            std::env::set_var("GIT_CLAWLER_WSL_HELPER", v);
        }

        // 3 input repos → 3 rows.
        assert_eq!(result.rows.len(), 3);

        // Bootstrap must be called exactly once per distro group (Ubuntu, Debian).
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "bootstrap must run once per WSL distro group, not per repo"
        );
        let mut got: Vec<String> = calls
            .lock()
            .unwrap()
            .iter()
            .map(|d| d.clone().unwrap_or_default())
            .collect();
        got.sort();
        assert_eq!(got, vec!["Debian".to_string(), "Ubuntu".to_string()]);
    }

    /// On non-Windows builds the bootstrap-resolved path can't actually feed
    /// `WslExecutor` (no `wsl.exe`), but rows must still be produced (with an
    /// Err status) so the UI can render. This guards the contract that
    /// `bootstrap` errors propagate as row errors rather than aborting the
    /// whole call.
    #[tokio::test]
    async fn bootstrap_error_surfaces_as_row_error() {
        use crate::wsl::BootstrapError;
        use std::future::Future;
        use std::pin::Pin;

        let prev = std::env::var("GIT_CLAWLER_WSL_HELPER").ok();
        std::env::remove_var("GIT_CLAWLER_WSL_HELPER");

        let config = AppConfig {
            repositories: vec![RepoEntry {
                name: Some("wsl-repo".into()),
                path: PathBuf::from("/home/me/proj"),
                fetch: None,
                distro: Some("Ubuntu".into()),
            }],
            ..Default::default()
        };

        let bootstrap: Box<crate::commands::BootstrapFn> = Box::new(|_distro| {
            let fut: Pin<Box<dyn Future<Output = Result<String, BootstrapError>> + Send>> =
                Box::pin(async move { Err(BootstrapError::NoInstallPath) });
            fut
        });

        let result = list_repositories_inner_with_config(config, Some(&*bootstrap))
            .await
            .unwrap();

        if let Some(v) = prev {
            std::env::set_var("GIT_CLAWLER_WSL_HELPER", v);
        }

        assert_eq!(result.rows.len(), 1);
        assert!(matches!(result.rows[0].status, RowStatus::Err(_)));
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use crate::dto::{
        AppConfigDto, GeneralConfigDto, GithubConfigDto, GuiConfigDto, RepoEntryDto,
        ScanRootEntryDto,
    };
    use git_clawler_core::config::RepoEntry;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    fn minimal_dto() -> AppConfigDto {
        AppConfigDto {
            general: GeneralConfigDto {
                refresh_interval_secs: 30,
                auto_fetch: false,
            },
            repositories: vec![],
            scan_roots: vec![],
            github: GithubConfigDto {
                token_env: None,
                enabled: false,
                enable_issues: true,
                enable_prs: true,
            },
            notifications: crate::dto::NotificationsConfigDto {
                enabled: false,
                cooldown_secs: 300,
            },
            gui: GuiConfigDto {
                theme: "dark".into(),
                start_in_tray: false,
                hide_from_taskbar: false,
                close_to_tray: false,
                global_shortcut: None,
                window_x: None,
                window_y: None,
                window_width: None,
                window_height: None,
                window_maximized: None,
                column_widths: BTreeMap::new(),
            },
        }
    }

    #[tokio::test]
    async fn scan_root_discovers_git_repositories() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(&repo).unwrap();
        git2::Repository::init(&repo).unwrap();

        let found = scan_root_inner(dir.path().to_string_lossy().into_owned(), 1, vec![])
            .await
            .unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].path,
            displayable_path(&repo.canonicalize().unwrap())
        );
        assert_eq!(found[0].name, None);
    }

    #[tokio::test]
    async fn scan_root_rejects_depth_over_32() {
        let dir = tempfile::tempdir().unwrap();
        let err = scan_root_inner(dir.path().to_string_lossy().into_owned(), 33, vec![])
            .await
            .unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn get_save_roundtrip_via_inner_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let dto = minimal_dto();
        save_config_inner_to_path(&path, dto.clone()).await.unwrap();
        let loaded = get_config_inner_from_path(&path).await.unwrap();
        assert_eq!(loaded.general.refresh_interval_secs, 30);
        assert!(!loaded.general.auto_fetch);
    }

    #[tokio::test]
    async fn save_config_preserves_existing_gui_layout_preferences() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut initial = minimal_dto();
        initial.gui.window_x = Some(-900);
        initial.gui.window_y = Some(32);
        initial.gui.window_width = Some(1200);
        initial.gui.window_height = Some(800);
        initial.gui.window_maximized = Some(true);
        initial.gui.column_widths = BTreeMap::from([("branch".into(), 140)]);
        save_config_inner_to_path(&path, initial).await.unwrap();

        let mut next = minimal_dto();
        next.gui.theme = "light".into();
        next.gui.window_x = Some(10);
        next.gui.window_y = Some(20);
        next.gui.window_width = Some(640);
        next.gui.window_height = Some(480);
        next.gui.window_maximized = Some(false);
        next.gui.column_widths = BTreeMap::from([("branch".into(), 320)]);
        save_config_inner_to_path(&path, next).await.unwrap();

        let loaded = get_config_inner_from_path(&path).await.unwrap();
        assert_eq!(loaded.gui.theme, "light");
        assert_eq!(loaded.gui.window_x, Some(-900));
        assert_eq!(loaded.gui.window_y, Some(32));
        assert_eq!(loaded.gui.window_width, Some(1200));
        assert_eq!(loaded.gui.window_height, Some(800));
        assert_eq!(loaded.gui.window_maximized, Some(true));
        assert_eq!(loaded.gui.column_widths.get("branch"), Some(&140));
    }

    #[tokio::test]
    async fn save_column_widths_updates_only_layout_preferences() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut initial = minimal_dto();
        initial.general.refresh_interval_secs = 77;
        initial.gui.theme = "light".into();
        save_config_inner_to_path(&path, initial).await.unwrap();

        let loaded = save_column_widths_inner_to_path(
            &path,
            BTreeMap::from([("branch".into(), 180), ("conflict".into(), 96)]),
        )
        .await
        .unwrap();

        assert_eq!(loaded.general.refresh_interval_secs, 77);
        assert_eq!(loaded.gui.theme, "light");
        assert_eq!(loaded.gui.column_widths.get("branch"), Some(&180));
        assert_eq!(loaded.gui.column_widths.get("conflict"), Some(&96));
    }

    #[tokio::test]
    async fn save_creates_file_when_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("subdir").join("config.toml");
        save_config_inner_to_path(&path, minimal_dto())
            .await
            .unwrap();
        assert!(path.exists());
    }

    #[tokio::test]
    async fn validate_rejects_zero_refresh_interval() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.general.refresh_interval_secs = 0;
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn validate_rejects_refresh_interval_over_max() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.general.refresh_interval_secs = 86401;
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn validate_rejects_notification_cooldown_below_min() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.notifications.cooldown_secs = 0;
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
        assert!(err.message.contains("notifications.cooldown_secs"));
    }

    #[tokio::test]
    async fn validate_rejects_notification_cooldown_over_max() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.notifications.cooldown_secs = 86401;
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
        assert!(err.message.contains("notifications.cooldown_secs"));
    }

    #[tokio::test]
    async fn validate_accepts_notification_cooldown_bounds() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for cooldown_secs in [1, 86400] {
            let mut dto = minimal_dto();
            dto.notifications.cooldown_secs = cooldown_secs;
            save_config_inner_to_path(&path, dto)
                .await
                .unwrap_or_else(|e| panic!("expected ok for {cooldown_secs}, got {e:?}"));
        }
    }

    #[tokio::test]
    async fn validate_rejects_depth_over_32() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.scan_roots.push(ScanRootEntryDto {
            path: dir.path().to_string_lossy().into_owned(),
            depth: 33,
            exclude: vec![],
            distro: None,
        });
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn validate_rejects_relative_repo_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.repositories.push(RepoEntryDto {
            name: None,
            path: "relative/path".into(),
            fetch: None,
            distro: None,
        });
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn validate_rejects_nonexistent_repo_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        let missing = dir.path().join("missing-native-repo");
        dto.repositories.push(RepoEntryDto {
            name: None,
            path: missing.to_string_lossy().into_owned(),
            fetch: None,
            distro: None,
        });
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn validate_rejects_duplicate_repo_paths() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let repo_path = dir.path().to_string_lossy().into_owned();
        let mut dto = minimal_dto();
        dto.repositories.push(RepoEntryDto {
            name: None,
            path: repo_path.clone(),
            fetch: None,
            distro: None,
        });
        dto.repositories.push(RepoEntryDto {
            name: None,
            path: repo_path,
            fetch: None,
            distro: None,
        });
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn validate_rejects_invalid_token_env() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for bad in &["1FOO", "FOO BAR", "FOO-BAR"] {
            let mut dto = minimal_dto();
            dto.github.token_env = Some((*bad).to_string());
            let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
            assert_eq!(
                err.kind, "validation",
                "expected validation error for {bad}"
            );
        }
    }

    #[tokio::test]
    async fn validate_accepts_valid_token_env() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for good in &["GITHUB_TOKEN", "_X1"] {
            let mut dto = minimal_dto();
            dto.github.token_env = Some((*good).to_string());
            save_config_inner_to_path(&path, dto)
                .await
                .unwrap_or_else(|e| panic!("expected ok for {good}, got {e:?}"));
        }
    }

    #[tokio::test]
    async fn validate_rejects_blank_global_shortcut() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.gui.global_shortcut = Some("   ".into());
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn validate_accepts_none_global_shortcut() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.gui.global_shortcut = None;
        save_config_inner_to_path(&path, dto).await.unwrap();
    }

    #[tokio::test]
    async fn dto_roundtrip_preserves_paths() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().to_path_buf();
        let config = AppConfig {
            repositories: vec![RepoEntry {
                name: Some("test".into()),
                path: repo_path.clone(),
                fetch: None,
                distro: None,
            }],
            ..Default::default()
        };
        let dto = AppConfigDto::from(config);
        let config2 = AppConfig::try_from(dto).unwrap();
        assert_eq!(config2.repositories[0].path, repo_path);
        assert_eq!(config2.repositories[0].name.as_deref(), Some("test"));
    }

    #[tokio::test]
    async fn validate_rejects_repo_path_that_is_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("not_a_dir.txt");
        std::fs::write(&file_path, b"x").unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.repositories.push(RepoEntryDto {
            name: None,
            path: file_path.to_string_lossy().into_owned(),
            fetch: None,
            distro: None,
        });
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn validate_rejects_nonexistent_scan_root() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        let missing_path = dir.path().join("missing-scan-root");
        dto.scan_roots.push(ScanRootEntryDto {
            path: missing_path.to_string_lossy().into_owned(),
            depth: 1,
            exclude: vec![],
            distro: None,
        });
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn validate_rejects_scan_root_that_is_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("scan_file.txt");
        std::fs::write(&file_path, b"x").unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.scan_roots.push(ScanRootEntryDto {
            path: file_path.to_string_lossy().into_owned(),
            depth: 1,
            exclude: vec![],
            distro: None,
        });
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn validate_accepts_wsl_scan_root_with_distro() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.scan_roots.push(ScanRootEntryDto {
            path: "/home/example/projects".into(),
            depth: 3,
            exclude: vec![],
            distro: Some("Ubuntu".into()),
        });
        save_config_inner_to_path(&path, dto).await.unwrap();
        let loaded = get_config_inner_from_path(&path).await.unwrap();
        assert_eq!(loaded.scan_roots[0].distro, Some("Ubuntu".into()));
    }

    #[tokio::test]
    async fn validate_rejects_wsl_scan_root_with_windows_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.scan_roots.push(ScanRootEntryDto {
            path: "C:\\workspace".into(),
            depth: 3,
            exclude: vec![],
            distro: Some("Ubuntu".into()),
        });
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
        assert!(err.message.contains("Linux absolute path"));
    }

    #[tokio::test]
    async fn validate_rejects_duplicate_scan_roots() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let scan_path = dir.path().to_string_lossy().into_owned();
        let mut dto = minimal_dto();
        dto.scan_roots.push(ScanRootEntryDto {
            path: scan_path.clone(),
            depth: 1,
            exclude: vec![],
            distro: None,
        });
        dto.scan_roots.push(ScanRootEntryDto {
            path: scan_path,
            depth: 2,
            exclude: vec![],
            distro: None,
        });
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn save_returns_validation_kind_on_invalid_dto() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.general.refresh_interval_secs = 0;
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn save_config_normalizes_shortcut_alias() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.gui.global_shortcut = Some("ctrl+shift+g".into());
        save_config_inner_to_path(&path, dto).await.unwrap();
        let loaded = get_config_inner_from_path(&path).await.unwrap();
        assert_eq!(
            loaded.gui.global_shortcut.as_deref(),
            Some("Control+Shift+G")
        );
    }

    #[tokio::test]
    async fn save_config_rejects_invalid_shortcut() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.gui.global_shortcut = Some("Ctrl+Shift".into());
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
        assert!(
            err.message.contains("gui.global_shortcut"),
            "expected message to mention gui.global_shortcut, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn save_config_preserves_page_up_shortcut_casing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.gui.global_shortcut = Some("ctrl+pageup".into());
        save_config_inner_to_path(&path, dto).await.unwrap();
        let loaded = get_config_inner_from_path(&path).await.unwrap();
        assert_eq!(
            loaded.gui.global_shortcut.as_deref(),
            Some("Control+PageUp")
        );
    }

    #[tokio::test]
    async fn validate_rejects_unknown_theme() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.gui.theme = "blue".into();
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn validate_rejects_invalid_column_width() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut dto = minimal_dto();
        dto.gui.column_widths.insert("branch".into(), 1);
        let err = save_config_inner_to_path(&path, dto).await.unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[tokio::test]
    async fn request_refresh_inner_sends_signal() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
        request_refresh_inner(&tx).unwrap();
        assert_eq!(rx.try_recv(), Ok(()));
    }

    #[tokio::test]
    async fn request_refresh_inner_reports_full_queue() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<()>(1);
        tx.try_send(()).unwrap();
        let err = request_refresh_inner(&tx).unwrap_err();
        assert_eq!(err.kind, "refresh");
        assert!(err.message.contains("full"));
    }

    #[tokio::test]
    async fn request_refresh_inner_reports_closed_queue() {
        let (tx, rx) = tokio::sync::mpsc::channel::<()>(1);
        drop(rx);
        let err = request_refresh_inner(&tx).unwrap_err();
        assert_eq!(err.kind, "refresh");
        assert!(err.message.contains("not running"));
    }

    // -----------------------------------------------------------------------
    // gui `validate_for_save` ↔ core `validate` consistency.
    //
    // core::validate enforces a SUBSET of the GUI rules: only absolute-path,
    // WSL-posix, (distro, path) duplication, and GUI layout ranges. It does NOT
    // check empty paths, existence, refresh interval, cooldown, depth, token_env
    // or theme. So we can only assert agreement on those shared rules, and we
    // pin the asymmetry separately so a future core change is forced to update
    // this test deliberately. All cases use WSL entries (distro=Some + posix
    // path) so the GUI's `is_dir` existence check is skipped.
    // -----------------------------------------------------------------------

    /// (gui_ok, core_ok) for the same DTO. A `try_from` failure (invalid theme)
    /// counts as core-not-ok, mirroring how the save path treats it.
    fn gui_and_core_results(dto: AppConfigDto) -> (bool, bool) {
        let gui_ok = validate_for_save(&dto).is_ok();
        let core_ok = match AppConfig::try_from(dto) {
            Ok(cfg) => git_clawler_core::config::validate(&cfg).is_ok(),
            Err(_) => false,
        };
        (gui_ok, core_ok)
    }

    fn wsl_repo(path: &str) -> RepoEntryDto {
        RepoEntryDto {
            name: None,
            path: path.into(),
            fetch: None,
            distro: Some("Ubuntu".into()),
        }
    }

    #[test]
    fn gui_and_core_agree_on_valid_wsl_config() {
        let mut dto = minimal_dto();
        dto.repositories = vec![wsl_repo("/home/u/proj")];
        assert_eq!(gui_and_core_results(dto), (true, true));
    }

    #[test]
    fn gui_and_core_agree_rejecting_wsl_non_linux_path() {
        // Both layers reject, though via different rules: core checks
        // absolute-path before the WSL rule, so it rejects with `RelativePath`
        // while the GUI rejects with the WSL message. We only assert ok/not-ok
        // agreement, which holds either way.
        let mut dto = minimal_dto();
        dto.repositories = vec![wsl_repo("C:\\proj")];
        assert_eq!(gui_and_core_results(dto), (false, false));
    }

    #[test]
    fn gui_and_core_agree_rejecting_duplicate_distro_path() {
        let mut dto = minimal_dto();
        dto.repositories = vec![wsl_repo("/home/u/proj"), wsl_repo("/home/u/proj")];
        assert_eq!(gui_and_core_results(dto), (false, false));
    }

    #[test]
    fn gui_and_core_agree_rejecting_window_width_out_of_range() {
        let mut dto = minimal_dto();
        dto.repositories = vec![wsl_repo("/home/u/proj")];
        dto.gui.window_width = Some(git_clawler_core::config::GUI_MIN_WINDOW_WIDTH - 1);
        assert_eq!(gui_and_core_results(dto), (false, false));
    }

    #[test]
    fn gui_and_core_agree_rejecting_column_width_out_of_range() {
        let mut dto = minimal_dto();
        dto.repositories = vec![wsl_repo("/home/u/proj")];
        dto.gui.column_widths = BTreeMap::from([(
            "branch".into(),
            git_clawler_core::config::GUI_MAX_COLUMN_WIDTH + 1,
        )]);
        assert_eq!(gui_and_core_results(dto), (false, false));
    }

    #[test]
    fn zero_refresh_interval_is_rejected_before_starting_a_timer() {
        // Both config entry points reject zero: tokio::time::interval would
        // otherwise panic and silently stop refresh after a hand-edited config.
        let mut dto = minimal_dto();
        dto.general.refresh_interval_secs = 0;
        assert_eq!(gui_and_core_results(dto), (false, false));
    }
}
