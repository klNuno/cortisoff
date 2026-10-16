//! DDC/CI RGB gain: the panel tints itself.
//!
//! The only colour path left on an output with HDR on, where the gamma ramp is
//! undefined and Night Light is switched off, because Windows is not involved
//! in the result. It pays for that on the wire: the T0 probe measured 185 ms to
//! read three channels on one monitor. So nothing here sits on a focus path, a
//! capture only asks outputs that can turn HDR on, and a fade step goes out only
//! when the integer gain it rounds to has changed.
//!
//! Panels do not sit at a neutral gain. The probe read 51/45/48, 100/92/96 and
//! 50/50/50 on three monitors. A tint is therefore the captured gain scaled
//! down, never a fraction of the maximum, and letting go means writing the
//! captured gain back, never a default.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::mem::size_of;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorRedGreenOrBlueGain, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, SetMonitorRedGreenOrBlueGain, MC_BLUE_GAIN, MC_GAIN_TYPE,
    MC_GREEN_GAIN, MC_RED_GAIN, PHYSICAL_MONITOR,
};
use windows::Win32::Graphics::Gdi::{EnumDisplayDevicesW, DISPLAY_DEVICEW, HMONITOR};
use windows::Win32::UI::WindowsAndMessaging::EDD_GET_DEVICE_INTERFACE_NAME;

use crate::color::Rgb;
use crate::screen;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gain {
    pub min: u32,
    pub current: u32,
    pub max: u32,
}

/// Red, green and blue, in that order.
pub type Gains = [Gain; 3];

const CHANNELS: [MC_GAIN_TYPE; 3] = [MC_RED_GAIN, MC_GREEN_GAIN, MC_BLUE_GAIN];

/// Marks a panel's entry in the baseline text. The ramp decoder already skips
/// an entry that is not 768 ramp values, so the two share one string and a
/// journal written before this backend existed still restores.
const PREFIX: &str = "ddc:";

/// A physical monitor behind a GDI device, `\\.\DISPLAY1#0`. One device drives
/// several when the output is mirrored.
pub fn key(device: &str, index: usize) -> String {
    format!("{device}#{index}")
}

pub fn device_of(key: &str) -> &str {
    key.rsplit_once('#').map_or(key, |(device, _)| device)
}

/// Every channel of every physical monitor behind `devices` that answers.
/// Around 185 ms a panel, so read the module comment before calling it from
/// anywhere new.
pub fn capture(devices: &[String]) -> Vec<(String, Gains)> {
    let mut out = Vec::new();
    for (device, h) in screen::handles() {
        if !devices.contains(&device) {
            continue;
        }
        let Some(physical) = open(h) else {
            continue;
        };
        for (i, pm) in physical.iter().enumerate() {
            if let (Some(r), Some(g), Some(b)) = (
                read(pm, CHANNELS[0]),
                read(pm, CHANNELS[1]),
                read(pm, CHANNELS[2]),
            ) {
                out.push((key(&device, i), [r, g, b]));
            }
        }
        close(&physical);
    }
    out
}

/// What reaches a panel. Behind a trait so the writer's timing, ordering and
/// verification are tested without a monitor.
pub trait Wire: Send + Sync {
    /// Set each `(channel, value)` in the order given. True only when every
    /// one was accepted: a panel that takes red and refuses blue is not
    /// carrying the tint.
    fn set(&self, key: &str, channels: &[(usize, u32)]) -> bool;
    /// What the panel holds now, `None` when it does not answer.
    fn get(&self, key: &str) -> Option<[u32; 3]>;
}

/// The monitors on this machine, through dxva2.
struct Panels;

impl Panels {
    fn with<R>(key: &str, f: impl FnOnce(&PHYSICAL_MONITOR) -> Option<R>) -> Option<R> {
        let (device, index) = key
            .rsplit_once('#')
            .and_then(|(d, i)| Some((d, i.parse::<usize>().ok()?)))?;
        let (_, h) = screen::handles().into_iter().find(|(d, _)| d == device)?;
        let physical = open(h)?;
        let out = physical.get(index).and_then(f);
        close(&physical);
        out
    }
}

impl Wire for Panels {
    fn set(&self, key: &str, channels: &[(usize, u32)]) -> bool {
        Self::with(key, |pm| {
            let mut all = true;
            // Every channel is attempted even after a refusal, so a panel is
            // never left with one channel at the new gain for want of trying.
            for &(i, value) in channels {
                let ok = unsafe {
                    SetMonitorRedGreenOrBlueGain(pm.hPhysicalMonitor, CHANNELS[i], value)
                };
                all &= ok != 0;
            }
            Some(all)
        })
        .unwrap_or(false)
    }

