//! The daemon's first call, on the main thread as the daemon makes it: the
//! process ends with a prohibited activation policy, so no Dock icon. No
//! test harness, because the harness runs every test off the main thread.

#[cfg(target_os = "macos")]
fn main() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    let mtm = MainThreadMarker::new().expect("main thread");
    let before = NSApplication::sharedApplication(mtm).activationPolicy();
    let done = cortisoff_mac::background_only();
    let after = NSApplication::sharedApplication(mtm).activationPolicy();
    assert!(
        done && after == NSApplicationActivationPolicy::Prohibited,
        "activation policy {before:?} before, {after:?} after"
    );
    println!("test background_only_on_the_main_thread ... ok ({before:?} before)");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
