//! Test doubles for everything an operating system normally provides.
//!
//! Compiled for this crate's own tests and for dependents that enable the
//! `test-util` feature. Never part of a shipping binary.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use crate::actuator::{Actuator, ActuatorError};
use crate::capability::{CapabilityId, CapabilitySet, Control};
use crate::event::Event;
use crate::event::TimerId;
use crate::journal::{JournalEntry, JournalStore};
use crate::keys::{Chord, Hotkey, WindowAction, WindowInfo};
use crate::value::Value;
use crate::Host;

/// A shared, ordered record of everything that happened, so a test can assert
/// on ordering across the journal and the actuators, which is exactly what the
/// crash-safety invariant is about.
#[derive(Debug, Clone, Default)]
pub struct Trace(Rc<RefCell<Vec<String>>>);

impl Trace {
    pub fn push(&self, event: impl Into<String>) {
        self.0.borrow_mut().push(event.into());
    }

    pub fn events(&self) -> Vec<String> {
        self.0.borrow().clone()
    }
}

#[derive(Debug)]
pub struct MockActuator {
    id: CapabilityId,
    state: RefCell<Value>,
    volatile: bool,
    trace: Trace,
    /// What the next `recapture` hands back, standing in for a monitor that
    /// arrived after the baseline was captured. Taken once, the way real
    /// hardware only moves once per event.
    grown: RefCell<Option<Value>>,
    /// Set for an actuator whose baseline is no number, as a colour
    /// temperature captures gamma ramps: `capture` hands back the first,
    /// applying it puts the second back on the screen, and `rest` is the
    /// second.
    opaque: Option<(Value, Value)>,
    /// The host's clock, which a slow capture moves on.
    clock: Rc<Cell<u64>>,
    /// How long `capture` takes, as reading a panel over DDC/CI does.
    capture_ms: Cell<u64>,
    /// How long the next `apply` takes, once, as a gamma write does while a
    /// game changes the display mode.
    apply_ms: Cell<u64>,
    /// Set for an actuator that holds outputs apart, with what each one holds.
    outputs: Option<RefCell<BTreeMap<String, Value>>>,
    /// The last baseline captured, which an apply recognises as a restore.
    captured: RefCell<Option<Value>>,
}

impl Actuator for MockActuator {
    fn id(&self) -> CapabilityId {
        self.id.clone()
    }

    fn capture(&self) -> Result<Value, ActuatorError> {
        self.trace.push(format!("capture {}", self.id));
        self.clock.set(self.clock.get() + self.capture_ms.get());
        let value = match &self.opaque {
            Some((baseline, _)) => baseline.clone(),
            None => self.state.borrow().clone(),
        };
        *self.captured.borrow_mut() = Some(value.clone());
        Ok(value)
    }

    fn apply(&self, value: &Value) -> Result<(), ActuatorError> {
        self.trace.push(format!("apply {} {:?}", self.id, value));
        self.clock.set(self.clock.get() + self.apply_ms.take());
        // The baseline back lets go of every output, as a real one does.
        if let Some(outputs) = &self.outputs {
            let baseline = match &self.opaque {
                Some((baseline, _)) => baseline == value,
                None => self.captured.borrow().as_ref() == Some(value),
            };
            if baseline {
                outputs.borrow_mut().clear();
            }
        }
        *self.state.borrow_mut() = match &self.opaque {
            Some((baseline, rest)) if baseline == value => rest.clone(),
            _ => value.clone(),
        };
        Ok(())
    }

    fn per_output(&self) -> bool {
        self.outputs.is_some()
    }

    fn apply_output(&self, output: &str, value: &Value) -> Result<(), ActuatorError> {
        let Some(outputs) = &self.outputs else {
            return Err(ActuatorError::on(self.id.clone(), "no outputs"));
        };
        self.trace
            .push(format!("apply {} on {output} {:?}", self.id, value));
        outputs
            .borrow_mut()
            .insert(output.to_owned(), value.clone());
        Ok(())
    }

    fn release_output(&self, output: &str) -> Result<(), ActuatorError> {
        let Some(outputs) = &self.outputs else {
            return Err(ActuatorError::on(self.id.clone(), "no outputs"));
        };
        self.trace.push(format!("release {} on {output}", self.id));
        outputs.borrow_mut().remove(output);
        Ok(())
    }

    fn recapture(&self) -> Option<Result<Value, ActuatorError>> {
        let grown = self.grown.borrow_mut().take()?;
        self.trace.push(format!("recapture {}", self.id));
        Some(Ok(grown))
    }

    fn volatile(&self) -> bool {
        self.volatile
    }

    /// A number rests where it is. Anything else has no resting number, as a
    /// display mode has none on a real machine.
    fn rest(&self) -> Option<Value> {
        if let Some((_, rest)) = &self.opaque {
            return Some(rest.clone());
        }
        match &*self.state.borrow() {
            value @ Value::Int(_) => Some(value.clone()),
            _ => None,
        }
    }

