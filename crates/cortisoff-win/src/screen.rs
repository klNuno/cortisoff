//! Monitors: enumerating them, reading their gamma ramp, writing it back, and
//! asking whether HDR is on.
//!
//! Everything in this file is per monitor. Most of VibranceGUI's bug reports
//! trace back to treating any of it as global.

use std::ffi::c_void;
use std::mem::size_of;

use windows::core::PCWSTR;
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};
use windows::Win32::UI::ColorSystem::{GetDeviceGammaRamp, SetDeviceGammaRamp};

use crate::color::Ramp;

/// A monitor's device name, `\\.\DISPLAY1` and friends. Stable across a
/// session, which is what the journal keys on.
pub fn device_names() -> Vec<String> {
    handles().into_iter().map(|(name, _)| name).collect()
}

/// Every monitor with its device name, for the calls that want the handle.
pub fn handles() -> Vec<(String, HMONITOR)> {
    monitors()
        .into_iter()
        .filter_map(|h| device_name(h).map(|n| (trim(&n), h)))
        .collect()
}

pub fn capture(device: &str) -> Option<Ramp> {
    let hdc = dc(device)?;
    let mut ramp: Ramp = [0; 768];
    let ok = unsafe { GetDeviceGammaRamp(hdc, ramp.as_mut_ptr() as *mut c_void) };
    unsafe {
        let _ = DeleteDC(hdc);
    }
    ok.as_bool().then_some(ramp)
}

pub fn write(device: &str, ramp: &Ramp) -> bool {
    let Some(hdc) = dc(device) else {
        return false;
    };
    let ok = unsafe { SetDeviceGammaRamp(hdc, ramp.as_ptr() as *const c_void) };
    unsafe {
        let _ = DeleteDC(hdc);
    }
    ok.as_bool()
}

/// One output's HDR state, keyed on the same device name as its ramp.
pub struct Hdr {
    pub device: String,
    /// The output can turn HDR on, whether or not it is on now.
    pub supported: bool,
    pub enabled: bool,
}

/// HDR per output. No software colour path works on an output with HDR on:
/// Night Light is unavailable and the gamma ramp is undefined, so the panel
/// itself has to do it through DDC/CI.
pub fn hdr_outputs() -> Vec<Hdr> {
    let mut out: Vec<Hdr> = Vec::new();
    for path in active_paths() {
        let Some(device) = source_name(&path) else {
            continue;
        };
        // Bit 0 is advancedColorSupported, bit 1 advancedColorEnabled.
        let bits = advanced_color(&path).unwrap_or(0);
        let (supported, enabled) = (bits & 0b01 != 0, bits & 0b10 != 0);
        match out.iter_mut().find(|o| o.device == device) {
            // A cloned source drives several targets, and one with HDR on is
            // enough to take the ramp away from all of them.
            Some(o) => {
                o.supported |= supported;
                o.enabled |= enabled;
            }
            None => out.push(Hdr {
                device,
                supported,
                enabled,
            }),
        }
    }
    out
}

/// True when the ICM clamp has been lifted. Admin once, plus a reboot, and
/// without it a colour filter is visibly weak. Reported by the doctor rather
/// than fixed silently.
pub fn gamma_range_unlocked() -> bool {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
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

fn active_paths() -> Vec<DISPLAYCONFIG_PATH_INFO> {
    let mut n_paths = 0u32;
    let mut n_modes = 0u32;
    if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes) }
        .is_err()
    {
        return Vec::new();
    }

    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
    if unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut n_paths,
            paths.as_mut_ptr(),
            &mut n_modes,
            modes.as_mut_ptr(),
            None,
        )
    }
    .is_err()
    {
        return Vec::new();
    }
    paths.truncate(n_paths as usize);
    paths
}

/// The GDI name of the source a path starts from, which is what ties the CCD
/// view of an output to its ramp and to its physical monitors.
fn source_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<String> {
    let mut info = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
            adapterId: path.sourceInfo.adapterId,
            id: path.sourceInfo.id,
        },
        ..Default::default()
    };
    if unsafe { DisplayConfigGetDeviceInfo(&mut info.header) } != 0 {
        return None;
    }
    Some(trim(&info.viewGdiDeviceName))
}

fn advanced_color(path: &DISPLAYCONFIG_PATH_INFO) -> Option<u32> {
    let mut info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            size: size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
            adapterId: path.targetInfo.adapterId,
            id: path.targetInfo.id,
        },
        ..Default::default()
    };
    if unsafe { DisplayConfigGetDeviceInfo(&mut info.header) } != 0 {
        return None;
    }
    Some(unsafe { info.Anonymous.value })
}

fn monitors() -> Vec<HMONITOR> {
    let mut found: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut found as *mut Vec<HMONITOR> as isize),
        );
    }
    found
}

unsafe extern "system" fn collect(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let found = &mut *(data.0 as *mut Vec<HMONITOR>);
    found.push(h);
    TRUE
}

fn device_name(h: HMONITOR) -> Option<[u16; 32]> {
    let mut info = MONITORINFOEXW {
        monitorInfo: windows::Win32::Graphics::Gdi::MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(h, &mut info.monitorInfo as *mut _) }
        .as_bool()
        .then_some(info.szDevice)
}

fn trim(raw: &[u16; 32]) -> String {
    let end = raw.iter().position(|c| *c == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..end])
}

/// A DC for one monitor by name. `GetDC(None)` would only ever reach the
/// primary.
fn dc(device: &str) -> Option<HDC> {
    let wide: Vec<u16> = device.encode_utf16().chain(std::iter::once(0)).collect();
    let hdc = unsafe { CreateDCW(PCWSTR(wide.as_ptr()), PCWSTR::null(), PCWSTR::null(), None) };
    (!hdc.is_invalid()).then_some(hdc)
}
