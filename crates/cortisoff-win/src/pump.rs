//! The message pump, and the [`Host`] built on it.
//!
//! One hidden window carries everything: the session notifications, the
//! display, clock and power broadcasts, the tray icon and every timer. It is a
//! real top-level window that is never shown, not a message-only one: a
//! message-only window receives no broadcast at all, so `WM_DISPLAYCHANGE`,
//! `WM_TIMECHANGE` and `TaskbarCreated` would never arrive.

use std::time::Duration;

use cortisoff_host::{
    ids, Actuator, ActuatorError, CapabilityId, CapabilitySet, Chord, Event, Host, Hotkey, TimerId,
    WindowAction, WindowInfo,
};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::HPOWERNOTIFY;
use windows::Win32::System::SystemInformation::{GetLocalTime, GetTickCount, GetTickCount64};
use windows::Win32::UI::Accessibility::HWINEVENTHOOK;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, GetForegroundWindow, KillTimer,
    MsgWaitForMultipleObjectsEx, PeekMessageW, RegisterClassW, SetCoalescableTimer, MSG,
    MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, WINDOW_STYLE, WNDCLASSW, WS_EX_TOOLWINDOW,
};

use crate::audio::Audio;
use crate::colortemp::ColorTemp;
use crate::hotkeys::Hotkeys;
use crate::mode::DisplayMode;
use crate::names::Names;
use crate::notice::Notice;
use crate::sources::{self, Raw};
use crate::tray::{self, Tray};
use crate::vibrance::Vibrance;
use crate::wallpaper::Wallpaper;

/// The wall clock, local, as year, month, day, hour, minute, second and
/// millisecond.
///
/// The one reading in the project: the plugins' clock events and the
/// daemon's log both come through here, so they never disagree about the hour
/// across a daylight-saving change.
pub fn local_time() -> (u16, u8, u8, u8, u8, u8, u16) {
    let now = unsafe { GetLocalTime() };
    (
        now.wYear,
        now.wMonth as u8,
        now.wDay as u8,
        now.wHour as u8,
        now.wMinute as u8,
        now.wSecond as u8,
        now.wMilliseconds,
    )
}

/// The daemon's own beats, kept away from the engine's timer ids.
const IDLE_TIMER: usize = 0x0100_0000;
const AUDIO_TIMER: usize = 0x0100_0001;
const DISPLAY_TIMER: usize = 0x0100_0002;
const NOTICE_TIMER: usize = 0x0100_0003;

/// How long the notification-only icon stays when Windows never says its
/// balloon is done, which it does not for one shown during quiet hours.
const NOTICE_LINGER: Duration = Duration::from_secs(30);

/// When a display change is pushed again, counted from the message that
/// announced it.
///
/// `WM_DISPLAYCHANGE` arrives while the driver is still working. A tint
/// written inside that window is accepted and then dropped when the driver
/// finishes reprogramming the output, which is what made switching a monitor
/// off and on look like cortisoff had quit: the ramp went back to neutral and
/// nothing asked for it again until the plugin's next claim, hours later. So the
/// answer is pushed again as the configuration settles. Three extra applies
/// per display change cost a few ramp writes and nothing else.
const SETTLE: [Duration; 3] = [
    Duration::from_millis(400),
    Duration::from_millis(1_500),
    Duration::from_millis(4_000),
];
/// Engine timer ids are shifted by one, because a Windows timer id of zero is
/// legal but easy to confuse with "no timer".
const ENGINE_TIMER_BASE: usize = 1;

/// How long without input counts as away. Windows emits no idle event at all,
/// so reaching it is a timer set for that moment; coming back is raw input.
const DEFAULT_IDLE_AFTER: Duration = Duration::from_secs(5 * 60);

/// How often idle is looked at again while a film or a fullscreen game holds
/// the user present, the one idle case with no event to wait for.
const HELD_RECHECK: Duration = Duration::from_secs(15);

/// How often `audio.active` is read while a plugin watches it. A meter has no
/// event, and a film starting is not a frame deadline.
const AUDIO_EVERY: Duration = Duration::from_secs(30);

/// The longest wait when the kernel refused the wake event, the only case
/// where the pump has to look again on its own.
const FALLBACK_TICK_MS: u32 = 500;

