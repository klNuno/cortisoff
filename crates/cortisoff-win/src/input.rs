//! Synthetic keyboard input: a chord pressed, text typed, as if from the
//! keyboard. Both land in whatever window has the focus.
//!
//! Each call is one `SendInput` batch, because the system inserts a batch
//! into the input stream as a unit: nothing the person types can land in
//! the middle of it, between a modifier going down and the key it holds.

use cortisoff_host::Chord;
use windows::Win32::Foundation::GetLastError;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, VIRTUAL_KEY,
    VK_LMENU, VK_RETURN,
};

use crate::keymap;

/// Press and release `chord`: modifiers down, key down, key up, modifiers
/// up in the reverse order.
pub fn send_chord(chord: &Chord) -> Result<(), String> {
    let (_, vk) =
        keymap::chord(chord).ok_or_else(|| format!("{chord} has no key Windows can press"))?;
    send(&chord_inputs(
        &keymap::modifier_keys(chord.mods),
        vk,
        scan_code,
    ))
}

/// Type `text` into the focused window, character by character, with no
/// regard for the keyboard layout: every character goes as itself.
pub fn type_text(text: &str) -> Result<(), String> {
    let inputs: Vec<INPUT> = strokes(text)
        .into_iter()
        .flat_map(|stroke| match stroke {
            Stroke::Unit(unit) => [unicode(unit, false), unicode(unit, true)],
            Stroke::Enter => [
                key(VK_RETURN, scan_code(VK_RETURN), false),
                key(VK_RETURN, scan_code(VK_RETURN), true),
            ],
        })
        .collect();
    send(&inputs)
}

/// One tap of the left Alt key. Windows lets the process that sent the last
/// input event take the foreground, which is the whole trick.
pub fn alt_tap() -> Result<(), String> {
    send(&[
        key(VK_LMENU, scan_code(VK_LMENU), false),
        key(VK_LMENU, scan_code(VK_LMENU), true),
    ])
}

fn send(inputs: &[INPUT]) -> Result<(), String> {
    if inputs.is_empty() {
        return Ok(());
    }
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        return Ok(());
    }
    let error = unsafe { GetLastError() };
    Err(format!(
        "Windows accepted {sent} of {} key events ({}). a window running as administrator refuses input from a program that is not",
        inputs.len(),
        windows::core::Error::from(error.to_hresult()).message()
    ))
}

/// The scan code a real keyboard sends for `vk`, without the 0xE0 prefix,
/// which travels as [`KEYEVENTF_EXTENDEDKEY`]. Filled in because some
/// applications, games first, read the scan code and ignore the virtual
/// key; zero there reads as no key at all.
fn scan_code(vk: VIRTUAL_KEY) -> u16 {
    (unsafe { MapVirtualKeyW(u32::from(vk.0), MAPVK_VK_TO_VSC) } & 0xFF) as u16
}

/// The batch for one chord. `scan` is a parameter so the order and the
/// flags are testable without a keyboard layout.
fn chord_inputs(
    held: &[VIRTUAL_KEY],
    vk: VIRTUAL_KEY,
    scan: impl Fn(VIRTUAL_KEY) -> u16,
) -> Vec<INPUT> {
    let mut out = Vec::with_capacity(held.len() * 2 + 2);
    out.extend(held.iter().map(|&m| key(m, scan(m), false)));
    out.push(key(vk, scan(vk), false));
    out.push(key(vk, scan(vk), true));
    out.extend(held.iter().rev().map(|&m| key(m, scan(m), true)));
    out
}

fn key(vk: VIRTUAL_KEY, scan: u16, up: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if keymap::extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    keyboard(KEYBDINPUT {
        wVk: vk,
        wScan: scan,
        dwFlags: flags,
        ..Default::default()
    })
}

fn unicode(unit: u16, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    keyboard(KEYBDINPUT {
        wVk: VIRTUAL_KEY(0),
        wScan: unit,
        dwFlags: flags,
        ..Default::default()
    })
}

fn keyboard(ki: KEYBDINPUT) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki },
    }
}

