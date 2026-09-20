//! The whole decision layer, as a pure function.
//!
//! `on_event` takes an event and returns actions. No clock, no thread, no OS,
//! no I/O, and no interior mutability beyond its own state. Every behaviour the
//! project ships is therefore testable without a GPU, without admin rights and
//! without Windows.

use std::time::Duration;

use cortisoff_host::{Action, CapIndex, CapabilityId, Event, TimerId, Value};

use crate::arbiter::{Arbiter, OwnerId};
use crate::rule::{Compiled, Rule, RuleState};
use crate::table::CapTable;
use crate::transition::{self, DAY};
use crate::world::World;

/// The one timer the engine owns. Setting it again replaces the pending one,
/// so the engine never juggles timer ids.
pub const SCHEDULE_TIMER: TimerId = TimerId(0);

/// The timer a preview fades back on. Distinct from the schedule timer so
/// ending a drag never cancels a fade the schedule is running.
pub const PREVIEW_TIMER: TimerId = TimerId(1);

/// How a preview fades back: this many steps, this far apart. Twenty steps
/// forty milliseconds apart is under a second, long enough to read as a fade
/// and short enough that the slider feels released.
const PREVIEW_STEPS: u32 = 20;
const PREVIEW_STEP: Duration = Duration::from_millis(40);

/// A preview on its way back to what held the capability before the drag.
#[derive(Debug, Clone, PartialEq)]
struct Return {
    cap: CapIndex,
    from: Value,
    to: Value,
    /// Steps taken so far, out of [`PREVIEW_STEPS`].
    step: u32,
}

#[derive(Debug)]
pub struct Engine {
    arbiter: Arbiter,
    rules: Vec<Compiled>,
    world: World,
    /// Whether each rule's condition held at the last evaluation. Edge
    /// detection lives here: a rule claims when this flips up and releases when
    /// it flips down, so no rule author ever writes an undo path.
    holding: Vec<bool>,
    /// Every second of the day at which some active rule's schedule can
    /// change truth value or start or stop fading. The engine asks to be woken
    /// at exactly these, which is what keeps a blue light schedule from
    /// costing a wakeup per minute.
    clock_edges: Vec<u32>,
    /// Every fade of every active rule, as `(start, length)`. Inside one the
    /// engine steps every 40 milliseconds, and outside all of them it sleeps until
    /// the next edge.
    ramps: Vec<(u32, u32)>,
    /// Previews fading back after a drag ended, one per capability at most.
    returns: Vec<Return>,
}

