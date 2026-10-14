//! What the Linux and macOS daemons share below their `Host`: the control
//! socket, the once-per-session lock, stopping on a signal, the local clock
//! and the process list.
//!
//! It is the unix twin of what `cortisoff-win` gives the Windows daemon
//! through `pipe.rs`, `console.rs`, `process.rs`, `startup.rs` and
//! `pump::local_time`, with the same names and shapes, so the daemon's unix
//! platform module reads like its Windows one with other paths. Displays,
//! focus and idle are each host's own business and live in those crates. The
//! crate compiles to nothing on Windows.

#![cfg(unix)]

mod clock;
mod console;
mod paths;
mod process;
mod ready;
mod relay;
mod socket;
mod startup;

pub use clock::{local_time, time_of_day_event};
pub use console::{request_stop, stop_requested, watch_for_stop};
pub use paths::socket_path;
pub use process::{claim_dashboard, claim_session, efficiency_mode, InstanceLock};
pub use ready::wait_for_daemon;
pub use relay::Relay;
pub use socket::{ask, connect, Client, Incoming, SocketServer};
pub use startup::running_processes;

/// A private directory per test, removed when the test ends. Short names on
/// purpose: macOS caps a socket path at 104 bytes and its temporary directory
/// already spends about fifty of them.
#[cfg(test)]
pub(crate) mod scratch {
    use std::os::unix::fs::DirBuilderExt;
    use std::path::{Path, PathBuf};

    pub struct Scratch(PathBuf);

    impl Scratch {
        pub fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("cxu-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(&dir)
                .expect("scratch directory");
            Self(dir)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }

        pub fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
