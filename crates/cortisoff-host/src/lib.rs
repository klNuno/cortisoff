//! The contract between `cortisoff-core` and an operating system.
//!
//! This crate holds types, traits, and the one piece of logic every platform
//! reuses: the [`Dispatcher`], which journals baselines before applying. It
//! makes no OS call, reads no clock, and depends on nothing but serde. Both
//! sides of the project point at it and neither points at the other:
//!
//! ```text
//! cortisoff-core  ->  cortisoff-host  <-  cortisoff-win / -linux / -mac
//! ```
//!
//! Adding an operating system means implementing [`Host`] and a set of
//! [`Actuator`]s. It never means touching the core.

#![forbid(unsafe_code)]

mod actuator;
mod capability;
mod dispatcher;
mod event;
mod interference;
mod journal;
mod value;

/// Test doubles, for this crate's own tests and for dependents enabling the
/// `test-util` feature. Never compiled into a shipping binary.
#[cfg(any(test, feature = "test-util"))]
pub mod mock;

pub use actuator::{Actuator, ActuatorError};
pub use capability::{ids, label, CapIndex, CapabilityId, CapabilitySet, Control, Unavailable};
pub use dispatcher::{Dispatcher, Outcome};
pub use event::{Action, AppRef, Event, LogLevel, TimerId};
pub use interference::{
    identify_running, identity_ramp, inspect, GammaRamp, RampVerdict, KNOWN_COLOUR_APPS,
};
pub use journal::{JournalEntry, JournalStore};
pub use value::Value;

use std::time::Duration;

/// One operating system's implementation.
///
/// The daemon drives it: probe, install what the rules read, then call
/// [`Host::poll`] in a loop and feed what comes back to the supervisor.
pub trait Host {
    /// Probe what this machine can actually do, right now. Never a `#[cfg]`
    /// decision: the same Windows binary reports different capabilities on an
    /// NVIDIA machine and on an Intel laptop.
    fn probe(&self) -> CapabilitySet;

    /// Update the activity indicator when an unblocked claim is held.
    fn set_active(&mut self, _active: bool) {}

    /// Install only the sources named here. Anything absent must not be
    /// hooked, because an installed hook costs on every system-wide event.
    fn install_sources(&mut self, wanted: &[CapabilityId]) -> Result<(), ActuatorError>;

    fn actuator(&self, cap: &CapabilityId) -> Option<&dyn Actuator>;

    /// Fire `id` once, `after` from now, give or take `tolerance`. Once: the
    /// engine re-arms what it still needs on every clock event, so a platform
    /// whose native timers repeat (a Win32 `WM_TIMER` does) kills its timer
    /// when it fires. Arming an id that is pending replaces it.
    fn set_timer(&mut self, id: TimerId, after: Duration, tolerance: Duration);
    fn cancel_timer(&mut self, id: TimerId);

    /// Block in whatever native loop this platform requires until at least one
    /// event is ready or `timeout` elapses, then hand back everything that
    /// arrived, in order.
    ///
    /// The platform still owns its loop: a Win32 message pump, an epoll wait,
    /// a CFRunLoop, each running inside this call. What it does not own is the
    /// daemon, which is why this returns events rather than taking a callback.
    /// A callback would need the host mutably while the host is already
    /// borrowed to run the loop, and there is no honest way to write that.
    ///
    /// Returning an empty vector is normal and costs nothing: it means the
    /// timeout elapsed with nothing to report.
    fn poll(&mut self, timeout: Duration) -> Vec<Event>;
}

/// Wall clock, injected so the core never reads one.
///
/// The core asks for timers and reacts to the events the host pushes back when
/// they fire. Idle logic is therefore deterministic under test.
pub trait Clock {
    fn now_millis(&self) -> u64;
}
