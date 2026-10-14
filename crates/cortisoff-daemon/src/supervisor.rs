//! The loop's body: everything between an event arriving from the platform
//! and the hardware being touched.
//!
//! Three things meet here. [`Claims`] decides who wins each capability and
//! fades between winners. The plugin [`Runtime`] hears events, timers and
//! hotkeys and answers with claims. The [`Dispatcher`] journals a baseline
//! before anything is applied. The platform owns the loop itself, so this is
//! not a thread: it is a function of an event.

use std::path::PathBuf;
use std::time::Duration;

use cortisoff_core::{fadeable, priority, CapTable, Claims, OwnerId, FADE_TIMER, PREVIEW_FADE_MS};
use cortisoff_host::{
    Action, ActuatorError, CapIndex, CapabilityId, CapabilitySet, Dispatcher, Event, Host,
    JournalStore, LogLevel, TimerId, Value,
};
use cortisoff_ipc::{
    CapabilityReport, ClaimReport, DoctorReport, Notification, Origin, PluginReport, StateReport,
    WorldReport,
};
use cortisoff_runtime::{install, Command, Env, Package, Runtime, Store};

use crate::startup::Interference;

/// The plugins' own timers, all folded onto one host timer armed for the
/// earliest.
pub const PLUGIN_TIMER: TimerId = TimerId(2);

/// While a plugin is read from a folder, its files are looked at this often
/// so a save reloads it.
pub const WATCH_TIMER: TimerId = TimerId(3);
const WATCH_MS: u64 = 1000;

/// Past this many unread notifications the oldest are dropped, so a stalled
/// `cortisoff-cli trace` costs a bounded amount of memory.
const MAX_PENDING: usize = 4096;

pub struct Supervisor<J> {
    claims: Claims,
    runtime: Runtime,
    store: Store,
    dispatcher: Dispatcher<J>,
    gate: Interference,
    probed: CapabilitySet,
    /// Where installed plugins live, for a rescan. `None` in tests.
    plugins_dir: Option<PathBuf>,
    pending: Vec<Notification>,
    warnings: Vec<String>,
    errors: Vec<ActuatorError>,
    /// When [`PLUGIN_TIMER`] is armed for, on the monotonic clock.
    plugin_armed: Option<u64>,
    watching: bool,
    stopped: bool,
    /// What the host was last told about fades.
    fading: bool,
}

impl<J: JournalStore> Supervisor<J> {
    pub fn new(
        probed: CapabilitySet,
        journal: J,
        gate: Interference,
        runtime: Runtime,
        store: Store,
    ) -> Self {
        let warnings = gate.warnings.clone();
        Self {
            claims: Claims::new(CapTable::from_set(&probed)),
            runtime,
            store,
            dispatcher: Dispatcher::new(journal),
            gate,
            probed,
            plugins_dir: None,
            pending: Vec::new(),
            warnings,
            errors: Vec::new(),
            plugin_armed: None,
            watching: false,
            stopped: false,
            fading: false,
        }
    }

