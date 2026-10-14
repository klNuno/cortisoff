//! The host: one `poll(2)` over the X connection, the wake pipe and two
//! timerfds, and one queue for every timer the engine and the host want.
//!
//! Nothing here polls on a beat for something that has an event. Focus and
//! window state are property changes, monitors are RandR notifications,
//! hotkeys are key events on a grab, and the end of an idle period is the
//! first raw input event. What has no event here, the power source and the
//! sound card, is read on a thirty second timer with fifteen seconds of
//! slack, only while a plugin reads it.
//!
//! One host for X11 and Wayland, the backends picked at runtime. Under a
//! Wayland session the D-Bus services stand in where XWayland cannot see:
//! logind for the lock, Mutter's idle monitor for idle, GNOME's Night Light
//! for the colour, the desktop portal for global shortcuts. Their signals
//! arrive on threads (`dbus.rs`) that wake the same poll through the wake
//! pipe.

use std::os::fd::AsFd;
use std::path::Path;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::Duration;

use cortisoff_host::{
    ids, Actuator, ActuatorError, CapabilityId, CapabilitySet, Chord, Event, Host, Hotkey, TimerId,
    WindowAction, WindowInfo,
};
use rustix::event::{poll, PollFd, PollFlags};
use rustix::time::Timespec;
use x11rb::connection::Connection;
use x11rb::protocol::randr::{ConnectionExt as _, NotifyMask};
use x11rb::protocol::xinput::{self, ConnectionExt as _, XIEventMask};
use x11rb::protocol::xproto::{EventMask, Mapping, Window};
use x11rb::protocol::Event as XEvent;

use crate::clipboard::{self, Owner};
use crate::clock;
use crate::dbus::{self, Buses, IdleMonitor, Logind, Signal};
use crate::gamma::ColorTemp;
use crate::input::{self, Hotkeys};
use crate::keymap::Keymap;
use crate::mode::DisplayMode;
use crate::nightlight::NightLight;
use crate::procfs;
use crate::shortcuts::Shortcuts;
use crate::timers::{Key, Timers};
use crate::wake::{Alarm, ClockWatch, WakePipe};
use crate::wm;
use crate::x11::{Display, State};

/// When a display change is pushed again, counted from the notification.
/// The same reason as on Windows: a ramp written while the driver is still
/// reprogramming an output is accepted and then dropped.
const SETTLE: [Duration; 3] = [
    Duration::from_millis(400),
    Duration::from_millis(1_500),
    Duration::from_millis(4_000),
];

/// How long without input counts as away, as on Windows.
const DEFAULT_IDLE_AFTER: u32 = 5 * 60;

/// The beat for what has no event here: the power source and the sound
/// card. Half of it is slack, so it rides along with other wakeups.
const SOURCE_EVERY: Duration = Duration::from_secs(30);
const SOURCE_SLACK: Duration = Duration::from_secs(15);

/// A threshold is a promise to the user, so its timer has little slack.
const THRESHOLD_SLACK_MS: u64 = 500;
/// While a gate holds idle at zero, or the user is away, the gates are read
/// again this often: sound starting or a film going fullscreen ends an
/// idle period with no input at all.
const RECHECK_MS: u64 = 15_000;
const RECHECK_SLACK_MS: u64 = 5_000;
/// Without XInput 2.1 nothing announces input, so the end of an idle period
/// is found by looking every second, which is what Windows does.
const RESUME_MS: u64 = 1_000;
const RESUME_SLACK_MS: u64 = 100;

/// The boot clock gaining this much on the monotonic one between two polls
/// means the machine slept.
const SLEPT_MS: u64 = 1_000;

/// How far ahead the next daylight saving edge is looked for. A year, so a
/// zone whose summer lasts more than six months still finds its autumn edge.
const DST_HORIZON_DAYS: u32 = 370;
const DAY_S: i64 = 86_400;

const NO_KEYMAP: &str = "the X server would not send its keyboard mapping";

const WAYLAND_FOCUS: &str = "this is a Wayland session, where no protocol tells a client which window has the focus or whether it fills the screen. GNOME keeps that to the shell, and XWayland only sees X11 windows, so its answer would be wrong for every native one";
const WAYLAND_INPUT: &str = "this is a Wayland session, where an application cannot press keys in another one: keys sent through XWayland only reach X11 windows. The desktop's RemoteDesktop portal would allow it, after asking the user, and this host does not speak it yet";

/// A Wayland session: XWayland stands in for X, or there is no X at all and
/// the compositor announced itself. Keys and shortcuts then belong to the
/// compositor, not to any X server.
fn wayland_session(x: Option<&Display>) -> bool {
    match x {
        Some(x) => x.xwayland,
        None => std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()),
    }
}

/// Which of the two colour backends this session has.
enum Colour {
    Gamma(ColorTemp),
    Gnome(NightLight),
}

impl Colour {
    fn actuator(&self) -> &dyn Actuator {
        match self {
            Self::Gamma(a) => a,
            Self::Gnome(a) => a,
        }
    }

    fn backend_name(&self) -> &'static str {
        match self {
            Self::Gamma(a) => a.backend_name(),
            Self::Gnome(a) => a.backend_name(),
        }
    }
}

/// Where the idle time comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleWay {
    /// MIT-SCREEN-SAVER on a real X server, XInput for the return.
    X11,
    /// Mutter's idle monitor: a watch per threshold and one for the return.
    Mutter,
    None,
}

