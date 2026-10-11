//! Chords as macOS takes them: a virtual key code, plus the modifier bits of
//! whichever API receives the chord.
//!
//! The codes are the `kVK_*` constants of `HIToolbox/Events.h`. A letter or a
//! digit is a position on an ANSI keyboard, not a character: `kVK_ANSI_A` is
//! the key left of S, which types Q on an AZERTY layout. Following the
//! current layout needs `UCKeyTranslate` over the layout data, and is not
//! written yet.

use cortisoff_host::{Key, Mods, Named};

/// Why a key has no code here, worded for the plugin author who asked.
pub const SYSTEM_KEY: &str =
    "macOS sends the volume and media keys as system events, not as key codes";

/// The virtual key code for `key`, or `None` when macOS has none for it.
pub fn code(key: Key) -> Option<u16> {
    match key {
        Key::Char(c) => char_code(c),
        Key::F(n) => function_code(n),
        Key::Named(n) => named_code(n),
    }
}

fn char_code(c: char) -> Option<u16> {
    Some(match c.to_ascii_lowercase() {
        'a' => 0x00,
        's' => 0x01,
        'd' => 0x02,
        'f' => 0x03,
        'h' => 0x04,
        'g' => 0x05,
        'z' => 0x06,
        'x' => 0x07,
        'c' => 0x08,
        'v' => 0x09,
        'b' => 0x0B,
        'q' => 0x0C,
        'w' => 0x0D,
        'e' => 0x0E,
        'r' => 0x0F,
        'y' => 0x10,
        't' => 0x11,
        '1' => 0x12,
        '2' => 0x13,
        '3' => 0x14,
        '4' => 0x15,
        '6' => 0x16,
        '5' => 0x17,
        '9' => 0x19,
        '7' => 0x1A,
        '8' => 0x1C,
        '0' => 0x1D,
        'o' => 0x1F,
        'u' => 0x20,
        'i' => 0x22,
        'p' => 0x23,
        'l' => 0x25,
        'j' => 0x26,
        'k' => 0x28,
        'n' => 0x2D,
        'm' => 0x2E,
        _ => return None,
    })
}

/// F1 to F20. Apple keyboards stop there and `Events.h` defines nothing
/// above it, so F21 to F24 have no code.
fn function_code(n: u8) -> Option<u16> {
    Some(match n {
        1 => 0x7A,
        2 => 0x78,
        3 => 0x63,
        4 => 0x76,
        5 => 0x60,
        6 => 0x61,
        7 => 0x62,
        8 => 0x64,
        9 => 0x65,
        10 => 0x6D,
        11 => 0x67,
        12 => 0x6F,
        13 => 0x69,
        14 => 0x6B,
        15 => 0x71,
        16 => 0x6A,
        17 => 0x40,
        18 => 0x4F,
        19 => 0x50,
        20 => 0x5A,
        _ => return None,
    })
}

fn named_code(n: Named) -> Option<u16> {
    Some(match n {
        Named::Space => 0x31,
        Named::Enter => 0x24,
        Named::Tab => 0x30,
        Named::Escape => 0x35,
        // `kVK_Delete` is the key above Return, the one a PC calls Backspace.
        Named::Backspace => 0x33,
        Named::Delete => 0x75,
        // Apple extended keyboards put Help where a PC has Insert, and F13
        // and F15 where it has Print Screen and Pause. Those are the keys a
        // person reaching for them finds.
        Named::Insert => 0x72,
        Named::PrintScreen => 0x69,
        Named::Pause => 0x71,
        Named::Home => 0x73,
        Named::End => 0x77,
        Named::PageUp => 0x74,
        Named::PageDown => 0x79,
        Named::Up => 0x7E,
        Named::Down => 0x7D,
        Named::Left => 0x7B,
        Named::Right => 0x7C,
        // `Events.h` names codes for the volume keys, but the keys themselves
        // arrive as `NX_KEYTYPE` system events: a hotkey registered on those
        // codes never fires, and a posted one changes nothing.
        Named::VolumeUp
        | Named::VolumeDown
        | Named::VolumeMute
        | Named::MediaNext
        | Named::MediaPrevious
        | Named::MediaPlayPause
        | Named::MediaStop => return None,
    })
}

/// The modifier bits Carbon's `RegisterEventHotKey` wants: `cmdKey`,
/// `shiftKey`, `optionKey` and `controlKey` from `Events.h`.
pub fn carbon_modifiers(m: Mods) -> u32 {
    let mut out = 0;
    if m.meta {
        out |= 0x0100;
    }
    if m.shift {
        out |= 0x0200;
    }
    if m.alt {
        out |= 0x0800;
    }
    if m.ctrl {
        out |= 0x1000;
    }
    out
}