    /// Where `Rescan` and `Remove` look for installed plugins.
    pub fn with_plugins_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.plugins_dir = Some(dir.into());
        self
    }

    /// Sentences for the doctor that came from outside, such as a journal
    /// file that could not be read.
    pub fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Put back whatever a previous run left applied, then start every
    /// enabled plugin against the world the host has already pushed.
    ///
    /// Recovery comes first on purpose: a plugin that claims at once would
    /// otherwise capture a poisoned baseline as this run's neutral.
    pub fn start(&mut self, host: &mut dyn Host) {
        self.stopped = false;
        for e in self.dispatcher.recover(&*host) {
            self.fail(e);
        }
        if let Some(now @ Event::TimeOfDay { .. }) = host.time_of_day() {
            let (_, actions) = self.claims.on_event(&now);
            self.run(host, actions);
        }
        let commands = self.call(host, |rt, env| rt.start_all(env));
        self.apply(host, commands);
        self.settle(host);
    }

    /// One event, all the way to the hardware.
    pub fn handle(&mut self, host: &mut dyn Host, event: Event) {
        match event {
            Event::TimerFired { id } if id == FADE_TIMER => {
                let now = host.monotonic_ms();
                let actions = self.claims.tick(now);
                self.run(host, actions);
            }
            Event::TimerFired { id } if id == PLUGIN_TIMER => {
                self.plugin_armed = None;
                let commands = self.call(host, |rt, env| rt.on_timer(env));
                self.apply(host, commands);
            }
            Event::TimerFired { id } if id == WATCH_TIMER => {
                self.watching = false;
                let commands = self.call(host, |rt, env| rt.reload_changed(env));
                self.apply(host, commands);
            }
            Event::TimerFired { .. } => {}
            Event::Hotkey { id } => {
                let commands = self.call(host, |rt, env| rt.on_hotkey(env, id));
                self.apply(host, commands);
            }
            Event::Shutdown => self.shutdown(host),
            event => {
                self.note(Notification::Event(event.clone()));
                // Monitors moved. Whatever is applied may now cover hardware
                // that arrived after its baseline was captured, so the
                // baseline grows before anything else is pushed at it.
                if matches!(event, Event::DisplayConfigChanged) {
                    for e in self.dispatcher.rebaseline(&*host) {
                        self.fail(e);
                    }
                }
                let (_, actions) = self.claims.on_event(&event);
                self.run(host, actions);
                let commands = self.call(host, |rt, env| rt.on_event(env, &event));
                self.apply(host, commands);
            }
        }
        // An HTTP answer a plugin waited for wakes the loop with no event of
        // its own, so every turn looks.
        let commands = self.call(host, |rt, env| rt.on_http(env));
        self.apply(host, commands);
        self.settle(host);
    }

    /// Nothing arrived but the loop woke: an HTTP answer, most likely.
    pub fn idle_turn(&mut self, host: &mut dyn Host) {
        let commands = self.call(host, |rt, env| rt.on_http(env));
        if !commands.is_empty() {
            self.apply(host, commands);
            self.settle(host);
        }
    }

    /// A call into the runtime with an [`Env`] built from this supervisor's
    /// world and probe.
    fn call<R>(&mut self, host: &mut dyn Host, f: impl FnOnce(&mut Runtime, &mut Env) -> R) -> R {
        let now = host.monotonic_ms();
        let mut env = Env {
            host,
            world: self.claims.world(),
            probed: &self.probed,
            now,
        };
        f(&mut self.runtime, &mut env)
    }

    /// Carry out what plugins asked for.
    fn apply(&mut self, host: &mut dyn Host, commands: Vec<Command>) {
        let mut save = false;
        for command in commands {
            match command {
                Command::Claim {
                    owner,
                    cap,
                    value,
                    priority,
                    fade_ms,
                } => {
                    let Ok(index) = self.claims.index_of(&cap) else {
                        continue;
                    };
                    let fade = if self.stopped { 0 } else { fade_ms };
                    let rest = rest_of(host, &cap);
                    let owner = OwnerId::Plugin(owner);
                    let now = host.monotonic_ms();
                    let actions =
                        self.claims
                            .claim(index, owner, priority, value, fade, now, rest.as_ref());
                    self.run(host, actions);
                    let shown = host.monotonic_ms();
                    self.claims.begin_at(index, owner, now, shown);
                }
                Command::Release {
                    owner,
                    cap,
                    fade_ms,
                } => {
                    let Ok(index) = self.claims.index_of(&cap) else {
                        continue;
                    };
                    self.release(host, index, OwnerId::Plugin(owner), fade_ms);
                }
                Command::ReleaseAll { owner, fade_ms } => {
                    let owner = OwnerId::Plugin(owner);
                    if fade_ms == 0 || self.stopped {
                        let actions = self.claims.release_owner(owner);
                        self.run(host, actions);
                    } else {
                        for index in self.held_by(owner) {
                            self.release(host, index, owner, fade_ms);
                        }
                    }
                }
                Command::Run { program, args } => self.spawn(&program, &args),
                Command::Log { level, message } => self.note(Notification::Log { level, message }),
                Command::Changed { id } => {
                    if let Some(report) = self.report(&id) {
                        self.note(Notification::Plugin(Box::new(report)));
                    }
                }
                Command::ListChanged => self.note(Notification::Plugins),
                Command::Save => save = true,
            }
        }
        if save {
            self.save_store();
        }
    }

    fn release(&mut self, host: &mut dyn Host, index: CapIndex, owner: OwnerId, fade_ms: u64) {
        let fade = if self.stopped { 0 } else { fade_ms };
        let cap = self.claims.arbiter().table().id(index).clone();
        let rest = self.baseline(host, &cap);
        let now = host.monotonic_ms();
        let actions = self.claims.release(index, owner, fade, now, rest.as_ref());
        self.run(host, actions);
    }

    /// Where a release fades to once nobody holds the capability: the
    /// baseline captured before the first apply when it is a number, the
    /// actuator's own idea of rest otherwise. A colour temperature captures
    /// its gamma ramps, which no fade can travel to, so it fades to 6600 K
    /// and the restore at the end puts the ramps themselves back.
    fn baseline(&self, host: &dyn Host, cap: &CapabilityId) -> Option<Value> {
        self.dispatcher
            .baseline(cap)
            .filter(|v| fadeable(v, v))
            .cloned()
            .or_else(|| rest_of(host, cap))
    }

    fn held_by(&self, owner: OwnerId) -> Vec<CapIndex> {
        let arbiter = self.claims.arbiter();
        (0..arbiter.table().len())
            .map(|i| CapIndex(i as u16))
            .filter(|&cap| arbiter.stack(cap).iter().any(|c| c.owner == owner))
            .collect()
    }

    fn save_store(&mut self) {
        self.runtime.store_into(&mut self.store);
        if let Err(e) = self.store.save() {
            self.note(Notification::Log {
                level: LogLevel::Error,
                message: format!("plugin settings were not saved: {e}"),
            });
        }
    }

    /// After every turn: hand the host what plugins now watch and which keys
    /// they listen for, and arm the timers they need.
    fn settle(&mut self, host: &mut dyn Host) {
        if let Some((sources, thresholds)) = self.runtime.take_sources() {
            host.set_idle_thresholds(&thresholds);
            if let Err(e) = host.install_sources(&sources) {
                self.fail(e);
            }
        }
        if let Some(hotkeys) = self.runtime.take_hotkeys() {
            let refused = host.set_hotkeys(&hotkeys);
            if !refused.is_empty() {
                let commands = self.runtime.hotkeys_refused(refused);
                self.apply(host, commands);
            }
        }
        let now = host.monotonic_ms();
        match self.runtime.next_due() {
            Some(due) if self.plugin_armed != Some(due) => {
                let after = due.saturating_sub(now);
                let slack = (after / 20).clamp(1, 500);
                host.set_timer(
                    PLUGIN_TIMER,
                    Duration::from_millis(after),
                    Duration::from_millis(slack),
                );
                self.plugin_armed = Some(due);
            }
            None if self.plugin_armed.is_some() => {
                host.cancel_timer(PLUGIN_TIMER);
                self.plugin_armed = None;
            }
            _ => {}
        }
        let watch = !self.stopped && self.runtime.watches_folders();
        if watch && !self.watching {
            host.set_timer(
                WATCH_TIMER,
                Duration::from_millis(WATCH_MS),
                Duration::from_millis(250),
            );
            self.watching = true;
        } else if !watch && self.watching {
            host.cancel_timer(WATCH_TIMER);
            self.watching = false;
        }
        host.set_active(self.active());
    }

    // ---- what clients ask -------------------------------------------------

    /// A value the user set by hand. It outranks every plugin until it is
    /// cleared, and the plugins underneath keep holding meanwhile.
    pub fn manual_set(
        &mut self,
        host: &mut dyn Host,
        cap: &CapabilityId,
        value: Value,
    ) -> Result<(), String> {
        let index = self.settable(host, cap)?;
        let now = host.monotonic_ms();
        let actions = self.claims.claim(
            index,
            OwnerId::Manual,
            priority::MANUAL,
            value,
            0,
            now,
            None,
        );
        self.run(host, actions);
        self.settle(host);
        Ok(())
    }

    pub fn manual_clear(&mut self, host: &mut dyn Host, cap: &CapabilityId) -> Result<(), String> {
        let index = self.settable(host, cap)?;
        self.release(host, index, OwnerId::Manual, 0);
        self.settle(host);
        Ok(())
    }

    /// A slider mid-drag. Every call pushes the value at once.
    pub fn preview(
        &mut self,
        host: &mut dyn Host,
        cap: &CapabilityId,
        value: Value,
    ) -> Result<(), String> {
        let index = self.settable(host, cap)?;
        let now = host.monotonic_ms();
        let actions = self.claims.claim(
            index,
            OwnerId::Preview,
            priority::PREVIEW,
            value,
            0,
            now,
            None,
        );
        self.run(host, actions);
        Ok(())
    }

    /// The drag ended, or the client that was dragging went away. The value
    /// eases back to whatever holds the capability underneath.
    pub fn preview_end(&mut self, host: &mut dyn Host, cap: &CapabilityId) -> Result<(), String> {
        let index = self.claims.index_of(cap)?;
        let rest = self.baseline(host, cap);
        let now = host.monotonic_ms();
        let actions =
            self.claims
                .release(index, OwnerId::Preview, PREVIEW_FADE_MS, now, rest.as_ref());
        self.run(host, actions);
        Ok(())
    }

    fn settable(&self, host: &dyn Host, cap: &CapabilityId) -> Result<CapIndex, String> {
        if host.actuator(cap).is_none() {
            return Err(match self.probed.reason_missing(cap) {
                Some(why) => format!("{cap}: {why}"),
                None => format!("{cap} is not something cortisoff can set on this machine"),
            });
        }
        self.claims.index_of(cap)
    }

    pub fn set_enabled(&mut self, host: &mut dyn Host, id: &str, on: bool) -> Result<(), String> {
        let commands = self.call(host, |rt, env| rt.set_enabled(env, id, on))?;
        self.apply(host, commands);
        self.settle(host);
        Ok(())
    }

    pub fn set_settings(
        &mut self,
        host: &mut dyn Host,
        id: &str,
        settings: serde_json::Value,
    ) -> Result<(), String> {
        let commands = self.call(host, |rt, env| rt.set_settings(env, id, settings))?;
        self.apply(host, commands);
        self.settle(host);
        Ok(())
    }

    pub fn message(
        &mut self,
        host: &mut dyn Host,
        id: &str,
        name: &str,
        data: &serde_json::Value,
    ) -> Result<(), String> {
        let commands = self.call(host, |rt, env| rt.message(env, id, name, data))?;
        self.apply(host, commands);
        self.settle(host);
        Ok(())
    }

    /// Read every plugin folder again: one was installed, updated or removed.
    pub fn rescan(&mut self, host: &mut dyn Host) -> Result<(), String> {
        let packages = self.discover();
        let store = self.store.clone();
        let commands = self.call(host, |rt, env| rt.replace(env, packages, &store));
        self.apply(host, commands);
        self.save_store();
        self.settle(host);
        Ok(())
    }

    fn discover(&mut self) -> Vec<Package> {
        let Some(dir) = self.plugins_dir.clone() else {
            return cortisoff_runtime::builtin();
        };
        let (packages, problems) = cortisoff_runtime::discover(&dir, &self.store.links);
        for problem in problems {
            self.note(Notification::Log {
                level: LogLevel::Warn,
                message: problem,
            });
        }
        packages
    }

    /// Run a plugin from a folder in place, switched on, reloaded on save.
    pub fn link(&mut self, host: &mut dyn Host, path: &str) -> Result<String, String> {
        let dir = std::fs::canonicalize(path).map_err(|e| format!("{path}: {e}"))?;
        let pkg = Package::from_dir(&dir, Origin::Linked)?;
        let id = pkg.manifest.id.clone();
        if !self.store.links.contains(&dir) {
            self.store.links.push(dir);
        }
        self.store.plugins.entry(id.clone()).or_default().enabled = true;
        self.rescan(host)?;
        Ok(id)
    }

    /// Forget a linked folder or delete an installed plugin. A built-in one
    /// only switches off.
    pub fn remove(&mut self, host: &mut dyn Host, id: &str) -> Result<(), String> {
        match self.runtime.origin(id) {
            None => return Err(format!("no plugin is called {id:?}")),
            Some(Origin::Builtin) => {
                return Err(format!(
                    "{id} is built in. It switches off, it does not uninstall"
                ))
            }
            Some(Origin::Linked) => {
                let dir = self.runtime.dir(id);
                self.store.links.retain(|l| Some(l) != dir.as_ref());
            }
            Some(Origin::Installed) => {
                let (Some(dir), Some(root)) = (self.runtime.dir(id), self.plugins_dir.clone())
                else {
                    return Err(format!("{id} has no folder to remove"));
                };
                // Stopped first: on Windows a file in use cannot be deleted.
                self.set_enabled(host, id, false)?;
                install::uninstall(&dir, &root)?;
            }
        }
        self.store.plugins.remove(id);
        self.rescan(host)
    }

    /// Hand every screen back the way it was found.
    ///
    /// Plugins hear `stop` first, then whatever still holds anything is
    /// released at once. The restores go through the dispatcher like any
    /// other action, so a journal entry is cleared only once its restore
    /// succeeded and a monitor that refuses keeps its entry for the next run.
    pub fn shutdown(&mut self, host: &mut dyn Host) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        let commands = self.call(host, |rt, env| rt.shutdown(env));
        self.apply(host, commands);
        let actions = self.claims.shutdown();
        self.run(host, actions);
        host.cancel_timer(PLUGIN_TIMER);
        host.cancel_timer(WATCH_TIMER);
        host.set_hotkeys(&[]);
        host.set_active(false);
    }

    /// The last thing a subscriber sees, so a client can tell a clean exit
    /// from a crash.
    pub fn note_closing(&mut self) {
        self.note(Notification::Closing);
    }

    pub fn drain(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.pending)
    }

    pub fn errors(&self) -> &[ActuatorError] {
        &self.errors
    }

    // ---- the hardware ----------------------------------------------------

    /// Execute a batch, in order. Apply and restore go to the dispatcher, and
    /// everything it hands back is routed here.
    fn run(&mut self, host: &mut dyn Host, actions: Vec<Action>) {
        // Before anything else: a fade whose last frame lands on the value
        // the screen already shows ends with no action at all.
        let fading = self.claims.moving();
        if fading != self.fading {
            self.fading = fading;
            host.set_fading(fading);
        }
        if actions.is_empty() {
            return;
        }
        let (allowed, refused) = self.split_blocked(actions);
        for cap in refused {
            self.note(Notification::Log {
                level: LogLevel::Warn,
                message: format!("{cap} is left alone this run, another application drives it"),
            });
        }
        for action in &allowed {
            if matches!(
                action,
                Action::Apply { .. } | Action::RestoreBaseline { .. }
            ) {
                self.note(Notification::Action(action.clone()));
            }
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
                Action::Apply { .. } | Action::RestoreBaseline { .. } => {}
            }
        }
    }

    /// Nothing is applied to any screen: no plugin, manual value or preview
    /// holds any capability, and no release is still fading. The moment an
    /// update can stop everything without anything visible moving.
    pub fn quiet(&self) -> bool {
        let arbiter = self.claims.arbiter();
        (0..arbiter.table().len()).all(|i| arbiter.holder(CapIndex(i as u16)).is_none())
    }

    /// Install updates without asking, as stored.
    pub fn auto_update(&self) -> bool {
        self.store.app.auto_update
    }

    pub fn set_auto_update(&mut self, on: bool) {
        if self.store.app.auto_update != on {
            self.store.app.auto_update = on;
            self.save_store();
        }
    }

    /// A notification from outside the plugins, such as an update check.
    pub fn notify(&mut self, notification: Notification) {
        self.note(notification);
    }

    /// A running plugin, or a manual value that is not blocked.
    fn active(&self) -> bool {
        if self.stopped {
            return false;
        }
        if self.runtime.active() {
            return true;
        }
        let arbiter = self.claims.arbiter();
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
                Some(cap) if self.gate.blocks(cap) => {
                    if !refused.contains(cap) {
                        refused.push(cap.clone());
                    }
                }
                _ => allowed.push(action),
            }
        }
        (allowed, refused)
    }

    fn spawn(&mut self, program: &str, args: &[String]) {
        // Detached on purpose: the daemon must never sit waiting on a child.
        let mut command = std::process::Command::new(program);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match command.spawn() {
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

    // ---- reports ---------------------------------------------------------

    fn owner_name(&self, owner: OwnerId) -> String {
        match owner {
            OwnerId::Plugin(key) => self
                .runtime
                .name_of(key)
                .map_or_else(|| format!("plugin {key}"), str::to_owned),
            OwnerId::Manual => "manual".to_owned(),
            OwnerId::Preview => "preview".to_owned(),
        }
    }

    fn holds(&self, key: u32) -> Vec<CapabilityId> {
        let table = self.claims.arbiter().table();
        self.held_by(OwnerId::Plugin(key))
            .into_iter()
            .map(|cap| table.id(cap).clone())
            .collect()
    }

    pub fn plugins(&self) -> Vec<PluginReport> {
        let mut reports = self.runtime.reports();
        for (report, key) in reports.iter_mut().zip(self.runtime.keys()) {
            report.holds = self.holds(key);
        }
        reports
    }

    pub fn report(&self, id: &str) -> Option<PluginReport> {
        let mut report = self.runtime.report(id)?;
        if let Some(key) = self.runtime.key_of(id) {
            report.holds = self.holds(key);
        }
        Some(report)
    }

    pub fn state(&self) -> StateReport {
        let w = self.claims.world();
        let arbiter = self.claims.arbiter();
        let table = arbiter.table();
        let claims = (0..table.len())
            .filter_map(|i| {
                let cap = CapIndex(i as u16);
                // Highest priority first, the latest claim first among
                // equals: the order the arbiter picks a winner in.
                let mut stack: Vec<_> = arbiter.stack(cap).iter().rev().collect();
                stack.sort_by_key(|c| std::cmp::Reverse(c.priority));
                let top = stack.first()?;
                Some(ClaimReport {
                    cap: table.id(cap).clone(),
                    owner: self.owner_name(top.owner),
                    priority: top.priority,
                    value: top.value.clone(),
                    shadowed: stack[1..]
                        .iter()
                        .map(|c| self.owner_name(c.owner))
                        .collect(),
                })
            })
            .collect();
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
            plugins: self.plugins(),
            claims,
        }
    }

    /// `host` answers one question per capability: is there an actuator
    /// behind it. A sensor and an actuator are both capabilities and the
    /// probe does not separate them.
    pub fn doctor(&self, host: &dyn Host) -> DoctorReport {
        let probed = &self.probed;
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
            plugins: self.plugins(),
            journal_dirty,
            warnings: self.warnings.clone(),
        }
    }
}

