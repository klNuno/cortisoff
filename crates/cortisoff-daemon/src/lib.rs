//! The daemon: the process that runs the plugins and owns the hardware.
//!
//! Everything a test can reach without an operating system lives in the
//! modules below [`run`]: paths, the file-backed journal, the startup gate,
//! the supervisor and the server. [`run`] is the one place that picks a
//! platform, and the app binary calls it for `cortisoff --daemon`.

#![forbid(unsafe_code)]

mod journal;
mod log;
mod paths;
mod run;
mod server;
mod startup;
mod supervisor;

pub use journal::FileJournal;
pub use log::{append, DaemonLog, Mark, Previous, Stamp};
pub use paths::{Layout, PathError, Paths};
pub use run::{autostart, run, spawn_detached, Options, USAGE};
pub use server::{ClientId, Outbound, Server};
pub use startup::Interference;
pub use supervisor::{Supervisor, PLUGIN_TIMER, WATCH_TIMER};
