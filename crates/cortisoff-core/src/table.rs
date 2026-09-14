use std::collections::BTreeMap;

use cortisoff_host::{CapIndex, CapabilityId, CapabilitySet};

/// The capability table, built once at load and never mutated afterwards.
///
/// Strings are interned here so the engine's hot path indexes a vector instead
/// of hashing a string on every event.
#[derive(Debug, Clone, Default)]
pub struct CapTable {
    ids: Vec<CapabilityId>,
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

    pub fn intern(&mut self, id: CapabilityId) -> CapIndex {
        if let Some(i) = self.index.get(&id) {
            return *i;
        }
        let i = CapIndex(u16::try_from(self.ids.len()).expect("more than 65535 capabilities"));
        self.ids.push(id.clone());
        self.index.insert(id, i);
        i
    }

    pub fn index_of(&self, id: &CapabilityId) -> Option<CapIndex> {
        self.index.get(id).copied()
    }

    pub fn id(&self, i: CapIndex) -> &CapabilityId {
        &self.ids[i.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}
