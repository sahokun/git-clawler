use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use crate::tui::app::App;

pub fn render(frame: &mut Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(frame.area());

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(root[0]);

    render_repos(frame, panes[0], app);
    render_details(frame, panes[1], app);
    render_footer(frame, root[1], app);

    if app.show_help {
        render_help(frame, centered_rect(60, 40, frame.area()));
    }
    if app.confirm_delete.is_some() {
        render_delete_confirm(frame, centered_rect(70, 30, frame.area()), app);
    }
}

fn render_repos(frame: &mut Frame, area: Rect, app: &App) {
    let visible_items: Vec<(usize, ListItem)> = app
        .repos
        .iter()
        .enumerate()
        .filter(|(_, repo)| {
            app.filter
                .as_deref()
                .map(|filter| {
                    filter.is_empty() || repo.name.contains(filter) || repo.path.contains(filter)
                })
                .unwrap_or(true)
        })
        .map(|(i, repo)| {
            let indicator = match &repo.status {
                _ if repo.error.is_some() => "!",
                Some(status) if status.conflict_predicted == Some(true) => "⚠",
                Some(status) if status.uncommitted_files > 0 => "●",
                Some(status) if status.ahead > 0 || status.behind > 0 => "↕",
                Some(_) => "✓",
                None => "?",
            };
            let style = if i == app.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            (
                i,
                ListItem::new(format!("{indicator} {}", repo.name)).style(style),
            )
        })
        .collect();

    let title = match &app.filter {
        Some(f) if !f.is_empty() => format!("Repositories [filter: {f}]"),
        Some(_) => "Repositories [filter: (empty)]".to_string(),
        None => "Repositories".to_string(),
    };

    if visible_items.is_empty() && app.filter.is_some() {
        let msg = match &app.filter {
            Some(f) => format!("No matches for \"{}\"", f),
            None => "No repositories configured".to_string(),
        };
        let paragraph = Paragraph::new(msg)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().title(title).borders(Borders::ALL));
        frame.render_widget(paragraph, area);
    } else {
        let items: Vec<ListItem> = visible_items.into_iter().map(|(_, item)| item).collect();
        let list = List::new(items).block(Block::default().title(title).borders(Borders::ALL));
        frame.render_widget(list, area);
    }
}

fn render_details(frame: &mut Frame, area: Rect, app: &App) {
    let text = app
        .selected_visible_repo()
        .map(|repo| match &repo.status {
            Some(status) => {
                let distro = repo.distro.as_deref().unwrap_or("<native>");
                let mut s = format!(
                    "Name: {}\nPath: {}\nDistro: {}\nBranch: {}\nUncommitted: {}\nAhead / Behind: {} / {}\nUnpushed branches: {}\nConflict: {}",
                    repo.name,
                    repo.path,
                    distro,
                    status.current_branch.as_deref().unwrap_or("-"),
                    status.uncommitted_files,
                    status.ahead,
                    status.behind,
                    status.unpushed_branches.len(),
                    match status.conflict_predicted {
                        Some(true) => "possible",
                        Some(false) => "unlikely",
                        None => "unknown",
                    }
                );
                if let Some(gh) = &repo.github {
                    let issues = gh
                        .open_issues
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "\u{2014}".to_string());
                    let prs = gh
                        .open_prs
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "\u{2014}".to_string());
                    s.push_str(&format!("\nIssues: {}  PRs: {}", issues, prs));
                }
                s.push_str("\nRecent commits:");
                if status.recent_commits.is_empty() {
                    s.push_str("\n  No recent commits found.");
                } else {
                    for commit in &status.recent_commits {
                        s.push_str(&format!(
                            "\n  {} · {} · {} · {}",
                            commit.short_hash,
                            commit.summary,
                            commit.author_name,
                            format_commit_time(&commit.committed_at)
                        ));
                    }
                }
                s
            }
            None => format!(
                "Name: {}\nPath: {}\nDistro: {}\nStatus: {}",
                repo.name,
                repo.path,
                repo.distro.as_deref().unwrap_or("<native>"),
                repo.error.as_deref().unwrap_or("not scanned")
            ),
        })
        .unwrap_or_else(|| "No repositories configured".to_string());

    let paragraph =
        Paragraph::new(text).block(Block::default().title("Details").borders(Borders::ALL));
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let footer_text = match &app.filter {
        Some(f) => format!(
            "Filter: \"{}\"  type to filter  Backspace: delete  Esc: clear  Ctrl-C: quit",
            f
        ),
        None => "q/Esc: quit  r: refresh  d: remove  j/k: move  /: filter  ?: help  Ctrl-C: quit"
            .to_string(),
    };
    frame.render_widget(Paragraph::new(footer_text), area);
}

