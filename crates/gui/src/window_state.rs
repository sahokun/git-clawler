#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    fn right(self) -> i64 {
        i64::from(self.x) + i64::from(self.width)
    }

    fn bottom(self) -> i64 {
        i64::from(self.y) + i64::from(self.height)
    }
}

pub fn intersects_enough(window: Rect, monitor: Rect) -> bool {
    const MIN_VISIBLE_WIDTH: i64 = 80;
    const MIN_VISIBLE_HEIGHT: i64 = 80;

    let left = i64::from(window.x).max(i64::from(monitor.x));
    let top = i64::from(window.y).max(i64::from(monitor.y));
    let right = window.right().min(monitor.right());
    let bottom = window.bottom().min(monitor.bottom());

    right - left >= MIN_VISIBLE_WIDTH && bottom - top >= MIN_VISIBLE_HEIGHT
}

pub fn should_restore_position(window: Rect, monitors: &[Rect]) -> bool {
    if window.width == 0 || window.height == 0 || monitors.is_empty() {
        return false;
    }
    monitors
        .iter()
        .copied()
        .any(|monitor| intersects_enough(window, monitor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_position_on_negative_coordinate_monitor() {
        let monitors = [Rect {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        }];
        let window = Rect {
            x: -1600,
            y: 120,
            width: 1000,
            height: 700,
        };

        assert!(should_restore_position(window, &monitors));
    }

    #[test]
    fn skips_offscreen_position() {
        let monitors = [Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }];
        let window = Rect {
            x: 2500,
            y: 100,
            width: 900,
            height: 600,
        };

        assert!(!should_restore_position(window, &monitors));
    }

    #[test]
    fn skips_position_with_only_a_sliver_visible() {
        let monitors = [Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }];
        let window = Rect {
            x: 1900,
            y: 20,
            width: 900,
            height: 600,
        };

        assert!(!should_restore_position(window, &monitors));
    }
}
