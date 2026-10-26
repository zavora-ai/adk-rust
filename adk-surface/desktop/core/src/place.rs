//! Where the panel opens (spec 3.3 §5.5): at the tray icon, inside the screen's work area;
//! and whether a click on the icon opens it or closes it.

use std::time::{Duration, Instant};

/// A point in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    /// From the left.
    pub x: i32,
    /// From the top.
    pub y: i32,
}

/// A size in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

/// A rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Its top-left corner.
    pub origin: Point,
    /// Its size.
    pub size: Size,
}

impl Rect {
    fn right(self) -> i64 {
        i64::from(self.origin.x) + i64::from(self.size.width)
    }

    fn bottom(self) -> i64 {
        i64::from(self.origin.y) + i64::from(self.size.height)
    }
}

/// The gap between the panel and the work area's edge, or the tray icon.
const GAP: i64 = 8;

/// Where a panel of `size` opens: centred under the tray icon `tray` when the icon is in the
/// top half of `work_area` (a menu bar), above it otherwise (a taskbar); at the top-right of
/// `work_area` without an icon position (Linux, whose tray reports no clicks). A panel no larger
/// than the work area always lies inside it.
///
/// # Example
///
/// ```
/// use adk_desktop_core::{Point, Rect, Size, place_panel};
///
/// let screen = Rect { origin: Point { x: 0, y: 25 }, size: Size { width: 1440, height: 875 } };
/// let icon = Rect { origin: Point { x: 1200, y: 0 }, size: Size { width: 24, height: 24 } };
/// let panel = Size { width: 420, height: 700 };
/// assert_eq!(place_panel(Some(icon), screen, panel), Point { x: 1002, y: 32 });
/// assert_eq!(place_panel(None, screen, panel), Point { x: 1012, y: 33 });
/// ```
pub fn place_panel(tray: Option<Rect>, work_area: Rect, size: Size) -> Point {
    let (width, height) = (i64::from(size.width), i64::from(size.height));
    let (x, y) = match tray {
        Some(icon) => {
            let centre = i64::from(icon.origin.x) + i64::from(icon.size.width) / 2;
            let middle = i64::from(work_area.origin.y) + i64::from(work_area.size.height) / 2;
            let y = if i64::from(icon.origin.y) < middle {
                icon.bottom() + GAP
            } else {
                i64::from(icon.origin.y) - GAP - height
            };
            (centre - width / 2, y)
        }
        None => (work_area.right() - GAP - width, i64::from(work_area.origin.y) + GAP),
    };
    Point {
        x: clamp(x, work_area.origin.x, work_area.right() - width),
        y: clamp(y, work_area.origin.y, work_area.bottom() - height),
    }
}

/// `value` within `low..=high`, or `low` when the panel is larger than the work area.
fn clamp(value: i64, low: i32, high: i64) -> i32 {
    let low = i64::from(low);
    let placed = if high < low { low } else { value.clamp(low, high) };
    i32::try_from(placed).unwrap_or(if placed < 0 { i32::MIN } else { i32::MAX })
}

/// What a click on the tray icon does to the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    /// Open it at the icon.
    Show,
    /// Close it.
    Hide,
    /// Nothing: this click already closed it.
    Nothing,
}

/// Opens and closes the panel from the tray icon (spec 3.3 §5.5: "a click on the tray icon").
///
/// The panel hides when it loses the focus. Pressing the tray icon takes the focus first, as on
/// Windows, where the taskbar takes it on mouse-down, so the panel is already hidden when the
/// click (mouse-up) arrives. A click within [`PanelToggle::WITHIN`] of a hide on focus loss is
/// the click that closed the panel, not a click to open it again.
///
/// # Example
///
/// ```
/// use std::time::{Duration, Instant};
/// use adk_desktop_core::{PanelToggle, Toggle};
///
/// let mut toggle = PanelToggle::default();
/// let pressed = Instant::now();
/// assert_eq!(toggle.click(pressed, false), Toggle::Show);
/// // The next press blurs the open panel, which hides; its release must not open it again.
/// toggle.blurred(pressed + Duration::from_secs(3));
/// assert_eq!(toggle.click(pressed + Duration::from_millis(3_120), false), Toggle::Nothing);
/// ```
#[derive(Debug, Default)]
pub struct PanelToggle {
    hidden_on_blur: Option<Instant>,
}

impl PanelToggle {
    /// How soon after a hide on focus loss a click still counts as the one that hid it.
    pub const WITHIN: Duration = Duration::from_millis(400);

    /// The panel, shown, lost the focus at `at` and hid.
    pub fn blurred(&mut self, at: Instant) {
        self.hidden_on_blur = Some(at);
    }

    /// A click on the icon at `at`, with the panel `visible` or not.
    pub fn click(&mut self, at: Instant, visible: bool) -> Toggle {
        let hid_it = self
            .hidden_on_blur
            .take()
            .is_some_and(|hidden| at.saturating_duration_since(hidden) <= Self::WITHIN);
        match (visible, hid_it) {
            (true, _) => Toggle::Hide,
            (false, true) => Toggle::Nothing,
            (false, false) => Toggle::Show,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_that_blurred_the_panel_closes_it_and_the_next_one_opens_it() {
        let mut toggle = PanelToggle::default();
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        // Without a blur first (macOS, or the menu's Show decisions) a click toggles.
        assert_eq!(toggle.click(at(0), false), Toggle::Show);
        assert_eq!(toggle.click(at(1_000), true), Toggle::Hide);
        assert_eq!(toggle.click(at(2_000), false), Toggle::Show);
        // Windows: mouse-down blurs and hides the panel, then mouse-up clicks.
        toggle.blurred(at(5_000));
        assert_eq!(toggle.click(at(5_150), false), Toggle::Nothing);
        // The blur is spent: the next click opens it.
        assert_eq!(toggle.click(at(5_400), false), Toggle::Show);
        // Clicking elsewhere hid it a while ago: a click on the icon opens it.
        toggle.blurred(at(9_000));
        assert_eq!(toggle.click(at(9_000) + PanelToggle::WITHIN * 2, false), Toggle::Show);
    }

    const PANEL: Size = Size { width: 420, height: 700 };

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect { origin: Point { x, y }, size: Size { width, height } }
    }

    #[test]
    fn under_a_menu_bar_above_a_taskbar_and_top_right_without_an_icon() {
        let mac = rect(0, 25, 1440, 875);
        assert_eq!(place_panel(Some(rect(1200, 0, 24, 24)), mac, PANEL), Point { x: 1002, y: 32 });
        let windows = rect(0, 0, 1920, 1040);
        assert_eq!(
            place_panel(Some(rect(1800, 1040, 24, 40)), windows, PANEL),
            Point { x: 1500, y: 332 }
        );
        assert_eq!(place_panel(None, windows, PANEL), Point { x: 1492, y: 8 });
    }

    #[test]
    fn an_icon_at_the_edge_keeps_the_panel_on_screen() {
        let screen = rect(0, 25, 1440, 875);
        assert_eq!(
            place_panel(Some(rect(1430, 0, 24, 24)), screen, PANEL),
            Point { x: 1020, y: 32 }
        );
        assert_eq!(place_panel(Some(rect(-10, 0, 24, 24)), screen, PANEL), Point { x: 0, y: 32 });
    }

    #[test]
    fn a_second_monitor_left_of_the_first() {
        let left = rect(-1920, 0, 1920, 1080);
        let placed = place_panel(None, left, PANEL);
        assert_eq!(placed, Point { x: -428, y: 8 });
    }
}
