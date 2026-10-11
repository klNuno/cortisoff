//! The Windows half of the installer: known folders, the Start Menu shortcut,
//! the per-user uninstall entry and the self-delete after exit.

use std::path::{Path, PathBuf};
use std::process::Command;

use windows::core::{Interface, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, HANDLE, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IPersistFile,
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Programs, FOLDERID_UserProgramFiles, IShellLinkW, SHGetKnownFolderPath, ShellLink,
    KF_FLAG_DONT_VERIFY,
};

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Cortisoff";
const SHORTCUT: &str = "Cortisoff.lnk";

fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
    // SAFETY: the returned buffer is owned by us and freed right after the
    // copy, whatever the conversion gives.
    unsafe {
        let raw: PWSTR = SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY, HANDLE::default()).ok()?;
        let path = raw.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(raw.0 as *const _));
        path
    }
}

/// `%LOCALAPPDATA%\Programs`, where per-user installers put applications.
pub(super) fn user_programs() -> PathBuf {
    known_folder(&FOLDERID_UserProgramFiles).unwrap_or_else(|| {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("Programs")
    })
}

fn shortcut_path() -> Result<PathBuf, String> {
    known_folder(&FOLDERID_Programs)
        .or_else(|| {
            std::env::var_os("APPDATA").map(|appdata| {
                PathBuf::from(appdata)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Start Menu")
                    .join("Programs")
            })
        })
        .map(|dir| dir.join(SHORTCUT))
        .ok_or_else(|| "cannot find the Start Menu folder".to_owned())
}

/// COM for the length of one call. A thread the caller already initialised
/// in the other apartment model is fine for a shell link, so that is used
/// as is and left alone.
struct Com(bool);

impl Com {
    fn enter() -> Result<Self, String> {
        // SAFETY: balanced by the Drop below when it succeeded.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr.is_ok() {
            Ok(Com(true))
        } else if hr == RPC_E_CHANGED_MODE {
            Ok(Com(false))
        } else {
            Err(format!("cannot start COM: {}", hr.message()))
        }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: pairs with the successful CoInitializeEx in enter.
            unsafe { CoUninitialize() };
        }
    }
}

pub(super) fn create_shortcut(app: &Path) -> Result<(), String> {
    let path = shortcut_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let _com = Com::enter()?;
    let fail = |e: windows::core::Error| format!("cannot write the Start Menu shortcut: {e}");
    // SAFETY: plain COM calls on an object we created, with owned strings
    // that outlive each call.
    unsafe {
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(fail)?;
        link.SetPath(&HSTRING::from(app)).map_err(fail)?;
        if let Some(dir) = app.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir))
                .map_err(fail)?;
        }
        link.SetDescription(&HSTRING::from("Cortisoff"))
            .map_err(fail)?;
        link.SetIconLocation(&HSTRING::from(app), 0).map_err(fail)?;
        let file: IPersistFile = link.cast().map_err(fail)?;
        file.Save(&HSTRING::from(path.as_path()), true)
            .map_err(fail)?;
    }
    Ok(())
}

