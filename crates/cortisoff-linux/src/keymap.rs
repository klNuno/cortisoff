//! Chords and characters to X keysyms, keysyms to keycodes, and which
//! modifier bit means what on this keyboard.
//!
//! All of it is arithmetic over the server's keyboard mapping, so it lives
//! apart from the requests and every case is tested with a fixture map.

use cortisoff_host::{Key, Mods, Named};

pub const SHIFT: u16 = 1;
pub const LOCK: u16 = 1 << 1;
pub const CONTROL: u16 = 1 << 2;

/// Keysyms the host presses or looks for by name.
pub mod sym {
    pub const SHIFT_L: u32 = 0xffe1;
    pub const CONTROL_L: u32 = 0xffe3;
    pub const ALT_L: u32 = 0xffe9;
    pub const ALT_R: u32 = 0xffea;
    pub const SUPER_L: u32 = 0xffeb;
    pub const SUPER_R: u32 = 0xffec;
    pub const NUM_LOCK: u32 = 0xff7f;
    pub const RETURN: u32 = 0xff0d;
    pub const TAB: u32 = 0xff09;
    pub const BACKSPACE: u32 = 0xff08;
}

/// Which of Mod1 to Mod5 carries Alt, Super and NumLock on this keyboard.
/// The defaults are what nearly every layout uses, and what is assumed when
/// the modifier map says nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Masks {
    pub alt: u16,
    pub meta: u16,
    pub numlock: u16,
}

impl Default for Masks {
    fn default() -> Self {
        Self {
            alt: 1 << 3,
            meta: 1 << 6,
            numlock: 1 << 4,
        }
    }
}

/// The core keyboard mapping as the server sent it: `per` keysyms for each
/// keycode from `min` up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keymap {
    pub min: u8,
    pub per: u8,
    pub syms: Vec<u32>,
    pub masks: Masks,
}

impl Keymap {
    /// `modifiers` is the `GetModifierMapping` reply: eight rows of
    /// `per_modifier` keycodes, Shift, Lock, Control, then Mod1 to Mod5.
    pub fn new(min: u8, per: u8, syms: Vec<u32>, modifiers: &[u8], per_modifier: u8) -> Self {
        let mut map = Self {
            min,
            per,
            syms,
            masks: Masks::default(),
        };
        map.masks = map.read_masks(modifiers, usize::from(per_modifier));
        map
    }

    fn syms_of(&self, keycode: u8) -> &[u32] {
        let per = usize::from(self.per);
        if keycode < self.min || per == 0 {
            return &[];
        }
        let start = usize::from(keycode - self.min) * per;
        self.syms.get(start..start + per).unwrap_or(&[])
    }

    fn read_masks(&self, modifiers: &[u8], per_modifier: usize) -> Masks {
        let mut found = Masks {
            alt: 0,
            meta: 0,
            numlock: 0,
        };
        if per_modifier > 0 {
            for (row, keycodes) in modifiers.chunks(per_modifier).enumerate().skip(3).take(5) {
                let bit = 1u16 << row;
                let holds = |wanted: &[u32]| {
                    keycodes
                        .iter()
                        .filter(|k| **k != 0)
                        .any(|k| self.syms_of(*k).iter().any(|s| wanted.contains(s)))
                };
                if found.alt == 0 && holds(&[sym::ALT_L, sym::ALT_R]) {
                    found.alt = bit;
                }
                if found.meta == 0 && holds(&[sym::SUPER_L, sym::SUPER_R]) {
                    found.meta = bit;
                }
                if found.numlock == 0 && holds(&[sym::NUM_LOCK]) {
                    found.numlock = bit;
                }
            }
        }
        let default = Masks::default();
        Masks {
            alt: if found.alt == 0 {
                default.alt
            } else {
                found.alt
            },
            meta: if found.meta == 0 {
                default.meta
            } else {
                found.meta
            },
            numlock: if found.numlock == 0 {
                default.numlock
            } else {
                found.numlock
            },
        }
    }

