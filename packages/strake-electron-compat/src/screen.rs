//! `screen` display enumeration over winit monitor metrics (issue #96).
//!
//! [`Screen`] is a headless-testable snapshot of the OS display list: the
//! embedder builds it from winit monitor handles at startup (and refreshes it
//! on display-change events where winit reports them) while unit tests inject
//! a fixed monitor list. [`Screen::default`] carries one sane fallback
//! primary display so the shim keeps working headless; [`ShellWindow`] uses
//! the primary display to place windows when the app provides real metrics.
//!
//! [`ShellWindow`]: crate::ShellWindow

use crate::Bounds;

/// One OS display (`Electron.Display` subset).
#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    /// Stable display id (winit has no stable id; the embedder assigns one
    /// per snapshot, keeping 0 for the primary).
    pub id: u64,
    /// Display bounds in physical pixels (`Display.bounds`).
    pub bounds: Bounds,
    /// Work area in physical pixels (`Display.workArea`); defaults to the
    /// full bounds until the embedder subtracts taskbars/docks.
    pub work_area: Bounds,
    /// Pixel scale factor (`Display.scaleFactor`).
    pub scale_factor: f32,
}

impl Display {
    /// A display with the work area covering the full bounds.
    pub fn new(id: u64, bounds: Bounds, scale_factor: f32) -> Self {
        Self {
            id,
            bounds,
            work_area: bounds,
            scale_factor,
        }
    }

    /// Whether `point` lies inside these bounds.
    pub fn contains_point(&self, x: i32, y: i32) -> bool {
        let x_in = x >= self.bounds.x && x < self.bounds.x.saturating_add(self.bounds.width as i32);
        let y_in =
            y >= self.bounds.y && y < self.bounds.y.saturating_add(self.bounds.height as i32);
        x_in && y_in
    }

    /// Overlap area with `bounds` in square pixels (for `getDisplayMatching`).
    pub fn overlap_area(&self, bounds: &Bounds) -> i64 {
        let left = self.bounds.x.max(bounds.x) as i64;
        let top = self.bounds.y.max(bounds.y) as i64;
        let right = (self.bounds.x.saturating_add(self.bounds.width as i32) as i64)
            .min(bounds.x.saturating_add(bounds.width as i32) as i64);
        let bottom = (self.bounds.y.saturating_add(self.bounds.height as i32) as i64)
            .min(bounds.y.saturating_add(bounds.height as i32) as i64);
        (right - left).max(0) * (bottom - top).max(0)
    }
}

/// Snapshot of the OS display list (`Electron.screen`).
#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    displays: Vec<Display>,
}

impl Screen {
    /// Snapshot from an explicit display list (winit monitors at runtime, an
    /// injected list in tests). An empty list means "no metrics available".
    pub fn new(displays: Vec<Display>) -> Self {
        Self { displays }
    }

    /// All displays (`screen.getAllDisplays`).
    pub fn get_all_displays(&self) -> &[Display] {
        &self.displays
    }

    /// The primary display (`screen.getPrimaryDisplay`): the first display in
    /// the snapshot. `None` when no metrics are available.
    pub fn get_primary_display(&self) -> Option<&Display> {
        self.displays.first()
    }

    /// The display with the greatest overlap with `bounds`
    /// (`screen.getDisplayMatching`). Falls back to the primary on ties and
    /// to `None` when no metrics are available.
    pub fn get_display_matching(&self, bounds: &Bounds) -> Option<&Display> {
        self.displays
            .iter()
            .max_by_key(|display| display.overlap_area(bounds))
            .filter(|best| best.overlap_area(bounds) > 0)
            .or_else(|| self.get_primary_display())
    }

    /// The display containing `point`, else the nearest by centre distance
    /// (`screen.getDisplayNearestPoint`). `None` without metrics.
    pub fn get_display_nearest_point(&self, x: i32, y: i32) -> Option<&Display> {
        if let Some(hit) = self
            .displays
            .iter()
            .find(|display| display.contains_point(x, y))
        {
            return Some(hit);
        }
        self.displays.iter().min_by_key(|display| {
            let cx = display.bounds.x + (display.bounds.width as i32) / 2;
            let cy = display.bounds.y + (display.bounds.height as i32) / 2;
            let dx = (cx - x) as i64;
            let dy = (cy - y) as i64;
            dx * dx + dy * dy
        })
    }

