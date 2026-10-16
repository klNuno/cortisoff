//! The claim stack plus time: a value that fades in, a release that fades
//! back, and a preview that eases off when the slider is let go.
//!
//! The arbiter decides who wins a capability. This decides how the screen
//! gets from one winner to the next, so a plugin asks for "3200 K over half
//! an hour" once and never steps the fade itself.

use std::time::Duration;

use cortisoff_host::{ids, Action, CapIndex, CapabilityId, Event, TimerId, Value};

use crate::arbiter::{Arbiter, OwnerId};
use crate::blend::{blend_on, ease, fadeable, peak_rate, Curve, EASE_PEAK};
use crate::table::CapTable;
use crate::world::{Signals, World};

/// The one timer fades run on. Arming it again replaces the pending one.
pub const FADE_TIMER: TimerId = TimerId(1);

/// How long a preview takes to ease back once the slider is let go: long
/// enough to read as a fade, short enough that the slider feels released.
pub const PREVIEW_FADE_MS: u64 = 800;

/// The shortest step between two frames of a fade, about 60 Hz. At 25 Hz a
/// one second release crossed 3000 K in 25 visible jumps. 15 and not 16: a
/// Windows timer fires on the system tick, 15.6 ms unless an app asked for
/// finer, and 16 ms waited for the second tick, which made it 32 Hz.
const FRAME_MS: u64 = 15;

/// The longest. A fade of 100 K over an hour changes the screen every 36 s,
/// and waking every 36 s for it is the point: no frame timer runs for a
/// value that has not moved.
const MAX_STEP_MS: u64 = 5_000;

#[derive(Debug, Clone, PartialEq)]
struct Fade {
    cap: CapIndex,
    owner: OwnerId,
    priority: i32,
    from: Value,
    to: Value,
    curve: Curve,
    start: u64,
    length: u64,
    /// The owner lets go at the end, and `to` is what lies underneath.
    release: bool,
    /// Where the capability rests with nothing holding it, for a release
    /// whose target moves while it fades.
    rest: Option<Value>,
}

#[derive(Debug)]
pub struct Claims {
    arbiter: Arbiter,
    world: World,
    fades: Vec<Fade>,
    armed: bool,
}

