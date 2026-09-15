//! The loop's body: everything that happens between an event arriving from
//! the platform and the hardware being touched.
//!
//! The platform owns the loop itself, so this is not a thread and not a
//! runtime. It is a function of an event, and the only reason it lives in the
//! daemon rather than in the core is that it routes side effects: timers,
//! child processes, the journal, and the notification stream clients read.

use cortisoff_core::{Condition, Engine, OwnerId, Rule, RuleState, Signals};
use std::collections::BTreeMap;

use cortisoff_host::{
    ids, Action, ActuatorError, CapIndex, CapabilityId, CapabilitySet, Event, Host, JournalStore,
    LogLevel, Value,
};
use cortisoff_ipc::{
    CapabilityReport, ClaimReport, DoctorReport, Notification, RuleReport, RuleStateReport,
    StateReport, WorldReport,
};

use crate::startup::Interference;

/// Every source a rule can read, paired with the signal that says a rule reads
/// it. Installing a hook nobody reads costs on every system-wide event, so the
/// host is asked for exactly this subset and nothing more.
const SOURCES: &[(Signals, &str)] = &[
    (Signals::APP, ids::APP_FOCUS),
    (Signals::FULLSCREEN, ids::APP_FULLSCREEN),
    (Signals::MAXIMIZED, ids::APP_MAXIMIZED),
    (Signals::IDLE, ids::USER_IDLE),
    (Signals::LOCKED, ids::SESSION_LOCK),
    (Signals::POWER, ids::POWER_SOURCE),
    (Signals::AUDIO, ids::AUDIO_ACTIVE),
    // Signals::TIME has no source. The engine asks for a timer at its own
    // schedule edges instead, which is why a blue light rule costs nothing
    // between them.
];

pub struct Supervisor<J> {
    engine: Engine,
    dispatcher: cortisoff_host::Dispatcher<J>,
    gate: Interference,
    /// Everything a subscriber has not read yet. Bounded, because a client
    /// that stops reading must not grow the daemon without limit.
    pending: Vec<Notification>,
    warnings: Vec<String>,
    errors: Vec<ActuatorError>,
    rules_path: Option<std::path::PathBuf>,
    /// The rules as authored, kept so an editing client gets what is running
    /// rather than what the disk happens to hold, and so it never has to parse
    /// TOML to show a switch.
    authored: Vec<Rule>,
    /// Where each previewed capability rested before its drag began, read
    /// once at the first preview: an actuator whose rest is "what the driver
    /// shows now" would otherwise report the preview itself.
    previewing: BTreeMap<CapabilityId, Option<Value>>,
}

/// Past this many unread notifications the oldest are dropped and the drop is
/// itself reported, so a stalled `cortisoff trace` costs a bounded amount of
/// memory instead of the machine.
const MAX_PENDING: usize = 4096;

impl<J: JournalStore> Supervisor<J> {
    pub fn new(engine: Engine, journal: J, gate: Interference) -> Self {
        let mut warnings = gate.warnings.clone();
        warnings.shrink_to_fit();
        Self {
            engine,
            dispatcher: cortisoff_host::Dispatcher::new(journal),
            gate,
            pending: Vec::new(),
            warnings,
            errors: Vec::new(),
            rules_path: None,
            authored: Vec::new(),
            previewing: BTreeMap::new(),
        }
    }

    /// Extra sentences for the doctor that came from outside the engine, such
    /// as a journal file that could not be read.
    pub fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Put back whatever a previous run left applied, install only the sources
    /// some rule actually reads, then evaluate once against the world the host
    /// has already pushed.
    ///
    /// Recovery comes first on purpose: a rule that fires immediately would
    /// otherwise capture a poisoned baseline as this run's neutral.
    pub fn start(&mut self, host: &mut dyn Host) {
        for e in self.dispatcher.recover(&*host) {
            self.fail(e);
        }

        let wanted = self.wanted_sources();
        if let Err(e) = host.install_sources(&wanted) {
            self.fail(e);
        }

        let actions = self.engine.sync();
        self.run(host, actions);
    }

    /// One event, all the way to the hardware.
    pub fn handle(&mut self, host: &mut dyn Host, event: Event) {
        // Timer identities are internal. Keep existing trace clients compatible
        // and publish the clock and resulting actions as before.
        if !matches!(event, Event::TimerFired { .. }) {
            self.note(Notification::Event(event.clone()));
        }
        let before = self.holding_names();
        let actions = self.engine.on_event(&event);
        self.report_edges(before);
        self.run(host, actions);
    }

