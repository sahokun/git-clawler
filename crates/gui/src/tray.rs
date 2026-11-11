//! System-tray status derivation and tooltip formatting.
//!
//! Pure (platform-agnostic) logic lives at the top of the file so it can
//! be unit-tested on Linux. Tauri integration (icon installation, menu
//! event handling, runtime updates) is gated to `target_os = "windows"`.

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use crate::dto::{RepoRow, RowStatus};

/// Aggregate state used to pick the tray icon colour and tooltip mood.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayStatus {
    Green,
    Yellow,
    Red,
}

/// Derive the tray colour from a list of repository rows.
///
/// Rules:
/// - rows whose status is `Err` do not influence the colour unless every
///   row is an error (then Red).
/// - among the remaining rows: any unpushed branch → Red; any uncommitted
///   change → Yellow; otherwise Green (also Green for an empty list).
pub fn derive_tray_status(rows: &[RepoRow]) -> TrayStatus {
    let non_err: Vec<&RepoRow> = rows
        .iter()
        .filter(|r| matches!(r.status, RowStatus::Ok(_)))
        .collect();
    let err_count = rows.len() - non_err.len();

    if non_err.is_empty() {
        return if err_count > 0 {
            TrayStatus::Red
        } else {
            TrayStatus::Green
        };
    }

    let any_unpushed = non_err.iter().any(|r| match &r.status {
        RowStatus::Ok(s) => !s.unpushed_branches.is_empty(),
        RowStatus::Err(_) => false,
    });
    if any_unpushed {
        return TrayStatus::Red;
    }
    let any_uncommitted = non_err.iter().any(|r| match &r.status {
        RowStatus::Ok(s) => s.uncommitted_files > 0,
        RowStatus::Err(_) => false,
    });
    if any_uncommitted {
        TrayStatus::Yellow
    } else {
        TrayStatus::Green
    }
}

/// Format the tooltip shown on the tray icon.
pub fn format_tooltip(rows: &[RepoRow]) -> String {
    let mut clean = 0usize;
    let mut dirty = 0usize;
    let mut unpushed = 0usize;
    let mut err_count = 0usize;
    for r in rows {
        match &r.status {
            RowStatus::Err(_) => err_count += 1,
            RowStatus::Ok(s) => {
                let has_unpushed = !s.unpushed_branches.is_empty();
                if s.uncommitted_files > 0 {
                    dirty += 1;
                }
                if has_unpushed {
                    unpushed += 1;
                }
                if s.uncommitted_files == 0 && !has_unpushed {
                    clean += 1;
                }
            }
        }
    }
    format!(
        "git-clawler — {clean} clean / {dirty} uncommitted / {unpushed} unpushed (err {err_count})"
    )
}

// ---------------------------------------------------------------------------
// Tauri integration (Windows only)
// ---------------------------------------------------------------------------

pub const MENU_ID_OPEN: &str = "open";
pub const MENU_ID_REFRESH: &str = "refresh";
pub const MENU_ID_SETTINGS: &str = "settings";
pub const MENU_ID_EXIT: &str = "exit";

pub const EVENT_SETTINGS_OPEN: &str = "settings://open";
pub const EVENT_REFRESH_REQUESTED: &str = "refresh://requested";

fn tray_icon_bytes_for_status(status: TrayStatus) -> &'static [u8] {
    match status {
        TrayStatus::Green => include_bytes!("../resources/tray/green.png"),
        TrayStatus::Yellow => include_bytes!("../resources/tray/yellow.png"),
        TrayStatus::Red => include_bytes!("../resources/tray/red.png"),
    }
}

#[cfg(target_os = "windows")]
fn icon_bytes_for_status(status: TrayStatus) -> &'static [u8] {
    tray_icon_bytes_for_status(status)
}

