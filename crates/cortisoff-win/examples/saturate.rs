//! Applies a vibrance level through the real actuator, then puts the baseline
//! back and checks it came back exactly.
//!
//! The same shape as `tint`, for the same reason: the unit tests exercise the
//! bookkeeping, not a driver. Run it with VibranceGUI closed, or it overwrites
//! the level between the capture and the restore.
//!
//!     cargo run -p cortisoff-win --example saturate -- 100 5

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
    use std::time::Duration;

    use cortisoff_host::{ids, CapabilityId, Host, Value};
    use cortisoff_win::WinHost;

    pub fn main() {
        let level: i64 = std::env::args()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(100);
        let hold = Duration::from_secs(
            std::env::args()
                .nth(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(5),
        );

        let host = match WinHost::new() {
            Ok(h) => h,
            Err(e) => {
                eprintln!("no host: {e}");
                std::process::exit(1);
            }
        };
        for warning in host.warnings() {
            println!("warning: {warning}");
        }

        let cap = CapabilityId::new(ids::DISPLAY_VIBRANCE);
        let Some(actuator) = host.actuator(&cap) else {
            eprintln!("display.vibrance is not available on this machine");
            std::process::exit(1);
        };

        let baseline = match actuator.capture() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("capture failed: {e}");
                std::process::exit(1);
            }
        };
        println!("baseline {baseline:?}");

        match actuator.apply(&Value::Int(level)) {
            Ok(()) => println!("applied {level}, holding {} s", hold.as_secs()),
            Err(e) => eprintln!("apply failed: {e}"),
        }
        match actuator.capture() {
            Ok(now) => println!("reads back as {now:?}"),
            Err(e) => eprintln!("recapture failed: {e}"),
        }
        std::thread::sleep(hold);

        match actuator.apply(&baseline) {
            Ok(()) => println!("baseline restored"),
            Err(e) => eprintln!("restore failed: {e}"),
        }
        match actuator.capture() {
            Ok(after) if after == baseline => println!("round trip exact"),
            Ok(after) => println!("DID NOT COME BACK: {after:?}"),
            Err(e) => println!("recapture failed: {e}"),
        }
    }
}