pub struct LinuxHost {
    x: Option<Rc<Display>>,
    /// Why there is no X display, or why the one there was went away.
    no_x: String,
    wake: WakePipe,
    alarm: Option<Alarm>,
    clock_watch: Option<ClockWatch>,
    timers: Timers,
    /// Boot clock minus monotonic clock at the last look. It grows by
    /// however long the machine slept.
    skew: u64,
    /// Events a later `install_sources` owes the engine: a source switched
    /// on after the snapshot starts from the world as it is now.
    pending: Vec<Event>,
    watch_focus: bool,
    watch_state: bool,
    /// The window whose `_NET_WM_STATE` this client listens to.
    watched: Option<Window>,
    active: Option<Window>,
    idle_watched: bool,
    idle_thresholds: Vec<u32>,
    idle: bool,
    idle_seconds: u32,
    /// Raw input is selected on the root, waiting for the first key or move
    /// to end an idle period.
    listening: bool,
    fullscreen: bool,
    maximized: bool,
    power_watched: bool,
    on_battery: bool,
    audio_watched: bool,
    audio_active: bool,
    /// How far through [`SETTLE`] the current display change is.
    settling: Option<usize>,
    buses: Buses,
    logind: Result<Logind, String>,
    locked: bool,
    mutter: Result<IdleMonitor, String>,
    color_temp: Result<Colour, String>,
    display_mode: Result<DisplayMode, String>,
    keymap: Option<Keymap>,
    hotkeys: Hotkeys,
    /// Global shortcuts through the desktop portal, for a Wayland session.
    /// `Err` holds why there are none.
    shortcuts: Result<Shortcuts, String>,
    /// The last key release on a grab. X sends an auto-repeat as a release
    /// and a press with the same timestamp, and a held hotkey must fire once.
    last_release: Option<(u8, u32)>,
    clipboard: Option<Owner>,
    snapshot_sent: bool,
}

impl LinuxHost {
    /// Fails only when the process cannot make a pipe. No X display is not
    /// a failure: the host then serves timers and the waker and the probe
    /// says why everything else is missing.
    pub fn new() -> Result<Self, String> {
        let wake = WakePipe::new()?;
        let (x, no_x) = match Display::connect() {
            Ok(display) => (Some(Rc::new(display)), String::new()),
            Err(reason) => (None, reason),
        };
        let buses = Buses::connect(wake.waker());
        // A real X server keeps its RandR ramps. Under XWayland, or with no
        // X at all, GNOME's Night Light is the one colour the compositor
        // shows.
        let color_temp = match &x {
            Some(d) => ColorTemp::new(Rc::clone(d)).map(Colour::Gamma),
            None => Err(no_x.clone()),
        }
        .or_else(|x_why| {
            NightLight::new(&buses)
                .map(Colour::Gnome)
                .map_err(|gnome_why| format!("{x_why}; and {gnome_why}"))
        });
        let x11_idle = x.as_ref().is_some_and(|d| d.screensaver && !d.xwayland);
        let mutter = if x11_idle {
            Err("the X server answers for idle".to_owned())
        } else {
            IdleMonitor::new(&buses)
        };
        let logind = Logind::new(&buses);
        let locked = logind
            .as_ref()
            .ok()
            .and_then(|l| l.locked().ok())
            .unwrap_or(false);
        // Without timedated a zone change still reaches the next clock event.
        let _ = dbus::watch_zone(&buses);
        let display_mode = match &x {
            Some(d) => DisplayMode::new(Rc::clone(d)),
            None => Err(no_x.clone()),
        };
        let keymap = x.as_deref().and_then(input::load_keymap);
        let shortcuts = if wayland_session(x.as_deref()) {
            Shortcuts::new(&buses)
        } else {
            Err("an X11 session grabs its keys".to_owned())
        };
        if let Some(d) = &x {
            // Always on, as `WM_DISPLAYCHANGE` is on Windows: the colour
            // tint needs to hear about a monitor change whatever the plugins
            // read.
            if d.randr {
                let _ = d.conn.randr_select_input(
                    d.root,
                    NotifyMask::SCREEN_CHANGE | NotifyMask::CRTC_CHANGE | NotifyMask::OUTPUT_CHANGE,
                );
            }
            d.flush();
        }

        let mut host = Self {
            x,
            no_x,
            wake,
            alarm: Alarm::new(),
            clock_watch: ClockWatch::new(),
            timers: Timers::default(),
            skew: clock::boot_ms().saturating_sub(clock::mono_ms()),
            pending: Vec::new(),
            watch_focus: false,
            watch_state: false,
            watched: None,
            active: None,
            idle_watched: false,
            idle_thresholds: vec![DEFAULT_IDLE_AFTER],
            idle: false,
            idle_seconds: 0,
            listening: false,
            fullscreen: false,
            maximized: false,
            power_watched: false,
            on_battery: false,
            audio_watched: false,
            audio_active: false,
            settling: None,
            buses,
            logind,
            locked,
            mutter,
            color_temp,
            display_mode,
            keymap,
            hotkeys: Hotkeys::default(),
            shortcuts,
            last_release: None,
            clipboard: None,
            snapshot_sent: false,
        };
        host.arm_dst();
        Ok(host)
    }

