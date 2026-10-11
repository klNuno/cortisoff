//! The X11 connection and the properties every part of the host reads.

use cortisoff_host::AppRef;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::screensaver::ConnectionExt as _;
use x11rb::protocol::xinput::ConnectionExt as _;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::procfs;

x11rb::atom_manager! {
    pub Atoms: AtomsCookie {
        _NET_SUPPORTED,
        _NET_ACTIVE_WINDOW,
        _NET_CLIENT_LIST_STACKING,
        _NET_WM_PID,
        _NET_WM_NAME,
        _NET_WM_STATE,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_HIDDEN,
        _NET_CLOSE_WINDOW,
        WM_CHANGE_STATE,
        UTF8_STRING,
        CLIPBOARD,
        TARGETS,
        TEXT,
        INCR,
        CORTISOFF_CLIPBOARD,
    }
}

pub struct Display {
    pub conn: RustConnection,
    pub root: Window,
    pub atoms: Atoms,
    /// RandR 1.2 or later: CRTCs, their gamma and their modes.
    pub randr: bool,
    pub screensaver: bool,
    pub xtest: bool,
    /// XInput 2.1 or later: raw input events on the root, whatever grabs
    /// another client holds. What ends an idle period without polling.
    pub xinput: bool,
    /// The X server is XWayland. It reports outputs and accepts gamma ramps
    /// and modes, and the compositor never passes them to the monitor.
    pub xwayland: bool,
    /// `_NET_SUPPORTED` as the window manager published it at connect time.
    pub supported: Vec<Atom>,
    /// The screen's size in pixels and millimetres at connect time, for
    /// keeping the same density when a mode change resizes the screen.
    pub screen_px: (u16, u16),
    pub screen_mm: (u16, u16),
}

impl Display {
    pub fn connect() -> Result<Self, String> {
        let (conn, screen) =
            RustConnection::connect(None).map_err(|e| match std::env::var("DISPLAY") {
                Ok(d) if !d.is_empty() => {
                    format!("could not reach the X server at DISPLAY={d}: {e}")
                }
                _ => {
                    "DISPLAY is not set, so this session has no X server and no XWayland".to_owned()
                }
            })?;
        let (root, screen_px, screen_mm) = {
            let s = &conn.setup().roots[screen];
            (
                s.root,
                (s.width_in_pixels, s.height_in_pixels),
                (s.width_in_millimeters, s.height_in_millimeters),
            )
        };
        let atoms = Atoms::new(&conn)
            .map_err(|e| e.to_string())
            .and_then(|cookie| cookie.reply().map_err(|e| e.to_string()))
            .map_err(|e| format!("the X server would not name its atoms: {e}"))?;

        let randr = conn
            .randr_query_version(1, 5)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|v| (v.major_version, v.minor_version) >= (1, 2));
        let screensaver = conn
            .screensaver_query_version(1, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some();
        let xtest = conn
            .xtest_get_version(2, 2)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some();
        let xinput = conn
            .xinput_xi_query_version(2, 2)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|v| (v.major_version, v.minor_version) >= (2, 1));
        let xwayland = conn
            .extension_information("XWAYLAND")
            .ok()
            .flatten()
            .is_some();

