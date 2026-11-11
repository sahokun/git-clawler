use chrono::{DateTime, Local, Utc};
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, Table};

use crate::scan::{ListRow, ScanOutcome};

/// Format a UTC timestamp as `HH:MM:SS` in the local timezone.
pub fn format_local_time_hms(ts: DateTime<Utc>) -> String {
    ts.with_timezone(&Local).format("%H:%M:%S").to_string()
}

pub fn format_watch_footer(last: DateTime<Utc>, interval_secs: u64) -> String {
    let interval_label = if interval_secs < 60 {
        format!("{}s", interval_secs)
    } else {
        format!("{}m", interval_secs / 60)
    };
    format!(
        "  watch  [refresh: {}]  [q to quit, r to refresh]\n  Last refresh: {}",
        interval_label,
        format_local_time_hms(last),
    )
}

pub fn render_list_table(rows: &[ListRow], color: bool, show_github: bool) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    let mut header = vec![
        "Repository",
        "Branch",
        "Uncommitted",
        "Push",
        "A / B",
        "Conflict",
    ];
    if show_github {
        header.push("Issues");
        header.push("PRs");
    }
    header.push("Last scan");
    table.set_header(header);

    for row in rows {
        let repo_label = if row.target.is_current {
            format!("* {}", row.target.path.display())
        } else {
            format!("  {}", row.target.path.display())
        };

        let last_scan = format_local_time_hms(row.scanned_at);

        match &row.outcome {
            ScanOutcome::Ok(status) => {
                let uncmt = status.uncommitted_files;
                let push = status.unpushed_branches.len();
                let ahead = status.ahead;
                let behind = status.behind;
                let branch = status.current_branch.as_deref().unwrap_or("-").to_string();

                let uncmt_cell = if color && uncmt > 0 {
                    Cell::new(uncmt.to_string()).fg(Color::Yellow)
                } else {
                    Cell::new(uncmt.to_string())
                };

                let push_cell = if color && push > 0 {
                    Cell::new(push.to_string()).fg(Color::Red)
                } else {
                    Cell::new(push.to_string())
                };

                let ab_text = format!("{} / {}", ahead, behind);
                let ab_cell = if color && behind > 0 {
                    Cell::new(ab_text).fg(Color::Cyan)
                } else {
                    Cell::new(ab_text)
                };

                let conflict_text = match status.conflict_predicted {
                    None => "-",
                    Some(true) => "⚠",
                    Some(false) => "✓",
                };
                let conflict_cell = if color && status.conflict_predicted == Some(true) {
                    Cell::new(conflict_text).fg(Color::Yellow)
                } else {
                    Cell::new(conflict_text)
                };

                let mut cells = vec![
                    Cell::new(repo_label),
                    Cell::new(branch),
                    uncmt_cell,
                    push_cell,
                    ab_cell,
                    conflict_cell,
                ];
                if show_github {
                    let (issues, prs) = status
                        .github
                        .as_ref()
                        .map(|g| {
                            (
                                g.open_issues
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "-".to_string()),
                                g.open_prs
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "-".to_string()),
                            )
                        })
                        .unwrap_or_else(|| ("-".to_string(), "-".to_string()));
                    cells.push(Cell::new(issues));
                    cells.push(Cell::new(prs));
                }
                cells.push(Cell::new(last_scan));
                table.add_row(cells);
            }
            ScanOutcome::Err(_) => {
                let mut cells = vec![
                    Cell::new(repo_label),
                    Cell::new("ERROR"),
                    Cell::new("-"),
                    Cell::new("-"),
                    Cell::new("-"),
                    Cell::new("-"),
                ];
                if show_github {
                    cells.push(Cell::new("-"));
                    cells.push(Cell::new("-"));
                }
                cells.push(Cell::new(last_scan));
                table.add_row(cells);
            }
        }
    }

    table.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{ListRow, ListTarget, ScanOutcome};
    use chrono::Utc;
    use git_clawler_core::{GithubInfo, RepoStatus};
    use std::path::PathBuf;

    fn make_target(name: &str, is_current: bool) -> ListTarget {
        ListTarget {
            index: 0,
            name: name.to_string(),
            path: PathBuf::from(format!("/tmp/{}", name)),
            is_current,
            distro: None,
            fetch: false,
        }
    }

    fn ok_row(name: &str, is_current: bool, status: RepoStatus) -> ListRow {
        ListRow {
            target: make_target(name, is_current),
            scanned_at: Utc::now(),
            outcome: ScanOutcome::Ok(status),
            remote_url: None,
        }
    }

    fn err_row(name: &str) -> ListRow {
        ListRow {
            target: make_target(name, false),
            scanned_at: Utc::now(),
            outcome: ScanOutcome::Err("test error".to_string()),
            remote_url: None,
        }
    }

    #[test]
    fn format_local_time_hms_produces_hh_mm_ss() {
        use chrono::TimeZone;
        // 2024-01-15 13:45:09 UTC
        let ts = Utc.with_ymd_and_hms(2024, 1, 15, 13, 45, 9).unwrap();
        let s = format_local_time_hms(ts);
        // The string must be exactly HH:MM:SS (8 chars with two colons).
        assert!(
            s.len() == 8 && s.chars().nth(2) == Some(':') && s.chars().nth(5) == Some(':'),
            "unexpected format: {s}"
        );
    }

    #[test]
    fn render_table_contains_header_columns() {
        let rows = vec![ok_row("repo-a", false, RepoStatus::default())];
        let out = render_list_table(&rows, false, false);
        assert!(out.contains("Repository"));
        assert!(out.contains("Branch"));
        assert!(out.contains("Uncommitted"));
        assert!(out.contains("Push"));
        assert!(out.contains("Conflict"));
        assert!(out.contains("Last scan"));
        assert!(!out.contains("Issues"));
        assert!(!out.contains("PRs"));
        // Old header names must be gone
        assert!(!out.contains("Uncmt"), "old header 'Uncmt' must not appear");
        assert!(
            !out.contains("Updated"),
            "old header 'Updated' must not appear"
        );
    }

    #[test]
    fn render_table_github_columns_are_optional() {
        let rows = vec![ok_row(
            "repo-a",
            false,
            RepoStatus {
                github: Some(GithubInfo {
                    open_issues: Some(12),
                    open_prs: Some(3),
                }),
                ..RepoStatus::default()
            },
        )];
        let out = render_list_table(&rows, false, true);
        assert!(out.contains("Issues"));
        assert!(out.contains("PRs"));
        assert!(out.contains("12"));
        assert!(out.contains("3"));
    }

    #[test]
    fn render_table_current_repo_has_star_with_full_path() {
        let rows = vec![ok_row("my-repo", true, RepoStatus::default())];
        let out = render_list_table(&rows, false, false);
        // Full path format: "* /tmp/my-repo"
        assert!(
            out.contains("* /tmp/my-repo"),
            "expected '* /tmp/my-repo', got:\n{out}"
        );
    }

    #[test]
    fn render_table_non_current_repo_shows_full_path() {
        let rows = vec![ok_row("other-repo", false, RepoStatus::default())];
        let out = render_list_table(&rows, false, false);
        assert!(!out.contains("* /tmp/other-repo"));
        assert!(out.contains("/tmp/other-repo"));
    }

    #[test]
    fn render_table_error_row_shows_error() {
        let rows = vec![err_row("bad-repo")];
        let out = render_list_table(&rows, false, false);
        assert!(out.contains("ERROR"));
        assert!(out.contains("/tmp/bad-repo"));
    }

    #[test]
    fn render_table_ahead_behind_format() {
        let status = RepoStatus {
            ahead: 2,
            behind: 1,
            ..RepoStatus::default()
        };
        let rows = vec![ok_row("my-repo", false, status)];
        let out = render_list_table(&rows, false, false);
        assert!(out.contains("2 / 1"));
        assert!(out.contains("A / B"));
    }

    #[test]
    fn render_table_no_ansi_when_color_false() {
        let status = RepoStatus {
            uncommitted_files: 5,
            ahead: 3,
            behind: 2,
            ..RepoStatus::default()
        };
        let rows = vec![ok_row("colored-repo", false, status)];
        let out = render_list_table(&rows, false, false);
        assert!(!out.contains("\x1b["));
    }

    #[test]
    fn r1_render_table_conflict_predicted_true_shows_warning() {
        let status = RepoStatus {
            conflict_predicted: Some(true),
            ..RepoStatus::default()
        };
        let rows = vec![ok_row("conflict-repo", false, status)];
        let out = render_list_table(&rows, false, false);
        assert!(out.contains("⚠"), "expected ⚠ marker, got:\n{}", out);
    }

    #[test]
    fn r2_render_table_conflict_predicted_none_shows_dash() {
        let status = RepoStatus {
            conflict_predicted: None,
            ..RepoStatus::default()
        };
        let rows = vec![ok_row("unknown", false, status)];
        let out = render_list_table(&rows, false, false);
        // None => "-", not "?". Use a repo path without ASCII hyphens so this
        // assertion specifically covers the conflict marker.
        assert!(out.contains('-'), "expected '-' marker, got:\n{}", out);
        assert!(
            !out.contains('?'),
            "must not show '?' marker, got:\n{}",
            out
        );
    }

    #[test]
    fn format_watch_footer_no_next_has_last_refresh() {
        let now = Utc::now();
        let s = format_watch_footer(now, 300);
        assert!(s.contains("watch"));
        assert!(s.contains("5m"));
        assert!(s.contains("q to quit"));
        assert!(s.contains("r to refresh"));
        assert!(s.contains("Last refresh"));
        // "Next" must be gone
        assert!(
            !s.contains("Next"),
            "footer must not contain 'Next', got: {s}"
        );
    }

    #[test]
    fn format_watch_footer_seconds_for_short_interval() {
        let now = Utc::now();
        let s = format_watch_footer(now, 30);
        assert!(s.contains("30s"), "should contain '30s', got: {}", s);
        assert!(!s.contains("0m"), "must NOT contain '0m', got: {}", s);
    }

    #[test]
    fn format_watch_footer_minutes_for_long_interval() {
        let now = Utc::now();
        let s = format_watch_footer(now, 300);
        assert!(s.contains("5m"), "should contain '5m', got: {}", s);
    }
}
