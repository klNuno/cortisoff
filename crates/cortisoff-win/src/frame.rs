//! The window's frame, answered by the system.
//!
//! The window draws its own title bar, but a press on it still has to do what
//! it does on any framed window. Moving the window from a drawn frame means
//! asking the system to start a move after the press reached the app, which
//! is one drawn frame late: on a slow frame the button is already up by then,
//! and the move never starts. So the window tells the system which of its
//! pixels are the caption and which are its edges, through `WM_NCHITTEST`,
//! and the system moves, snaps, maximises on a double click and resizes it
//! itself, from the press on.

use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, IsZoomed, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTLEFT, HTRIGHT,
    HTTOP, HTTOPLEFT, HTTOPRIGHT, WM_NCDESTROY, WM_NCHITTEST,
};

/// A rectangle in client pixels: left, top, right, bottom, the last two
/// excluded.
pub type Area = [i32; 4];

/// What the window's last drawn frame said about its own pixels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Regions {
    /// Where a press moves the window.
    pub caption: Vec<Area>,
    /// Holes in the caption: the widgets drawn over it, which take their own
    /// presses.
    pub buttons: Vec<Area>,
    /// How far in from the window's edge a press resizes it. Zero while it
    /// cannot be resized: maximised or fullscreen.
    pub edge: i32,
    /// How far along an edge a press still takes the corner.
    pub corner: i32,
}

static REGIONS: Mutex<Option<Regions>> = Mutex::new(None);

/// Tells this subclass apart from any other on the same window.
const SUBCLASS: usize = 0x636f_7274;

/// Takes over the hit test of `hwnd`, a window of this thread. False when the
/// system refused: the window then keeps moving through its drawn frame.
pub fn adopt(hwnd: isize) -> bool {
    let hwnd = HWND(hwnd as *mut _);
    // SAFETY: `hwnd` is the caller's live window, and `subclass` removes
    // itself when the window goes.
    unsafe { SetWindowSubclass(hwnd, Some(subclass), SUBCLASS, 0).as_bool() }
}

/// What the latest drawn frame says. Called once per frame; cheap when
/// nothing moved.
pub fn set(regions: Regions) {
    if let Ok(mut slot) = REGIONS.lock() {
        if slot.as_ref() != Some(&regions) {
            *slot = Some(regions);
        }
    }
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_NCHITTEST => {
            // The point is in screen coordinates, two signed words.
            let screen = POINT {
                x: i32::from(lparam.0 as u16 as i16),
                y: i32::from((lparam.0 >> 16) as u16 as i16),
            };
            if let Some(code) = answer(hwnd, screen) {
                return LRESULT(code as isize);
            }
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS);
        }
        _ => {}
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

/// What a press at `screen` does, or `None` for the app's own client area.
fn answer(hwnd: HWND, screen: POINT) -> Option<u32> {
    // Never wait: the frame that sets the regions runs on this same thread.
    let slot = REGIONS.try_lock().ok()?;
    let regions = slot.as_ref()?;
    let mut p = screen;
    let mut client = RECT::default();
    // SAFETY: plain reads of a live window's geometry.
    unsafe {
        if !ScreenToClient(hwnd, &mut p).as_bool() || GetClientRect(hwnd, &mut client).is_err() {
            return None;
        }
        let resizable = !IsZoomed(hwnd).as_bool();
        hit(
            regions,
            (p.x, p.y),
            (client.right, client.bottom),
            resizable,
        )
    }
}

/// The hit test itself: `p` in client pixels, `size` the client area's.
fn hit(r: &Regions, (x, y): (i32, i32), (w, h): (i32, i32), resizable: bool) -> Option<u32> {
    if resizable && r.edge > 0 {
        let (left, right) = (x < r.edge, x >= w - r.edge);
        let (top, bottom) = (y < r.edge, y >= h - r.edge);
        let (near_left, near_right) = (x < r.corner, x >= w - r.corner);
        let (near_top, near_bottom) = (y < r.corner, y >= h - r.corner);
        let code = if (top && near_left) || (left && near_top) {
            Some(HTTOPLEFT)
        } else if (top && near_right) || (right && near_top) {
            Some(HTTOPRIGHT)
        } else if (bottom && near_left) || (left && near_bottom) {
            Some(HTBOTTOMLEFT)
        } else if (bottom && near_right) || (right && near_bottom) {
            Some(HTBOTTOMRIGHT)
        } else if top {
            Some(HTTOP)
        } else if bottom {
            Some(HTBOTTOM)
        } else if left {
            Some(HTLEFT)
        } else if right {
            Some(HTRIGHT)
        } else {
            None
        };
        if code.is_some() {
            return code;
        }
    }
    let inside = |a: &Area| x >= a[0] && y >= a[1] && x < a[2] && y < a[3];
    (r.caption.iter().any(inside) && !r.buttons.iter().any(inside)).then_some(HTCAPTION)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regions() -> Regions {
        Regions {
            caption: vec![[0, 0, 248, 32], [248, 0, 1000, 32]],
            buttons: vec![[862, 0, 1000, 32], [10, 14, 238, 50]],
            edge: 5,
            corner: 12,
        }
    }

    #[test]
    fn a_press_on_the_band_moves_the_window_but_not_on_its_buttons() {
        let r = regions();
        let size = (1000, 700);
        assert_eq!(hit(&r, (500, 20), size, true), Some(HTCAPTION));
        assert_eq!(hit(&r, (5, 10), size, true), Some(HTCAPTION));
        // The close button and the switch drawn over the sidebar's band.
        assert_eq!(hit(&r, (980, 20), size, true), None);
        assert_eq!(hit(&r, (100, 20), size, true), None);
        // Below the band, the page.
        assert_eq!(hit(&r, (500, 40), size, true), None);
    }

    #[test]
    fn the_edges_resize_and_their_ends_take_the_corners() {
        let r = regions();
        let size = (1000, 700);
        assert_eq!(hit(&r, (0, 300), size, true), Some(HTLEFT));
        assert_eq!(hit(&r, (999, 300), size, true), Some(HTRIGHT));
        assert_eq!(hit(&r, (500, 2), size, true), Some(HTTOP));
        assert_eq!(hit(&r, (500, 697), size, true), Some(HTBOTTOM));
        assert_eq!(hit(&r, (2, 10), size, true), Some(HTTOPLEFT));
        assert_eq!(hit(&r, (10, 2), size, true), Some(HTTOPLEFT));
        assert_eq!(hit(&r, (996, 690), size, true), Some(HTBOTTOMRIGHT));
        assert_eq!(hit(&r, (990, 696), size, true), Some(HTBOTTOMRIGHT));
        // Over the close button the top edge still resizes, as on Windows 11.
        assert_eq!(hit(&r, (900, 1), size, true), Some(HTTOP));
        // The row Windows keeps above the client area for the shadow.
        assert_eq!(hit(&r, (500, -1), size, true), Some(HTTOP));
        // Inside the corner square, off both edges: the page.
        assert_eq!(hit(&r, (9, 690), size, true), None);
    }

    #[test]
    fn a_maximised_window_has_no_edges_but_keeps_its_band() {
        let r = regions();
        let size = (1000, 700);
        assert_eq!(hit(&r, (0, 300), size, false), None);
        assert_eq!(hit(&r, (500, 2), size, false), Some(HTCAPTION));
        let fixed = Regions {
            edge: 0,
            ..regions()
        };
        assert_eq!(hit(&fixed, (999, 300), size, true), None);
    }
}