    /// Machine facts the user should know that are not failures.
    fn machine_warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        match &self.x {
            None => out.push(format!(
                "{}. Only timers, notifications, the power source and the sound card work here",
                self.no_x
            )),
            Some(x) if x.xwayland => out.push(
                "this session is Wayland: the clipboard and the window list go through XWayland, which sees X11 applications only (the clipboard is shared with Wayland ones), no window is reported as focused, text cannot be typed or keys sent into another application, and global shortcuts go through the desktop portal, which asks the user to allow them"
                    .to_owned(),
            ),
            Some(_) => {}
        }
        if Path::new(procfs::ASOUND).is_dir() {
            out.push(
                "audio.active reads the ALSA sound cards: PulseAudio and PipeWire hold a card open a few seconds after the sound stops, and Bluetooth headphones are not an ALSA card, so sound playing there is not seen"
                    .to_owned(),
            );
        }
        out
    }

    fn display(&self) -> Result<Rc<Display>, String> {
        self.x.clone().ok_or_else(|| self.no_x.clone())
    }

    fn idle_way(&self) -> IdleWay {
        if self
            .x
            .as_ref()
            .is_some_and(|x| x.screensaver && !x.xwayland)
        {
            IdleWay::X11
        } else if self.mutter.is_ok() {
            IdleWay::Mutter
        } else {
            IdleWay::None
        }
    }

    fn idle_supported(&self) -> bool {
        self.idle_way() != IdleWay::None
    }

    /// The X server is real, not XWayland: its focus and window state are
    /// the truth.
    fn x11_session(&self) -> bool {
        self.x.as_ref().is_some_and(|x| !x.xwayland)
    }

    /// The X server is gone: the session ended or the server crashed. The
    /// host keeps serving timers, and stops reading a socket that would
    /// report the same error on every poll.
    fn lose_x(&mut self, reason: String) {
        self.x = None;
        // GNOME's Night Light does not go through X and stays.
        if matches!(self.color_temp, Ok(Colour::Gamma(_))) {
            self.color_temp = Err(reason.clone());
        }
        self.display_mode = Err(reason.clone());
        self.no_x = reason;
        self.clipboard = None;
        self.keymap = None;
        self.hotkeys = Hotkeys::default();
        self.watched = None;
        self.active = None;
        self.listening = false;
        self.watch_focus = false;
        self.watch_state = false;
        self.timers.cancel(Key::Idle);
        self.timers.cancel(Key::Settle);
        self.settling = None;
    }

    fn arm_dst(&mut self) {
        clock::refresh_zone();
        let now = clock::wall_s();
        match clock::next_offset_change(now, DAY_S, DST_HORIZON_DAYS, clock::utc_offset) {
            // A second past the edge, so the clock pushed there already
            // reads the new offset.
            Some(edge) => {
                let after = Duration::from_secs((edge - now).max(0) as u64 + 1);
                self.timers
                    .set(Key::Clock, clock::boot_ms(), after, Duration::from_secs(1));
            }
            None => self.timers.cancel(Key::Clock),
        }
    }

    fn arm_settle(&mut self, step: usize) {
        match SETTLE.get(step) {
            // No slack: the point is to land while the driver settles.
            Some(after) => {
                self.settling = Some(step);
                self.timers
                    .set(Key::Settle, clock::boot_ms(), *after, Duration::ZERO);
            }
            None => {
                self.settling = None;
                self.timers.cancel(Key::Settle);
            }
        }
    }

    fn state_of(&self, window: Option<Window>) -> State {
        match (&self.x, window) {
            (Some(x), Some(w)) => x.window_state(w),
            _ => State::default(),
        }
    }

    /// Listen to `_NET_WM_STATE` on the focused window, and only on it.
    /// Selecting before reading the state is what makes the pair race free.
    fn follow(&mut self) {
        let Some(x) = self.x.clone() else {
            return;
        };
        let want = if self.watch_state { self.active } else { None };
        if want == self.watched {
            return;
        }
        if let Some(old) = self.watched {
            x.select(old, EventMask::NO_EVENT);
        }
        if let Some(new) = want {
            x.select(new, EventMask::PROPERTY_CHANGE);
        }
        self.watched = want;
    }

    /// The focus moved, or a source starting needs to hear where it is.
    fn focus_events(&mut self, window: Option<Window>, force: bool) -> Vec<Event> {
        let mut out = Vec::new();
        let moved = window != self.active;
        self.active = window;
        if moved || force {
            let app = match (&self.x, window) {
                (Some(x), Some(w)) => x.app(w),
                _ => None,
            };
            out.extend(app.map(Event::AppFocus));
        }
        self.follow();
        out.extend(self.state_events(force));
        out
    }

    /// Fullscreen and maximised for the focused window, pushed on a change.
    fn state_events(&mut self, force: bool) -> Vec<Event> {
        let state = self.state_of(self.active);
        let mut out = Vec::new();
        if force || state.fullscreen != self.fullscreen {
            self.fullscreen = state.fullscreen;
            out.push(Event::Fullscreen {
                exclusive: state.fullscreen,
            });
            // Fullscreen holds idle at zero, so its edge moves idle too.
            if self.idle_watched {
                out.extend(self.idle_changed());
            }
        }
        if force || state.maximized != self.maximized {
            self.maximized = state.maximized;
            out.push(Event::Maximized {
                maximized: state.maximized,
            });
        }
        out
    }

    /// Milliseconds since the last input, and whether a film or sound holds
    /// idle at zero: a mouse that has not moved during a film is not an
    /// absent user.
    fn idle_reading(&self) -> (u64, bool) {
        if self.idle_way() == IdleWay::Mutter {
            let raw = self
                .mutter
                .as_ref()
                .ok()
                .and_then(IdleMonitor::idle_ms)
                .unwrap_or(0);
            // No fullscreen to read on Wayland: sound is the one gate.
            let gated = raw >= 1_000 && procfs::playing(Path::new(procfs::ASOUND));
            return (raw, gated);
        }
        let Some(x) = self.x.as_ref().filter(|_| self.idle_way() == IdleWay::X11) else {
            return (0, false);
        };
        let raw = x.idle_ms().map_or(0, u64::from);
        if raw < 1_000 {
            return (raw, false);
        }
        let fullscreen = x
            .active_window()
            .is_some_and(|w| x.window_state(w).fullscreen);
        (
            raw,
            fullscreen || procfs::playing(Path::new(procfs::ASOUND)),
        )
    }

    fn sample_idle(&mut self) -> Event {
        let (raw, gated) = self.idle_reading();
        let away_ms = if gated { 0 } else { raw };
        let seconds = (away_ms / 1000).min(u64::from(u32::MAX)) as u32;
        self.idle = seconds >= DEFAULT_IDLE_AFTER;
        self.idle_seconds = idle_bucket(seconds, &self.idle_thresholds);
        if self.idle_watched && self.idle_supported() {
            let away = self.idle_seconds > 0;
            let raw_input = self.listen_for_input(away);
            let plan = if self.idle_way() == IdleWay::Mutter {
                // Mutter's watches announce each threshold and the return;
                // only the gates are looked at again.
                (gated || away).then_some((
                    Duration::from_millis(RECHECK_MS),
                    Duration::from_millis(RECHECK_SLACK_MS),
                ))
            } else {
                idle_check(away_ms, gated, raw_input, &self.idle_thresholds)
            };
            match plan {
                Some((after, slack)) => self.timers.set(Key::Idle, clock::boot_ms(), after, slack),
                None => self.timers.cancel(Key::Idle),
            }
        }
        Event::Idle {
            idle: self.idle,
            seconds: self.idle_seconds,
        }
    }

    fn idle_changed(&mut self) -> Option<Event> {
        let before = (self.idle, self.idle_seconds);
        let event = self.sample_idle();
        (before != (self.idle, self.idle_seconds)).then_some(event)
    }

    /// Select raw key, button and motion events on the root while away, and
    /// nothing otherwise: a moving mouse is hundreds of events a second, and
    /// only the first one after an absence means anything. Returns whether
    /// raw input is what will announce the user's return.
    fn listen_for_input(&mut self, on: bool) -> bool {
        if self.idle_way() == IdleWay::Mutter {
            return self.mutter.as_mut().is_ok_and(|m| m.listen_for_return(on));
        }
        let Some(x) = self.x.clone().filter(|x| x.xinput) else {
            return false;
        };
        if on != self.listening {
            let mask = if on {
                XIEventMask::RAW_KEY_PRESS | XIEventMask::RAW_BUTTON_PRESS | XIEventMask::RAW_MOTION
            } else {
                XIEventMask::from(0u32)
            };
            let sent = x
                .conn
                .xinput_xi_select_events(
                    x.root,
                    &[xinput::EventMask {
                        deviceid: u16::from(xinput::Device::ALL_MASTER),
                        mask: vec![mask],
                    }],
                )
                .is_ok();
            self.listening = on && sent;
        }
        self.listening
    }

    fn read_power() -> bool {
        procfs::supplies(Path::new(procfs::POWER_SUPPLIES)).is_some_and(|s| procfs::on_battery(&s))
    }

    fn read_audio() -> bool {
        procfs::playing(Path::new(procfs::ASOUND))
    }

    fn power_changed(&mut self) -> Option<Event> {
        let on_battery = Self::read_power();
        (on_battery != self.on_battery).then(|| {
            self.on_battery = on_battery;
            Event::PowerSource { on_battery }
        })
    }

    fn audio_changed(&mut self) -> Option<Event> {
        let active = Self::read_audio();
        (active != self.audio_active).then(|| {
            self.audio_active = active;
            Event::AudioActive { active }
        })
    }

    /// Everything a plugin could already be told about, before any real event.
    fn snapshot(&mut self) -> Vec<Event> {
        self.pending.clear();
        let mut out: Vec<Event> = clock::time_of_day().into_iter().collect();
        let active = self.x.as_ref().and_then(|x| x.active_window());
        self.active = active;
        self.follow();
        let state = self.state_of(active);
        self.fullscreen = state.fullscreen;
        self.maximized = state.maximized;
        out.push(Event::Fullscreen {
            exclusive: self.fullscreen,
        });
        out.push(self.sample_idle());
        out.push(Event::SessionLock {
            locked: self.locked,
        });
        self.on_battery = Self::read_power();
        out.push(Event::PowerSource {
            on_battery: self.on_battery,
        });
        self.audio_active = Self::read_audio();
        out.push(Event::AudioActive {
            active: self.audio_active,
        });
        out.push(Event::Maximized {
            maximized: self.maximized,
        });
        let app = match (&self.x, active) {
            (Some(x), Some(w)) => x.app(w),
            _ => None,
        };
        out.extend(app.map(Event::AppFocus));
        out
    }

    fn reload_keymap(&mut self, x: &Display) {
        let Some(new) = input::load_keymap(x) else {
            return;
        };
        // Typing a character the layout lacks borrows a keycode and gives it
        // back, which arrives here as two changes ending where they started.
        if self.keymap.as_ref() == Some(&new) {
            return;
        }
        let old = self.keymap.replace(new);
        if let (Some(old), Some(new)) = (old, &self.keymap) {
            for (id, why) in self.hotkeys.regrab(x, &old, new) {
                eprintln!("warning: hotkey {id} is lost after a keyboard layout change: {why}");
            }
        }
    }

    fn hotkey(&mut self, keycode: u8, state: u16, time: u32) -> Option<u32> {
        let repeat = self.last_release.take() == Some((keycode, time));
        if repeat {
            return None;
        }
        self.hotkeys.pressed(self.keymap.as_ref()?, keycode, state)
    }

    fn drain_x(&mut self, out: &mut Vec<Event>) {
        let Some(x) = self.x.clone() else {
            return;
        };
        let (mut focus, mut state, mut display, mut remap, mut input) =
            (false, false, false, false, false);
        loop {
            let event = match x.conn.poll_for_event() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(e) => {
                    self.lose_x(format!("the X server closed the connection: {e}"));
                    return;
                }
            };
            match event {
                XEvent::PropertyNotify(p)
                    if p.window == x.root && p.atom == x.atoms._NET_ACTIVE_WINDOW =>
                {
                    focus = true;
                }
                XEvent::PropertyNotify(p)
                    if Some(p.window) == self.watched && p.atom == x.atoms._NET_WM_STATE =>
                {
                    state = true;
                }
                XEvent::RandrScreenChangeNotify(_) | XEvent::RandrNotify(_) => display = true,
                XEvent::KeyPress(k) => {
                    if let Some(id) = self.hotkey(k.detail, u16::from(k.state), k.time) {
                        out.push(Event::Hotkey { id });
                    }
                }
                XEvent::KeyRelease(k) => self.last_release = Some((k.detail, k.time)),
                XEvent::MappingNotify(m) if m.request != Mapping::POINTER => remap = true,
                XEvent::SelectionRequest(r) => {
                    if let Some(owner) = &self.clipboard {
                        owner.answer(&x, &r);
                    }
                }
                XEvent::SelectionClear(c) => {
                    if let Some(owner) = &mut self.clipboard {
                        owner.lost(&x, &c);
                    }
                }
                XEvent::XinputRawKeyPress(_)
                | XEvent::XinputRawButtonPress(_)
                | XEvent::XinputRawMotion(_) => input = true,
                // Errors are answers to requests that raced a window going
                // away; nothing waits on them.
                _ => {}
            }
        }
        if remap {
            self.reload_keymap(&x);
        }
        if focus && self.watch_focus {
            let window = x.active_window();
            out.extend(self.focus_events(window, false));
        } else if state {
            out.extend(self.state_events(false));
        }
        // Answered now and again as the configuration settles.
        if display {
            self.arm_settle(0);
            out.push(Event::DisplayConfigChanged);
        }
        if input && self.listening {
            out.extend(self.idle_changed());
        }
    }

    fn fire_due(&mut self, out: &mut Vec<Event>) {
        for key in self.timers.take_due(clock::boot_ms()) {
            match key {
                // A schedule edge or a fade step: the time for schedules,
                // the id for previews, as on Windows.
                Key::Engine(id) => {
                    out.extend(clock::time_of_day());
                    out.push(Event::TimerFired { id: TimerId(id) });
                }
                Key::Idle => out.extend(self.idle_changed()),
                Key::Power => {
                    self.timers
                        .set(Key::Power, clock::boot_ms(), SOURCE_EVERY, SOURCE_SLACK);
                    out.extend(self.power_changed());
                }
                Key::Audio => {
                    self.timers
                        .set(Key::Audio, clock::boot_ms(), SOURCE_EVERY, SOURCE_SLACK);
                    if let Some(event) = self.audio_changed() {
                        out.push(event);
                        if self.idle_watched {
                            out.extend(self.idle_changed());
                        }
                    }
                }
                Key::Settle => {
                    let next = self.settling.map_or(SETTLE.len(), |step| step + 1);
                    self.arm_settle(next);
                    out.push(Event::DisplayConfigChanged);
                }
                Key::Clock => {
                    out.extend(clock::time_of_day());
                    self.arm_dst();
                }
            }
        }
    }

    /// True once per suspend.
    fn slept(&mut self) -> bool {
        let skew = clock::boot_ms().saturating_sub(clock::mono_ms());
        let slept = skew >= self.skew.saturating_add(SLEPT_MS);
        self.skew = skew;
        slept
    }

    /// What the D-Bus threads heard since the last look.
    fn drain_dbus(&mut self, out: &mut Vec<Event>) {
        for signal in self.buses.inbox.drain() {
            match signal {
                Signal::Locked(locked) => {
                    if locked != self.locked {
                        self.locked = locked;
                        out.push(Event::SessionLock { locked });
                    }
                }
                Signal::IdleWatch(id) => {
                    let ours = self.mutter.as_mut().is_ok_and(|m| m.fired(id));
                    if ours && self.idle_watched {
                        out.extend(self.idle_changed());
                    }
                }
                Signal::Zone => {
                    out.extend(clock::time_of_day());
                    self.arm_dst();
                }
                Signal::Shortcut(name) => {
                    if let Some(id) = self.shortcuts.as_ref().ok().and_then(|s| s.hotkey(&name)) {
                        out.push(Event::Hotkey { id });
                    }
                }
                Signal::Bound(generation, outcome) => {
                    if let Ok(portal) = &mut self.shortcuts {
                        for (id, why) in portal.bound_late(generation, outcome) {
                            eprintln!("warning: hotkey {id} is not bound: {why}");
                        }
                    }
                }
            }
        }
    }

    /// Mutter's watches follow the thresholds while a plugin reads idle.
    fn sync_idle_watches(&mut self) {
        let on = self.idle_watched && self.idle_way() == IdleWay::Mutter;
        let thresholds = self.idle_thresholds.clone();
        if let Ok(m) = &mut self.mutter {
            if on {
                m.watch(&thresholds);
            } else {
                m.unwatch();
            }
        }
    }

    /// Everything ready without blocking. Returns whether another thread
    /// woke the host, which must end the poll even with no event.
    fn gather(&mut self, out: &mut Vec<Event>) -> bool {
        let woken = self.wake.drain();
        self.drain_dbus(out);
        if let Some(alarm) = &self.alarm {
            alarm.drain();
        }
        let moved = self.clock_watch.as_ref().is_some_and(ClockWatch::moved);
        if self.slept() || moved {
            // The engine armed its schedule from a time that is no longer
            // true, and a laptop may have been unplugged while asleep.
            out.extend(clock::time_of_day());
            self.arm_dst();
            if self.power_watched {
                self.timers
                    .set(Key::Power, clock::boot_ms(), Duration::ZERO, Duration::ZERO);
            }
        }
        self.drain_x(out);
        self.fire_due(out);
        woken
    }

    fn wait(&mut self, timeout: Duration) {
        let now = clock::boot_ms();
        let limit = now.saturating_add(millis(timeout));
        let until = self.timers.wake_at().map_or(limit, |t| t.min(limit));
        let left = until.saturating_sub(now);
        if left == 0 {
            return;
        }
        // A millisecond late: `now` was truncated to the millisecond, so an
        // alarm at `until` itself could end the wait before the deadline.
        if let Some(alarm) = &self.alarm {
            alarm.arm(Some(until.saturating_add(1)));
        }
        if let Some(x) = &self.x {
            x.flush();
        }
        let wake = self.wake.fd();
        let alarm = self.alarm.as_ref().map(Alarm::fd);
        let watch = self.clock_watch.as_ref().map(ClockWatch::fd);
        let xfd = self.x.as_ref().map(|x| x.conn.stream().as_fd());
        let mut fds: Vec<PollFd<'_>> = [Some(&wake), alarm.as_ref(), watch.as_ref(), xfd.as_ref()]
            .into_iter()
            .flatten()
            .map(|fd| PollFd::new(fd, PollFlags::IN))
            .collect();
        let timeout = Timespec {
            tv_sec: (left / 1000) as i64,
            tv_nsec: ((left % 1000) * 1_000_000) as _,
        };
        // An interrupted wait is a wakeup like any other: the caller looks
        // at everything again.
        let _ = poll(&mut fds, Some(&timeout));
    }

    fn refuse_all(wanted: &[Hotkey], why: &str) -> Vec<(u32, String)> {
        wanted.iter().map(|h| (h.id, why.to_owned())).collect()
    }
}

