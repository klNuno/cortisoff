//! Executes the actions the engine emits, with the crash-safety ordering the
//! project promises.
//!
//! A non-volatile actuator's baseline lands durably in the journal before the
//! first apply touches the hardware, so a crash at any instant leaves enough
//! on disk for the next run to put the screen back. This is generic over
//! [`Host`] and [`JournalStore`] on purpose: every platform reuses it, and it
//! is tested here without one.

use std::collections::BTreeMap;

use crate::actuator::ActuatorError;
use crate::capability::CapabilityId;
use crate::event::Action;
use crate::journal::JournalStore;
use crate::value::Value;
use crate::Host;

#[derive(Debug)]
pub struct Dispatcher<J> {
    journal: J,
    /// Baselines captured this run, volatile actuators included. The journal
    /// holds only the non-volatile subset, because the OS undoes the rest by
    /// itself when the process dies.
    captured: BTreeMap<CapabilityId, Value>,
}

/// What one batch of actions produced.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Actions the dispatcher does not own: timers, logs, program launches.
    /// The caller routes them, in order.
    pub deferred: Vec<Action>,
    /// Failures are collected rather than returned early: one broken actuator
    /// must not stop the rest of the batch from landing.
    pub errors: Vec<ActuatorError>,
}

impl<J: JournalStore> Dispatcher<J> {
    pub fn new(journal: J) -> Self {
        Self {
            journal,
            captured: BTreeMap::new(),
        }
    }

    /// The journal underneath, so the doctor can report what is still dirty
    /// without a second handle on the file.
    pub fn journal(&self) -> &J {
        &self.journal
    }

    /// Put back what a previous run left applied. Runs once at startup, before
    /// any rule fires.
    ///
    /// An entry is cleared only after its restore succeeded, so a monitor that
    /// is unplugged today keeps its entry for the run where it comes back.
    /// Restoring a baseline twice is harmless, which is what makes that safe.
    pub fn recover(&mut self, host: &dyn Host) -> Vec<ActuatorError> {
        let entries = match self.journal.dirty() {
            Ok(entries) => entries,
            Err(e) => return vec![e],
        };
        let mut errors = Vec::new();
        for entry in entries {
            let restored = match host.actuator(&entry.cap) {
                None => Err(ActuatorError::on(
                    entry.cap.clone(),
                    "journaled baseline has no actuator on this run",
                )),
                Some(actuator) => actuator.apply(&entry.baseline),
            };
            match restored.and_then(|()| self.journal.clear(&entry.cap)) {
                Ok(()) => {}
                Err(e) => errors.push(e),
            }
        }
        errors
    }

    pub fn execute(&mut self, host: &dyn Host, actions: Vec<Action>) -> Outcome {
        let mut out = Outcome::default();
        for action in actions {
            match action {
                Action::Apply { cap, value } => {
                    if let Err(e) = self.apply(host, &cap, &value) {
                        out.errors.push(e);
                    }
                }
                Action::RestoreBaseline { cap } => {
                    if let Err(e) = self.restore(host, &cap) {
                        out.errors.push(e);
                    }
                }
                other => out.deferred.push(other),
            }
        }
        out
    }

    fn apply(
        &mut self,
        host: &dyn Host,
        cap: &CapabilityId,
        value: &Value,
    ) -> Result<(), ActuatorError> {
        let actuator = host
            .actuator(cap)
            .ok_or_else(|| ActuatorError::on(cap.clone(), "no actuator for this capability"))?;
        if !self.captured.contains_key(cap) {
            let baseline = actuator.capture()?;
            if !actuator.volatile() {
                // Durable before the apply, or a crash between the two leaves
                // no way back. A failing journal therefore aborts the apply:
                // a screen that keeps its colour is better than one that
                // cannot get it back.
                self.journal.record(cap, &baseline)?;
            }
            self.captured.insert(cap.clone(), baseline);
        }
        actuator.apply(value)
    }

    fn restore(&mut self, host: &dyn Host, cap: &CapabilityId) -> Result<(), ActuatorError> {
        // Nothing captured means nothing was ever applied. The arbiter does
        // not emit that, but tolerating it costs nothing.
        let Some(baseline) = self.captured.get(cap).cloned() else {
            return Ok(());
        };
        let actuator = host
            .actuator(cap)
            .ok_or_else(|| ActuatorError::on(cap.clone(), "no actuator for this capability"))?;
        // The capture is dropped only once the hardware is actually back, so a
        // failed restore stays retryable.
        actuator.apply(&baseline)?;
        self.captured.remove(cap);
        if !actuator.volatile() {
            self.journal.clear(cap)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MemJournal, MockHost};
    use crate::{ids, Event, LogLevel};

    const TEMP: &str = ids::DISPLAY_COLOR_TEMP;
    const MODE: &str = ids::DISPLAY_MODE;

    fn cap(s: &str) -> CapabilityId {
        CapabilityId::new(s)
    }

    fn apply_action(cap_id: &str, v: i64) -> Action {
        Action::Apply {
            cap: cap(cap_id),
            value: Value::Int(v),
        }
    }

    fn restore_action(cap_id: &str) -> Action {
        Action::RestoreBaseline { cap: cap(cap_id) }
    }

    #[test]
    fn the_first_apply_journals_the_baseline_before_touching_the_actuator() {
        let host = MockHost::new().with_actuator(TEMP, Value::Int(6500), false);
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));

        let out = d.execute(&host, vec![apply_action(TEMP, 3400)]);

