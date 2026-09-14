//! `audio.active`: is something audible playing on the default output.
//!
//! Polled, and only on a slow beat while a rule reads it. Windows does offer
//! per-session state callbacks, but a session being "active" is not the same
//! thing as sound: Discord and Steam hold an active session all day in
//! silence. What answers the real question is the session's peak meter, and a
//! meter has no event. The idle beat already runs every few minutes, so the
//! idle gate costs nothing extra.

use windows::core::Interface;
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, AudioSessionStateActive, IAudioSessionManager2, IMMDeviceEnumerator,
    MMDeviceEnumerator,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};

/// Below this a meter is reading noise, not a film.
const AUDIBLE: f32 = 0.0005;

pub struct Audio {
    enumerator: Option<IMMDeviceEnumerator>,
}

impl Audio {
    /// Initialises COM on the calling thread, which is the pump's.
    pub fn new() -> Self {
        // Already initialised is fine, and so is a different apartment: the
        // enumerator below either works or it does not.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let enumerator = unsafe {
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
        }
        .ok();
        Self { enumerator }
    }

    pub fn available(&self) -> bool {
        self.enumerator.is_some()
    }

    /// True while any session on the default output is active and not silent.
    pub fn playing(&self) -> bool {
        let Some(enumerator) = &self.enumerator else {
            return false;
        };
        unsafe {
            let Ok(device) = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) else {
                return false;
            };
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                return false;
            };
            let Ok(sessions) = manager.GetSessionEnumerator() else {
                return false;
            };
            for i in 0..sessions.GetCount().unwrap_or(0) {
                let Ok(session) = sessions.GetSession(i) else {
                    continue;
                };
                if session.GetState().ok() != Some(AudioSessionStateActive) {
                    continue;
                }
                let Ok(meter) = session.cast::<IAudioMeterInformation>() else {
                    continue;
                };
                if meter.GetPeakValue().unwrap_or(0.0) > AUDIBLE {
                    return true;
                }
            }
        }
        false
    }
}
