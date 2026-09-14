use serde::{Deserialize, Serialize};

use crate::actuator::ActuatorError;
use crate::capability::CapabilityId;
use crate::value::Value;

/// One capability whose baseline sits in the journal because a value is
/// currently applied on top of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub cap: CapabilityId,
    pub baseline: Value,
}

/// Where baselines survive a crash.
///
/// The contract that makes the journal worth having: `record` returns only
/// once the entry would survive the process dying on the very next
/// instruction. A file-backed implementation must flush before returning, not
/// buffer.
pub trait JournalStore {
    fn record(&mut self, cap: &CapabilityId, baseline: &Value) -> Result<(), ActuatorError>;

    /// The capability is back at its baseline, so a crash no longer needs the
    /// entry.
    fn clear(&mut self, cap: &CapabilityId) -> Result<(), ActuatorError>;

    /// What a previous run left behind by dying with values still applied.
    fn dirty(&self) -> Result<Vec<JournalEntry>, ActuatorError>;
}