pub(super) fn remove_shortcut() -> Result<(), String> {
    let path = shortcut_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

/// The values Settings > Apps reads for a per-user program.
pub(super) fn uninstall_values(
    app: &Path,
    version: &str,
    size_kb: u32,
) -> Vec<(&'static str, Value)> {
    let quoted = format!("\"{}\"", app.display());
    let dir = app
        .parent()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    vec![
        ("DisplayName", Value::Text("Cortisoff".to_owned())),
        ("DisplayVersion", Value::Text(version.to_owned())),
        ("Publisher", Value::Text("Cortisoff".to_owned())),
        ("DisplayIcon", Value::Text(format!("{},0", app.display()))),
        ("InstallLocation", Value::Text(dir)),
        (
            "UninstallString",
            Value::Text(format!("{quoted} --uninstall")),
        ),
        ("NoModify", Value::Number(1)),
        ("NoRepair", Value::Number(1)),
        ("EstimatedSize", Value::Number(size_kb)),
    ]
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Value {
    Text(String),
    Number(u32),
}

pub(super) fn write_uninstall_entry(app: &Path) -> Result<(), String> {
    let size_kb = std::fs::metadata(app)
        .map(|m| u32::try_from(m.len() / 1024).unwrap_or(u32::MAX))
        .unwrap_or(0);
    let values = uninstall_values(app, env!("CARGO_PKG_VERSION"), size_kb);
    let fail = |what: &str, code: windows::Win32::Foundation::WIN32_ERROR| {
        format!(
            "cannot write the uninstall entry ({what}): {}",
            windows::core::Error::from(code.to_hresult()).message()
        )
    };
    let mut key = HKEY::default();
    // SAFETY: HKCU is always open for the current user; the key we create is
    // closed below on every path.
    unsafe {
        let code = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(UNINSTALL_KEY),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        );
        if code.is_err() {
            return Err(fail("create", code));
        }
        let mut result = Ok(());
        for (name, value) in &values {
            let (kind, bytes) = match value {
                Value::Text(text) => (REG_SZ, wide_bytes(text)),
                Value::Number(n) => (REG_DWORD, n.to_le_bytes().to_vec()),
            };
            let code = RegSetValueExW(key, &HSTRING::from(*name), 0, kind, Some(&bytes));
            if code.is_err() {
                result = Err(fail(name, code));
                break;
            }
        }
        let _ = RegCloseKey(key);
        result
    }
}

/// A `REG_SZ` payload: UTF-16 little endian with its terminating nul.
fn wide_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

pub(super) fn remove_uninstall_entry() -> Result<(), String> {
    // SAFETY: deletes a key under the current user's own hive.
    let code = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(UNINSTALL_KEY)) };
    if code.is_ok() || code == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!(
            "cannot remove the uninstall entry: {}",
            windows::core::Error::from(code.to_hresult()).message()
        ))
    }
}

/// The command a detached `cmd` runs to remove the app once the process
/// holding it has exited: retry the delete once a second for a minute, then
/// remove the folder if nothing else is left in it.
pub(super) fn self_delete_command(app: &Path, dir: &Path) -> String {
    let app = app.display();
    let dir = dir.display();
    format!(
        "/d /s /c \"for /l %i in (1,1,60) do @(del /f /q \"{app}\" >nul 2>&1 & \
         if not exist \"{app}\" (rmdir \"{dir}\" >nul 2>&1 & exit /b 0) & \
         ping -n 2 127.0.0.1 >nul)\""
    )
}

pub(super) fn delete_after_exit(app: &Path, dir: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    let system = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    Command::new(system.join("System32").join("cmd.exe"))
        .raw_arg(self_delete_command(app, dir))
        .current_dir(&system)
        .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot schedule the removal of {}: {e}", app.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_uninstall_entry_runs_the_installed_app() {
        let app = Path::new(r"C:\Users\Ana\AppData\Local\Programs\Cortisoff\cortisoff.exe");
        let values = uninstall_values(app, "1.2.0", 9000);
        let get = |name: &str| values.iter().find(|(n, _)| *n == name).map(|(_, v)| v);
        assert_eq!(
            get("UninstallString"),
            Some(&Value::Text(
                r#""C:\Users\Ana\AppData\Local\Programs\Cortisoff\cortisoff.exe" --uninstall"#
                    .to_owned()
            ))
        );
        assert_eq!(
            get("InstallLocation"),
            Some(&Value::Text(
                r"C:\Users\Ana\AppData\Local\Programs\Cortisoff".to_owned()
            ))
        );
        assert_eq!(
            get("DisplayVersion"),
            Some(&Value::Text("1.2.0".to_owned()))
        );
        assert_eq!(get("EstimatedSize"), Some(&Value::Number(9000)));
    }

    #[test]
    fn registry_strings_end_in_a_wide_nul() {
        assert_eq!(wide_bytes("Ab"), vec![b'A', 0, b'b', 0, 0, 0]);
    }

    #[test]
    fn the_self_delete_quotes_paths_with_spaces() {
        let command = self_delete_command(
            Path::new(r"C:\Users\Ana Maria\Programs\Cortisoff\cortisoff.exe"),
            Path::new(r"C:\Users\Ana Maria\Programs\Cortisoff"),
        );
        assert!(command.starts_with("/d /s /c \""));
        assert!(command.ends_with(")\""));
        assert!(
            command.contains(r#"del /f /q "C:\Users\Ana Maria\Programs\Cortisoff\cortisoff.exe""#)
        );
        assert!(command.contains(r#"rmdir "C:\Users\Ana Maria\Programs\Cortisoff""#));
    }

    #[test]
    fn known_folders_resolve_without_creating_anything() {
        assert!(user_programs().ends_with("Programs"));
        assert!(shortcut_path()
            .unwrap()
            .ends_with(Path::new("Programs").join(SHORTCUT)));
    }
}