pub struct WinHost {
    window: HWND,
    foreground_hook: Option<HWINEVENTHOOK>,
    location_hook: Option<HWINEVENTHOOK>,
    /// Some plugin watches `app.maximized`, so the location hook follows the
    /// foreground window's process.
    location_wanted: bool,
    /// Raw input is registered, which it is only while the user is away.
    input_sunk: bool,
    session_watched: bool,
    power_watch: Option<HPOWERNOTIFY>,
    audio_watched: bool,
    idle_after: Duration,
    idle_thresholds: Vec<u32>,
    idle_watched: bool,
    idle_seconds: u32,
    /// Last state pushed, so a beat that changes nothing pushes nothing.
    idle: bool,
    fullscreen: bool,
    maximized: bool,
    on_battery: bool,
    audio_active: bool,
    names: Names,
    audio: Audio,
    tray: Option<Tray>,
    hotkeys: Hotkeys,
    /// The icon a notification comes from when there is no tray icon.
    notice: Option<Notice>,
    color_temp: Option<ColorTemp>,
    vibrance: Option<Vibrance>,
    display_mode: Option<DisplayMode>,
    wallpaper: Option<Wallpaper>,
    /// The first poll answers with a snapshot: plugins decide from the
    /// world, so they need the world before they can mean anything.
    snapshot_sent: bool,
    /// How far through [`SETTLE`] the current display change is. `None`
    /// between changes.
    settling: Option<usize>,
}

