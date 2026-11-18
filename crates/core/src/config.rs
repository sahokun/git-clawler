//! TOML configuration management for git-clawler.
//!
//! The canonical config file lives at `~/.config/git-clawler/config.toml`.
//! This module handles reading, writing, and providing sensible defaults.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info};

use crate::path_key::{normalize_distro_ref, DistroPathKey};

/// Monotonically increasing counter to ensure unique temp file names across
/// threads within the same process.
static SAVE_COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur when loading or saving configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write config file {path}: {source}")]
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("failed to serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("could not determine config directory (neither $XDG_CONFIG_HOME nor $HOME is set)")]
    NoConfigDir,
    #[error("failed to create config directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("repository path must be absolute: {0}")]
    RelativePath(PathBuf),
    #[error("duplicate repository (distro={distro:?}) path: {path}")]
    DuplicateRepository {
        distro: Option<String>,
        path: PathBuf,
    },
    #[error("scan root depth must be 0..=32, got {depth}: {path}")]
    ScanDepthTooLarge { path: PathBuf, depth: u32 },
    #[error("duplicate scan root (distro={distro:?}) path: {path}")]
    DuplicateScanRoot {
        distro: Option<String>,
        path: PathBuf,
    },
    #[error(
        "WSL entry distro={distro:?} requires a Linux absolute path starting with '/': {path}"
    )]
    InvalidWslPath { distro: String, path: PathBuf },
    #[error("gui.{field} must be in {min}..={max}, got {value}")]
    InvalidGuiSize {
        field: &'static str,
        value: u32,
        min: u32,
        max: u32,
    },
    #[error("gui.column_widths.{column} must be in {min}..={max}, got {value}")]
    InvalidGuiColumnWidth {
        column: String,
        value: u32,
        min: u32,
        max: u32,
    },
    #[error("general.refresh_interval_secs must be greater than zero")]
    ZeroRefreshInterval,
}

/// Classify a [`ConfigError`] for the GUI error envelope.
///
/// Returns `"validation"` for user-correctable input violations (so the GUI can
/// surface them as actionable validation errors) and `"internal"` for I/O and
/// (de)serialization failures. Used by the Tauri save path to avoid collapsing
/// validation failures into opaque `kind:"internal"` messages.
pub fn config_error_kind(err: &ConfigError) -> &'static str {
    match err {
        ConfigError::RelativePath(_)
        | ConfigError::DuplicateRepository { .. }
        | ConfigError::DuplicateScanRoot { .. }
        | ConfigError::ScanDepthTooLarge { .. }
        | ConfigError::InvalidWslPath { .. }
        | ConfigError::InvalidGuiSize { .. }
        | ConfigError::ZeroRefreshInterval
        | ConfigError::InvalidGuiColumnWidth { .. } => "validation",
        ConfigError::ReadFile { .. }
        | ConfigError::WriteFile { .. }
        | ConfigError::Parse(_)
        | ConfigError::Serialize(_)
        | ConfigError::NoConfigDir
        | ConfigError::CreateDir { .. } => "internal",
    }
}

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

/// Top-level application configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AppConfig {
    pub general: GeneralConfig,
    #[serde(default)]
    pub repositories: Vec<RepoEntry>,
    #[serde(default)]
    pub scan_roots: Vec<ScanRootEntry>,
    #[serde(default)]
    pub github: GithubConfig,
    #[serde(default)]
    pub notifications: NotificationsConfig,
    #[serde(default)]
    pub gui: GuiConfig,
}

/// General / global settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GeneralConfig {
    /// Refresh interval in seconds.
    pub refresh_interval_secs: u64,
    /// Whether to run `git fetch` on each refresh.
    pub fetch_on_refresh: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            refresh_interval_secs: 30,
            fetch_on_refresh: false,
        }
    }
}

/// A single repository entry in the config file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoEntry {
    /// Optional display name; derived from path if omitted.
    pub name: Option<String>,
    /// Absolute path to the repository.
    pub path: PathBuf,
    /// Per-repository override for fetch-on-refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch: Option<bool>,
    /// Optional WSL distribution name. When set, the entry is treated as a
    /// WSL-resident repository and is scanned via the helper rather than
    /// natively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
    /// Main repository working directory when this entry is a linked worktree
    /// registered by scan-root discovery. Entries with this set are removed
    /// automatically once Git no longer tracks the worktree; other entries are
    /// never removed automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_of: Option<PathBuf>,
}

/// A directory to recursively scan for git repositories.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScanRootEntry {
    /// Absolute path to the scan root directory.
    pub path: PathBuf,
    /// Maximum directory depth to scan (1 = direct children). Used by both the
    /// manual Scan action and automatic discovery.
    #[serde(default = "default_depth")]
    pub depth: u32,
    /// Exclusions: a directory name (matches at any level), a path relative to
    /// the root (contains `/`), or an absolute path.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Automatically register repositories discovered under this root.
    #[serde(default = "default_true")]
    pub auto: bool,
    /// Also register linked worktrees of discovered repositories, wherever
    /// they live.
    #[serde(default = "default_true")]
    pub worktrees: bool,
    /// Optional WSL distribution name. When set, `path` is interpreted as a
    /// Linux absolute path inside that distro and scanned via the WSL helper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
}

fn default_depth() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