impl Host for LinuxHost {
    fn warnings(&self) -> Vec<String> {
        self.machine_warnings()
    }

    fn probe(&self) -> CapabilitySet {
        let id = CapabilityId::new;
        let mut set = CapabilitySet::new();
        match &self.x {
            Some(x) => {
                let ewmh = x.supports(x.atoms._NET_ACTIVE_WINDOW);
                for cap in [ids::APP_FOCUS, ids::APP_MAXIMIZED, ids::APP_FULLSCREEN] {
                    set = if x.xwayland {
                        set.missing(id(cap), WAYLAND_FOCUS)
                    } else if ewmh {
                        set.available(id(cap), Some("ewmh"))
                    } else {
                        set.missing(id(cap), "the window manager does not publish _NET_ACTIVE_WINDOW, which is how an X11 client learns the focused window")
                    };
                }
                set = match (self.idle_way(), &self.mutter) {
                    (IdleWay::X11, _) => set.available(id(ids::USER_IDLE), Some("mit-screen-saver")),
                    (IdleWay::Mutter, _) => set.available(id(ids::USER_IDLE), Some("mutter-idle-monitor")),
                    (IdleWay::None, Err(why)) if x.xwayland => set.missing(id(ids::USER_IDLE), format!("XWayland only hears input sent to X11 windows, so someone typing in a Wayland window would read as away; and {why}")),
                    (IdleWay::None, Err(why)) => set.missing(id(ids::USER_IDLE), format!("the X server has no MIT-SCREEN-SAVER extension, which is how the time since the last input is read; and {why}")),
                    (IdleWay::None, Ok(_)) => unreachable!("a working idle monitor is an idle way"),
                };
                set = if x.randr {
                    set.available(id(ids::DISPLAY_CONFIG), Some("randr"))
                } else {
                    set.missing(
                        id(ids::DISPLAY_CONFIG),
                        "the X server has no RandR 1.2, which is what announces a monitor change",
                    )
                };
            }
            None => {
                for cap in [
                    ids::APP_FOCUS,
                    ids::APP_MAXIMIZED,
                    ids::APP_FULLSCREEN,
                    ids::DISPLAY_CONFIG,
                ] {
                    set = set.missing(id(cap), self.no_x.clone());
                }
                set = match &self.mutter {
                    Ok(_) => set.available(id(ids::USER_IDLE), Some("mutter-idle-monitor")),
                    Err(why) => {
                        set.missing(id(ids::USER_IDLE), format!("{}; and {why}", self.no_x))
                    }
                };
            }
        }
        set = match &self.logind {
            Ok(_) => set.available(id(ids::SESSION_LOCK), Some("logind")),
            Err(why) => set.missing(id(ids::SESSION_LOCK), why.clone()),
        };
        set = if Path::new(procfs::POWER_SUPPLIES).is_dir() {
            set.available(id(ids::POWER_SOURCE), Some("sysfs"))
        } else {
            set.missing(
                id(ids::POWER_SOURCE),
                "/sys/class/power_supply is absent, so the power source cannot be read",
            )
        };
        set = if Path::new(procfs::ASOUND).is_dir() {
            set.available(id(ids::AUDIO_ACTIVE), Some("alsa-proc"))
        } else {
            set.missing(
                id(ids::AUDIO_ACTIVE),
                "/proc/asound is absent: no ALSA sound card, so playback cannot be seen",
            )
        };
        set = match &self.color_temp {
            Ok(a) => set.available(id(ids::DISPLAY_COLOR_TEMP), Some(a.backend_name())),
            Err(why) => set.missing(id(ids::DISPLAY_COLOR_TEMP), why.clone()),
        };
        set = match &self.display_mode {
            Ok(a) => set.available(id(ids::DISPLAY_MODE), Some(a.backend_name())),
            Err(why) => set.missing(id(ids::DISPLAY_MODE), why.clone()),
        };
        set.missing(
            id(ids::DISPLAY_VIBRANCE),
            "no Linux backend writes digital vibrance yet: NVIDIA's goes through NV-CONTROL, AMD's is a kernel display property",
        )
        .missing(
            id(ids::WALLPAPER_PLAYBACK),
            "no Linux wallpaper player is driven yet: Wallpaper Engine and Lively are Windows applications",
        )
    }

