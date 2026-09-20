//! Test doubles for everything an operating system normally provides.
//!
//! Compiled for this crate's own tests and for dependents that enable the
//! `test-util` feature. Never part of a shipping binary.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use crate::actuator::{Actuator, ActuatorError};
use crate::capability::{CapabilityId, CapabilitySet, Control};
use crate::event::Event;
use crate::event::TimerId;
use crate::journal::{JournalEntry, JournalStore};
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
}

impl Actuator for MockActuator {
    fn id(&self) -> CapabilityId {
        self.id.clone()
    }

    fn capture(&self) -> Result<Value, ActuatorError> {
        self.trace.push(format!("capture {}", self.id));
        Ok(self.state.borrow().clone())
    }

    fn apply(&self, value: &Value) -> Result<(), ActuatorError> {
        self.trace.push(format!("apply {} {:?}", self.id, value));
        *self.state.borrow_mut() = value.clone();
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
    pub installed: Vec<CapabilityId>,
    pub active: bool,
    pub idle_thresholds: Vec<u32>,
    pub timers: Vec<(TimerId, Duration, Duration)>,
    pub cancelled: Vec<TimerId>,
    /// What the next `poll` hands back, so a test can drive a loop the same
    /// way a real platform would.
    pub queued: Vec<Event>,
    trace: Trace,
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
    fn set_idle_thresholds(&mut self, seconds: &[u32]) {
        self.idle_thresholds = seconds.to_vec();
    }
    fn set_active(&mut self, active: bool) {
        self.active = active;
    }
    fn probe(&self) -> CapabilitySet {
        self.actuators.keys().fold(CapabilitySet::new(), |set, id| {
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

    /// Events a test has queued, handed back on the next poll. A mock never
    /// blocks: the timeout is the platform's business.
    fn poll(&mut self, _timeout: Duration) -> Vec<Event> {
        std::mem::take(&mut self.queued)
    }
}