impl Engine {
    pub fn new(table: CapTable, rules: Vec<Rule>) -> Self {
        let rules: Vec<Compiled> = rules
            .into_iter()
            .enumerate()
            .map(|(i, r)| Compiled::bind(i as u32, r, &table))
            .collect();
        let (clock_edges, ramps) = schedule(&rules);
        Self {
            holding: vec![false; rules.len()],
            arbiter: Arbiter::new(table),
            rules,
            world: World::default(),
            clock_edges,
            ramps,
            returns: Vec::new(),
        }
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn arbiter(&self) -> &Arbiter {
        &self.arbiter
    }

    /// Every loaded rule, supported or not. This is what `cortisoff doctor`
    /// prints, which is why unsupported rules stay in the list.
    pub fn rules(&self) -> &[Compiled] {
        &self.rules
    }

    /// Rules whose condition holds right now, for `cortisoff trace`.
    pub fn firing(&self) -> impl Iterator<Item = &Compiled> {
        self.rules
            .iter()
            .zip(&self.holding)
            .filter_map(|(r, held)| held.then_some(r))
    }

    pub fn on_event(&mut self, event: &Event) -> Vec<Action> {
        match event {
            // A clean exit. Every claim goes, so the host restores baselines
            // before the process dies.
            Event::Shutdown => self.arbiter.release_everything(),

            // Monitors moved. Anything believed to be applied may have landed
            // on hardware that is now gone, so the belief is dropped and every
            // held claim is pushed again.
            Event::DisplayConfigChanged => {
                self.arbiter.invalidate();
                self.sync()
            }

            // Only one capability lost its value. Every other one is left
            // alone, so a refresh rate is not pushed again for a tint.
            Event::Reapply { cap } => match self.arbiter.table().index_of(cap) {
                Some(index) => {
                    self.arbiter.invalidate_one(index);
                    self.evaluate(|_| true)
                }
                None => Vec::new(),
            },

            // The clock moved, by a fired timer or by the host noticing a
            // resume or timezone change. Rescheduled even when the minute did
            // not change, because a timer that fired seconds early would
            // otherwise end the chain of wakeups.
            Event::TimeOfDay {
                minute,
                second,
                millisecond,
            } => {
                let mut out = Vec::new();
                out.extend(match self.world.absorb(event) {
                    None => Vec::new(),
                    Some(changed) => self.evaluate(|r| r.reads.intersects(changed)),
                });
                out.extend(self.next_wakeup(
                    (u32::from(*minute) * 60 + u32::from(*second)) * 1000 + u32::from(*millisecond),
                ));
                out
            }

            Event::TimerFired { id } if *id == PREVIEW_TIMER => self.step_returns(),

            _ => match self.world.absorb(event) {
                None => Vec::new(),
                Some(changed) => self.evaluate(|r| r.reads.intersects(changed)),
            },
        }
    }

    /// Re-evaluate every rule against the current world.
    ///
    /// Used at startup, once the host has pushed an initial snapshot, and after
    /// a display change. Idempotent: a rule already holding re-claims the same
    /// value and the arbiter emits nothing, unless the belief was invalidated
    /// first, which is exactly the point.
    pub fn sync(&mut self) -> Vec<Action> {
        let mut out = self.evaluate(|_| true);
        if let Some(now) = self.world.second_of_day {
            out.extend(self.next_wakeup(now * 1000 + u32::from(self.world.millisecond)));
        }
        out
    }

    /// A value the user set by hand, from the CLI or the dashboard. It
    /// outranks every rule and stays until it is cleared, which is why it is
    /// an owner in the claim stack rather than a rule appended to the list:
    /// the rule underneath keeps holding and takes the capability back the
    /// moment the override goes.
    pub fn manual_set(&mut self, cap: &CapabilityId, value: Value) -> Result<Vec<Action>, String> {
        let index = self.index_of(cap)?;
        Ok(self
            .arbiter
            .claim(index, OwnerId::Manual, crate::priority::MANUAL, value)
            .into_iter()
            .collect())
    }

    /// Drop the override. The capability falls back to whichever rule is still
    /// holding, or to its baseline when none is.
    pub fn manual_clear(&mut self, cap: &CapabilityId) -> Result<Vec<Action>, String> {
        let index = self.index_of(cap)?;
        Ok(self
            .arbiter
            .release(index, OwnerId::Manual)
            .into_iter()
            .collect())
    }

    /// Show a value while a slider is dragged. It sits above the manual
    /// override so the override is still there to fall back to, and a
    /// preview already fading back is cut short by the new drag.
    pub fn preview(&mut self, cap: &CapabilityId, value: Value) -> Result<Vec<Action>, String> {
        let index = self.index_of(cap)?;
        let was_fading = !self.returns.is_empty();
        self.returns.retain(|r| r.cap != index);
        let mut out: Vec<Action> = self
            .arbiter
            .claim(index, OwnerId::Preview, crate::priority::PREVIEW, value)
            .into_iter()
            .collect();
        if was_fading && self.returns.is_empty() {
            out.push(Self::preview_stop());
        }
        Ok(out)
    }

    /// The drag ended. The capability fades back over [`PREVIEW_STEPS`] to
    /// the claim underneath, or to `rest` when nothing holds it, and the
    /// preview claim is released on the last step. Anything that cannot blend
    /// is released at once.
    pub fn preview_end(
        &mut self,
        cap: &CapabilityId,
        rest: Option<Value>,
    ) -> Result<Vec<Action>, String> {
        let index = self.index_of(cap)?;
        self.returns.retain(|r| r.cap != index);
        let Some(from) = self
            .arbiter
            .stack(index)
            .iter()
            .find(|c| c.owner == OwnerId::Preview)
            .map(|c| c.value.clone())
        else {
            return Ok(Vec::new());
        };
        let to = self
            .arbiter
            .beneath(index, OwnerId::Preview)
            .map(|c| c.value.clone())
            .or(rest)
            .filter(|to| transition::fadeable(&from, to) && *to != from);
        let Some(to) = to else {
            return Ok(self
                .arbiter
                .release(index, OwnerId::Preview)
                .into_iter()
                .collect());
        };
        self.returns.push(Return {
            cap: index,
            from,
            to,
            step: 0,
        });
        Ok(vec![Self::preview_tick()])
    }

    fn preview_tick() -> Action {
        Action::SetTimer {
            id: PREVIEW_TIMER,
            after: PREVIEW_STEP,
            tolerance: Duration::from_millis(10),
        }
    }

    /// The last fade back is over. Said explicitly, because a host whose
    /// timers repeat until told otherwise would keep firing the preview timer
    /// every 40 ms for the rest of the session, each fire a clock event.
    fn preview_stop() -> Action {
        Action::CancelTimer { id: PREVIEW_TIMER }
    }

    /// One step of every fade back. Each step is a fresh preview claim, so the
    /// arbiter's belief about the screen stays right and the final release
    /// emits nothing when the value underneath is already there. The step
    /// that finishes the last fade cancels the timer.
    fn step_returns(&mut self) -> Vec<Action> {
        if self.returns.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut returns = std::mem::take(&mut self.returns);
        for r in &mut returns {
            r.step += 1;
            let done = r.step >= PREVIEW_STEPS;
            let value = if done {
                r.to.clone()
            } else {
                let s = f64::from(r.step) / f64::from(PREVIEW_STEPS);
                transition::blend(&r.from, &r.to, s)
            };
            out.extend(self.arbiter.claim(
                r.cap,
                OwnerId::Preview,
                crate::priority::PREVIEW,
                value,
            ));
            if done {
                out.extend(self.arbiter.release(r.cap, OwnerId::Preview));
            }
        }
        returns.retain(|r| r.step < PREVIEW_STEPS);
        self.returns = returns;
        out.push(if self.returns.is_empty() {
            Self::preview_stop()
        } else {
            Self::preview_tick()
        });
        out
    }

    fn index_of(&self, cap: &CapabilityId) -> Result<CapIndex, String> {
        self.arbiter
            .table()
            .index_of(cap)
            .ok_or_else(|| format!("this machine has no {cap}"))
    }

    /// Swap the rule set for a freshly parsed one, keeping the world and the
    /// capability table.
    ///
    /// Every rule claim is released first, so a rule deleted from the file
    /// cannot leave a value applied that nothing owns any more. A manual
    /// override survives, because reloading a file is not the user changing
    /// their mind about a slider.
    pub fn reload(&mut self, rules: Vec<Rule>) -> Vec<Action> {
        let mut out = Vec::new();
        let owners: Vec<u32> = self.rules.iter().map(|r| r.id).collect();
        for id in owners {
            out.extend(self.arbiter.release_all(OwnerId::Rule(id)));
        }

        let table = self.arbiter.table().clone();
        self.rules = rules
            .into_iter()
            .enumerate()
            .map(|(i, r)| Compiled::bind(i as u32, r, &table))
            .collect();
        self.holding = vec![false; self.rules.len()];
        (self.clock_edges, self.ramps) = schedule(&self.rules);

        out.extend(self.sync());
        out
    }

    /// Give every capability back, whoever was holding it. Manual overrides
    /// included, because the daemon is going away and nothing will be left to
    /// undo them.
    ///
    /// The engine stays usable afterwards. What it forgets is who was holding
    /// what, not the world or the rules, so a shutdown that is cancelled
    /// halfway is a `sync` away from correct.
    pub fn shutdown(&mut self) -> Vec<Action> {
        self.holding.fill(false);
        self.arbiter.release_everything()
    }

    /// The next wakeup after `now`, in milliseconds of the day: a step while a fade
    /// runs, the next schedule edge otherwise, or `None` when no active rule
    /// reads the clock.
    fn next_wakeup(&self, now: u32) -> Option<Action> {
        let day = DAY * 1000;
        let now = now % day;
        let edge = self
            .clock_edges
            .iter()
            .map(|&edge| match (edge * 1000 + day - now) % day {
                // An edge equal to now is the one that just fired. The next
                // occurrence is tomorrow.
                0 => day,
                d => d,
            })
            .min();
        let step = self
            .ramps
            .iter()
            .filter_map(|&(start, length)| {
                let into = (now + day - start * 1000) % day;
                let length_ms = length * 1000;
                (into < length_ms).then(|| transition::step(length).min(length_ms - into))
            })
            .min();

        let (after, tolerance) = match (step, edge) {
            // A fade is running. Keep timer slack below one frame.
            (Some(step), edge) => (edge.map_or(step, |e| e.min(step)), 10),
            // A schedule edge is not a frame deadline. Half a minute of slack
            // lets the OS batch the wakeup with anything else pending.
            (None, Some(edge)) => (edge, 30_000),
            (None, None) => return None,
        };
        Some(Action::SetTimer {
            id: SCHEDULE_TIMER,
            after: Duration::from_millis(u64::from(after)),
            tolerance: Duration::from_millis(tolerance),
        })
    }

    fn evaluate(&mut self, considered: impl Fn(&Compiled) -> bool) -> Vec<Action> {
        // Destructured so the arbiter can be borrowed mutably while the rules
        // are read.
        let Self {
            arbiter,
            rules,
            world,
            holding,
            ..
        } = self;

        let mut out = Vec::new();
        for (i, rule) in rules.iter().enumerate() {
            if !matches!(rule.state, RuleState::Active) || !considered(rule) {
                continue;
            }
            let strength = rule.when.strength(world, rule.fade);
            let now = strength > 0.0;
            let before = holding[i];
            holding[i] = now;

            match (before, now) {
                // Claiming covers both the rising edge and a rule that already
                // held: re-claiming an unchanged value emits nothing, and it is
                // what makes an invalidated capability get pushed again. A
                // fading value is re-claimed at every step, and the arbiter
                // turns each one that moved into an apply.
                (_, true) => {
                    for ((cap, value), from) in rule.set.iter().zip(&rule.from) {
                        let value = match from {
                            Some(from) if strength < 1.0 => {
                                transition::blend(from, value, strength)
                            }
                            _ => value.clone(),
                        };
                        out.extend(arbiter.claim(
                            *cap,
                            OwnerId::Rule(rule.id),
                            rule.priority,
                            value,
                        ));
                    }
                }
                (true, false) => out.extend(arbiter.release_all(OwnerId::Rule(rule.id))),
                (false, false) => {}
            }
        }
        out
    }
}

/// The edges and fades of every active rule, sorted and deduplicated.
fn schedule(rules: &[Compiled]) -> (Vec<u32>, Vec<(u32, u32)>) {
    let mut edges = Vec::new();
    let mut ramps = Vec::new();
    for rule in rules.iter().filter(|r| r.is_active()) {
        rule.when.clock_edges(rule.fade, &mut edges);
        rule.when.ramps(rule.fade, &mut ramps);
    }
    edges.sort_unstable();
    edges.dedup();
    ramps.sort_unstable();
    ramps.dedup();
    (edges, ramps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{Assign, Condition};
    use cortisoff_host::{ids, AppRef, CapabilityId, CapabilitySet, Value};

    const VIBRANCE: &str = ids::DISPLAY_VIBRANCE;
    const TEMP: &str = ids::DISPLAY_COLOR_TEMP;

    fn table() -> CapTable {
        let set = CapabilitySet::new()
            .available(CapabilityId::new(VIBRANCE), Some("nvapi"))
            .available(CapabilityId::new(TEMP), Some("magnification"));
        CapTable::from_set(&set)
    }

    fn rule(name: &str, when: Condition, priority: i32, set: &[(&str, i64)]) -> Rule {
        Rule {
            name: name.into(),
            when,
            priority,
            enabled: true,
            set: set
                .iter()
                .map(|(cap, v)| Assign {
                    cap: CapabilityId::new(*cap),
                    value: Value::Int(*v),
                })
                .collect(),
            transition: None,
        }
    }

    fn app(exe: &str) -> Event {
        Event::AppFocus(AppRef {
            exe: exe.into(),
            title: None,
            pid: 1,
            started_at_ms: 0,
        })
    }

    fn titled(exe: &str, title: &str) -> Event {
        Event::AppFocus(AppRef {
            exe: exe.into(),
            title: Some(title.into()),
            pid: 1,
            started_at_ms: 0,
        })
    }

    fn on_app(exe: &str) -> Condition {
        Condition::App {
            exe: Some(exe.into()),
            title_contains: None,
        }
    }

    fn apply(cap: &str, v: i64) -> Action {
        Action::Apply {
            cap: CapabilityId::new(cap),
            value: Value::Int(v),
        }
    }

    fn restore(cap: &str) -> Action {
        Action::RestoreBaseline {
            cap: CapabilityId::new(cap),
        }
    }

    #[test]
    fn inactivity_delays_fire_independently_and_release_on_input() {
        let mut engine = Engine::new(
            table(),
            vec![
                rule("short", Condition::IdleFor(60), 0, &[(TEMP, 3200)]),
                rule("long", Condition::IdleFor(120), 0, &[(VIBRANCE, 80)]),
            ],
        );
        assert!(engine
            .on_event(&Event::Idle {
                idle: false,
                seconds: 59
            })
            .is_empty());
        assert_eq!(
            engine.on_event(&Event::Idle {
                idle: false,
                seconds: 60
            }),
            vec![apply(TEMP, 3200)]
        );
        assert_eq!(
            engine.on_event(&Event::Idle {
                idle: false,
                seconds: 120
            }),
            vec![apply(VIBRANCE, 80)]
        );
        let restored = engine.on_event(&Event::Idle {
            idle: false,
            seconds: 0,
        });
        assert!(restored.contains(&restore(TEMP)) && restored.contains(&restore(VIBRANCE)));
    }

    #[test]
    fn a_rule_claims_when_its_condition_starts_holding() {
        let mut e = Engine::new(
            table(),
            vec![rule("game", on_app("valorant.exe"), 0, &[(VIBRANCE, 100)])],
        );
        assert_eq!(e.on_event(&app("valorant.exe")), vec![apply(VIBRANCE, 100)]);
    }

    /// The reason rules are predicates and not handlers. Nobody wrote an undo
    /// path here, and switching away still restores.
    #[test]
    fn a_rule_releases_when_its_condition_stops_holding() {
        let mut e = Engine::new(
            table(),
            vec![rule("game", on_app("valorant.exe"), 0, &[(VIBRANCE, 100)])],
        );
        e.on_event(&app("valorant.exe"));
        assert_eq!(e.on_event(&app("explorer.exe")), vec![restore(VIBRANCE)]);
    }

    /// `SetWinEventHook` re-announces the foreground window constantly. An
    /// event that changes nothing must reach no rule at all.
    #[test]
    fn an_event_that_changes_nothing_emits_nothing() {
        let mut e = Engine::new(
            table(),
            vec![rule("game", on_app("valorant.exe"), 0, &[(VIBRANCE, 100)])],
        );
        e.on_event(&app("valorant.exe"));
        assert!(e.on_event(&app("valorant.exe")).is_empty());
    }

    #[test]
    fn a_rule_is_skipped_when_the_signal_it_reads_did_not_move() {
        let mut e = Engine::new(
            table(),
            vec![rule(
                "battery",
                Condition::OnBattery(true),
                0,
                &[(VIBRANCE, 20)],
            )],
        );
        assert!(e.on_event(&app("valorant.exe")).is_empty());
        assert_eq!(
            e.on_event(&Event::PowerSource { on_battery: true }),
            vec![apply(VIBRANCE, 20)]
        );
    }

    #[test]
    fn the_higher_priority_rule_wins_and_the_lower_one_takes_over_on_release() {
        let mut e = Engine::new(
            table(),
            vec![
                rule("default", Condition::Always, -100, &[(VIBRANCE, 50)]),
                rule("game", on_app("valorant.exe"), 100, &[(VIBRANCE, 100)]),
            ],
        );
        assert_eq!(e.sync(), vec![apply(VIBRANCE, 50)]);
        assert_eq!(e.on_event(&app("valorant.exe")), vec![apply(VIBRANCE, 100)]);
        assert_eq!(e.on_event(&app("explorer.exe")), vec![apply(VIBRANCE, 50)]);
    }

    #[test]
    fn a_rule_naming_an_absent_capability_loads_unsupported_and_never_claims() {
        let mut e = Engine::new(
            table(),
            vec![rule(
                "nope",
                Condition::Always,
                0,
                &[("display.hologram", 1)],
            )],
        );
        assert_eq!(
            e.rules()[0].state,
            RuleState::Unsupported {
                missing: vec![CapabilityId::new("display.hologram")]
            }
        );
        assert!(e.sync().is_empty());
    }

    #[test]
    fn a_rule_that_sets_nothing_is_reported_rather_than_silently_loaded() {
        let e = Engine::new(table(), vec![rule("empty", Condition::Always, 0, &[])]);
        assert_eq!(e.rules()[0].state, RuleState::Empty);
    }

    /// The dashboard's switch. The rule keeps everything it was configured
    /// with and stops existing as far as the engine is concerned, and the one
    /// underneath takes the capability back rather than the screen falling to
    /// its baseline.
    #[test]
    fn a_disabled_rule_never_claims_and_the_rule_under_it_takes_over() {
        let mut off = rule("game", on_app("valorant.exe"), 100, &[(VIBRANCE, 100)]);
        off.enabled = false;
        let mut e = Engine::new(
            table(),
            vec![
                rule("default", Condition::Always, -100, &[(VIBRANCE, 50)]),
                off,
            ],
        );
        assert_eq!(e.rules()[1].state, RuleState::Disabled);
        assert_eq!(e.sync(), vec![apply(VIBRANCE, 50)]);
        assert!(e.on_event(&app("valorant.exe")).is_empty());
    }

    #[test]
    fn shutdown_releases_every_claim() {
        let mut e = Engine::new(
            table(),
            vec![rule(
                "game",
                on_app("valorant.exe"),
                0,
                &[(VIBRANCE, 100), (TEMP, 6500)],
            )],
        );
        e.on_event(&app("valorant.exe"));
        assert_eq!(
            e.on_event(&Event::Shutdown),
            vec![restore(TEMP), restore(VIBRANCE)]
        );
    }

    /// A monitor hotplug arrives with no tint on the new panel while the
    /// arbiter still believes the value is applied. Without the invalidation
    /// the new monitor stays untinted for as long as the rule holds.
    #[test]
    fn a_display_change_pushes_every_held_claim_again() {
        let mut e = Engine::new(
            table(),
            vec![rule("game", on_app("valorant.exe"), 0, &[(VIBRANCE, 100)])],
        );
        e.on_event(&app("valorant.exe"));
        assert_eq!(
            e.on_event(&Event::DisplayConfigChanged),
            vec![apply(VIBRANCE, 100)]
        );
    }

    /// A game going exclusive fullscreen takes the tint off the screen and
    /// nothing else. Only the tint is pushed again.
    #[test]
    fn a_reapply_pushes_one_capability_and_leaves_the_others_alone() {
        let mut e = Engine::new(
            table(),
            vec![rule(
                "game",
                on_app("valorant.exe"),
                0,
                &[(VIBRANCE, 100), (TEMP, 3400)],
            )],
        );
        e.on_event(&app("valorant.exe"));
        assert_eq!(
            e.on_event(&Event::Reapply {
                cap: CapabilityId::new(TEMP)
            }),
            vec![apply(TEMP, 3400)]
        );
        assert!(e
            .on_event(&Event::Reapply {
                cap: CapabilityId::new("display.hologram")
            })
            .is_empty());
    }

    #[test]
    fn the_executable_matches_on_file_name_and_ignores_case() {
        let mut e = Engine::new(
            table(),
            vec![rule("game", on_app("valorant.exe"), 0, &[(VIBRANCE, 100)])],
        );
        assert_eq!(
            e.on_event(&app("C:\\Riot Games\\VALORANT.exe")),
            vec![apply(VIBRANCE, 100)]
        );
    }

    #[test]
    fn a_title_condition_needs_both_halves_to_hold() {
        let when = Condition::App {
            exe: Some("chrome.exe".into()),
            title_contains: Some("YouTube".into()),
        };
        let mut e = Engine::new(table(), vec![rule("video", when, 0, &[(VIBRANCE, 80)])]);
        assert!(e.on_event(&titled("chrome.exe", "GitHub")).is_empty());
        assert_eq!(
            e.on_event(&titled("chrome.exe", "cat video - youtube")),
            vec![apply(VIBRANCE, 80)]
        );
    }

    #[test]
    fn a_composite_condition_reads_every_signal_underneath_it() {
        let when = Condition::All(vec![
            on_app("vlc.exe"),
            Condition::Not(Box::new(Condition::Idle(true))),
        ]);
        let mut e = Engine::new(table(), vec![rule("film", when, 0, &[(TEMP, 6500)])]);
        assert_eq!(e.on_event(&app("vlc.exe")), vec![apply(TEMP, 6500)]);
        // Idle is the other half of the condition, so it must reach this rule.
        assert_eq!(
            e.on_event(&Event::Idle {
                idle: true,
                seconds: 300
            }),
            vec![restore(TEMP)]
        );
    }

    fn at(hh: u16, mm: u16) -> Event {
        Event::TimeOfDay {
            minute: hh * 60 + mm,
            second: 0,
            millisecond: 0,
        }
    }

    #[test]
    fn a_one_minute_fade_requests_small_steps() {
        let mut rule = night_light(0);
        rule.transition.as_mut().unwrap().seconds = 60;
        let mut engine = Engine::new(table(), vec![rule]);
        let actions = engine.on_event(&at(20, 0));
        assert!(
            actions.iter().any(|action| matches!(action,
                Action::SetTimer { after, tolerance, .. }
                    if *after <= Duration::from_millis(40)
                        && *tolerance <= Duration::from_millis(10)
            )),
            "fade timer: {actions:?}"
        );
    }

    #[test]
    fn a_one_minute_fade_changes_within_each_second() {
        let mut rule = night_light(0);
        rule.transition.as_mut().unwrap().seconds = 60;
        let mut engine = Engine::new(table(), vec![rule]);
        let mut previous = 6600;
        for elapsed in (40..=60_000u32).step_by(40) {
            let actions = engine.on_event(&Event::TimeOfDay {
                minute: 20 * 60 + (elapsed / 60_000) as u16,
                second: ((elapsed / 1000) % 60) as u8,
                millisecond: (elapsed % 1000) as u16,
            });
            for action in actions {
                if let Action::Apply {
                    value: Value::Int(value),
                    ..
                } = action
                {
                    assert!(
                        value <= previous && previous - value <= 4,
                        "temperature jumped from {previous} to {value} at {elapsed} ms"
                    );
                    previous = value;
                }
            }
        }
        assert_eq!(previous, 3200);
        assert_eq!(engine.on_event(&at(21, 0)), vec![wakeup_in(9 * 60 + 59)]);
    }

    #[test]
    fn the_morning_fade_releases_at_its_end() {
        let mut rule = night_light(0);
        rule.transition.as_mut().unwrap().seconds = 60;
        let mut engine = Engine::new(table(), vec![rule]);
        let mut previous = 3200;
        for elapsed in (0..60_000u32).step_by(40) {
            let actions = engine.on_event(&Event::TimeOfDay {
                minute: 6 * 60 + 59,
                second: (elapsed / 1000) as u8,
                millisecond: (elapsed % 1000) as u16,
            });
            for action in actions {
                if let Action::Apply {
                    value: Value::Int(value),
                    ..
                } = action
                {
                    assert!(value >= previous && value - previous <= 4);
                    previous = value;
                }
            }
        }
        assert!(6600 - previous <= 4);
        assert_eq!(
            engine.on_event(&at(7, 0)),
            vec![restore(TEMP), wakeup_in(13 * 60)]
        );
    }

    #[test]
    fn schedule_clock_updates_do_not_advance_preview_returns() {
        let mut engine = Engine::new(table(), vec![]);
        let cap = CapabilityId::new(TEMP);
        engine.manual_set(&cap, Value::Int(4000)).unwrap();
        engine.preview(&cap, Value::Int(2000)).unwrap();
        engine.preview_end(&cap, None).unwrap();
        assert!(engine.on_event(&at(20, 0)).is_empty());
    }

    fn step_in(milliseconds: u64) -> Action {
        Action::SetTimer {
            id: crate::engine::SCHEDULE_TIMER,
            after: Duration::from_millis(milliseconds),
            tolerance: Duration::from_millis(10),
        }
    }

    /// LightBulb's settings as a rule: 6600 K by day, 3200 K by night, forty
    /// minutes to get from one to the other, offset 0.
    fn night_light(offset: u8) -> Rule {
        let mut r = rule("night", between("20:00", "07:00"), 0, &[(TEMP, 3200)]);
        r.transition = Some(crate::Transition {
            seconds: 40 * 60,
            offset,
            from: vec![Assign {
                cap: CapabilityId::new(TEMP),
                value: Value::Int(6600),
            }],
        });
        r
    }

    /// The whole evening and morning of a faded rule. Nothing before the
    /// window, a sine up to full strength stepped every 40 milliseconds, a long
    /// sleep, a cosine back down, and the baseline exactly at the end.
    #[test]
    fn a_transition_fades_in_and_out_and_steps_only_while_it_runs() {
        let mut e = Engine::new(table(), vec![night_light(0)]);
        assert_eq!(e.on_event(&at(19, 0)), vec![wakeup_in(60)]);
        // Halfway through: 6600 - 3400 * sin(pi / 4).
        assert_eq!(
            e.on_event(&at(20, 20)),
            vec![apply(TEMP, 4196), step_in(40)]
        );
        assert_eq!(
            e.on_event(&at(20, 40)),
            vec![apply(TEMP, 3200), wakeup_in(9 * 60 + 40)]
        );
        assert!(e
            .on_event(&at(23, 0))
            .iter()
            .all(|a| matches!(a, Action::SetTimer { .. })));
        assert_eq!(e.on_event(&at(6, 40)), vec![apply(TEMP, 4196), step_in(40)]);
        assert_eq!(
            e.on_event(&at(7, 0)),
            vec![restore(TEMP), wakeup_in(13 * 60)]
        );
    }

    /// At 100 % the fade sits before the window, so the screen is already
    /// warming at 19:40 and fully warm at 20:00.
    #[test]
    fn a_full_offset_moves_the_fade_outside_the_window() {
        let mut e = Engine::new(table(), vec![night_light(100)]);
        assert_eq!(
            e.on_event(&at(19, 40)),
            vec![apply(TEMP, 4196), step_in(40)]
        );
        assert_eq!(e.on_event(&at(20, 0))[0], apply(TEMP, 3200));
    }

    /// A step never jumps past the end of the fade it belongs to.
    #[test]
    fn the_last_step_of_a_fade_lands_on_its_end() {
        let mut e = Engine::new(table(), vec![night_light(0)]);
        let late = Event::TimeOfDay {
            minute: 20 * 60 + 39,
            second: 59,
            millisecond: 980,
        };
        assert_eq!(e.on_event(&late).last(), Some(&step_in(20)));
    }

    fn between(start: &str, end: &str) -> Condition {
        Condition::Between {
            start: start.parse().unwrap(),
            end: end.parse().unwrap(),
        }
    }

    fn wakeup_in(minutes: u64) -> Action {
        Action::SetTimer {
            id: crate::engine::SCHEDULE_TIMER,
            after: Duration::from_secs(minutes * 60),
            tolerance: Duration::from_secs(30),
        }
    }

    /// The blue light filter, as a rule. The claim rises inside the window,
    /// falls at its end, and each clock event also asks for exactly the next
    /// wakeup the schedule needs.
    #[test]
    fn a_schedule_rule_claims_in_its_window_and_asks_for_the_next_edge() {
        let mut e = Engine::new(
            table(),
            vec![rule("night", between("20:00", "07:00"), 0, &[(TEMP, 3400)])],
        );
        // 20:30: inside. Next edge is 07:00, in 10h30.
        assert_eq!(
            e.on_event(&at(20, 30)),
            vec![apply(TEMP, 3400), wakeup_in(10 * 60 + 30)]
        );
        // 23:00: still inside, nothing to re-apply, wakeup still 07:00.
        assert_eq!(e.on_event(&at(23, 0)), vec![wakeup_in(8 * 60)]);
        // 07:00: the window closed. Next edge is 20:00.
        assert_eq!(
            e.on_event(&at(7, 0)),
            vec![restore(TEMP), wakeup_in(13 * 60)]
        );
    }

    /// A timer can fire seconds early and land in the same minute. Not
    /// rescheduling here would end the chain of wakeups for good.
    #[test]
    fn a_clock_event_in_the_same_minute_still_reschedules() {
        let mut e = Engine::new(
            table(),
            vec![rule("night", between("20:00", "07:00"), 0, &[(TEMP, 3400)])],
        );
        e.on_event(&at(23, 0));
        assert_eq!(e.on_event(&at(23, 0)), vec![wakeup_in(8 * 60)]);
    }

    /// Until the host has pushed a clock, a schedule condition never holds:
    /// tinting the screen based on a guessed time is worse than waiting one
    /// event for the real one.
    #[test]
    fn a_schedule_rule_never_holds_on_an_unknown_clock() {
        let mut e = Engine::new(
            table(),
            vec![rule("night", between("20:00", "07:00"), 0, &[(TEMP, 3400)])],
        );
        assert!(e.sync().is_empty());
    }

    #[test]
    fn no_active_schedule_rule_means_no_wakeup_at_all() {
        let mut e = Engine::new(
            table(),
            vec![
                rule("game", on_app("valorant.exe"), 0, &[(VIBRANCE, 100)]),
                // Unsupported: its schedule must not wake anything either.
                rule(
                    "nope",
                    between("20:00", "07:00"),
                    0,
                    &[("display.hologram", 1)],
                ),
            ],
        );
        assert!(e.on_event(&at(12, 0)).is_empty());
    }

    #[test]
    fn firing_lists_the_rules_that_currently_hold() {
        let mut e = Engine::new(
            table(),
            vec![
                rule("always", Condition::Always, 0, &[(TEMP, 3400)]),
                rule("game", on_app("valorant.exe"), 0, &[(VIBRANCE, 100)]),
            ],
        );
        e.sync();
        assert_eq!(
            e.firing().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["always"]
        );
    }
    fn preview_tick() -> Action {
        Action::SetTimer {
            id: PREVIEW_TIMER,
            after: PREVIEW_STEP,
            tolerance: Duration::from_millis(10),
        }
    }

    fn preview_stop() -> Action {
        Action::CancelTimer { id: PREVIEW_TIMER }
    }

    /// Only the preview timer advances a preview return.
    fn tick(e: &mut Engine) -> Vec<Action> {
        e.on_event(&Event::TimerFired { id: PREVIEW_TIMER })
    }

    /// The slider under the cursor outranks the override it will replace.
    #[test]
    fn a_preview_shows_over_a_manual_override() {
        let mut e = Engine::new(table(), vec![]);
        let temp = CapabilityId::new(TEMP);
        e.manual_set(&temp, Value::Int(4000)).unwrap();
        assert_eq!(
            e.preview(&temp, Value::Int(2700)).unwrap(),
            vec![apply(TEMP, 2700)]
        );
        assert_eq!(
            e.arbiter()
                .holder(e.arbiter().table().index_of(&temp).unwrap()),
            Some(OwnerId::Preview)
        );
    }

    /// Releasing the slider is not a snap: twenty claims walk back to the
    /// override, and the final release finds the value already on screen.
    #[test]
    fn ending_a_preview_fades_back_to_what_held_before() {
        let mut e = Engine::new(table(), vec![]);
        let temp = CapabilityId::new(TEMP);
        e.manual_set(&temp, Value::Int(4000)).unwrap();
        e.preview(&temp, Value::Int(2000)).unwrap();
        assert_eq!(e.preview_end(&temp, None).unwrap(), vec![preview_tick()]);

        let mut seen = Vec::new();
        for _ in 0..PREVIEW_STEPS - 1 {
            let out = tick(&mut e);
            assert_eq!(out.len(), 2, "a step applies and asks for the next tick");
            assert_eq!(out[1], preview_tick());
            match &out[0] {
                Action::Apply {
                    value: Value::Int(v),
                    ..
                } => seen.push(*v),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(seen.windows(2).all(|w| w[0] < w[1]), "monotonic: {seen:?}");
        assert!(*seen.last().unwrap() < 4000);

        // The last step lands exactly on the override and stops the timer,
        // which on Windows repeats until told to.
        assert_eq!(tick(&mut e), vec![apply(TEMP, 4000), preview_stop()]);
        assert_eq!(
            e.arbiter()
                .holder(e.arbiter().table().index_of(&temp).unwrap()),
            Some(OwnerId::Manual)
        );
        assert!(tick(&mut e).is_empty());
    }

    /// Nothing underneath: the fade goes to the screen's own rest, then the
    /// baseline is put back, which is the same picture.
    #[test]
    fn ending_a_preview_with_nothing_beneath_fades_to_rest_then_restores() {
        let mut e = Engine::new(table(), vec![]);
        let temp = CapabilityId::new(TEMP);
        e.preview(&temp, Value::Int(3000)).unwrap();
        e.preview_end(&temp, Some(Value::Int(6600))).unwrap();
        let mut last = Vec::new();
        for _ in 0..PREVIEW_STEPS {
            last = tick(&mut e);
        }
        assert_eq!(last, vec![apply(TEMP, 6600), restore(TEMP), preview_stop()]);
        assert_eq!(
            e.arbiter()
                .holder(e.arbiter().table().index_of(&temp).unwrap()),
            None
        );
    }

    /// A display mode has nothing to blend through, so it is released at once.
    #[test]
    fn a_preview_that_cannot_blend_is_released_at_once() {
        let mut e = Engine::new(table(), vec![]);
        let temp = CapabilityId::new(TEMP);
        e.preview(&temp, Value::Text("warm".into())).unwrap();
        assert_eq!(e.preview_end(&temp, None).unwrap(), vec![restore(TEMP)]);
        assert!(tick(&mut e).is_empty());
    }

    /// Grabbing the slider again mid-fade stops the fade where it is.
    #[test]
    fn a_new_drag_cuts_a_fade_back_short() {
        let mut e = Engine::new(table(), vec![]);
        let temp = CapabilityId::new(TEMP);
        e.manual_set(&temp, Value::Int(4000)).unwrap();
        e.preview(&temp, Value::Int(2000)).unwrap();
        e.preview_end(&temp, None).unwrap();
        tick(&mut e);
        assert_eq!(
            e.preview(&temp, Value::Int(2500)).unwrap(),
            vec![apply(TEMP, 2500), preview_stop()]
        );
        assert!(tick(&mut e).is_empty(), "no fade is running any more");
    }

    /// The timer stops with the last fade, not with each one: two sliders
    /// released together share it until the second lands.
    #[test]
    fn the_preview_timer_is_cancelled_once_with_the_last_fade() {
        let mut e = Engine::new(table(), vec![]);
        let temp = CapabilityId::new(TEMP);
        let vib = CapabilityId::new(VIBRANCE);
        e.manual_set(&temp, Value::Int(4000)).unwrap();
        e.manual_set(&vib, Value::Int(50)).unwrap();
        e.preview(&temp, Value::Int(2000)).unwrap();
        e.preview(&vib, Value::Int(100)).unwrap();
        e.preview_end(&temp, None).unwrap();
        e.preview_end(&vib, None).unwrap();

        // A new drag on one of them stops only its own fade.
        assert_eq!(
            e.preview(&vib, Value::Int(80)).unwrap(),
            vec![apply(VIBRANCE, 80)]
        );
        let mut stops = 0;
        for _ in 0..PREVIEW_STEPS + 2 {
            stops += tick(&mut e)
                .iter()
                .filter(|a| **a == preview_stop())
                .count();
        }
        assert_eq!(stops, 1);
    }

    /// Ending a preview that was never started is not an error and does
    /// nothing, which is what a client that lost track needs.
    #[test]
    fn ending_a_preview_that_was_never_started_does_nothing() {
        let mut e = Engine::new(table(), vec![]);
        assert!(e
            .preview_end(&CapabilityId::new(TEMP), None)
            .unwrap()
            .is_empty());
    }
}