    fn set_idle_thresholds(&mut self, seconds: &[u32]) {
        self.idle_thresholds = seconds.to_vec();
        self.idle_thresholds.push(DEFAULT_IDLE_AFTER);
        self.idle_thresholds.sort_unstable();
        self.idle_thresholds.dedup();
        self.sync_idle_watches();
        if self.idle_watched {
            let changed = self.idle_changed();
            if self.snapshot_sent {
                self.pending.extend(changed);
            }
        }
    }

    /// Only what some plugin watches: a root property listener wakes the
    /// process on every focus change and every window opening.
    fn install_sources(&mut self, wanted: &[CapabilityId]) -> Result<(), ActuatorError> {
        let asked = |id: &str| wanted.iter().any(|c| c.as_str() == id);
        let after_snapshot = self.snapshot_sent;
        let mut owed = Vec::new();

        if let Some(x) = self.x.clone() {
            // XWayland's focus is not the session's: see `WAYLAND_FOCUS`.
            let ewmh = !x.xwayland && x.supports(x.atoms._NET_ACTIVE_WINDOW);
            let focus = ewmh
                && (asked(ids::APP_FOCUS)
                    || asked(ids::APP_MAXIMIZED)
                    || asked(ids::APP_FULLSCREEN));
            let state = ewmh && (asked(ids::APP_MAXIMIZED) || asked(ids::APP_FULLSCREEN));
            let started = focus && !self.watch_focus;
            self.watch_focus = focus;
            self.watch_state = state;
            x.select(
                x.root,
                if focus {
                    EventMask::PROPERTY_CHANGE
                } else {
                    EventMask::NO_EVENT
                },
            );
            if started {
                // The focus may have moved while nobody listened.
                let window = x.active_window();
                owed.extend(self.focus_events(window, true));
            } else {
                self.follow();
            }
        }

        let idle = asked(ids::USER_IDLE) && self.idle_supported();
        if idle != self.idle_watched {
            self.idle_watched = idle;
            self.sync_idle_watches();
            if idle {
                owed.extend(self.idle_changed());
            } else {
                self.timers.cancel(Key::Idle);
                self.listen_for_input(false);
            }
        }

        let power = asked(ids::POWER_SOURCE);
        if power != self.power_watched {
            self.power_watched = power;
            if power {
                self.timers
                    .set(Key::Power, clock::boot_ms(), SOURCE_EVERY, SOURCE_SLACK);
                owed.extend(self.power_changed());
            } else {
                self.timers.cancel(Key::Power);
            }
        }

        let audio = asked(ids::AUDIO_ACTIVE);
        if audio != self.audio_watched {
            self.audio_watched = audio;
            if audio {
                self.timers
                    .set(Key::Audio, clock::boot_ms(), SOURCE_EVERY, SOURCE_SLACK);
                owed.extend(self.audio_changed());
            } else {
                self.timers.cancel(Key::Audio);
            }
        }

        if let Some(x) = &self.x {
            x.flush();
        }
        // Before the snapshot the snapshot says all of it.
        if after_snapshot {
            self.pending.extend(owed);
        }
        Ok(())
    }

