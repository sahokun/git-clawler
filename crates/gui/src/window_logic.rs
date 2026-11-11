//! Pure-logic helpers for window state transitions. Lives in its own
//! module so it can be unit-tested on every platform (the rest of
//! `window.rs` is Windows-only Tauri glue).

/// Returns true if the window should be hidden (minimized to tray).
///
/// Only hides when transitioning from non-minimized to minimized state
/// AND `close_to_tray` is enabled.
pub fn should_hide_on_minimize(
    prev_minimized: bool,
    curr_minimized: bool,
    close_to_tray: bool,
) -> bool {
    !prev_minimized && curr_minimized && close_to_tray
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowStateSnapshot {
    Normal {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    Maximized,
}

pub fn should_save_normal_window_state(width: u32, height: u32) -> bool {
    width >= git_clawler_core::config::GUI_MIN_WINDOW_WIDTH
        && height >= git_clawler_core::config::GUI_MIN_WINDOW_HEIGHT
}

pub fn merge_window_state(
    gui: &mut git_clawler_core::config::GuiConfig,
    snapshot: WindowStateSnapshot,
) -> bool {
    match snapshot {
        WindowStateSnapshot::Normal {
            x,
            y,
            width,
            height,
        } => {
            if !should_save_normal_window_state(width, height) {
                return false;
            }
            if gui.window_x == Some(x)
                && gui.window_y == Some(y)
                && gui.window_width == Some(width)
                && gui.window_height == Some(height)
                && gui.window_maximized == Some(false)
            {
                return false;
            }
            gui.window_x = Some(x);
            gui.window_y = Some(y);
            gui.window_width = Some(width);
            gui.window_height = Some(height);
            gui.window_maximized = Some(false);
            true
        }
        WindowStateSnapshot::Maximized => {
            if gui.window_maximized == Some(true) {
                return false;
            }
            gui.window_maximized = Some(true);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{merge_window_state, should_hide_on_minimize, WindowStateSnapshot};

    #[test]
    fn test_no_hide_when_not_minimized() {
        assert!(!should_hide_on_minimize(false, false, true));
    }

    #[test]
    fn test_hide_on_minimize_edge_true_to_true() {
        // Already minimized, no new transition → no hide
        assert!(!should_hide_on_minimize(true, true, true));
    }

    #[test]
    fn test_hide_on_minimize_edge_false_to_true() {
        // Fresh minimize transition with tray enabled → hide
        assert!(should_hide_on_minimize(false, true, true));
    }

    #[test]
    fn test_no_hide_when_tray_disabled() {
        // Minimize transition but tray disabled → no hide
        assert!(!should_hide_on_minimize(false, true, false));
    }

    #[test]
    fn test_no_hide_on_restore() {
        // Transition from minimized back to normal → no hide
        assert!(!should_hide_on_minimize(true, false, true));
    }

    #[test]
    fn merge_window_state_saves_normal_geometry() {
        let mut gui = git_clawler_core::config::GuiConfig::default();

        let changed = merge_window_state(
            &mut gui,
            WindowStateSnapshot::Normal {
                x: -1200,
                y: 80,
                width: 1280,
                height: 720,
            },
        );

        assert!(changed);
        assert_eq!(gui.window_x, Some(-1200));
        assert_eq!(gui.window_y, Some(80));
        assert_eq!(gui.window_width, Some(1280));
        assert_eq!(gui.window_height, Some(720));
        assert_eq!(gui.window_maximized, Some(false));
    }

    #[test]
    fn merge_window_state_ignores_too_small_normal_geometry() {
        let mut gui = git_clawler_core::config::GuiConfig::default();

        let changed = merge_window_state(
            &mut gui,
            WindowStateSnapshot::Normal {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
        );

        assert!(!changed);
        assert_eq!(gui, git_clawler_core::config::GuiConfig::default());
    }

    #[test]
    fn merge_window_state_saves_maximized_without_clearing_geometry() {
        let mut gui = git_clawler_core::config::GuiConfig {
            window_x: Some(10),
            window_y: Some(20),
            window_width: Some(800),
            window_height: Some(600),
            ..Default::default()
        };

        let changed = merge_window_state(&mut gui, WindowStateSnapshot::Maximized);

        assert!(changed);
        assert_eq!(gui.window_x, Some(10));
        assert_eq!(gui.window_y, Some(20));
        assert_eq!(gui.window_width, Some(800));
        assert_eq!(gui.window_height, Some(600));
        assert_eq!(gui.window_maximized, Some(true));
    }
}
