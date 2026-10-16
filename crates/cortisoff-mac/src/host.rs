//! [`MacHost`]: the CFRunLoop host.
//!
//! Callbacks only queue what they saw (see [`crate::runloop`]); `poll` runs
//! the main thread's run loop until one of them fires, a host timer is due
//! or the timeout runs out, then turns the queue into events. Nothing here
//! needs `NSApplication`: notification centres, the display callback and
//! IOKit all deliver through the bare run loop, and Carbon hotkeys are
//! dispatched by hand after each pass.

use std::sync::Arc;
use std::time::Duration;

use cortisoff_host::{
    ids, Actuator, ActuatorError, CapabilityId, CapabilitySet, Chord, Event, Host, Hotkey, TimerId,
    WindowAction, WindowInfo,
};
use objc2::rc::autoreleasepool;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardTypeString, NSWorkspace, NSWorkspaceActiveSpaceDidChangeNotification,
    NSWorkspaceDidActivateApplicationNotification, NSWorkspaceDidWakeNotification,
};
use objc2_core_foundation::{kCFRunLoopDefaultMode, CFRunLoop};
use objc2_core_graphics::{
    CGEventSource, CGEventSourceStateID, CGEventType, CGPreflightScreenCaptureAccess,
};
use objc2_foundation::{
    NSDistributedNotificationCenter, NSNotificationCenter, NSString,
    NSSystemClockDidChangeNotification, NSSystemTimeZoneDidChangeNotification,
};

use crate::activity::InputWatch;
use crate::colortemp::ColorTemp;
use crate::fit::{self, Fit};
use crate::hotkeys::Hotkeys;
use crate::mode::DisplayMode;
use crate::runloop::{Raw, Shared};
use crate::sources::{self, DisplayWatch, Observers, PowerWatch};
use crate::timers::{idle_bucket, idle_check_after, Beat, Return, Timers, LAYOUT_AFTER_MS};
use crate::windows::{self, Placements, Window};
use crate::{ax, clock, input, notify, screen};

/// When a display change is pushed again, counted from the callback that
/// announced it. The same series as the Windows host: a table written while
/// WindowServer is still reconfiguring can be dropped when it finishes, and
/// three extra applies cost three table writes.
const SETTLE: [Duration; 3] = [
    Duration::from_millis(400),
    Duration::from_millis(1_500),
    Duration::from_millis(4_000),
];

/// How long without input counts as away, unless the daemon says otherwise.
const DEFAULT_IDLE_AFTER: Duration = Duration::from_secs(5 * 60);

/// How often the front window's fit is read while a plugin watches it. macOS has
/// no event for another application's window being resized, and a window
/// going fullscreen is not a frame deadline.
const WINDOW_EVERY: Duration = Duration::from_secs(2);
const WINDOW_SLACK: Duration = Duration::from_secs(1);

/// The power source when IOKit hands over no notification source.
const POWER_EVERY: Duration = Duration::from_secs(30);
const POWER_SLACK: Duration = Duration::from_secs(15);

/// `kCGAnyInputEventType`.
const ANY_INPUT: CGEventType = CGEventType(u32::MAX);

