//! `display.mode` on macOS, through `CGDisplaySetDisplayMode` on the main
//! display.
//!
//! Only the main display, the one with the menu bar: macOS gives no cheap way
//! to tell which display a fullscreen game took, and a rule aimed at the game
//! must not move a second monitor instead. The choice of mode itself is in
//! [`crate::modes`], where it is tested.

use cortisoff_host::{ids, Actuator, ActuatorError, CapabilityId, Control, Value};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString};
use objc2_core_graphics::{
    kCGDisplayShowDuplicateLowResolutionModes, CGDisplayCopyAllDisplayModes,
    CGDisplayCopyDisplayMode, CGDisplayMode, CGDisplaySetDisplayMode, CGError, CGMainDisplayID,
};

use crate::modes::{self, Setting};

pub struct DisplayMode;

impl DisplayMode {
    /// `None` when the main display will not say what mode it is in, which
    /// is a session with no WindowServer behind it.
    pub fn new() -> Option<Self> {
        current(CGMainDisplayID()).map(|_| Self)
    }

    pub fn backend_name(&self) -> &'static str {
        "cgdisplaymode"
    }

    /// Switches the main display to the mode at `pick`'s choice, or does
    /// nothing when it is already there, so a redundant apply costs no
    /// flash.
    fn switch(
        &self,
        display: u32,
        choose: impl FnOnce(&[Setting], &Setting) -> Option<usize>,
    ) -> Result<(), ActuatorError> {
        let now = current(display)
            .ok_or_else(|| ActuatorError::on(self.id(), "the main display has no current mode"))?;
        let (settings, handles) = all(display);
        let Some(index) = choose(&settings, &now) else {
            return Err(ActuatorError::on(
                self.id(),
                "the main display offers no mode of that size",
            ));
        };
        if settings[index] == now {
            return Ok(());
        }
        // SAFETY: a mode this display listed, and no options.
        let status = unsafe { CGDisplaySetDisplayMode(display, Some(&handles[index]), None) };
        if status == CGError::Success {
            Ok(())
        } else {
            Err(ActuatorError::on(
                self.id(),
                format!("CGDisplaySetDisplayMode failed with {}", status.0),
            ))
        }
    }
}

impl Actuator for DisplayMode {
    fn id(&self) -> CapabilityId {
        CapabilityId::new(ids::DISPLAY_MODE)
    }

    fn capture(&self) -> Result<Value, ActuatorError> {
        let display = CGMainDisplayID();
        current(display)
            .map(|now| Value::Text(modes::encode(&[(display, now)])))
            .ok_or_else(|| ActuatorError::on(self.id(), "the main display has no current mode"))
    }

    fn apply(&self, value: &Value) -> Result<(), ActuatorError> {
        let display = CGMainDisplayID();
        match value {
            Value::Mode {
                width,
                height,
                refresh_mhz,
            } => self.switch(display, |all, now| {
                modes::pick(all, now, *width, *height, *refresh_mhz)
            }),
            // A captured baseline coming back. The main display can change
            // between the capture and the restore; the saved entry for the
            // display that is main now wins, and the first entry stands in
            // when none matches, because the mode is what the user had on
            // the screen with the menu bar.
            Value::Text(encoded) => {
                let saved = modes::decode(encoded);
                let Some(&(_, wanted)) = saved
                    .iter()
                    .find(|(d, _)| *d == display)
                    .or_else(|| saved.first())
                else {
                    return if encoded.trim().is_empty() {
                        Ok(())
                    } else {
                        Err(ActuatorError::on(
                            self.id(),
                            "the saved display mode does not parse",
                        ))
                    };
                };
                self.switch(display, |all, _| modes::find_saved(all, &wanted))
            }
            other => Err(ActuatorError::on(
                self.id(),
                format!("expected a display mode, got {other:?}"),
            )),
        }
    }

    /// The mode outlives the process, and a crash at 720p is the worst thing
    /// this daemon could leave behind. The journal is the only way back.
    fn volatile(&self) -> bool {
        false
    }

    fn control(&self) -> Control {
        Control::Text {
            hint: "2560x1440@120 or @120".to_owned(),
        }
    }
}

fn setting(mode: &CGDisplayMode) -> Setting {
    let mode = Some(mode);
    Setting {
        width: CGDisplayMode::width(mode) as u32,
        height: CGDisplayMode::height(mode) as u32,
        pixel_width: CGDisplayMode::pixel_width(mode) as u32,
        pixel_height: CGDisplayMode::pixel_height(mode) as u32,
        refresh_mhz: (CGDisplayMode::refresh_rate(mode) * 1000.0)
            .round()
            .max(0.0) as u32,
        io_id: CGDisplayMode::io_display_mode_id(mode),
    }
}

fn current(display: u32) -> Option<Setting> {
    CGDisplayCopyDisplayMode(display).map(|m| setting(&m))
}

/// Every mode the desktop can use, with the handle to switch to each. The
/// low resolution twins of HiDPI modes are asked for: without them a Retina
/// display lists only half of what System Settings offers.
fn all(display: u32) -> (Vec<Setting>, Vec<CFRetained<CGDisplayMode>>) {
    // SAFETY: reading a constant CoreGraphics exports.
    let key: &CFString = unsafe { kCGDisplayShowDuplicateLowResolutionModes };
    let options = CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[CFBoolean::new(true)]);
    // SAFETY: the options dictionary holds the one documented key with a
    // boolean, which is its documented type.
    let options: &CFDictionary = AsRef::<CFDictionary>::as_ref(&*options);
    let Some(list) = (unsafe { CGDisplayCopyAllDisplayModes(display, Some(options)) }) else {
        return (Vec::new(), Vec::new());
    };
    // SAFETY: CoreGraphics documents the array as holding display modes.
    let list: CFRetained<CFArray<CGDisplayMode>> = unsafe { CFRetained::cast_unchecked(list) };
    list.iter()
        .filter(|m| CGDisplayMode::is_usable_for_desktop_gui(Some(m)))
        .map(|m| (setting(&m), m))
        .unzip()
}
