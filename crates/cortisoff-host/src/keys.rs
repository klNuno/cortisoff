//! Key chords as plugins write them, `ctrl+alt+n`, parsed once here so every
//! platform agrees on the syntax and only maps the result to its own codes.

use serde::{Deserialize, Serialize};

/// Modifier keys held with a chord. `meta` is the Windows key, Super on
/// Linux and Command on macOS.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

/// The key a chord ends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    /// A letter or a digit, lowercase.
    Char(char),
    /// F1 to F24.
    F(u8),
    Named(Named),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Named {
    Space,
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    PrintScreen,
    Pause,
    VolumeUp,
    VolumeDown,
    VolumeMute,
    MediaNext,
    MediaPrevious,
    MediaPlayPause,
    MediaStop,
}

/// Modifiers plus one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

/// A global hotkey a plugin asked for. `id` comes back in
/// [`crate::Event::Hotkey`] when it is pressed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    pub id: u32,
    pub chord: Chord,
}

impl std::str::FromStr for Chord {
    type Err = String;

    /// `ctrl+alt+n`, `shift+f5`, `win+space`, `ctrl+volume_up`. Case does not
    /// matter. The last part is the key, every other part a modifier.
    fn from_str(text: &str) -> Result<Self, String> {
        let parts: Vec<String> = text
            .split('+')
            .map(|p| p.trim().to_ascii_lowercase())
            .collect();
        let Some((last, mods_text)) = parts.split_last() else {
            return Err("an empty key chord".to_owned());
        };
        let mut mods = Mods::default();
        for m in mods_text {
            match m.as_str() {
                "ctrl" | "control" => mods.ctrl = true,
                "alt" | "option" => mods.alt = true,
                "shift" => mods.shift = true,
                "win" | "meta" | "super" | "cmd" | "command" => mods.meta = true,
                other => return Err(format!("{other:?} is not a modifier in {text:?}")),
            }
        }
        let key = parse_key(last).ok_or_else(|| format!("{last:?} is not a key in {text:?}"))?;
        Ok(Chord { mods, key })
    }
}

fn parse_key(s: &str) -> Option<Key> {
    let mut chars = s.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphanumeric() {
            return Some(Key::Char(c.to_ascii_lowercase()));
        }
    }
    if let Some(n) = s.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        if (1..=24).contains(&n) {
            return Some(Key::F(n));
        }
    }
    let named = match s {
        "space" => Named::Space,
        "enter" | "return" => Named::Enter,
        "tab" => Named::Tab,
        "esc" | "escape" => Named::Escape,
        "backspace" => Named::Backspace,
        "delete" | "del" => Named::Delete,
        "insert" | "ins" => Named::Insert,
        "home" => Named::Home,
        "end" => Named::End,
        "pageup" | "page_up" => Named::PageUp,
        "pagedown" | "page_down" => Named::PageDown,
        "up" => Named::Up,
        "down" => Named::Down,
        "left" => Named::Left,
        "right" => Named::Right,
        "printscreen" | "print_screen" => Named::PrintScreen,
        "pause" => Named::Pause,
        "volume_up" => Named::VolumeUp,
        "volume_down" => Named::VolumeDown,
        "volume_mute" | "mute" => Named::VolumeMute,
        "media_next" => Named::MediaNext,
        "media_previous" | "media_prev" => Named::MediaPrevious,
        "media_play_pause" | "play_pause" => Named::MediaPlayPause,
        "media_stop" => Named::MediaStop,
        _ => return None,
    };
    Some(Key::Named(named))
}

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let m = self.mods;
        for (on, name) in [
            (m.ctrl, "ctrl"),
            (m.alt, "alt"),
            (m.shift, "shift"),
            (m.meta, "win"),
        ] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            Key::Char(c) => write!(f, "{c}"),
            Key::F(n) => write!(f, "f{n}"),
            Key::Named(n) => write!(f, "{}", format!("{n:?}").to_ascii_lowercase()),
        }
    }
}

/// A top-level window, as the AutoHotkey layer of the plugin library sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Opaque, valid while the window lives.
    pub id: u64,
    pub title: String,
    pub exe: String,
    pub pid: u32,
    pub focused: bool,
}

/// What a plugin can do to a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    Activate,
    Minimize,
    Maximize,
    Restore,
    Close,
}

impl std::str::FromStr for WindowAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "activate" | "focus" => Self::Activate,
            "minimize" => Self::Minimize,
            "maximize" => Self::Maximize,
            "restore" => Self::Restore,
            "close" => Self::Close,
            other => return Err(format!("{other:?} is not a window action")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chord_reads_its_modifiers_and_its_key() {
        let c: Chord = "Ctrl+Alt+N".parse().unwrap();
        assert!(c.mods.ctrl && c.mods.alt && !c.mods.shift && !c.mods.meta);
        assert_eq!(c.key, Key::Char('n'));
        assert_eq!(c.to_string(), "ctrl+alt+n");
        let c: Chord = "win+f12".parse().unwrap();
        assert!(c.mods.meta);
        assert_eq!(c.key, Key::F(12));
        let c: Chord = "shift+volume_up".parse().unwrap();
        assert_eq!(c.key, Key::Named(Named::VolumeUp));
    }

    #[test]
    fn a_wrong_chord_says_which_part() {
        assert!("ctrl+hyper+n".parse::<Chord>().unwrap_err().contains("hyper"));
        assert!("ctrl+f99".parse::<Chord>().unwrap_err().contains("f99"));
        assert!("".parse::<Chord>().is_err());
    }
}
