//! A chord as plugins write it, turned into what Win32 wants: hotkey
//! modifier bits and a virtual key. Pure, so every key the chord parser
//! accepts is proved to land somewhere without touching a keyboard.

use cortisoff_host::{Chord, Key, Mods, Named};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, VIRTUAL_KEY, VK_0, VK_A, VK_APPS,
    VK_BACK, VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_INSERT,
    VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE,
    VK_MEDIA_PREV_TRACK, VK_MEDIA_STOP, VK_NEXT, VK_NUMLOCK, VK_PAUSE, VK_PRIOR, VK_RCONTROL,
    VK_RETURN, VK_RIGHT, VK_RMENU, VK_RWIN, VK_SNAPSHOT, VK_SPACE, VK_TAB, VK_UP, VK_VOLUME_DOWN,
    VK_VOLUME_MUTE, VK_VOLUME_UP,
};

/// The modifier bits and the virtual key of `chord`. `None` for a key no
/// keyboard layout gives a fixed virtual key to: a `Char` that is neither a
/// letter nor a digit, or an `F` outside 1 to 24. The parser never builds
/// those, but a chord can also arrive deserialized.
pub fn chord(chord: &Chord) -> Option<(HOT_KEY_MODIFIERS, VIRTUAL_KEY)> {
    Some((modifiers(chord.mods), key(chord.key)?))
}

fn modifiers(mods: Mods) -> HOT_KEY_MODIFIERS {
    let mut out = HOT_KEY_MODIFIERS(0);
    for (on, bit) in [
        (mods.ctrl, MOD_CONTROL),
        (mods.alt, MOD_ALT),
        (mods.shift, MOD_SHIFT),
        (mods.meta, MOD_WIN),
    ] {
        if on {
            out |= bit;
        }
    }
    out
}

fn key(key: Key) -> Option<VIRTUAL_KEY> {
    match key {
        Key::Char(c) => {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() {
                Some(VIRTUAL_KEY(VK_A.0 + (c as u16 - 'a' as u16)))
            } else if c.is_ascii_digit() {
                Some(VIRTUAL_KEY(VK_0.0 + (c as u16 - '0' as u16)))
            } else {
                None
            }
        }
        Key::F(n) if (1..=24).contains(&n) => Some(VIRTUAL_KEY(VK_F1.0 + u16::from(n) - 1)),
        Key::F(_) => None,
        Key::Named(named) => Some(match named {
            Named::Space => VK_SPACE,
            Named::Enter => VK_RETURN,
            Named::Tab => VK_TAB,
            Named::Escape => VK_ESCAPE,
            Named::Backspace => VK_BACK,
            Named::Delete => VK_DELETE,
            Named::Insert => VK_INSERT,
            Named::Home => VK_HOME,
            Named::End => VK_END,
            Named::PageUp => VK_PRIOR,
            Named::PageDown => VK_NEXT,
            Named::Up => VK_UP,
            Named::Down => VK_DOWN,
            Named::Left => VK_LEFT,
            Named::Right => VK_RIGHT,
            Named::PrintScreen => VK_SNAPSHOT,
            Named::Pause => VK_PAUSE,
            Named::VolumeUp => VK_VOLUME_UP,
            Named::VolumeDown => VK_VOLUME_DOWN,
            Named::VolumeMute => VK_VOLUME_MUTE,
            Named::MediaNext => VK_MEDIA_NEXT_TRACK,
            Named::MediaPrevious => VK_MEDIA_PREV_TRACK,
            Named::MediaPlayPause => VK_MEDIA_PLAY_PAUSE,
            Named::MediaStop => VK_MEDIA_STOP,
        }),
    }
}

/// The keys to hold for `mods`, in the order they go down. The left-hand
/// keys, because that is what a physical keyboard sends for a plain ctrl.
pub fn modifier_keys(mods: Mods) -> Vec<VIRTUAL_KEY> {
    [
        (mods.ctrl, VK_LCONTROL),
        (mods.alt, VK_LMENU),
        (mods.shift, VK_LSHIFT),
        (mods.meta, VK_LWIN),
    ]
    .into_iter()
    .filter_map(|(on, vk)| on.then_some(vk))
    .collect()
}

