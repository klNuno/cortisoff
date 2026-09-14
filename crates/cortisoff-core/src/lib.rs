//! The engine. It knows no operating system.
//!
//! No OS call, no clock read, no thread, and no dependency on any platform
//! crate. The whole crate is a function of state and incoming events, which is
//! what lets `cargo test` run on any machine with no GPU, no admin rights and
//! no Windows.
//!
//! Breaking that is not a style question: it removes the project's ability to
//! be contributed to by anyone who does not own the hardware.

#![forbid(unsafe_code)]

mod arbiter;
mod config;
mod engine;
mod rule;
mod table;
mod transition;
mod world;
mod write;

pub use arbiter::{Arbiter, Claim, OwnerId};
pub use config::{parse_rules, ConfigError};
pub use engine::{Engine, PREVIEW_TIMER, SCHEDULE_TIMER};
pub use rule::{Assign, ClockTime, Compiled, Condition, Rule, RuleState};
pub use table::CapTable;
pub use transition::{format_duration, parse_duration, start_from_rest, Fade, Transition};
pub use world::{Signals, World};
pub use write::write_rules;

/// Priorities the project ships with. A rule may name any integer, these are
/// the anchors documented for rule authors.
pub mod priority {
    /// A rule that sets a default and expects to lose to everything.
    pub const BACKGROUND: i32 = -100;
    /// The normal band for rules loaded from a file.
    pub const RULE: i32 = 0;
    /// A rule reacting to the focused application, which should beat a
    /// schedule-driven one.
    pub const FOCUS: i32 = 100;
    /// The user reached for the CLI or the dashboard. Nothing outranks that.
    pub const MANUAL: i32 = 1000;
    /// A slider being dragged. It outranks the override it is about to
    /// replace, and it is gone the moment the drag ends.
    pub const PREVIEW: i32 = 2000;
}