        assert!(out.errors.is_empty());
        assert_eq!(
            host.trace().events(),
            vec![
                format!("capture {TEMP}"),
                format!("journal {TEMP}"),
                format!("apply {TEMP} Int(3400)"),
            ]
        );
    }

    /// The second value arrives while the first is still applied. Capturing
    /// again would record the tinted screen as the user's neutral.
    #[test]
    fn a_second_apply_does_not_capture_again() {
        let host = MockHost::new().with_actuator(TEMP, Value::Int(6500), false);
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));

        d.execute(&host, vec![apply_action(TEMP, 3400)]);
        d.execute(&host, vec![apply_action(TEMP, 2700)]);

        let captures = host
            .trace()
            .events()
            .iter()
            .filter(|e| e.starts_with("capture"))
            .count();
        assert_eq!(captures, 1);
    }

    #[test]
    fn restore_puts_back_the_baseline_and_clears_the_journal() {
        let host = MockHost::new().with_actuator(TEMP, Value::Int(6500), false);
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));

        d.execute(&host, vec![apply_action(TEMP, 3400)]);
        let out = d.execute(&host, vec![restore_action(TEMP)]);

        assert!(out.errors.is_empty());
        assert_eq!(host.value(TEMP), Some(Value::Int(6500)));
        // Cleared after the hardware is back, never before: a crash between
        // the two re-restores on the next run, which is harmless.
        assert_eq!(
            host.trace().events().last().unwrap(),
            &format!("unjournal {TEMP}")
        );
    }

    #[test]
    fn a_volatile_actuator_is_never_journaled_but_still_restored_in_run() {
        let host = MockHost::new().with_actuator(TEMP, Value::Int(6500), true);
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));

        d.execute(&host, vec![apply_action(TEMP, 3400)]);
        d.execute(&host, vec![restore_action(TEMP)]);

        let events = host.trace().events();
        assert!(events.iter().all(|e| !e.starts_with("journal")));
        assert_eq!(host.value(TEMP), Some(Value::Int(6500)));
    }

    /// The previous run died with a tint applied. The next run's first move.
    #[test]
    fn recover_restores_a_dirty_journal_and_clears_it() {
        let host = MockHost::new().with_actuator(TEMP, Value::Int(3400), false);
        let journal = MemJournal::seeded(host.trace(), &[(cap(TEMP), Value::Int(6500))]);
        let mut d = Dispatcher::new(journal);

        let errors = d.recover(&host);

        assert!(errors.is_empty());
        assert_eq!(host.value(TEMP), Some(Value::Int(6500)));
    }

    /// The journaled monitor is unplugged today. The entry must survive for
    /// the run where it comes back.
    #[test]
    fn recover_keeps_the_entry_when_the_actuator_is_missing() {
        let host = MockHost::new();
        let journal = MemJournal::seeded(host.trace(), &[(cap(MODE), Value::Int(60))]);
        let mut d = Dispatcher::new(journal);

        let errors = d.recover(&host);

        assert_eq!(errors.len(), 1);
        // Recover again with the actuator present: the entry was kept.
        let host = MockHost::new().with_actuator(MODE, Value::Int(144), false);
        assert!(d.recover(&host).is_empty());
        assert_eq!(host.value(MODE), Some(Value::Int(60)));
    }

    /// The invariant itself: no durable baseline, no apply.
    #[test]
    fn a_failing_journal_write_aborts_the_apply() {
        let host = MockHost::new().with_actuator(TEMP, Value::Int(6500), false);
        let mut journal = MemJournal::new(host.trace());
        journal.refuse_records();
        let mut d = Dispatcher::new(journal);

        let out = d.execute(&host, vec![apply_action(TEMP, 3400)]);

        assert_eq!(out.errors.len(), 1);
        assert_eq!(host.value(TEMP), Some(Value::Int(6500)));
    }

    #[test]
    fn an_apply_on_a_missing_actuator_is_an_error_not_a_panic() {
        let host = MockHost::new();
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));
        let out = d.execute(&host, vec![apply_action(TEMP, 3400)]);
        assert_eq!(out.errors.len(), 1);
    }

    #[test]
    fn restore_with_nothing_captured_is_a_no_op() {
        let host = MockHost::new().with_actuator(TEMP, Value::Int(6500), false);
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));
        let out = d.execute(&host, vec![restore_action(TEMP)]);
        assert!(out.errors.is_empty());
        assert!(host.trace().events().is_empty());
    }

    #[test]
    fn actions_the_dispatcher_does_not_own_are_deferred_in_order() {
        let host = MockHost::new().with_actuator(TEMP, Value::Int(6500), false);
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));

        let log = Action::Log {
            level: LogLevel::Info,
            message: "hello".into(),
        };
        let run = Action::Run {
            program: "x".into(),
            args: vec![],
        };
        let out = d.execute(
            &host,
            vec![log.clone(), apply_action(TEMP, 3400), run.clone()],
        );

        assert_eq!(out.deferred, vec![log, run]);
        assert_eq!(host.value(TEMP), Some(Value::Int(3400)));
    }

    /// End to end through real engine actions, no OS anywhere.
    #[test]
    fn engine_actions_flow_through_unchanged() {
        let host = MockHost::new().with_actuator(TEMP, Value::Int(6500), false);
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));

        // What the engine emits on a rising then falling edge.
        d.execute(
            &host,
            vec![Action::Apply {
                cap: cap(TEMP),
                value: Value::Int(3400),
            }],
        );
        assert_eq!(host.value(TEMP), Some(Value::Int(3400)));

        d.execute(&host, vec![Action::RestoreBaseline { cap: cap(TEMP) }]);
        assert_eq!(host.value(TEMP), Some(Value::Int(6500)));

        // Shutdown path reuses RestoreBaseline, so there is nothing new to
        // execute: the event exists at the engine layer only.
        let _ = Event::Shutdown;
    }
}
