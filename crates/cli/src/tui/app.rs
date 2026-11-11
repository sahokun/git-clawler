use std::time::Instant;

use git_clawler_core::{GithubInfo, RepoStatus};

pub struct App {
    pub repos: Vec<RepoEntry>,
    pub selected: usize,
    pub last_scan: Instant,
    pub status_message: Option<String>,
    pub show_help: bool,
    pub confirm_delete: Option<usize>,
    pub filter: Option<String>,
    pub quit: bool,
}

pub struct RepoEntry {
    pub name: String,
    pub path: String,
    pub distro: Option<String>,
    pub status: Option<RepoStatus>,
    pub error: Option<String>,
    /// GitHub Issue/PR counts, populated when `[github]` config is enabled
    /// or `--with-github` flag is passed.
    pub github: Option<GithubInfo>,
}

impl App {
    pub fn new(repos: Vec<RepoEntry>) -> Self {
        Self {
            repos,
            selected: 0,
            last_scan: Instant::now(),
            status_message: None,
            show_help: false,
            confirm_delete: None,
            filter: None,
            quit: false,
        }
    }

    pub fn selected_repo(&self) -> Option<&RepoEntry> {
        self.repos.get(self.selected)
    }

    pub fn visible_indices(&self) -> Vec<usize> {
        self.repos
            .iter()
            .enumerate()
            .filter_map(|(index, repo)| {
                let visible = self
                    .filter
                    .as_deref()
                    .map(|filter| {
                        filter.is_empty()
                            || repo.name.contains(filter)
                            || repo.path.contains(filter)
                    })
                    .unwrap_or(true);
                visible.then_some(index)
            })
            .collect()
    }

    pub fn selected_visible_repo(&self) -> Option<&RepoEntry> {
        self.selected_visible_index()
            .and_then(|index| self.repos.get(index))
    }

    pub fn selected_visible_index(&self) -> Option<usize> {
        let visible = self.visible_indices();
        if visible.contains(&self.selected) {
            Some(self.selected)
        } else {
            visible.first().copied()
        }
    }

    pub fn clamp_selection_to_visible(&mut self) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            self.selected = 0;
        } else if !visible.contains(&self.selected) {
            self.selected = visible[0];
        }
    }
}
