//! Backend 3: DDC/CI RGB gain, driven into the panel itself.
//!
//! The only colour path that survives HDR, exclusive fullscreen and the
//! compositor, because Windows is not involved in the result. It pays for that
//! with latency measured in tens to hundreds of milliseconds and with monitors
//! that quietly ignore gain outside certain picture modes. Never put it on a
//! hot path.

use std::time::{Duration, Instant};

use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorRedGreenOrBlueGain,
    GetNumberOfPhysicalMonitorsFromHMONITOR, GetPhysicalMonitorsFromHMONITOR,
    SetMonitorRedGreenOrBlueGain, MC_BLUE_GAIN, MC_GAIN_TYPE, MC_GREEN_GAIN, MC_RED_GAIN,
    PHYSICAL_MONITOR,
};
use windows::Win32::Graphics::Gdi::HMONITOR;

use crate::color::Rgb;

pub struct Gain {
    pub min: u32,
    pub current: u32,
    pub max: u32,
}

pub struct Probe {
    pub description: String,
    pub red: Gain,
    pub green: Gain,
    pub blue: Gain,
    pub read_latency: Duration,
}

const CHANNELS: [MC_GAIN_TYPE; 3] = [MC_RED_GAIN, MC_GREEN_GAIN, MC_BLUE_GAIN];

fn open(h: HMONITOR) -> Option<Vec<PHYSICAL_MONITOR>> {
    let mut n = 0u32;
    unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(h, &mut n) }.ok()?;
    if n == 0 {
        return None;
    }
    let mut buf = vec![PHYSICAL_MONITOR::default(); n as usize];
    unsafe { GetPhysicalMonitorsFromHMONITOR(h, &mut buf) }.ok()?;
    Some(buf)
}

fn close(buf: &[PHYSICAL_MONITOR]) {
    unsafe {
        let _ = DestroyPhysicalMonitors(buf);
    }
}

fn read(pm: &PHYSICAL_MONITOR, ch: MC_GAIN_TYPE) -> Option<Gain> {
    let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
    let ok = unsafe {
        GetMonitorRedGreenOrBlueGain(pm.hPhysicalMonitor, ch, &mut min, &mut cur, &mut max)
    };
    (ok != 0).then_some(Gain {
        min,
        current: cur,
        max,
    })
}

/// Reads every channel on every physical monitor and times it. The latency
/// number is the whole reason this runs before anything is designed around it.
pub fn probe(h: HMONITOR) -> Vec<Probe> {
    let Some(buf) = open(h) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for pm in &buf {
        let started = Instant::now();
        let (Some(red), Some(green), Some(blue)) = (
            read(pm, MC_RED_GAIN),
            read(pm, MC_GREEN_GAIN),
            read(pm, MC_BLUE_GAIN),
        ) else {
            continue;
        };
        // PHYSICAL_MONITOR is packed(1), so the field is copied out before any
        // reference to it exists.
        let desc = pm.szPhysicalMonitorDescription;
        out.push(Probe {
            description: String::from_utf16_lossy(&desc)
                .trim_end_matches('\0')
                .to_string(),
            red,
            green,
            blue,
            read_latency: started.elapsed(),
        });
    }
    close(&buf);
    out
}

pub struct Timing {
    pub write: Duration,
    pub restore: Duration,
}

/// Tints by scaling each captured gain by its multiplier, holds, then puts the
/// captured gains back.
///
/// Capture and restore are not a convenience here, they are mandatory. These
/// gains live in the panel and no monitor ships at a neutral 50/50/50: the
/// probe read 51/45/48 and 100/92/96 on this machine. Applying a tint with no
/// restore path destroys a user's calibration permanently, and re-running at
/// 6500 K would compound the reduction rather than undo it.
pub fn tint_then_restore(h: HMONITOR, c: &Rgb, hold: Duration) -> Option<Timing> {
    let buf = open(h)?;

    // Capture first. Nothing is written until every channel is known.
    let mut captured: Vec<(usize, MC_GAIN_TYPE, Gain)> = Vec::new();
    for (i, pm) in buf.iter().enumerate() {
        for ch in CHANNELS {
            if let Some(g) = read(pm, ch) {
                captured.push((i, ch, g));
            }
        }
    }
    if captured.is_empty() {
        close(&buf);
        return None;
    }

    let started = Instant::now();
    for (i, ch, g) in &captured {
        let m = match *ch {
            MC_GREEN_GAIN => c.g,
            MC_BLUE_GAIN => c.b,
            _ => c.r,
        };
        let target = ((g.current as f32 * m).round() as u32).clamp(g.min, g.max);
        unsafe {
            SetMonitorRedGreenOrBlueGain(buf[*i].hPhysicalMonitor, *ch, target);
        }
    }
    let write = started.elapsed();

    std::thread::sleep(hold);

    let started = Instant::now();
    for (i, ch, g) in &captured {
        unsafe {
            SetMonitorRedGreenOrBlueGain(buf[*i].hPhysicalMonitor, *ch, g.current);
        }
    }
    let restore = started.elapsed();

    close(&buf);
    Some(Timing { write, restore })
}
