//! Switches the refresh rate through the real actuator, then puts the baseline
//! back and checks it came back exactly.
//!
//! The same shape as `tint` and `saturate`, and for the same reason: the unit
//! tests exercise the matching arithmetic, not a display driver. This is the
//! only thing that proves a real monitor accepted the mode and went back.
//!
//!     cargo run -p cortisoff-win --example refresh -- show
//!     cargo run -p cortisoff-win --example refresh -- 60 5
//!     cargo run -p cortisoff-win --example refresh -- 2560x1440@240 5

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
        let wanted = std::env::args().nth(1).unwrap_or_else(|| "60".to_owned());
        let hold = Duration::from_secs(
            std::env::args()
                .nth(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(5),
        );
        // Read-only, so it is safe to run first on a machine whose outputs you do
        // not want to guess at, a virtual display adapter included.
        let show = wanted == "show";
        let target = match parse(&wanted) {
            Some(target) => target,
            None if show => Value::Mode {
                width: 0,
                height: 0,
                refresh_mhz: 0,
            },
            None => {
                eprintln!("could not read {wanted:?}. try show, 60, @240 or 2560x1440@240");
                std::process::exit(1);
            }
        };

        let host = match WinHost::new() {
            Ok(h) => h,
            Err(e) => {
                eprintln!("no host: {e}");
                std::process::exit(1);
            }
        };

        let cap = CapabilityId::new(ids::DISPLAY_MODE);
        let Some(actuator) = host.actuator(&cap) else {
            eprintln!("display.mode is not available on this machine");
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
        if show {
            return;
        }

        match actuator.apply(&target) {
            Ok(()) => println!("applied {target:?}, holding {} s", hold.as_secs()),
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

    /// The same three shapes the rule file takes, kept here rather than pulled out
    /// of the core so this example stays a test of the Windows half only.
    fn parse(text: &str) -> Option<Value> {
        let (size, refresh) = match text.split_once('@') {
            Some((size, refresh)) => (size, Some(refresh)),
            None => ("", Some(text)),
        };
        let (width, height) = match size.split_once('x') {
            Some((w, h)) => (w.parse().ok()?, h.parse().ok()?),
            None => (0, 0),
        };
        let refresh_mhz = match refresh {
            Some(r) if !r.is_empty() => (r.parse::<f64>().ok()? * 1000.0).round() as u32,
            _ => 0,
        };
        Some(Value::Mode {
            width,
            height,
            refresh_mhz,
        })
    }
}