    /// The keycode that types `keysym`, and whether Shift has to be held for
    /// it. Only the first two levels count: anything further needs AltGr or
    /// a group switch, and typing those goes through a spare keycode instead.
    /// The unshifted level wins when both have it.
    pub fn keycode(&self, keysym: u32) -> Option<(u8, bool)> {
        let per = usize::from(self.per);
        if per == 0 || keysym == 0 {
            return None;
        }
        let mut shifted = None;
        for (n, row) in self.syms.chunks(per).enumerate() {
            let code = u8::try_from(usize::from(self.min) + n).ok()?;
            if row.first() == Some(&keysym) {
                return Some((code, false));
            }
            if shifted.is_none() && row.get(1) == Some(&keysym) {
                shifted = Some((code, true));
            }
        }
        shifted
    }

    /// Keycodes that send nothing on this layout, which typing borrows for
    /// a character the layout has no key for.
    pub fn spares(&self) -> Vec<u8> {
        let per = usize::from(self.per);
        if per == 0 {
            return Vec::new();
        }
        self.syms
            .chunks(per)
            .enumerate()
            .filter(|(_, row)| row.iter().all(|s| *s == 0))
            .filter_map(|(n, _)| u8::try_from(usize::from(self.min) + n).ok())
            .collect()
    }

    /// The modifier state a chord means, as a core protocol mask.
    pub fn mask(&self, mods: Mods) -> u16 {
        let mut mask = 0;
        if mods.shift {
            mask |= SHIFT;
        }
        if mods.ctrl {
            mask |= CONTROL;
        }
        if mods.alt {
            mask |= self.masks.alt;
        }
        if mods.meta {
            mask |= self.masks.meta;
        }
        mask
    }

    /// A grab is exact about modifiers, so CapsLock or NumLock being on
    /// would make a hotkey silently dead. Every combination of the two is
    /// grabbed alongside the chord.
    pub fn variants(&self, mask: u16) -> [u16; 4] {
        let num = self.masks.numlock;
        [mask, mask | LOCK, mask | num, mask | LOCK | num]
    }

    /// A key event's state reduced to the modifiers a chord can name.
    pub fn clean(&self, state: u16) -> u16 {
        state & (SHIFT | CONTROL | self.masks.alt | self.masks.meta)
    }
}

/// The keysym a chord ends on.
pub fn keysym_for_key(key: Key) -> u32 {
    match key {
        // Letters and digits: their Latin-1 keysym is their code point, and
        // a lowercase letter is what sits on level one of its key.
        Key::Char(c) => u32::from(c.to_ascii_lowercase()),
        // XK_F1 to XK_F35 are contiguous.
        Key::F(n) => 0xffbe + u32::from(n.clamp(1, 35)) - 1,
        Key::Named(named) => match named {
            Named::Space => 0x0020,
            Named::Enter => sym::RETURN,
            Named::Tab => sym::TAB,
            Named::Escape => 0xff1b,
            Named::Backspace => sym::BACKSPACE,
            Named::Delete => 0xffff,
            Named::Insert => 0xff63,
            Named::Home => 0xff50,
            Named::End => 0xff57,
            Named::PageUp => 0xff55,
            Named::PageDown => 0xff56,
            Named::Up => 0xff52,
            Named::Down => 0xff54,
            Named::Left => 0xff51,
            Named::Right => 0xff53,
            Named::PrintScreen => 0xff61,
            Named::Pause => 0xff13,
            Named::VolumeUp => 0x1008_ff13,
            Named::VolumeDown => 0x1008_ff11,
            Named::VolumeMute => 0x1008_ff12,
            Named::MediaNext => 0x1008_ff17,
            Named::MediaPrevious => 0x1008_ff16,
            Named::MediaPlayPause => 0x1008_ff14,
            Named::MediaStop => 0x1008_ff15,
        },
    }
}

