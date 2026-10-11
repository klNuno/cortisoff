//! Whether a window fills its display, from rectangles alone.
//!
//! macOS has no maximised state to ask for: a zoomed window stops at the
//! menu bar and the Dock, and a native fullscreen window covers the whole
//! display. Both mean "an app is fullscreen" to the person using it, which is
//! what `app.maximized` reports; only the second is `app.fullscreen`.

/// A rectangle in CoreGraphics global coordinates: points, origin at the top
/// left of the main display, y growing downwards. `kCGWindowBounds` and
/// `CGDisplayBounds` both use it, so they compare directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    fn right(&self) -> f64 {
        self.x + self.w
    }

    fn bottom(&self) -> f64 {
        self.y + self.h
    }

    fn overlap(&self, other: &Rect) -> (f64, f64) {
        let w = self.right().min(other.right()) - self.x.max(other.x);
        let h = self.bottom().min(other.bottom()) - self.y.max(other.y);
        (w.max(0.0), h.max(0.0))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fit {
    pub maximized: bool,
    pub fullscreen: bool,
}

/// Room left for a Dock on the side, in points. A side Dock at its largest
/// magnified size is about this wide.
const SIDE_SLACK: f64 = 120.0;
/// Room left for the menu bar and a bottom Dock, in points. A window less
/// than this short of its display's height is zoomed; one shorter than that
/// is a large window the user placed, and treating it as maximised would
/// switch Night light off under a document.
const HEIGHT_SLACK: f64 = 160.0;
/// Rounding between window and display bounds, in points.
const EDGE: f64 = 1.0;

/// How `window` sits on the display it covers most of, among `screens`.
pub fn fit(window: Rect, screens: &[Rect]) -> Fit {
    let best = screens
        .iter()
        .map(|s| {
            let (w, h) = window.overlap(s);
            (s, w, h)
        })
        .filter(|(_, w, h)| *w > 0.0 && *h > 0.0)
        .max_by(|(_, aw, ah), (_, bw, bh)| (aw * ah).total_cmp(&(bw * bh)));
    let Some((screen, covered_w, covered_h)) = best else {
        return Fit::default();
    };
    let fullscreen = window.x <= screen.x + EDGE
        && window.y <= screen.y + EDGE
        && window.right() >= screen.right() - EDGE
        && window.bottom() >= screen.bottom() - EDGE;
    let zoomed = covered_w >= screen.w - SIDE_SLACK && covered_h >= screen.h - HEIGHT_SLACK;
    Fit {
        maximized: fullscreen || zoomed,
        fullscreen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    fn main_display() -> Rect {
        rect(0.0, 0.0, 2560.0, 1440.0)
    }

    #[test]
    fn a_native_fullscreen_window_is_both() {
        let f = fit(main_display(), &[main_display()]);
        assert_eq!(
            f,
            Fit {
                maximized: true,
                fullscreen: true
            }
        );
    }

    #[test]
    fn a_zoomed_window_under_the_menu_bar_and_above_the_dock_is_maximised() {
        let f = fit(rect(0.0, 25.0, 2560.0, 1340.0), &[main_display()]);
        assert!(f.maximized && !f.fullscreen, "{f:?}");
    }

    #[test]
    fn a_zoomed_window_beside_a_side_dock_is_maximised() {
        let f = fit(rect(70.0, 25.0, 2490.0, 1415.0), &[main_display()]);
        assert!(f.maximized && !f.fullscreen, "{f:?}");
    }

    #[test]
    fn a_large_window_the_user_placed_is_not() {
        let f = fit(rect(80.0, 60.0, 2400.0, 1200.0), &[main_display()]);
        assert_eq!(f, Fit::default());
    }

    /// The second display sits to the right of the main one, and a window
    /// filling it is judged against it, not against the main display.
    #[test]
    fn a_window_is_judged_against_the_display_it_is_on() {
        let second = rect(2560.0, -200.0, 1920.0, 1080.0);
        let screens = [main_display(), second];
        let f = fit(second, &screens);
        assert!(f.fullscreen);
        let f = fit(rect(2560.0, -175.0, 1920.0, 1055.0), &screens);
        assert!(f.maximized && !f.fullscreen);
        let small = fit(rect(2600.0, 0.0, 800.0, 600.0), &screens);
        assert_eq!(small, Fit::default());
    }

    /// A window spilling over two displays is judged on the one holding most
    /// of it, and covering one of them only partly is not filling it.
    #[test]
    fn a_window_across_two_displays_counts_where_most_of_it_is() {
        let second = rect(2560.0, 0.0, 2560.0, 1440.0);
        let f = fit(rect(1280.0, 0.0, 2560.0, 1440.0), &[main_display(), second]);
        assert_eq!(f, Fit::default());
    }

    #[test]
    fn a_window_off_every_display_is_neither() {
        let f = fit(rect(9000.0, 9000.0, 100.0, 100.0), &[main_display()]);
        assert_eq!(f, Fit::default());
        assert_eq!(fit(main_display(), &[]), Fit::default());
    }
}
