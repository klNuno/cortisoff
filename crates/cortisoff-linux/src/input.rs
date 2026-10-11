//! Global hotkeys through core key grabs, and synthetic input through XTest.

use std::time::Duration;

use cortisoff_host::{Chord, Hotkey};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConnectionExt as _, GrabMode, ModMask, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
};
use x11rb::protocol::xtest::ConnectionExt as _;

use crate::keymap::{keysym_for_char, keysym_for_key, sym, Keymap};
use crate::x11::Display;

/// The keyboard mapping and modifier map as the server holds them now.
/// Read again on every `MappingNotify`, because a layout switch moves keys.
pub fn load_keymap(x: &Display) -> Option<Keymap> {
    let setup = x.conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let keys = x
        .conn
        .get_keyboard_mapping(min, max.checked_sub(min)?.checked_add(1)?)
        .ok()?
        .reply()
        .ok()?;
    let mods = x.conn.get_modifier_mapping().ok()?.reply().ok()?;
    Some(Keymap::new(
        min,
        keys.keysyms_per_keycode,
        keys.keysyms,
        &mods.keycodes,
        mods.keycodes_per_modifier(),
    ))
}

/// One chord this process holds a grab for, all four lock variants of it.
#[derive(Debug, Clone)]
struct Grab {
    id: u32,
    keycode: u8,
    mask: u16,
}

#[derive(Debug, Default)]
pub struct Hotkeys {
    held: Vec<Grab>,
    /// What was asked for last, to grab again after a layout change.
    wanted: Vec<Hotkey>,
}

impl Hotkeys {
    /// Replace every grab with these. Returns the chords refused, with why.
    pub fn set(&mut self, x: &Display, map: &Keymap, wanted: &[Hotkey]) -> Vec<(u32, String)> {
        self.release(x, map);
        self.wanted = wanted.to_vec();
        let mut refused = Vec::new();
        for hotkey in wanted {
            let chord = hotkey.chord;
            let Some((keycode, _)) = map.keycode(keysym_for_key(chord.key)) else {
                refused.push((
                    hotkey.id,
                    format!("no key on this keyboard layout sends {chord}"),
                ));
                continue;
            };
            let mask = map.mask(chord.mods);
            if self
                .held
                .iter()
                .any(|g| g.keycode == keycode && g.mask == mask)
            {
                refused.push((
                    hotkey.id,
                    format!("{chord} is already taken by another cortisoff hotkey"),
                ));
                continue;
            }
            match grab(x, keycode, &map.variants(mask)) {
                Ok(()) => self.held.push(Grab {
                    id: hotkey.id,
                    keycode,
                    mask,
                }),
                Err(()) => refused.push((
                    hotkey.id,
                    format!("another application already owns {chord}"),
                )),
            }
        }
        x.flush();
        refused
    }

    /// The keyboard map changed under the grabs: the same chords may sit on
    /// other keycodes now. Grab them again and return what the new layout
    /// refused.
    pub fn regrab(&mut self, x: &Display, old: &Keymap, new: &Keymap) -> Vec<(u32, String)> {
        self.release(x, old);
        let wanted = std::mem::take(&mut self.wanted);
        self.set(x, new, &wanted)
    }

    fn release(&mut self, x: &Display, map: &Keymap) {
        for g in self.held.drain(..) {
            for mask in map.variants(g.mask) {
                let _ = x.conn.ungrab_key(g.keycode, x.root, ModMask::from(mask));
            }
        }
    }

    /// The hotkey a key press on the root means, if any.
    pub fn pressed(&self, map: &Keymap, keycode: u8, state: u16) -> Option<u32> {
        let state = map.clean(state);
        self.held
            .iter()
            .find(|g| g.keycode == keycode && g.mask == state)
            .map(|g| g.id)
    }
}

