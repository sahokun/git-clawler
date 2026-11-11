use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::tui::app::App;

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    Refresh,
    DeleteSelected,
    Quit,
    None,
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Effect {
    // Ctrl-C always quits regardless of any modal state.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.quit = true;
        return Effect::Quit;
    }

    if app.confirm_delete.is_some() {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                return Effect::DeleteSelected;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.confirm_delete = None;
                return Effect::None;
            }
            _ => return Effect::None,
        }
    }

    // When help is visible, q / Esc / ? close the help overlay; nothing else quits.
    if app.show_help {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('?') => {
                app.show_help = false;
                return Effect::None;
            }
            _ => return Effect::None,
        }
    }

    // When filter is active, most keys feed the filter text.
    if app.filter.is_some() {
        match key.code {
            KeyCode::Esc => {
                app.filter = None;
                app.clamp_selection_to_visible();
                return Effect::None;
            }
            KeyCode::Enter => return Effect::None,
            KeyCode::Backspace => {
                if let Some(filter) = &mut app.filter {
                    filter.pop();
                }
                app.clamp_selection_to_visible();
                return Effect::None;
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                if let Some(filter) = &mut app.filter {
                    filter.push(c);
                }
                app.clamp_selection_to_visible();
                return Effect::None;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            app.quit = true;
            Effect::Quit
        }
        KeyCode::Char('r') => Effect::Refresh,
        KeyCode::Char('d') => {
            if let Some(index) = app.selected_visible_index() {
                app.confirm_delete = Some(index);
            }
            Effect::None
        }
        KeyCode::Char('/') => {
            app.filter = Some(String::new());
            app.clamp_selection_to_visible();
            Effect::None
        }
        KeyCode::Char('j') | KeyCode::Down => {
            move_selection(app, 1);
            Effect::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            move_selection(app, -1);
            Effect::None
        }
        KeyCode::Char('?') => {
            app.show_help = !app.show_help;
            Effect::None
        }
        _ => Effect::None,
    }
}

