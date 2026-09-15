//! The message pump, and the [`Host`] built on it.
//!
//! One hidden window carries everything: the session notifications, the
//! display, clock and power broadcasts, the tray icon and every timer. It is a
//! real top-level window that is never shown, not a message-only one: a
//! message-only window receives no broadcast at all, so `WM_DISPLAYCHANGE`,
//! `WM_TIMECHANGE` and `TaskbarCreated` would never arrive.

use std::time::Duration;

use cortisoff_host::{
    ids, Actuator, ActuatorError, CapabilityId, CapabilitySet, Event, Host, TimerId,
};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::HPOWERNOTIFY;
use windows::Win32::System::SystemInformation::{GetLocalTime, GetTickCount};
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
use crate::mode::DisplayMode;
use crate::names::Names;
use crate::sources::{self, Raw};
use crate::tray::{self, Tray};
use crate::vibrance::Vibrance;
use crate::wallpaper::Wallpaper;

/// The daemon's own beats, kept away from the engine's timer ids.
const IDLE_TIMER: usize = 0x0100_0000;
const AUDIO_TIMER: usize = 0x0100_0001;
/// Engine timer ids are shifted by one, because a Windows timer id of zero is
/// legal but easy to confuse with "no timer".
const ENGINE_TIMER_BASE: usize = 1;

/// How long without input counts as away. Windows emits no idle event at all,
/// so this is a poll, one of the two in the whole daemon.
const DEFAULT_IDLE_AFTER: Duration = Duration::from_secs(5 * 60);

/// How often `audio.active` is read while a rule reads it. A meter has no
/// event, and a film starting is not a frame deadline.
const AUDIO_EVERY: Duration = Duration::from_secs(30);