impl WinHost {
    pub fn new() -> Result<Self, ActuatorError> {
        let instance = unsafe { GetModuleHandleW(None) }
            .map_err(|e| ActuatorError::new(format!("no module handle: {e}")))?;
        let class = w!("cortisoff.pump");

        let wc = WNDCLASSW {
            lpfnWndProc: Some(sources::wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        // Zero means the class already exists, which is fine on a restart
        // inside the same process and is not worth distinguishing.
        unsafe { RegisterClassW(&wc) };

        let window = unsafe {
            CreateWindowExW(
                // Never shown, and a tool window besides, so nothing could put
                // it on the taskbar or in alt-tab even if something showed it.
                WS_EX_TOOLWINDOW,
                class,
                w!("cortisoff"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                None,
                None,
                instance,
                None,
            )
        }
        .map_err(|e| ActuatorError::new(format!("could not create the daemon window: {e}")))?;

        Ok(Self {
            window,
            foreground_hook: None,
            location_hook: None,
            location_wanted: false,
            input_sunk: false,
            session_watched: false,
            power_watch: None,
            audio_watched: false,
            idle_after: DEFAULT_IDLE_AFTER,
            idle_thresholds: vec![DEFAULT_IDLE_AFTER.as_secs() as u32],
            idle_watched: false,
            idle_seconds: 0,
            idle: false,
            fullscreen: false,
            maximized: false,
            on_battery: false,
            audio_active: false,
            names: Names::default(),
            audio: Audio::new(),
            tray: None,
            hotkeys: Hotkeys::new(window),
            notice: None,
            color_temp: ColorTemp::new(),
            vibrance: Vibrance::new(),
            display_mode: DisplayMode::new(),
            wallpaper: Wallpaper::new(),
            snapshot_sent: false,
            settling: None,
        })
    }

    /// Put an icon in the notification area. Only the running daemon wants
    /// one: `--doctor` builds a host too, and an icon flashing up for the
    /// length of a report is noise.
    pub fn with_tray(mut self) -> Self {
        self.tray = Some(Tray::add(self.window));
        self
    }

    /// Machine facts the user should know that are not failures. The doctor
    /// prints these next to the capability list.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.color_temp.is_some() && !crate::screen::gamma_range_unlocked() {
            out.push(
                r"Windows refuses a gamma ramp below about 3400 K, so a warmer colour temperature is sent as the strongest ramp it accepts: 3200 K loses almost nothing, 2500 K looks like 3400 K. lifting the clamp is one registry value, HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ICM\GdiIcmGammaRange set to 256, admin once plus a reboot"
                    .to_owned(),
            );
        }
        // Only when it would surprise someone writing a plugin. Every NVIDIA
        // driver seen so far reports 0 to 100 with 50 as neutral, and saying
        // so every time would be noise.
        if let Some((min, max, default)) = self.vibrance.as_ref().and_then(Vibrance::range) {
            if (min, max, default) != (0, 100, 50) {
                out.push(format!(
                    "this driver reports vibrance from {min} to {max}, neutral at {default}, not the usual 0 to 100. a claimed value is clamped to that range"
                ));
            }
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

    fn clock(&self) -> Event {
        let (_, _, _, hour, minute, second, millisecond) = local_time();
        Event::TimeOfDay {
            minute: u16::from(hour) * 60 + u16::from(minute),
            second: second.min(59),
            millisecond,
        }
    }

    /// True while an exclusive-fullscreen application or a presentation owns
    /// the screen. Also what stops the wallpaper pausing during a film: a
    /// mouse that has not moved in ten minutes is not an absent user.
    fn fullscreen_now(&self) -> bool {
        match unsafe { SHQueryUserNotificationState() } {
            Ok(state) => state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE,
            Err(_) => false,
        }
    }

    /// Seconds since the last keyboard or mouse input, and whether a
    /// fullscreen application or sound playing holds the user present
    /// whatever the input says.
    fn away(&self) -> (u32, bool) {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
            return (0, false);
        }
        let seconds = unsafe { GetTickCount() }.wrapping_sub(info.dwTime) / 1000;
        if seconds == 0 {
            return (0, false);
        }
        (seconds, self.fullscreen_now() || self.audio.playing())
    }

    fn sample_idle(&mut self) -> Event {
        let (seconds, held) = self.away();
        let seconds = if held { 0 } else { seconds };
        self.idle = u64::from(seconds) >= self.idle_after.as_secs();
        self.idle_seconds = idle_bucket(seconds, &self.idle_thresholds);
        if self.idle_watched {
            self.arm_idle(seconds, held);
        }
        Event::Idle {
            idle: self.idle,
            seconds: self.idle_seconds,
        }
    }

    fn arm_idle_timer(&mut self) {
        let (seconds, held) = self.away();
        self.arm_idle(if held { 0 } else { seconds }, held);
    }

    /// Wake at the next threshold, and hear the user come back.
    ///
    /// Reaching a threshold is a timer set for exactly then: no input for N
    /// seconds has no event. Coming back is raw input, registered only while
    /// away. A film or a fullscreen game holds the user present, and its end
    /// has no event here when no plugin watches sound, so that one case is
    /// looked at again every [`HELD_RECHECK`].
    fn arm_idle(&mut self, seconds: u32, held: bool) {
        let away = idle_bucket(seconds, &self.idle_thresholds) > 0;
        let sunk = self.sink_input(away);
        let wait = if held {
            Some(HELD_RECHECK.as_secs() as u32)
        } else if away && !sunk {
            // Raw input refused: the old way, one look a second.
            Some(1)
        } else {
            next_threshold_in(seconds, &self.idle_thresholds)
        };
        match wait {
            Some(s) => unsafe {
                SetCoalescableTimer(self.window, IDLE_TIMER, s.max(1) * 1000, None, 100);
            },
            None => {
                let _ = unsafe { KillTimer(self.window, IDLE_TIMER) };
            }
        }
    }

    /// Register or drop raw input; true when the wanted state holds.
    fn sink_input(&mut self, on: bool) -> bool {
        if on != self.input_sunk && sources::sink_input(self.window, on) {
            self.input_sunk = on;
        }
        self.input_sunk == on
    }

    /// Scope the location hook to the process owning `hwnd`, the new
    /// foreground window. False only when Windows refused the hook for a real
    /// window.
    fn follow_foreground(&mut self, hwnd: HWND) -> bool {
        if let Some(hook) = self.location_hook.take() {
            sources::unhook_foreground(hook);
        }
        if !self.location_wanted || hwnd.is_invalid() {
            return true;
        }
        self.location_hook = sources::hook_location(hwnd);
        self.location_hook.is_some()
    }

    /// Arm the next push of a display change, or stop once the last one in
    /// [`SETTLE`] has fired. A change arriving mid-series restarts it, which
    /// is what a monitor announcing itself three times in a row needs.
    fn arm_settle(&mut self, step: usize) {
        match SETTLE.get(step) {
            Some(after) => {
                self.settling = Some(step);
                let ms = after.as_millis() as u32;
                // No slack: the point is to land while the driver is still
                // settling, and a coalesced timer could drift past it.
                unsafe { SetCoalescableTimer(self.window, DISPLAY_TIMER, ms, None, 0) };
            }
            None => {
                self.settling = None;
                let _ = unsafe { KillTimer(self.window, DISPLAY_TIMER) };
            }
        }
    }

    fn foreground_event(&mut self, hwnd: HWND) -> Option<Event> {
        self.names.resolve(hwnd).map(Event::AppFocus)
    }

    /// Whether the focused window fills its monitor, pushed only on a change.
    fn maximized_event(&mut self, hwnd: HWND) -> Option<Event> {
        let maximized = crate::maximized::covers_monitor(hwnd);
        if maximized == self.maximized {
            return None;
        }
        self.maximized = maximized;
        Some(Event::Maximized { maximized })
    }

    /// Everything a plugin could already be told about, pushed before the first
    /// real event so no plugin acts on a half-known world.
    fn snapshot(&mut self) -> Vec<Event> {
        self.fullscreen = self.fullscreen_now();
        let idle_event = self.sample_idle();
        self.on_battery = crate::power::on_battery();
        self.audio_active = self.audio.playing();
        let mut out = vec![
            self.clock(),
            Event::Fullscreen {
                exclusive: self.fullscreen,
            },
            idle_event,
            Event::SessionLock { locked: false },
            Event::PowerSource {
                on_battery: self.on_battery,
            },
            Event::AudioActive {
                active: self.audio_active,
            },
        ];
        let hwnd = unsafe { GetForegroundWindow() };
        self.maximized = crate::maximized::covers_monitor(hwnd);
        out.push(Event::Maximized {
            maximized: self.maximized,
        });
        if !hwnd.is_invalid() {
            out.extend(self.foreground_event(hwnd));
        }
        out
    }

    fn translate(&mut self, raw: Raw) -> Vec<Event> {
        match raw {
            Raw::Foreground(hwnd) => {
                let hwnd = HWND(hwnd as *mut _);
                self.follow_foreground(hwnd);
                let mut out: Vec<Event> = self.foreground_event(hwnd).into_iter().collect();
                out.extend(self.maximized_event(hwnd));
                // Cheap one-shot read, and the moment it can change is exactly
                // an application taking the screen.
                let fullscreen = self.fullscreen_now();
                if fullscreen != self.fullscreen {
                    self.fullscreen = fullscreen;
                    out.push(Event::Fullscreen {
                        exclusive: fullscreen,
                    });
                    // The colour backend is chosen per apply, and exclusive
                    // fullscreen is where the magnifier stops reaching the
                    // screen. A tint held across that edge has to be pushed
                    // again through whichever backend works now.
                    if self.color_temp.is_some() {
                        out.push(Event::Reapply {
                            cap: CapabilityId::new(ids::DISPLAY_COLOR_TEMP),
                        });
                    }
                }
                out
            }
            Raw::Moved => {
                let hwnd = unsafe { GetForegroundWindow() };
                self.maximized_event(hwnd).into_iter().collect()
            }
            Raw::Lock(locked) => vec![Event::SessionLock { locked }],
            // Answered now and again as the configuration settles: see SETTLE.
            Raw::DisplayChanged => {
                self.arm_settle(0);
                vec![Event::DisplayConfigChanged]
            }
            Raw::ClockMoved => vec![self.clock()],
            Raw::Power(on_battery) => {
                if on_battery == self.on_battery {
                    return Vec::new();
                }
                self.on_battery = on_battery;
                vec![Event::PowerSource { on_battery }]
            }
            Raw::Timer(IDLE_TIMER) | Raw::Input => {
                if !self.idle_watched {
                    self.sink_input(false);
                    return Vec::new();
                }
                self.names.prune();
                let before = (self.idle, self.idle_seconds);
                let event = self.sample_idle();
                if before == (self.idle, self.idle_seconds) {
                    Vec::new()
                } else {
                    vec![event]
                }
            }

            // The driver has had another moment to finish. Push the change
            // again, and keep the series going until it runs out.
            Raw::Timer(DISPLAY_TIMER) => {
                let next = self.settling.map_or(SETTLE.len(), |step| step + 1);
                self.arm_settle(next);
                vec![Event::DisplayConfigChanged]
            }
            Raw::Timer(AUDIO_TIMER) => {
                let active = self.audio.playing();
                if active == self.audio_active {
                    return Vec::new();
                }
                self.audio_active = active;
                vec![Event::AudioActive { active }]
            }
            Raw::Timer(NOTICE_TIMER) => {
                self.drop_notice();
                Vec::new()
            }
            // The engine's own timer. It asked for a schedule edge or a fade
            // step. Send the time for schedules and the id for previews. It asked
            // once, and a Win32 timer repeats until killed: left alone, the
            // 40 ms fade timer went on firing after the fade, a clock event
            // twenty five times a second for the rest of the session.
            Raw::Timer(id) if (ENGINE_TIMER_BASE..IDLE_TIMER).contains(&id) => {
                let _ = unsafe { KillTimer(self.window, id) };
                vec![
                    self.clock(),
                    Event::TimerFired {
                        id: TimerId((id - ENGINE_TIMER_BASE) as u32),
                    },
                ]
            }
            Raw::Timer(_) => Vec::new(),
            Raw::TrayOpen => {
                if let Err(e) = tray::open_dashboard() {
                    eprintln!("warning: {e}");
                }
                Vec::new()
            }
            Raw::TrayMenu(x, y) => {
                self.tray_menu(x, y);
                Vec::new()
            }
            Raw::TaskbarCreated => {
                if let Some(tray) = &self.tray {
                    tray.show();
                }
                Vec::new()
            }
            Raw::Hotkey(id) => self
                .hotkeys
                .pressed(id)
                .map(|id| Event::Hotkey { id })
                .into_iter()
                .collect(),
            Raw::NoticeDone => {
                if self.notice.as_mut().is_some_and(Notice::finished) {
                    self.drop_notice();
                }
                Vec::new()
            }
        }
    }

    /// A balloon from the tray icon when there is one, from an icon added
    /// for the purpose otherwise.
    fn show_notice(&mut self, title: &str, body: &str) -> Result<(), String> {
        if let Some(tray) = &self.tray {
            return tray.notify(title, body);
        }
        let mut notice = match self.notice.take() {
            Some(notice) => notice,
            None => Notice::add(self.window)?,
        };
        let shown = notice.show(title, body);
        if shown.is_err() && notice.idle() {
            drop(notice);
            self.drop_notice();
            return shown;
        }
        self.notice = Some(notice);
        // Rearmed by every balloon, so the icon outlives the last one.
        let ms = NOTICE_LINGER.as_millis() as u32;
        unsafe { SetCoalescableTimer(self.window, NOTICE_TIMER, ms, None, ms / 10) };
        shown
    }

    fn drop_notice(&mut self) {
        self.notice = None;
        let _ = unsafe { KillTimer(self.window, NOTICE_TIMER) };
    }

    fn tray_menu(&self, x: i32, y: i32) {
        let Some(tray) = &self.tray else {
            return;
        };
        let autostart = crate::autostart::enabled();
        match tray.menu(x, y, autostart) {
            Some(tray::MENU_OPEN) => {
                if let Err(e) = tray::open_dashboard() {
                    eprintln!("warning: {e}");
                }
            }
            Some(tray::MENU_AUTOSTART) => {
                let changed = if autostart {
                    crate::autostart::disable()
                } else {
                    std::env::current_exe()
                        .map_err(|e| e.to_string())
                        .and_then(|exe| crate::autostart::enable(&exe))
                };
                if let Err(e) = changed {
                    eprintln!("warning: {e}");
                }
            }
            Some(tray::MENU_QUIT) => crate::console::request_stop(),
            _ => {}
        }
    }
}

impl Host for WinHost {
    fn monotonic_ms(&self) -> u64 {
        // SAFETY: no argument, no failure mode.
        unsafe { GetTickCount64() }
    }

    fn waker(&self) -> Option<cortisoff_host::Waker> {
        Some(std::sync::Arc::new(crate::pipe::wake))
    }

    fn set_idle_thresholds(&mut self, seconds: &[u32]) {
        self.idle_thresholds = seconds.to_vec();
        self.idle_thresholds
            .push(self.idle_after.as_secs().max(1) as u32);
        self.idle_thresholds.sort_unstable();
        self.idle_thresholds.dedup();
    }

    fn set_active(&mut self, active: bool) {
        if let Some(tray) = &mut self.tray {
            tray.set_active(active);
        }
    }
    fn probe(&self) -> CapabilitySet {
        let set = CapabilitySet::new()
            .available(CapabilityId::new(ids::APP_FOCUS), Some("winevent"))
            .available(CapabilityId::new(ids::APP_FULLSCREEN), Some("shqueryuns"))
            .available(CapabilityId::new(ids::APP_MAXIMIZED), Some("winevent"))
            .available(CapabilityId::new(ids::USER_IDLE), Some("lastinput"))
            .available(CapabilityId::new(ids::SESSION_LOCK), Some("wtsnotify"))
            .available(
                CapabilityId::new(ids::DISPLAY_CONFIG),
                Some("wm_displaychange"),
            )
            .available(CapabilityId::new(ids::POWER_SOURCE), Some("powersetting"));

        let set = if self.audio.available() {
            set.available(CapabilityId::new(ids::AUDIO_ACTIVE), Some("wasapi"))
        } else {
            set.missing(
                CapabilityId::new(ids::AUDIO_ACTIVE),
                "no audio endpoint enumerator answered on this machine",
            )
        };

        let set = match &self.color_temp {
            Some(actuator) => set.available(
                CapabilityId::new(ids::DISPLAY_COLOR_TEMP),
                Some(actuator.backend_name()),
            ),
            None => set.missing(
                CapabilityId::new(ids::DISPLAY_COLOR_TEMP),
                "neither the magnifier nor a gamma ramp answered on this machine",
            ),
        };

        let set = match &self.display_mode {
            Some(actuator) => set.available(
                CapabilityId::new(ids::DISPLAY_MODE),
                Some(actuator.backend_name()),
            ),
            None => set.missing(
                CapabilityId::new(ids::DISPLAY_MODE),
                "no output would report a display mode",
            ),
        };

        let set = match &self.wallpaper {
            Some(actuator) => set.available(
                CapabilityId::new(ids::WALLPAPER_PLAYBACK),
                Some(actuator.backend_name()),
            ),
            None => set.missing(
                CapabilityId::new(ids::WALLPAPER_PLAYBACK),
                "neither Wallpaper Engine nor Lively is installed or running",
            ),
        };

        match &self.vibrance {
            Some(actuator) => set.available(
                CapabilityId::new(ids::DISPLAY_VIBRANCE),
                Some(actuator.backend_name()),
            ),
            None => set.missing(
                CapabilityId::new(ids::DISPLAY_VIBRANCE),
                "no NVIDIA output answered NVAPI. AMD goes through ADL, which is not written yet",
            ),
        }
    }

    /// Only what some plugin watches. An installed hook costs on every system-wide
    /// event, so a plugin set that never watches the focused application must
    /// not install the foreground hook at all.
    fn install_sources(&mut self, wanted: &[CapabilityId]) -> Result<(), ActuatorError> {
        let asked = |id: &str| wanted.iter().any(|c| c.as_str() == id);

        // A plugin reading only whether something is maximised still needs to
        // know when the focus moves to a window that is, so the foreground
        // hook serves both.
        let foreground = asked(ids::APP_FOCUS) || asked(ids::APP_MAXIMIZED);
        match (foreground, self.foreground_hook) {
            (true, None) => {
                self.foreground_hook = Some(sources::hook_foreground().ok_or_else(|| {
                    ActuatorError::on(
                        CapabilityId::new(ids::APP_FOCUS),
                        "SetWinEventHook refused the foreground hook",
                    )
                })?);
            }
            (false, Some(hook)) => {
                sources::unhook_foreground(hook);
                self.foreground_hook = None;
            }
            _ => {}
        }

        let maximized = asked(ids::APP_MAXIMIZED);
        if maximized != self.location_wanted {
            self.location_wanted = maximized;
            let foreground = if maximized {
                unsafe { GetForegroundWindow() }
            } else {
                HWND::default()
            };
            if !self.follow_foreground(foreground) {
                return Err(ActuatorError::on(
                    CapabilityId::new(ids::APP_MAXIMIZED),
                    "SetWinEventHook refused the location hook",
                ));
            }
        }

        match (asked(ids::SESSION_LOCK), self.session_watched) {
            (true, false) => self.session_watched = sources::watch_session(self.window),
            (false, true) => {
                sources::unwatch_session(self.window);
                self.session_watched = false;
            }
            _ => {}
        }

        match (asked(ids::POWER_SOURCE), self.power_watch) {
            (true, None) => {
                self.power_watch = Some(crate::power::watch(self.window).ok_or_else(|| {
                    ActuatorError::on(
                        CapabilityId::new(ids::POWER_SOURCE),
                        "RegisterPowerSettingNotification refused the power source",
                    )
                })?);
            }
            (false, Some(handle)) => {
                crate::power::unwatch(handle);
                self.power_watch = None;
            }
            _ => {}
        }

        self.idle_watched = asked(ids::USER_IDLE);
        if self.idle_watched {
            self.arm_idle_timer();
        } else {
            let _ = unsafe { KillTimer(self.window, IDLE_TIMER) };
            self.sink_input(false);
        }

        match (asked(ids::AUDIO_ACTIVE), self.audio_watched) {
            (true, false) => {
                let ms = AUDIO_EVERY.as_millis() as u32;
                unsafe { SetCoalescableTimer(self.window, AUDIO_TIMER, ms, None, ms / 2) };
                self.audio_watched = true;
            }
            (false, true) => {
                let _ = unsafe { KillTimer(self.window, AUDIO_TIMER) };
                self.audio_watched = false;
            }
            _ => {}
        }

        Ok(())
    }

    fn actuator(&self, cap: &CapabilityId) -> Option<&dyn Actuator> {
        match cap.as_str() {
            ids::DISPLAY_COLOR_TEMP => self.color_temp.as_ref().map(|a| a as &dyn Actuator),
            ids::DISPLAY_VIBRANCE => self.vibrance.as_ref().map(|a| a as &dyn Actuator),
            ids::DISPLAY_MODE => self.display_mode.as_ref().map(|a| a as &dyn Actuator),
            ids::WALLPAPER_PLAYBACK => self.wallpaper.as_ref().map(|a| a as &dyn Actuator),
            _ => None,
        }
    }

    fn set_timer(&mut self, id: TimerId, after: Duration, tolerance: Duration) {
        let ms = after.as_millis().min(u128::from(u32::MAX)) as u32;
        let slack = tolerance.as_millis().min(u128::from(u32::MAX)) as u32;
        unsafe {
            SetCoalescableTimer(
                self.window,
                ENGINE_TIMER_BASE + id.0 as usize,
                ms.max(1),
                None,
                slack,
            )
        };
    }

    fn cancel_timer(&mut self, id: TimerId) {
        let _ = unsafe { KillTimer(self.window, ENGINE_TIMER_BASE + id.0 as usize) };
    }

    fn time_of_day(&self) -> Option<Event> {
        Some(self.clock())
    }

    fn set_hotkeys(&mut self, wanted: &[Hotkey]) -> Vec<(u32, String)> {
        self.hotkeys.set(wanted)
    }

    fn send_keys(&mut self, chord: &Chord) -> Result<(), String> {
        crate::input::send_chord(chord)
    }

    fn type_text(&mut self, text: &str) -> Result<(), String> {
        crate::input::type_text(text)
    }

    fn notify(&mut self, title: &str, body: &str) -> Result<(), String> {
        self.show_notice(title, body)
    }

    fn clipboard(&self) -> Result<String, String> {
        crate::clipboard::read(self.window)
    }

    fn set_clipboard(&mut self, text: &str) -> Result<(), String> {
        crate::clipboard::write(self.window, text)
    }

    fn windows(&self) -> Vec<WindowInfo> {
        crate::toplevel::list()
    }

    fn window_action(&mut self, id: u64, action: WindowAction) -> Result<(), String> {
        crate::toplevel::act(id, action)
    }

    fn poll(&mut self, timeout: Duration) -> Vec<Event> {
        if !self.snapshot_sent {
            self.snapshot_sent = true;
            return self.snapshot();
        }

        // `u32::MAX` is INFINITE. Blocks until a message arrives, a pipe client
        // sends something, or something calls `pipe::wake`. This is the whole
        // cost of an idle daemon: one thread parked in the kernel.
        let mut ms = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
        let arrived = crate::pipe::arrived();
        if arrived.is_none() {
            // Without the event nothing but a message can wake the wait, and
            // a pipe client or a Ctrl-C would go unseen.
            ms = ms.min(FALLBACK_TICK_MS);
        }
        let handles = arrived.as_ref().map(std::slice::from_ref);
        unsafe {
            MsgWaitForMultipleObjectsEx(handles, ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }

        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            // No TranslateMessage: this window has no keyboard input to
            // translate, and the callbacks have already recorded what matters.
            unsafe { DispatchMessageW(&message) };
        }

        sources::drain()
            .into_iter()
            .flat_map(|raw| self.translate(raw))
            .collect()
    }
}

impl Drop for WinHost {
    fn drop(&mut self) {
        // Before the window goes, or the icon lingers until a mouse passes
        // over it.
        self.tray = None;
        self.drop_notice();
        // Also tied to the window, and unregistered while it still exists.
        self.hotkeys.clear();
        if let Some(hook) = self.foreground_hook.take() {
            sources::unhook_foreground(hook);
        }
        if let Some(hook) = self.location_hook.take() {
            sources::unhook_foreground(hook);
        }
        if self.session_watched {
            sources::unwatch_session(self.window);
        }
        if let Some(handle) = self.power_watch.take() {
            crate::power::unwatch(handle);
        }
        let _ = unsafe { KillTimer(self.window, IDLE_TIMER) };
        let _ = unsafe { KillTimer(self.window, AUDIO_TIMER) };
        self.sink_input(false);
        let _ = unsafe { DestroyWindow(self.window) };
    }
}

/// Silences the unused-import warnings for the types that only appear inside
/// the window procedure's signature.
const _: fn(WPARAM, LPARAM, PCWSTR) = |_, _, _| {};

/// Emit only threshold crossings, rather than one core event per idle poll.
fn idle_bucket(seconds: u32, thresholds: &[u32]) -> u32 {
    thresholds
        .iter()
        .copied()
        .filter(|t| *t <= seconds)
        .max()
        .unwrap_or(0)
}

/// Seconds until the next threshold above `seconds`, `None` past the last.
fn next_threshold_in(seconds: u32, thresholds: &[u32]) -> Option<u32> {
    thresholds
        .iter()
        .filter_map(|t| t.checked_sub(seconds).filter(|d| *d > 0))
        .min()
}

#[cfg(test)]
mod idle_tests {
    use super::*;

    #[test]
    fn each_configured_delay_has_its_own_crossing() {
        let thresholds = [60, 120, 300];
        assert_eq!(idle_bucket(59, &thresholds), 0);
        assert_eq!(idle_bucket(60, &thresholds), 60);
        assert_eq!(idle_bucket(119, &thresholds), 60);
        assert_eq!(idle_bucket(120, &thresholds), 120);
        assert_eq!(idle_bucket(0, &thresholds), 0);
    }

    /// The timer lands on the next crossing and nowhere before it: an active
    /// user costs one wakeup per threshold, an absent one none past the last.
    #[test]
    fn the_idle_timer_waits_for_the_next_crossing_only() {
        let thresholds = [60, 120, 300];
        assert_eq!(next_threshold_in(0, &thresholds), Some(60));
        assert_eq!(next_threshold_in(59, &thresholds), Some(1));
        assert_eq!(next_threshold_in(60, &thresholds), Some(60));
        assert_eq!(next_threshold_in(200, &thresholds), Some(100));
        assert_eq!(next_threshold_in(300, &thresholds), None);
        assert_eq!(next_threshold_in(4000, &thresholds), None);
    }
}