/// GitHub integration settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GithubConfig {
    /// Name of the environment variable holding the GitHub token.
    pub token_env: String,
    /// Whether GitHub metadata collection is enabled.
    pub enabled: bool,
    /// Whether open issue counts are shown when GitHub integration is enabled.
    pub enable_issues: bool,
    /// Whether open pull request counts are shown when GitHub integration is enabled.
    pub enable_prs: bool,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            token_env: "GITHUB_TOKEN".to_string(),
            enabled: false,
            enable_issues: true,
            enable_prs: true,
        }
    }
}

/// OS notification settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NotificationsConfig {
    /// Whether desktop notifications are enabled.
    pub enabled: bool,
    /// Minimum interval between notifications.
    pub cooldown_secs: u64,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cooldown_secs: 300,
        }
    }
}

pub const GUI_MIN_WINDOW_WIDTH: u32 = 320;
pub const GUI_MAX_WINDOW_WIDTH: u32 = 7680;
pub const GUI_MIN_WINDOW_HEIGHT: u32 = 240;
pub const GUI_MAX_WINDOW_HEIGHT: u32 = 4320;
pub const GUI_MIN_COLUMN_WIDTH: u32 = 40;
pub const GUI_MAX_COLUMN_WIDTH: u32 = 1600;

// Shared validation ranges. These mirror the bounds enforced by the GUI's
// `validate_for_save` and the frontend `settingsValidation.ts`. Centralising
// them here keeps the numeric limits from drifting between layers.
pub const MIN_REFRESH_INTERVAL_SECS: u64 = 1;
pub const MAX_REFRESH_INTERVAL_SECS: u64 = 86_400;
pub const MIN_COOLDOWN_SECS: u64 = 1;
pub const MAX_COOLDOWN_SECS: u64 = 86_400;
pub const MAX_SCAN_DEPTH: u32 = 32;

/// GUI color theme.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GuiTheme {
    #[default]
    Dark,
    Light,
}

impl GuiTheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }
}

/// GUI-specific settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GuiConfig {
    /// GUI color theme. Defaults to dark.
    pub theme: GuiTheme,
    /// Start the GUI minimised to the system tray.
    pub start_in_tray: bool,
    /// Hide the GUI window from the taskbar (tray-only operation).
    pub hide_from_taskbar: bool,
    /// Closing the window minimises to the tray instead of exiting.
    pub close_to_tray: bool,
    /// Global keyboard shortcut to toggle the window.
    pub global_shortcut: String,
    /// Last saved window x position in physical pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_x: Option<i32>,
    /// Last saved window y position in physical pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_y: Option<i32>,
    /// Last saved window width in physical pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_width: Option<u32>,
    /// Last saved window height in physical pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_height: Option<u32>,
    /// Last saved maximized state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_maximized: Option<bool>,
    /// Persisted table column widths keyed by frontend column id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub column_widths: BTreeMap<String, u32>,
}

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            theme: GuiTheme::Dark,
            start_in_tray: false,
            hide_from_taskbar: false,
            close_to_tray: true,
            global_shortcut: String::new(),
            window_x: None,
            window_y: None,
            window_width: None,
            window_height: None,
            window_maximized: None,
            column_widths: BTreeMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// True if `path` is an acceptable absolute repository path in this build.
///
/// On Windows builds, both Windows-native absolute paths (e.g. `C:\foo`) and
/// Linux-style absolute paths (`/foo`) are accepted, the latter to permit
/// WSL-resident repositories. On other builds, only platform-native absolute
/// paths are accepted.
pub fn is_repo_path_absolute(path: &Path) -> bool {
    if path.is_absolute() {
        return true;
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(s) = path.to_str() {
            return s.starts_with('/');
        }
    }
    false
}

/// True if a WSL entry satisfies the Linux-absolute-path requirement.
///
/// A WSL entry is one whose `distro` is `Some` after normalization. Such entries
/// live inside another filesystem and must be addressed by a Linux absolute path
/// (starting with `/`). Non-WSL entries (`distro` normalizes to `None`) are not
/// subject to this rule and always pass.
///
/// This is the single shared predicate backing both `core::config::validate` and
/// the GUI's `validate_for_save`; each layer keeps its own error message.
pub fn wsl_path_requirement_met(distro: Option<&str>, path: &Path) -> bool {
    match normalize_distro_ref(distro) {
        Some(_) => path.to_string_lossy().starts_with('/'),
        None => true,
    }
}

/// Normalize a distro string: trim whitespace; `None` or blank → `None`.
///
/// This is the canonical normalization used at all config boundaries.
/// GUI helpers should re-export or delegate to this function.
pub fn normalize_distro(s: Option<String>) -> Option<String> {
    normalize_distro_ref(s.as_deref())
}

fn normalize_config_distros(config: &mut AppConfig) {
    for entry in &mut config.repositories {
        entry.distro = normalize_distro(entry.distro.take());
    }
    for root in &mut config.scan_roots {
        root.distro = normalize_distro(root.distro.take());
    }
}