/// The keysym that types a character. Latin-1 keysyms are their code
/// points; everything else has a Unicode keysym, `0x0100_0000` plus the
/// code point. Control characters other than the three a text holds have
/// no key at all.
pub fn keysym_for_char(c: char) -> Option<u32> {
    let code = u32::from(c);
    match c {
        '\n' | '\r' => Some(sym::RETURN),
        '\t' => Some(sym::TAB),
        '\u{8}' => Some(sym::BACKSPACE),
        _ if c.is_control() => None,
        _ if (0x20..=0x7e).contains(&code) || (0xa0..=0xff).contains(&code) => Some(code),
        _ => Some(0x0100_0000 | code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slice of a US layout, keycodes 8 to 15, two keysyms each.
    fn us() -> Keymap {
        let syms = vec![
            0,
            0, // 8: nothing
            u32::from('a'),
            u32::from('A'), // 9
            u32::from('n'),
            u32::from('N'), // 10
            u32::from('1'),
            u32::from('!'), // 11
            sym::ALT_L,
            0xffe7, // 12: Alt, Meta when shifted
            sym::SUPER_L,
            0, // 13
            sym::NUM_LOCK,
            0, // 14
            0,
            0, // 15: nothing
        ];
        // Shift, Lock, Control, Mod1 to Mod5, one keycode each.
        let modifiers = [0, 0, 0, 12, 14, 0, 13, 0];
        Keymap::new(8, 2, syms, &modifiers, 1)
    }

    #[test]
    fn a_character_finds_its_key_and_its_level() {
        let map = us();
        assert_eq!(map.keycode(u32::from('a')), Some((9, false)));
        assert_eq!(map.keycode(u32::from('A')), Some((9, true)));
        assert_eq!(map.keycode(u32::from('!')), Some((11, true)));
        assert_eq!(map.keycode(u32::from('z')), None);
        assert_eq!(map.keycode(0), None, "NoSymbol is never a key");
    }

    #[test]
    fn unused_keycodes_are_spares() {
        assert_eq!(us().spares(), vec![8, 15]);
    }

    #[test]
    fn the_modifier_map_says_which_bit_is_alt_super_and_numlock() {
        let map = us();
        assert_eq!(
            map.masks,
            Masks {
                alt: 1 << 3,
                meta: 1 << 6,
                numlock: 1 << 4
            }
        );
        // A layout that moved Super to Mod3.
        let moved = Keymap::new(8, 2, us().syms, &[0, 0, 0, 12, 14, 13, 0, 0], 1);
        assert_eq!(moved.masks.meta, 1 << 5);
        // An empty modifier map falls back to the usual bits.
        let blank = Keymap::new(8, 2, us().syms, &[], 0);
        assert_eq!(blank.masks, Masks::default());
    }

    #[test]
    fn a_chord_becomes_a_mask_and_its_four_lock_variants() {
        let map = us();
        let chord: cortisoff_host::Chord = "ctrl+alt+n".parse().unwrap();
        let mask = map.mask(chord.mods);
        assert_eq!(mask, CONTROL | (1 << 3));
        assert_eq!(
            map.variants(mask),
            [mask, mask | LOCK, mask | (1 << 4), mask | LOCK | (1 << 4)]
        );
        let win: cortisoff_host::Chord = "win+shift+f5".parse().unwrap();
        assert_eq!(map.mask(win.mods), SHIFT | (1 << 6));
    }

    /// CapsLock and NumLock on, plus a mouse button held: the event still
    /// matches the chord that was grabbed.
    #[test]
    fn a_key_events_state_is_cleaned_to_what_a_chord_can_name() {
        let map = us();
        let state = CONTROL | (1 << 3) | LOCK | (1 << 4) | (1 << 8);
        assert_eq!(map.clean(state), CONTROL | (1 << 3));
    }

    #[test]
    fn chord_keys_map_to_their_keysyms() {
        assert_eq!(keysym_for_key(Key::Char('n')), 0x6e);
        assert_eq!(keysym_for_key(Key::Char('7')), 0x37);
        assert_eq!(keysym_for_key(Key::F(1)), 0xffbe);
        assert_eq!(keysym_for_key(Key::F(12)), 0xffc9);
        assert_eq!(keysym_for_key(Key::F(24)), 0xffd5);
        assert_eq!(keysym_for_key(Key::Named(Named::Enter)), 0xff0d);
        assert_eq!(keysym_for_key(Key::Named(Named::PageDown)), 0xff56);
        assert_eq!(keysym_for_key(Key::Named(Named::VolumeUp)), 0x1008_ff13);
        assert_eq!(
            keysym_for_key(Key::Named(Named::MediaPlayPause)),
            0x1008_ff14
        );
    }

    #[test]
    fn characters_map_to_latin1_or_unicode_keysyms() {
        assert_eq!(keysym_for_char('a'), Some(0x61));
        assert_eq!(keysym_for_char('é'), Some(0xe9));
        assert_eq!(keysym_for_char('€'), Some(0x0100_20ac));
        assert_eq!(keysym_for_char('\n'), Some(sym::RETURN));
        assert_eq!(keysym_for_char('\t'), Some(sym::TAB));
        assert_eq!(keysym_for_char('\u{7}'), None);
    }
}