pub struct WinHost {
    window: HWND,
    foreground_hook: Option<HWINEVENTHOOK>,
    location_hook: Option<HWINEVENTHOOK>,
    session_watched: bool,
    power_watch: Option<HPOWERNOTIFY>,
    audio_watched: bool,
    idle_after: Duration,
    /// Last state pushed, so a beat that changes nothing pushes nothing.
    idle: bool,
    fullscreen: bool,
    maximized: bool,
    on_battery: bool,
    audio_active: bool,
    names: Names,
    audio: Audio,
    tray: Option<Tray>,
    color_temp: Option<ColorTemp>,
    vibrance: Option<Vibrance>,
    display_mode: Option<DisplayMode>,
    wallpaper: Option<Wallpaper>,
    /// The first poll answers with a snapshot: rules are predicates over a
    /// world, so they need the world before they can mean anything.
    snapshot_sent: bool,
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
            session_watched: false,
            power_watch: None,
            audio_watched: false,
            idle_after: DEFAULT_IDLE_AFTER,
            idle: false,
            fullscreen: false,
            maximized: false,
            on_battery: false,
            audio_active: false,
            names: Names::default(),
            audio: Audio::new(),
            tray: None,
            color_temp: ColorTemp::new(),
            vibrance: Vibrance::new(),
            display_mode: DisplayMode::new(),
            wallpaper: Wallpaper::new(),
            snapshot_sent: false,
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
        // Only when it would surprise someone writing a rule. Every NVIDIA
        // driver seen so far reports 0 to 100 with 50 as neutral, and saying
        // so every time would be noise.
        if let Some((min, max, default)) = self.vibrance.as_ref().and_then(Vibrance::range) {
            if (min, max, default) != (0, 100, 50) {
                out.push(format!(
                    "this driver reports vibrance from {min} to {max}, neutral at {default}, not the usual 0 to 100. a rule's value is clamped to that range"
                ));
            }
        }
        out
    }

    /// How long without input counts as away. The rule format says `idle =
    /// true`, not a duration, so the threshold is the host's and it is the
    /// same for every rule.
    pub fn idle_after(mut self, after: Duration) -> Self {
        self.idle_after = after;
        self
    }

    fn clock(&self) -> Event {
        let now = unsafe { GetLocalTime() };
        Event::TimeOfDay {
            minute: now.wHour * 60 + now.wMinute,
            second: now.wSecond.min(59) as u8,
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

    fn idle_now(&self) -> bool {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if unsafe { GetLastInputInfo(&mut info) }.as_bool() {
            // Both are tick counts and both wrap at the same place, so the
            // wrapping difference is the right answer even across 49 days.
            let since = unsafe { GetTickCount() }.wrapping_sub(info.dwTime);
            u64::from(since) >= self.idle_after.as_millis() as u64
        } else {
            false
        }
    }

    /// Away means no input, nothing fullscreen and nothing audible. Checked in
    /// that order, cheapest first, so a person at the keyboard never costs a
    /// walk through the audio sessions.
    fn away_now(&self) -> bool {
        self.idle_now() && !self.fullscreen_now() && !self.audio.playing()
    }

    /// The idle beat: half the threshold, never tighter than fifteen seconds,
    /// and with generous slack so Windows folds it into a wakeup it was going
    /// to make anyway.
    fn arm_idle_timer(&self) {
        let period = (self.idle_after / 2).max(Duration::from_secs(15));
        let ms = period.as_millis().min(u128::from(u32::MAX)) as u32;
        unsafe { SetCoalescableTimer(self.window, IDLE_TIMER, ms, None, ms / 2) };
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

    /// Everything a rule could already be true about, pushed before the first
    /// real event so no rule fires on a half-known world.
    fn snapshot(&mut self) -> Vec<Event> {
        self.fullscreen = self.fullscreen_now();
        self.idle = self.away_now();
        self.on_battery = crate::power::on_battery();
        self.audio_active = self.audio.playing();
        let mut out = vec![
            self.clock(),
            Event::Fullscreen {
                exclusive: self.fullscreen,
            },
            Event::Idle { idle: self.idle },
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
            Raw::DisplayChanged => vec![Event::DisplayConfigChanged],
            Raw::ClockMoved => vec![self.clock()],
            Raw::Power(on_battery) => {
                if on_battery == self.on_battery {
                    return Vec::new();
                }
                self.on_battery = on_battery;
                vec![Event::PowerSource { on_battery }]
            }
            Raw::Timer(IDLE_TIMER) => {
                self.names.prune();
                let idle = self.away_now();
                if idle == self.idle {
                    return Vec::new();
                }
                self.idle = idle;
                vec![Event::Idle { idle }]
            }
            Raw::Timer(AUDIO_TIMER) => {
                let active = self.audio.playing();
                if active == self.audio_active {
                    return Vec::new();
                }
                self.audio_active = active;
                vec![Event::AudioActive { active }]
            }
            // The engine's own timer. It asked for a schedule edge or a fade
            // step, so the answer is the time, not the timer id. It asked
            // once, and a Win32 timer repeats until killed: left alone, the
            // 40 ms fade timer went on firing after the fade, a clock event
            // twenty five times a second for the rest of the session.
            Raw::Timer(id) if id < IDLE_TIMER => {
                let _ = unsafe { KillTimer(self.window, id) };
                vec![self.clock()]
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
        }
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

    /// Only what some rule reads. An installed hook costs on every system-wide
    /// event, so a rule set that never mentions the focused application must
    /// not install the foreground hook at all.
    fn install_sources(&mut self, wanted: &[CapabilityId]) -> Result<(), ActuatorError> {
        let asked = |id: &str| wanted.iter().any(|c| c.as_str() == id);

        // A rule reading only whether something is maximised still needs to
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

        match (asked(ids::APP_MAXIMIZED), self.location_hook) {
            (true, None) => {
                self.location_hook = Some(sources::hook_location().ok_or_else(|| {
                    ActuatorError::on(
                        CapabilityId::new(ids::APP_MAXIMIZED),
                        "SetWinEventHook refused the location hook",
                    )
                })?);
            }
            (false, Some(hook)) => {
                sources::unhook_foreground(hook);
                self.location_hook = None;
            }
            _ => {}
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

        if asked(ids::USER_IDLE) {
            self.arm_idle_timer();
        } else {
            let _ = unsafe { KillTimer(self.window, IDLE_TIMER) };
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

    fn poll(&mut self, timeout: Duration) -> Vec<Event> {
        if !self.snapshot_sent {
            self.snapshot_sent = true;
            return self.snapshot();
        }

        let ms = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
        // Blocks until a message arrives or the timeout runs out. This is the
        // whole cost of an idle daemon: one thread parked in the kernel.
        unsafe {
            MsgWaitForMultipleObjectsEx(None, ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
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
        let _ = unsafe { DestroyWindow(self.window) };
    }
}

/// Silences the unused-import warnings for the types that only appear inside
/// the window procedure's signature.
const _: fn(WPARAM, LPARAM, PCWSTR) = |_, _, _| {};