fn rest_of(host: &dyn Host, cap: &CapabilityId) -> Option<Value> {
    host.actuator(cap).and_then(|a| a.rest())
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use cortisoff_host::mock::{MemJournal, MockHost, Trace};
    use cortisoff_runtime::{Files, Manifest};

    /// A plugin from one script, switched on.
    pub(crate) fn plugin(id: &str, script: &'static str) -> Package {
        let manifest = format!(
            "id = \"{id}\"\nname = \"{id}\"\nversion = \"1\"\nplatforms = [\"windows\", \"linux\", \"macos\"]\n"
        );
        let files: &'static [(&'static str, &'static str)] =
            Box::leak(Box::new([("init.lua", script)]));
        Package {
            manifest: Manifest::parse(&manifest).unwrap(),
            files: Files::Builtin(files),
            origin: Origin::Installed,
            url: None,
        }
    }

    pub(crate) fn supervisor(host: &MockHost, packages: Vec<Package>) -> Supervisor<MemJournal> {
        let mut store = Store::in_memory();
        for p in &packages {
            store
                .plugins
                .entry(p.manifest.id.clone())
                .or_default()
                .enabled = true;
        }
        let runtime = Runtime::new(packages, &store, None);
        Supervisor::new(
            host.probe(),
            MemJournal::new(Trace::default()),
            Interference::clear(),
            runtime,
            store,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{plugin, supervisor};
    use super::*;
    use cortisoff_host::ids;
    use cortisoff_host::mock::{MemJournal, MockHost, Trace};

    fn host() -> MockHost {
        let mut host = MockHost::new()
            .with_actuator(ids::DISPLAY_VIBRANCE, Value::Int(50), false)
            .with_actuator(ids::DISPLAY_COLOR_TEMP, Value::Int(6500), false);
        host.now_ms = 10_000;
        host
    }

    const GAME: &str = "return {
        start = function(cx) cx:watch('app.focus') end,
        on = function(cx, s, ev)
          if ev.exe == 'game.exe' then cx:claim('display.vibrance', 100, { priority = 100 })
          else cx:release('display.vibrance') end
        end,
    }";

    fn focus(exe: &str) -> Event {
        Event::AppFocus(cortisoff_host::AppRef {
            exe: exe.to_owned(),
            title: None,
            pid: 1,
            started_at_ms: 0,
        })
    }

    #[test]
    fn a_plugin_claim_reaches_the_hardware_and_its_release_gives_it_back() {
        let mut host = host();
        let mut sup = supervisor(&host, vec![plugin("game", GAME)]);
        sup.start(&mut host);
        assert_eq!(host.installed, vec![CapabilityId::new(ids::APP_FOCUS)]);
        sup.handle(&mut host, focus("game.exe"));
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(100)));
        assert_eq!(
            sup.plugins()[0].holds,
            vec![CapabilityId::new(ids::DISPLAY_VIBRANCE)]
        );
        sup.handle(&mut host, focus("notepad.exe"));
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(50)));
    }

    #[test]
    fn a_manual_value_beats_the_plugin_and_gives_it_back_on_clear() {
        let mut host = host();
        let mut sup = supervisor(&host, vec![plugin("game", GAME)]);
        sup.start(&mut host);
        sup.handle(&mut host, focus("game.exe"));
        let cap = CapabilityId::new(ids::DISPLAY_VIBRANCE);
        sup.manual_set(&mut host, &cap, Value::Int(10)).unwrap();
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(10)));
        let state = sup.state();
        assert_eq!(state.claims[0].owner, "manual");
        assert_eq!(state.claims[0].shadowed, vec!["game".to_owned()]);
        sup.manual_clear(&mut host, &cap).unwrap();
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(100)));
    }

    #[test]
    fn switching_a_plugin_off_fades_its_claims_away() {
        let mut host = host();
        let mut sup = supervisor(
            &host,
            vec![plugin(
                "warm",
                "return { start = function(cx) cx:claim('display.color_temp', 3200) end }",
            )],
        );
        sup.start(&mut host);
        assert_eq!(host.value(ids::DISPLAY_COLOR_TEMP), Some(Value::Int(3200)));
        sup.set_enabled(&mut host, "warm", false).unwrap();
        // Fading: one step in, not back yet.
        assert_ne!(host.value(ids::DISPLAY_COLOR_TEMP), Some(Value::Int(6500)));
        for _ in 0..100 {
            host.now_ms += 40;
            sup.handle(&mut host, Event::TimerFired { id: FADE_TIMER });
        }
        assert_eq!(host.value(ids::DISPLAY_COLOR_TEMP), Some(Value::Int(6500)));
        assert!(sup.plugins()[0].holds.is_empty());
    }

    #[test]
    fn a_release_fades_even_when_the_baseline_is_no_number() {
        // A colour temperature captures its gamma ramps. The release still
        // travels to where the screen rests, then the ramps go back. The
        // plugin lets go on every clock event, as Night light does after its
        // morning edge, and every fade frame brings one.
        let mut host = MockHost::new().with_opaque_actuator(
            ids::DISPLAY_COLOR_TEMP,
            Value::Text("ramps".to_owned()),
            Value::Int(6600),
        );
        host.now_ms = 10_000;
        let mut sup = supervisor(
            &host,
            vec![plugin(
                "warm",
                "return {
                   start = function(cx) cx:watch('clock') cx:claim('display.color_temp', 3200) end,
                   on = function(cx) cx:release('display.color_temp', { fade = 1000 }) end,
                 }",
            )],
        );
        sup.start(&mut host);
        assert_eq!(host.value(ids::DISPLAY_COLOR_TEMP), Some(Value::Int(3200)));
        let clock = Event::TimeOfDay {
            minute: 420,
            second: 0,
            millisecond: 0,
        };
        let mut seen = Vec::new();
        for _ in 0..100 {
            sup.handle(&mut host, clock.clone());
            host.now_ms += 40;
            sup.handle(&mut host, Event::TimerFired { id: FADE_TIMER });
            if let Some(Value::Int(k)) = host.value(ids::DISPLAY_COLOR_TEMP) {
                seen.push(k);
            }
        }
        assert!(
            seen.iter().any(|k| *k > 3300 && *k < 6500),
            "passes through the middle: {seen:?}"
        );
        assert!(host
            .trace()
            .events()
            .iter()
            .any(|e| e == "apply display.color_temp Text(\"ramps\")"));
        assert!(sup.plugins()[0].holds.is_empty());
    }

    #[test]
    fn a_fade_runs_from_its_first_frame_not_from_the_request() {
        // A colour temperature reads its panels over DDC/CI before the first
        // frame, half a second. The fade must not have run on meanwhile, or
        // the next frame jumps half of it.
        let mut host = MockHost::new().with_opaque_actuator(
            ids::DISPLAY_COLOR_TEMP,
            Value::Text("ramps".to_owned()),
            Value::Int(6600),
        );
        host.slow_capture(ids::DISPLAY_COLOR_TEMP, 500);
        host.now_ms = 10_000;
        let mut sup = supervisor(
            &host,
            vec![plugin(
                "warm",
                "return { start = function(cx) cx:claim('display.color_temp', 3200, { fade = 1000 }) end }",
            )],
        );
        sup.start(&mut host);
        assert_eq!(host.value(ids::DISPLAY_COLOR_TEMP), Some(Value::Int(6600)));
        host.now_ms += 40;
        sup.handle(&mut host, Event::TimerFired { id: FADE_TIMER });
        let Some(Value::Int(first)) = host.value(ids::DISPLAY_COLOR_TEMP) else {
            panic!("no number on screen");
        };
        assert!(first > 6400, "the first step is a small one: {first}");
        assert!(host.fading, "the host knows a fade runs");
        // Frames close enough that the last ones already show 3200 K, so the
        // one that ends the fade has nothing to apply.
        for _ in 0..300 {
            host.now_ms += 5;
            sup.handle(&mut host, Event::TimerFired { id: FADE_TIMER });
        }
        assert_eq!(host.value(ids::DISPLAY_COLOR_TEMP), Some(Value::Int(3200)));
        assert!(!host.fading, "and that it ended");
    }

    #[test]
    fn plugin_timers_ride_one_host_timer() {
        let mut host = host();
        let mut sup = supervisor(
            &host,
            vec![plugin(
                "t",
                "local n = 0
                 return { start = function(cx)
                   cx:every(5000, function(cx) n = n + 1 cx:publish({ n = n }) end)
                 end }",
            )],
        );
        sup.start(&mut host);
        assert!(host
            .timers
            .iter()
            .any(|(id, after, _)| *id == PLUGIN_TIMER && after.as_millis() == 5000));
        host.now_ms += 5000;
        sup.handle(&mut host, Event::TimerFired { id: PLUGIN_TIMER });
        assert_eq!(sup.plugins()[0].state["n"].as_f64(), Some(1.0));
        assert_eq!(
            host.timers
                .iter()
                .filter(|(id, ..)| *id == PLUGIN_TIMER)
                .count(),
            2,
            "armed again for the next one"
        );
    }

    #[test]
    fn hotkeys_are_registered_and_pressed() {
        let mut host = host();
        let mut sup = supervisor(
            &host,
            vec![plugin(
                "k",
                "return { start = function(cx) cx:hotkey('ctrl+alt+k', function(cx) cx:type('x') end) end }",
            )],
        );
        sup.start(&mut host);
        assert_eq!(host.hotkeys.len(), 1);
        let id = host.hotkeys[0].id;
        sup.handle(&mut host, Event::Hotkey { id });
        assert_eq!(host.desktop, vec!["type x".to_owned()]);
        sup.shutdown(&mut host);
        assert!(host.hotkeys.is_empty());
    }

    #[test]
    fn shutting_down_restores_at_once() {
        let mut host = host();
        let mut sup = supervisor(&host, vec![plugin("game", GAME)]);
        sup.start(&mut host);
        sup.handle(&mut host, focus("game.exe"));
        sup.handle(&mut host, Event::Shutdown);
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(50)));
        assert!(!host.active);
    }

    #[test]
    fn a_preview_eases_back_to_what_was_there() {
        let mut host = host();
        let mut sup = supervisor(&host, Vec::new());
        sup.start(&mut host);
        let cap = CapabilityId::new(ids::DISPLAY_VIBRANCE);
        sup.preview(&mut host, &cap, Value::Int(90)).unwrap();
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(90)));
        sup.preview_end(&mut host, &cap).unwrap();
        for _ in 0..60 {
            host.now_ms += 40;
            sup.handle(&mut host, Event::TimerFired { id: FADE_TIMER });
        }
        assert_eq!(host.value(ids::DISPLAY_VIBRANCE), Some(Value::Int(50)));
    }

    #[test]
    fn setting_what_the_machine_lacks_is_a_sentence() {
        let mut host = host();
        let mut sup = supervisor(&host, Vec::new());
        let err = sup
            .manual_set(&mut host, &CapabilityId::new("display.mode"), Value::Int(1))
            .unwrap_err();
        assert!(err.contains("display.mode"), "{err}");
    }

    #[test]
    fn a_builtin_cannot_be_removed() {
        let mut host = host();
        let runtime = Runtime::new(cortisoff_runtime::builtin(), &Store::in_memory(), None);
        let mut sup = Supervisor::new(
            host.probe(),
            MemJournal::new(Trace::default()),
            Interference::clear(),
            runtime,
            Store::in_memory(),
        );
        sup.start(&mut host);
        assert!(sup
            .remove(&mut host, "night-light")
            .unwrap_err()
            .contains("built in"));
    }
}
