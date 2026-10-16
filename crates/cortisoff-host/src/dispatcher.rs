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

    /// What this run captured before its first apply on `cap`: the value the
    /// capability goes back to once nobody holds it, and so where a release
    /// fades to.
    pub fn baseline(&self, cap: &CapabilityId) -> Option<&Value> {
        self.captured.get(cap)
    }

    /// Put back what a previous run left applied. Runs once at startup, before
    /// any plugin claims.
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

    /// Grow the baselines of everything applied right now, after the platform
    /// reports that the hardware moved.
    ///
    /// The same ordering as the first apply: a baseline that changed is
    /// durable before anything is pushed at the monitor it describes. The
    /// caller runs this on a display-configuration event, ahead of the actions
    /// the engine emits for it.
    pub fn rebaseline(&mut self, host: &dyn Host) -> Vec<ActuatorError> {
        let mut errors = Vec::new();
        for (cap, baseline) in &mut self.captured {
            let Some(actuator) = host.actuator(cap) else {
                continue;
            };
            let fresh = match actuator.recapture() {
                None => continue,
                Some(Ok(fresh)) if fresh == *baseline => continue,
                Some(Ok(fresh)) => fresh,
                Some(Err(e)) => {
                    errors.push(e);
                    continue;
                }
            };
            // The journal first, and the in-memory copy only if it took: a
            // baseline this process believes in but cannot write down is the
            // one case the crash path cannot recover from.
            if !actuator.volatile() {
                if let Err(e) = self.journal.record(cap, &fresh) {
                    errors.push(e);
                    continue;
                }
            }
            *baseline = fresh;
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
        // One output of a capability shares the whole capability's actuator
        // and baseline: the journal entry covers every output at once.
        let output = cap.output();
        let whole = output
            .as_ref()
            .map_or_else(|| cap.clone(), |(p, _)| p.clone());
        let actuator = host
            .actuator(&whole)
            .ok_or_else(|| ActuatorError::on(cap.clone(), "no actuator for this capability"))?;
        if !self.captured.contains_key(&whole) {
            let baseline = actuator.capture()?;
            if !actuator.volatile() {
                // Durable before the apply, or a crash between the two leaves
                // no way back. A failing journal therefore aborts the apply:
                // a screen that keeps its colour is better than one that
                // cannot get it back.
                self.journal.record(&whole, &baseline)?;
            }
            self.captured.insert(whole, baseline);
        }
        match output {
            Some((_, output)) => actuator.apply_output(output, value),
            None => actuator.apply(value),
        }
    }

    fn restore(&mut self, host: &dyn Host, cap: &CapabilityId) -> Result<(), ActuatorError> {
        // Nobody holds one output any more: it shows the whole capability
        // again. The baseline stays captured, since it covers the others, and
        // goes only with the whole capability's own restore. When that came
        // first, everything is back already.
        if let Some((whole, output)) = cap.output() {
            if !self.captured.contains_key(&whole) {
                return Ok(());
            }
            let actuator = host
                .actuator(&whole)
                .ok_or_else(|| ActuatorError::on(cap.clone(), "no actuator for this capability"))?;
            return actuator.release_output(output);
        }
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

    const SCREEN: &str = r"\\.\DISPLAY3";

    /// One output held first, before anything holds the whole capability:
    /// the whole baseline is captured and journaled under the capability's
    /// own name, then only that output is touched.
    #[test]
    fn one_output_journals_the_whole_baseline_first() {
        let host = MockHost::new()
            .with_actuator(TEMP, Value::Int(6600), false)
            .with_outputs(TEMP);
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));
        let one = cap(TEMP).on_output(SCREEN);

        let out = d.execute(
            &host,
            vec![Action::Apply {
                cap: one.clone(),
                value: Value::Int(5000),
            }],
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            host.trace().events(),
            vec![
                format!("capture {TEMP}"),
                format!("journal {TEMP}"),
                format!("apply {TEMP} on {SCREEN} Int(5000)"),
            ]
        );
        assert_eq!(host.output_value(TEMP, SCREEN), Some(Value::Int(5000)));
        assert_eq!(host.output_value(TEMP, "other"), Some(Value::Int(6600)));

        // Letting the output go keeps the baseline: it covers the others.
        d.execute(&host, vec![Action::RestoreBaseline { cap: one }]);
        assert_eq!(host.output_value(TEMP, SCREEN), Some(Value::Int(6600)));
        assert!(d.baseline(&cap(TEMP)).is_some());
        assert_eq!(
            host.trace().events().last().unwrap(),
            &format!("release {TEMP} on {SCREEN}")
        );
    }

    /// The whole capability went back first, a shutdown releasing in table
    /// order: the output's own release has nothing left to do.
    #[test]
    fn an_output_released_after_the_whole_restore_is_a_no_op() {
        let host = MockHost::new()
            .with_actuator(TEMP, Value::Int(6600), false)
            .with_outputs(TEMP);
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));
        let one = cap(TEMP).on_output(SCREEN);
        d.execute(
            &host,
            vec![
                apply_action(TEMP, 3000),
                Action::Apply {
                    cap: one.clone(),
                    value: Value::Int(6600),
                },
            ],
        );
        let out = d.execute(
            &host,
            vec![restore_action(TEMP), Action::RestoreBaseline { cap: one }],
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(!host
            .trace()
            .events()
            .iter()
            .any(|e| e.starts_with("release")));
    }

    #[test]
    fn an_actuator_without_outputs_refuses_one() {
        let host = MockHost::new().with_actuator(TEMP, Value::Int(6600), false);
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));
        let out = d.execute(
            &host,
            vec![Action::Apply {
                cap: cap(TEMP).on_output(SCREEN),
                value: Value::Int(5000),
            }],
        );
        assert_eq!(out.errors.len(), 1);
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

    /// A monitor arrived while the tint was held. The baseline that gets
    /// restored, and the one on disk, both have to cover it.
    #[test]
    fn a_baseline_that_grew_is_journaled_and_restored() {
        let host = MockHost::new().with_actuator(TEMP, Value::Int(6500), false);
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));
        d.execute(&host, vec![apply_action(TEMP, 3400)]);

        host.grow_baseline(TEMP, Value::Int(6000));
        assert!(d.rebaseline(&host).is_empty());
        d.execute(&host, vec![restore_action(TEMP)]);

        assert_eq!(host.value(TEMP), Some(Value::Int(6000)));
        assert_eq!(
            host.trace().events(),
            vec![
                format!("capture {TEMP}"),
                format!("journal {TEMP}"),
                format!("apply {TEMP} Int(3400)"),
                format!("recapture {TEMP}"),
                // Durable before anything else touches the new monitor.
                format!("journal {TEMP}"),
                format!("apply {TEMP} Int(6000)"),
                format!("unjournal {TEMP}"),
            ]
        );
    }

    /// Nothing moved under the actuator, so nothing is written.
    #[test]
    fn rebaseline_without_a_change_touches_neither_journal_nor_hardware() {
        let host = MockHost::new().with_actuator(TEMP, Value::Int(6500), false);
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));
        d.execute(&host, vec![apply_action(TEMP, 3400)]);
        let before = host.trace().events().len();

        assert!(d.rebaseline(&host).is_empty());

        assert_eq!(host.trace().events().len(), before);
    }

    /// The journal is what makes the new monitor recoverable. If it refuses,
    /// the process must keep believing in the baseline it can still restore.
    #[test]
    fn a_refused_journal_write_leaves_the_old_baseline_in_place() {
        let host = MockHost::new().with_actuator(TEMP, Value::Int(6500), false);
        let mut d = Dispatcher::new(MemJournal::new(host.trace()));
        d.execute(&host, vec![apply_action(TEMP, 3400)]);
        // The disk goes away after the first baseline landed on it.
        d.journal.refuse_records();

        host.grow_baseline(TEMP, Value::Int(6000));
        assert_eq!(d.rebaseline(&host).len(), 1);
        d.execute(&host, vec![restore_action(TEMP)]);

        assert_eq!(host.value(TEMP), Some(Value::Int(6500)));
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
