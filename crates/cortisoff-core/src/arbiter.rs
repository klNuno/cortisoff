//! Who owns a capability right now, and what happens when they let go.
//!
//! Last-writer-wins is why VibranceGUI and f.lux fight over the gamma ramp when
//! both are running: neither remembers what the other found there. This module
//! is the fix, and it is also where "quit the game and everything goes back
//! exactly as it was" comes from, for free.
//!
//! Nothing here touches an operating system, so all of it is tested below.

use cortisoff_host::{Action, CapIndex, Value};

use crate::table::CapTable;

/// Who holds a claim. A plugin, a manual override from the CLI or the
/// window, or a slider being dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OwnerId {
    Plugin(u32),
    Manual,
    Preview,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    pub owner: OwnerId,
    pub priority: i32,
    pub value: Value,
    /// Insertion order, so equal priorities resolve to the most recent claim
    /// instead of to whichever happened to be first in memory.
    seq: u64,
}

#[derive(Debug, Default)]
struct Slot {
    claims: Vec<Claim>,
    /// What is currently pushed to the hardware, so an unchanged top emits
    /// nothing. Matters: on a 185 ms DDC/CI write, a redundant apply is
    /// visible.
    applied: Option<Value>,
}

#[derive(Debug)]
pub struct Arbiter {
    table: CapTable,
    slots: Vec<Slot>,
    seq: u64,
}

impl Arbiter {
    pub fn new(table: CapTable) -> Self {
        let slots = (0..table.len()).map(|_| Slot::default()).collect();
        Self {
            table,
            slots,
            seq: 0,
        }
    }

    pub fn table(&self) -> &CapTable {
        &self.table
    }

    /// Who currently drives this capability, for `cortisoff trace`.
    pub fn holder(&self, cap: CapIndex) -> Option<OwnerId> {
        self.top(cap).map(|c| c.owner)
    }

    /// What is pushed to the hardware right now, as far as this arbiter
    /// knows. Where a fade starts from.
    pub fn applied(&self, cap: CapIndex) -> Option<&Value> {
        self.slots[cap.0 as usize].applied.as_ref()
    }

    /// The value `owner` holds on `cap`, if any.
    pub fn held(&self, cap: CapIndex, owner: OwnerId) -> Option<&Value> {
        self.slots[cap.0 as usize]
            .claims
            .iter()
            .find(|c| c.owner == owner)
            .map(|c| &c.value)
    }

    /// Push the winner of every capability again, for after [`Self::invalidate`].
    pub fn resettle(&mut self) -> Vec<Action> {
        (0..self.slots.len())
            .filter_map(|i| self.settle(CapIndex(i as u16)))
            .collect()
    }

    /// The same, for one capability.
    pub fn resettle_one(&mut self, cap: CapIndex) -> Option<Action> {
        self.settle(cap)
    }

    pub fn stack(&self, cap: CapIndex) -> &[Claim] {
        &self.slots[cap.0 as usize].claims
    }

    /// The claim that would win if `owner` let go: what a preview fades back
    /// to, and `None` when the capability would return to its baseline.
    pub fn beneath(&self, cap: CapIndex, owner: OwnerId) -> Option<&Claim> {
        self.slots[cap.0 as usize]
            .claims
            .iter()
            .filter(|c| c.owner != owner)
            .max_by_key(|c| (c.priority, c.seq))
    }

    /// Take or update a claim.
    ///
    /// An owner holds at most one claim per capability: claiming again replaces
    /// its own, it does not stack on itself.
    pub fn claim(
        &mut self,
        cap: CapIndex,
        owner: OwnerId,
        priority: i32,
        value: Value,
    ) -> Option<Action> {
        self.seq += 1;
        let seq = self.seq;
        let slot = &mut self.slots[cap.0 as usize];
        slot.claims.retain(|c| c.owner != owner);
        slot.claims.push(Claim {
            owner,
            priority,
            value,
            seq,
        });
        self.settle(cap)
    }

    /// Drop a claim. The capability falls back to the next holder down, or to
    /// its baseline when nobody is left.
    pub fn release(&mut self, cap: CapIndex, owner: OwnerId) -> Option<Action> {
        let slot = &mut self.slots[cap.0 as usize];
        let before = slot.claims.len();
        slot.claims.retain(|c| c.owner != owner);
        if slot.claims.len() == before {
            return None;
        }
        self.settle(cap)
    }

