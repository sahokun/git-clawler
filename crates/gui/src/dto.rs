//! Data Transfer Objects for the GUI frontend (serde-serializable).
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]
use git_clawler_core::config::{
    AppConfig, GeneralConfig, GithubConfig, GuiConfig, GuiTheme, NotificationsConfig, RepoEntry,
    ScanRootEntry,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRepositoriesResponse {
    pub rows: Vec<RepoRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRow {
    pub index: usize,
    pub name: String,
    pub path: String,
    pub is_current: bool,
    pub scanned_at: String, // RFC 3339
    pub status: RowStatus,
    pub distro: Option<String>,
    #[serde(default)]
    pub remote_url: Option<String>,
    /// Main repository path when this row is a linked worktree.
    #[serde(default)]
    pub worktree_of: Option<String>,
}

/// Per-repository scan outcome.
///
/// Serialised as `{ "kind": "ok", "value": { … } }` or
/// `{ "kind": "err", "value": "…" }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum RowStatus {
    Ok(RepoStatusDto),
    Err(String),
    /// The registered path no longer exists. Never removed automatically.
    NotFound(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoStatusDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_warning: Option<String>,
    pub current_branch: Option<String>,
    pub uncommitted_files: u32,
    pub unpushed_branches: Vec<UnpushedBranchDto>,
    pub ahead: u32,
    pub behind: u32,
    #[serde(default)]
    pub open_issues: Option<u64>,
    #[serde(default)]
    pub open_prs: Option<u64>,
    /// Pull conflict prediction. `null` ⇒ undetermined.
    #[serde(default)]
    pub conflict_predicted: Option<bool>,
    #[serde(default)]
    pub recent_commits: Vec<RecentCommitDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentCommitDto {
    pub short_hash: String,
    pub summary: String,
    pub author_name: String,
    pub committed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnpushedBranchDto {
    pub name: String,
    pub ahead: u32,
    pub has_upstream: bool,
}

/// Structured error returned from a Tauri command.
///
/// `kind` ∈ `{"config", "internal", "validation"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandError {
    pub kind: String,
    pub message: String,
}

// ---------------------------------------------------------------------------
// AppConfigDto: serialization boundary for the GUI settings dialog.
//
// DTO layer for converting values such as `PathBuf` into frontend-friendly
// strings when passing `git_clawler_core::config::AppConfig` to the frontend.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfigDto {
    pub general: GeneralConfigDto,
    pub repositories: Vec<RepoEntryDto>,
    pub scan_roots: Vec<ScanRootEntryDto>,
    pub github: GithubConfigDto,
    pub notifications: NotificationsConfigDto,
    pub gui: GuiConfigDto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfigDto {
    pub refresh_interval_secs: u64,
    /// core 側 `fetch_on_refresh` に対応 (フロントでは `auto_fetch` 名)。
    pub auto_fetch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoEntryDto {
    pub name: Option<String>,
    pub path: String,
    pub fetch: Option<bool>,
    pub distro: Option<String>,
    /// Main repository path for an auto-registered linked worktree; carried
    /// through Settings saves so it is not lost.
    #[serde(default)]
    pub worktree_of: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanRootEntryDto {
    pub path: String,
    pub depth: u32,
    pub exclude: Vec<String>,
    pub distro: Option<String>,
    #[serde(default = "default_true")]
    pub auto: bool,
    #[serde(default = "default_true")]
    pub worktrees: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubConfigDto {
    /// 環境変数名。空文字 / `None` はデフォルト ("GITHUB_TOKEN") にフォールバック。
    pub token_env: Option<String>,
    pub enabled: bool,
    pub enable_issues: bool,
    pub enable_prs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationsConfigDto {
    pub enabled: bool,
    pub cooldown_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiConfigDto {
    pub theme: String,
    pub start_in_tray: bool,
    pub hide_from_taskbar: bool,
    pub close_to_tray: bool,
    /// Empty strings become `None`; saving converts `None` back to an empty string.
    pub global_shortcut: Option<String>,
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
    pub window_width: Option<u32>,
    pub window_height: Option<u32>,
    pub window_maximized: Option<bool>,
    pub column_widths: BTreeMap<String, u32>,
}

impl From<AppConfig> for AppConfigDto {
    fn from(cfg: AppConfig) -> Self {
        AppConfigDto {
            general: GeneralConfigDto {
                refresh_interval_secs: cfg.general.refresh_interval_secs,
                auto_fetch: cfg.general.fetch_on_refresh,
            },
            repositories: cfg
                .repositories
                .into_iter()
                .map(|r| RepoEntryDto {
                    name: r.name,
                    path: r.path.to_string_lossy().into_owned(),
                    fetch: r.fetch,
                    distro: r.distro,
                    worktree_of: r.worktree_of.map(|p| p.to_string_lossy().into_owned()),
                })
                .collect(),
            scan_roots: cfg
                .scan_roots
                .into_iter()
                .map(|s| ScanRootEntryDto {
                    path: s.path.to_string_lossy().into_owned(),
                    depth: s.depth,
                    exclude: s.exclude,
                    distro: s.distro,
                    auto: s.auto,
                    worktrees: s.worktrees,
                })
                .collect(),
            github: GithubConfigDto {
                token_env: if cfg.github.token_env.is_empty() {
                    None
                } else {
                    Some(cfg.github.token_env)
                },
                enabled: cfg.github.enabled,
                enable_issues: cfg.github.enable_issues,
                enable_prs: cfg.github.enable_prs,
            },
            notifications: NotificationsConfigDto {
                enabled: cfg.notifications.enabled,
                cooldown_secs: cfg.notifications.cooldown_secs,
            },
            gui: GuiConfigDto {
                theme: cfg.gui.theme.as_str().to_string(),
                start_in_tray: cfg.gui.start_in_tray,
                hide_from_taskbar: cfg.gui.hide_from_taskbar,
                close_to_tray: cfg.gui.close_to_tray,
                global_shortcut: if cfg.gui.global_shortcut.is_empty() {
                    None
                } else {
                    Some(cfg.gui.global_shortcut)
                },
                window_x: cfg.gui.window_x,
                window_y: cfg.gui.window_y,
                window_width: cfg.gui.window_width,
                window_height: cfg.gui.window_height,
                window_maximized: cfg.gui.window_maximized,
                column_widths: cfg.gui.column_widths,
            },
        }
    }
}

impl TryFrom<AppConfigDto> for AppConfig {
    type Error = String;

    fn try_from(dto: AppConfigDto) -> Result<Self, Self::Error> {
        Ok(AppConfig {
            general: GeneralConfig {
                refresh_interval_secs: dto.general.refresh_interval_secs,
                fetch_on_refresh: dto.general.auto_fetch,
            },
            repositories: dto
                .repositories
                .into_iter()
                .map(|r| RepoEntry {
                    worktree_of: r.worktree_of.map(PathBuf::from),
                    name: r.name,
                    path: PathBuf::from(&r.path),
                    fetch: r.fetch,
                    distro: crate::wsl::normalize_distro(r.distro),
                })
                .collect(),
            scan_roots: dto
                .scan_roots
                .into_iter()
                .map(|s| ScanRootEntry {
                    auto: s.auto,
                    worktrees: s.worktrees,
                    path: PathBuf::from(&s.path),
                    depth: s.depth,
                    exclude: s.exclude,
                    distro: crate::wsl::normalize_distro(s.distro),
                })
                .collect(),
            github: GithubConfig {
                token_env: dto
                    .github
                    .token_env
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "GITHUB_TOKEN".to_string()),
                enabled: dto.github.enabled,
                enable_issues: dto.github.enable_issues,
                enable_prs: dto.github.enable_prs,
            },
            notifications: NotificationsConfig {
                enabled: dto.notifications.enabled,
                cooldown_secs: dto.notifications.cooldown_secs,
            },
            gui: GuiConfig {
                theme: GuiTheme::parse(&dto.gui.theme)
                    .ok_or_else(|| format!("invalid gui.theme: {}", dto.gui.theme))?,
                start_in_tray: dto.gui.start_in_tray,
                hide_from_taskbar: dto.gui.hide_from_taskbar,
                close_to_tray: dto.gui.close_to_tray,
                global_shortcut: dto.gui.global_shortcut.unwrap_or_default(),
                window_x: dto.gui.window_x,
                window_y: dto.gui.window_y,
                window_width: dto.gui.window_width,
                window_height: dto.gui.window_height,
                window_maximized: dto.gui.window_maximized,
                column_widths: dto.gui.column_widths,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_clawler_core::config::{AppConfig, RepoEntry, ScanRootEntry};
    use std::path::PathBuf;

    #[test]
    fn app_config_dto_roundtrip_with_distro() {
        let cfg = AppConfig {
            repositories: vec![RepoEntry {
                worktree_of: None,
                name: Some("proj".into()),
                path: PathBuf::from("/home/user/proj"),
                fetch: None,
                distro: Some("Ubuntu".into()),
            }],
            ..Default::default()
        };
        let dto = AppConfigDto::from(cfg);
        assert_eq!(dto.repositories[0].distro, Some("Ubuntu".into()));

        let back = AppConfig::try_from(dto).unwrap();
        assert_eq!(back.repositories[0].distro, Some("Ubuntu".into()));
    }

    #[test]
    fn app_config_dto_roundtrip_scan_root_with_distro() {
        let cfg = AppConfig {
            scan_roots: vec![ScanRootEntry {
                auto: true,
                worktrees: true,
                path: PathBuf::from("/home/user/workspace"),
                depth: 4,
                exclude: vec!["target".into()],
                distro: Some("Ubuntu".into()),
            }],
            ..Default::default()
        };
        let dto = AppConfigDto::from(cfg);
        assert_eq!(dto.scan_roots[0].distro, Some("Ubuntu".into()));

        let back = AppConfig::try_from(dto).unwrap();
        assert_eq!(back.scan_roots[0].distro, Some("Ubuntu".into()));
        assert_eq!(back.scan_roots[0].depth, 4);
    }

    #[test]
    fn app_config_dto_roundtrip_gui_preferences() {
        let cfg = AppConfig {
            gui: GuiConfig {
                theme: GuiTheme::Light,
                window_x: Some(-320),
                window_y: Some(25),
                window_width: Some(1200),
                window_height: Some(800),
                window_maximized: Some(true),
                column_widths: BTreeMap::from([("repository".to_string(), 420)]),
                ..Default::default()
            },
            ..Default::default()
        };
        let dto = AppConfigDto::from(cfg);
        assert_eq!(dto.gui.theme, "light");
        assert_eq!(dto.gui.window_x, Some(-320));
        assert_eq!(dto.gui.window_y, Some(25));
        assert_eq!(dto.gui.window_width, Some(1200));
        assert_eq!(dto.gui.window_maximized, Some(true));
        assert_eq!(dto.gui.column_widths.get("repository"), Some(&420));

        let back = AppConfig::try_from(dto).unwrap();
        assert_eq!(back.gui.theme, GuiTheme::Light);
        assert_eq!(back.gui.window_x, Some(-320));
        assert_eq!(back.gui.window_y, Some(25));
        assert_eq!(back.gui.window_height, Some(800));
        assert_eq!(back.gui.window_maximized, Some(true));
        assert_eq!(back.gui.column_widths.get("repository"), Some(&420));
    }

    #[test]
    fn app_config_dto_rejects_unknown_theme() {
        let mut dto = AppConfigDto::from(AppConfig::default());
        dto.gui.theme = "solarized".into();
        let err = AppConfig::try_from(dto).unwrap_err();
        assert!(err.contains("invalid gui.theme"));
    }
}