    fn get(&self, key: &str) -> Option<[u32; 3]> {
        Self::with(key, |pm| {
            Some([
                read(pm, CHANNELS[0])?.current,
                read(pm, CHANNELS[1])?.current,
                read(pm, CHANNELS[2])?.current,
            ])
        })
    }
}

/// The channels to send to go from `from` to `to`, in an order that never
/// shows a pink step. Each set is a separate 50 ms DDC/CI command, and the
/// panel shows every state in between, or keeps one for good when a later
/// command is lost: lowering green before blue, or raising blue before green,
/// leaves blue above green, which is magenta. So falling channels go first,
/// blue then green then red, and rising ones after, red then green then blue.
/// A channel already at its value is skipped unless `all`.
fn order(from: Option<[u32; 3]>, to: [u32; 3], all: bool) -> Vec<(usize, u32)> {
    let mut out = Vec::with_capacity(3);
    for i in [2, 1, 0] {
        if from.is_none_or(|f| to[i] < f[i]) {
            out.push((i, to[i]));
        }
    }
    for i in [0, 1, 2] {
        let Some(f) = from else { continue };
        if to[i] > f[i] || (all && to[i] == f[i]) {
            out.push((i, to[i]));
        }
    }
    out
}

/// Panel writes off the caller's thread.
///
/// One gain change is up to three DDC/CI sets of about 50 ms each. Made on
/// the colour actuator's thread, every step where a panel's integer gain moved
/// froze the gamma ramps of every other monitor for 150 ms: a 60 s fade on a
/// machine with one HDR panel and two SDR ones stuttered sixteen times in its
/// first fourteen seconds. Here the caller posts the gains it wants and goes
/// on; a panel slower than the fade skips to the latest gains rather than
/// queueing every step behind it.
///
/// A panel can accept a set and not apply it. One did on the way back from a
/// night: green stayed at 64 of 92, the next capture took that for the
/// baseline, and the screen was pink from then on. So once a panel has had
/// no write for [`VERIFY_AFTER`], its gains are read back and written again
/// when they are not what was asked, up to [`MISSES`] times.
pub struct Writer {
    shared: Arc<Shared>,
}

struct Shared {
    queue: Mutex<Queue>,
    changed: Condvar,
    wire: Box<dyn Wire>,
}

#[derive(Default)]
struct Queue {
    /// The newest gains asked of each panel and not yet written.
    pending: BTreeMap<String, [u32; 3]>,
    /// What each panel holds, as last written or read, so a write sends only
    /// what moved.
    known: BTreeMap<String, [u32; 3]>,
    /// Panels to read back, once their writes have stopped.
    checks: BTreeMap<String, Check>,
    /// Panels that refused a posted write since the caller last asked.
    refused: Vec<String>,
    busy: bool,
    running: bool,
    closed: bool,
}

#[derive(Debug, Clone, Copy)]
struct Check {
    due: Instant,
    target: [u32; 3],
    /// Reads that already found something else.
    misses: u8,
}

/// How long [`Writer::settle`] waits for writes already posted. Three panels
/// with one write each take about half a second.
const SETTLE: Duration = Duration::from_secs(3);

/// How long a panel goes without a write before it is read back. Long enough
/// that a fade is read as it ends or pauses, never on each step.
const VERIFY_AFTER: Duration = Duration::from_millis(1500);

/// Reads that find the wrong gains before a panel counts as refusing.
const MISSES: u8 = 3;

impl Writer {
    pub fn new() -> Self {
        Self::with(Panels)
    }

    fn with(wire: impl Wire + 'static) -> Self {
        Writer {
            shared: Arc::new(Shared {
                queue: Mutex::new(Queue::default()),
                changed: Condvar::new(),
                wire: Box::new(wire),
            }),
        }
    }

    /// Ask for `values` on `key` and return at once. A pending write to the
    /// same panel is replaced.
    pub fn post(&self, key: &str, values: [u32; 3]) {
        let mut q = self.lock();
        q.pending.insert(key.to_owned(), values);
        if !q.running {
            q.running = true;
            let shared = Arc::clone(&self.shared);
            let spawned = std::thread::Builder::new()
                .name("ddc".to_owned())
                .spawn(move || run(&shared));
            if spawned.is_err() {
                // No thread: write here rather than never.
                q.running = false;
                let pending = std::mem::take(&mut q.pending);
                drop(q);
                for (key, values) in pending {
                    self.write_now(&key, values);
                }
                return;
            }
        }
        self.shared.changed.notify_all();
    }