    /// Drop everything one owner holds, across every capability. Used when a
    /// plugin is switched off or reloaded.
    pub fn release_all(&mut self, owner: OwnerId) -> Vec<Action> {
        (0..self.slots.len())
            .filter_map(|i| self.release(CapIndex(i as u16), owner))
            .collect()
    }

    /// Release every claim held by anyone, in capability order. This is what
    /// runs on a clean shutdown, and it is the reason a normal exit never
    /// leaves the screen tinted.
    pub fn release_everything(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        for i in 0..self.slots.len() {
            let cap = CapIndex(i as u16);
            self.slots[i].claims.clear();
            if let Some(a) = self.settle(cap) {
                out.push(a);
            }
        }
        out
    }

    /// Forget what is believed to be applied, without touching a single claim.
    ///
    /// For when the hardware moved underneath: a monitor hotplug or a mode
    /// change drops the tint the arbiter still thinks is on the screen, and
    /// without this the next settle sees an unchanged winner and emits nothing,
    /// leaving the new monitor untinted forever.
    pub fn invalidate(&mut self) {
        for slot in &mut self.slots {
            slot.applied = None;
        }
    }

    /// The same, for one capability. What a backend switch needs: the tint
    /// went off the screen, the refresh rate did not, and pushing a display
    /// mode again for nothing costs a black frame.
    pub fn invalidate_one(&mut self, cap: CapIndex) {
        self.slots[cap.0 as usize].applied = None;
    }

    fn top(&self, cap: CapIndex) -> Option<&Claim> {
        self.slots[cap.0 as usize]
            .claims
            .iter()
            .max_by_key(|c| (c.priority, c.seq))
    }

    /// Compare the winning claim against what is already applied and emit an
    /// action only when they differ.
    fn settle(&mut self, cap: CapIndex) -> Option<Action> {
        let winner = self.top(cap).map(|c| c.value.clone());
        let slot = &mut self.slots[cap.0 as usize];
        match winner {
            Some(v) => {
                if slot.applied.as_ref() == Some(&v) {
                    return None;
                }
                slot.applied = Some(v.clone());
                Some(Action::Apply {
                    cap: self.table.id(cap).clone(),
                    value: v,
                })
            }
            None => {
                slot.applied.take()?;
                Some(Action::RestoreBaseline {
                    cap: self.table.id(cap).clone(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cortisoff_host::{CapabilityId, CapabilitySet};

    const VIBRANCE: &str = "display.vibrance";
    const TEMP: &str = "display.color_temp";

    fn arbiter() -> (Arbiter, CapIndex, CapIndex) {
        let set = CapabilitySet::new()
            .available(CapabilityId::new(VIBRANCE), Some("nvapi"))
            .available(CapabilityId::new(TEMP), Some("magnification"));
        let table = CapTable::from_set(&set);
        let vib = table.index_of(&CapabilityId::new(VIBRANCE)).unwrap();
        let temp = table.index_of(&CapabilityId::new(TEMP)).unwrap();
        (Arbiter::new(table), vib, temp)
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
    fn first_claim_applies() {
        let (mut a, vib, _) = arbiter();
        assert_eq!(
            a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(80)),
            Some(apply(VIBRANCE, 80))
        );
    }

    #[test]
    fn reclaiming_the_same_value_emits_nothing() {
        let (mut a, vib, _) = arbiter();
        a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(80));
        assert_eq!(a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(80)), None);
    }

    #[test]
    fn an_owner_holds_one_claim_and_updates_it_in_place() {
        let (mut a, vib, _) = arbiter();
        a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(80));
        assert_eq!(
            a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(50)),
            Some(apply(VIBRANCE, 50))
        );
        assert_eq!(a.stack(vib).len(), 1);
    }

    #[test]
    fn higher_priority_preempts() {
        let (mut a, vib, _) = arbiter();
        a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(80));
        assert_eq!(
            a.claim(vib, OwnerId::Manual, 100, Value::Int(20)),
            Some(apply(VIBRANCE, 20))
        );
    }

    #[test]
    fn a_lower_priority_claim_under_a_held_capability_changes_nothing() {
        let (mut a, vib, _) = arbiter();
        a.claim(vib, OwnerId::Manual, 100, Value::Int(20));
        assert_eq!(a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(80)), None);
    }