pub struct MacHost {
    shared: Arc<Shared>,
    timers: Timers,
    hotkeys: Hotkeys,
    placements: Placements,
    /// The wall clock moving underneath the engine's timers. Always on, like
    /// `WM_TIMECHANGE` on Windows: a schedule is wrong after a time zone
    /// change whether or not a plugin watches anything else.
    _clock_observers: Observers,
    focus_observers: Observers,
    lock_observers: Observers,
    /// The layout switch, watched while a hotkey is registered.
    layout_observers: Observers,
    /// The activity tap, on only while the user is away.
    input_watch: Option<InputWatch>,
    /// WindowServer refused the tap during this absence; asked again at the
    /// next one.
    input_refused: bool,
    display_watch: Option<DisplayWatch>,
    power_watch: Option<PowerWatch>,
    power_watched: bool,
    window_watched: bool,
    idle_watched: bool,
    idle_after: Duration,
    idle_thresholds: Vec<u32>,
    idle_seconds: u32,
    /// Last state pushed, so a beat that changes nothing pushes nothing.
    idle: bool,
    fullscreen: bool,
    maximized: bool,
    on_battery: bool,
    locked: bool,
    color_temp: Option<ColorTemp>,
    display_mode: Option<DisplayMode>,
    /// The first poll answers with a snapshot: plugins decide from the
    /// world, so they need the world before they can mean anything.
    snapshot_sent: bool,
    /// How far through [`SETTLE`] the current display change is.
    settling: Option<usize>,
    /// Every call touching AppKit has to be on the main thread, and holding
    /// the marker keeps this type there: it is neither `Send` nor `Sync`.
    _main: MainThreadMarker,
}

fn ms(d: Duration) -> u64 {
    d.as_millis().min(u128::from(u64::MAX)) as u64
}

impl MacHost {
    /// Builds the host on the calling thread, which has to be the main one:
    /// AppKit refuses the others, and the main run loop is where
    /// WindowServer delivers.
    pub fn new() -> Result<Self, String> {
        let main = MainThreadMarker::new()
            .ok_or_else(|| "the macOS host has to be created on the main thread".to_owned())?;
        autoreleasepool(|_| {
            let shared = Shared::new()?;
            let mut clock_observers = Observers::default();
            // SAFETY: reading constants Foundation and AppKit export.
            let (clock_changed, zone_changed, woke) = unsafe {
                (
                    NSSystemClockDidChangeNotification,
                    NSSystemTimeZoneDidChangeNotification,
                    NSWorkspaceDidWakeNotification,
                )
            };
            let local = NSNotificationCenter::defaultCenter();
            clock_observers.observe(local.clone(), clock_changed, &shared, Raw::ClockMoved);
            clock_observers.observe(local, zone_changed, &shared, Raw::ClockMoved);
            let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
            clock_observers.observe(workspace, woke, &shared, Raw::ClockMoved);
            let display_watch = DisplayWatch::new(Arc::clone(&shared)).ok();
            Ok(Self {
                hotkeys: Hotkeys::new(Arc::clone(&shared)),
                shared,
                timers: Timers::default(),
                placements: Placements::default(),
                _clock_observers: clock_observers,
                focus_observers: Observers::default(),
                lock_observers: Observers::default(),
                layout_observers: Observers::default(),
                input_watch: None,
                input_refused: false,
                display_watch,
                power_watch: None,
                power_watched: false,
                window_watched: false,
                idle_watched: false,
                idle_after: DEFAULT_IDLE_AFTER,
                idle_thresholds: vec![DEFAULT_IDLE_AFTER.as_secs() as u32],
                idle_seconds: 0,
                idle: false,
                fullscreen: false,
                maximized: false,
                on_battery: false,
                locked: false,
                color_temp: ColorTemp::new(),
                display_mode: DisplayMode::new(),
                snapshot_sent: false,
                settling: None,
                _main: main,
            })
        })
    }

