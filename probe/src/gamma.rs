//! Backend 2: the GDI gamma ramp, per monitor.
//!
//! The only backend that still applies in exclusive fullscreen, and the only
//! one that survives the process that set it. A crash here is what leaves a
//! screen orange, so in the daemon this actuator is non-volatile and journals
//! its baseline before the first apply.
//!
//! Windows clamps the usable range unless GdiIcmGammaRange is set to 256, which
//! is why `range_unlocked` exists: without it the filter looks weak and users
//! report it as a bug rather than as a missing registry value.

use std::ffi::c_void;

use windows::core::{PCWSTR, Result};
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};
use windows::Win32::UI::ColorSystem::{GetDeviceGammaRamp, SetDeviceGammaRamp};

use crate::color::Rgb;

pub type Ramp = [u16; 256 * 3];

/// True when the ICM clamp has been lifted. Admin once, plus a reboot.
pub fn range_unlocked() -> bool {
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            windows::core::w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ICM"),
            windows::core::w!("GdiIcmGammaRange"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    err.is_ok() && value == 256
}

fn device_name(h: HMONITOR) -> Option<Vec<u16>> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe { GetMonitorInfoW(h, &mut info.monitorInfo as *mut _) };
    if !ok.as_bool() {
        return None;
    }
    Some(info.szDevice.to_vec())
}

/// A DC for one monitor. GetDC(None) would only ever reach the primary, and
/// every colour change in this project is per monitor by rule.
fn dc(h: HMONITOR) -> Option<HDC> {
    let name = device_name(h)?;
    let hdc = unsafe { CreateDCW(PCWSTR(name.as_ptr()), PCWSTR::null(), PCWSTR::null(), None) };
    if hdc.is_invalid() {
        None
    } else {
        Some(hdc)
    }
}

pub fn capture(h: HMONITOR) -> Option<Ramp> {
    let hdc = dc(h)?;
    let mut ramp: Ramp = [0; 256 * 3];
    let ok = unsafe { GetDeviceGammaRamp(hdc, ramp.as_mut_ptr() as *mut c_void) };
    unsafe { let _ = DeleteDC(hdc); };
    ok.as_bool().then_some(ramp)
}

pub fn apply(h: HMONITOR, c: &Rgb) -> Result<()> {
    let mut ramp: Ramp = [0; 256 * 3];
    for i in 0..256usize {
        let base = (i as f32) / 255.0;
        let v = |m: f32| ((base * m * 65535.0).round() as u32).min(65535) as u16;
        ramp[i] = v(c.r);
        ramp[256 + i] = v(c.g);
        ramp[512 + i] = v(c.b);
    }
    set(h, &ramp)
}

pub fn set(h: HMONITOR, ramp: &Ramp) -> Result<()> {
    let hdc = dc(h).ok_or_else(|| windows::core::Error::from_win32())?;
    let ok = unsafe { SetDeviceGammaRamp(hdc, ramp.as_ptr() as *const c_void) };
    unsafe { let _ = DeleteDC(hdc); };
    if ok.as_bool() {
        Ok(())
    } else {
        Err(windows::core::Error::from_win32())
    }
}

pub fn reset(h: HMONITOR) -> Result<()> {
    apply(
        h,
        &Rgb {
            r: 1.0,
            g: 1.0,
            b: 1.0,
        },
    )
}