    /// A timer the engine asked for has fired. The host converts it back into
    /// whatever event it stood for, except the schedule timer, which the host
    /// answers with the current [`Event::TimeOfDay`].
    pub fn timer_fired(&mut self, host: &mut dyn Host, minute: u16, second: u8) {
        self.handle(
            host,
            Event::TimeOfDay {
                minute,
                second,
                millisecond: 0,
            },
        );
    }

    /// Where `Request::Reload` reads from. Absent when the daemon was handed
    /// its rules directly, which is what the tests do.
    pub fn with_rules_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.rules_path = Some(path.into());
        self
    }

    /// The same rules the engine was built from. The engine keeps them
    /// compiled, which loses everything a client would need to show them back.
    pub fn with_rules(mut self, rules: Vec<Rule>) -> Self {
        self.authored = rules;
        self
    }

    pub fn authored(&self) -> &[Rule] {
        &self.authored
    }

    /// Write these rules to the rule file, then load them.
    ///
    /// The whole file, because a set is the only thing a client can hand over
    /// without an identity to keep in sync. A set that does not survive the
    /// round trip through TOML never reaches the disk, and a set that reaches
    /// the disk but fails to load puts the previous file back: the way to lose
    /// a working configuration is to half-apply a bad one.
    pub fn save_rules(&mut self, host: &mut dyn Host, rules: Vec<Rule>) -> Result<(), String> {
        let path = self
            .rules_path
            .clone()
            .ok_or_else(|| "this daemon was not started from a rule file".to_owned())?;

        let mut seen = std::collections::BTreeSet::new();
        for rule in &rules {
            if rule.name.trim().is_empty() {
                return Err("a rule needs a name. It is what the trace prints".to_owned());
            }
            if !seen.insert(rule.name.clone()) {
                return Err(format!(
                    "two rules are called {:?}. Names are how a rule is pointed at",
                    rule.name
                ));
            }
        }

        let text = cortisoff_core::write_rules(&rules);
        // Reading back what we are about to write catches the one bug this
        // whole path can have: a rule the writer cannot express.
        // Compared without order: the file sorts keys, so a set listed as
        // vibrance then colour comes back as colour then vibrance and means
        // exactly the same thing.
        match cortisoff_core::parse_rules(&text) {
            Ok(back) if canonical(&back) == canonical(&rules) => {}
            Ok(_) => {
                return Err(
                    "these rules do not survive being written out. Nothing was saved".to_owned(),
                )
            }
            Err(e) => return Err(format!("writing these rules produced a bad file: {e}")),
        }

        let previous = std::fs::read_to_string(&path).ok();
        write_atomically(&path, &text)?;

        match self.reload(host) {
            Ok(()) => Ok(()),
            Err(e) => {
                // The daemon kept running on the old rules, so the file has to
                // go back to matching them.
                if let Some(previous) = previous {
                    let _ = write_atomically(&path, &previous);
                }
                Err(e)
            }
        }
    }

    /// A value the user set by hand. It outranks every rule until it is
    /// cleared, and the rule underneath keeps holding meanwhile.
    pub fn manual_set(
        &mut self,
        host: &mut dyn Host,
        cap: &CapabilityId,
        value: cortisoff_host::Value,
    ) -> Result<(), String> {
        let actions = self.engine.manual_set(cap, value)?;
        self.run(host, actions);
        Ok(())
    }

    pub fn manual_clear(&mut self, host: &mut dyn Host, cap: &CapabilityId) -> Result<(), String> {
        let actions = self.engine.manual_clear(cap)?;
        self.run(host, actions);
        Ok(())
    }

    /// A slider mid-drag. The first call remembers where the screen rested,
    /// every call pushes the value.
    pub fn preview(
        &mut self,
        host: &mut dyn Host,
        cap: &CapabilityId,
        value: Value,
    ) -> Result<(), String> {
        if !self.previewing.contains_key(cap) {
            let rest = host.actuator(cap).and_then(|a| a.rest());
            self.previewing.insert(cap.clone(), rest);
        }
        let actions = self.engine.preview(cap, value)?;
        self.run(host, actions);
        Ok(())
    }

    /// The drag ended, or the client that was dragging went away.
    pub fn preview_end(&mut self, host: &mut dyn Host, cap: &CapabilityId) -> Result<(), String> {
        let rest = self.previewing.remove(cap).flatten();
        let actions = self.engine.preview_end(cap, rest)?;
        self.run(host, actions);
        Ok(())
    }

    /// Re-read the rule file. A file that does not parse leaves the running
    /// rules untouched, because losing a working configuration to a missing
    /// bracket is not acceptable.
    pub fn reload(&mut self, host: &mut dyn Host) -> Result<(), String> {
        let path = self
            .rules_path
            .clone()
            .ok_or_else(|| "this daemon was not started from a rule file".to_owned())?;
        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let rules = cortisoff_core::parse_rules(&source)
            .map_err(|e| format!("{}: {}: {}", path.display(), e.at, e.message))?;

        self.authored = rules.clone();
        let actions = self.engine.reload(resting(host, rules));
        // A rule added from the dashboard may read something no rule read at
        // startup, and one switched off may leave a hook nobody needs.
        let wanted = self.wanted_sources();
        if let Err(e) = host.install_sources(&wanted) {
            self.fail(e);
        }
        self.run(host, actions);
        Ok(())
    }

    /// Hand every screen back the way it was found, then say so.
    ///
    /// Called on the way out, and it is the reason a user can quit cortisoff
    /// without rebooting. The restores go through the dispatcher like any
    /// other action, so each journal entry is cleared only once its restore
    /// actually succeeded and a monitor that refuses keeps its entry for the
    /// next run.
    pub fn shutdown(&mut self, host: &mut dyn Host) {
        let actions = self.engine.shutdown();
        self.run(host, actions);
    }

    /// The last thing a subscriber sees. Sent before the transport closes, so
    /// a client can tell a clean exit from a crash.
    pub fn note_closing(&mut self) {
        self.note(Notification::Closing);
    }

    /// Notifications nobody has read yet. Draining is the subscriber's job, so
    /// a daemon with no client attached keeps only what fits in the bound.
    pub fn drain(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.pending)
    }

    pub fn errors(&self) -> &[ActuatorError] {
        &self.errors
    }

    fn wanted_sources(&self) -> Vec<CapabilityId> {
        let read: Signals = self
            .engine
            .rules()
            .iter()
            .filter(|r| r.is_active())
            .fold(Signals::NONE, |acc, r| acc | r.reads);
        SOURCES
            .iter()
            .filter(|(signal, _)| read.intersects(*signal))
            .map(|(_, id)| CapabilityId::new(*id))
            .collect()
    }

    /// Execute a batch, in order. Apply and restore go to the dispatcher, and
    /// everything it hands back is routed here.
    fn run(&mut self, host: &mut dyn Host, actions: Vec<Action>) {
        let (allowed, refused) = self.split_blocked(actions);
        for cap in refused {
            self.note(Notification::Log {
                level: LogLevel::Warn,
                message: format!("{cap} is left alone this run, another application drives it"),
            });
        }

        for action in &allowed {
            self.note(Notification::Action(action.clone()));
        }

        let outcome = self.dispatcher.execute(&*host, allowed);
        for e in outcome.errors {
            self.fail(e);
        }

        for action in outcome.deferred {
            match action {
                Action::SetTimer {
                    id,
                    after,
                    tolerance,
                } => host.set_timer(id, after, tolerance),
                Action::CancelTimer { id } => host.cancel_timer(id),
                Action::Log { level, message } => self.note(Notification::Log { level, message }),
                Action::Run { program, args } => self.spawn(&program, &args),
                // The dispatcher owns these two and never defers them.
                Action::Apply { .. } | Action::RestoreBaseline { .. } => {}
            }
        }
        host.set_active(self.active());
    }

    /// Whether at least one capability has an unblocked claim.
    fn active(&self) -> bool {
        let arbiter = self.engine.arbiter();
        let table = arbiter.table();
        (0..table.len()).any(|i| {
            let cap = CapIndex(i as u16);
            arbiter.holder(cap).is_some() && !self.gate.blocks(table.id(cap))
        })
    }

    /// Drop what the startup gate refuses, keeping the rest of the batch. A
    /// blocked colour temperature must not stop a refresh rate change in the
    /// same batch from landing.
    fn split_blocked(&self, actions: Vec<Action>) -> (Vec<Action>, Vec<CapabilityId>) {
        let mut allowed = Vec::with_capacity(actions.len());
        let mut refused = Vec::new();
        for action in actions {
            let cap = match &action {
                Action::Apply { cap, .. } | Action::RestoreBaseline { cap } => Some(cap),
                _ => None,
            };
            match cap {
                Some(cap) if self.gate.blocks(cap) => refused.push(cap.clone()),
                _ => allowed.push(action),
            }
        }
        (allowed, refused)
    }

    fn spawn(&mut self, program: &str, args: &[String]) {
        // Detached on purpose: the wallpaper adapters are one-shot commands
        // and the daemon must never sit waiting on one.
        match std::process::Command::new(program).args(args).spawn() {
            Ok(_) => self.note(Notification::Log {
                level: LogLevel::Trace,
                message: format!("ran {program}"),
            }),
            Err(e) => self.note(Notification::Log {
                level: LogLevel::Error,
                message: format!("could not run {program}: {e}"),
            }),
        }
    }

    fn fail(&mut self, e: ActuatorError) {
        self.note(Notification::Log {
            level: LogLevel::Error,
            message: e.to_string(),
        });
        self.errors.push(e);
    }

    fn note(&mut self, notification: Notification) {
        if self.pending.len() >= MAX_PENDING {
            self.pending.remove(0);
        }
        self.pending.push(notification);
    }

    fn holding_names(&self) -> Vec<String> {
        self.engine.firing().map(|r| r.name.clone()).collect()
    }

    /// The `cortisoff trace` line. Computed by comparing the firing set before
    /// and after, because the engine reports actions and not edges, and an
    /// edge that claims nothing is still worth seeing when debugging a rule.
    fn report_edges(&mut self, before: Vec<String>) {
        let after = self.holding_names();
        for name in after.iter().filter(|n| !before.contains(n)) {
            self.note(Notification::RuleEdge {
                rule: name.clone(),
                holding: true,
            });
        }
        for name in before.iter().filter(|n| !after.contains(n)) {
            self.note(Notification::RuleEdge {
                rule: name.clone(),
                holding: false,
            });
        }
    }

    pub fn state(&self) -> StateReport {
        let w = self.engine.world();
        StateReport {
            active: self.active(),
            world: WorldReport {
                app: w.app.clone(),
                fullscreen: w.fullscreen,
                maximized: w.maximized,
                idle: w.idle,
                locked: w.locked,
                on_battery: w.on_battery,
                audio_active: w.audio_active,
                minute_of_day: w.minute_of_day(),
            },
            rules: self.rule_reports(),
            claims: self.claim_reports(),
        }
    }

    /// `host` is here only to answer one question per capability: is there an
    /// actuator behind it. A sensor and an actuator are both capabilities and
    /// the probe does not separate them, but a client showing an override box
    /// on `app.focus` is offering something that can only be refused.
    pub fn doctor(&self, host: &dyn Host, probed: &CapabilitySet) -> DoctorReport {
        let mut capabilities: Vec<CapabilityReport> = probed
            .ids()
            .map(|id| {
                let actuator = host.actuator(id);
                CapabilityReport {
                    id: id.clone(),
                    backend: probed.backend(id).map(str::to_owned),
                    missing_because: None,
                    settable: actuator.is_some(),
                    label: cortisoff_host::label(id.as_str()),
                    control: actuator.map(|a| a.control()),
                }
            })
            .collect();
        capabilities.extend(probed.gaps().iter().map(|gap| CapabilityReport {
            id: gap.id.clone(),
            backend: None,
            missing_because: Some(gap.reason.clone()),
            settable: false,
            label: cortisoff_host::label(gap.id.as_str()),
            control: None,
        }));

        let journal_dirty = self
            .dispatcher
            .journal()
            .dirty()
            .map(|entries| entries.into_iter().map(|e| e.cap).collect())
            .unwrap_or_default();

        DoctorReport {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            pid: std::process::id(),
            capabilities,
            rules: self.rule_reports(),
            journal_dirty,
            warnings: self.warnings.clone(),
        }
    }

    fn rule_reports(&self) -> Vec<RuleReport> {
        let table = self.engine.arbiter().table();
        let firing: Vec<u32> = self.engine.firing().map(|r| r.id).collect();
        self.engine
            .rules()
            .iter()
            .map(|rule| RuleReport {
                name: rule.name.clone(),
                priority: rule.priority,
                state: match &rule.state {
                    RuleState::Active => RuleStateReport::Active,
                    RuleState::Disabled => RuleStateReport::Disabled,
                    RuleState::Empty => RuleStateReport::Empty,
                    RuleState::Unsupported { missing } => RuleStateReport::Unsupported {
                        missing: missing.clone(),
                    },
                },
                holding: firing.contains(&rule.id),
                sets: rule.set.iter().map(|(i, _)| table.id(*i).clone()).collect(),
            })
            .collect()
    }

    fn claim_reports(&self) -> Vec<ClaimReport> {
        let arbiter = self.engine.arbiter();
        let table = arbiter.table();
        let mut out = Vec::new();
        for i in 0..table.len() {
            let cap = CapIndex(i as u16);
            let Some(owner) = arbiter.holder(cap) else {
                continue;
            };
            let stack = arbiter.stack(cap);
            let Some(top) = stack.iter().find(|c| c.owner == owner) else {
                continue;
            };
            let mut shadowed: Vec<&cortisoff_core::Claim> =
                stack.iter().filter(|c| c.owner != owner).collect();
            shadowed.sort_by_key(|c| std::cmp::Reverse(c.priority));
            out.push(ClaimReport {
                cap: table.id(cap).clone(),
                owner: self.owner_name(owner),
                priority: top.priority,
                value: top.value.clone(),
                shadowed: shadowed
                    .into_iter()
                    .map(|c| self.owner_name(c.owner))
                    .collect(),
            });
        }
        out
    }

    fn owner_name(&self, owner: OwnerId) -> String {
        match owner {
            OwnerId::Manual => "manual".to_owned(),
            OwnerId::Preview => "preview".to_owned(),
            OwnerId::Rule(id) => self
                .engine
                .rules()
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.name.clone())
                .unwrap_or_else(|| format!("rule {id}")),
        }
    }
}

