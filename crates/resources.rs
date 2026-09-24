// Shared by the daemon's and the command line's build scripts, pulled in with
// `include!`. It gives each Windows executable a version resource and the app
// icon. Without one, Windows has no name to show for the process: the taskbar
// settings list the tray icon as a blank line and Task Manager shows the bare
// file name.

/// Write a resource script naming `binary` and link it into that binary only.
/// A no-op for any other target, so the crates still check on Linux.
fn embed(binary: &str, description: &str) {
    println!("cargo:rerun-if-changed=../resources.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    #[cfg(windows)]
    {
        let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let icon = dir.join("../../app/src-tauri/icons/icon.ico");
        println!("cargo:rerun-if-changed={}", icon.display());
        let version = std::env::var("CARGO_PKG_VERSION").unwrap();
        let mut parts = version
            .split(['.', '-', '+'])
            .map(|p| p.parse::<u16>().unwrap_or(0));
        let numbers: Vec<u16> = (0..4).map(|_| parts.next().unwrap_or(0)).collect();
        let comma = numbers
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let script = format!(
            r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "cortisoff"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{binary}"
      VALUE "OriginalFilename", "{binary}.exe"
      VALUE "ProductName", "cortisoff"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
            icon = icon.display().to_string().replace('\\', "/"),
        );
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        let rc = out.join(format!("{binary}.rc"));
        std::fs::write(&rc, script).unwrap();
        embed_resource::compile_for(&rc, [binary], embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
    #[cfg(not(windows))]
    let _ = (binary, description);
}