    /// Write on this thread, once every posted write has gone out so the two
    /// never cross on the wire, and read it back. For the first write to a
    /// panel, whose answer decides its path, and for putting a baseline back,
    /// which must land: every channel is sent whatever the panel is believed
    /// to hold, and sent again while a read finds something else. True once
    /// the panel holds `values`, or took them and cannot be read.
    pub fn write_now(&self, key: &str, values: [u32; 3]) -> bool {
        let mut q = self.settled();
        // This is the target from now on: nothing older goes out after it.
        q.pending.remove(key);
        q.checks.remove(key);
        let mut known = q.known.get(key).copied();
        q.busy = true;
        drop(q);
        let wire = &self.shared.wire;
        let mut ok = false;
        for _ in 0..MISSES {
            if !wire.set(key, &order(known, values, true)) {
                known = None;
                break;
            }
            match wire.get(key) {
                Some(held) => {
                    known = Some(held);
                    if held == values {
                        ok = true;
                        break;
                    }
                }
                None => {
                    known = Some(values);
                    ok = true;
                    break;
                }
            }
        }
        let mut q = self.lock();
        q.busy = false;
        match known {
            Some(v) => q.known.insert(key.to_owned(), v),
            None => q.known.remove(key),
        };
        self.shared.changed.notify_all();
        ok
    }

    /// Wait for every posted write to go out, up to [`SETTLE`]. Before a
    /// capture reads the panels and before anything writes them directly.
    /// A read back still to come is not waited for.
    pub fn settle(&self) {
        drop(self.settled());
    }

    /// The gains just read from each panel, which is what it holds now.
    pub fn read_back(&self, gains: &[(String, Gains)]) {
        let mut q = self.lock();
        for (key, g) in gains {
            q.known.insert(key.clone(), currents(g));
        }
    }

    /// Panels that refused a posted write since the last call.
    pub fn take_refused(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().refused)
    }

    fn settled(&self) -> MutexGuard<'_, Queue> {
        let q = self.lock();
        let (q, _) = self
            .shared
            .changed
            .wait_timeout_while(q, SETTLE, |q| q.busy || !q.pending.is_empty())
            .unwrap_or_else(PoisonError::into_inner);
        q
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.lock().closed = true;
        self.shared.changed.notify_all();
    }
}