/// Whether `vk` sits behind the 0xE0 scan code prefix. Sent without the
/// extended flag, the arrows and the navigation block arrive as the numeric
/// keypad's twins, so an application reading scan codes sees "keypad 8"
/// where a plugin pressed Up. The media and volume keys share one scan
/// code each with a letter, B for volume up, unless the prefix is there.
pub fn extended(vk: VIRTUAL_KEY) -> bool {
    matches!(
        vk,
        VK_INSERT
            | VK_DELETE
            | VK_HOME
            | VK_END
            | VK_PRIOR
            | VK_NEXT
            | VK_UP
            | VK_DOWN
            | VK_LEFT
            | VK_RIGHT
            | VK_LWIN
            | VK_RWIN
            | VK_APPS
            | VK_SNAPSHOT
            | VK_DIVIDE
            | VK_NUMLOCK
            | VK_RCONTROL
            | VK_RMENU
            | VK_VOLUME_UP
            | VK_VOLUME_DOWN
            | VK_VOLUME_MUTE
            | VK_MEDIA_NEXT_TRACK
            | VK_MEDIA_PREV_TRACK
            | VK_MEDIA_PLAY_PAUSE
            | VK_MEDIA_STOP
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_9, VK_F12, VK_F24, VK_Z};

    fn parsed(text: &str) -> (HOT_KEY_MODIFIERS, VIRTUAL_KEY) {
        chord(&text.parse().unwrap()).unwrap()
    }

    #[test]
    fn letters_and_digits_are_their_ascii_codes() {
        assert_eq!(parsed("a").1, VK_A);
        assert_eq!(parsed("z").1, VK_Z);
        assert_eq!(parsed("0").1, VK_0);
        assert_eq!(parsed("9").1, VK_9);
        // A chord deserialized rather than parsed may carry a capital.
        let upper = Chord {
            mods: Mods::default(),
            key: Key::Char('Q'),
        };
        assert_eq!(chord(&upper).unwrap().1, VIRTUAL_KEY(0x51));
    }

    #[test]
    fn every_function_key_maps_and_nothing_past_f24() {
        for n in 1..=24u8 {
            let c = Chord {
                mods: Mods::default(),
                key: Key::F(n),
            };
            assert_eq!(
                chord(&c).unwrap().1,
                VIRTUAL_KEY(VK_F1.0 + u16::from(n) - 1)
            );
        }
        assert_eq!(parsed("f12").1, VK_F12);
        assert_eq!(parsed("f24").1, VK_F24);
        for n in [0, 25, 255] {
            let c = Chord {
                mods: Mods::default(),
                key: Key::F(n),
            };
            assert!(chord(&c).is_none(), "f{n}");
        }
    }

    #[test]
    fn a_char_that_is_not_a_letter_or_a_digit_has_no_key() {
        for c in ['@', ' ', 'é', '\n'] {
            let ch = Chord {
                mods: Mods::default(),
                key: Key::Char(c),
            };
            assert!(chord(&ch).is_none(), "{c:?}");
        }
    }

    #[test]
    fn every_named_key_maps_to_its_own_virtual_key() {
        let all = [
            (Named::Space, VK_SPACE),
            (Named::Enter, VK_RETURN),
            (Named::Tab, VK_TAB),
            (Named::Escape, VK_ESCAPE),
            (Named::Backspace, VK_BACK),
            (Named::Delete, VK_DELETE),
            (Named::Insert, VK_INSERT),
            (Named::Home, VK_HOME),
            (Named::End, VK_END),
            (Named::PageUp, VK_PRIOR),
            (Named::PageDown, VK_NEXT),
            (Named::Up, VK_UP),
            (Named::Down, VK_DOWN),
            (Named::Left, VK_LEFT),
            (Named::Right, VK_RIGHT),
            (Named::PrintScreen, VK_SNAPSHOT),
            (Named::Pause, VK_PAUSE),
            (Named::VolumeUp, VK_VOLUME_UP),
            (Named::VolumeDown, VK_VOLUME_DOWN),
            (Named::VolumeMute, VK_VOLUME_MUTE),
            (Named::MediaNext, VK_MEDIA_NEXT_TRACK),
            (Named::MediaPrevious, VK_MEDIA_PREV_TRACK),
            (Named::MediaPlayPause, VK_MEDIA_PLAY_PAUSE),
            (Named::MediaStop, VK_MEDIA_STOP),
        ];
        let mut seen = std::collections::HashSet::new();
        for (named, vk) in all {
            let c = Chord {
                mods: Mods::default(),
                key: Key::Named(named),
            };
            assert_eq!(chord(&c).unwrap().1, vk, "{named:?}");
            assert!(seen.insert(vk.0), "{named:?} shares a key");
        }
    }

    #[test]
    fn every_spelling_the_parser_accepts_has_a_key() {
        for text in [
            "space",
            "enter",
            "return",
            "tab",
            "esc",
            "escape",
            "backspace",
            "delete",
            "del",
            "insert",
            "ins",
            "home",
            "end",
            "pageup",
            "page_up",
            "pagedown",
            "page_down",
            "up",
            "down",
            "left",
            "right",
            "printscreen",
            "print_screen",
            "pause",
            "volume_up",
            "volume_down",
            "volume_mute",
            "mute",
            "media_next",
            "media_previous",
            "media_prev",
            "media_play_pause",
            "play_pause",
            "media_stop",
        ] {
            assert!(chord(&text.parse().unwrap()).is_some(), "{text}");
        }
    }

    #[test]
    fn modifiers_become_hotkey_bits_and_held_keys() {
        let (mods, vk) = parsed("ctrl+alt+shift+win+t");
        assert_eq!(mods, MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_WIN);
        assert_eq!(vk, VIRTUAL_KEY(0x54));
        assert_eq!(parsed("t").0, HOT_KEY_MODIFIERS(0));
        assert_eq!(parsed("alt+f4").0, MOD_ALT);

        let c: Chord = "win+shift+s".parse().unwrap();
        assert_eq!(modifier_keys(c.mods), vec![VK_LSHIFT, VK_LWIN]);
        let c: Chord = "ctrl+alt+shift+win+s".parse().unwrap();
        assert_eq!(
            modifier_keys(c.mods),
            vec![VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN]
        );
        assert!(modifier_keys(Mods::default()).is_empty());
    }

    #[test]
    fn the_navigation_block_is_extended_and_letters_are_not() {
        for vk in [
            VK_UP,
            VK_DOWN,
            VK_LEFT,
            VK_RIGHT,
            VK_INSERT,
            VK_DELETE,
            VK_HOME,
            VK_END,
            VK_PRIOR,
            VK_NEXT,
            VK_LWIN,
            VK_VOLUME_UP,
            VK_MEDIA_PLAY_PAUSE,
        ] {
            assert!(extended(vk), "{vk:?}");
        }
        for vk in [
            VK_A,
            VK_0,
            VK_F1,
            VK_RETURN,
            VK_SPACE,
            VK_LCONTROL,
            VK_LMENU,
            VK_PAUSE,
        ] {
            assert!(!extended(vk), "{vk:?}");
        }
    }
}
