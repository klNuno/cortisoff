include!("../resources.rs");

fn main() {
    embed("cortisoff", "cortisoff");
    delay_window_dlls();
}

/// The daemon is this same executable, and Windows maps every DLL an
/// executable imports before `main` runs. OpenGL and UI Automation serve the
/// window alone, and were mapped into a process that never draws: OpenGL with
/// GLU32 behind it, UI Automation with a dozen more. Delay-loaded, each maps
/// on its first call, which the daemon never makes. The other two are the
/// daemon's, each for one backend only (DDC/CI, the magnifier).
fn delay_window_dlls() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os != "windows" || env != "msvc" {
        return;
    }
    for dll in [
        "opengl32.dll",
        "uiautomationcore.dll",
        "dxva2.dll",
        "magnification.dll",
    ] {
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
    }
    println!("cargo:rustc-link-arg-bins=delayimp.lib");
}
