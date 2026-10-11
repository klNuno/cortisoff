//! The windows the window manager manages, and what EWMH lets a client ask
//! of them. Every action is a request to the window manager, which may say
//! no; none of them touches a window directly.

use cortisoff_host::{WindowAction, WindowInfo};
use x11rb::protocol::xproto::{Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask};

use crate::x11::Display;

/// `_NET_WM_STATE` actions and `WM_CHANGE_STATE`'s iconic state.
const REMOVE: u32 = 0;
const ADD: u32 = 1;
const ICONIC: u32 = 3;
/// The request comes from a pager or a tool acting for the user, which is
/// what lets a focus-stealing guard accept it.
const SOURCE_PAGER: u32 = 2;

/// Front to back, as the contract asks: `_NET_CLIENT_LIST_STACKING` is
/// bottom first.
pub fn windows(x: &Display) -> Vec<WindowInfo> {
    let active = x.active_window();
    x.stacking()
        .into_iter()
        .rev()
        .map(|window| {
            let app = x.app(window);
            WindowInfo {
                id: u64::from(window),
                title: app
                    .as_ref()
                    .and_then(|a| a.title.clone())
                    .unwrap_or_default(),
                exe: app.as_ref().map(|a| a.exe.clone()).unwrap_or_default(),
                pid: app.map_or(0, |a| a.pid),
                focused: Some(window) == active,
            }
        })
        .collect()
}

pub fn act(x: &Display, id: u64, action: WindowAction) -> Result<(), String> {
    let window = u32::try_from(id)
        .ok()
        .filter(|w| x.stacking().contains(w))
        .ok_or_else(|| format!("no open window has the id {id}"))?;
    let a = &x.atoms;
    let maximized = [
        a._NET_WM_STATE_MAXIMIZED_VERT,
        a._NET_WM_STATE_MAXIMIZED_HORZ,
    ];
    match action {
        WindowAction::Activate => activate(x, window),
        WindowAction::Close => message(
            x,
            window,
            a._NET_CLOSE_WINDOW,
            [x11rb::CURRENT_TIME, SOURCE_PAGER, 0, 0, 0],
        ),
        WindowAction::Minimize => message(x, window, a.WM_CHANGE_STATE, [ICONIC, 0, 0, 0, 0]),
        WindowAction::Maximize => message(
            x,
            window,
            a._NET_WM_STATE,
            [ADD, maximized[0], maximized[1], SOURCE_PAGER, 0],
        ),
        // What restore means on Windows: a minimised window comes back, a
        // maximised one goes back to its own size.
        WindowAction::Restore => {
            let states: Vec<Atom> = x.list32(window, a._NET_WM_STATE, AtomEnum::ATOM);
            if states.contains(&a._NET_WM_STATE_HIDDEN) {
                activate(x, window)
            } else {
                message(
                    x,
                    window,
                    a._NET_WM_STATE,
                    [REMOVE, maximized[0], maximized[1], SOURCE_PAGER, 0],
                )
            }
        }
    }
}

fn activate(x: &Display, window: u32) -> Result<(), String> {
    message(
        x,
        window,
        x.atoms._NET_ACTIVE_WINDOW,
        [SOURCE_PAGER, x11rb::CURRENT_TIME, 0, 0, 0],
    )
}

/// A client message to the root, where the window manager listens for it.
fn message(x: &Display, window: u32, kind: Atom, data: [u32; 5]) -> Result<(), String> {
    let event = ClientMessageEvent::new(32, window, kind, data);
    x.conn
        .send_event(
            false,
            x.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )
        .map_err(|e| format!("the X connection failed: {e}"))?
        .check()
        .map_err(|e| format!("the X server refused the request: {e}"))
}
