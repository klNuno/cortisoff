//! Which key types which character, read from the active keyboard layout.
//!
//! A chord names a character (`ctrl+a`), and macOS registers and posts key
//! codes, which are positions. On a US layout the two agree; on AZERTY the
//! key at the US A position types Q. `UCKeyTranslate` over the current
//! layout's `uchr` data answers what each key types, so a chord lands on the
//! key that carries its letter, the way Windows' virtual keys already do.
//! A character the layout does not type at all (Latin letters on a Cyrillic
//! layout) keeps its US position, which is again what Windows does.

use std::collections::HashMap;
use std::ffi::c_void;

type OsStatus = i32;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *const c_void;
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        layout: *const c_void,
        virtual_key_code: u16,
        key_action: u16,
        modifier_key_state: u32,
        keyboard_type: u32,
        key_translate_options: u32,
        dead_key_state: *mut u32,
        max_string_length: usize,
        actual_string_length: *mut usize,
        unicode_string: *mut u16,
    ) -> OsStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFRelease(cf: *const c_void);
}

/// `kUCKeyActionDisplay`: what the key shows, with no dead key pending.
const ACTION_DISPLAY: u16 = 3;
/// `kUCKeyTranslateNoDeadKeysMask`: a dead key answers its own character.
const NO_DEAD_KEYS: u32 = 1;
/// `shiftKey >> 8`, the modifier state `UCKeyTranslate` takes.
const SHIFT: u32 = 0x02;

/// The keys that type characters: the main block, the ISO key left of Z
/// and the two extra JIS keys. The keypad is left out, or AZERTY's keypad
/// 1, which types 1 unshifted, would win over the main row's 1.
fn typing_keys() -> impl Iterator<Item = u16> {
    (0x00..=0x32).chain([0x5D, 0x5E])
}

/// Character to key code for the letters and digits a chord can name.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Layout {
    keys: HashMap<char, u16>,
}

impl Layout {
    /// Built from what each key types, unshifted first, then with shift,
    /// so a digit AZERTY puts on a shifted key still finds that key, and a
    /// key that types a character without shift wins over one needing it.
    pub fn from_translate(translate: impl Fn(u16, u32) -> Option<char>) -> Self {
        let mut keys = HashMap::new();
        for mods in [0, SHIFT] {
            for code in typing_keys() {
                let Some(c) = translate(code, mods) else {
                    continue;
                };
                let c = c.to_lowercase().next().unwrap_or(c);
                if c.is_alphanumeric() {
                    keys.entry(c).or_insert(code);
                }
            }
        }
        Self { keys }
    }

    pub fn code(&self, c: char) -> Option<u16> {
        self.keys.get(&c.to_ascii_lowercase()).copied()
    }

    /// The layout active now. `None` when the input source carries no
    /// `uchr` data, and callers keep the US positions. Main thread only:
    /// the text input services assert it.
    pub fn current() -> Option<Self> {
        // SAFETY: a Copy-rule call; the source is released below.
        let source = unsafe { TISCopyCurrentKeyboardLayoutInputSource() };
        if source.is_null() {
            return None;
        }
        // SAFETY: a valid source and a key the framework exports. The data
        // follows the Get rule: owned by the source, alive while it is.
        let data = unsafe { TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData) };
        let layout = if data.is_null() {
            None
        } else {
            // SAFETY: a CFData the source owns, read before it is released.
            let bytes = unsafe { CFDataGetBytePtr(data) };
            // SAFETY: plain call.
            let keyboard = u32::from(unsafe { LMGetKbdType() });
            (!bytes.is_null())
                .then(|| Self::from_translate(|code, mods| translate(bytes, keyboard, code, mods)))
        };
        // SAFETY: the reference the Copy call returned, released once.
        unsafe { CFRelease(source) };
        layout
    }
}

fn translate(layout: *const u8, keyboard: u32, code: u16, mods: u32) -> Option<char> {
    let mut dead = 0u32;
    let mut len = 0usize;
    let mut out = [0u16; 4];
    // SAFETY: `layout` points at the `uchr` data of a live input source, and
    // the buffer and counters are sized as passed.
    let status = unsafe {
        UCKeyTranslate(
            layout.cast(),
            code,
            ACTION_DISPLAY,
            mods,
            keyboard,
            NO_DEAD_KEYS,
            &mut dead,
            out.len(),
            &mut len,
            out.as_mut_ptr(),
        )
    };
    if status != 0 || len == 0 {
        return None;
    }
    let mut chars = char::decode_utf16(out[..len.min(out.len())].iter().copied());
    match (chars.next(), chars.next()) {
        (Some(Ok(c)), None) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AZERTY's main block, as far as these tests need it.
    fn azerty(code: u16, mods: u32) -> Option<char> {
        let shifted = mods & SHIFT != 0;
        Some(match (code, shifted) {
            (0x00, false) => 'q',
            (0x00, true) => 'Q',
            (0x0C, false) => 'a',
            (0x0C, true) => 'A',
            (0x06, false) => 'w',
            (0x0D, false) => 'z',
            (0x2E, false) => ',',
            (0x29, false) => 'm',
            (0x12, false) => '&',
            (0x12, true) => '1',
            (0x13, false) => '\u{e9}',
            (0x13, true) => '2',
            _ => return None,
        })
    }

    #[test]
    fn azerty_letters_land_on_the_key_that_types_them() {
        let layout = Layout::from_translate(azerty);
        assert_eq!(layout.code('a'), Some(0x0C));
        assert_eq!(layout.code('A'), Some(0x0C));
        assert_eq!(layout.code('q'), Some(0x00));
        assert_eq!(layout.code('z'), Some(0x0D));
        assert_eq!(layout.code('w'), Some(0x06));
        assert_eq!(layout.code('m'), Some(0x29));
    }

    #[test]
    fn a_digit_on_a_shifted_key_still_finds_its_key() {
        let layout = Layout::from_translate(azerty);
        assert_eq!(layout.code('1'), Some(0x12));
        assert_eq!(layout.code('2'), Some(0x13));
        assert_eq!(layout.code('9'), None, "absent, the caller falls back");
    }

    #[test]
    fn an_unshifted_key_wins_over_a_shifted_one() {
        // Two keys type 1: one shifted, one plain. The plain one is what a
        // person presses for "ctrl+1".
        let layout = Layout::from_translate(|code, mods| match (code, mods) {
            (0x12, SHIFT) => Some('1'),
            (0x20, 0) => Some('1'),
            _ => None,
        });
        assert_eq!(layout.code('1'), Some(0x20));
    }

    #[test]
    fn the_keypad_is_never_scanned() {
        assert!(typing_keys().all(|code| !(0x41..=0x5C).contains(&code)));
        let layout = Layout::from_translate(|code, mods| match (code, mods) {
            (0x53, 0) => Some('1'),
            (0x12, SHIFT) => Some('1'),
            _ => None,
        });
        assert_eq!(layout.code('1'), Some(0x12));
    }
}