fn move_selection(app: &mut App, delta: isize) {
    let visible = app.visible_indices();
    if visible.is_empty() {
        app.selected = 0;
        return;
    }
    let current_pos = visible
        .iter()
        .position(|index| *index == app.selected)
        .unwrap_or(0);
    let next_pos = (current_pos as isize + delta).clamp(0, visible.len() as isize - 1) as usize;
    app.selected = visible[next_pos];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::RepoEntry;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn app_with_repos(count: usize) -> App {
        App::new(
            (0..count)
                .map(|i| RepoEntry {
                    name: format!("repo-{i}"),
                    path: format!("/repo/{i}"),
                    distro: None,
                    status: None,
                    error: None,
                    github: None,
                })
                .collect(),
        )
    }

    #[test]
    fn q_quits_when_help_not_visible() {
        let mut app = app_with_repos(1);
        assert_eq!(handle_key(&mut app, key(KeyCode::Char('q'))), Effect::Quit);
        assert!(app.quit);
    }

    #[test]
    fn esc_quits_when_help_not_visible() {
        let mut app = app_with_repos(1);
        assert_eq!(handle_key(&mut app, key(KeyCode::Esc)), Effect::Quit);
        assert!(app.quit);
    }

    #[test]
    fn ctrl_c_always_quits() {
        let mut app = app_with_repos(1);
        let event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut app, event), Effect::Quit);
        assert!(app.quit);
    }

    #[test]
    fn ctrl_c_quits_even_when_help_visible() {
        let mut app = app_with_repos(1);
        app.show_help = true;
        let event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut app, event), Effect::Quit);
        assert!(app.quit);
    }

    #[test]
    fn q_closes_help_when_help_visible() {
        let mut app = app_with_repos(1);
        app.show_help = true;
        let effect = handle_key(&mut app, key(KeyCode::Char('q')));
        assert_eq!(effect, Effect::None, "q should close help, not quit");
        assert!(!app.show_help, "help should be closed");
        assert!(!app.quit, "app should not quit");
    }

    #[test]
    fn esc_closes_help_when_help_visible() {
        let mut app = app_with_repos(1);
        app.show_help = true;
        let effect = handle_key(&mut app, key(KeyCode::Esc));
        assert_eq!(effect, Effect::None, "Esc should close help, not quit");
        assert!(!app.show_help, "help should be closed");
        assert!(!app.quit, "app should not quit");
    }

    #[test]
    fn question_closes_help_when_help_visible() {
        let mut app = app_with_repos(1);
        app.show_help = true;
        let effect = handle_key(&mut app, key(KeyCode::Char('?')));
        assert_eq!(effect, Effect::None, "? should close help overlay");
        assert!(!app.show_help, "help should be closed");
        assert!(!app.quit);
    }

    #[test]
    fn question_opens_help_when_help_not_visible() {
        let mut app = app_with_repos(1);
        assert_eq!(handle_key(&mut app, key(KeyCode::Char('?'))), Effect::None);
        assert!(app.show_help);
    }

    #[test]
    fn r_requests_refresh() {
        let mut app = app_with_repos(1);
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('r'))),
            Effect::Refresh
        );
    }

    #[test]
    fn d_opens_delete_confirmation_for_selected_repo() {
        let mut app = app_with_repos(2);
        app.selected = 1;
        assert_eq!(handle_key(&mut app, key(KeyCode::Char('d'))), Effect::None);
        assert_eq!(app.confirm_delete, Some(1));
    }

    #[test]
    fn delete_confirmation_y_requests_delete() {
        let mut app = app_with_repos(1);
        app.confirm_delete = Some(0);
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('y'))),
            Effect::DeleteSelected
        );
    }

    #[test]
    fn delete_confirmation_enter_requests_delete() {
        let mut app = app_with_repos(1);
        app.confirm_delete = Some(0);
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Enter)),
            Effect::DeleteSelected
        );
    }

    #[test]
    fn delete_confirmation_n_cancels() {
        let mut app = app_with_repos(1);
        app.confirm_delete = Some(0);
        assert_eq!(handle_key(&mut app, key(KeyCode::Char('n'))), Effect::None);
        assert_eq!(app.confirm_delete, None);
    }

    #[test]
    fn down_and_j_move_selection_until_end() {
        let mut app = app_with_repos(2);
        assert_eq!(handle_key(&mut app, key(KeyCode::Down)), Effect::None);
        assert_eq!(app.selected, 1);
        assert_eq!(handle_key(&mut app, key(KeyCode::Char('j'))), Effect::None);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn up_and_k_move_selection_until_start() {
        let mut app = app_with_repos(2);
        app.selected = 1;
        assert_eq!(handle_key(&mut app, key(KeyCode::Up)), Effect::None);
        assert_eq!(app.selected, 0);
        assert_eq!(handle_key(&mut app, key(KeyCode::Char('k'))), Effect::None);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn slash_starts_filter_and_chars_append() {
        let mut app = app_with_repos(1);
        assert_eq!(handle_key(&mut app, key(KeyCode::Char('/'))), Effect::None);
        assert_eq!(app.filter.as_deref(), Some(""));
        assert_eq!(handle_key(&mut app, key(KeyCode::Char('a'))), Effect::None);
        assert_eq!(app.filter.as_deref(), Some("a"));
        assert_eq!(handle_key(&mut app, key(KeyCode::Backspace)), Effect::None);
        assert_eq!(app.filter.as_deref(), Some(""));
    }

    #[test]
    fn navigation_skips_filtered_out_repos() {
        let mut app = App::new(vec![
            RepoEntry {
                name: "alpha".into(),
                path: "/alpha".into(),
                distro: None,
                status: None,
                error: None,
                github: None,
            },
            RepoEntry {
                name: "beta".into(),
                path: "/beta".into(),
                distro: None,
                status: None,
                error: None,
                github: None,
            },
            RepoEntry {
                name: "alpine".into(),
                path: "/alpine".into(),
                distro: None,
                status: None,
                error: None,
                github: None,
            },
        ]);
        app.filter = Some("al".into());
        app.clamp_selection_to_visible();
        assert_eq!(app.selected, 0);
        assert_eq!(handle_key(&mut app, key(KeyCode::Down)), Effect::None);
        assert_eq!(app.selected, 2);
    }
}