    /// The reason the stack exists. Releasing the winner must fall back to the
    /// claim underneath, never straight to the baseline.
    #[test]
    fn releasing_the_top_falls_back_to_the_claim_below() {
        let (mut a, vib, _) = arbiter();
        a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(80));
        a.claim(vib, OwnerId::Manual, 100, Value::Int(20));
        assert_eq!(a.release(vib, OwnerId::Manual), Some(apply(VIBRANCE, 80)));
    }

    #[test]
    fn releasing_the_last_claim_restores_the_baseline() {
        let (mut a, vib, _) = arbiter();
        a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(80));
        assert_eq!(a.release(vib, OwnerId::Plugin(1)), Some(restore(VIBRANCE)));
    }

    #[test]
    fn releasing_a_claim_that_is_not_on_top_emits_nothing() {
        let (mut a, vib, _) = arbiter();
        a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(80));
        a.claim(vib, OwnerId::Manual, 100, Value::Int(20));
        assert_eq!(a.release(vib, OwnerId::Plugin(1)), None);
    }

    #[test]
    fn releasing_an_owner_that_holds_nothing_is_a_no_op() {
        let (mut a, vib, _) = arbiter();
        assert_eq!(a.release(vib, OwnerId::Plugin(7)), None);
    }

    #[test]
    fn equal_priority_resolves_to_the_most_recent_claim() {
        let (mut a, vib, _) = arbiter();
        a.claim(vib, OwnerId::Plugin(1), 5, Value::Int(80));
        assert_eq!(
            a.claim(vib, OwnerId::Plugin(2), 5, Value::Int(30)),
            Some(apply(VIBRANCE, 30))
        );
        assert_eq!(a.holder(vib), Some(OwnerId::Plugin(2)));
    }

    #[test]
    fn releasing_the_recent_of_two_equal_priorities_falls_back_to_the_older() {
        let (mut a, vib, _) = arbiter();
        a.claim(vib, OwnerId::Plugin(1), 5, Value::Int(80));
        a.claim(vib, OwnerId::Plugin(2), 5, Value::Int(30));
        assert_eq!(
            a.release(vib, OwnerId::Plugin(2)),
            Some(apply(VIBRANCE, 80))
        );
    }

    /// Batch order is the capability table's order, which is alphabetical
    /// because the set is a BTreeMap. Deterministic on purpose: a host
    /// replaying a batch must never see two different orders for one state.
    #[test]
    fn release_all_drops_one_owner_across_every_capability() {
        let (mut a, vib, temp) = arbiter();
        a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(80));
        a.claim(temp, OwnerId::Plugin(1), 0, Value::Int(3400));
        let actions = a.release_all(OwnerId::Plugin(1));
        assert_eq!(actions, vec![restore(TEMP), restore(VIBRANCE)]);
    }

    #[test]
    fn release_all_leaves_other_owners_alone() {
        let (mut a, vib, temp) = arbiter();
        a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(80));
        a.claim(temp, OwnerId::Manual, 0, Value::Int(3400));
        assert_eq!(a.release_all(OwnerId::Plugin(1)), vec![restore(VIBRANCE)]);
        assert_eq!(a.holder(temp), Some(OwnerId::Manual));
    }

    /// A clean shutdown must never leave the screen tinted or the refresh rate
    /// moved, whatever was stacked at the time.
    #[test]
    fn shutdown_restores_everything_once_per_capability() {
        let (mut a, vib, temp) = arbiter();
        a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(80));
        a.claim(vib, OwnerId::Manual, 100, Value::Int(20));
        a.claim(temp, OwnerId::Plugin(2), 0, Value::Int(3400));
        assert_eq!(
            a.release_everything(),
            vec![restore(TEMP), restore(VIBRANCE)]
        );
        assert!(a.holder(vib).is_none());
    }

    #[test]
    fn shutdown_on_an_untouched_arbiter_emits_nothing() {
        let (mut a, _, _) = arbiter();
        assert!(a.release_everything().is_empty());
    }

    /// The whole point of tracking what is applied: a claim that resolves to
    /// the value already on the hardware must not cost a 185 ms DDC/CI write.
    #[test]
    fn a_preemption_that_lands_on_the_same_value_emits_nothing() {
        let (mut a, vib, _) = arbiter();
        a.claim(vib, OwnerId::Plugin(1), 0, Value::Int(80));
        assert_eq!(a.claim(vib, OwnerId::Manual, 100, Value::Int(80)), None);
        assert_eq!(a.holder(vib), Some(OwnerId::Manual));
    }
}