        let mut display = Self {
            conn,
            root,
            atoms,
            randr,
            screensaver,
            xtest,
            xinput,
            xwayland,
            supported: Vec::new(),
            screen_px,
            screen_mm,
        };
        display.supported = display.atoms_of(root, display.atoms._NET_SUPPORTED);
        // Older XWayland has no extension of its own but names its outputs.
        if !display.xwayland && display.randr {
            display.xwayland = crate::randr::heads(&display)
                .iter()
                .any(|h| h.name.starts_with("XWAYLAND"));
        }
        Ok(display)
    }

    pub fn supports(&self, atom: Atom) -> bool {
        self.supported.contains(&atom)
    }

    /// The first 32 bit item of a property.
    pub fn prop32(&self, window: Window, property: Atom, kind: impl Into<Atom>) -> Option<u32> {
        self.conn
            .get_property(false, window, property, kind, 0, 1)
            .ok()?
            .reply()
            .ok()?
            .value32()?
            .next()
    }

    pub fn list32(&self, window: Window, property: Atom, kind: impl Into<Atom>) -> Vec<u32> {
        self.conn
            .get_property(false, window, property, kind, 0, 4096)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().map(Iterator::collect))
            .unwrap_or_default()
    }

    fn atoms_of(&self, window: Window, property: Atom) -> Vec<Atom> {
        self.list32(window, property, AtomEnum::ATOM)
    }

    /// `None` when nothing has the focus, or a window manager without EWMH.
    pub fn active_window(&self) -> Option<Window> {
        self.prop32(self.root, self.atoms._NET_ACTIVE_WINDOW, AtomEnum::WINDOW)
            .filter(|w| *w != x11rb::NONE)
    }

    /// Every managed window, bottom of the stack first.
    pub fn stacking(&self) -> Vec<Window> {
        self.list32(
            self.root,
            self.atoms._NET_CLIENT_LIST_STACKING,
            AtomEnum::WINDOW,
        )
    }

    pub fn pid(&self, window: Window) -> Option<u32> {
        self.prop32(window, self.atoms._NET_WM_PID, AtomEnum::CARDINAL)
            .filter(|p| *p != 0)
    }

    /// `_NET_WM_NAME` in UTF-8, or the legacy `WM_NAME` in Latin-1.
    pub fn title(&self, window: Window) -> Option<String> {
        let text = |property: Atom, kind: Atom| {
            let reply = self
                .conn
                .get_property(false, window, property, kind, 0, 1024)
                .ok()?
                .reply()
                .ok()?;
            (reply.format == 8 && !reply.value.is_empty()).then_some(reply)
        };
        if let Some(reply) = text(self.atoms._NET_WM_NAME, self.atoms.UTF8_STRING) {
            return Some(String::from_utf8_lossy(&reply.value).into_owned());
        }
        let reply = text(AtomEnum::WM_NAME.into(), AtomEnum::ANY.into())?;
        Some(if reply.type_ == self.atoms.UTF8_STRING {
            String::from_utf8_lossy(&reply.value).into_owned()
        } else {
            reply.value.iter().map(|b| char::from(*b)).collect()
        })
    }

    /// `WM_CLASS`, class part: what names an application with no pid.
    fn class(&self, window: Window) -> Option<String> {
        let reply = self
            .conn
            .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
            .ok()?
            .reply()
            .ok()?;
        let mut parts = reply.value.split(|b| *b == 0).filter(|p| !p.is_empty());
        let instance = parts.next();
        let class = parts.next().or(instance)?;
        Some(String::from_utf8_lossy(class).into_owned())
    }

    /// The application behind a window. A client that sets no
    /// `_NET_WM_PID`, an old toolkit or a remote one, is named by its
    /// `WM_CLASS` with a pid of zero rather than not named at all.
    pub fn app(&self, window: Window) -> Option<AppRef> {
        let title = self.title(window);
        match self.pid(window) {
            Some(pid) => Some(AppRef {
                exe: procfs::exe(pid).or_else(|| self.class(window))?,
                title,
                pid,
                started_at_ms: procfs::started_at_ms(pid).unwrap_or(0),
            }),
            None => Some(AppRef {
                exe: self.class(window)?,
                title,
                pid: 0,
                started_at_ms: 0,
            }),
        }
    }

    pub fn window_state(&self, window: Window) -> State {
        let states = self.atoms_of(window, self.atoms._NET_WM_STATE);
        State::from_atoms(
            &states,
            self.atoms._NET_WM_STATE_FULLSCREEN,
            self.atoms._NET_WM_STATE_MAXIMIZED_VERT,
            self.atoms._NET_WM_STATE_MAXIMIZED_HORZ,
        )
    }

    /// Milliseconds since the last keyboard or mouse input on this server.
    pub fn idle_ms(&self) -> Option<u32> {
        if !self.screensaver {
            return None;
        }
        self.conn
            .screensaver_query_info(self.root)
            .ok()?
            .reply()
            .ok()
            .map(|info| info.ms_since_user_input)
    }

    /// Replace this client's event mask on a window. Each client has its own
    /// mask, so this never disturbs what the window's owner selected. A
    /// window that died in the meantime answers with an error event the
    /// loop ignores.
    pub fn select(&self, window: Window, mask: EventMask) {
        let _ = self
            .conn
            .change_window_attributes(window, &ChangeWindowAttributesAux::new().event_mask(mask));
    }

    pub fn flush(&self) {
        let _ = self.conn.flush();
    }
}

/// What `_NET_WM_STATE` says about a window, in the host's two words.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct State {
    pub fullscreen: bool,
    /// Fills its monitor: maximised both ways, or fullscreen. What a person
    /// means by "an app is fullscreen", as on Windows.
    pub maximized: bool,
}

impl State {
    pub fn from_atoms(states: &[Atom], fullscreen: Atom, vert: Atom, horz: Atom) -> Self {
        let has = |a: Atom| states.contains(&a);
        let fullscreen = has(fullscreen);
        Self {
            fullscreen,
            maximized: fullscreen || (has(vert) && has(horz)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: Atom = 301;
    const VERT: Atom = 302;
    const HORZ: Atom = 303;
    const HIDDEN: Atom = 304;

    #[test]
    fn maximised_means_both_ways_or_fullscreen() {
        let read = |s: &[Atom]| State::from_atoms(s, FS, VERT, HORZ);
        assert_eq!(read(&[]), State::default());
        assert_eq!(
            read(&[VERT]),
            State::default(),
            "half a maximise is a tiled window"
        );
        assert_eq!(
            read(&[HORZ, VERT, HIDDEN]),
            State {
                fullscreen: false,
                maximized: true
            }
        );
        assert_eq!(
            read(&[FS]),
            State {
                fullscreen: true,
                maximized: true
            }
        );
    }
}
