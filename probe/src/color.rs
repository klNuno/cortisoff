//! Kelvin to per-channel multipliers.
//!
//! Tanner Helland's blackbody approximation, normalised so that the brightest
//! channel is always 1.0. Every backend needs the same three numbers, so the
//! maths lives here once and none of them owns it.

pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

pub fn from_kelvin(kelvin: f32) -> Rgb {
    let t = kelvin.clamp(1000.0, 40000.0) / 100.0;

    let r = if t <= 66.0 {
        255.0
    } else {
        329.698_73 * (t - 60.0).powf(-0.133_204_76)
    };

    let g = if t <= 66.0 {
        99.470_8 * t.ln() - 161.119_57
    } else {
        288.122_16 * (t - 60.0).powf(-0.075_514_85)
    };

    let b = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_73 * (t - 10.0).ln() - 305.044_8
    };

    let (r, g, b) = (
        r.clamp(0.0, 255.0),
        g.clamp(0.0, 255.0),
        b.clamp(0.0, 255.0),
    );

    // Normalise on the peak channel: a colour temperature must never dim the
    // screen, only tint it. Brightness is a separate capability.
    let peak = r.max(g).max(b).max(1.0);
    Rgb {
        r: r / peak,
        g: g / peak,
        b: b / peak,
    }
}
