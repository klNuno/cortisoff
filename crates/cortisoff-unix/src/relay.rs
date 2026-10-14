//! The guard's half of a stop: pass it on to the worker.
//!
//! A terminal's Ctrl-C reaches the whole process group, guard and worker
//! alike. launchd, systemd and `kill` signal one pid, the guard's, and a
//! guard that only noted the stop would leave its worker running with the
//! screen still changed, until something killed it with no chance to restore
//! anything. So the guard catches SIGINT, SIGTERM and SIGHUP like the worker
//! does, and on each sends the worker SIGTERM: the worker restores every
//! baseline and writes its exit line. A worker still there after the grace
//! period gets SIGKILL, and the journal covers what it held.
//!
//! The pass-on runs on the signal thread `console` already parks on its
//! self-pipe, through the waker slot the guard has no socket for: never in a
//! signal handler, so it may start a thread for the grace period.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::console;

/// A worker's pid for the guard to pass a stop on to.
#[derive(Clone)]
pub struct Relay {
    /// The running worker, 0 between two of them.
    child: Arc<AtomicI32>,
    grace: Duration,
}

impl Relay {
    /// Catch SIGINT, SIGTERM and SIGHUP in this process and pass each on to
    /// the tracked worker as SIGTERM, then as SIGKILL once `grace` has gone
    /// by. `None` when the handlers could not be set up: the guard then dies
    /// of the signal as it would have before.
    pub fn install(grace: Duration) -> Option<Self> {
        let relay = Self::new(grace);
        let forward = relay.clone();
        console::set_waker(Arc::new(move || forward.forward()));
        console::watch_for_stop().then_some(relay)
    }

    fn new(grace: Duration) -> Self {
        Self {
            child: Arc::new(AtomicI32::new(0)),
            grace,
        }
    }

    /// The worker just started. A stop that came in before its pid was known
    /// is passed on now, or the worker would never hear it.
    pub fn track(&self, pid: u32) {
        self.child.store(pid as i32, Ordering::SeqCst);
        if console::stop_requested() {
            self.forward();
        }
    }

    /// The worker has been waited for: its pid may now belong to anyone.
    pub fn untrack(&self) {
        self.child.store(0, Ordering::SeqCst);
    }

    /// True once this process was asked to stop. A worker ending after that
    /// is not started again, however it ended.
    pub fn stopped(&self) -> bool {
        console::stop_requested()
    }

    fn forward(&self) {
        let pid = self.child.load(Ordering::SeqCst);
        if pid <= 0 {
            return;
        }
        // SAFETY: `pid` is a child this process has not waited for yet, so
        // the number cannot have been handed to another process.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let child = Arc::clone(&self.child);
        let grace = self.grace;
        let _ = std::thread::Builder::new()
            .name("cortisoff-stop-grace".to_owned())
            .spawn(move || {
                std::thread::sleep(grace);
                // Still the same worker, still not waited for. The guard
                // clears the pid right after `wait` returns, which leaves a
                // window of a few instructions, never one of `grace`.
                if child.load(Ordering::SeqCst) == pid {
                    // SAFETY: as above.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    /// A worker that says `ready` once `script` has set itself up, then
    /// sleeps.
    fn run(script: &str) -> Child {
        let mut child = Command::new("sh")
            .args(["-c", &format!("{script} echo ready; exec sleep 30")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("sh starts");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line.trim(), "ready");
        child
    }

    /// The worker's pid set directly, not through `track`: `track` passes on
    /// a stop already requested, and `console`'s test raises one in this
    /// process while it runs. That test owns the flag and the waker; these
    /// test the pass-on alone.
    fn tracking(relay: &Relay, worker: &Child) {
        relay.child.store(worker.id() as i32, Ordering::SeqCst);
    }

    #[test]
    fn a_relayed_stop_ends_the_worker_with_sigterm() {
        let relay = Relay::new(Duration::from_secs(5));
        let mut worker = run("");
        tracking(&relay, &worker);
        let began = Instant::now();
        relay.forward();
        let status = worker.wait().unwrap();
        relay.untrack();
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        assert!(
            began.elapsed() < Duration::from_secs(4),
            "ended by SIGTERM, not the grace kill"
        );
    }

    #[test]
    fn a_worker_that_ignores_sigterm_is_killed_after_the_grace() {
        let relay = Relay::new(Duration::from_millis(200));
        let mut worker = run("trap '' TERM;");
        tracking(&relay, &worker);
        relay.forward();
        let status = worker.wait().unwrap();
        relay.untrack();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }

    #[test]
    fn nothing_is_signalled_with_no_worker() {
        let relay = Relay::new(Duration::from_millis(1));
        relay.forward();
        relay.untrack();
        relay.forward();
    }
}