/// All variants or none: a chord that works only with NumLock off is a
/// hotkey that seems to break at random.
fn grab(x: &Display, keycode: u8, variants: &[u16]) -> Result<(), ()> {
    let mut taken = Vec::new();
    for mask in variants {
        let checked = x
            .conn
            .grab_key(
                false,
                x.root,
                ModMask::from(*mask),
                keycode,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .ok()
            .map(|cookie| cookie.check());
        match checked {
            Some(Ok(())) => taken.push(*mask),
            _ => {
                for mask in taken {
                    let _ = x.conn.ungrab_key(keycode, x.root, ModMask::from(mask));
                }
                return Err(());
            }
        }
    }
    Ok(())
}

const NO_XTEST: &str = "the X server has no XTEST extension, which is how cortisoff presses keys";

fn fake(x: &Display, kind: u8, keycode: u8) -> Result<(), String> {
    x.conn
        .xtest_fake_input(kind, keycode, x11rb::CURRENT_TIME, x11rb::NONE, 0, 0, 0)
        .map(drop)
        .map_err(|e| format!("the X connection failed: {e}"))
}

/// A round trip, so every fake event before it has been processed and any
/// error has come back.
fn sync(x: &Display) -> Result<(), String> {
    x.conn
        .get_input_focus()
        .map_err(|e| e.to_string())?
        .reply()
        .map(drop)
        .map_err(|e| format!("the X server refused a key: {e}"))
}

fn modifier(map: &Keymap, keysym: u32, name: &str) -> Result<u8, String> {
    map.keycode(keysym)
        .map(|(code, _)| code)
        .ok_or_else(|| format!("no key on this keyboard layout sends {name}"))
}

pub fn send_chord(x: &Display, map: &Keymap, chord: &Chord) -> Result<(), String> {
    if !x.xtest {
        return Err(NO_XTEST.to_owned());
    }
    let (key, needs_shift) = map
        .keycode(keysym_for_key(chord.key))
        .ok_or_else(|| format!("no key on this keyboard layout sends {chord}"))?;
    let m = chord.mods;
    let mut held = Vec::new();
    for (on, keysym, name) in [
        (m.ctrl, sym::CONTROL_L, "Control"),
        (m.alt, sym::ALT_L, "Alt"),
        (m.shift || needs_shift, sym::SHIFT_L, "Shift"),
        (m.meta, sym::SUPER_L, "Super"),
    ] {
        if on {
            held.push(modifier(map, keysym, name)?);
        }
    }
    for code in &held {
        fake(x, KEY_PRESS_EVENT, *code)?;
    }
    fake(x, KEY_PRESS_EVENT, key)?;
    fake(x, KEY_RELEASE_EVENT, key)?;
    for code in held.iter().rev() {
        fake(x, KEY_RELEASE_EVENT, *code)?;
    }
    sync(x)
}

/// One character to type: a key on the layout, or a keysym that needs a
/// borrowed keycode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stroke {
    Key { code: u8, shift: bool },
    Borrow(u32),
}

/// How long a client gets to read a remapped keycode before it changes
/// again. A client reads the new mapping when it handles the
/// `MappingNotify`, which can be after the next remap; `xdotool` pays the
/// same wait for the same reason. Only text with characters the layout has
/// no key for pays it.
const REMAP_SETTLE: Duration = Duration::from_millis(40);

/// Splits strokes into runs that need at most `spares` distinct borrowed
/// keysyms each, in order.
fn runs(strokes: &[Stroke], spares: usize) -> Vec<&[Stroke]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut borrowed: Vec<u32> = Vec::new();
    for (i, stroke) in strokes.iter().enumerate() {
        if let Stroke::Borrow(keysym) = stroke {
            if !borrowed.contains(keysym) {
                if borrowed.len() == spares {
                    out.push(&strokes[start..i]);
                    start = i;
                    borrowed.clear();
                }
                borrowed.push(*keysym);
            }
        }
    }
    if start < strokes.len() {
        out.push(&strokes[start..]);
    }
    out
}