    /// A number is a percentage, a boolean a switch, the rest text.
    fn control(&self) -> Control {
        match &*self.state.borrow() {
            Value::Int(now) => Control::Range {
                min: 0,
                max: 100,
                step: 1,
                default: *now,
                unit: Some("%".to_owned()),
            },
            Value::Bool(_) => Control::Switch,
            _ => Control::Text {
                hint: String::new(),
            },
        }
    }
}

/// An in-memory [`JournalStore`]. Trivially durable, since the test process
/// outlives the "crash" it simulates.
#[derive(Debug, Default)]
pub struct MemJournal {
    entries: BTreeMap<CapabilityId, Value>,
    refuse_records: bool,
    trace: Trace,
}

impl MemJournal {
    pub fn new(trace: Trace) -> Self {
        Self {
            trace,
            ..Self::default()
        }
    }

    /// A journal already holding entries, as left behind by a run that died.
    pub fn seeded(trace: Trace, entries: &[(CapabilityId, Value)]) -> Self {
        Self {
            entries: entries.iter().cloned().collect(),
            refuse_records: false,
            trace,
        }
    }

    /// Every further `record` fails, simulating a disk that cannot take the
    /// baseline.
    pub fn refuse_records(&mut self) {
        self.refuse_records = true;
    }
}

impl JournalStore for MemJournal {
    fn record(&mut self, cap: &CapabilityId, baseline: &Value) -> Result<(), ActuatorError> {
        if self.refuse_records {
            return Err(ActuatorError::on(cap.clone(), "journal refused the write"));
        }
        self.trace.push(format!("journal {cap}"));
        self.entries.insert(cap.clone(), baseline.clone());
        Ok(())
    }

    fn clear(&mut self, cap: &CapabilityId) -> Result<(), ActuatorError> {
        if self.entries.remove(cap).is_some() {
            self.trace.push(format!("unjournal {cap}"));
        }
        Ok(())
    }

    fn dirty(&self) -> Result<Vec<JournalEntry>, ActuatorError> {
        Ok(self
            .entries
            .iter()
            .map(|(cap, baseline)| JournalEntry {
                cap: cap.clone(),
                baseline: baseline.clone(),
            })
            .collect())
    }
}

/// A [`Host`] with no operating system underneath.
#[derive(Debug, Default)]
pub struct MockHost {
    actuators: BTreeMap<CapabilityId, MockActuator>,
    /// Event sources `probe` reports beside the actuators.
    sources: Vec<CapabilityId>,
    pub installed: Vec<CapabilityId>,
    pub active: bool,
    /// What `set_fading` said last.
    pub fading: bool,
    pub idle_thresholds: Vec<u32>,
    pub timers: Vec<(TimerId, Duration, Duration)>,
    pub cancelled: Vec<TimerId>,
    /// What the next `poll` hands back, so a test can drive a loop the same
    /// way a real platform would.
    pub queued: Vec<Event>,
    /// What `time_of_day` answers: the wall clock a test decides.
    pub clock: Option<Event>,
    /// What `monotonic_ms` answers. A test moves it by hand.
    pub now_ms: u64,
    /// The hotkeys registered last.
    pub hotkeys: Vec<Hotkey>,
    /// Keys sent, text typed and notifications shown, in order, for asserts.
    pub desktop: Vec<String>,
    pub clipboard_text: String,
    pub open_windows: Vec<WindowInfo>,
    /// What `warnings` answers, read at each call as a real host reads its
    /// permissions.
    pub warnings: Vec<String>,
    trace: Trace,
    /// Time the actuators took, added to `now_ms`.
    spent: Rc<Cell<u64>>,
}

