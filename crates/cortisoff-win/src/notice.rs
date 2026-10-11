//! Notifications, as balloons on a notification area icon. Windows 10 and
//! 11 show a balloon as a toast, with no WinRT and no registered app id.

use std::mem::size_of;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO,
    NIIF_NOSOUND, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW,
    NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

/// The icon added for notifications when there is no tray icon. The tray's
/// own icon is 1.
pub const ICON_ID: u32 = 2;

/// Show a balloon on icon `id` of `window`, which must already be there.
pub fn show(window: HWND, id: u32, title: &str, body: &str) -> Result<(), String> {
    let (title, body) = text(title, body)?;
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: id,
        uFlags: NIF_INFO,
        // Silent: cortisoff never plays a sound, and a plugin that wants
        // one can ask for it some other way.
        dwInfoFlags: NIIF_INFO | NIIF_NOSOUND,
        ..Default::default()
    };
    fill(&mut data.szInfoTitle, &title);
    fill(&mut data.szInfo, &body);
    if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
        Ok(())
    } else {
        Err(
            "the notification area did not take the notification; Explorer may not be running"
                .to_owned(),
        )
    }
}

/// An icon that exists only so a notification has somewhere to come from,
/// for a host with no tray icon. Removed when its last balloon is done, or
/// when this is dropped.
///
/// A plain icon, not one added with `NIS_HIDDEN`: nothing documents that a
/// hidden icon may show a balloon, and a notification that silently never
/// appears is the worst outcome. Windows 11 puts a new icon in the overflow
/// area anyway, so the taskbar does not change.
pub struct Notice {
    window: HWND,
    icon: HICON,
    owned: bool,
    /// Balloons shown and not yet reported done. The icon goes when this
    /// reaches zero, so the end of one balloon never takes a later one with
    /// it.
    pending: u32,
}

impl Notice {
    pub fn add(window: HWND) -> Result<Notice, String> {
        let (icon, owned) = crate::tray::load_icon(crate::tray::ICO);
        let notice = Notice {
            window,
            icon,
            owned,
            pending: 0,
        };
        let mut data = notice.data();
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = crate::tray::TRAY_MESSAGE;
        data.hIcon = icon;
        fill(&mut data.szTip, "cortisoff");
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            // Dropping it would delete an icon that was never added, which
            // is harmless, and still frees the picture.
            return Err(
                "the notification area is not there; Explorer may not be running".to_owned(),
            );
        }
        // Version 4 is what puts the icon id in the high word of every
        // message, which is how a balloon's end is told apart from the
        // tray's clicks.
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) };
        Ok(notice)
    }

    pub fn show(&mut self, title: &str, body: &str) -> Result<(), String> {
        show(self.window, ICON_ID, title, body)?;
        self.pending += 1;
        Ok(())
    }

    /// One balloon timed out or was clicked. True when none is left and
    /// the icon can go.
    pub fn finished(&mut self) -> bool {
        self.pending = self.pending.saturating_sub(1);
        self.idle()
    }

    /// No balloon of this icon is on screen.
    pub fn idle(&self) -> bool {
        self.pending == 0
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.window,
            uID: ICON_ID,
            ..Default::default()
        }
    }
}

impl Drop for Notice {
    fn drop(&mut self) {
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data()) };
        if self.owned {
            let _ = unsafe { DestroyIcon(self.icon) };
        }
    }
}

/// The title and the body Windows will show. A balloon with an empty body
/// is not shown at all, it is how one is taken down, so a title alone
/// becomes the body.
fn text(title: &str, body: &str) -> Result<(String, String), String> {
    match (title.trim().is_empty(), body.trim().is_empty()) {
        (true, true) => Err("a notification needs a title or a body".to_owned()),
        (false, true) => Ok((String::new(), title.to_owned())),
        _ => Ok((title.to_owned(), body.to_owned())),
    }
}

/// Copy `text` into a fixed UTF-16 field, cut to fit with room for the
/// terminating zero, and never between the two halves of a surrogate pair.
fn fill(field: &mut [u16], text: &str) {
    let room = field.len().saturating_sub(1);
    let mut used = 0;
    let mut buffer = [0u16; 2];
    for c in text.chars() {
        let units = c.encode_utf16(&mut buffer);
        if used + units.len() > room {
            break;
        }
        field[used..used + units.len()].copy_from_slice(units);
        used += units.len();
    }
    field[used..].fill(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_alone_becomes_the_body() {
        assert_eq!(
            text("Saved", "").unwrap(),
            (String::new(), "Saved".to_owned())
        );
        assert_eq!(
            text("Saved", "  ").unwrap(),
            (String::new(), "Saved".to_owned())
        );
        assert_eq!(
            text("", "Body").unwrap(),
            (String::new(), "Body".to_owned())
        );
        assert_eq!(text("T", "B").unwrap(), ("T".to_owned(), "B".to_owned()));
        assert!(text(" ", "").is_err());
    }

    #[test]
    fn a_field_is_cut_to_fit_with_its_zero() {
        let mut field = [0xFFFFu16; 6];
        fill(&mut field, "abcdefgh");
        assert_eq!(String::from_utf16_lossy(&field[..5]), "abcde");
        assert_eq!(field[5], 0);

        let mut field = [0xFFFFu16; 6];
        fill(&mut field, "ab");
        assert_eq!(&field, &[97, 98, 0, 0, 0, 0]);
    }

    #[test]
    fn a_surrogate_pair_is_never_split() {
        // Four units of room: "abc" takes three, the face needs two.
        let mut field = [0xFFFFu16; 5];
        fill(&mut field, "abc\u{1F600}");
        assert_eq!(&field, &[97, 98, 99, 0, 0]);

        let mut field = [0xFFFFu16; 5];
        fill(&mut field, "ab\u{1F600}");
        assert_eq!(&field, &[97, 98, 0xD83D, 0xDE00, 0]);
    }

    #[test]
    fn the_real_fields_take_their_documented_lengths() {
        let mut data = NOTIFYICONDATAW::default();
        let long = "x".repeat(1000);
        fill(&mut data.szInfoTitle, &long);
        fill(&mut data.szInfo, &long);
        assert_eq!(data.szInfoTitle.iter().position(|&u| u == 0), Some(63));
        assert_eq!(data.szInfo.iter().position(|&u| u == 0), Some(255));
    }

    #[test]
    fn a_balloon_count_only_lets_go_at_zero() {
        let mut notice = Notice {
            window: HWND::default(),
            icon: HICON::default(),
            owned: false,
            pending: 2,
        };
        assert!(!notice.finished());
        assert!(notice.finished());
        assert!(notice.finished());
        // Never dropped: the drop calls Shell_NotifyIcon, and a test never
        // touches the notification area.
        std::mem::forget(notice);
    }
}