    /// Machine facts the user should know that are not failures: the two
    /// permissions macOS gates parts of this host behind.
    fn machine_warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !ax::trusted() {
            out.push(
                "macOS has not given cortisoff the Accessibility permission, so typing, sending keys and window actions other than activate are refused. turn it on in System Settings, Privacy & Security, Accessibility"
                    .to_owned(),
            );
        }
        if !CGPreflightScreenCaptureAccess() {
            out.push(
                "macOS hides other applications' window titles from cortisoff without the Screen Recording permission, so app.focus reports no title and a plugin waiting for a title never sees one. turn it on in System Settings, Privacy & Security, Screen Recording"
                    .to_owned(),
            );
        }
        if self.display_watch.is_none() {
            out.push(
                "CoreGraphics refused the display reconfiguration callback, so a display plugged in later is not tinted until a plugin claims again"
                    .to_owned(),
            );
        }
        out
    }

    /// How long without input sets the world's `idle` flag. A plugin wanting
    /// its own delay passes `after` to `cx:watch("user.idle")`, which reaches
    /// `set_idle_thresholds`; this is the default for every other plugin.
    pub fn idle_after(mut self, after: Duration) -> Self {
        self.idle_after = after;
        self
    }

    fn now(&self) -> u64 {
        clock::monotonic_ms()
    }

    fn arm(&mut self, beat: Beat, after: Duration, tolerance: Duration) {
        let now = self.now();
        self.timers.arm(beat, now, ms(after), ms(tolerance));
    }

    fn front_fit(&self, windows: &[Window]) -> Fit {
        windows::front_pid()
            .and_then(|pid| windows::front_of(windows, pid))
            .map(|w| fit::fit(w.bounds, &screen::frames()))
            .unwrap_or_default()
    }

    /// Seconds since the last input, or zero while something holds the
    /// screen: a film or a game played with a controller is not an absent
    /// user. The fit is only read once a threshold is reached, so the common
    /// case costs one call. The flag says the screen was what held it.
    fn away_seconds(&self) -> (u32, bool) {
        let seconds = CGEventSource::seconds_since_last_event_type(
            CGEventSourceStateID::CombinedSessionState,
            ANY_INPUT,
        );
        if !seconds.is_finite() || seconds < 1.0 {
            return (0, false);
        }
        let seconds = seconds.min(f64::from(u32::MAX)) as u32;
        if idle_bucket(seconds, &self.idle_thresholds) > 0
            && self.front_fit(&windows::on_screen()).fullscreen
        {
            return (0, true);
        }
        (seconds, false)
    }

    fn sample_idle(&mut self) -> Event {
        let (seconds, held) = self.away_seconds();
        self.idle = u64::from(seconds) >= self.idle_after.as_secs();
        self.idle_seconds = idle_bucket(seconds, &self.idle_thresholds);
        if self.idle_watched {
            self.watch_return(self.idle_seconds > 0);
            self.arm_idle_at(seconds, held);
        }
        Event::Idle {
            idle: self.idle,
            seconds: self.idle_seconds,
        }
    }

    /// The activity tap goes in when an absence starts and out when it ends,
    /// so a user at the keyboard costs no tap at all.
    fn watch_return(&mut self, away: bool) {
        if !away {
            self.input_watch = None;
            self.input_refused = false;
            return;
        }
        if self.input_watch.is_none() && !self.input_refused {
            self.input_watch = InputWatch::new(Arc::clone(&self.shared));
            self.input_refused = self.input_watch.is_none();
        }
    }

    fn arm_idle_at(&mut self, seconds: u32, held: bool) {
        let back = match &self.input_watch {
            Some(watch) if watch.sees_keys() => Return::Tap,
            Some(_) => Return::MouseTap,
            None => Return::Sampled,
        };
        let after = idle_check_after(seconds, &self.idle_thresholds, held, back);
        self.arm(
            Beat::Idle,
            Duration::from_secs(u64::from(after)),
            Duration::from_millis(100),
        );
    }

    /// Hotkeys sit on key codes picked for the layout active when they were
    /// registered, so a layout switch registers them again.
    fn watch_layout(&mut self, on: bool) {
        match (on, self.layout_observers.is_empty()) {
            (true, true) => {
                let center = NSDistributedNotificationCenter::defaultCenter().into_super();
                let name = NSString::from_str(
                    "com.apple.Carbon.TISNotifySelectedKeyboardInputSourceChanged",
                );
                self.layout_observers
                    .observe(center, &name, &self.shared, Raw::Layout);
            }
            (false, false) => self.layout_observers = Observers::default(),
            _ => {}
        }
    }

    /// Arm the next push of a display change, or stop once the last one in
    /// [`SETTLE`] has fired. A change arriving mid-series restarts it.
    fn arm_settle(&mut self, step: usize) {
        match SETTLE.get(step) {
            Some(after) => {
                self.settling = Some(step);
                // No slack: the point is to land while the configuration is
                // still settling.
                self.arm(Beat::Settle, *after, Duration::ZERO);
            }
            None => {
                self.settling = None;
                self.timers.cancel(Beat::Settle);
            }
        }
    }

    fn read_power(&self) -> bool {
        sources::on_battery().unwrap_or(false)
    }

    /// Maximised and fullscreen, pushed only on a change.
    fn window_events(&mut self, windows: &[Window]) -> Vec<Event> {
        let fit = self.front_fit(windows);
        let mut out = Vec::new();
        if fit.maximized != self.maximized {
            self.maximized = fit.maximized;
            out.push(Event::Maximized {
                maximized: fit.maximized,
            });
        }
        if fit.fullscreen != self.fullscreen {
            self.fullscreen = fit.fullscreen;
            out.push(Event::Fullscreen {
                exclusive: fit.fullscreen,
                outputs: Vec::new(),
            });
            // A game taking the screen is where a display's tables are most
            // likely to be rewritten under a held tint, by the game or by
            // a mode change. One extra write per edge settles it.
            if self.color_temp.is_some() {
                out.push(Event::Reapply {
                    cap: CapabilityId::new(ids::DISPLAY_COLOR_TEMP),
                });
            }
        }
        out
    }

    /// Everything a plugin could already be told about, pushed before the
    /// first real event so no plugin acts on a half-known world.
    fn snapshot(&mut self) -> Vec<Event> {
        let windows = windows::on_screen();
        let fit = self.front_fit(&windows);
        self.fullscreen = fit.fullscreen;
        self.maximized = fit.maximized;
        let idle = self.sample_idle();
        self.on_battery = self.read_power();
        self.locked = sources::locked();
        let mut out = vec![
            clock::time_of_day(),
            Event::Fullscreen {
                exclusive: self.fullscreen,
                outputs: Vec::new(),
            },
            idle,
            Event::SessionLock {
                locked: self.locked,
            },
            Event::PowerSource {
                on_battery: self.on_battery,
            },
            Event::Maximized {
                maximized: self.maximized,
            },
        ];
        out.extend(windows::focused_app(&windows).map(Event::AppFocus));
        out
    }

    fn translate(&mut self, raw: Raw) -> Vec<Event> {
        match raw {
            Raw::Focus => {
                let windows = windows::on_screen();
                let mut out: Vec<Event> = windows::focused_app(&windows)
                    .map(Event::AppFocus)
                    .into_iter()
                    .collect();
                if self.window_watched {
                    out.extend(self.window_events(&windows));
                }
                self.placements.prune(&windows);
                out
            }
            Raw::Lock(locked) => {
                if locked == self.locked {
                    return Vec::new();
                }
                self.locked = locked;
                vec![Event::SessionLock { locked }]
            }
            // Answered now and again as the configuration settles.
            Raw::DisplayChanged => {
                self.arm_settle(0);
                vec![Event::DisplayConfigChanged]
            }
            Raw::ClockMoved => vec![clock::time_of_day()],
            Raw::Power => self.power_event(),
            Raw::Hotkey(id) => vec![Event::Hotkey { id }],
            // One input ends the absence; the tap has done its job.
            Raw::Input => {
                self.input_watch = None;
                if !self.idle_watched {
                    return Vec::new();
                }
                let before = (self.idle, self.idle_seconds);
                let event = self.sample_idle();
                if before == (self.idle, self.idle_seconds) {
                    Vec::new()
                } else {
                    vec![event]
                }
            }
            // Read on a later pass: see `LAYOUT_AFTER_MS`.
            Raw::Layout => {
                self.arm(
                    Beat::Layout,
                    Duration::from_millis(LAYOUT_AFTER_MS),
                    Duration::ZERO,
                );
                Vec::new()
            }
        }
    }

    fn power_event(&mut self) -> Vec<Event> {
        let on_battery = self.read_power();
        if on_battery == self.on_battery {
            return Vec::new();
        }
        self.on_battery = on_battery;
        vec![Event::PowerSource { on_battery }]
    }

    fn on_beat(&mut self, beat: Beat) -> Vec<Event> {
        match beat {
            // The engine asked once for a schedule edge or a fade step: send
            // the time for schedules and the id for previews.
            Beat::Engine(id) => vec![clock::time_of_day(), Event::TimerFired { id }],
            Beat::Idle => {
                let before = (self.idle, self.idle_seconds);
                let event = self.sample_idle();
                if before == (self.idle, self.idle_seconds) {
                    Vec::new()
                } else {
                    vec![event]
                }
            }
            Beat::Window => {
                if !self.window_watched {
                    return Vec::new();
                }
                self.arm(Beat::Window, WINDOW_EVERY, WINDOW_SLACK);
                let windows = windows::on_screen();
                self.window_events(&windows)
            }
            Beat::Settle => {
                let next = self.settling.map_or(SETTLE.len(), |step| step + 1);
                self.arm_settle(next);
                vec![Event::DisplayConfigChanged]
            }
            Beat::Power => {
                if !self.power_watched {
                    return Vec::new();
                }
                self.arm(Beat::Power, POWER_EVERY, POWER_SLACK);
                self.power_event()
            }
            Beat::Layout => {
                self.hotkeys.follow_layout();
                Vec::new()
            }
        }
    }

    /// Runs the run loop for at most `wait`, or not at all when something is
    /// already queued.
    fn wait(&self, wait: Duration) {
        if self.shared.pending() || wait.is_zero() {
            return;
        }
        // SAFETY: reading a constant CoreFoundation exports.
        let mode = unsafe { kCFRunLoopDefaultMode };
        // Returning after one handled source is what makes a signal from
        // the wake source, a notification or the display callback end the
        // wait at once.
        CFRunLoop::run_in_mode(mode, wait.as_secs_f64(), true);
    }
}

