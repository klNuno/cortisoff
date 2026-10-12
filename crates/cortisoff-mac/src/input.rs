//! Synthetic keyboard input: `send_keys` and `type_text`.
//!
//! macOS drops events posted by a process without the Accessibility
//! permission, silently. Both calls check it first so a plugin gets a
//! sentence instead of a success that typed nothing.

use cortisoff_host::Chord;
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventTapLocation};

use crate::layout::Layout;
use crate::{ax, keys};

pub fn send_keys(chord: &Chord) -> Result<(), String> {
    let code = keys::code(chord.key, Layout::current().as_ref())
        .ok_or_else(|| keys::SYSTEM_KEY.to_owned())?;
    if !ax::trusted() {
        return Err(ax::PERMISSION.to_owned());
    }
    let flags = CGEventFlags(keys::event_flags(chord.mods));
    for down in [true, false] {
        let event = CGEvent::new_keyboard_event(None, code, down)
            .ok_or_else(|| "macOS would not create a keyboard event".to_owned())?;
        CGEvent::set_flags(Some(&event), flags);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
    Ok(())
}

pub fn type_text(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    if !ax::trusted() {
        return Err(ax::PERMISSION.to_owned());
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    for chunk in keys::text_chunks(&units) {
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(None, 0, down)
                .ok_or_else(|| "macOS would not create a keyboard event".to_owned())?;
            // No modifiers: a hotkey that triggered this may still be held,
            // and a held Control would turn the text into shortcuts.
            CGEvent::set_flags(Some(&event), CGEventFlags(0));
            // SAFETY: the pointer covers `chunk.len()` UTF-16 units, which
            // the call copies.
            unsafe {
                CGEvent::keyboard_set_unicode_string(
                    Some(&event),
                    chunk.len() as _,
                    chunk.as_ptr(),
                );
            }
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
    }
    Ok(())
}
