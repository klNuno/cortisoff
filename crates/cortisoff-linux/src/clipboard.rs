//! The CLIPBOARD selection.
//!
//! An X selection is not a buffer. Writing one makes this process its owner,
//! a promise to answer whoever asks for as long as nobody takes it over. So
//! the owner window lives on the host's own connection and its requests are
//! answered in the poll loop, long after `set_clipboard` returned.
//!
//! Reading goes through a second, short-lived connection. The conversation
//! with another owner then never mixes with the host's event stream, and a
//! slow owner costs that one call its timeout and nothing else.

use std::time::{Duration, Instant};

use rustix::event::{poll, PollFd, PollFlags};
use rustix::time::Timespec;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Property,
    SelectionClearEvent, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
    SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::x11::Display;

/// How long an owner gets to answer, and each chunk of a long transfer.
const ANSWER_WITHIN: Duration = Duration::from_secs(1);

/// An invisible window that owns the selection while this process does.
pub struct Owner {
    window: Window,
    text: Option<String>,
}

impl Owner {
    pub fn new(x: &Display) -> Result<Self, String> {
        let window = x
            .conn
            .generate_id()
            .map_err(|e| format!("the X server gave no window id: {e}"))?;
        x.conn
            .create_window(
                0,
                window,
                x.root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new(),
            )
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| format!("could not create the clipboard window: {e}"))?;
        Ok(Self { window, text: None })
    }

    pub fn set(&mut self, x: &Display, text: &str) -> Result<(), String> {
        // One request has to carry it: an incremental transfer, for the
        // rare text past several megabytes, is not written.
        let limit = x.conn.maximum_request_bytes().saturating_sub(64);
        if text.len() > limit {
            return Err(format!(
                "{} bytes is more than one X request carries here, {limit}",
                text.len()
            ));
        }
        x.conn
            .set_selection_owner(self.window, x.atoms.CLIPBOARD, x11rb::CURRENT_TIME)
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| format!("the X server refused the clipboard: {e}"))?;
        let owner = x
            .conn
            .get_selection_owner(x.atoms.CLIPBOARD)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .owner;
        if owner != self.window {
            return Err("another application took the clipboard back at once".to_owned());
        }
        self.text = Some(text.to_owned());
        Ok(())
    }

    /// Someone else copied something. The text is theirs now.
    pub fn lost(&mut self, x: &Display, event: &SelectionClearEvent) {
        if event.owner == self.window && event.selection == x.atoms.CLIPBOARD {
            self.text = None;
        }
    }

    /// Answer one request, with the text or with a refusal. Every request
    /// gets an answer, or the asking application waits for its own timeout.
    pub fn answer(&self, x: &Display, req: &SelectionRequestEvent) {
        let a = &x.atoms;
        // An obsolete client may name no property; ICCCM says use the target.
        let property = if req.property == x11rb::NONE {
            req.target
        } else {
            req.property
        };
        let ours = req.owner == self.window && req.selection == a.CLIPBOARD;
        let served = match (&self.text, ours) {
            (Some(text), true) => {
                let string = u32::from(AtomEnum::STRING);
                if req.target == a.TARGETS {
                    x.conn
                        .change_property32(
                            PropMode::REPLACE,
                            req.requestor,
                            property,
                            AtomEnum::ATOM,
                            &[a.TARGETS, a.UTF8_STRING, a.TEXT, string],
                        )
                        .is_ok()
                } else if req.target == a.UTF8_STRING || req.target == a.TEXT {
                    x.conn
                        .change_property8(
                            PropMode::REPLACE,
                            req.requestor,
                            property,
                            a.UTF8_STRING,
                            text.as_bytes(),
                        )
                        .is_ok()
                } else if req.target == string {
                    x.conn
                        .change_property8(
                            PropMode::REPLACE,
                            req.requestor,
                            property,
                            AtomEnum::STRING,
                            &latin1(text),
                        )
                        .is_ok()
                } else {
                    false
                }
            }
            _ => false,
        };
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: req.time,
            requestor: req.requestor,
            selection: req.selection,
            target: req.target,
            property: if served { property } else { x11rb::NONE },
        };
        let _ = x
            .conn
            .send_event(false, req.requestor, EventMask::NO_EVENT, notify);
    }
}

