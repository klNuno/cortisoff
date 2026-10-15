//! Renders every page of the window to a PNG with no daemon, from the
//! built-in plugins and a made-up machine.
//!
//! `cargo test -p cortisoff-app --test capture -- --ignored --nocapture`
//! writes them to `target/captures/ui`. Ignored by default: it needs a GPU,
//! which CI runners do not have.

use cortisoff_app::{App, Shell, View, SIZE};
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

#[test]
#[ignore = "needs a GPU"]
fn capture_every_page() {
    let dir = std::env::var_os("CORTISOFF_CAPTURES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/captures/ui")
        });
    std::fs::create_dir_all(&dir).unwrap();
    let plugin = |id: &str| View::Plugin(id.to_owned());
    let pages = [
        ("overview", View::Overview, SIZE),
        ("stopped", View::Overview, SIZE),
        ("held", View::Overview, SIZE),
        ("held-hover", View::Overview, SIZE),
        ("logo-hover", View::Overview, SIZE),
        ("close-hover", View::Overview, SIZE),
        ("night-light", plugin("night-light"), SIZE),
        ("night-light-full", plugin("night-light"), [SIZE[0], 1240.0]),
        ("display-profiles", plugin("display-profiles"), SIZE),
        ("wallpaper-pause", plugin("wallpaper-pause"), SIZE),
        ("shortcuts", plugin("shortcuts"), SIZE),
        ("add", View::Add, SIZE),
        ("settings", View::Settings, SIZE),
    ];
    for (name, view, size) in pages {
        let mut harness = Harness::builder()
            .with_size(size)
            .with_pixels_per_point(1.0)
            .wgpu()
            .build_eframe(|_| {
                let shell = Shell::demo(view.clone());
                App(match name {
                    "stopped" => shell.stopped(),
                    "held" | "held-hover" => shell.held(),
                    _ => shell,
                })
            });
        harness.run_steps(4);
        if name == "held-hover" {
            harness.get_by_label("cortison").hover();
            harness.run_steps(4);
        }
        if name == "logo-hover" {
            // The logo is the on and off switch; this is how it answers the
            // pointer.
            harness.get_by_label("cortisoff").hover();
            harness.run_steps(4);
        }
        if name == "close-hover" {
            // The window's own caption buttons, where the system frame was.
            harness.get_by_label("Close").hover();
            harness.run_steps(4);
        }
        let image = harness.render().expect("render");
        let path = dir.join(format!("{name}.png"));
        image.save(&path).unwrap();
        println!("wrote {}", path.display());
    }
}
