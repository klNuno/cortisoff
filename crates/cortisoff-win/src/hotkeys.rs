//! Global hotkeys, registered on the pump's own window so `WM_HOTKEY`
//! arrives in the same queue `poll` already drains.
//!
//! A plugin set hands over the whole list each time. What it still wants
//! stays registered, untouched: unregistering and registering again would
//! open a window in which another application could take the chord.

use std::collections::{BTreeMap, HashMap};

use cortisoff_host::{Chord, Hotkey};
use windows::core::HRESULT;
use windows::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, HWND};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT, VIRTUAL_KEY,
};

use crate::keymap;

/// The range Windows leaves to applications. 0xC000 and above belong to
/// shared DLLs.
const LAST_ID: i32 = 0xBFFF;

pub struct Hotkeys {
    window: HWND,
    /// Win32 id to the plugin's hotkey it stands for.
    held: BTreeMap<i32, Hotkey>,
}

impl Hotkeys {
    pub fn new(window: HWND) -> Self {
        Self {
            window,
            held: BTreeMap::new(),
        }
    }

    /// Replace the registered set with `wanted`. Answers what was refused,
    /// with a sentence a person can act on.
    pub fn set(&mut self, wanted: &[Hotkey]) -> Vec<(u32, String)> {
        let plan = plan(&self.held, wanted);
        // Every removal before any addition: a chord moving from one id to
        // another is free again only once its old registration is gone.
        for id in plan.drop {
            let _ = unsafe { UnregisterHotKey(self.window, id) };
            self.held.remove(&id);
        }
        let mut refused = plan.refused;
        for (hotkey, mods, vk) in plan.add {
            let Some(id) = free_id(&self.held) else {
                refused.push((
                    hotkey.id,
                    format!("{} was not registered: too many hotkeys", hotkey.chord),
                ));
                continue;
            };
            match unsafe { RegisterHotKey(self.window, id, mods | MOD_NOREPEAT, u32::from(vk.0)) } {
                Ok(()) => {
                    self.held.insert(id, hotkey);
                }
                Err(e) => refused.push((hotkey.id, refusal(&hotkey.chord, e.code(), &e.message()))),
            }
        }
        refused
    }

    /// The plugin's id for the Win32 id a `WM_HOTKEY` carried. `None` for
    /// one this set no longer holds, such as a press already queued when
    /// the set changed, and for the system's own negative ids.
    pub fn pressed(&self, id: i32) -> Option<u32> {
        self.held.get(&id).map(|h| h.id)
    }

    /// Unregister everything. Has to run while the window still exists.
    pub fn clear(&mut self) {
        for id in std::mem::take(&mut self.held).into_keys() {
            let _ = unsafe { UnregisterHotKey(self.window, id) };
        }
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        self.clear();
    }
}

/// What a new set changes against what is held.
#[derive(Debug, Default)]
struct Plan {
    /// Win32 ids to unregister.
    drop: Vec<i32>,
    /// New registrations, already mapped to Win32 codes.
    add: Vec<(Hotkey, HOT_KEY_MODIFIERS, VIRTUAL_KEY)>,
    /// Refused before Windows was asked.
    refused: Vec<(u32, String)>,
}

fn plan(held: &BTreeMap<i32, Hotkey>, wanted: &[Hotkey]) -> Plan {
    let mut out = Plan::default();
    // Duplicates are refused here, never sent to Windows: the second
    // registration of a chord this thread already holds fails with the same
    // code as one held by another application, and "another application
    // already uses it" would then be a lie.
    let mut by_id: HashMap<u32, &Chord> = HashMap::new();
    let mut by_chord: HashMap<&Chord, u32> = HashMap::new();
    let mut accepted: Vec<&Hotkey> = Vec::new();
    for hotkey in wanted {
        if let Some(chord) = by_id.get(&hotkey.id) {
            out.refused.push((
                hotkey.id,
                format!(
                    "hotkey {} is already {chord}, so {} was not registered",
                    hotkey.id, hotkey.chord
                ),
            ));
            continue;
        }
        if let Some(other) = by_chord.get(&hotkey.chord) {
            out.refused.push((
                hotkey.id,
                format!("{} is already hotkey {other}", hotkey.chord),
            ));
            continue;
        }
        by_id.insert(hotkey.id, &hotkey.chord);
        by_chord.insert(&hotkey.chord, hotkey.id);
        accepted.push(hotkey);
    }

    for (id, hotkey) in held {
        if !accepted.contains(&hotkey) {
            out.drop.push(*id);
        }
    }
    for hotkey in accepted {
        if held.values().any(|h| h == hotkey) {
            continue;
        }
        match keymap::chord(&hotkey.chord) {
            Some((mods, vk)) => out.add.push((hotkey.clone(), mods, vk)),
            None => out.refused.push((
                hotkey.id,
                format!("{} has no key Windows can register", hotkey.chord),
            )),
        }
    }
    out
}

/// The lowest Win32 id not in use, so a long session of reloads never runs
/// out of a range that is only 49151 wide.
fn free_id(held: &BTreeMap<i32, Hotkey>) -> Option<i32> {
    (1..=LAST_ID).find(|id| !held.contains_key(id))
}

