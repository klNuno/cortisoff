//! The half of the daemon that no operating system is needed to test.
//!
//! Paths, the file-backed journal, the startup gate and the supervisor. What
//! is missing from here is deliberate: the event loop, the hooks and the
//! actuators are the platform's, and the transport is the platform's too.
//! A `main` arrives with the first real [`cortisoff_host::Host`].

#![forbid(unsafe_code)]

mod journal;
mod log;
mod paths;
mod server;
mod startup;
mod supervisor;

pub use journal::FileJournal;
pub use log::{append, DaemonLog, Mark, Previous, Stamp};
pub use paths::{Layout, PathError, Paths};
pub use server::{ClientId, Outbound, Server};
pub use startup::Interference;
pub use supervisor::{resting, Supervisor};