    fn actuator(&self, cap: &CapabilityId) -> Option<&dyn Actuator> {
        match cap.as_str() {
            ids::DISPLAY_COLOR_TEMP => self.color_temp.as_ref().ok().map(Colour::actuator),
            ids::DISPLAY_MODE => self.display_mode.as_ref().ok().map(|a| a as &dyn Actuator),
            _ => None,
        }
    }

    fn set_timer(&mut self, id: TimerId, after: Duration, tolerance: Duration) {
        self.timers
            .set(Key::Engine(id.0), clock::boot_ms(), after, tolerance);
    }

    fn cancel_timer(&mut self, id: TimerId) {
        self.timers.cancel(Key::Engine(id.0));
    }

    fn time_of_day(&self) -> Option<Event> {
        clock::time_of_day()
    }

    fn poll(&mut self, timeout: Duration) -> Vec<Event> {
        if !self.snapshot_sent {
            self.snapshot_sent = true;
            return self.snapshot();
        }
        let mut out = std::mem::take(&mut self.pending);
        let woken = self.gather(&mut out);
        if out.is_empty() && !woken {
            self.wait(timeout);
            self.gather(&mut out);
        }
        // Answers to selection requests and grab changes leave now, not at
        // the next wait.
        if let Some(x) = &self.x {
            x.flush();
        }
        out
    }

