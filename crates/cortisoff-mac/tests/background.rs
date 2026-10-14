//! The daemon's first call, on the main thread as the daemon makes it: the
//! process ends with a prohibited activation policy, so no Dock icon. No
//! test harness, because the harness runs every test off the main thread.

#[cfg(target_os = "macos")]
fn main() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    assert!(
        cortisoff_mac::background_only(),
        "macOS refused the prohibited activation policy"
    );
    let mtm = MainThreadMarker::new().expect("main thread");
    assert_eq!(
        NSApplication::sharedApplication(mtm).activationPolicy(),
        NSApplicationActivationPolicy::Prohibited
    );
    println!("test background_only_on_the_main_thread ... ok");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