impl Host for MacHost {
    fn warnings(&self) -> Vec<String> {
        self.machine_warnings()
    }

    fn probe(&self) -> CapabilitySet {
        let id = CapabilityId::new;
        let set = CapabilitySet::new()
            .available(id(ids::APP_FOCUS), Some("nsworkspace"))
            .available(id(ids::APP_FULLSCREEN), Some("cgwindowlist"))
            .available(id(ids::APP_MAXIMIZED), Some("cgwindowlist"))
            .available(id(ids::USER_IDLE), Some("cgeventsource"))
            .available(id(ids::SESSION_LOCK), Some("distributed_notification"));
        let set = if self.display_watch.is_some() {
            set.available(id(ids::DISPLAY_CONFIG), Some("cgdisplay_reconfiguration"))
        } else {
            set.missing(
                id(ids::DISPLAY_CONFIG),
                "CoreGraphics refused the display reconfiguration callback",
            )
        };
        let set = if sources::on_battery().is_some() {
            set.available(id(ids::POWER_SOURCE), Some("iopowersources"))
        } else {
            set.missing(id(ids::POWER_SOURCE), "IOKit reports no power source")
        };
        let set = set
            .missing(
                id(ids::AUDIO_ACTIVE),
                "macOS only says whether an output device is open, not whether it plays sound, and a level meter is not written yet",
            )
            .missing(
                id(ids::DISPLAY_VIBRANCE),
                "macOS offers no public saturation control for a display",
            )
            .missing(
                id(ids::WALLPAPER_PLAYBACK),
                "no animated wallpaper application on macOS is driven yet",
            );
        let set = match &self.color_temp {
            Some(a) => set.available(id(ids::DISPLAY_COLOR_TEMP), Some(a.backend_name())),
            None => set.missing(
                id(ids::DISPLAY_COLOR_TEMP),
                "no display would give up its transfer table",
            ),
        };
        match &self.display_mode {
            Some(a) => set.available(id(ids::DISPLAY_MODE), Some(a.backend_name())),
            None => set.missing(
                id(ids::DISPLAY_MODE),
                "the main display would not report its mode",
            ),
        }
    }