    /// The boot clock, which counts a suspend as `GetTickCount64` does.
    fn monotonic_ms(&self) -> u64 {
        clock::boot_ms()
    }

    fn waker(&self) -> Option<cortisoff_host::Waker> {
        Some(self.wake.waker())
    }

    fn set_hotkeys(&mut self, wanted: &[Hotkey]) -> Vec<(u32, String)> {
        if wayland_session(self.x.as_deref()) {
            return match &mut self.shortcuts {
                Ok(portal) => portal.set(wanted),
                // Nothing wanted, nothing to refuse.
                Err(_) if wanted.is_empty() => Vec::new(),
                Err(why) => Self::refuse_all(
                    wanted,
                    &format!("this is a Wayland session, where a global shortcut goes through the desktop portal, and {why}"),
                ),
            };
        }
        let x = match self.display() {
            Ok(x) => x,
            Err(why) => return Self::refuse_all(wanted, &why),
        };
        let Some(map) = &self.keymap else {
            return Self::refuse_all(wanted, NO_KEYMAP);
        };
        self.hotkeys.set(&x, map, wanted)
    }

    fn send_keys(&mut self, chord: &Chord) -> Result<(), String> {
        if wayland_session(self.x.as_deref()) {
            return Err(WAYLAND_INPUT.to_owned());
        }
        let x = self.display()?;
        let map = self.keymap.as_ref().ok_or(NO_KEYMAP)?;
        input::send_chord(&x, map, chord)
    }