impl MockHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// The same trace the actuators and a [`MemJournal`] built from it write
    /// into.
    pub fn trace(&self) -> Trace {
        self.trace.clone()
    }

    /// An event source this machine offers, such as `app.focus`.
    pub fn with_source(mut self, id: &str) -> Self {
        self.sources.push(CapabilityId::new(id));
        self
    }

    pub fn with_actuator(mut self, id: &str, initial: Value, volatile: bool) -> Self {
        let id = CapabilityId::new(id);
        self.actuators.insert(
            id.clone(),
            MockActuator {
                id,
                state: RefCell::new(initial),
                volatile,
                trace: self.trace.clone(),
                grown: RefCell::new(None),
                opaque: None,
                clock: self.spent.clone(),
                capture_ms: Cell::new(0),
                apply_ms: Cell::new(0),
                outputs: None,
                captured: RefCell::new(None),
            },
        );
        self
    }

    /// An actuator whose baseline is no number: it captures `baseline` and
    /// rests at `rest`, which is also where it starts.
    pub fn with_opaque_actuator(mut self, id: &str, baseline: Value, rest: Value) -> Self {
        let id = CapabilityId::new(id);
        self.actuators.insert(
            id.clone(),
            MockActuator {
                id,
                state: RefCell::new(rest.clone()),
                volatile: false,
                trace: self.trace.clone(),
                grown: RefCell::new(None),
                opaque: Some((baseline, rest)),
                clock: self.spent.clone(),
                capture_ms: Cell::new(0),
                apply_ms: Cell::new(0),
                outputs: None,
                captured: RefCell::new(None),
            },
        );
        self
    }

    /// The hardware under this actuator moved: its next `recapture` hands
    /// back `baseline`, the way a monitor arriving mid-claim grows the one
    /// the journal holds.
    pub fn grow_baseline(&self, id: &str, baseline: Value) {
        if let Some(actuator) = self.actuators.get(&CapabilityId::new(id)) {
            *actuator.grown.borrow_mut() = Some(baseline);
        }
    }

    /// Capturing this actuator's baseline takes `ms` off the host's clock,
    /// as reading a panel over DDC/CI does.
    pub fn slow_capture(&self, id: &str, ms: u64) {
        if let Some(actuator) = self.actuators.get(&CapabilityId::new(id)) {
            actuator.capture_ms.set(ms);
        }
    }

    /// The next apply on this actuator takes `ms` off the host's clock, as a
    /// gamma write does while the display mode changes under it.
    pub fn slow_apply(&self, id: &str, ms: u64) {
        if let Some(actuator) = self.actuators.get(&CapabilityId::new(id)) {
            actuator.apply_ms.set(ms);
        }
    }

    /// This actuator holds outputs apart, as a colour temperature does
    /// monitor by monitor.
    pub fn with_outputs(mut self, id: &str) -> Self {
        if let Some(actuator) = self.actuators.get_mut(&CapabilityId::new(id)) {
            actuator.outputs = Some(RefCell::default());
        }
        self
    }

    /// What one output shows: its own value, else the whole actuator's.
    pub fn output_value(&self, id: &str, output: &str) -> Option<Value> {
        let actuator = self.actuators.get(&CapabilityId::new(id))?;
        let own = actuator
            .outputs
            .as_ref()
            .and_then(|o| o.borrow().get(output).cloned());
        Some(own.unwrap_or_else(|| actuator.state.borrow().clone()))
    }

    /// Queue an event for the next `poll`, as a platform would.
    pub fn queue(&mut self, event: Event) {
        self.queued.push(event);
    }

    /// What the actuator currently holds, for asserts.
    pub fn value(&self, id: &str) -> Option<Value> {
        self.actuators
            .get(&CapabilityId::new(id))
            .map(|a| a.state.borrow().clone())
    }
}

impl Host for MockHost {
    fn warnings(&self) -> Vec<String> {
        self.warnings.clone()
    }
    fn set_idle_thresholds(&mut self, seconds: &[u32]) {
        self.idle_thresholds = seconds.to_vec();
    }
    fn set_active(&mut self, active: bool) {
        self.active = active;
    }
    fn set_fading(&mut self, on: bool) {
        self.fading = on;
    }
    fn probe(&self) -> CapabilitySet {
        self.actuators
            .keys()
            .chain(&self.sources)
            .fold(CapabilitySet::new(), |set, id| {
                set.available(id.clone(), Some("mock"))
            })
    }

    fn install_sources(&mut self, wanted: &[CapabilityId]) -> Result<(), ActuatorError> {
        self.installed = wanted.to_vec();
        Ok(())
    }

    fn actuator(&self, cap: &CapabilityId) -> Option<&dyn Actuator> {
        self.actuators.get(cap).map(|a| a as &dyn Actuator)
    }

    fn set_timer(&mut self, id: TimerId, after: Duration, tolerance: Duration) {
        self.timers.push((id, after, tolerance));
    }

    fn cancel_timer(&mut self, id: TimerId) {
        self.cancelled.push(id);
    }

    fn time_of_day(&self) -> Option<Event> {
        self.clock.clone()
    }

    /// Events a test has queued, handed back on the next poll. A mock never
    /// blocks: the timeout is the platform's business.
    fn poll(&mut self, _timeout: Duration) -> Vec<Event> {
        std::mem::take(&mut self.queued)
    }

    fn monotonic_ms(&self) -> u64 {
        self.now_ms + self.spent.get()
    }

    fn set_hotkeys(&mut self, wanted: &[Hotkey]) -> Vec<(u32, String)> {
        self.hotkeys = wanted.to_vec();
        Vec::new()
    }

    fn send_keys(&mut self, chord: &Chord) -> Result<(), String> {
        self.desktop.push(format!("keys {chord}"));
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> Result<(), String> {
        self.desktop.push(format!("type {text}"));
        Ok(())
    }

    fn notify(&mut self, title: &str, body: &str) -> Result<(), String> {
        self.desktop.push(format!("notify {title}: {body}"));
        Ok(())
    }

    fn clipboard(&self) -> Result<String, String> {
        Ok(self.clipboard_text.clone())
    }

    fn set_clipboard(&mut self, text: &str) -> Result<(), String> {
        self.clipboard_text = text.to_owned();
        Ok(())
    }

    fn windows(&self) -> Vec<WindowInfo> {
        self.open_windows.clone()
    }

    fn window_action(&mut self, id: u64, action: WindowAction) -> Result<(), String> {
        self.desktop.push(format!("window {id} {action:?}"));
        Ok(())
    }
}