    /// Center `content` on the primary display (window placement, issue #96).
    /// `None` without metrics so the caller keeps its headless fallback.
    pub fn suggest_centered_position(&self, width: u32, height: u32) -> Option<(i32, i32)> {
        let primary = self.get_primary_display()?;
        let x = primary.bounds.x + ((primary.bounds.width as i32 - width as i32).max(0) / 2);
        let y = primary.bounds.y + ((primary.bounds.height as i32 - height as i32).max(0) / 2);
        Some((x, y))
    }
}

/// Headless fallback: one sane primary display so the shim keeps placing
/// windows without OS metrics. The embedder replaces this with real winit
/// monitor data wherever a display exists.
impl Default for Screen {
    fn default() -> Self {
        Self::new(vec![Display::new(
            0,
            Bounds {
                x: 0,
                y: 0,
                width: 1024,
                height: 768,
            },
            1.0,
        )])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_monitors() -> Screen {
        Screen::new(vec![
            Display::new(
                0,
                Bounds {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
                1.0,
            ),
            Display::new(
                1,
                Bounds {
                    x: 1920,
                    y: 0,
                    width: 2560,
                    height: 1440,
                },
                2.0,
            ),
        ])
    }

    #[test]
    fn primary_is_first_and_all_lists_both() {
        let screen = two_monitors();
        assert_eq!(screen.get_all_displays().len(), 2);
        assert_eq!(screen.get_primary_display().map(|d| d.id), Some(0));
        assert_eq!(
            screen.get_primary_display().map(|d| d.scale_factor),
            Some(1.0)
        );
    }

    #[test]
    fn matching_prefers_greatest_overlap() {
        let screen = two_monitors();
        let window = Bounds {
            x: 2000,
            y: 100,
            width: 800,
            height: 600,
        };
        assert_eq!(screen.get_display_matching(&window).map(|d| d.id), Some(1));
        let primary_window = Bounds {
            x: 100,
            y: 100,
            width: 800,
            height: 600,
        };
        assert_eq!(
            screen.get_display_matching(&primary_window).map(|d| d.id),
            Some(0)
        );
    }

    #[test]
    fn nearest_point_hits_inside_then_closest() {
        let screen = two_monitors();
        assert_eq!(
            screen.get_display_nearest_point(100, 100).map(|d| d.id),
            Some(0)
        );
        assert_eq!(
            screen.get_display_nearest_point(3000, 700).map(|d| d.id),
            Some(1)
        );
        // Between the monitors: the closer centre wins (the 1080p primary).
        assert_eq!(
            screen.get_display_nearest_point(1920, 2000).map(|d| d.id),
            Some(0)
        );
    }

    #[test]
    fn empty_screen_reports_no_metrics() {
        let screen = Screen::new(vec![]);
        assert!(screen.get_primary_display().is_none());
        assert!(screen.get_all_displays().is_empty());
        assert!(
            screen
                .get_display_matching(&Bounds {
                    x: 0,
                    y: 0,
                    width: 800,
                    height: 600,
                })
                .is_none()
        );
        assert!(screen.get_display_nearest_point(0, 0).is_none());
        assert_eq!(screen.suggest_centered_position(800, 600), None);
    }

    #[test]
    fn default_screen_centers_like_a_sane_fallback() {
        let screen = Screen::default();
        assert_eq!(screen.get_all_displays().len(), 1);
        assert_eq!(screen.suggest_centered_position(800, 600), Some((112, 84)));
    }

    #[test]
    fn centered_position_uses_real_primary_metrics() {
        let screen = two_monitors();
        // Centers on the primary (first) display, not the larger secondary.
        assert_eq!(screen.suggest_centered_position(800, 600), Some((560, 240)));
    }
}