    fn type_text(&mut self, text: &str) -> Result<(), String> {
        if wayland_session(self.x.as_deref()) {
            return Err(WAYLAND_INPUT.to_owned());
        }
        let x = self.display()?;
        let map = self.keymap.as_ref().ok_or(NO_KEYMAP)?;
        input::type_text(&x, map, text)
    }

    /// Through `notify-send`, which speaks the desktop notification
    /// protocol over D-Bus so this crate does not have to. It returns at
    /// once; a thread reaps it so no zombie stays behind.
    fn notify(&mut self, title: &str, body: &str) -> Result<(), String> {
        let spawned = Command::new("notify-send")
            .arg("--app-name=cortisoff")
            .arg("--")
            .arg(title)
            .arg(body)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(
                "notify-send is not installed: it comes with libnotify, the libnotify-bin package on Debian and Ubuntu"
                    .to_owned(),
            ),
            Err(e) => Err(format!("could not start notify-send: {e}")),
        }
    }

    fn clipboard(&self) -> Result<String, String> {
        let x = self.display()?;
        clipboard::read(&x, self.clipboard.as_ref())
    }

    fn set_clipboard(&mut self, text: &str) -> Result<(), String> {
        let x = self.display()?;
        let owner = match self.clipboard.take() {
            Some(owner) => owner,
            None => Owner::new(&x)?,
        };
        self.clipboard.insert(owner).set(&x, text)
    }

    /// Under XWayland the list holds X11 windows only, and the last one that
    /// had the focus keeps `_NET_ACTIVE_WINDOW` while a Wayland window is in
    /// front, so none is reported as focused there.
    fn windows(&self) -> Vec<WindowInfo> {
        let mut list = self.x.as_deref().map(wm::windows).unwrap_or_default();
        if !self.x11_session() {
            for w in &mut list {
                w.focused = false;
            }
        }
        list
    }

    fn window_action(&mut self, id: u64, action: WindowAction) -> Result<(), String> {
        let x = self.display()?;
        wm::act(&x, id, action)
    }
}

fn millis(d: Duration) -> u64 {
    d.as_millis().min(u128::from(u64::MAX)) as u64
}

/// The highest threshold reached, so only crossings are pushed and not one
/// event per check.
fn idle_bucket(seconds: u32, thresholds: &[u32]) -> u32 {
    thresholds
        .iter()
        .copied()
        .filter(|t| *t <= seconds)
        .max()
        .unwrap_or(0)
}

/// When to read the idle state again, and with how much slack: at the next
/// threshold, every [`RECHECK_MS`] while a gate holds idle at zero or the
/// user is away, and every second while away when no raw input will
/// announce the return. `away_ms` is already zero when gated. `None` means
/// nothing is left to wait for.
fn idle_check(
    away_ms: u64,
    gated: bool,
    raw_input: bool,
    thresholds: &[u32],
) -> Option<(Duration, Duration)> {
    let seconds = (away_ms / 1000).min(u64::from(u32::MAX)) as u32;
    let away = idle_bucket(seconds, thresholds) > 0;
    let mut plan = thresholds
        .iter()
        .map(|t| u64::from(*t) * 1000)
        .filter(|t| *t > away_ms)
        .min()
        .map(|t| (t - away_ms, THRESHOLD_SLACK_MS));
    let mut sooner = |candidate: (u64, u64)| {
        if plan.is_none_or(|(after, _)| candidate.0 < after) {
            plan = Some(candidate);
        }
    };
    if gated || away {
        sooner((RECHECK_MS, RECHECK_SLACK_MS));
    }
    if away && !raw_input {
        sooner((RESUME_MS, RESUME_SLACK_MS));
    }
    plan.map(|(after, slack)| (Duration::from_millis(after), Duration::from_millis(slack)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn each_threshold_is_its_own_crossing() {
        let thresholds = [60, 120, 300];
        assert_eq!(idle_bucket(0, &thresholds), 0);
        assert_eq!(idle_bucket(59, &thresholds), 0);
        assert_eq!(idle_bucket(60, &thresholds), 60);
        assert_eq!(idle_bucket(119, &thresholds), 60);
        assert_eq!(idle_bucket(300, &thresholds), 300);
    }

    /// Someone typing is looked at again when the first threshold could be
    /// reached, not before: no beat while the user is there.
    #[test]
    fn an_active_user_is_checked_at_the_next_threshold_only() {
        let thresholds = [60, 300];
        assert_eq!(
            idle_check(0, false, true, &thresholds),
            Some((s(60), ms(500)))
        );
        assert_eq!(
            idle_check(45_250, false, true, &thresholds),
            Some((ms(14_750), ms(500)))
        );
    }

    #[test]
    fn a_film_holding_idle_at_zero_is_checked_again_until_it_ends() {
        assert_eq!(idle_check(0, true, true, &[300]), Some((s(15), s(5))));
        assert_eq!(
            idle_check(0, true, true, &[10]),
            Some((s(10), ms(500))),
            "a threshold sooner than the recheck wins"
        );
    }

    #[test]
    fn while_away_raw_input_spares_the_one_second_beat() {
        let thresholds = [60, 300];
        assert_eq!(
            idle_check(61_000, false, true, &thresholds),
            Some((s(15), s(5)))
        );
        assert_eq!(
            idle_check(61_000, false, false, &thresholds),
            Some((s(1), ms(100)))
        );
        assert_eq!(
            idle_check(299_500, false, true, &thresholds),
            Some((ms(500), ms(500))),
            "the next threshold still lands on time"
        );
    }

    #[test]
    fn past_the_last_threshold_only_the_return_is_left() {
        assert_eq!(
            idle_check(900_000, false, true, &[300]),
            Some((s(15), s(5)))
        );
        assert_eq!(
            idle_check(900_000, false, false, &[300]),
            Some((s(1), ms(100)))
        );
        assert_eq!(idle_check(0, false, true, &[]), None);
    }
}