#[cfg(target_os = "windows")]
pub fn build_tray(app: &tauri::AppHandle, initial_status: TrayStatus) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let open = MenuItem::with_id(app, MENU_ID_OPEN, "Open", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, MENU_ID_REFRESH, "Refresh", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, MENU_ID_SETTINGS, "Settings...", true, None::<&str>)?;
    let exit = MenuItem::with_id(app, MENU_ID_EXIT, "Exit", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(app, &[&open, &refresh, &sep1, &settings, &sep2, &exit])?;

    let icon = tauri::image::Image::from_bytes(icon_bytes_for_status(initial_status))?;

    let app_handle = app.clone();
    TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .icon(icon)
        .tooltip("git-clawler")
        .on_menu_event(move |app, event| handle_menu_event(app, &event))
        .on_tray_icon_event(move |_tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::window::toggle_window_from_any_thread(&app_handle);
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn refresh_status(app: &tauri::AppHandle, status: TrayStatus, tooltip: String) {
    if let Some(tray) = app.tray_by_id("main-tray") {
        match tauri::image::Image::from_bytes(icon_bytes_for_status(status)) {
            Ok(icon) => {
                if let Err(e) = tray.set_icon(Some(icon)) {
                    tracing::warn!(error=%e, "tray set_icon failed");
                }
            }
            Err(e) => tracing::warn!(error=%e, "tray icon bytes invalid"),
        }
        if let Err(e) = tray.set_tooltip(Some(&tooltip)) {
            tracing::warn!(error=%e, "tray set_tooltip failed");
        }
    }
}

#[cfg(target_os = "windows")]
fn handle_menu_event(app: &tauri::AppHandle, event: &tauri::menu::MenuEvent) {
    use tauri::Emitter as _;
    match event.id().as_ref() {
        MENU_ID_OPEN => {
            if let Err(e) = crate::window::show_window(app) {
                tracing::warn!(error=%e, "show_window failed");
            }
        }
        MENU_ID_REFRESH => {
            use tauri::Manager;
            match app.try_state::<crate::realtime::ManualRefreshState>() {
                Some(state) => {
                    if let Err(e) = state.0.try_send(()) {
                        tracing::warn!(error=%e, "manual refresh request dropped");
                    }
                }
                None => {
                    tracing::warn!("ManualRefreshState unavailable; falling back to event");
                }
            }
            // Keep the event for future frontend hooks / observability.
            if let Err(e) = app.emit(EVENT_REFRESH_REQUESTED, ()) {
                tracing::warn!(error=%e, "emit refresh://requested failed");
            }
        }
        MENU_ID_SETTINGS => {
            if let Err(e) = crate::window::show_window(app) {
                tracing::warn!(error=%e, "show_window failed");
            }
            if let Err(e) = app.emit(EVENT_SETTINGS_OPEN, ()) {
                tracing::warn!(error=%e, "emit settings://open failed");
            }
        }
        MENU_ID_EXIT => {
            crate::request_app_exit(app);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Tests (pure logic only)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{RepoStatusDto, UnpushedBranchDto};

    // -----------------------------------------------------------------------
    // Tray icon asset regression tests
    //
    // These tests guard against the tray PNG assets being accidentally
    // reverted to the original single-color placeholder files (99-100 bytes).
    // They run on all platforms because `include_bytes!` embeds the asset at
    // compile time and the PNG header check is pure-Rust with no OS dependency.
    // -----------------------------------------------------------------------

    /// PNG magic bytes: \x89PNG\r\n\x1a\n
    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

    /// Minimum acceptable size for a non-placeholder tray icon (bytes).
    /// The original placeholder files were 99-100 bytes; the replacement icons
    /// are ~770+ bytes. 300 bytes is a conservative lower bound.
    const PNG_MIN_SIZE: usize = 300;

    const PNG_IHDR_DATA_LEN: u32 = 13;
    const PNG_FULL_IHDR_END: usize = 33;

    fn tray_icon_assets() -> [(&'static str, &'static [u8]); 3] {
        [
            ("green", tray_icon_bytes_for_status(TrayStatus::Green)),
            ("yellow", tray_icon_bytes_for_status(TrayStatus::Yellow)),
            ("red", tray_icon_bytes_for_status(TrayStatus::Red)),
        ]
    }

    fn assert_tray_icon_png(name: &str, bytes: &[u8]) {
        assert!(
            bytes.len() >= PNG_FULL_IHDR_END,
            "{name}.png too small to contain full IHDR chunk"
        );
        assert_eq!(
            &bytes[..8],
            PNG_MAGIC,
            "{name}.png does not start with PNG magic bytes"
        );
        let ihdr_len = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        assert_eq!(
            ihdr_len, PNG_IHDR_DATA_LEN,
            "{name}.png IHDR length is {ihdr_len}, expected {PNG_IHDR_DATA_LEN}"
        );
        assert_eq!(
            &bytes[12..16],
            b"IHDR",
            "{name}.png first chunk is not IHDR"
        );

        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        let bit_depth = bytes[24];
        let color_type = bytes[25];

        assert_eq!(w, 32, "{name}.png width is {w}, expected 32");
        assert_eq!(h, 32, "{name}.png height is {h}, expected 32");
        assert_eq!(
            bit_depth, 8,
            "{name}.png bit depth is {bit_depth}, expected 8"
        );
        assert_eq!(
            color_type, 6,
            "{name}.png color type is {color_type}, expected 6 (RGBA)"
        );
        assert!(
            bytes.len() >= PNG_MIN_SIZE,
            "{name}.png is only {} bytes — looks like a placeholder (min {})",
            bytes.len(),
            PNG_MIN_SIZE
        );
    }

    #[test]
    fn tray_icons_are_valid_pngs_and_not_placeholders() {
        for (name, bytes) in tray_icon_assets() {
            assert_tray_icon_png(name, bytes);
        }
    }

    /// The three icon assets must be distinct (different status colours).
    #[test]
    fn tray_icons_are_distinct() {
        let [(_, green), (_, yellow), (_, red)] = tray_icon_assets();
        assert_ne!(green, yellow, "green and yellow tray icons are identical");
        assert_ne!(yellow, red, "yellow and red tray icons are identical");
        assert_ne!(green, red, "green and red tray icons are identical");
    }

    fn ok_row(uncommitted: u32, unpushed: u32) -> RepoRow {
        let unpushed_branches = (0..unpushed)
            .map(|i| UnpushedBranchDto {
                name: format!("b{i}"),
                ahead: 1,
                has_upstream: true,
            })
            .collect();
        RepoRow {
            index: 0,
            name: "x".into(),
            path: "/x".into(),
            is_current: false,
            scanned_at: "2024-01-01T00:00:00Z".into(),
            status: RowStatus::Ok(RepoStatusDto {
                current_branch: Some("main".into()),
                uncommitted_files: uncommitted,
                unpushed_branches,
                ahead: 0,
                behind: 0,
                open_issues: None,
                open_prs: None,
                conflict_predicted: None,
                fetch_warning: None,
                recent_commits: vec![],
            }),
            distro: None,
            remote_url: None,
        }
    }

    fn err_row() -> RepoRow {
        RepoRow {
            index: 0,
            name: "e".into(),
            path: "/e".into(),
            is_current: false,
            scanned_at: "2024-01-01T00:00:00Z".into(),
            status: RowStatus::Err("boom".into()),
            distro: None,
            remote_url: None,
        }
    }

    #[test]
    fn empty_rows_is_green() {
        assert_eq!(derive_tray_status(&[]), TrayStatus::Green);
    }

    #[test]
    fn all_clean_is_green() {
        let rows = vec![ok_row(0, 0), ok_row(0, 0)];
        assert_eq!(derive_tray_status(&rows), TrayStatus::Green);
    }

    #[test]
    fn any_uncommitted_is_yellow() {
        let rows = vec![ok_row(0, 0), ok_row(2, 0)];
        assert_eq!(derive_tray_status(&rows), TrayStatus::Yellow);
    }

    #[test]
    fn any_unpushed_is_red() {
        let rows = vec![ok_row(0, 0), ok_row(0, 1)];
        assert_eq!(derive_tray_status(&rows), TrayStatus::Red);
    }

    #[test]
    fn unpushed_wins_over_uncommitted() {
        let rows = vec![ok_row(3, 0), ok_row(0, 1)];
        assert_eq!(derive_tray_status(&rows), TrayStatus::Red);
    }

    #[test]
    fn all_err_is_red() {
        let rows = vec![err_row(), err_row()];
        assert_eq!(derive_tray_status(&rows), TrayStatus::Red);
    }

    #[test]
    fn err_mixed_with_clean_is_green() {
        let rows = vec![err_row(), ok_row(0, 0)];
        assert_eq!(derive_tray_status(&rows), TrayStatus::Green);
    }

    #[test]
    fn err_mixed_with_uncommitted_is_yellow() {
        let rows = vec![err_row(), ok_row(2, 0)];
        assert_eq!(derive_tray_status(&rows), TrayStatus::Yellow);
    }

    #[test]
    fn tooltip_all_clean() {
        let rows = vec![ok_row(0, 0), ok_row(0, 0)];
        assert_eq!(
            format_tooltip(&rows),
            "git-clawler — 2 clean / 0 uncommitted / 0 unpushed (err 0)"
        );
    }

    #[test]
    fn tooltip_with_uncommitted_only() {
        let rows = vec![ok_row(0, 0), ok_row(3, 0)];
        assert_eq!(
            format_tooltip(&rows),
            "git-clawler — 1 clean / 1 uncommitted / 0 unpushed (err 0)"
        );
    }

    #[test]
    fn tooltip_with_unpushed_and_err() {
        let rows = vec![ok_row(0, 1), err_row()];
        assert_eq!(
            format_tooltip(&rows),
            "git-clawler — 0 clean / 0 uncommitted / 1 unpushed (err 1)"
        );
    }

    #[test]
    fn tooltip_mixed() {
        let rows = vec![ok_row(0, 0), ok_row(2, 0), ok_row(1, 1), err_row()];
        // clean: 1 (only the all-zero row)
        // dirty (uncommitted>0): rows with 2,0 and 1,1 → 2
        // unpushed: row with 1,1 → 1
        // err: 1
        assert_eq!(
            format_tooltip(&rows),
            "git-clawler — 1 clean / 2 uncommitted / 1 unpushed (err 1)"
        );
    }
}