fn render_delete_confirm(frame: &mut Frame, area: Rect, app: &App) {
    frame.render_widget(Clear, area);
    let repo = app
        .confirm_delete
        .and_then(|index| app.repos.get(index))
        .map(|repo| {
            format!(
                "{}\n{}\nDistro: {}",
                repo.name,
                repo.path,
                repo.distro.as_deref().unwrap_or("<native>")
            )
        })
        .unwrap_or_else(|| "selected repository".to_string());
    let text = format!(
        "Remove this repository from config?\n\n{repo}\n\nConfig entry only.\n\nEnter/y: remove  Esc/n: cancel"
    );
    let paragraph = Paragraph::new(text).block(
        Block::default()
            .title("Confirm remove: Enter/y remove, Esc/n cancel")
            .borders(Borders::ALL),
    );
    frame.render_widget(paragraph, area);
}

fn render_help(frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
    let help = Paragraph::new(
        "gclawl TUI\n\
         \n\
         j/k or ↑/↓: select\n\
         r: refresh\n\
         d: remove selected repository from config\n\
         Enter/y or Esc/n: confirm or cancel remove\n\
         /: start filter\n\
         Esc (in filter): clear filter\n\
         ?: open/close help\n\
         q / Esc: close help (when help visible), otherwise quit\n\
         Ctrl-C: always quit",
    )
    .block(Block::default().title("Help").borders(Borders::ALL));
    frame.render_widget(help, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn format_commit_time(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::RepoEntry;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn render_uses_test_backend() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let app = App::new(vec![RepoEntry {
            name: "repo".into(),
            path: "/repo".into(),
            distro: None,
            status: None,
            error: None,
            github: None,
        }]);
        terminal.draw(|frame| render(frame, &app)).expect("draw");
        let buffer = terminal.backend().buffer();
        let content = format!("{buffer:?}");
        assert!(content.contains("Repositories"));
        assert!(content.contains("Details"));
    }

    #[test]
    fn render_shows_github_counts_in_details() {
        use git_clawler_core::{GithubInfo, RepoStatus};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let app = App::new(vec![RepoEntry {
            name: "repo".into(),
            path: "/repo".into(),
            distro: None,
            status: Some(RepoStatus::default()),
            error: None,
            github: Some(GithubInfo {
                open_issues: Some(3),
                open_prs: Some(1),
            }),
        }]);
        terminal.draw(|frame| render(frame, &app)).expect("draw");
        let buffer = terminal.backend().buffer();
        let content = format!("{buffer:?}");
        assert!(content.contains("Issues:"), "should render Issues label");
        assert!(content.contains("PRs:"), "should render PRs label");
    }

    #[test]
    fn render_shows_recent_commits_in_details() {
        use git_clawler_core::{RecentCommit, RepoStatus};

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let app = App::new(vec![RepoEntry {
            name: "repo".into(),
            path: "/repo".into(),
            distro: None,
            status: Some(RepoStatus {
                recent_commits: vec![RecentCommit {
                    short_hash: "abc1234".into(),
                    summary: "Add TUI commits".into(),
                    author_name: "Alice".into(),
                    committed_at: "2026-05-06T12:34:00Z".into(),
                }],
                ..RepoStatus::default()
            }),
            error: None,
            github: None,
        }]);
        terminal.draw(|frame| render(frame, &app)).expect("draw");
        let buffer = terminal.backend().buffer();
        let content = format!("{buffer:?}");
        assert!(content.contains("Recent commits"));
        assert!(content.contains("abc1234"));
        assert!(content.contains("Add TUI commits"));
    }

    #[test]
    fn render_footer_shows_filter_state_when_active() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut app = App::new(vec![RepoEntry {
            name: "myrepo".into(),
            path: "/myrepo".into(),
            distro: None,
            status: None,
            error: None,
            github: None,
        }]);
        app.filter = Some("myf".into());
        terminal.draw(|frame| render(frame, &app)).expect("draw");
        let buffer = terminal.backend().buffer();
        let content = format!("{buffer:?}");
        assert!(
            content.contains("Filter") || content.contains("myf"),
            "footer should show filter state, got:\n{content}"
        );
    }

    #[test]
    fn render_delete_confirmation_popup() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut app = App::new(vec![RepoEntry {
            name: "repo".into(),
            path: "/repo".into(),
            distro: None,
            status: None,
            error: None,
            github: None,
        }]);
        app.confirm_delete = Some(0);
        terminal.draw(|frame| render(frame, &app)).expect("draw");
        let buffer = terminal.backend().buffer();
        let content = format!("{buffer:?}");
        assert!(content.contains("Confirm remove"));
        assert!(content.contains("Distro:"));
        assert!(content.contains("Enter/y"), "got:\n{content}");
        assert!(content.contains("Esc/n"), "got:\n{content}");
    }

    #[test]
    fn render_repos_shows_filter_in_title_when_active() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut app = App::new(vec![RepoEntry {
            name: "myrepo".into(),
            path: "/myrepo".into(),
            distro: None,
            status: None,
            error: None,
            github: None,
        }]);
        app.filter = Some("xyz".into());
        terminal.draw(|frame| render(frame, &app)).expect("draw");
        let buffer = terminal.backend().buffer();
        let content = format!("{buffer:?}");
        // When filter yields no matches, should show some indicator
        assert!(
            content.contains("filter") || content.contains("No matches") || content.contains("xyz"),
            "should show filter context when active, got:\n{content}"
        );
    }
}
