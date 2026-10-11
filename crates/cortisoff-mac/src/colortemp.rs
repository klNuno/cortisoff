//! `display.color_temp` on macOS, through each display's transfer table.
//!
//! `CGSetDisplayTransferByTable` is the output lookup table, the same layer
//! f.lux and Night Shift's predecessors wrote: it tints the cursor and a
//! fullscreen game along with everything else. The tint scales the table the
//! display already had rather than replacing it, so a ColorSync calibration
//! survives under the tint.
//!
//! WindowServer is reported to put the profiles back when the process that
//! changed a table exits, which would make this volatile. That has not been
//! seen on hardware here, so the actuator journals its baseline like the
//! Windows one: a restore that turns out unneeded costs one table write, and
//! a missing one leaves a screen orange.

use std::cell::RefCell;
use std::collections::BTreeSet;

use cortisoff_host::{ids, Actuator, ActuatorError, CapabilityId, Control, Value};
use objc2_core_graphics::{
    CGDisplayGammaTableCapacity, CGDisplayRestoreColorSyncSettings, CGError,
    CGGetDisplayTransferByTable, CGSetDisplayTransferByTable,
};

use crate::color::{self, Restored, Rgb, Table};
use crate::screen;

pub struct ColorTemp {
    /// The baseline as this process captured it, which is what a tint scales
    /// and what a release puts back. `None` between claims.
    captured: RefCell<Option<Vec<(u32, Table)>>>,
    /// Displays whose table carries this process's tint right now. A display
    /// in here is never captured again: its table is the tint, and taking it
    /// as a baseline is how a screen stays orange for good.
    live: RefCell<BTreeSet<u32>>,
}

impl ColorTemp {
    /// `None` when no display gives up its table, which is when this
    /// capability honestly does not exist: a headless machine, or a session
    /// with no WindowServer behind it.
    pub fn new() -> Option<Self> {
        screen::active()
            .into_iter()
            .any(|id| read(id).is_some())
            .then(|| Self {
                captured: RefCell::new(None),
                live: RefCell::new(BTreeSet::new()),
            })
    }

    pub fn backend_name(&self) -> &'static str {
        "transfer_table"
    }

    fn apply_tint(&self, c: &Rgb) -> Result<(), ActuatorError> {
        let captured = self.captured.borrow();
        let mut live = self.live.borrow_mut();
        let mut refused = Vec::new();
        let mut carried = false;
        for id in screen::active() {
            // A display that arrived after the capture is tinted from the
            // straight line until the next display change grows the
            // baseline through `recapture`.
            let base = captured
                .iter()
                .flatten()
                .find(|(d, _)| *d == id)
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| Table::identity(capacity(id).max(256)));
            if write(id, &base.tinted(c)) {
                live.insert(id);
                carried = true;
            } else {
                refused.push(id);
            }
        }
        if carried {
            Ok(())
        } else {
            Err(ActuatorError::on(
                self.id(),
                format!("no display accepted a transfer table (refused by {refused:?})"),
            ))
        }
    }

    /// Put back exactly what was captured, display by display.
    fn restore(&self, encoded: &str) -> Result<(), ActuatorError> {
        match color::restore(encoded, &screen::active(), write) {
            Restored::Done => {}
            Restored::Failed(ids) => {
                // Keep the baseline: the dispatcher keeps the journal entry
                // on an error, and a retry needs both.
                return Err(ActuatorError::on(
                    self.id(),
                    format!("could not restore the transfer table of displays {ids:?}"),
                ));
            }
            // Nothing readable is left of the baseline. The profiles are the
            // closest thing to it there is, and leaving the tint is worse.
            Restored::Unreadable => CGDisplayRestoreColorSyncSettings(),
        }
        self.captured.borrow_mut().take();
        self.live.borrow_mut().clear();
        Ok(())
    }
}

impl Actuator for ColorTemp {
    fn id(&self) -> CapabilityId {
        CapabilityId::new(ids::DISPLAY_COLOR_TEMP)
    }

