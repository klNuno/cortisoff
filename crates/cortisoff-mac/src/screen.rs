//! The displays, as CoreGraphics lists them.

use objc2_core_graphics::{CGDisplayBounds, CGError, CGGetActiveDisplayList};

use crate::fit::Rect;

/// Every active display. A mirror set counts once per member, which is what
/// a transfer table write wants: each panel has its own table.
pub fn active() -> Vec<u32> {
    let mut ids = [0u32; 32];
    let mut count = 0u32;
    // SAFETY: the buffer holds `ids.len()` entries and `count` is a valid out
    // pointer.
    let status = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    if status != CGError::Success {
        return Vec::new();
    }
    ids[..(count as usize).min(ids.len())].to_vec()
}

/// Every active display's frame, in the coordinates window bounds use.
pub fn frames() -> Vec<Rect> {
    active()
        .into_iter()
        .map(|id| {
            let b = CGDisplayBounds(id);
            Rect {
                x: b.origin.x,
                y: b.origin.y,
                w: b.size.width,
                h: b.size.height,
            }
        })
        .collect()
}
