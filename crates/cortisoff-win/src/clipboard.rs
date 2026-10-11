//! The clipboard, as text.

use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};

/// `CF_UNICODETEXT`. Spelled out rather than pulling in the whole
/// `Win32_System_Ole` binding for one number.
const CF_UNICODETEXT: u32 = 13;

/// How often, and how far apart, to try a clipboard another application
/// holds. Clipboard managers and remote desktop clients open it on every
/// change and close it within milliseconds.
const ATTEMPTS: u32 = 10;
const BETWEEN: Duration = Duration::from_millis(10);

/// The text on the clipboard. Empty when it holds something else, an image
/// or files: that is not an error, there is just no text.
pub fn read(owner: HWND) -> Result<String, String> {
    let _open = Open::new(owner)?;
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) }.is_err() {
        return Ok(String::new());
    }
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT) }.map_err(|e| {
        format!(
            "the clipboard would not hand over its text: {}",
            e.message()
        )
    })?;
    let memory = HGLOBAL(handle.0);
    let data = unsafe { GlobalLock(memory) } as *const u16;
    if data.is_null() {
        return Err("the clipboard's text could not be read".to_owned());
    }
    // The block can be larger than the string, never smaller, and the
    // string ends at its first zero.
    let units = unsafe { std::slice::from_raw_parts(data, GlobalSize(memory) / 2) };
    let text = String::from_utf16_lossy(until_nul(units));
    let _ = unsafe { GlobalUnlock(memory) };
    Ok(text)
}

/// Put `text` on the clipboard, replacing whatever was there.
pub fn write(owner: HWND, text: &str) -> Result<(), String> {
    let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
    // Filled before the clipboard is opened, so it is held for as short a
    // time as possible.
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, units.len() * 2) }
        .map_err(|e| format!("no memory for the clipboard: {}", e.message()))?;
    let data = unsafe { GlobalLock(memory) } as *mut u16;
    if data.is_null() {
        let _ = unsafe { GlobalFree(memory) };
        return Err("no memory for the clipboard".to_owned());
    }
    unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), data, units.len()) };
    let _ = unsafe { GlobalUnlock(memory) };

    let placed = Open::new(owner).and_then(|_open| {
        unsafe { EmptyClipboard() }
            .map_err(|e| format!("the clipboard could not be emptied: {}", e.message()))?;
        unsafe { SetClipboardData(CF_UNICODETEXT, HANDLE(memory.0)) }
            .map(|_| ())
            .map_err(|e| format!("the clipboard refused the text: {}", e.message()))
    });
    // Once placed, the memory is the system's. Until then it is still ours.
    if placed.is_err() {
        let _ = unsafe { GlobalFree(memory) };
    }
    placed
}

/// The clipboard, open, closed on drop whatever happens in between.
struct Open;

impl Open {
    /// `owner` must be a window of this process for a write: opened with
    /// none, `EmptyClipboard` leaves the clipboard without an owner and
    /// `SetClipboardData` then fails.
    fn new(owner: HWND) -> Result<Open, String> {
        for attempt in 0..ATTEMPTS {
            if unsafe { OpenClipboard(owner) }.is_ok() {
                return Ok(Open);
            }
            if attempt + 1 < ATTEMPTS {
                std::thread::sleep(BETWEEN);
            }
        }
        Err("another application is holding the clipboard".to_owned())
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

fn until_nul(units: &[u16]) -> &[u16] {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    &units[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_ends_at_the_first_zero() {
        assert_eq!(until_nul(&[104, 105, 0, 33, 0]), &[104, 105]);
        assert_eq!(until_nul(&[104, 105]), &[104, 105]);
        assert!(until_nul(&[0]).is_empty());
        assert!(until_nul(&[]).is_empty());
    }

    /// Read-only: opens the clipboard of whoever runs the tests, reads, and
    /// closes it. Whatever is there, the answer is text or a sentence.
    #[test]
    fn reading_the_clipboard_answers_text_or_a_sentence() {
        match read(HWND::default()) {
            Ok(_) => {}
            Err(e) => assert!(e.contains("clipboard"), "{e}"),
        }
    }
}