    /// Every active display's whole table, not a temperature: a calibrated
    /// display does not sit at the identity, so "restore 6600 K" would
    /// silently drop the user's profile.
    fn capture(&self) -> Result<Value, ActuatorError> {
        let live = self.live.borrow();
        let tables: Vec<(u32, Table)> = screen::active()
            .into_iter()
            .filter(|id| !live.contains(id))
            .filter_map(|id| read(id).map(|t| (id, t)))
            .collect();
        if tables.is_empty() {
            return Err(ActuatorError::on(
                self.id(),
                "no display would give up its transfer table",
            ));
        }
        let encoded = color::encode(&tables);
        *self.captured.borrow_mut() = Some(tables);
        Ok(Value::Text(encoded))
    }

    /// A display arrived, or came back, while a tint was held. Only what is
    /// missing from the baseline is read, and never a display this claim is
    /// already tinting.
    fn recapture(&self) -> Option<Result<Value, ActuatorError>> {
        let mut held = self.captured.borrow_mut();
        let tables = held.as_mut()?;
        let live = self.live.borrow();
        let fresh: Vec<(u32, Table)> = screen::active()
            .into_iter()
            .filter(|id| !tables.iter().any(|(d, _)| d == id))
            .filter(|id| !live.contains(id))
            .filter_map(|id| read(id).map(|t| (id, t)))
            .collect();
        if fresh.is_empty() {
            return None;
        }
        tables.extend(fresh);
        Some(Ok(Value::Text(color::encode(tables))))
    }

    fn apply(&self, value: &Value) -> Result<(), ActuatorError> {
        match value {
            Value::Int(kelvin) => self.apply_tint(&color::from_kelvin(*kelvin as f32)),
            // A captured baseline coming back, from a release or from the
            // journal after a crash.
            Value::Text(encoded) => self.restore(encoded),
            other => Err(ActuatorError::on(
                self.id(),
                format!("expected a colour temperature in kelvin, got {other:?}"),
            )),
        }
    }

    fn volatile(&self) -> bool {
        false
    }

    /// 6600 K, where the curve leaves every channel at full and the display
    /// shows its own profile.
    fn rest(&self) -> Option<Value> {
        Some(Value::Int(color::NEUTRAL_KELVIN))
    }

    fn control(&self) -> Control {
        Control::Range {
            min: 1500,
            max: color::NEUTRAL_KELVIN,
            step: 50,
            default: 3400,
            unit: Some("K".to_owned()),
        }
    }
}

fn capacity(id: u32) -> usize {
    CGDisplayGammaTableCapacity(id) as usize
}

/// The table a display shows right now.
fn read(id: u32) -> Option<Table> {
    let capacity = capacity(id);
    if capacity == 0 {
        return None;
    }
    let mut table = Table {
        red: vec![0.0; capacity],
        green: vec![0.0; capacity],
        blue: vec![0.0; capacity],
    };
    let mut samples = 0u32;
    // SAFETY: each buffer holds `capacity` entries, and `samples` is a valid
    // out pointer.
    let status = unsafe {
        CGGetDisplayTransferByTable(
            id,
            capacity as u32,
            table.red.as_mut_ptr(),
            table.green.as_mut_ptr(),
            table.blue.as_mut_ptr(),
            &mut samples,
        )
    };
    let samples = samples as usize;
    if status != CGError::Success || samples == 0 || samples > capacity {
        return None;
    }
    table.red.truncate(samples);
    table.green.truncate(samples);
    table.blue.truncate(samples);
    Some(table)
}

fn write(id: u32, table: &Table) -> bool {
    if !table.is_valid() {
        return false;
    }
    // SAFETY: the three channels hold `table.len()` entries each, checked
    // by `is_valid`.
    let status = unsafe {
        CGSetDisplayTransferByTable(
            id,
            table.len() as u32,
            table.red.as_ptr(),
            table.green.as_ptr(),
            table.blue.as_ptr(),
        )
    };
    status == CGError::Success
}
