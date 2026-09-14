//! cortisoff T0 probe.
//!
//! Checks the colour backends on the connected hardware:
//!
//!   1. Do all three backends apply a visible tint.
//!   2. Can HDR and exclusive fullscreen be read on event, cheaply.
//!   3. What does DDC/CI actually cost on these monitors.
//!   4. Does the magnification effect really drop when the process is killed,
//!      and does the gamma ramp really survive. `volatile()` depends on it.
//!
//! Throwaway by design. The parts that survive move into cortisoff-win.

mod color;
mod context;
mod ddc;
mod gamma;
mod mag;

use windows::Win32::Graphics::Gdi::HMONITOR;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("report");
    let kelvin: f32 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3400.0);
    // Held for a fixed time rather than until Enter: the probe has to be
    // runnable from a shell with no stdin, otherwise it applies and restores
    // in the same millisecond and nobody sees anything.
    let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15);

    match cmd {
        "report" => report(),
        "mag" => cmd_mag(kelvin, secs),
        "gamma" => cmd_gamma(kelvin, secs),
        "ddc" => cmd_ddc(kelvin, secs),
        "hold" => cmd_hold(kelvin),
        "reset" => reset_all(),
        "dump" => dump(),
        other => {
            eprintln!("unknown command: {other}");
            eprintln!("usage: cortisoff-probe [report|mag|gamma|ddc|hold|reset] [kelvin]");
            std::process::exit(2);
        }
    }
}

fn report() {
    println!("== context ==");
    match context::notification_state() {
        Some(s) => println!("notification state   {s}"),
        None => println!("notification state   query failed"),
    }
    println!("exclusive fullscreen {}", context::exclusive_fullscreen());
    println!("gamma range unlocked {}", gamma::range_unlocked());

    println!();
    println!("== outputs (CCD) ==");
    match context::outputs() {
        Ok(outs) if outs.is_empty() => println!("no active output reported"),
        Ok(outs) => {
            for o in outs {
                println!(
                    "{:<40} hdr supported {:<5} enabled {}",
                    o.name, o.hdr_supported, o.hdr_enabled
                );
            }
        }
        Err(e) => println!("query failed: {e}"),
    }

    println!();
    println!("== monitors (GDI) ==");
    let mons = context::monitors();
    println!("{} monitor handle(s)", mons.len());

    println!();
    println!("== DDC/CI ==");
    let mut any = false;
    for h in &mons {
        for p in ddc::probe(*h) {
            any = true;
            println!(
                "{:<40} r {:>3}/{:<3} g {:>3}/{:<3} b {:>3}/{:<3}  read {:?}",
                p.description,
                p.red.current,
                p.red.max,
                p.green.current,
                p.green.max,
                p.blue.current,
                p.blue.max,
                p.read_latency
            );
        }
    }
    if !any {
        println!("no monitor answered DDC/CI. The HDR fallback does not exist here.");
    }
}

fn each_monitor(f: impl Fn(HMONITOR)) {
    for h in context::monitors() {
        f(h);
    }
}

fn cmd_mag(kelvin: f32, secs: u64) {
    let c = color::from_kelvin(kelvin);
    println!("magnification effect at {kelvin}K -> {:.3} {:.3} {:.3}", c.r, c.g, c.b);
    if let Err(e) = mag::init() {
        eprintln!("MagInitialize failed: {e}");
        return;
    }
    match mag::apply(&c) {
        Ok(()) => println!("applied, holding {secs}s"),
        Err(e) => {
            eprintln!("MagSetFullscreenColorEffect failed: {e}");
            let _ = mag::shutdown();
            return;
        }
    }
    hold(secs);
    let _ = mag::reset();
    let _ = mag::shutdown();
    println!("reset");
}

fn cmd_gamma(kelvin: f32, secs: u64) {
    let c = color::from_kelvin(kelvin);
    println!("gamma ramp at {kelvin}K -> {:.3} {:.3} {:.3}", c.r, c.g, c.b);
    if !gamma::range_unlocked() {
        println!("GdiIcmGammaRange is not 256: Windows will clamp this and it will look weak.");
    }
    let baselines: Vec<_> = context::monitors()
        .into_iter()
        .map(|h| (h, gamma::capture(h)))
        .collect();
    for (h, _) in &baselines {
        if let Err(e) = gamma::apply(*h, &c) {
            eprintln!("SetDeviceGammaRamp failed: {e}");
        }
    }
    println!("applied, holding {secs}s");
    hold(secs);
    for (h, base) in &baselines {
        match base {
            Some(r) => {
                let _ = gamma::set(*h, r);
            }
            None => {
                let _ = gamma::reset(*h);
            }
        }
    }
}

fn cmd_ddc(kelvin: f32, secs: u64) {
    let c = color::from_kelvin(kelvin);
    println!("DDC/CI gain at {kelvin}K -> {:.3} {:.3} {:.3}", c.r, c.g, c.b);
    println!("captured gains are restored afterwards, holding {secs}s");
    let hold = std::time::Duration::from_secs(secs);
    each_monitor(|h| match ddc::tint_then_restore(h, &c, hold) {
        Some(t) => println!("write {:?}, restore {:?}", t.write, t.restore),
        None => println!("monitor did not answer"),
    });
}

/// The volatility test. Apply both surviving-capable backends, then kill this
/// process from outside and look at the screen. The magnification effect is
/// expected to vanish, the gamma ramp is expected to stay.
fn cmd_hold(kelvin: f32) {
    let c = color::from_kelvin(kelvin);
    let _ = mag::init();
    let mag_ok = mag::apply(&c).is_ok();
    let mons = context::monitors();
    let gamma_ok = mons.iter().all(|h| gamma::apply(*h, &c).is_ok());

    println!("pid {}", std::process::id());
    println!("magnification applied {mag_ok}, gamma applied {gamma_ok}");
    println!();
    println!("Kill this process from another terminal, then look at the screen:");
    println!("  taskkill /F /PID {}", std::process::id());
    println!("Expected: the magnification tint disappears, the gamma tint stays.");
    println!("Run `cortisoff-probe reset` afterwards to clear the gamma ramp.");
    loop {
        std::thread::park();
    }
}

fn reset_all() {
    let _ = mag::init();
    let _ = mag::reset();
    let _ = mag::shutdown();
    each_monitor(|h| {
        let _ = gamma::reset(h);
    });
    println!("magnification effect cleared, gamma ramps reset to linear.");
    println!("DDC/CI gain is not touched: it lives in the panel, not in Windows.");
}

/// Reads back the gamma ramp actually installed on each monitor.
///
/// Tells a residue left by this probe apart from a tint another application is
/// applying on purpose. A linear ramp ends at 65535 on all three channels;
/// anything lower on green or blue is somebody's blue light filter.
fn dump() {
    for (i, h) in context::monitors().into_iter().enumerate() {
        match gamma::capture(h) {
            Some(r) => {
                let (top_r, top_g, top_b) = (r[255], r[511], r[767]);
                let linear = top_r == 65535 && top_g == 65535 && top_b == 65535;
                println!(
                    "monitor {i}  top of ramp r {top_r} g {top_g} b {top_b}  {}",
                    if linear {
                        "linear, nothing applied"
                    } else {
                        "tinted by something"
                    }
                );
            }
            None => println!("monitor {i}  ramp unreadable"),
        }
    }
}

fn hold(secs: u64) {
    std::thread::sleep(std::time::Duration::from_secs(secs));
}
