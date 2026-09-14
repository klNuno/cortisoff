//! Hand-written NVAPI bindings, only as much as digital vibrance needs.
//!
//! The Rust NVAPI crates are dead: `nvapi-sys` last published in September
//! 2020 and does not build against a current toolchain. This is about two
//! hundred lines with no dependency to rot.
//!
//! NVAPI exports exactly one symbol worth having, `nvapi_QueryInterface`. It
//! takes a function id and hands back a pointer, so binding a call is a
//! constant and a signature. The ids are stable across driver versions, which
//! is the whole reason the export exists.

use std::ffi::c_void;

use windows::core::s;
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

/// Function ids. Stable by design, and the same numbers every wrapper in every
/// language uses.
mod id {
    pub const INITIALIZE: u32 = 0x0150E828;
    pub const UNLOAD: u32 = 0xD22BDD7E;
    pub const ENUM_DISPLAY_HANDLE: u32 = 0x9ABDD40D;
    pub const DISPLAY_NAME: u32 = 0x22A78B05;
    pub const GET_DVC_INFO_EX: u32 = 0x0E45002D;
    pub const SET_DVC_LEVEL_EX: u32 = 0x4A82C2B1;
}

/// Zero is success and everything else is a code. Only two are worth naming:
/// the end of an enumeration, which is not a failure, and everything else,
/// which the caller reports rather than interprets.
const OK: i32 = 0;
const END_ENUMERATION: i32 = -9;