fn run(shared: &Shared) {
    let lock = || shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
    let mut q = lock();
    loop {
        if q.closed && q.pending.is_empty() {
            q.running = false;
            return;
        }
        // A write on the caller's thread is under way: wait for it.
        if q.busy {
            q = shared
                .changed
                .wait(q)
                .unwrap_or_else(PoisonError::into_inner);
            continue;
        }
        let now = Instant::now();
        // The newest target of a panel first, then a read back that is due.
        let (key, target, misses, read) = if let Some((key, values)) = q.pending.pop_first() {
            (key, values, 0, None)
        } else if let Some((key, check)) = q
            .checks
            .iter()
            .find(|(_, c)| c.due <= now)
            .map(|(k, c)| (k.clone(), *c))
        {
            q.checks.remove(&key);
            q.busy = true;
            drop(q);
            let read = shared.wire.get(&key);
            q = lock();
            q.busy = false;
            shared.changed.notify_all();
            if q.pending.contains_key(&key) {
                // A new target arrived meanwhile: it goes out next instead.
                continue;
            }
            if read == Some(check.target) {
                q.known.insert(key, check.target);
                continue;
            }
            if check.misses + 1 >= MISSES {
                match read {
                    Some(held) => q.known.insert(key.clone(), held),
                    None => q.known.remove(&key),
                };
                q.refused.push(key);
                continue;
            }
            (key, check.target, check.misses + 1, Some(read))
        } else {
            let next = q.checks.values().map(|c| c.due).min();
            q = match next {
                Some(due) => {
                    shared
                        .changed
                        .wait_timeout(q, due.saturating_duration_since(now))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => shared
                    .changed
                    .wait(q)
                    .unwrap_or_else(PoisonError::into_inner),
            };
            continue;
        };
        // After a read, every channel goes from what it found; a read that
        // found nothing leaves nothing to skip.
        let (known, all) = match read {
            Some(found) => (found, true),
            None => (q.known.get(&key).copied(), false),
        };
        q.busy = true;
        drop(q);
        let ok = shared.wire.set(&key, &order(known, target, all));
        q = lock();
        q.busy = false;
        if ok {
            q.known.insert(key.clone(), target);
            let due = Instant::now() + VERIFY_AFTER;
            q.checks.insert(
                key,
                Check {
                    due,
                    target,
                    misses,
                },
            );
        } else {
            q.known.remove(&key);
            q.checks.remove(&key);
            q.refused.push(key);
        }
        shared.changed.notify_all();
    }
}

/// The gains a colour temperature asks of one panel. Always scaled from the
/// captured gain, so fade steps never compound, and clamped to what the panel
/// said it accepts.
pub fn targets(base: &Gains, c: &Rgb) -> [u32; 3] {
    let scale = |g: &Gain, m: f32| {
        let wanted = (g.current as f32 * m.clamp(0.0, 1.0)).round() as u32;
        wanted.clamp(g.min, g.max.max(g.min))
    };
    [
        scale(&base[0], c.r),
        scale(&base[1], c.g),
        scale(&base[2], c.b),
    ]
}

pub fn currents(base: &Gains) -> [u32; 3] {
    base.map(|g| g.current)
}

/// `ddc:\\.\DISPLAY1#0=0/51/100,0/45/100,0/48/100`, entries joined by `;`.
pub fn encode(gains: &[(String, Gains)]) -> String {
    gains
        .iter()
        .map(|(key, g)| {
            let channels: Vec<String> = g
                .iter()
                .map(|c| format!("{}/{}/{}", c.min, c.current, c.max))
                .collect();
            format!("{PREFIX}{key}={}", channels.join(","))
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// The panel entries of a baseline, skipping ramps and anything damaged.
pub fn decode(encoded: &str) -> Vec<(String, Gains)> {
    encoded
        .split(';')
        .filter_map(|part| {
            let (key, channels) = part.strip_prefix(PREFIX)?.split_once('=')?;
            let parsed: Vec<Gain> = channels
                .split(',')
                .map(|c| {
                    let mut n = c.split('/').map(|v| v.parse::<u32>().ok());
                    let gain = Gain {
                        min: n.next()??,
                        current: n.next()??,
                        max: n.next()??,
                    };
                    n.next().is_none().then_some(gain)
                })
                .collect::<Option<_>>()?;
            let gains: Gains = parsed.try_into().ok()?;
            Some((key.to_owned(), gains))
        })
        .collect()
}

/// The gains each panel had before this process first tinted it, on disk
/// until they are verified back.
///
/// The journal holds a baseline only as good as its capture, and a capture
/// reads whatever the panel shows. After a restore the panel took and did not
/// apply, it showed the night, and from then on the night was its baseline:
/// every evening started from it and every morning went back to it. A panel
/// remembered here is captured at its remembered gains instead, whatever it
/// shows, until a restore of it is read back right.
///
/// Keyed by the monitor itself, not by `\\.\DISPLAY3#0`: the device numbers
/// move when a monitor is unplugged, and one panel's gains written to another
/// is the bug this exists to prevent.
pub struct Memory {
    path: Option<PathBuf>,
    held: RefCell<BTreeMap<String, Gains>>,
    identify: fn(&str) -> String,
}

impl Memory {
    /// `%LOCALAPPDATA%\cortisoff\panels`, as left by the last run.
    pub fn open() -> Self {
        let path = std::env::var_os("LOCALAPPDATA")
            .map(|local| PathBuf::from(local).join("cortisoff").join("panels"));
        Self::at(path, identity)
    }

    /// Nothing on disk, for a test.
    #[cfg(test)]
    pub fn none() -> Self {
        Self::at(None, |key| key.to_owned())
    }

    fn at(path: Option<PathBuf>, identify: fn(&str) -> String) -> Self {
        let held = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|text| decode(&text).into_iter().collect())
            .unwrap_or_default();
        Memory {
            path,
            held: RefCell::new(held),
            identify,
        }
    }

    /// Gains just read, with the current value of each panel remembered here
    /// put back in place of what it shows.
    pub fn correct(&self, gains: Vec<(String, Gains)>) -> Vec<(String, Gains)> {
        let held = self.held.borrow();
        gains
            .into_iter()
            .map(|(key, mut read)| {
                if let Some(base) = held.get(&(self.identify)(&key)) {
                    for (g, b) in read.iter_mut().zip(base) {
                        g.current = b.current.clamp(g.min, g.max.max(g.min));
                    }
                }
                (key, read)
            })
            .collect()
    }

    /// `key` is about to be tinted from `base`. Kept if it is already held:
    /// the first base is the real one.
    pub fn hold(&self, key: &str, base: &Gains) {
        let id = (self.identify)(key);
        if self.held.borrow().contains_key(&id) {
            return;
        }
        self.held.borrow_mut().insert(id, *base);
        self.save();
    }

    /// `key` is back at its own gains, read back.
    pub fn forget(&self, key: &str) {
        let id = (self.identify)(key);
        if self.held.borrow_mut().remove(&id).is_some() {
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let held = self.held.borrow();
        if held.is_empty() {
            let _ = std::fs::remove_file(path);
            return;
        }
        let entries: Vec<(String, Gains)> = held.iter().map(|(k, g)| (k.clone(), *g)).collect();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, encode(&entries));
    }
}

/// The monitor behind `\\.\DISPLAY3#0`, by its device interface, which holds
/// its model and serial and does not move with the device numbers. The key
/// itself when Windows does not say.
fn identity(key: &str) -> String {
    let Some((device, index)) = key
        .rsplit_once('#')
        .and_then(|(d, i)| Some((d, i.parse::<u32>().ok()?)))
    else {
        return key.to_owned();
    };
    let wide: Vec<u16> = device.encode_utf16().chain(Some(0)).collect();
    let mut dd = DISPLAY_DEVICEW {
        cb: size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    let found = unsafe {
        EnumDisplayDevicesW(
            PCWSTR(wide.as_ptr()),
            index,
            &mut dd,
            EDD_GET_DEVICE_INTERFACE_NAME,
        )
    };
    let id = String::from_utf16_lossy(&dd.DeviceID);
    let id = id.trim_end_matches('\0');
    if found.as_bool() && !id.is_empty() {
        // Without `;` and `=`, which the encoding uses.
        id.replace([';', '='], "_")
    } else {
        key.to_owned()
    }
}

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

fn read(pm: &PHYSICAL_MONITOR, channel: MC_GAIN_TYPE) -> Option<Gain> {
    let (mut min, mut current, mut max) = (0u32, 0u32, 0u32);
    let ok = unsafe {
        GetMonitorRedGreenOrBlueGain(
            pm.hPhysicalMonitor,
            channel,
            &mut min,
            &mut current,
            &mut max,
        )
    };
    (ok != 0).then_some(Gain { min, current, max })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{self, from_kelvin, ramp_for};

    fn gain(min: u32, current: u32, max: u32) -> Gain {
        Gain { min, current, max }
    }

    /// The probe's second monitor, deliberately not neutral.
    fn calibrated() -> Gains {
        [gain(0, 100, 100), gain(0, 92, 100), gain(0, 96, 100)]
    }

    #[test]
    fn a_warm_tint_scales_the_captured_gain_not_the_maximum() {
        let c = from_kelvin(3200.0);
        let t = targets(&calibrated(), &c);
        assert_eq!(t[0], 100);
        assert_eq!(t[1], (92.0 * c.g).round() as u32);
        assert_eq!(t[2], (96.0 * c.b).round() as u32);
        assert!(t[2] < 96 && t[2] < t[1]);
    }

    /// Every fade step scales the same captured gain, so running 3200 K twice
    /// lands on the same numbers instead of going warmer each time.
    #[test]
    fn repeated_steps_do_not_compound() {
        let c = from_kelvin(3200.0);
        assert_eq!(targets(&calibrated(), &c), targets(&calibrated(), &c));
    }

    #[test]
    fn neutral_writes_the_captured_gain_back() {
        let base = [gain(0, 51, 100), gain(0, 45, 100), gain(0, 48, 100)];
        assert_eq!(targets(&base, &Rgb::NEUTRAL), currents(&base));
    }

    #[test]
    fn a_target_stays_inside_what_the_panel_accepts() {
        let base = [gain(20, 30, 40), gain(20, 30, 40), gain(20, 30, 40)];
        let t = targets(&base, &from_kelvin(1000.0));
        assert!(t.iter().all(|v| (20..=40).contains(v)), "{t:?}");
    }

    /// A panel reporting its range backwards must not panic the daemon.
    #[test]
    fn an_inverted_range_does_not_panic() {
        let base = [gain(50, 10, 5); 3];
        let _ = targets(&base, &from_kelvin(3200.0));
    }

    #[test]
    fn a_key_names_its_device() {
        let k = key(r"\\.\DISPLAY2", 1);
        assert_eq!(k, r"\\.\DISPLAY2#1");
        assert_eq!(device_of(&k), r"\\.\DISPLAY2");
    }

    #[test]
    fn gains_survive_a_round_trip() {
        let gains = vec![
            (key(r"\\.\DISPLAY1", 0), calibrated()),
            (key(r"\\.\DISPLAY3", 0), [gain(0, 50, 100); 3]),
        ];
        assert_eq!(decode(&encode(&gains)), gains);
    }

    /// Ramps and gains share one baseline string. Each decoder has to take its
    /// own entries and nothing else, or a restore writes a ramp to a panel.
    #[test]
    fn ramps_and_gains_share_a_baseline_without_mixing() {
        let ramp = ramp_for(&from_kelvin(5000.0));
        let gains = vec![(key(r"\\.\DISPLAY1", 0), calibrated())];
        let text = format!(
            "{};{}",
            color::encode(&[(r"\\.\DISPLAY1".to_owned(), ramp)]),
            encode(&gains)
        );
        let ramps = color::decode(&text);
        assert_eq!(ramps.len(), 1);
        assert_eq!(ramps[0].0, r"\\.\DISPLAY1");
        assert_eq!(decode(&text), gains);
    }

    #[test]
    fn a_damaged_panel_entry_is_skipped() {
        let good = encode(&[(key("a", 0), calibrated())]);
        let text = format!("{good};ddc:b#0=0/50/100,0/50;ddc:c#0=1/2/3/4,0/1/2,0/1/2");
        let decoded = decode(&text);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].0, "a#0");
    }

    /// A panel in memory. It records every set, can refuse them all, can
    /// lose the next few (accept and not apply), and can block until let go.
    #[derive(Clone, Default)]
    struct Fake(Arc<FakeState>);

    /// One call to [`Wire::set`]: the panel and its channels in order.
    type Set = (String, Vec<(usize, u32)>);

    #[derive(Default)]
    struct FakeState {
        held: Mutex<BTreeMap<String, [u32; 3]>>,
        sets: Mutex<Vec<Set>>,
        reads: Mutex<usize>,
        refuse: std::sync::atomic::AtomicBool,
        lose: Mutex<usize>,
        gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        delay: Mutex<Duration>,
    }

    impl Fake {
        fn holding(key: &str, values: [u32; 3]) -> Self {
            let fake = Fake::default();
            fake.0.held.lock().unwrap().insert(key.to_owned(), values);
            fake
        }

        fn held(&self, key: &str) -> Option<[u32; 3]> {
            self.0.held.lock().unwrap().get(key).copied()
        }

        fn sets(&self) -> Vec<Set> {
            self.0.sets.lock().unwrap().clone()
        }

        /// What each set ended on, as the panel would have shown it.
        fn written(&self) -> Vec<[u32; 3]> {
            let mut now = [0; 3];
            self.sets()
                .iter()
                .map(|(_, channels)| {
                    for &(i, v) in channels {
                        now[i] = v;
                    }
                    now
                })
                .collect()
        }
    }

    impl Wire for Fake {
        fn set(&self, key: &str, channels: &[(usize, u32)]) -> bool {
            self.0
                .sets
                .lock()
                .unwrap()
                .push((key.to_owned(), channels.to_vec()));
            if let Some(gate) = &*self.0.gate.lock().unwrap() {
                let _ = gate.recv();
            }
            std::thread::sleep(*self.0.delay.lock().unwrap());
            if self.0.refuse.load(std::sync::atomic::Ordering::SeqCst) {
                return false;
            }
            let mut lose = self.0.lose.lock().unwrap();
            if *lose > 0 {
                *lose -= 1;
                return true;
            }
            let mut held = self.0.held.lock().unwrap();
            let panel = held.entry(key.to_owned()).or_insert([0; 3]);
            for &(i, v) in channels {
                panel[i] = v;
            }
            true
        }

        fn get(&self, key: &str) -> Option<[u32; 3]> {
            *self.0.reads.lock().unwrap() += 1;
            self.held(key)
        }
    }

    /// Waits for the writer's thread to reach `done`, a few seconds at most.
    fn eventually(done: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(10), "never happened");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Going warm lowers blue before green, and going back raises green
    /// before blue, so no step on the way has blue above green.
    #[test]
    fn channels_go_out_in_an_order_that_is_never_pink() {
        assert_eq!(
            order(Some([100, 92, 96]), [100, 64, 41], false),
            [(2, 41), (1, 64)]
        );
        assert_eq!(
            order(Some([100, 64, 41]), [100, 92, 96], false),
            [(1, 92), (2, 96)]
        );
        // Mixed: what falls first, then what rises.
        assert_eq!(
            order(Some([90, 60, 50]), [100, 50, 40], false),
            [(2, 40), (1, 50), (0, 100)]
        );
        // Nothing known: every channel, warmest first.
        assert_eq!(order(None, [1, 2, 3], false), [(2, 3), (1, 2), (0, 1)]);
        // Sent whole, a channel already in place still goes out.
        assert_eq!(
            order(Some([100, 92, 41]), [100, 64, 41], true),
            [(1, 64), (0, 100), (2, 41)]
        );
    }

    #[test]
    fn a_slow_panel_skips_to_the_latest_gains() {
        let fake = Fake::default();
        let (open, gate) = std::sync::mpsc::channel::<()>();
        *fake.0.gate.lock().unwrap() = Some(gate);
        let writer = Writer::with(fake.clone());
        writer.post("p#0", [1, 1, 1]);
        eventually(|| !fake.sets().is_empty());
        // The panel is busy with the first step; the fade moves on twice.
        writer.post("p#0", [2, 2, 2]);
        writer.post("p#0", [3, 3, 3]);
        open.send(()).unwrap();
        open.send(()).unwrap();
        writer.settle();
        assert_eq!(fake.written(), [[1, 1, 1], [3, 3, 3]]);
        *fake.0.gate.lock().unwrap() = None;
    }

    #[test]
    fn posting_returns_before_the_panel_answers() {
        let fake = Fake::default();
        *fake.0.delay.lock().unwrap() = Duration::from_millis(300);
        let writer = Writer::with(fake);
        let start = std::time::Instant::now();
        writer.post("p#0", [1, 2, 3]);
        assert!(start.elapsed() < Duration::from_millis(100));
        writer.settle();
        assert!(start.elapsed() >= Duration::from_millis(300));
    }

    /// A fade step that moves blue alone sends blue alone.
    #[test]
    fn a_write_sends_only_what_moved() {
        let fake = Fake::holding("p#0", [100, 92, 96]);
        let writer = Writer::with(fake.clone());
        writer.read_back(&[("p#0".to_owned(), calibrated())]);
        writer.post("p#0", [100, 92, 80]);
        writer.settle();
        writer.post("p#0", [100, 90, 79]);
        writer.settle();
        let sent: Vec<Vec<(usize, u32)>> = fake.sets().into_iter().map(|s| s.1).collect();
        assert_eq!(sent, [vec![(2, 80)], vec![(2, 79), (1, 90)]]);
    }

    #[test]
    fn a_refusal_is_reported_once_and_its_panel_forgotten() {
        let fake = Fake::default();
        fake.0
            .refuse
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let writer = Writer::with(fake.clone());
        writer.read_back(&[("p#0".to_owned(), calibrated())]);
        writer.post("p#0", [1, 1, 1]);
        writer.settle();
        assert_eq!(writer.take_refused(), ["p#0"]);
        assert!(writer.take_refused().is_empty());
        // Nothing is assumed about a panel that refused: the next write sends
        // every channel.
        writer.post("p#0", [2, 2, 2]);
        writer.settle();
        assert_eq!(fake.sets()[1].1.len(), 3);
    }

    #[test]
    fn a_direct_write_goes_out_after_the_posted_ones() {
        let fake = Fake::default();
        *fake.0.delay.lock().unwrap() = Duration::from_millis(50);
        let writer = Writer::with(fake.clone());
        writer.post("p#0", [1, 1, 1]);
        writer.post("q#0", [2, 2, 2]);
        assert!(writer.write_now("p#0", [9, 9, 9]));
        let order: Vec<(String, [u32; 3])> = fake
            .sets()
            .iter()
            .map(|(k, c)| {
                let mut v = [0; 3];
                for &(i, x) in c {
                    v[i] = x;
                }
                (k.clone(), v)
            })
            .collect();
        assert_eq!(
            order,
            [
                ("p#0".to_owned(), [1, 1, 1]),
                ("q#0".to_owned(), [2, 2, 2]),
                ("p#0".to_owned(), [9, 9, 9])
            ]
        );
    }

    /// The panel that stayed pink: it took the way back and kept green where
    /// the night had put it. A direct write reads it back and sends it again.
    #[test]
    fn a_direct_write_that_is_lost_is_sent_again() {
        let fake = Fake::holding("p#0", [100, 64, 41]);
        *fake.0.lose.lock().unwrap() = 1;
        let writer = Writer::with(fake.clone());
        writer.read_back(&[(
            "p#0".to_owned(),
            [Gain {
                min: 0,
                current: 0,
                max: 100,
            }; 3],
        )]);
        assert!(writer.write_now("p#0", [100, 92, 96]));
        assert_eq!(fake.held("p#0"), Some([100, 92, 96]));
        assert_eq!(fake.sets().len(), 2);
        // Sent whole both times, whatever it was believed to hold.
        assert!(fake.sets().iter().all(|(_, c)| c.len() == 3));
    }

    #[test]
    fn a_direct_write_that_never_lands_is_a_failure() {
        let fake = Fake::holding("p#0", [100, 64, 41]);
        *fake.0.lose.lock().unwrap() = usize::MAX;
        let writer = Writer::with(fake.clone());
        assert!(!writer.write_now("p#0", [100, 92, 96]));
        assert_eq!(fake.sets().len(), MISSES as usize);
    }

    /// A posted write lost at the end of a fade is found once the panel has
    /// been quiet for a moment, and sent again.
    #[test]
    fn a_posted_write_that_is_lost_is_found_and_sent_again() {
        let fake = Fake::holding("p#0", [100, 92, 96]);
        let writer = Writer::with(fake.clone());
        writer.read_back(&[("p#0".to_owned(), calibrated())]);
        *fake.0.lose.lock().unwrap() = 1;
        writer.post("p#0", [100, 64, 41]);
        writer.settle();
        assert_eq!(fake.held("p#0"), Some([100, 92, 96]), "lost");
        eventually(|| fake.held("p#0") == Some([100, 64, 41]));
        assert!(writer.take_refused().is_empty());
    }

    /// A panel that keeps losing writes is given up on after a few reads,
    /// and its monitor goes back to its ramp.
    #[test]
    fn a_panel_that_keeps_losing_writes_counts_as_refusing() {
        let fake = Fake::holding("p#0", [100, 92, 96]);
        let writer = Writer::with(fake.clone());
        writer.read_back(&[("p#0".to_owned(), calibrated())]);
        *fake.0.lose.lock().unwrap() = usize::MAX;
        writer.post("p#0", [100, 64, 41]);
        eventually(|| !writer.lock().refused.is_empty());
        assert_eq!(writer.take_refused(), ["p#0"]);
        assert_eq!(fake.sets().len(), MISSES as usize);
    }

    /// Mid fade the panel is not read: each step pushes the read back.
    #[test]
    fn a_panel_still_fading_is_not_read() {
        let fake = Fake::holding("p#0", [100, 92, 96]);
        let writer = Writer::with(fake.clone());
        for b in (80..96).rev() {
            writer.post("p#0", [100, 92, b]);
            writer.settle();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(*fake.0.reads.lock().unwrap(), 0);
    }

    fn memory_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cortisoff-memory-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        path
    }

    /// The panel that stayed pink: the next run reads 100/64/41 off it and
    /// captures 100/92/96, what it had before the night began.
    #[test]
    fn a_remembered_panel_is_captured_at_its_own_gains() {
        let path = memory_file("remembered");
        let memory = Memory::at(Some(path.clone()), |k| k.to_owned());
        memory.hold("p#0", &calibrated());
        memory.hold("p#0", &[gain(0, 1, 100); 3]);
        let next_run = Memory::at(Some(path.clone()), |k| k.to_owned());
        let shown = vec![(
            "p#0".to_owned(),
            [gain(0, 100, 100), gain(0, 64, 100), gain(0, 41, 100)],
        )];
        assert_eq!(
            next_run.correct(shown),
            vec![("p#0".to_owned(), calibrated())]
        );
        // Back and verified: the file goes, and a panel is read as it is.
        next_run.forget("p#0");
        assert!(!path.exists());
        let after = Memory::at(Some(path), |k| k.to_owned());
        let shown = vec![("p#0".to_owned(), [gain(0, 50, 100); 3])];
        assert_eq!(after.correct(shown.clone()), shown);
    }
}
