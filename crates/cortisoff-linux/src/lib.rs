//! The Linux implementation of [`cortisoff_host::Host`], for X11 sessions.
//!
//! XWayland counts as X11 here: the sources see X11 applications through it,
//! and the actuators report what XWayland does not pass on to the monitor.
//! A session with no X display at all, pure Wayland or headless, still gets a
//! host: it serves timers and the waker and reports every X11 capability
//! missing with the reason, rather than failing to start.
//!
//! The crate compiles to nothing on other platforms, so a Windows workspace
//! check never pulls an X11 stack.

#![cfg(target_os = "linux")]

pub mod autostart;
mod clipboard;
mod clock;
mod color;
mod gamma;
mod host;
mod input;
mod keymap;
mod mode;
mod procfs;
mod randr;
mod timers;
mod wake;
mod wm;
mod x11;

pub use host::LinuxHost;