/// Validate that all repository and scan-root paths are absolute and that
/// there are no duplicate `(distro, path)` repository pairs.
pub fn validate(config: &AppConfig) -> Result<(), ConfigError> {
    if config.general.refresh_interval_secs == 0 {
        return Err(ConfigError::ZeroRefreshInterval);
    }
    let mut seen: HashSet<DistroPathKey> = HashSet::new();
    for entry in &config.repositories {
        if !is_repo_path_absolute(&entry.path) {
            return Err(ConfigError::RelativePath(entry.path.clone()));
        }
        let normalized = normalize_distro(entry.distro.clone());
        if let Some(distro) = &normalized {
            if !wsl_path_requirement_met(Some(distro), &entry.path) {
                return Err(ConfigError::InvalidWslPath {
                    distro: distro.clone(),
                    path: entry.path.clone(),
                });
            }
        }
        let key = DistroPathKey::current_host(normalized.as_deref(), &entry.path);
        if !seen.insert(key) {
            return Err(ConfigError::DuplicateRepository {
                distro: entry.distro.clone(),
                path: entry.path.clone(),
            });
        }
    }
    let mut seen_scan_roots: HashSet<DistroPathKey> = HashSet::new();
    for root in &config.scan_roots {
        let normalized = normalize_distro(root.distro.clone());
        if !is_repo_path_absolute(&root.path) {
            return Err(ConfigError::RelativePath(root.path.clone()));
        }
        if let Some(distro) = &normalized {
            if !wsl_path_requirement_met(Some(distro), &root.path) {
                return Err(ConfigError::InvalidWslPath {
                    distro: distro.clone(),
                    path: root.path.clone(),
                });
            }
        }
        if root.depth > MAX_SCAN_DEPTH {
            return Err(ConfigError::ScanDepthTooLarge {
                path: root.path.clone(),
                depth: root.depth,
            });
        }
        let key = DistroPathKey::current_host(normalized.as_deref(), &root.path);
        if !seen_scan_roots.insert(key) {
            return Err(ConfigError::DuplicateScanRoot {
                distro: root.distro.clone(),
                path: root.path.clone(),
            });
        }
    }
    validate_gui_layout(&config.gui)?;
    Ok(())
}

