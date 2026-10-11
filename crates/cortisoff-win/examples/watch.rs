//! Prints what the Windows host sees, for as many seconds as you pass it.
//!
//! `cargo run -p cortisoff-win --example watch -- 20`, then alt-tab, lock the
//! session, unplug a monitor. Nothing is applied and nothing is changed: this
//! is the source side only.

#[cfg(not(windows))]
fn main() {
    eprintln!("this example drives Windows APIs and runs on Windows only");
}

#[cfg(windows)]
fn main() {
    imp::main();
}

#[cfg(windows)]
mod imp {
    use std::time::{Duration, Instant};

    use cortisoff_host::{ids, CapabilityId, Host};
    use cortisoff_win::WinHost;

    pub fn main() {
        let seconds: u64 = std::env::args()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);

        let mut host = WinHost::new().expect("message window");
        let wanted: Vec<CapabilityId> = [ids::APP_FOCUS, ids::SESSION_LOCK, ids::USER_IDLE]
            .iter()
            .map(|id| CapabilityId::new(*id))
            .collect();
        host.install_sources(&wanted).expect("sources");

        println!("watching for {seconds}s");
        let until = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < until {
            for event in host.poll(Duration::from_millis(500)) {
                println!("{event:?}");
            }
        }
        println!("done");
    }
}
