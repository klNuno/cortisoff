//! The macOS half of cortisoff: [`MacHost`], its event sources and its
//! actuators, the twin of `cortisoff-win`.
//!
//! The whole crate is empty on any other system, so the workspace builds
//! everywhere and the daemon picks a host with one `#[cfg]` of its own.
//! What this machine can do is still probed at runtime, never decided here.
#![cfg(target_os = "macos")]

pub mod autostart;
mod ax;
mod clock;
mod color;
mod colortemp;
mod fit;
mod host;
mod hotkeys;
mod input;
mod keys;
mod mode;
mod modes;
mod notify;
mod runloop;
mod screen;
mod sources;
mod timers;
mod windows;

pub use host::MacHost;