    fn set_idle_thresholds(&mut self, seconds: &[u32]) {
        self.idle_thresholds = seconds.to_vec();
        self.idle_thresholds
            .push(self.idle_after.as_secs().max(1) as u32);
        self.idle_thresholds.sort_unstable();
        self.idle_thresholds.dedup();
    }

    /// Only what some plugin watches. An observer costs a wakeup on every
    /// matching notification system-wide, so a plugin set that never watches
    /// the focused application installs none.
    fn install_sources(&mut self, wanted: &[CapabilityId]) -> Result<(), ActuatorError> {
        let asked = |id: &str| wanted.iter().any(|c| c.as_str() == id);
        autoreleasepool(|_| {
            // Fullscreen and maximised are read on a focus change as well as
            // on the slow beat, so they need the activation observer too.
            let window = asked(ids::APP_MAXIMIZED) || asked(ids::APP_FULLSCREEN);
            let focus = asked(ids::APP_FOCUS) || window;
            match (focus, self.focus_observers.is_empty()) {
                (true, true) => {
                    let center = NSWorkspace::sharedWorkspace().notificationCenter();
                    // SAFETY: reading constants AppKit exports.
                    let (activated, space) = unsafe {
                        (
                            NSWorkspaceDidActivateApplicationNotification,
                            NSWorkspaceActiveSpaceDidChangeNotification,
                        )
                    };
                    self.focus_observers.observe(
                        center.clone(),
                        activated,
                        &self.shared,
                        Raw::Focus,
                    );
                    // Switching to a fullscreen Space changes the front
                    // window without always activating another application.
                    self.focus_observers
                        .observe(center, space, &self.shared, Raw::Focus);
                }
                (false, false) => self.focus_observers = Observers::default(),
                _ => {}
            }

            self.window_watched = window;
            if window {
                if !self.timers.is_armed(Beat::Window) {
                    self.arm(Beat::Window, WINDOW_EVERY, WINDOW_SLACK);
                }
            } else {
                self.timers.cancel(Beat::Window);
            }

            match (asked(ids::SESSION_LOCK), self.lock_observers.is_empty()) {
                (true, true) => {
                    let center = NSDistributedNotificationCenter::defaultCenter().into_super();
                    for (name, locked) in [
                        ("com.apple.screenIsLocked", true),
                        ("com.apple.screenIsUnlocked", false),
                    ] {
                        let name = NSString::from_str(name);
                        self.lock_observers.observe(
                            center.clone(),
                            &name,
                            &self.shared,
                            Raw::Lock(locked),
                        );
                    }
                }
                (false, false) => self.lock_observers = Observers::default(),
                _ => {}
            }

            let power = asked(ids::POWER_SOURCE);
            if power && !self.power_watched {
                self.power_watch = PowerWatch::new(Arc::clone(&self.shared));
                if self.power_watch.is_none() {
                    self.arm(Beat::Power, POWER_EVERY, POWER_SLACK);
                }
            } else if !power {
                self.power_watch = None;
                self.timers.cancel(Beat::Power);
            }
            self.power_watched = power;

            self.idle_watched = asked(ids::USER_IDLE);
            if self.idle_watched {
                let (seconds, held) = self.away_seconds();
                self.arm_idle_at(seconds, held);
            } else {
                self.timers.cancel(Beat::Idle);
                self.input_watch = None;
            }
        });
        Ok(())
    }