/// The `CGEventFlags` bits a synthesized key press carries:
/// `kCGEventFlagMaskShift`, `Control`, `Alternate` and `Command`.
pub fn event_flags(m: Mods) -> u64 {
    let mut out = 0;
    if m.shift {
        out |= 0x0002_0000;
    }
    if m.ctrl {
        out |= 0x0004_0000;
    }
    if m.alt {
        out |= 0x0008_0000;
    }
    if m.meta {
        out |= 0x0010_0000;
    }
    out
}

/// UTF-16 units per synthetic typing event. Some applications keep only
/// the start of a longer string carried by one event, and 20 is the limit
/// typing tools for macOS settled on.
pub const TEXT_CHUNK: usize = 20;

/// Cuts text for synthetic typing at most every [`TEXT_CHUNK`] UTF-16
/// units, never between the two halves of a surrogate pair.
pub fn text_chunks(units: &[u16]) -> Vec<&[u16]> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < units.len() {
        let mut end = (start + TEXT_CHUNK).min(units.len());
        if end < units.len() && (0xDC00..=0xDFFF).contains(&units[end]) {
            end -= 1;
        }
        out.push(&units[start..end]);
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cortisoff_host::Chord;

    fn chord(text: &str) -> Chord {
        text.parse().unwrap()
    }

    /// Spot checks against `Events.h`, one per block of the table, because a
    /// wrong entry presses a different key without any error.
    #[test]
    fn codes_match_the_header() {
        assert_eq!(code(Key::Char('a')), Some(0x00));
        assert_eq!(code(Key::Char('N')), Some(0x2D));
        assert_eq!(code(Key::Char('5')), Some(0x17));
        assert_eq!(code(Key::Char('0')), Some(0x1D));
        assert_eq!(code(Key::F(1)), Some(0x7A));
        assert_eq!(code(Key::F(12)), Some(0x6F));
        assert_eq!(code(Key::F(20)), Some(0x5A));
        assert_eq!(code(Key::Named(Named::Space)), Some(0x31));
        assert_eq!(code(Key::Named(Named::Backspace)), Some(0x33));
        assert_eq!(code(Key::Named(Named::Delete)), Some(0x75));
        assert_eq!(code(Key::Named(Named::Up)), Some(0x7E));
    }

    /// Every letter and digit the chord parser accepts has a code, and no two
    /// share one: a duplicate is a typo that silently binds the wrong key.
    #[test]
    fn every_letter_and_digit_has_its_own_code() {
        let mut seen = std::collections::HashSet::new();
        for c in ('a'..='z').chain('0'..='9') {
            let code = code(Key::Char(c)).unwrap_or_else(|| panic!("{c} has no code"));
            assert!(seen.insert(code), "{c} shares code {code:#x}");
        }
        for n in 1..=20 {
            let code = code(Key::F(n)).unwrap_or_else(|| panic!("f{n} has no code"));
            assert!(seen.insert(code), "f{n} shares code {code:#x}");
        }
    }

    #[test]
    fn keys_macos_does_not_have_are_refused_rather_than_guessed() {
        assert_eq!(code(Key::F(21)), None);
        assert_eq!(code(Key::F(24)), None);
        assert_eq!(code(Key::Named(Named::VolumeUp)), None);
        assert_eq!(code(Key::Named(Named::MediaPlayPause)), None);
        assert_eq!(code(Key::Char('-')), None);
    }

    #[test]
    fn modifiers_map_to_both_apis() {
        let c = chord("ctrl+alt+shift+cmd+n");
        assert_eq!(carbon_modifiers(c.mods), 0x0100 | 0x0200 | 0x0800 | 0x1000);
        assert_eq!(
            event_flags(c.mods),
            0x0002_0000 | 0x0004_0000 | 0x0008_0000 | 0x0010_0000
        );
        let c = chord("win+space");
        assert_eq!(carbon_modifiers(c.mods), 0x0100);
        assert_eq!(event_flags(c.mods), 0x0010_0000);
        assert_eq!(carbon_modifiers(chord("f5").mods), 0);
    }

    #[test]
    fn text_is_cut_in_chunks_that_keep_surrogate_pairs_whole() {
        let text: String = "a".repeat(19) + "\u{1F600}" + "bc";
        let units: Vec<u16> = text.encode_utf16().collect();
        let parts = text_chunks(&units);
        assert_eq!(parts[0].len(), 19);
        assert_eq!(String::from_utf16(parts[1]).unwrap(), "\u{1F600}bc");
        let joined: Vec<u16> = parts.concat();
        assert_eq!(joined, units);
    }

    #[test]
    fn short_text_is_one_chunk() {
        let units: Vec<u16> = "hello".encode_utf16().collect();
        assert_eq!(text_chunks(&units), vec![&units[..]]);
    }
}