pub fn type_text(x: &Display, map: &Keymap, text: &str) -> Result<(), String> {
    if !x.xtest {
        return Err(NO_XTEST.to_owned());
    }
    let strokes: Vec<Stroke> = text
        .chars()
        .filter_map(keysym_for_char)
        .map(|keysym| match map.keycode(keysym) {
            Some((code, shift)) => Stroke::Key { code, shift },
            None => Stroke::Borrow(keysym),
        })
        .collect();
    let needs_borrow = strokes.iter().any(|s| matches!(s, Stroke::Borrow(_)));
    let spares = map.spares();
    if needs_borrow && spares.is_empty() {
        return Err(
            "this keyboard layout has no free keycode to type a character it has no key for"
                .to_owned(),
        );
    }
    let shift = if strokes
        .iter()
        .any(|s| matches!(s, Stroke::Key { shift: true, .. }))
    {
        Some(modifier(map, sym::SHIFT_L, "Shift")?)
    } else {
        None
    };

    let per = map.per.max(1);
    let mut used: Vec<u8> = Vec::new();
    let result = (|| {
        for run in runs(&strokes, spares.len().max(1)) {
            let mut slots: Vec<(u32, u8)> = Vec::new();
            for stroke in run {
                if let Stroke::Borrow(keysym) = stroke {
                    if !slots.iter().any(|(k, _)| k == keysym) {
                        let code = spares[slots.len()];
                        let row = vec![*keysym; usize::from(per)];
                        x.conn
                            .change_keyboard_mapping(1, code, per, &row)
                            .map_err(|e| e.to_string())?;
                        slots.push((*keysym, code));
                        if !used.contains(&code) {
                            used.push(code);
                        }
                    }
                }
            }
            if !slots.is_empty() {
                sync(x)?;
                std::thread::sleep(REMAP_SETTLE);
            }
            for stroke in run {
                let (code, shifted) = match stroke {
                    Stroke::Key { code, shift } => (*code, *shift),
                    Stroke::Borrow(keysym) => (
                        slots
                            .iter()
                            .find(|(k, _)| k == keysym)
                            .map_or(0, |(_, c)| *c),
                        false,
                    ),
                };
                if shifted {
                    if let Some(s) = shift {
                        fake(x, KEY_PRESS_EVENT, s)?;
                    }
                }
                fake(x, KEY_PRESS_EVENT, code)?;
                fake(x, KEY_RELEASE_EVENT, code)?;
                if shifted {
                    if let Some(s) = shift {
                        fake(x, KEY_RELEASE_EVENT, s)?;
                    }
                }
            }
            sync(x)?;
            if !slots.is_empty() {
                std::thread::sleep(REMAP_SETTLE);
            }
        }
        Ok(())
    })();

    // The borrowed keycodes go back to sending nothing, whatever happened.
    if !used.is_empty() {
        let empty = vec![0u32; usize::from(per)];
        for code in &used {
            let _ = x.conn.change_keyboard_mapping(1, *code, per, &empty);
        }
        let _ = sync(x);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: u8) -> Stroke {
        Stroke::Key { code, shift: false }
    }

    #[test]
    fn text_on_the_layout_is_one_run() {
        let strokes = [key(9), key(10), key(9)];
        assert_eq!(runs(&strokes, 2), vec![&strokes[..]]);
    }

    /// Two spare keycodes, three foreign characters: the third one starts a
    /// second run, and a repeat of one already borrowed does not.
    #[test]
    fn foreign_characters_are_split_by_the_spares_available() {
        let a = Stroke::Borrow(0x0100_20ac);
        let b = Stroke::Borrow(0x0100_2603);
        let c = Stroke::Borrow(0x0100_2764);
        let strokes = [a, key(9), b, a, c, key(10), c];
        let split = runs(&strokes, 2);
        assert_eq!(split, vec![&strokes[..4], &strokes[4..]]);
    }

    #[test]
    fn nothing_to_type_is_no_run() {
        assert!(runs(&[], 4).is_empty());
    }
}