    fn actuator(&self, cap: &CapabilityId) -> Option<&dyn Actuator> {
        match cap.as_str() {
            ids::DISPLAY_COLOR_TEMP => self.color_temp.as_ref().map(|a| a as &dyn Actuator),
            ids::DISPLAY_MODE => self.display_mode.as_ref().map(|a| a as &dyn Actuator),
            _ => None,
        }
    }

    fn set_timer(&mut self, id: TimerId, after: Duration, tolerance: Duration) {
        self.arm(Beat::Engine(id), after, tolerance);
    }

    fn cancel_timer(&mut self, id: TimerId) {
        self.timers.cancel(Beat::Engine(id));
    }

    fn time_of_day(&self) -> Option<Event> {
        Some(clock::time_of_day())
    }

    fn poll(&mut self, timeout: Duration) -> Vec<Event> {
        // No NSApplication drains autorelease pools here, so every pass
        // brings its own, or each notification leaks what it returned.
        autoreleasepool(|_| {
            if !self.snapshot_sent {
                self.snapshot_sent = true;
                return self.snapshot();
            }

            let now = self.now();
            let wait = match self.timers.deadline() {
                Some(deadline) => timeout.min(Duration::from_millis(deadline.saturating_sub(now))),
                None => timeout,
            };
            self.wait(wait);
            self.hotkeys.drain();

            let mut raws = self.shared.take();
            // A display change announces itself once per display, and the
            // settle series answers all of them the same way.
            let mut seen_display = false;
            raws.retain(|raw| {
                if *raw == Raw::DisplayChanged {
                    let first = !seen_display;
                    seen_display = true;
                    first
                } else {
                    true
                }
            });

            let mut out: Vec<Event> = raws
                .into_iter()
                .flat_map(|raw| self.translate(raw))
                .collect();
            let now = self.now();
            for beat in self.timers.take_due(now) {
                out.extend(self.on_beat(beat));
            }
            out
        })
    }