fn refusal(chord: &Chord, code: HRESULT, message: &str) -> String {
    if code == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult() {
        format!("another application already uses {chord}")
    } else {
        format!("Windows refused {chord}: {message}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::ERROR_ACCESS_DENIED;
    use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL};

    fn hk(id: u32, chord: &str) -> Hotkey {
        Hotkey {
            id,
            chord: chord.parse().unwrap(),
        }
    }

    fn held(entries: &[(i32, Hotkey)]) -> BTreeMap<i32, Hotkey> {
        entries.iter().cloned().collect()
    }

    #[test]
    fn a_first_set_registers_everything() {
        let p = plan(&BTreeMap::new(), &[hk(1, "ctrl+alt+t"), hk(2, "f9")]);
        assert!(p.drop.is_empty());
        assert!(p.refused.is_empty());
        assert_eq!(p.add.len(), 2);
        assert_eq!(p.add[0].0, hk(1, "ctrl+alt+t"));
        assert_eq!(p.add[0].1, MOD_CONTROL | MOD_ALT);
        assert_eq!(p.add[0].2, VIRTUAL_KEY(0x54));
    }

    #[test]
    fn what_is_unchanged_is_left_alone() {
        let now = held(&[(1, hk(10, "ctrl+alt+t")), (2, hk(11, "f9"))]);
        let p = plan(&now, &[hk(11, "f9"), hk(10, "ctrl+alt+t")]);
        assert!(p.drop.is_empty() && p.add.is_empty() && p.refused.is_empty());
    }

    #[test]
    fn what_is_gone_is_dropped_and_what_is_new_is_added() {
        let now = held(&[(1, hk(10, "ctrl+alt+t")), (2, hk(11, "f9"))]);
        let p = plan(&now, &[hk(10, "ctrl+alt+t"), hk(12, "f10")]);
        assert_eq!(p.drop, vec![2]);
        assert_eq!(p.add.len(), 1);
        assert_eq!(p.add[0].0, hk(12, "f10"));
    }

    #[test]
    fn a_chord_that_changes_under_the_same_id_is_registered_again() {
        let now = held(&[(1, hk(10, "ctrl+alt+t"))]);
        let p = plan(&now, &[hk(10, "ctrl+alt+y")]);
        assert_eq!(p.drop, vec![1]);
        assert_eq!(p.add[0].0, hk(10, "ctrl+alt+y"));
    }

    #[test]
    fn a_chord_moving_to_another_id_is_dropped_then_added() {
        let now = held(&[(1, hk(10, "ctrl+alt+t"))]);
        let p = plan(&now, &[hk(20, "ctrl+alt+t")]);
        assert_eq!(p.drop, vec![1]);
        assert_eq!(p.add[0].0, hk(20, "ctrl+alt+t"));
    }

    #[test]
    fn an_empty_set_drops_everything() {
        let now = held(&[(1, hk(10, "ctrl+alt+t")), (7, hk(11, "f9"))]);
        let p = plan(&now, &[]);
        assert_eq!(p.drop, vec![1, 7]);
        assert!(p.add.is_empty());
    }

    #[test]
    fn duplicates_are_refused_before_windows_is_asked() {
        let p = plan(
            &BTreeMap::new(),
            &[hk(1, "ctrl+alt+t"), hk(2, "ctrl+alt+t"), hk(1, "f9")],
        );
        assert_eq!(p.add.len(), 1);
        assert_eq!(p.add[0].0, hk(1, "ctrl+alt+t"));
        assert_eq!(p.refused.len(), 2);
        assert_eq!(p.refused[0].0, 2);
        assert_eq!(p.refused[0].1, "ctrl+alt+t is already hotkey 1");
        assert_eq!(p.refused[1].0, 1);
        assert!(p.refused[1].1.contains("f9"), "{}", p.refused[1].1);
    }

    #[test]
    fn a_chord_with_no_key_is_refused() {
        let odd = Hotkey {
            id: 5,
            chord: Chord {
                mods: Default::default(),
                key: cortisoff_host::Key::F(30),
            },
        };
        let p = plan(&BTreeMap::new(), &[odd]);
        assert!(p.add.is_empty());
        assert_eq!(p.refused.len(), 1);
        assert_eq!(p.refused[0].0, 5);
    }

    #[test]
    fn ids_are_reused_from_the_bottom() {
        assert_eq!(free_id(&BTreeMap::new()), Some(1));
        let now = held(&[(1, hk(10, "a")), (2, hk(11, "b")), (4, hk(12, "c"))]);
        assert_eq!(free_id(&now), Some(3));
    }

    #[test]
    fn a_refusal_says_who_holds_the_chord() {
        let chord: Chord = "ctrl+alt+t".parse().unwrap();
        assert_eq!(
            refusal(&chord, ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult(), "x"),
            "another application already uses ctrl+alt+t"
        );
        assert_eq!(
            refusal(
                &chord,
                ERROR_ACCESS_DENIED.to_hresult(),
                "Access is denied."
            ),
            "Windows refused ctrl+alt+t: Access is denied."
        );
    }
}