/// One thing to type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stroke {
    /// A UTF-16 code unit sent as a character. A character outside the
    /// basic plane is two of these, high surrogate first, and the
    /// receiving window puts the pair back together.
    Unit(u16),
    /// The Enter key itself. A line break sent as a character is a
    /// control code most editors ignore, where a person typing it presses
    /// Enter.
    Enter,
}

/// `\n`, `\r\n` and a lone `\r` are each one Enter, so text copied from
/// either kind of file types the same number of lines.
fn strokes(text: &str) -> Vec<Stroke> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut buffer = [0u16; 2];
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                chars.next_if_eq(&'\n');
                out.push(Stroke::Enter);
            }
            '\n' => out.push(Stroke::Enter),
            c => out.extend(c.encode_utf16(&mut buffer).iter().map(|&u| Stroke::Unit(u))),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LCONTROL, VK_LSHIFT, VK_UP};

    fn ki(input: &INPUT) -> KEYBDINPUT {
        assert_eq!(input.r#type, INPUT_KEYBOARD);
        unsafe { input.Anonymous.ki }
    }

    #[test]
    fn text_becomes_units_and_line_breaks_become_enter() {
        assert_eq!(
            strokes("a\nb"),
            vec![
                Stroke::Unit('a' as u16),
                Stroke::Enter,
                Stroke::Unit('b' as u16)
            ]
        );
        assert_eq!(strokes("x\r\ny"), strokes("x\ny"));
        assert_eq!(strokes("x\ry"), strokes("x\ny"));
        assert_eq!(strokes("\r\n\r\n"), vec![Stroke::Enter, Stroke::Enter]);
        assert_eq!(strokes("é"), vec![Stroke::Unit(0x00E9)]);
        assert!(strokes("").is_empty());
    }

    #[test]
    fn a_character_past_the_basic_plane_is_two_units() {
        // U+1F600, a face.
        assert_eq!(
            strokes("\u{1F600}"),
            vec![Stroke::Unit(0xD83D), Stroke::Unit(0xDE00)]
        );
        // Every unit typed, in order, is the string's own UTF-16.
        let text = "a\u{1F600}\u{20AC}z";
        let units: Vec<u16> = strokes(text)
            .into_iter()
            .map(|s| match s {
                Stroke::Unit(u) => u,
                Stroke::Enter => panic!("no line break here"),
            })
            .collect();
        assert_eq!(units, text.encode_utf16().collect::<Vec<_>>());
    }

    #[test]
    fn a_chord_holds_its_modifiers_around_the_key() {
        let inputs = chord_inputs(&[VK_LCONTROL, VK_LSHIFT], VIRTUAL_KEY(0x54), |vk| vk.0);
        let seen: Vec<(u16, bool)> = inputs
            .iter()
            .map(|i| {
                let k = ki(i);
                (k.wVk.0, k.dwFlags.contains(KEYEVENTF_KEYUP))
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                (VK_LCONTROL.0, false),
                (VK_LSHIFT.0, false),
                (0x54, false),
                (0x54, true),
                (VK_LSHIFT.0, true),
                (VK_LCONTROL.0, true),
            ]
        );
        assert!(inputs.iter().all(|i| ki(i).wScan == ki(i).wVk.0));
        assert!(inputs
            .iter()
            .all(|i| !ki(i).dwFlags.contains(KEYEVENTF_EXTENDEDKEY)));
    }

    #[test]
    fn an_arrow_goes_with_the_extended_flag_both_ways() {
        let inputs = chord_inputs(&[], VK_UP, |_| 0x48);
        assert_eq!(inputs.len(), 2);
        for i in &inputs {
            assert!(ki(i).dwFlags.contains(KEYEVENTF_EXTENDEDKEY));
        }
        assert!(!ki(&inputs[0]).dwFlags.contains(KEYEVENTF_KEYUP));
        assert!(ki(&inputs[1]).dwFlags.contains(KEYEVENTF_KEYUP));
    }

    #[test]
    fn a_unit_is_sent_as_a_character_not_a_key() {
        let down = ki(&unicode(0xD83D, false));
        let up = ki(&unicode(0xD83D, true));
        assert_eq!(down.wVk, VIRTUAL_KEY(0));
        assert_eq!(down.wScan, 0xD83D);
        assert_eq!(down.dwFlags, KEYEVENTF_UNICODE);
        assert_eq!(up.dwFlags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
    }
}
