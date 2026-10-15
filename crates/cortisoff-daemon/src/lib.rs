//! The daemon: the process that runs the plugins and owns the hardware.
//!
//! Everything a test can reach without an operating system lives in the
//! modules below [`run`]: paths, the file-backed journal, the startup gate,
//! the supervisor and the server. [`run`] is the one place that picks a
//! platform, and the app binary calls it for `cortisoff --daemon`.

#![forbid(unsafe_code)]

mod guard;
mod journal;
mod log;
mod logon;
mod paths;
mod run;
mod server;
mod startup;
mod supervisor;
mod updates;

pub use guard::{crash, supervise, RESTART};
pub use journal::FileJournal;
pub use log::{append, DaemonLog, Mark, Previous, Stamp};
pub use paths::{Layout, PathError, Paths};
pub use run::{
    autostart, run, spawn_detached, spawn_held, start_at_logon, start_held, starts_at_logon,
    Options, USAGE,
};
pub use server::{ClientId, Outbound, Server};
pub use startup::Interference;
pub use supervisor::{Supervisor, PLUGIN_TIMER, WATCH_TIMER};
pub use updates::{Found, Staged, Updates};
