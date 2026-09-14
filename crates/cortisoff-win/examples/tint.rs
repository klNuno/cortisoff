//! Applies a colour temperature through the real actuator, then puts the
//! baseline back and checks it came back byte for byte.
//!
//! The one thing the unit tests cannot answer: they exercise the arithmetic,
//! not a monitor. Run it with nothing else driving the screen, or the other
//! application overwrites the ramp between the capture and the restore and the
//! comparison is meaningless.
//!
//!     cargo run -p cortisoff-win --example tint -- show
//!     cargo run -p cortisoff-win --example tint -- 3400 6
//!
//! Kelvin then seconds to hold. The baseline is restored on the way out even
//! when the apply fails. `show` stops after the capture: HDR per output and
//! the panel gains DDC/CI answered with, and nothing written anywhere.

use std::time::Duration;

use cortisoff_host::{ids, CapabilityId, Host, Value};
use cortisoff_win::WinHost;

fn main() {
    let first = std::env::args().nth(1);
    let show = first.as_deref() == Some("show");
    let kelvin: i64 = first.and_then(|s| s.parse().ok()).unwrap_or(3400);
    let hold = Duration::from_secs(
        std::env::args()
            .nth(2)
            .and_then(|s| s.parse().ok())
            .unwrap_or(6),
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

    let cap = CapabilityId::new(ids::DISPLAY_COLOR_TEMP);
    let Some(actuator) = host.actuator(&cap) else {
        eprintln!("display.color_temp is not available on this machine");
        std::process::exit(1);
    };

    let baseline = match actuator.capture() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("capture failed: {e}");
            std::process::exit(1);
        }
    };
    let encoded = match &baseline {
        Value::Text(s) => s.clone(),
        other => {
            eprintln!("the baseline should be text, got {other:?}");
            std::process::exit(1);
        }
    };
    println!("baseline captured, {} bytes", encoded.len());
    if show {
        for output in cortisoff_win::hdr_outputs() {
            let hdr = match (output.supported, output.enabled) {
                (_, true) => "HDR on",
                (true, false) => "HDR supported, off",
                (false, false) => "no HDR",
            };
            println!("  {}: {hdr}", output.device);
        }
        let panels: Vec<&str> = encoded
            .split(';')
            .filter_map(|entry| entry.strip_prefix("ddc:"))
            .collect();
        if panels.is_empty() {
            println!("  no panel answered DDC/CI, or no output can turn HDR on");
        }
        for panel in panels {
            println!("  panel {panel}, min/current/max for red, green, blue");
        }
        return;
    }

    match actuator.apply(&Value::Int(kelvin)) {
        Ok(()) => println!("applied {kelvin} K, holding {} s", hold.as_secs()),
        Err(e) => eprintln!("apply failed: {e}"),
    }
    std::thread::sleep(hold);

    match actuator.apply(&baseline) {
        Ok(()) => println!("baseline restored"),
        Err(e) => eprintln!("restore failed: {e}"),
    }

    match actuator.capture() {
        Ok(Value::Text(after)) if after == encoded => println!("round trip exact"),
        Ok(Value::Text(after)) => {
            println!("RAMP DID NOT COME BACK: {} bytes now", after.len());
            for (device, before, now) in diff(&encoded, &after) {
                println!("  {device}: {before} -> {now}");
            }
        }
        Ok(other) => println!("unexpected recapture: {other:?}"),
        Err(e) => println!("recapture failed: {e}"),
    }
}

/// First differing entry per device, so a mismatch says which monitor and
/// where in the ramp rather than dumping 768 numbers.
fn diff(before: &str, after: &str) -> Vec<(String, String, String)> {
    let now: Vec<(String, String)> = split(after);
    split(before)
        .into_iter()
        .filter_map(|(device, was)| {
            let is = now.iter().find(|(d, _)| *d == device)?;
            (is.1 != was).then(|| (device, short(&was), short(&is.1)))
        })
        .collect()
}

fn split(encoded: &str) -> Vec<(String, String)> {
    encoded
        .split(';')
        .filter(|s| !s.is_empty())
        .filter_map(|entry| {
            let (name, hex) = entry.split_once('=')?;
            Some((name.to_string(), hex.to_string()))
        })
        .collect()
}

fn short(hex: &str) -> String {
    hex.chars().take(16).collect()
}