/// `sizeof(struct) | (version << 16)`, NVAPI's way of versioning a struct in
/// its first field. Getting it wrong is an error code, not a crash, which is
/// the one merciful thing about this API.
const fn nv_version(size: usize, version: u32) -> u32 {
    size as u32 | (version << 16)
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct DvcInfoEx {
    version: u32,
    pub current: i32,
    pub min: i32,
    pub max: i32,
    pub default: i32,
}

type QueryInterface = unsafe extern "C" fn(u32) -> *mut c_void;
type Initialize = unsafe extern "C" fn() -> i32;
type Unload = unsafe extern "C" fn() -> i32;
type EnumDisplayHandle = unsafe extern "C" fn(u32, *mut *mut c_void) -> i32;
type DisplayName = unsafe extern "C" fn(*mut c_void, *mut u8) -> i32;
type GetDvcInfoEx = unsafe extern "C" fn(*mut c_void, u32, *mut DvcInfoEx) -> i32;
type SetDvcLevelEx = unsafe extern "C" fn(*mut c_void, u32, *mut DvcInfoEx) -> i32;

/// One NVIDIA output, already resolved to the device name Windows uses, so a
/// rule and the journal can key on the same string the gamma actuator does.
pub struct Output {
    handle: *mut c_void,
    pub device: String,
}

pub struct Nvapi {
    library: HMODULE,
    unload: Option<Unload>,
    get: GetDvcInfoEx,
    set: SetDvcLevelEx,
    pub outputs: Vec<Output>,
}

impl Nvapi {
    /// `None` on any machine without an NVIDIA driver, without an NVIDIA
    /// output, or whose driver is too old for the DVC calls. Never an error:
    /// an absent capability is reported by the doctor, not raised.
    pub fn open() -> Option<Self> {
        let library = unsafe { LoadLibraryW(windows::core::w!("nvapi64.dll")) }.ok()?;
        let query = unsafe { GetProcAddress(library, s!("nvapi_QueryInterface")) }?;
        let query: QueryInterface = unsafe { std::mem::transmute(query) };

        let initialize: Initialize = unsafe { bind(query, id::INITIALIZE) }?;
        if unsafe { initialize() } != OK {
            unsafe {
                let _ = FreeLibrary(library);
            }
            return None;
        }

        let unload: Option<Unload> = unsafe { bind(query, id::UNLOAD) };
        let enumerate: EnumDisplayHandle = unsafe { bind(query, id::ENUM_DISPLAY_HANDLE) }?;
        let name: Option<DisplayName> = unsafe { bind(query, id::DISPLAY_NAME) };
        let get: GetDvcInfoEx = unsafe { bind(query, id::GET_DVC_INFO_EX) }?;
        let set: SetDvcLevelEx = unsafe { bind(query, id::SET_DVC_LEVEL_EX) }?;

        let outputs = enumerate_outputs(enumerate, name);
        if outputs.is_empty() {
            if let Some(unload) = unload {
                unsafe { unload() };
            }
            unsafe {
                let _ = FreeLibrary(library);
            }
            return None;
        }

        Some(Self {
            library,
            unload,
            get,
            set,
            outputs,
        })
    }

    /// What one output reports today: the level, and the range it lives in.
    /// The range is read rather than assumed, because it is what the doctor
    /// shows the user and what a rule's value is clamped to.
    pub fn info(&self, output: &Output) -> Option<DvcInfoEx> {
        let mut info = DvcInfoEx {
            version: nv_version(std::mem::size_of::<DvcInfoEx>(), 1),
            ..Default::default()
        };
        // Output id zero means "the one this display handle already names".
        (unsafe { (self.get)(output.handle, 0, &mut info) } == OK).then_some(info)
    }

    pub fn set_level(&self, output: &Output, level: i32) -> Result<(), i32> {
        let Some(current) = self.info(output) else {
            return Err(END_ENUMERATION);
        };
        let mut wanted = DvcInfoEx {
            version: nv_version(std::mem::size_of::<DvcInfoEx>(), 1),
            current: level.clamp(current.min, current.max),
            ..current
        };
        match unsafe { (self.set)(output.handle, 0, &mut wanted) } {
            OK => Ok(()),
            code => Err(code),
        }
    }
}

impl Drop for Nvapi {
    fn drop(&mut self) {
        if let Some(unload) = self.unload {
            unsafe { unload() };
        }
        unsafe {
            let _ = FreeLibrary(self.library);
        }
    }
}

/// The handle is a driver-owned token, never dereferenced here, and the
/// enumeration that produced it runs once at startup on the daemon's thread.
unsafe impl Send for Nvapi {}

unsafe fn bind<T>(query: QueryInterface, id: u32) -> Option<T> {
    let pointer = query(id);
    if pointer.is_null() {
        return None;
    }
    // Every id in this file maps to a signature declared above it. A wrong
    // pairing is undefined behaviour, which is why the ids are not computed
    // and never come from outside.
    Some(std::mem::transmute_copy(&pointer))
}

fn enumerate_outputs(enumerate: EnumDisplayHandle, name: Option<DisplayName>) -> Vec<Output> {
    let mut out = Vec::new();
    for index in 0..16u32 {
        let mut handle: *mut c_void = std::ptr::null_mut();
        if unsafe { enumerate(index, &mut handle) } != OK || handle.is_null() {
            break;
        }
        out.push(Output {
            handle,
            device: name
                .and_then(|name| display_name(name, handle))
                .unwrap_or_else(|| format!("nvidia-output-{index}")),
        });
    }
    out
}

/// `NvAPI_GetAssociatedNvidiaDisplayName` writes into a fixed 64 byte buffer.
/// NVAPI has no way to ask for the size, so the size is the contract.
fn display_name(name: DisplayName, handle: *mut c_void) -> Option<String> {
    let mut buffer = [0u8; 64];
    if unsafe { name(handle, buffer.as_mut_ptr()) } != OK {
        return None;
    }
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    Some(String::from_utf8_lossy(&buffer[..end]).into_owned())
}

/// So a caller can say what went wrong without a table of every NVAPI code.
pub fn describe(code: i32) -> String {
    match code {
        -1 => "NVAPI reported a generic error".to_owned(),
        -5 => "this NVIDIA driver does not implement digital vibrance".to_owned(),
        -8 => "NVAPI was handed an argument it refused".to_owned(),
        -104 => "the display does not support digital vibrance".to_owned(),
        other => format!("NVAPI refused the call with code {other}"),
    }
}

/// Unused today, kept so a future caller does not rediscover the id.
#[allow(dead_code)]
pub const ERROR_MESSAGE_ID: u32 = 0x6C2D048C;

#[cfg(test)]
mod tests {
    use super::*;

    /// The version field is a size and a version packed together. Getting it
    /// wrong is an error code from every call, with nothing pointing at the
    /// cause, so it is worth pinning.
    #[test]
    fn the_dvc_struct_versions_itself_the_way_nvapi_expects() {
        assert_eq!(std::mem::size_of::<DvcInfoEx>(), 20);
        assert_eq!(nv_version(20, 1), 0x0001_0014);
    }
}
