//! The Windows implementation of [`cortisoff_host::Host`].
//!
//! Everything Windows-specific in the project lives under here, and nothing
//! here knows what a plugin is. The crate compiles to nothing on other
//! platforms, so `cargo check --workspace --target x86_64-unknown-linux-gnu`
//! stays the proof that the core is portable.

#![cfg(windows)]

mod audio;
pub mod autostart;
mod clipboard;
mod color;
mod colortemp;
mod console;
mod ddc;
mod hotkeys;
mod input;
mod keymap;
mod maximized;
mod mode;
mod names;
mod notice;
mod nvapi;
mod pipe;
mod power;
mod process;
mod pump;
mod screen;
mod sources;
mod startup;
mod toplevel;
mod tray;
mod vibrance;
mod wallpaper;

pub use console::{attach_parent_console, request_stop, stop_requested, watch_for_stop};
pub use pipe::{ask, connect, session_id, wait_for_daemon, Client, Incoming, PipeServer};
pub use process::{
    claim_dashboard, claim_session, efficiency_mode, raise_dashboard, shed_memory, usage,
    InstanceLock, ProcessUsage,
};
pub use pump::{local_time, WinHost};
pub use screen::{hdr_outputs, Hdr};
pub use startup::{current_ramp, running_processes};
pub use tray::open_dashboard;