/// The rules as the engine should run them: every fade that names no start
/// begins from what its actuator says the screen shows when nothing holds it.
///
/// Only the engine's copy. What the user wrote, and what the dashboard is
/// shown, keeps saying "from the current value" by leaving `from` out, so a
/// monitor that changes its resting point changes the fade with it.
pub fn resting(host: &dyn Host, mut rules: Vec<Rule>) -> Vec<Rule> {
    cortisoff_core::start_from_rest(&mut rules, |cap| host.actuator(cap).and_then(|a| a.rest()));
    rules
}

/// Rules with every order that carries no meaning taken out: what a rule sets,
/// where its fades start, and the branches of `all` and `any`.
fn canonical(rules: &[Rule]) -> Vec<Rule> {
    rules
        .iter()
        .cloned()
        .map(|mut rule| {
            rule.set.sort_by(|a, b| a.cap.cmp(&b.cap));
            if let Some(t) = &mut rule.transition {
                t.from.sort_by(|a, b| a.cap.cmp(&b.cap));
            }
            rule.when = canonical_condition(rule.when);
            rule
        })
        .collect()
}

fn canonical_condition(condition: Condition) -> Condition {
    let sorted = |branches: Vec<Condition>| {
        let mut branches: Vec<Condition> = branches.into_iter().map(canonical_condition).collect();
        branches.sort_by_cached_key(|c| format!("{c:?}"));
        branches
    };
    match condition {
        Condition::All(branches) => Condition::All(sorted(branches)),
        Condition::Any(branches) => Condition::Any(sorted(branches)),
        Condition::Not(inner) => Condition::Not(Box::new(canonical_condition(*inner))),
        other => other,
    }
}

