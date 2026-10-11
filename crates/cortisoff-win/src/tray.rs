//! The notification area icon.
//!
//! The one visible trace of a daemon whose whole job is invisible, and the way
//! a person opens the dashboard or quits without a terminal. Left click opens
//! the window, right click gives the menu.

use std::mem::size_of;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, DestroyIcon, DestroyMenu,
    GetSystemMetrics, LoadIconW, RegisterWindowMessageW, SetForegroundWindow, TrackPopupMenu,
    HICON, IDI_APPLICATION, LR_DEFAULTCOLOR, MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED,
    SM_CXSMICON, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP,
};

/// What the icon sends the window when someone touches it.
pub const TRAY_MESSAGE: u32 = WM_APP + 1;

pub const MENU_OPEN: usize = 1;
pub const MENU_AUTOSTART: usize = 2;
pub const MENU_QUIT: usize = 3;

const ICON_ID: u32 = 1;

/// The same picture the dashboard wears, so the two read as one application.
pub(crate) static ICO: &[u8] = include_bytes!("../../../assets/icon.ico");
static ACTIVE_ICO: &[u8] = include_bytes!("../../../assets/active.ico");

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Explorer broadcasts this when it restarts, and every icon has to be added
/// again or it is gone until the daemon restarts.
pub fn taskbar_created() -> u32 {
    TASKBAR_CREATED.load(Ordering::Relaxed)
}

pub struct Tray {
    window: HWND,
    icon: HICON,
    owned: bool,
    active: bool,
}

impl Tray {
    /// Always a tray, even when Explorer is not up yet at logon: the icon is
    /// added again on `TaskbarCreated`.
    pub fn add(window: HWND) -> Tray {
        let message = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        TASKBAR_CREATED.store(message, Ordering::Relaxed);
        let (icon, owned) = load_icon(ICO);
        let tray = Tray {
            window,
            icon,
            owned,
            active: false,
        };
        tray.show();
        tray
    }

    pub fn set_active(&mut self, active: bool) {
        if self.active == active {
            return;
        }
        let (icon, owned) = load_icon(if active { ACTIVE_ICO } else { ICO });
        let mut data = self.data();
        data.uFlags = NIF_ICON;
        data.hIcon = icon;
        // Retain the desired state across a shell restart: show() reuses it.
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
        if self.owned {
            let _ = unsafe { DestroyIcon(self.icon) };
        }
        self.icon = icon;
        self.owned = owned;
        self.active = active;
    }

    pub fn show(&self) -> bool {
        let mut data = self.data();
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = TRAY_MESSAGE;
        data.hIcon = self.icon;
        for (slot, unit) in data.szTip.iter_mut().zip("cortisoff".encode_utf16()) {
            *slot = unit;
        }
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool();
        // Version 4 is what puts the click position in the message and sends
        // a plain select on left click.
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) };
        added
    }

    /// The right click menu. Blocks in the menu's own loop until it closes and
    /// answers which entry was picked, if any.
    pub fn menu(&self, x: i32, y: i32, autostart: bool) -> Option<usize> {
        unsafe {
            let menu = CreatePopupMenu().ok()?;
            let check = if autostart { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("Open cortisoff"));
            let _ = AppendMenuW(
                menu,
                MF_STRING | check,
                MENU_AUTOSTART,
                w!("Start when I log on"),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                MENU_QUIT,
                w!("Quit and put the screen back"),
            );
            // Without it the menu stays open when the person clicks elsewhere.
            let _ = SetForegroundWindow(self.window);
            let picked = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                x,
                y,
                0,
                self.window,
                None,
            );
            let _ = DestroyMenu(menu);
            (picked.0 > 0).then_some(picked.0 as usize)
        }
    }

    /// A notification balloon on this icon.
    pub fn notify(&self, title: &str, body: &str) -> Result<(), String> {
        crate::notice::show(self.window, ICON_ID, title, body)
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

impl Drop for Tray {
    fn drop(&mut self) {
        let data = self.data();
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        if self.owned {
            let _ = unsafe { DestroyIcon(self.icon) };
        }
    }
}

/// The window: the same executable as the daemon, started with no argument.
/// It raises an already open window rather than opening a second one.
pub fn open_dashboard() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(&exe)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not start {}: {e}", exe.display()))
}

pub(crate) fn load_icon(ico: &[u8]) -> (HICON, bool) {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    if let Some(bits) = ico_image(ico, size as u32) {
        if let Ok(icon) = unsafe {
            CreateIconFromResourceEx(bits, true, 0x0003_0000, size, size, LR_DEFAULTCOLOR)
        } {
            return (icon, true);
        }
    }
    let fallback = unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default();
    (fallback, false)
}

/// The image data of the `.ico` entry that best fits `size` pixels: the
/// smallest one at least that big, or the biggest there is. Windows scales
/// down cleanly and scales up badly.
pub fn ico_image(ico: &[u8], size: u32) -> Option<&[u8]> {
    let u16_at = |at: usize| {
        ico.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |at: usize| {
        ico.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if u16_at(2)? != 1 {
        return None;
    }
    let count = usize::from(u16_at(4)?);
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let at = 6 + i * 16;
        // Zero means 256, the one size that does not fit in a byte.
        let width = match *ico.get(at)? {
            0 => 256,
            w => u32::from(w),
        };
        let len = u32_at(at + 8)? as usize;
        let offset = u32_at(at + 12)? as usize;
        entries.push((width, ico.get(offset..offset.checked_add(len)?)?));
    }
    let fits = entries
        .iter()
        .filter(|(w, _)| *w >= size)
        .min_by_key(|(w, _)| *w);
    let best = fits.or_else(|| entries.iter().max_by_key(|(w, _)| *w))?;
    Some(best.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_icon_has_an_image_for_every_tray_size() {
        for size in [16, 20, 24, 32] {
            let image = ico_image(ICO, size).expect("an image");
            assert!(image.len() > 40, "{size} px is {} bytes", image.len());
        }
    }

    #[test]
    fn a_file_that_is_not_an_icon_gives_nothing() {
        assert!(ico_image(b"not an icon at all", 16).is_none());
        assert!(ico_image(&[], 16).is_none());
    }
}
