pub mod app;
pub mod update;
pub mod view;

pub use app::{App, RepoEntry};

use std::io;
use std::time::Duration;

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use git_clawler_core::{config::GithubConfig, AppConfig};
use ratatui::{backend::CrosstermBackend, Terminal};

use self::update::Effect;

struct TuiGuard;

impl TuiGuard {
    fn enter() -> anyhow::Result<Self> {
        // Enable raw mode FIRST so the guard's Drop will clean it up if the
        // subsequent EnterAlternateScreen call fails.
        enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(guard)
    }
}

impl Drop for TuiGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Run the TUI.
///
/// `with_github` forces GitHub enrichment for this session regardless of the
/// `[github].enabled` config value.  Setting the flag is equivalent to
/// temporarily enabling `[github]` in the config without persisting the change.
pub fn run(config: AppConfig, with_github: bool) -> anyhow::Result<()> {
    let mut config = config;
    let config_path = git_clawler_core::config::default_config_path()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut app = App::new(entries_from_config(&config));
    refresh(&runtime, &config, &mut app, with_github);

    let _guard = TuiGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|frame| view::render(frame, &app))?;
        if app.quit {
            break;
        }
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match update::handle_key(&mut app, key) {
                    Effect::Refresh => refresh(&runtime, &config, &mut app, with_github),
                    Effect::DeleteSelected => {
                        if let Err(e) = remove_selected_repo(&mut config, &config_path, &mut app) {
                            app.confirm_delete = None;
                            app.status_message = Some(format!("Remove failed: {e}"));
                        }
                    }
                    Effect::Quit => break,
                    Effect::None => {}
                }
            }
        }
    }
    Ok(())
}

fn remove_selected_repo(
    config: &mut AppConfig,
    config_path: &std::path::Path,
    app: &mut App,
) -> anyhow::Result<()> {
    let Some(index) = app.confirm_delete.take() else {
        return Ok(());
    };
    let Some(repo) = app.repos.get(index) else {
        app.clamp_selection_to_visible();
        return Ok(());
    };
    let path = std::path::PathBuf::from(&repo.path);
    let distro = repo.distro.clone();
    let mut latest_config = match config_path.try_exists() {
        Ok(true) => git_clawler_core::config::load_from(config_path)?,
        Ok(false) => AppConfig::default(),
        Err(e) => return Err(e.into()),
    };
    let mut registry = git_clawler_core::Registry::from_config(&latest_config);
    let removed = registry.remove_by_distro_path(distro.as_deref(), &path)?;
    latest_config.repositories = registry.to_config_entries();
    git_clawler_core::config::save_to(&latest_config, config_path)?;
    *config = latest_config;
    app.repos = entries_from_config(config);
    app.clamp_selection_to_visible();
    app.status_message = Some(format!("Removed from config: {}", removed.path().display()));
    Ok(())
}

fn entries_from_config(config: &AppConfig) -> Vec<RepoEntry> {
    git_clawler_core::Registry::from_config(config)
        .list()
        .iter()
        .map(|repo| RepoEntry {
            name: repo.name.clone(),
            path: repo.path().to_string_lossy().into_owned(),
            distro: repo.distro.clone(),
            status: None,
            error: None,
            github: None,
        })
        .collect()
}

fn refresh(
    runtime: &tokio::runtime::Runtime,
    config: &AppConfig,
    app: &mut App,
    with_github: bool,
) {
    // If `--with-github` is specified, force-enable enrichment for this run
    // without persisting the config change.
    let effective_github = if with_github && !config.github.enabled {
        GithubConfig {
            enabled: true,
            ..config.github.clone()
        }
    } else {
        config.github.clone()
    };

    let rows = runtime.block_on(async {
        let targets = git_clawler_core::build_targets(config);
        if effective_github.enabled {
            git_clawler_core::scan_all_with_github(targets, &effective_github).await
        } else {
            git_clawler_core::scan_all(targets).await
        }
    });

    for row in rows {
        if let Some(entry) = app.repos.iter_mut().find(|entry| {
            let path = row.target.path.to_string_lossy();
            entry.path == path.as_ref()
        }) {
            match row.outcome {
                git_clawler_core::ScanOutcome::Ok(status) => {
                    entry.github = status.github.clone();
                    entry.status = Some(status);
                    entry.error = None;
                }
                git_clawler_core::ScanOutcome::Err(error) => {
                    entry.status = None;
                    entry.github = None;
                    entry.error = Some(error);
                }
            }
        }
    }
    app.last_scan = std::time::Instant::now();
    app.status_message = Some("Refreshed".to_string());
}