/// What the clipboard holds as text. When this process is the owner its
/// own text is the answer: asking itself over a second connection would
/// wait on a loop that is not running during this call.
pub fn read(x: &Display, ours: Option<&Owner>) -> Result<String, String> {
    let owner = x
        .conn
        .get_selection_owner(x.atoms.CLIPBOARD)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("the X server would not name the clipboard owner: {e}"))?
        .owner;
    if owner == x11rb::NONE {
        return Ok(String::new());
    }
    if let Some(ours) = ours.filter(|o| o.window == owner) {
        return Ok(ours.text.clone().unwrap_or_default());
    }
    let atoms = &x.atoms;
    let (conn, screen) = RustConnection::connect(None)
        .map_err(|e| format!("could not open a second X connection: {e}"))?;
    let root = conn.setup().roots[screen].root;
    let window = conn.generate_id().map_err(|e| e.to_string())?;
    conn.create_window(
        0,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        x11rb::COPY_FROM_PARENT,
        // An incremental transfer is announced chunk by chunk through
        // property changes on this window.
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| e.to_string())?;

    let property = atoms.CORTISOFF_CLIPBOARD;
    for target in [atoms.UTF8_STRING, u32::from(AtomEnum::STRING)] {
        conn.convert_selection(
            window,
            atoms.CLIPBOARD,
            target,
            property,
            x11rb::CURRENT_TIME,
        )
        .map_err(|e| e.to_string())?;
        let answer = wait(
            &conn,
            Instant::now() + ANSWER_WITHIN,
            |e| matches!(e, Event::SelectionNotify(n) if n.requestor == window),
        )?;
        let Event::SelectionNotify(answer) = answer else {
            unreachable!()
        };
        if answer.property == x11rb::NONE {
            continue;
        }
        let bytes = fetch(&conn, window, property, atoms.INCR)?;
        return Ok(if target == atoms.UTF8_STRING {
            String::from_utf8_lossy(&bytes).into_owned()
        } else {
            bytes.iter().map(|b| char::from(*b)).collect()
        });
    }
    // The owner offers no text, an image or a file list.
    Ok(String::new())
}

fn fetch(
    conn: &RustConnection,
    window: Window,
    property: u32,
    incr: u32,
) -> Result<Vec<u8>, String> {
    let take = || -> Result<_, String> {
        conn.get_property(true, window, property, AtomEnum::ANY, 0, u32::MAX / 4)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())
    };
    let first = take()?;
    if first.type_ != incr {
        return Ok(first.value);
    }
    // Deleting the INCR property, done by `take`, tells the owner to start.
    // Each chunk then arrives as a new value, and an empty one ends it.
    let mut out = Vec::new();
    loop {
        wait(conn, Instant::now() + ANSWER_WITHIN, |e| {
            matches!(e, Event::PropertyNotify(p)
                if p.window == window && p.atom == property && p.state == Property::NEW_VALUE)
        })?;
        let chunk = take()?;
        if chunk.value.is_empty() {
            return Ok(out);
        }
        out.extend(chunk.value);
    }
}

fn wait(
    conn: &RustConnection,
    deadline: Instant,
    wanted: impl Fn(&Event) -> bool,
) -> Result<Event, String> {
    loop {
        conn.flush().map_err(|e| e.to_string())?;
        while let Some(event) = conn.poll_for_event().map_err(|e| e.to_string())? {
            if wanted(&event) {
                return Ok(event);
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("the application holding the clipboard did not answer".to_owned());
        }
        let mut fds = [PollFd::new(conn.stream(), PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: left.as_secs() as i64,
            tv_nsec: left.subsec_nanos().into(),
        };
        let _ = poll(&mut fds, Some(&timeout));
    }
}

/// The `STRING` target is Latin-1 by definition. What it cannot hold
/// becomes a question mark rather than mojibake.
pub fn latin1(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin1_keeps_what_it_can_and_marks_the_rest() {
        assert_eq!(latin1("café"), vec![b'c', b'a', b'f', 0xe9]);
        assert_eq!(latin1("10 €"), b"10 ?".to_vec());
    }
}
