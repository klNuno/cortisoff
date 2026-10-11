//! Who holds what, and how a value gets there. It knows no operating system
//! and no plugin language.
//!
//! No OS call, no clock read, no thread, and no dependency on any platform
//! crate. The time comes in as an argument, so every fade is testable on any
//! machine with no GPU, no admin rights and no Windows.

#![forbid(unsafe_code)]

mod arbiter;
mod blend;
mod claims;
mod table;
mod world;

pub use arbiter::{Arbiter, Claim, OwnerId};
pub use blend::{blend, fadeable};
pub use claims::{Claims, FADE_TIMER, PREVIEW_FADE_MS};
pub use table::CapTable;
pub use world::{Signals, World};

/// Priorities the project ships with. A plugin may name any integer, these
/// are the anchors documented for plugin authors.
pub mod priority {
    /// A plugin that sets a default and expects to lose to everything.
    pub const BACKGROUND: i32 = -100;
    /// What a plugin claims with when it names no priority.
    pub const PLUGIN: i32 = 0;
    /// A plugin reacting to the focused application, which should beat a
    /// schedule-driven one.
    pub const FOCUS: i32 = 100;
    /// The user reached for the CLI or the window. Nothing outranks that.
    pub const MANUAL: i32 = 1000;
    /// A slider being dragged. It outranks the override it is about to
    /// replace, and it is gone the moment the drag ends.
    pub const PREVIEW: i32 = 2000;
}