/// Temporary file, flush, rename, the same shape the journal uses. A rule file
/// truncated by a crash mid-write is a user losing every rule they wrote.
fn write_atomically(path: &std::path::Path, contents: &str) -> Result<(), String> {
    use std::io::Write;

    let fail = |what: &str, e: std::io::Error| format!("could not {what} {}: {e}", path.display());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| fail("create the directory for", e))?;
    }
    let temp = path.with_extension("toml.tmp");
    let mut file = std::fs::File::create(&temp).map_err(|e| fail("write", e))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| fail("write", e))?;
    file.sync_all().map_err(|e| fail("flush", e))?;
    drop(file);
    std::fs::rename(&temp, path).map_err(|e| fail("replace", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cortisoff_core::{parse_rules, Assign, CapTable};
    use cortisoff_host::mock::{MemJournal, MockHost, Trace};
    use cortisoff_host::{AppRef, Value};

    const VIBRANCE: &str = ids::DISPLAY_VIBRANCE;
    const TEMP: &str = ids::DISPLAY_COLOR_TEMP;

    fn host() -> MockHost {
        MockHost::new()
            .with_actuator(VIBRANCE, Value::Int(50), false)
            .with_actuator(TEMP, Value::Int(6500), false)
    }

    fn supervisor(host: &MockHost, source: &str, gate: Interference) -> Supervisor<MemJournal> {
        let rules = parse_rules(source).expect("test rules parse");
        let engine = Engine::new(CapTable::from_set(&host.probe()), rules);
        Supervisor::new(engine, MemJournal::new(Trace::default()), gate)
    }

    fn focus(exe: &str) -> Event {
        Event::AppFocus(AppRef {
            exe: exe.to_owned(),
            title: None,
            pid: 1,
            started_at_ms: 0,
        })
    }

    const FOCUS_RULE: &str = r#"
[[rule]]
name = "vibrance in game"
when.app = "game.exe"
set.display.vibrance = 100
"#;

    /// An installed hook costs on every system-wide event, so a rule set that
    /// reads nothing but the focused application must not get an idle timer.
    #[test]
    fn only_the_sources_some_rule_reads_are_installed() {
        let mut h = host();
        let mut s = supervisor(&h, FOCUS_RULE, Interference::clear());
        s.start(&mut h);
        assert_eq!(h.installed, vec![CapabilityId::new(ids::APP_FOCUS)]);
    }

    #[test]
    fn a_rule_firing_reaches_the_actuator_and_going_back_restores_it() {
        let mut h = host();
        let mut s = supervisor(&h, FOCUS_RULE, Interference::clear());
        s.start(&mut h);
        assert!(!h.active && !s.state().active);

        s.handle(&mut h, focus("game.exe"));
        assert_eq!(h.value(VIBRANCE), Some(Value::Int(100)));
        assert!(h.active && s.state().active);

        s.handle(&mut h, focus("notepad.exe"));
        assert_eq!(h.value(VIBRANCE), Some(Value::Int(50)));
        assert!(!h.active && !s.state().active);
    }

    /// Ctrl-C is not a crash. Quitting has to hand every screen back, or a
    /// user who tries cortisoff once is left with an orange screen and no
    /// process left to fix it.
    #[test]
    fn shutting_down_puts_every_baseline_back_and_empties_the_journal() {
        let mut h = host();
        let mut s = supervisor(&h, FOCUS_RULE, Interference::clear());
        s.start(&mut h);
        s.handle(&mut h, focus("game.exe"));
        assert_eq!(h.value(VIBRANCE), Some(Value::Int(100)));

        s.shutdown(&mut h);
        assert_eq!(h.value(VIBRANCE), Some(Value::Int(50)));
        assert!(
            s.dispatcher
                .journal()
                .dirty()
                .expect("the mock journal always reads")
                .is_empty(),
            "a cleanly restored baseline has nothing left to recover"
        );
    }

    /// A manual override is the user's, not a rule's, and nothing survives the
    /// daemon to undo it either.
    #[test]
    fn shutting_down_also_drops_a_manual_override() {
        let mut h = host();
        let mut s = supervisor(&h, FOCUS_RULE, Interference::clear());
        s.start(&mut h);
        s.manual_set(&mut h, &CapabilityId::new(VIBRANCE), Value::Int(70))
            .expect("the capability exists");
        assert_eq!(h.value(VIBRANCE), Some(Value::Int(70)));
        assert!(h.active && s.state().active);

        s.shutdown(&mut h);
        assert_eq!(h.value(VIBRANCE), Some(Value::Int(50)));
        assert!(!h.active && !s.state().active);
    }

    /// What clicking the switch on a card does, end to end: the file changes,
    /// the rule stops holding, and the screen goes back on its own.
    #[test]
    fn switching_a_rule_off_rewrites_the_file_and_releases_what_it_held() {
        let dir = std::env::temp_dir().join("cortisoff-save-rules");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rules.toml");
        std::fs::write(&file, FOCUS_RULE).unwrap();

        let mut h = host();
        let mut s = supervisor(&h, FOCUS_RULE, Interference::clear())
            .with_rules_path(&file)
            .with_rules(parse_rules(FOCUS_RULE).unwrap());
        s.start(&mut h);
        s.handle(&mut h, focus("game.exe"));
        assert_eq!(h.value(VIBRANCE), Some(Value::Int(100)));

        let mut rules = s.authored().to_vec();
        rules[0].enabled = false;
        s.save_rules(&mut h, rules).expect("the set is saveable");

        assert_eq!(h.value(VIBRANCE), Some(Value::Int(50)));
        // Nothing reads the focused application any more, so its hook goes.
        assert!(h.installed.is_empty(), "still installed: {:?}", h.installed);
        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert!(on_disk.contains("enabled = false"), "wrote:\n{on_disk}");
        assert!(!s.authored()[0].enabled);
        // Still holding on paper, which is what makes the card say "would be
        // holding" rather than pretending the condition stopped being true.
        assert_eq!(
            s.state().rules[0].state,
            cortisoff_ipc::RuleStateReport::Disabled
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the settings dialog sends: rows in the order a person added them,
    /// which is not the order the file sorts them into. It used to be refused
    /// as a set that "does not survive being written out".
    #[test]
    fn a_rule_listed_in_another_order_than_the_file_uses_still_saves() {
        let dir = std::env::temp_dir().join("cortisoff-save-order");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rules.toml");
        std::fs::write(&file, FOCUS_RULE).unwrap();

        let mut h = host();
        let mut s = supervisor(&h, FOCUS_RULE, Interference::clear())
            .with_rules_path(&file)
            .with_rules(parse_rules(FOCUS_RULE).unwrap());
        s.start(&mut h);

        let rule = Rule {
            name: "rows in person order".to_owned(),
            when: Condition::All(vec![
                Condition::Not(Box::new(Condition::App {
                    exe: Some("vlc.exe".to_owned()),
                    title_contains: None,
                })),
                Condition::Between {
                    start: "20:00".parse().unwrap(),
                    end: "07:00".parse().unwrap(),
                },
            ]),
            priority: 0,
            enabled: true,
            set: vec![
                Assign {
                    cap: CapabilityId::new(VIBRANCE),
                    value: Value::Int(60),
                },
                Assign {
                    cap: CapabilityId::new(TEMP),
                    value: Value::Int(3200),
                },
            ],
            transition: None,
        };
        s.save_rules(&mut h, vec![rule])
            .expect("order carries no meaning");
        assert_eq!(s.authored()[0].name, "rows in person order");
        // Reading the clock now, and the application too.
        assert_eq!(h.installed, vec![CapabilityId::new(ids::APP_FOCUS)]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The rule file is the one thing a user cannot rebuild from memory. A
    /// refused save leaves it exactly as it was.
    #[test]
    fn a_refused_save_leaves_the_file_and_the_running_rules_alone() {
        let dir = std::env::temp_dir().join("cortisoff-save-refused");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rules.toml");
        std::fs::write(&file, FOCUS_RULE).unwrap();

        let mut h = host();
        let mut s = supervisor(&h, FOCUS_RULE, Interference::clear())
            .with_rules_path(&file)
            .with_rules(parse_rules(FOCUS_RULE).unwrap());
        s.start(&mut h);

        let twice = vec![s.authored()[0].clone(), s.authored()[0].clone()];
        let refused = s
            .save_rules(&mut h, twice)
            .expect_err("two rules, one name");
        assert!(refused.contains("two rules are called"), "{refused}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), FOCUS_RULE);
        assert_eq!(s.authored().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole point of the startup gate: LightBulb is driving the ramp, so
    /// the colour temperature is left alone while everything else still works.
    #[test]
    fn a_blocked_capability_is_never_touched_and_the_rest_of_the_batch_lands() {
        let source = r#"
[[rule]]
name = "both at once"
when.app = "game.exe"
set.display.vibrance = 100
set.display.color_temp = 3400
"#;
        let mut h = host();
        let mut s = supervisor(&h, source, Interference::assess(["lightbulb.exe"], None));
        s.start(&mut h);
        s.handle(&mut h, focus("game.exe"));

        assert_eq!(h.value(TEMP), Some(Value::Int(6500)));
        assert_eq!(h.value(VIBRANCE), Some(Value::Int(100)));
        assert!(s.errors().is_empty());
    }

    #[test]
    fn a_blocked_capability_says_so_once_in_the_stream() {
        let mut h = host();
        let source = r#"
[[rule]]
name = "night"
when.app = "game.exe"
set.display.color_temp = 3400
"#;
        let mut s = supervisor(&h, source, Interference::assess(["flux.exe"], None));
        s.start(&mut h);
        s.handle(&mut h, focus("game.exe"));
        assert!(!h.active && !s.state().active);

        let warned = s
            .drain()
            .into_iter()
            .filter(|n| {
                matches!(n, Notification::Log { level: LogLevel::Warn, message } if message.contains(TEMP))
            })
            .count();
        assert_eq!(warned, 1);
    }

    #[test]
    fn both_edges_of_a_rule_reach_a_subscriber() {
        let mut h = host();
        let mut s = supervisor(&h, FOCUS_RULE, Interference::clear());
        s.start(&mut h);
        s.drain();

        s.handle(&mut h, focus("game.exe"));
        s.handle(&mut h, focus("notepad.exe"));

        let edges: Vec<(String, bool)> = s
            .drain()
            .into_iter()
            .filter_map(|n| match n {
                Notification::RuleEdge { rule, holding } => Some((rule, holding)),
                _ => None,
            })
            .collect();
        assert_eq!(
            edges,
            vec![
                ("vibrance in game".to_owned(), true),
                ("vibrance in game".to_owned(), false),
            ]
        );
    }

    /// A clean exit is the reason a normal shutdown never leaves a tint
    /// behind, and the journal has to be empty afterwards or the next run
    /// restores something nobody applied.
    #[test]
    fn shutdown_puts_everything_back_and_empties_the_journal() {
        let mut h = host();
        let mut s = supervisor(&h, FOCUS_RULE, Interference::clear());
        s.start(&mut h);
        s.handle(&mut h, focus("game.exe"));
        s.handle(&mut h, Event::Shutdown);

        assert_eq!(h.value(VIBRANCE), Some(Value::Int(50)));
        assert!(s.doctor(&h, &h.probe()).journal_dirty.is_empty());
    }

    #[test]
    fn a_schedule_rule_asks_the_host_for_exactly_one_wakeup() {
        let source = r#"
[[rule]]
name = "blue light"
when.between = "20:00-07:00"
set.display.color_temp = 3400
"#;
        let mut h = host();
        let mut s = supervisor(&h, source, Interference::clear());
        s.start(&mut h);
        // Nothing to schedule until the host says what time it is.
        assert!(h.timers.is_empty());

        s.handle(
            &mut h,
            Event::TimeOfDay {
                minute: 21 * 60,
                second: 0,
                millisecond: 0,
            },
        );
        assert_eq!(h.value(TEMP), Some(Value::Int(3400)));
        assert_eq!(h.timers.len(), 1);
        // 21:00 to 07:00 is ten hours, and not one wakeup in between.
        assert_eq!(h.timers[0].1.as_secs(), 10 * 60 * 60);
    }

    #[test]
    fn the_doctor_reports_the_gate_warnings_verbatim() {
        let h = host();
        let gate = Interference::assess(["lightbulb.exe"], None);
        let expected = gate.warnings.clone();
        let s = supervisor(&h, FOCUS_RULE, gate);
        assert_eq!(s.doctor(&h, &h.probe()).warnings, expected);
    }

    /// A rule naming a capability this machine does not have is a line in the
    /// doctor, never an error and never a panic.
    #[test]
    fn an_unsupported_rule_is_listed_rather_than_dropped() {
        let source = r#"
[[rule]]
name = "wallpaper"
when.idle = true
set.wallpaper.playback = false
"#;
        let h = host();
        let s = supervisor(&h, source, Interference::clear());
        let report = s.doctor(&h, &h.probe());
        assert_eq!(report.rules.len(), 1);
        assert!(matches!(
            report.rules[0].state,
            RuleStateReport::Unsupported { .. }
        ));
    }

    /// An unsupported rule reads idle, but nothing can ever act on it, so
    /// installing the idle source would cost for nothing.
    #[test]
    fn an_unsupported_rule_installs_no_source() {
        let source = r#"
[[rule]]
name = "wallpaper"
when.idle = true
set.wallpaper.playback = false
"#;
        let mut h = host();
        let mut s = supervisor(&h, source, Interference::clear());
        s.start(&mut h);
        assert!(h.installed.is_empty());
    }

    #[test]
    fn the_state_report_names_who_holds_what() {
        let mut h = host();
        let mut s = supervisor(&h, FOCUS_RULE, Interference::clear());
        s.start(&mut h);
        s.handle(&mut h, focus("game.exe"));

        let state = s.state();
        assert_eq!(state.claims.len(), 1);
        assert_eq!(state.claims[0].owner, "vibrance in game");
        assert_eq!(state.claims[0].value, Value::Int(100));
        assert_eq!(state.world.app.unwrap().exe, "game.exe");
    }

    /// A fade that names no start begins from the actuator's resting value,
    /// on the engine's copy only: the rules as authored keep saying nothing.
    #[test]
    fn a_fade_with_no_start_is_given_the_actuators_rest() {
        let source = r#"
[[rule]]
name = "night"
when.between = "20:00-07:00"
set.display.color_temp = 3200
transition.duration = "40m"
"#;
        let h = host();
        let authored = parse_rules(source).expect("test rules parse");
        assert!(authored[0].transition.as_ref().unwrap().from.is_empty());

        let running = resting(&h, authored.clone());
        assert_eq!(
            running[0].transition.as_ref().unwrap().from,
            vec![Assign {
                cap: CapabilityId::new(TEMP),
                value: Value::Int(6500),
            }]
        );
        assert!(authored[0].transition.as_ref().unwrap().from.is_empty());
    }
}
