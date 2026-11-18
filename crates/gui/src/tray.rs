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
        RowStatus::Err(_) | RowStatus::NotFound(_) => false,
    });
    if any_unpushed {
        return TrayStatus::Red;
    }
    let any_uncommitted = non_err.iter().any(|r| match &r.status {
        RowStatus::Ok(s) => s.uncommitted_files > 0,
        RowStatus::Err(_) | RowStatus::NotFound(_) => false,
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
            RowStatus::Err(_) | RowStatus::NotFound(_) => err_count += 1,
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

// Reuse the application artwork, so tray branding cannot drift to old assets.
const TRAY_ICON: &[u8] = include_bytes!("../icons/32x32.png");

/// Brightest accent channel in the application artwork (teal ~ `#5ACFC6`).
const ACCENT_REFERENCE: u16 = 209;

/// Accent colour painted for each tray state, at full brightness.
fn status_rgb(status: TrayStatus) -> [u8; 3] {
    match status {
        TrayStatus::Green => [46, 204, 113],
        TrayStatus::Yellow => [241, 196, 15],
        TrayStatus::Red => [231, 76, 60],
    }
}

/// The artwork's accent is teal: green and blue both clearly exceed red.
/// The dark background, white glyph and orange node do not match.
fn is_accent(r: u8, g: u8, b: u8) -> bool {
    let (r, g, b) = (u16::from(r), u16::from(g), u16::from(b));
    g > r + 24 && b > r + 24 && g.max(b) >= 64
}

fn apply_status_colour(rgba: &mut [u8], status: TrayStatus) {
    let target = status_rgb(status);
    for pixel in rgba.chunks_exact_mut(4) {
        let [r, g, b] = [pixel[0], pixel[1], pixel[2]];
        // Only recolour the accent; retain the glyph, outline and alpha.
        if !is_accent(r, g, b) {
            continue;
        }
        // Scale by the pixel's brightness so anti-aliased edges stay smooth.
        let level = u16::from(g.max(b));
        for (channel, &full) in pixel[..3].iter_mut().zip(&target) {
            let scaled = u16::from(full) * level / ACCENT_REFERENCE;
            *channel = u8::try_from(scaled.min(255)).unwrap_or(u8::MAX);
        }
    }
}

#[cfg(target_os = "windows")]
fn icon_for_status(status: TrayStatus) -> tauri::Result<tauri::image::Image<'static>> {
    let image = tauri::image::Image::from_bytes(TRAY_ICON)?;
    let mut rgba = image.rgba().to_vec();
    apply_status_colour(&mut rgba, status);
    Ok(tauri::image::Image::new_owned(
        rgba,
        image.width(),
        image.height(),
    ))
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

    let icon = icon_for_status(initial_status)?;

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
        match icon_for_status(status) {
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

    #[test]
    fn status_colour_preserves_glyph_and_transparency() {
        // Accent at reference brightness, white glyph, transparent background,
        // orange node.
        let original = [
            90, 209, 198, 128, 240, 240, 240, 255, 20, 20, 20, 0, 221, 153, 44, 254,
        ];
        for status in [TrayStatus::Green, TrayStatus::Yellow, TrayStatus::Red] {
            let mut pixels = original;
            apply_status_colour(&mut pixels, status);
            assert_eq!(&pixels[..3], &status_rgb(status), "{status:?}");
            assert_eq!(pixels[3], 128, "{status:?} alpha");
            assert_eq!(&pixels[4..], &original[4..], "{status:?}");
        }
    }

    #[test]
    fn status_colour_scales_dim_accent_pixels() {
        let mut pixels = [43, 81, 82, 200];
        apply_status_colour(&mut pixels, TrayStatus::Red);
        // 82 / 209 of the full red accent.
        assert_eq!(pixels, [90, 29, 23, 200]);
    }

    /// Decode the real application artwork without Tauri, so this runs on
    /// every platform and catches artwork whose accent no longer matches.
    fn decode_artwork() -> Vec<u8> {
        let decoder = png::Decoder::new(std::io::Cursor::new(TRAY_ICON));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (32, 32));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        buf.truncate(info.buffer_size());
        buf
    }

    #[test]
    fn application_artwork_has_distinct_status_colours() {
        let original = decode_artwork();
        let accent = original
            .chunks_exact(4)
            .filter(|p| p[3] > 0 && is_accent(p[0], p[1], p[2]))
            .count();
        assert!(accent >= 50, "only {accent} accent pixels found");

        let render = |status| {
            let mut rgba = original.clone();
            apply_status_colour(&mut rgba, status);
            rgba
        };
        let (green, yellow, red) = (
            render(TrayStatus::Green),
            render(TrayStatus::Yellow),
            render(TrayStatus::Red),
        );
        assert_ne!(green, yellow);
        assert_ne!(yellow, red);
        assert_ne!(green, red);
        for icon in [&green, &yellow, &red] {
            for (a, b) in original.chunks_exact(4).zip(icon.chunks_exact(4)) {
                assert_eq!(a[3], b[3]);
            }
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn application_artwork_decodes_for_all_tray_states() {
        let green = icon_for_status(TrayStatus::Green).unwrap();
        assert_eq!((green.width(), green.height()), (32, 32));
        let yellow = icon_for_status(TrayStatus::Yellow).unwrap();
        let red = icon_for_status(TrayStatus::Red).unwrap();
        assert_ne!(green.rgba(), yellow.rgba());
        assert_ne!(yellow.rgba(), red.rgba());
        for (a, b) in green.rgba().chunks_exact(4).zip(red.rgba().chunks_exact(4)) {
            assert_eq!(a[3], b[3]);
        }
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
            worktree_of: None,
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
            worktree_of: None,
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