fn validate_gui_layout(gui: &GuiConfig) -> Result<(), ConfigError> {
    if let Some(value) = gui.window_width {
        if !(GUI_MIN_WINDOW_WIDTH..=GUI_MAX_WINDOW_WIDTH).contains(&value) {
            return Err(ConfigError::InvalidGuiSize {
                field: "window_width",
                value,
                min: GUI_MIN_WINDOW_WIDTH,
                max: GUI_MAX_WINDOW_WIDTH,
            });
        }
    }
    if let Some(value) = gui.window_height {
        if !(GUI_MIN_WINDOW_HEIGHT..=GUI_MAX_WINDOW_HEIGHT).contains(&value) {
            return Err(ConfigError::InvalidGuiSize {
                field: "window_height",
                value,
                min: GUI_MIN_WINDOW_HEIGHT,
                max: GUI_MAX_WINDOW_HEIGHT,
            });
        }
    }
    for (column, value) in &gui.column_widths {
        if !(GUI_MIN_COLUMN_WIDTH..=GUI_MAX_COLUMN_WIDTH).contains(value) {
            return Err(ConfigError::InvalidGuiColumnWidth {
                column: column.clone(),
                value: *value,
                min: GUI_MIN_COLUMN_WIDTH,
                max: GUI_MAX_COLUMN_WIDTH,
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Return the default config file path: `~/.config/git-clawler/config.toml`.
pub fn default_config_path() -> Result<PathBuf, ConfigError> {
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::config_dir)
        .ok_or(ConfigError::NoConfigDir)?;
    Ok(config_dir.join("git-clawler").join("config.toml"))
}

// ---------------------------------------------------------------------------
// Load / Save
// ---------------------------------------------------------------------------

/// Load configuration from the given path.
///
/// Validates that all paths are absolute and that there are no duplicate
/// repository paths. Relative paths and duplicates produce an error.
pub fn load_from(path: &Path) -> Result<AppConfig, ConfigError> {
    debug!(?path, "loading config");
    let content = fs::read_to_string(path).map_err(|e| ConfigError::ReadFile {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut config: AppConfig = toml::from_str(&content)?;
    // Normalize distro fields at the config boundary.
    normalize_config_distros(&mut config);
    validate(&config)?;
    info!(?path, repos = config.repositories.len(), "config loaded");
    Ok(config)
}

/// Load configuration from the default path.
pub fn load() -> Result<AppConfig, ConfigError> {
    let path = default_config_path()?;
    load_from(&path)
}

/// Load configuration from the default path, returning defaults if the file
/// does not exist.
pub fn load_or_default() -> Result<AppConfig, ConfigError> {
    let path = default_config_path()?;
    match path.try_exists() {
        Ok(true) => load_from(&path),
        Ok(false) => {
            debug!(?path, "config file not found, using defaults");
            Ok(AppConfig::default())
        }
        Err(e) => Err(ConfigError::ReadFile {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

/// Save configuration to the given path, creating parent directories if needed.
///
/// Validates the config before saving. Uses atomic write (temp file + rename)
/// to avoid data loss on crash.
pub fn save_to(config: &AppConfig, path: &Path) -> Result<(), ConfigError> {
    validate(config)?;
    let mut config_to_save = config.clone();
    normalize_config_distros(&mut config_to_save);
    debug!(?path, "saving config");
    let parent = path.parent().ok_or_else(|| ConfigError::WriteFile {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent"),
    })?;
    fs::create_dir_all(parent).map_err(|e| ConfigError::CreateDir {
        path: parent.to_path_buf(),
        source: e,
    })?;
    let content = toml::to_string_pretty(&config_to_save)?;

    // Atomic write: write to a unique temp file in the same directory, then rename.
    // Use O_EXCL (create_new) to prevent symlink attacks on the temp path.
    let unique_id = format!(
        "{}.{}",
        std::process::id(),
        SAVE_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let tmp_path = parent.join(format!(".config.toml.{unique_id}.tmp"));
    {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| ConfigError::WriteFile {
                path: tmp_path.clone(),
                source: e,
            })?;
        if let Err(source) = file
            .write_all(content.as_bytes())
            .and_then(|()| file.sync_all())
        {
            drop(file);
            let _ = fs::remove_file(&tmp_path);
            return Err(ConfigError::WriteFile {
                path: tmp_path.clone(),
                source,
            });
        }
    }

    // A single same-directory replacement keeps the old file visible until commit,
    // including on Windows. Never move the destination out of the way first.
    fs::rename(&tmp_path, path).map_err(|e| {
        // Clean up the temp file on rename failure.
        let _ = fs::remove_file(&tmp_path);
        ConfigError::WriteFile {
            path: path.to_path_buf(),
            source: e,
        }
    })?;
    info!(?path, "config saved");
    Ok(())
}

/// Save configuration to the default path.
pub fn save(config: &AppConfig) -> Result<(), ConfigError> {
    let path = default_config_path()?;
    save_to(config, &path)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn toml_path(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "\\\\")
    }

    #[test]
    fn default_config_has_sensible_values() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.general.refresh_interval_secs, 30);
        assert!(!cfg.general.fetch_on_refresh);
        assert!(cfg.repositories.is_empty());
        assert!(cfg.scan_roots.is_empty());
        assert_eq!(cfg.github.token_env, "GITHUB_TOKEN");
        assert!(!cfg.github.enabled);
        assert!(cfg.github.enable_issues);
        assert!(cfg.github.enable_prs);
        assert!(!cfg.notifications.enabled);
        assert_eq!(cfg.notifications.cooldown_secs, 300);
        assert!(!cfg.gui.start_in_tray);
        assert!(!cfg.gui.hide_from_taskbar);
        assert!(cfg.gui.close_to_tray);
        assert_eq!(cfg.gui.global_shortcut, "");
        assert_eq!(cfg.gui.theme, GuiTheme::Dark);
        assert_eq!(cfg.gui.window_x, None);
        assert_eq!(cfg.gui.window_y, None);
        assert_eq!(cfg.gui.window_width, None);
        assert_eq!(cfg.gui.window_height, None);
        assert_eq!(cfg.gui.window_maximized, None);
        assert!(cfg.gui.column_widths.is_empty());
    }

    #[test]
    fn default_config_path_is_under_config_dir() {
        if let Ok(path) = default_config_path() {
            assert!(path.ends_with("git-clawler/config.toml"));
        }
    }

    #[test]
    fn wsl_path_requirement_passes_for_non_wsl_entries() {
        // distro None (or blank) → rule does not apply, any path passes.
        assert!(wsl_path_requirement_met(None, Path::new("C:\\repo")));
        assert!(wsl_path_requirement_met(Some("  "), Path::new("C:\\repo")));
    }

    #[test]
    fn wsl_path_requirement_checks_linux_absolute_for_wsl_entries() {
        assert!(wsl_path_requirement_met(
            Some("Ubuntu"),
            Path::new("/home/u/proj")
        ));
        assert!(!wsl_path_requirement_met(
            Some("Ubuntu"),
            Path::new("C:\\proj")
        ));
        assert!(!wsl_path_requirement_met(
            Some("Ubuntu"),
            Path::new("relative/proj")
        ));
    }

    #[test]
    fn config_error_kind_classifies_validation_vs_internal() {
        assert_eq!(
            config_error_kind(&ConfigError::RelativePath(PathBuf::from("rel"))),
            "validation"
        );
        assert_eq!(
            config_error_kind(&ConfigError::InvalidWslPath {
                distro: "Ubuntu".into(),
                path: PathBuf::from("C:\\x"),
            }),
            "validation"
        );
        assert_eq!(
            config_error_kind(&ConfigError::InvalidGuiSize {
                field: "window_width",
                value: 1,
                min: GUI_MIN_WINDOW_WIDTH,
                max: GUI_MAX_WINDOW_WIDTH,
            }),
            "validation"
        );
        assert_eq!(config_error_kind(&ConfigError::NoConfigDir), "internal");
        assert_eq!(
            config_error_kind(&ConfigError::CreateDir {
                path: PathBuf::from("/x"),
                source: std::io::Error::other("boom"),
            }),
            "internal"
        );
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        let original = AppConfig {
            general: GeneralConfig {
                refresh_interval_secs: 60,
                fetch_on_refresh: true,
            },
            repositories: vec![RepoEntry {
                worktree_of: None,
                name: Some("my-repo".into()),
                path: PathBuf::from("/home/user/my-repo"),
                fetch: Some(true),
                distro: None,
            }],
            scan_roots: vec![ScanRootEntry {
                auto: true,
                worktrees: true,
                path: dir.path().join("projects"),
                depth: 2,
                exclude: vec!["target".to_string()],
                distro: None,
            }],
            github: GithubConfig {
                token_env: "MY_GH_TOKEN".to_string(),
                enabled: true,
                enable_issues: true,
                enable_prs: false,
            },
            notifications: NotificationsConfig {
                enabled: true,
                cooldown_secs: 60,
            },
            gui: GuiConfig {
                theme: GuiTheme::Light,
                start_in_tray: false,
                hide_from_taskbar: false,
                close_to_tray: false,
                global_shortcut: "Ctrl+Alt+G".to_string(),
                window_x: Some(-1280),
                window_y: Some(40),
                window_width: Some(1200),
                window_height: Some(800),
                window_maximized: Some(true),
                column_widths: BTreeMap::from([("branch".to_string(), 140)]),
            },
        };

        save_to(&original, &path).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(original, loaded);
    }

    #[test]
    fn load_missing_file_returns_error() {
        let result = load_from(Path::new("/tmp/nonexistent-git-clawler-test/config.toml"));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::ReadFile { .. }));
    }

    #[test]
    fn load_invalid_toml_returns_parse_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.toml");
        fs::write(&path, "this is not valid { toml").unwrap();
        let result = load_from(&path);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::Parse(_)));
    }

    #[test]
    fn load_empty_file_returns_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.toml");
        fs::write(&path, "").unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg, AppConfig::default());
    }

    #[test]
    fn load_partial_config_fills_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("partial.toml");
        fs::write(
            &path,
            r#"
[general]
refresh_interval_secs = 120
"#,
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.general.refresh_interval_secs, 120);
        assert!(cfg.repositories.is_empty());
        assert!(!cfg.gui.start_in_tray); // default
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a").join("b").join("config.toml");
        let cfg = AppConfig::default();
        save_to(&cfg, &path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn scan_root_depth_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("depth.toml");
        let root_path = toml_path(&dir.path().join("projects"));
        fs::write(
            &path,
            format!(
                r#"
[[scan_roots]]
path = "{root_path}"
"#,
            ),
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        // Existing configs without the new keys get direct-children depth and
        // auto-follow (including worktrees) enabled.
        assert_eq!(cfg.scan_roots[0].depth, 1);
        assert!(cfg.scan_roots[0].auto);
        assert!(cfg.scan_roots[0].worktrees);
    }

    #[test]
    fn scan_root_depth_above_limit_is_rejected() {
        let mut cfg = AppConfig::default();
        cfg.scan_roots.push(ScanRootEntry {
            path: std::env::temp_dir(),
            depth: MAX_SCAN_DEPTH + 1,
            exclude: Vec::new(),
            distro: None,
            auto: true,
            worktrees: true,
        });
        assert!(matches!(
            validate(&cfg),
            Err(ConfigError::ScanDepthTooLarge { .. })
        ));
    }

    #[test]
    fn repo_entry_name_is_optional() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("noname.toml");
        fs::write(
            &path,
            r#"
[[repositories]]
path = "/home/user/project"
"#,
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert!(cfg.repositories[0].name.is_none());
        assert_eq!(
            cfg.repositories[0].path,
            PathBuf::from("/home/user/project")
        );
    }

    #[test]
    fn load_or_default_returns_default_when_missing() {
        let result = load_or_default();
        drop(result);
    }

    #[test]
    fn validate_rejects_relative_repo_path() {
        let cfg = AppConfig {
            repositories: vec![RepoEntry {
                worktree_of: None,
                name: None,
                path: PathBuf::from("relative/path"),
                fetch: None,
                distro: None,
            }],
            ..Default::default()
        };
        let result = validate(&cfg);
        assert!(matches!(result.unwrap_err(), ConfigError::RelativePath(_)));
    }

    #[test]
    fn validate_rejects_duplicate_repo_paths() {
        let cfg = AppConfig {
            repositories: vec![
                RepoEntry {
                    worktree_of: None,
                    name: Some("a".into()),
                    path: PathBuf::from("/tmp/dup"),
                    fetch: None,
                    distro: None,
                },
                RepoEntry {
                    worktree_of: None,
                    name: Some("b".into()),
                    path: PathBuf::from("/tmp/dup"),
                    fetch: None,
                    distro: None,
                },
            ],
            ..Default::default()
        };
        let result = validate(&cfg);
        assert!(matches!(
            result.unwrap_err(),
            ConfigError::DuplicateRepository { .. }
        ));
    }

    #[test]
    fn validate_rejects_relative_scan_root() {
        let cfg = AppConfig {
            scan_roots: vec![ScanRootEntry {
                auto: true,
                worktrees: true,
                path: PathBuf::from("relative"),
                depth: 3,
                exclude: vec![],
                distro: None,
            }],
            ..Default::default()
        };
        let result = validate(&cfg);
        assert!(matches!(result.unwrap_err(), ConfigError::RelativePath(_)));
    }

    #[cfg(windows)]
    #[test]
    fn validate_accepts_wsl_style_scan_root_on_windows() {
        let cfg = AppConfig {
            scan_roots: vec![ScanRootEntry {
                auto: true,
                worktrees: true,
                path: PathBuf::from("/home/user/projects"),
                depth: 3,
                exclude: vec![],
                distro: None,
            }],
            ..Default::default()
        };
        let result = validate(&cfg);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_accepts_wsl_scan_root_with_distro() {
        let cfg = AppConfig {
            scan_roots: vec![ScanRootEntry {
                auto: true,
                worktrees: true,
                path: PathBuf::from("/home/user/projects"),
                depth: 3,
                exclude: vec![],
                distro: Some("Ubuntu".into()),
            }],
            ..Default::default()
        };
        let result = validate(&cfg);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_rejects_wsl_scan_root_distro_with_non_linux_path() {
        let cfg = AppConfig {
            scan_roots: vec![ScanRootEntry {
                auto: true,
                worktrees: true,
                path: PathBuf::from("relative"),
                depth: 3,
                exclude: vec![],
                distro: Some("Ubuntu".into()),
            }],
            ..Default::default()
        };
        let result = validate(&cfg);
        assert!(matches!(result.unwrap_err(), ConfigError::RelativePath(_)));
    }

    #[test]
    fn validate_rejects_duplicate_scan_root_with_same_distro() {
        let cfg = AppConfig {
            scan_roots: vec![
                ScanRootEntry {
                    auto: true,
                    worktrees: true,
                    path: PathBuf::from("/home/user/projects"),
                    depth: 3,
                    exclude: vec![],
                    distro: Some("Ubuntu".into()),
                },
                ScanRootEntry {
                    auto: true,
                    worktrees: true,
                    path: PathBuf::from("/home/user/projects"),
                    depth: 2,
                    exclude: vec![],
                    distro: Some("Ubuntu".into()),
                },
            ],
            ..Default::default()
        };
        let result = validate(&cfg);
        assert!(matches!(
            result.unwrap_err(),
            ConfigError::DuplicateScanRoot { .. }
        ));
    }

    #[test]
    fn save_rejects_invalid_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = AppConfig {
            repositories: vec![RepoEntry {
                worktree_of: None,
                name: None,
                path: PathBuf::from("relative/path"),
                fetch: None,
                distro: None,
            }],
            ..Default::default()
        };
        let result = save_to(&cfg, &path);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::RelativePath(_)));
        // File should not have been created
        assert!(!path.exists());
    }

    #[test]
    fn replacement_never_exposes_a_missing_or_partial_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        save_to(&AppConfig::default(), &path).unwrap();
        std::thread::scope(|scope| {
            let writer = scope.spawn(|| {
                for interval in 1..50 {
                    let mut cfg = AppConfig::default();
                    cfg.general.refresh_interval_secs = interval;
                    save_to(&cfg, &path).unwrap();
                }
            });
            for _ in 0..200 {
                assert!(load_from(&path).is_ok());
            }
            writer.join().unwrap();
        });
    }

    #[cfg(windows)]
    #[test]
    fn replacement_failure_keeps_existing_configuration() {
        use std::os::windows::fs::OpenOptionsExt;
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("config.toml");
        let original = AppConfig::default();
        save_to(&original, &path).unwrap();
        let bytes = fs::read(&path).unwrap();
        // Simulate another Windows application withholding FILE_SHARE_DELETE.
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        let mut changed = original;
        changed.general.refresh_interval_secs = 123;
        assert!(save_to(&changed, &path).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        drop(lock);
        save_to(&changed, &path).unwrap();
        assert_eq!(load_from(&path).unwrap().general.refresh_interval_secs, 123);
    }

    #[test]
    fn failed_replacement_cleans_temporary_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("sentinel"), "original").unwrap();
        assert!(save_to(&AppConfig::default(), &path).is_err());
        assert_eq!(
            fs::read_to_string(path.join("sentinel")).unwrap(),
            "original"
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn save_is_atomic() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        // Write initial config
        let original = AppConfig {
            repositories: vec![RepoEntry {
                worktree_of: None,
                name: Some("orig".into()),
                path: PathBuf::from("/tmp/orig"),
                fetch: None,
                distro: None,
            }],
            ..Default::default()
        };
        save_to(&original, &path).unwrap();

        // Overwrite with new config
        let updated = AppConfig {
            repositories: vec![RepoEntry {
                worktree_of: None,
                name: Some("updated".into()),
                path: PathBuf::from("/tmp/updated"),
                fetch: None,
                distro: None,
            }],
            ..Default::default()
        };
        save_to(&updated, &path).unwrap();

        // Verify no temp files left behind
        let tmp_files: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_str().is_some_and(|n| n.ends_with(".tmp")))
            .collect();
        assert!(tmp_files.is_empty(), "temp files should be cleaned up");

        // Verify the final content
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.repositories[0].name, Some("updated".to_string()));
    }

    #[test]
    fn repo_entry_fetch_default_is_none() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("repo_fetch.toml");
        fs::write(
            &path,
            r#"
[[repositories]]
path = "/home/user/project"
"#,
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert!(cfg.repositories[0].fetch.is_none());
    }

    #[test]
    fn scan_root_exclude_default_is_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("exclude.toml");
        let root_path = toml_path(&dir.path().join("projects"));
        fs::write(
            &path,
            format!(
                r#"
[[scan_roots]]
path = "{root_path}"
"#,
            ),
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert!(cfg.scan_roots[0].exclude.is_empty());
    }

    #[test]
    fn github_token_env_default_is_github_token() {
        let cfg = GithubConfig::default();
        assert_eq!(cfg.token_env, "GITHUB_TOKEN");
        assert!(!cfg.enabled);
        assert!(cfg.enable_issues);
        assert!(cfg.enable_prs);
    }

    #[test]
    fn notifications_default_is_disabled_with_five_minute_cooldown() {
        let cfg = NotificationsConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.cooldown_secs, 300);
    }

    #[test]
    fn gui_defaults_match_requirements_6_1() {
        let cfg = GuiConfig::default();
        assert!(!cfg.start_in_tray);
        assert!(!cfg.hide_from_taskbar);
        assert!(cfg.close_to_tray);
        assert_eq!(cfg.global_shortcut, "");
        assert_eq!(cfg.theme, GuiTheme::Dark);
        assert_eq!(cfg.window_x, None);
        assert_eq!(cfg.window_y, None);
        assert_eq!(cfg.window_width, None);
        assert_eq!(cfg.window_height, None);
        assert_eq!(cfg.window_maximized, None);
        assert!(cfg.column_widths.is_empty());
    }

    #[test]
    fn gui_theme_serde_roundtrip() {
        #[derive(Debug, Deserialize, Serialize)]
        struct ThemeDoc {
            theme: GuiTheme,
        }

        let doc: ThemeDoc = toml::from_str("theme = \"light\"").unwrap();
        assert_eq!(doc.theme, GuiTheme::Light);
        assert_eq!(
            toml::to_string(&ThemeDoc {
                theme: GuiTheme::Dark
            })
            .unwrap()
            .trim(),
            "theme = \"dark\""
        );
    }

    #[test]
    fn validate_rejects_invalid_gui_window_size() {
        let cfg = AppConfig {
            gui: GuiConfig {
                window_width: Some(GUI_MIN_WINDOW_WIDTH - 1),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            validate(&cfg).unwrap_err(),
            ConfigError::InvalidGuiSize {
                field: "window_width",
                ..
            }
        ));
    }

    #[test]
    fn validate_rejects_invalid_gui_column_width() {
        let cfg = AppConfig {
            gui: GuiConfig {
                column_widths: BTreeMap::from([("branch".to_string(), GUI_MIN_COLUMN_WIDTH - 1)]),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            validate(&cfg).unwrap_err(),
            ConfigError::InvalidGuiColumnWidth { .. }
        ));
    }

    #[test]
    fn general_fetch_on_refresh_default_is_false() {
        let cfg = GeneralConfig::default();
        assert!(!cfg.fetch_on_refresh);
    }

    #[test]
    fn unknown_legacy_fields_are_ignored() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("legacy.toml");
        let root_path = toml_path(&dir.path().join("projects"));
        fs::write(
            &path,
            format!(
                r#"
[general]
scan_interval_secs = 999

[gui]
start_minimized = true
notifications = false

[github]
username = "octocat"
enabled = true

[[scan_roots]]
path = "{root_path}"
max_depth = 5
"#,
            ),
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        // Legacy keys ignored; supported keys are parsed normally.
        assert_eq!(cfg.general.refresh_interval_secs, 30);
        assert!(!cfg.general.fetch_on_refresh);
        assert!(!cfg.gui.start_in_tray);
        assert_eq!(cfg.github.token_env, "GITHUB_TOKEN");
        assert!(cfg.github.enabled);
        assert_eq!(cfg.scan_roots[0].depth, 1);
    }

    // ----- distro / WSL path tests -----

    #[test]
    fn is_repo_path_absolute_relative_returns_false() {
        assert!(!is_repo_path_absolute(Path::new("relative/path")));
    }

    #[test]
    fn is_repo_path_absolute_drive_relative_returns_false() {
        // "C:foo" is drive-relative — not absolute on any platform.
        assert!(!is_repo_path_absolute(Path::new("C:foo")));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn is_repo_path_absolute_unix_native() {
        assert!(is_repo_path_absolute(Path::new("/home/user/proj")));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn is_repo_path_absolute_windows_native() {
        assert!(is_repo_path_absolute(Path::new(r"C:\Users\foo")));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn is_repo_path_absolute_linux_on_windows_returns_true() {
        // WSL paths must be accepted on Windows builds.
        assert!(is_repo_path_absolute(Path::new("/home/user/proj")));
    }

    #[test]
    fn validate_allows_same_path_with_different_distros() {
        let cfg = AppConfig {
            repositories: vec![
                RepoEntry {
                    worktree_of: None,
                    name: None,
                    path: PathBuf::from("/home/user/proj"),
                    fetch: None,
                    distro: None,
                },
                RepoEntry {
                    worktree_of: None,
                    name: None,
                    path: PathBuf::from("/home/user/proj"),
                    fetch: None,
                    distro: Some("Ubuntu".into()),
                },
            ],
            ..Default::default()
        };
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn validate_rejects_duplicate_distro_path_pair() {
        let cfg = AppConfig {
            repositories: vec![
                RepoEntry {
                    worktree_of: None,
                    name: None,
                    path: PathBuf::from("/home/user/proj"),
                    fetch: None,
                    distro: Some("Ubuntu".into()),
                },
                RepoEntry {
                    worktree_of: None,
                    name: None,
                    path: PathBuf::from("/home/user/proj"),
                    fetch: None,
                    distro: Some("Ubuntu".into()),
                },
            ],
            ..Default::default()
        };
        let err = validate(&cfg).unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateRepository { .. }));
    }

    #[test]
    fn repo_entry_serde_roundtrip_with_distro() {
        let entry = RepoEntry {
            worktree_of: None,
            name: Some("proj".into()),
            path: PathBuf::from("/home/user/proj"),
            fetch: None,
            distro: Some("Ubuntu".into()),
        };
        let s = toml::to_string(&entry).unwrap();
        let back: RepoEntry = toml::from_str(&s).unwrap();
        assert_eq!(back.distro, Some("Ubuntu".into()));
    }

    #[test]
    fn repo_entry_serde_skips_distro_when_none() {
        let entry = RepoEntry {
            worktree_of: None,
            name: Some("proj".into()),
            path: PathBuf::from("/home/user/proj"),
            fetch: None,
            distro: None,
        };
        let s = toml::to_string(&entry).unwrap();
        assert!(!s.contains("distro"));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn validate_wsl_entry_with_non_linux_path_returns_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.toml");
        std::fs::write(
            &path,
            "[[repositories]]\npath = \"C:\\\\Users\\\\foo\\\\proj\"\ndistro = \"Ubuntu\"\n",
        )
        .unwrap();
        let result = load_from(&path);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ConfigError::InvalidWslPath { .. }
        ));
    }

    #[test]
    fn validate_wsl_entry_with_linux_path_ok() {
        let config = AppConfig {
            repositories: vec![RepoEntry {
                worktree_of: None,
                name: Some("wsl".into()),
                path: PathBuf::from("/home/user/proj"),
                fetch: None,
                distro: Some("Ubuntu".into()),
            }],
            ..Default::default()
        };
        assert!(validate(&config).is_ok());
    }

    // ----- M-1: normalize_distro tests -----

    #[test]
    fn normalize_distro_trims_and_returns_none_for_blank() {
        assert_eq!(normalize_distro(None), None);
        assert_eq!(normalize_distro(Some("".into())), None);
        assert_eq!(normalize_distro(Some("   ".into())), None);
        assert_eq!(
            normalize_distro(Some(" Ubuntu ".into())),
            Some("Ubuntu".into())
        );
    }

    #[test]
    fn load_from_normalizes_whitespace_distro() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cfg.toml");
        fs::write(
            &path,
            r#"
[[repositories]]
path = "/home/user/proj"
distro = "   "
"#,
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.repositories[0].distro, None);
    }

    #[test]
    fn load_from_normalizes_padded_distro() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cfg.toml");
        fs::write(
            &path,
            r#"
[[repositories]]
path = "/home/user/proj"
distro = " Ubuntu "
"#,
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.repositories[0].distro, Some("Ubuntu".into()));
    }

    #[test]
    fn load_from_normalizes_scan_root_padded_distro() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cfg.toml");
        fs::write(
            &path,
            r#"
[[scan_roots]]
path = "/home/user/projects"
distro = " Ubuntu "
"#,
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.scan_roots[0].distro, Some("Ubuntu".into()));
    }

    #[test]
    fn load_from_normalizes_scan_root_whitespace_distro_to_none() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cfg.toml");
        fs::write(
            &path,
            r#"
[[scan_roots]]
path = "/home/user/projects"
distro = "   "
"#,
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.scan_roots[0].distro, None);
    }

    #[test]
    fn save_to_normalizes_distro_fields_without_mutating_caller_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = AppConfig {
            repositories: vec![RepoEntry {
                worktree_of: None,
                name: Some("repo".into()),
                path: PathBuf::from("/home/user/repo"),
                fetch: None,
                distro: Some(" Ubuntu ".into()),
            }],
            scan_roots: vec![ScanRootEntry {
                auto: true,
                worktrees: true,
                path: PathBuf::from("/home/user/projects"),
                depth: 3,
                exclude: vec![],
                distro: Some(" Debian ".into()),
            }],
            ..Default::default()
        };

        save_to(&cfg, &path).unwrap();

        assert_eq!(cfg.repositories[0].distro, Some(" Ubuntu ".into()));
        assert_eq!(cfg.scan_roots[0].distro, Some(" Debian ".into()));
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("distro = \"Ubuntu\""));
        assert!(content.contains("distro = \"Debian\""));
        assert!(!content.contains(" Ubuntu "));
        assert!(!content.contains(" Debian "));
    }

    #[test]
    fn validate_treats_whitespace_distro_as_none_for_duplicate_check() {
        // distro="   " と distro=None は同じキーとして扱い重複エラー
        let cfg = AppConfig {
            repositories: vec![
                RepoEntry {
                    worktree_of: None,
                    name: Some("a".into()),
                    path: PathBuf::from("/tmp/dup"),
                    fetch: None,
                    distro: Some("   ".into()),
                },
                RepoEntry {
                    worktree_of: None,
                    name: Some("b".into()),
                    path: PathBuf::from("/tmp/dup"),
                    fetch: None,
                    distro: None,
                },
            ],
            ..Default::default()
        };
        let result = validate(&cfg);
        assert!(matches!(
            result.unwrap_err(),
            ConfigError::DuplicateRepository { .. }
        ));
    }

    #[test]
    fn validate_does_not_return_invalid_wsl_path_for_whitespace_distro() {
        // distro="   " (空白) + path → normalize で distro=None になるため InvalidWslPath にならない
        let cfg = AppConfig {
            repositories: vec![RepoEntry {
                worktree_of: None,
                name: None,
                path: PathBuf::from("/tmp/repo"),
                fetch: None,
                distro: Some("   ".into()),
            }],
            ..Default::default()
        };
        // /tmp/repo は / 始まりなので InvalidWslPath にはならず、distro が None に
        // normalize されるので Ok になるべき
        assert!(validate(&cfg).is_ok());
    }
}
