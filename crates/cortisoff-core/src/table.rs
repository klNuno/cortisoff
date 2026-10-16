use std::collections::BTreeMap;

use cortisoff_host::{CapIndex, CapabilityId, CapabilitySet};

/// The capability table: every probed capability at load, then each one held
/// on a single output the first time a plugin asks for it. It only grows, so
/// an index handed out stays valid.
///
/// Strings are interned here so the engine's hot path indexes a vector instead
/// of hashing a string on every event.
#[derive(Debug, Clone, Default)]
pub struct CapTable {
    ids: Vec<CapabilityId>,
    /// For a capability held on one output, the whole capability.
    parents: Vec<Option<CapIndex>>,
    index: BTreeMap<CapabilityId, CapIndex>,
}

impl CapTable {
    pub fn from_set(set: &CapabilitySet) -> Self {
        let mut table = Self::default();
        for id in set.ids() {
            table.intern(id.clone());
        }
        table
    }

    /// The index of `id`, added when it is new. One output of a capability
    /// the table does not hold is refused: it would have no actuator.
    pub fn intern(&mut self, id: CapabilityId) -> CapIndex {
        if let Some(i) = self.index.get(&id) {
            return *i;
        }
        let parent = id.output().map(|(whole, _)| self.intern(whole));
        let i = CapIndex(u16::try_from(self.ids.len()).expect("more than 65535 capabilities"));
        self.ids.push(id.clone());
        self.parents.push(parent);
        self.index.insert(id, i);
        i
    }

    pub fn index_of(&self, id: &CapabilityId) -> Option<CapIndex> {
        self.index.get(id).copied()
    }

    pub fn id(&self, i: CapIndex) -> &CapabilityId {
        &self.ids[i.0 as usize]
    }

    /// The whole capability `i` is one output of, `None` when it is whole.
    pub fn parent(&self, i: CapIndex) -> Option<CapIndex> {
        self.parents[i.0 as usize]
    }

    /// Every output held apart from `i`.
    pub fn children(&self, i: CapIndex) -> impl Iterator<Item = CapIndex> + '_ {
        self.parents
            .iter()
            .enumerate()
            .filter(move |(_, p)| **p == Some(i))
            .map(|(n, _)| CapIndex(n as u16))
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}