impl Claims {
    pub fn new(table: CapTable) -> Self {
        Self {
            arbiter: Arbiter::new(table),
            world: World::default(),
            fades: Vec::new(),
            armed: false,
        }
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn arbiter(&self) -> &Arbiter {
        &self.arbiter
    }

    pub fn index_of(&self, cap: &CapabilityId) -> Result<CapIndex, String> {
        self.arbiter
            .table()
            .index_of(cap)
            .ok_or_else(|| format!("this machine has no {cap}"))
    }

    /// The same, for a claim: one output of a capability gets its own stack
    /// the first time it is asked for.
    pub fn slot_of(&mut self, cap: &CapabilityId) -> Result<CapIndex, String> {
        if let Ok(i) = self.index_of(cap) {
            return Ok(i);
        }
        match cap.output() {
            Some((whole, _)) if self.index_of(&whole).is_ok() => {
                Ok(self.arbiter.intern(cap.clone()))
            }
            _ => Err(format!("this machine has no {cap}")),
        }
    }

    /// Whether `owner` is still on its way to the value it claimed.
    pub fn fading(&self, cap: CapIndex, owner: OwnerId) -> bool {
        self.fades.iter().any(|f| f.cap == cap && f.owner == owner)
    }

    /// Whether any fade is under way.
    pub fn moving(&self) -> bool {
        !self.fades.is_empty()
    }

    /// The fade `owner` started at `asked` runs from `now` instead. A claim's
    /// first frame is applied before its fade moves, and a capability that
    /// captures its baseline then can take a while: a colour temperature
    /// reads every panel it drives over DDC/CI, half a second. Timed from the
    /// request, the next frame would jump by that much of the fade.
    pub fn begin_at(&mut self, cap: CapIndex, owner: OwnerId, asked: u64, now: u64) {
        for f in &mut self.fades {
            if f.cap == cap && f.owner == owner && !f.release && f.start == asked {
                f.start = now;
            }
        }
    }

    /// Fold an event from the platform in. Returns which part of the world
    /// moved, for routing to plugins, and anything the hardware needs again.
    pub fn on_event(&mut self, event: &Event) -> (Option<Signals>, Vec<Action>) {
        match event {
            Event::Shutdown => (None, self.shutdown()),
            // Monitors moved. Anything believed to be applied may have landed
            // on hardware that is now gone, so every winner is pushed again.
            Event::DisplayConfigChanged => {
                self.arbiter.invalidate();
                (None, self.arbiter.resettle())
            }
            // Only one capability lost its value. A refresh rate is not
            // pushed again for a tint. Its outputs held apart go with it: the
            // actuator puts them back as part of the whole.
            Event::Reapply { cap } => match self.arbiter.table().index_of(cap) {
                Some(index) => {
                    let family: Vec<CapIndex> = std::iter::once(index)
                        .chain(self.arbiter.table().children(index))
                        .collect();
                    let mut out = Vec::new();
                    for i in family {
                        self.arbiter.invalidate_one(i);
                        out.extend(self.arbiter.resettle_one(i));
                    }
                    (None, out)
                }
                None => (None, Vec::new()),
            },
            _ => (self.world.absorb(event), Vec::new()),
        }
    }

    /// Take or move a claim. With `fade_ms` above zero the value travels
    /// there from where it is now: the owner's own value when it already
    /// holds one, otherwise what the screen shows, otherwise `rest`. Values
    /// that cannot blend switch at once.
    #[allow(clippy::too_many_arguments)]
    pub fn claim(
        &mut self,
        cap: CapIndex,
        owner: OwnerId,
        priority: i32,
        value: Value,
        fade_ms: u64,
        now: u64,
        rest: Option<&Value>,
    ) -> Vec<Action> {
        // Asked again for where it is already heading: a plugin that claims
        // its target on every tick must not restart its own fade each time.
        if self.fades.iter().any(|f| {
            f.cap == cap
                && f.owner == owner
                && !f.release
                && f.to == value
                && f.priority == priority
        }) {
            return Vec::new();
        }
        let from = self
            .arbiter
            .held(cap, owner)
            .or_else(|| self.arbiter.applied(cap))
            .or_else(|| self.shown_whole(cap))
            .or(rest)
            .cloned();
        self.fades.retain(|f| !(f.cap == cap && f.owner == owner));
        let mut out = Vec::new();
        match from.filter(|from| fade_ms > 0 && fadeable(from, &value) && *from != value) {
            Some(from) => {
                out.extend(self.arbiter.claim(cap, owner, priority, from.clone()));
                self.fades.push(Fade {
                    cap,
                    owner,
                    priority,
                    from,
                    to: value,
                    curve: self.curve(cap),
                    start: now,
                    length: fade_ms,
                    release: false,
                    rest: rest.cloned(),
                });
            }
            None => out.extend(self.arbiter.claim(cap, owner, priority, value)),
        }
        out.extend(self.timer());
        self.follow(out)
    }

    /// Let go. With `fade_ms` above zero and the owner on top, the value
    /// travels to whatever lies underneath, or to `rest`, before the claim
    /// goes. An owner hidden under another claim lets go at once, since
    /// nothing on the screen would show the fade.
    pub fn release(
        &mut self,
        cap: CapIndex,
        owner: OwnerId,
        fade_ms: u64,
        now: u64,
        rest: Option<&Value>,
    ) -> Vec<Action> {
        // Asked again to let go while already on the way out: a plugin that
        // releases on every clock event must not restart its own fade each
        // time, or it never leaves its claimed value. A way out that ends
        // sooner (a fullscreen game during the morning fade) still takes over.
        if fade_ms > 0
            && self.fades.iter().any(|f| {
                f.cap == cap && f.owner == owner && f.release && f.start + f.length <= now + fade_ms
            })
        {
            return Vec::new();
        }
        self.fades.retain(|f| !(f.cap == cap && f.owner == owner));
        let mut out = Vec::new();
        let Some(held) = self.arbiter.held(cap, owner).cloned() else {
            out.extend(self.timer());
            return out;
        };
        let priority = self
            .arbiter
            .stack(cap)
            .iter()
            .find(|c| c.owner == owner)
            .map_or(0, |c| c.priority);
        let on_top = self.arbiter.holder(cap) == Some(owner);
        let to = self.beneath(cap, owner, rest);
        match to.filter(|to| fade_ms > 0 && on_top && fadeable(&held, to) && *to != held) {
            Some(to) => self.fades.push(Fade {
                cap,
                owner,
                priority,
                from: held,
                to,
                curve: self.curve(cap),
                start: now,
                length: fade_ms,
                release: true,
                rest: rest.cloned(),
            }),
            None => out.extend(self.arbiter.release(cap, owner)),
        }
        out.extend(self.timer());
        self.follow(out)
    }

    /// Everything one owner holds, at once. A plugin switched off or
    /// reloaded.
    pub fn release_owner(&mut self, owner: OwnerId) -> Vec<Action> {
        self.fades.retain(|f| f.owner != owner);
        let mut out = self.arbiter.release_all(owner);
        out.extend(self.timer());
        self.follow(out)
    }

    /// One frame of every fade, at `now`. Called when [`FADE_TIMER`] fires.
    pub fn tick(&mut self, now: u64) -> Vec<Action> {
        self.armed = false;
        let mut out = Vec::new();
        let mut fades = std::mem::take(&mut self.fades);
        for f in &mut fades {
            if f.release {
                if let Some(to) = self.beneath(f.cap, f.owner, f.rest.as_ref()) {
                    if fadeable(&f.from, &to) {
                        f.to = to;
                    }
                }
            }
            let s = progress(f, now);
            if s >= 1.0 {
                if f.release {
                    out.extend(self.arbiter.release(f.cap, f.owner));
                } else {
                    out.extend(self.arbiter.claim(f.cap, f.owner, f.priority, f.to.clone()));
                }
            } else {
                let value = blend_on(f.curve, &f.from, &f.to, ease(s));
                out.extend(self.arbiter.claim(f.cap, f.owner, f.priority, value));
            }
        }
        fades.retain(|f| progress(f, now) < 1.0);
        // A claim taken during the loop above went into `self.fades`.
        fades.append(&mut self.fades);
        self.fades = fades;
        out.extend(self.timer());
        self.follow(out)
    }

    /// A whole capability given back to its baseline lets go of its outputs
    /// too, so an output still held apart is pushed again after it: the game
    /// keeps its monitor neutral when the night ends under it.
    fn follow(&mut self, mut out: Vec<Action>) -> Vec<Action> {
        let mut again = Vec::new();
        for a in &out {
            let Action::RestoreBaseline { cap } = a else {
                continue;
            };
            let Some(whole) = self.arbiter.table().index_of(cap) else {
                continue;
            };
            again.extend(self.arbiter.table().children(whole));
        }
        for i in again {
            if self.arbiter.holder(i).is_some() {
                self.arbiter.invalidate_one(i);
                out.extend(self.arbiter.resettle_one(i));
            }
        }
        out
    }

    /// Give every capability back, whoever holds it.
    pub fn shutdown(&mut self) -> Vec<Action> {
        self.fades.clear();
        let mut out = self.arbiter.release_everything();
        out.extend(self.timer());
        out
    }

    /// A colour temperature fades in mireds, everything else in a line.
    fn curve(&self, cap: CapIndex) -> Curve {
        if self.arbiter.table().id(cap).parent().as_str() == ids::DISPLAY_COLOR_TEMP {
            Curve::Mired
        } else {
            Curve::Linear
        }
    }

    /// What lies under `owner`: the next claim down, else, for one output of
    /// a capability, what the whole capability shows right now, frame by
    /// frame while it fades, else `rest`.
    fn beneath(&self, cap: CapIndex, owner: OwnerId, rest: Option<&Value>) -> Option<Value> {
        self.arbiter
            .beneath(cap, owner)
            .map(|c| &c.value)
            .or_else(|| self.shown_whole(cap))
            .or(rest)
            .cloned()
    }

    /// For one output of a capability, the value the whole capability has on
    /// the screen: where that output comes from and goes back to.
    fn shown_whole(&self, cap: CapIndex) -> Option<&Value> {
        self.arbiter
            .table()
            .parent(cap)
            .and_then(|whole| self.arbiter.applied(whole))
    }

    /// The next frame, or the end of frames. Only as often as some fade
    /// actually changes the value at its steepest: an integer fade of 3400 K
    /// over thirty minutes needs a frame every few hundred milliseconds, not
    /// sixty a second.
    fn timer(&mut self) -> Option<Action> {
        if self.fades.is_empty() {
            if self.armed {
                self.armed = false;
                return Some(Action::CancelTimer { id: FADE_TIMER });
            }
            return None;
        }
        if self.armed {
            return None;
        }
        self.armed = true;
        let step = self
            .fades
            .iter()
            .map(|f| match (&f.from, &f.to) {
                (Value::Int(a), Value::Int(b)) if a != b => {
                    let peak = peak_rate(f.curve, *a as f64, *b as f64) * EASE_PEAK;
                    (f.length as f64 / (a.abs_diff(*b) as f64 * peak)) as u64
                }
                _ => FRAME_MS,
            })
            .min()
            .unwrap_or(FRAME_MS)
            .clamp(FRAME_MS, MAX_STEP_MS);
        Some(Action::SetTimer {
            id: FADE_TIMER,
            after: Duration::from_millis(step),
            // No slack at frame rate: batched with other timers, a frame
            // waits for a later tick and the fade drops to half its rate.
            tolerance: Duration::from_millis(match step {
                FRAME_MS => 0,
                ..=100 => 10,
                _ => step / 4,
            }),
        })
    }
}

fn progress(f: &Fade, now: u64) -> f64 {
    if f.length == 0 {
        return 1.0;
    }
    (now.saturating_sub(f.start) as f64 / f.length as f64).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::priority;
    use cortisoff_host::CapabilitySet;

    const TEMP: &str = "display.color_temp";

    fn claims() -> (Claims, CapIndex) {
        let set = CapabilitySet::new().available(CapabilityId::new(TEMP), None);
        let c = Claims::new(CapTable::from_set(&set));
        let i = c.index_of(&CapabilityId::new(TEMP)).unwrap();
        (c, i)
    }

    fn applied(actions: &[Action]) -> Vec<Value> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Apply { value, .. } => Some(value.clone()),
                _ => None,
            })
            .collect()
    }

    const P: OwnerId = OwnerId::Plugin(1);

    #[test]
    fn a_claim_with_a_fade_starts_from_rest_and_lands_on_its_value() {
        let (mut c, i) = claims();
        let rest = Value::Int(6600);
        let out = c.claim(i, P, 0, Value::Int(3200), 1000, 0, Some(&rest));
        assert_eq!(applied(&out), vec![Value::Int(6600)]);
        assert!(out
            .iter()
            .any(|a| matches!(a, Action::SetTimer { id, .. } if *id == FADE_TIMER)));
        // Half way in mireds, not in kelvin.
        assert_eq!(applied(&c.tick(500)), vec![Value::Int(4310)]);
        let out = c.tick(1000);
        assert_eq!(applied(&out), vec![Value::Int(3200)]);
        // The timer that fired was the last one: nothing is armed again.
        assert!(!out.iter().any(|a| matches!(a, Action::SetTimer { .. })));
        assert!(!c.fading(i, P));
    }

    #[test]
    fn a_new_claim_mid_fade_starts_where_the_screen_is() {
        let (mut c, i) = claims();
        let rest = Value::Int(6600);
        c.claim(i, P, 0, Value::Int(3200), 1000, 0, Some(&rest));
        c.tick(500);
        let out = c.claim(i, P, 0, Value::Int(6000), 1000, 500, Some(&rest));
        assert!(applied(&out).is_empty(), "{out:?}");
        assert_eq!(applied(&c.tick(1000)), vec![Value::Int(5016)]);
    }

    #[test]
    fn a_release_fades_back_to_rest_then_restores() {
        let (mut c, i) = claims();
        let rest = Value::Int(6600);
        c.claim(i, P, 0, Value::Int(3200), 0, 0, Some(&rest));
        let out = c.release(i, P, 1000, 0, Some(&rest));
        assert!(applied(&out).is_empty());
        assert_eq!(applied(&c.tick(500)), vec![Value::Int(4310)]);
        let out = c.tick(1000);
        assert!(out.contains(&Action::RestoreBaseline {
            cap: CapabilityId::new(TEMP)
        }));
        assert!(c.arbiter().holder(i).is_none());
    }

    #[test]
    fn releasing_again_on_the_way_out_keeps_the_fade_going() {
        let (mut c, i) = claims();
        let rest = Value::Int(6600);
        c.claim(i, P, 0, Value::Int(3200), 0, 0, Some(&rest));
        c.release(i, P, 1000, 0, Some(&rest));
        // Every frame, as a plugin watching the clock does.
        assert!(c.release(i, P, 1000, 250, Some(&rest)).is_empty());
        assert_eq!(applied(&c.tick(500)), vec![Value::Int(4310)]);
        assert!(c.release(i, P, 1000, 500, Some(&rest)).is_empty());
        let out = c.tick(1000);
        assert!(out.contains(&Action::RestoreBaseline {
            cap: CapabilityId::new(TEMP)
        }));
    }

    #[test]
    fn a_quicker_way_out_takes_over_a_slow_release() {
        let (mut c, i) = claims();
        let rest = Value::Int(6600);
        c.claim(i, P, 0, Value::Int(3200), 0, 0, Some(&rest));
        c.release(i, P, 300_000, 0, Some(&rest));
        c.tick(1000);
        c.release(i, P, 1000, 1000, Some(&rest));
        let out = c.tick(2000);
        assert!(out.contains(&Action::RestoreBaseline {
            cap: CapabilityId::new(TEMP)
        }));
        // And a release with no fade at all is immediate.
        c.claim(i, P, 0, Value::Int(3200), 0, 3000, Some(&rest));
        c.release(i, P, 300_000, 3000, Some(&rest));
        let out = c.release(i, P, 0, 3100, Some(&rest));
        assert!(out.contains(&Action::RestoreBaseline {
            cap: CapabilityId::new(TEMP)
        }));
    }

    #[test]
    fn a_preview_eases_back_to_the_claim_underneath() {
        let (mut c, i) = claims();
        c.claim(i, P, 0, Value::Int(4000), 0, 0, None);
        c.claim(
            i,
            OwnerId::Preview,
            priority::PREVIEW,
            Value::Int(2000),
            0,
            0,
            None,
        );
        c.release(i, OwnerId::Preview, PREVIEW_FADE_MS, 0, None);
        assert_eq!(applied(&c.tick(400)), vec![Value::Int(2667)]);
        assert_eq!(applied(&c.tick(800)), vec![Value::Int(4000)]);
        assert_eq!(c.arbiter().holder(i), Some(P));
    }

    #[test]
    fn a_hidden_owner_lets_go_at_once() {
        let (mut c, i) = claims();
        c.claim(i, P, 0, Value::Int(4000), 0, 0, None);
        c.claim(
            i,
            OwnerId::Manual,
            priority::MANUAL,
            Value::Int(5000),
            0,
            0,
            None,
        );
        let out = c.release(i, P, 1000, 0, None);
        assert!(out.is_empty(), "{out:?}");
        assert!(c.arbiter().held(i, P).is_none());
    }

    #[test]
    fn a_long_integer_fade_wakes_only_when_the_value_moves() {
        let (mut c, i) = claims();
        let rest = Value::Int(6600);
        let out = c.claim(i, P, 0, Value::Int(3200), 30 * 60_000, 0, Some(&rest));
        let after = out.iter().find_map(|a| match a {
            Action::SetTimer { after, .. } => Some(*after),
            _ => None,
        });
        // 3400 K, twice as fast at the warm end and 1.5 times at the middle
        // of the easing: a frame every 171 ms.
        assert_eq!(after, Some(Duration::from_millis(171)));
    }

    #[test]
    fn a_short_fade_runs_at_frame_rate_with_no_slack() {
        let (mut c, i) = claims();
        let rest = Value::Int(6600);
        let out = c.claim(i, P, 0, Value::Int(3200), 1000, 0, Some(&rest));
        assert!(out.iter().any(|a| matches!(
            a,
            Action::SetTimer { after, tolerance, .. }
                if *after == Duration::from_millis(FRAME_MS) && tolerance.is_zero()
        )));
    }

    #[test]
    fn a_display_change_pushes_every_winner_again() {
        let (mut c, i) = claims();
        c.claim(i, P, 0, Value::Int(4000), 0, 0, None);
        let (_, out) = c.on_event(&Event::DisplayConfigChanged);
        assert_eq!(applied(&out), vec![Value::Int(4000)]);
    }

    #[test]
    fn shutdown_gives_everything_back_and_stops_the_frames() {
        let (mut c, i) = claims();
        c.claim(i, P, 0, Value::Int(3200), 1000, 0, Some(&Value::Int(6600)));
        let out = c.shutdown();
        assert!(out.contains(&Action::RestoreBaseline {
            cap: CapabilityId::new(TEMP)
        }));
        assert!(out.contains(&Action::CancelTimer { id: FADE_TIMER }));
    }

    const NIGHT: OwnerId = OwnerId::Plugin(2);
    const SPARE: OwnerId = OwnerId::Plugin(3);

    fn one(c: &mut Claims, output: &str) -> CapIndex {
        c.slot_of(&CapabilityId::new(TEMP).on_output(output))
            .unwrap()
    }

    fn kelvin(v: &[Value]) -> i64 {
        match v {
            [Value::Int(k)] => *k,
            _ => panic!("one frame expected, got {v:?}"),
        }
    }

    fn applied_on(actions: &[Action], cap: &str) -> Vec<Value> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Apply { cap: c, value } if c.as_str() == cap => Some(value.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_output_is_its_own_stack_under_the_same_capability() {
        let (mut c, whole) = claims();
        let a = one(&mut c, "A");
        assert_ne!(a, whole);
        assert_eq!(one(&mut c, "A"), a, "asked twice, interned once");
        assert_eq!(c.arbiter().table().parent(a), Some(whole));
        assert!(c.slot_of(&CapabilityId::new("display.mode@A")).is_err());
        assert!(c.slot_of(&CapabilityId::new("display.nothing")).is_err());
    }

    #[test]
    fn an_output_fades_out_of_the_night_and_back_into_it() {
        let (mut c, whole) = claims();
        let rest = Value::Int(6600);
        c.claim(whole, NIGHT, 0, Value::Int(3000), 0, 0, Some(&rest));
        let a = one(&mut c, "A");
        // A game fills A: that output alone leaves the night, from 3000 K and
        // in mireds like the whole one.
        let out = c.claim(a, SPARE, 0, Value::Int(6600), 800, 0, Some(&rest));
        let id = format!("{TEMP}@A");
        assert_eq!(applied_on(&out, &id), vec![Value::Int(3000)]);
        assert!(kelvin(&applied_on(&c.tick(400), &id)) < 4800);
        assert_eq!(applied_on(&c.tick(800), &id), vec![Value::Int(6600)]);
        // It goes back to the night, not to the 6600 K rest.
        c.release(a, SPARE, 800, 1000, Some(&rest));
        let mid = kelvin(&applied_on(&c.tick(1400), &id));
        assert!(mid < 4800 && mid > 3000, "{mid}");
        let out = c.tick(1800);
        assert!(out.contains(&Action::RestoreBaseline {
            cap: CapabilityId::new(id.clone())
        }));
        assert!(c.arbiter().holder(a).is_none());
        assert!(
            applied_on(&out, TEMP).is_empty(),
            "the other outputs never moved"
        );
    }

    #[test]
    fn an_output_already_matching_the_night_lets_go_and_rides_its_fade() {
        let (mut c, whole) = claims();
        let rest = Value::Int(6600);
        c.claim(whole, NIGHT, 0, Value::Int(6600), 0, 0, Some(&rest));
        let a = one(&mut c, "A");
        c.claim(a, SPARE, 0, Value::Int(6600), 0, 0, Some(&rest));
        c.claim(whole, NIGHT, 0, Value::Int(3000), 1000, 0, Some(&rest));
        // The output shows what the whole one shows, so it lets go at once
        // and rides the whole one's fade from there, with no jump.
        let out = c.release(a, SPARE, 1000, 0, Some(&rest));
        assert!(out.contains(&Action::RestoreBaseline {
            cap: CapabilityId::new(format!("{TEMP}@A"))
        }));
        assert_eq!(applied_on(&c.tick(1000), TEMP), vec![Value::Int(3000)]);
    }

    #[test]
    fn the_night_ending_under_a_held_output_pushes_that_output_again() {
        let (mut c, whole) = claims();
        let rest = Value::Int(6600);
        c.claim(whole, NIGHT, 0, Value::Int(3000), 0, 0, Some(&rest));
        let a = one(&mut c, "A");
        c.claim(a, SPARE, 0, Value::Int(6600), 0, 0, Some(&rest));
        let out = c.release(whole, NIGHT, 0, 0, Some(&rest));
        let id = CapabilityId::new(format!("{TEMP}@A"));
        let restore = out
            .iter()
            .position(|a| {
                *a == Action::RestoreBaseline {
                    cap: CapabilityId::new(TEMP),
                }
            })
            .unwrap();
        let again = out
            .iter()
            .position(|a| {
                *a == Action::Apply {
                    cap: id.clone(),
                    value: Value::Int(6600),
                }
            })
            .unwrap();
        assert!(restore < again, "{out:?}");
    }

    #[test]
    fn a_reapply_of_the_whole_pushes_its_outputs_too() {
        let (mut c, whole) = claims();
        let rest = Value::Int(6600);
        c.claim(whole, NIGHT, 0, Value::Int(3000), 0, 0, Some(&rest));
        let a = one(&mut c, "A");
        c.claim(a, SPARE, 0, Value::Int(6600), 0, 0, Some(&rest));
        let (_, out) = c.on_event(&Event::Reapply {
            cap: CapabilityId::new(TEMP),
        });
        assert_eq!(applied_on(&out, TEMP), vec![Value::Int(3000)]);
        assert_eq!(
            applied_on(&out, &format!("{TEMP}@A")),
            vec![Value::Int(6600)]
        );
    }
}
