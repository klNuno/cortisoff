//! Reading the display context that decides which colour backend can work.
//!
//! Everything here is a one-shot read, triggered by an event in the real
//! daemon. Nothing in this file may end up on a timer.

use std::mem::size_of;

use windows::core::Result;
use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_ACCEPTS_NOTIFICATIONS, QUNS_APP, QUNS_BUSY,
    QUNS_NOT_PRESENT, QUNS_PRESENTATION_MODE, QUNS_QUIET_TIME, QUNS_RUNNING_D3D_FULL_SCREEN,
};

pub struct Output {
    pub name: String,
    pub hdr_supported: bool,
    pub hdr_enabled: bool,
}

/// Per-output HDR state, read through the CCD API.
///
/// This is what picks the DDC/CI backend: with HDR on, no software colour path
/// works at all, so the panel has to do it.
pub fn outputs() -> Result<Vec<Output>> {
    let mut n_paths = 0u32;
    let mut n_modes = 0u32;
    unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes) }
        .ok()?;

    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
    unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut n_paths,
            paths.as_mut_ptr(),
            &mut n_modes,
            modes.as_mut_ptr(),
            None,
        )
    }
    .ok()?;
    paths.truncate(n_paths as usize);

    let mut out = Vec::new();
    for p in &paths {
        let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
        name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
        name.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
        name.header.adapterId = p.targetInfo.adapterId;
        name.header.id = p.targetInfo.id;
        let friendly = if unsafe { DisplayConfigGetDeviceInfo(&mut name.header) } == 0 {
            String::from_utf16_lossy(&name.monitorFriendlyDeviceName)
                .trim_end_matches('\0')
                .to_string()
        } else {
            format!("target {}", p.targetInfo.id)
        };

        let mut aci = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO::default();
        aci.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO;
        aci.header.size = size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32;
        aci.header.adapterId = p.targetInfo.adapterId;
        aci.header.id = p.targetInfo.id;

        let (supported, enabled) = if unsafe { DisplayConfigGetDeviceInfo(&mut aci.header) } == 0 {
            // Bit 0 advancedColorSupported, bit 1 advancedColorEnabled.
            let bits = unsafe { aci.Anonymous.value };
            (bits & 0b01 != 0, bits & 0b10 != 0)
        } else {
            (false, false)
        };

        out.push(Output {
            name: friendly,
            hdr_supported: supported,
            hdr_enabled: enabled,
        });
    }
    Ok(out)
}

/// True when an exclusive-fullscreen D3D application owns the screen, which is
/// the one case where only the gamma ramp still applies.
pub fn exclusive_fullscreen() -> bool {
    matches!(notification_state(), Some(s) if s == "QUNS_RUNNING_D3D_FULL_SCREEN")
}

pub fn notification_state() -> Option<&'static str> {
    let s = unsafe { SHQueryUserNotificationState() }.ok()?;
    Some(match s {
        QUNS_NOT_PRESENT => "QUNS_NOT_PRESENT",
        QUNS_BUSY => "QUNS_BUSY",
        QUNS_RUNNING_D3D_FULL_SCREEN => "QUNS_RUNNING_D3D_FULL_SCREEN",
        QUNS_PRESENTATION_MODE => "QUNS_PRESENTATION_MODE",
        QUNS_ACCEPTS_NOTIFICATIONS => "QUNS_ACCEPTS_NOTIFICATIONS",
        QUNS_QUIET_TIME => "QUNS_QUIET_TIME",
        QUNS_APP => "QUNS_APP",
        _ => "unknown",
    })
}

pub fn monitors() -> Vec<HMONITOR> {
    let mut v: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut v as *mut Vec<HMONITOR> as isize),
        );
    }
    v
}

unsafe extern "system" fn collect(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let v = &mut *(data.0 as *mut Vec<HMONITOR>);
    v.push(h);
    TRUE
}
