//! Backend 1: the fullscreen magnifier colour effect.
//!
//! The default on desktop SDR. It runs through the compositor, so it is the
//! cleanest of the four, and the effect is owned by this process: killing the
//! process is supposed to drop it. Proving that claim is the point of the
//! `hold` command, because the whole `Actuator::volatile()` design rests on it.
//!
//! It does not apply in exclusive fullscreen. That is why the gamma backend
//! still exists.

use windows::core::Result;
use windows::Win32::UI::Magnification::{
    MagInitialize, MagSetFullscreenColorEffect, MagUninitialize, MAGCOLOREFFECT,
};

use crate::color::Rgb;

pub fn init() -> Result<()> {
    unsafe { MagInitialize() }.ok()
}

pub fn shutdown() -> Result<()> {
    unsafe { MagUninitialize() }.ok()
}

/// Row-major 5x5, applied as a row vector times the matrix. Scaling the
/// diagonal tints without touching alpha or the translation row.
pub fn apply(c: &Rgb) -> Result<()> {
    #[rustfmt::skip]
    let effect = MAGCOLOREFFECT {
        transform: [
            c.r, 0.0, 0.0, 0.0, 0.0,
            0.0, c.g, 0.0, 0.0, 0.0,
            0.0, 0.0, c.b, 0.0, 0.0,
            0.0, 0.0, 0.0, 1.0, 0.0,
            0.0, 0.0, 0.0, 0.0, 1.0,
        ],
    };
    unsafe { MagSetFullscreenColorEffect(&effect) }.ok()
}

pub fn reset() -> Result<()> {
    apply(&Rgb {
        r: 1.0,
        g: 1.0,
        b: 1.0,
    })
}
