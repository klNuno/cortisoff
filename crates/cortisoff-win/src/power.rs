//! `power.source`: on battery or on mains.
//!
//! A real notification, not a poll. `RegisterPowerSettingNotification` sends
//! the current state once at registration and again at every change, so the
//! window procedure is the only place that ever reads it after startup.

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, RegisterPowerSettingNotification, UnregisterPowerSettingNotification,
    HPOWERNOTIFY, POWERBROADCAST_SETTING, SYSTEM_POWER_STATUS,
};
use windows::Win32::System::SystemServices::GUID_ACDC_POWER_SOURCE;
use windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_WINDOW_HANDLE;

/// Whether the machine runs on battery right now. A desktop answers false,
/// and a machine that cannot say is treated as plugged in: dimming a screen on
/// a guess is worse than not dimming it.
pub fn on_battery() -> bool {
    let mut status = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() {
        return false;
    }
    status.ACLineStatus == 0
}

pub fn watch(window: HWND) -> Option<HPOWERNOTIFY> {
    unsafe {
        RegisterPowerSettingNotification(
            HANDLE(window.0),
            &GUID_ACDC_POWER_SOURCE,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        )
    }
    .ok()
}

pub fn unwatch(handle: HPOWERNOTIFY) {
    let _ = unsafe { UnregisterPowerSettingNotification(handle) };
}

/// The payload of a `PBT_POWERSETTINGCHANGE`, when it is the AC/DC one.
/// Zero is mains, one is battery, two is a UPS, which is a battery too.
///
/// # Safety
/// `lparam` must be the one Windows passed with that broadcast.
pub unsafe fn read_change(lparam: LPARAM) -> Option<bool> {
    let setting = lparam.0 as *const POWERBROADCAST_SETTING;
    if setting.is_null()
        || (*setting).PowerSetting != GUID_ACDC_POWER_SOURCE
        || (*setting).DataLength < 4
    {
        return None;
    }
    let data = std::ptr::addr_of!((*setting).Data) as *const u32;
    Some(data.read_unaligned() != 0)
}