    fn monotonic_ms(&self) -> u64 {
        clock::monotonic_ms()
    }

    fn waker(&self) -> Option<cortisoff_host::Waker> {
        let shared = Arc::clone(&self.shared);
        Some(Arc::new(move || shared.wake()))
    }

    fn set_hotkeys(&mut self, wanted: &[Hotkey]) -> Vec<(u32, String)> {
        autoreleasepool(|_| {
            let refused = self.hotkeys.set(wanted);
            self.watch_layout(self.hotkeys.active());
            refused
        })
    }

    fn send_keys(&mut self, chord: &Chord) -> Result<(), String> {
        input::send_keys(chord)
    }

    fn type_text(&mut self, text: &str) -> Result<(), String> {
        input::type_text(text)
    }

    fn notify(&mut self, title: &str, body: &str) -> Result<(), String> {
        notify::send(title, body)
    }

    /// Plain text only. A clipboard holding an image or a file reads as
    /// empty, which is what it holds as text.
    fn clipboard(&self) -> Result<String, String> {
        autoreleasepool(|_| {
            let board = NSPasteboard::generalPasteboard();
            // SAFETY: reading a constant AppKit exports.
            let kind = unsafe { NSPasteboardTypeString };
            Ok(board
                .stringForType(kind)
                .map(|s| s.to_string())
                .unwrap_or_default())
        })
    }

    fn set_clipboard(&mut self, text: &str) -> Result<(), String> {
        autoreleasepool(|_| {
            let board = NSPasteboard::generalPasteboard();
            // SAFETY: reading a constant AppKit exports.
            let kind = unsafe { NSPasteboardTypeString };
            board.clearContents();
            if board.setString_forType(&NSString::from_str(text), kind) {
                Ok(())
            } else {
                Err("macOS refused to put the text on the clipboard".to_owned())
            }
        })
    }

    fn windows(&self) -> Vec<WindowInfo> {
        autoreleasepool(|_| windows::list())
    }

    fn window_action(&mut self, id: u64, action: WindowAction) -> Result<(), String> {
        autoreleasepool(|_| self.placements.act(id, action))
    }
}
