//! The Linux implementation of [`cortisoff_host::Host`], for X11 and Wayland
//! sessions, one host picking its backends at runtime.
//!
//! On X11 the sources and actuators speak the X protocol. Under Wayland,
//! XWayland still serves X11 applications (clipboard, windows, typing), and
//! the session's D-Bus services answer what XWayland cannot see: logind for
//! the lock, Mutter for idle and GNOME's Night Light for the colour. A
//! session with neither, headless for instance, still gets a host: it serves
//! timers and the waker and reports every missing capability with the
//! reason, rather than failing to start.
//!
//! The crate compiles to nothing on other platforms, so a Windows workspace
//! check never pulls an X11 or D-Bus stack.

#![cfg(target_os = "linux")]

pub mod autostart;
mod clipboard;
mod clock;
mod color;
mod dbus;
mod gamma;
mod host;
mod input;
mod keymap;
mod mode;
mod nightlight;
mod procfs;
mod randr;
mod timers;
mod wake;
mod wm;
mod x11;

pub use host::LinuxHost;
