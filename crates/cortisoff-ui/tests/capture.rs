//! Renders every page to a PNG with no window and no daemon.
//!
//! `cargo test -p cortisoff-ui --test capture -- --nocapture` writes them to
//! `target/captures/ui`. The egui counterpart of `pnpm capture`.

use cortisoff_ui::{App, Page, Shell, SIZE};
use egui_kittest::Harness;

#[test]
fn capture_every_page() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/captures/ui");
    std::fs::create_dir_all(&dir).unwrap();
    let pages = [
        ("overview", Page::Overview, SIZE),
        ("night-light", Page::Plugin(0), SIZE),
        // The whole page, for what the window scrolls to.
        ("night-light-full", Page::Plugin(0), [SIZE[0], 1240.0]),
        ("display-profiles", Page::Plugin(1), SIZE),
        ("wallpaper", Page::Plugin(2), SIZE),
        ("automations", Page::Plugin(3), SIZE),
    ];
    for (name, page, size) in pages {
        let mut harness = Harness::builder()
            .with_size(size)
            .with_pixels_per_point(1.0)
            .wgpu()
            .build_eframe(|_| App(Shell::on(page)));
        harness.run_steps(4);
        let image = harness.render().expect("render");
        let path = dir.join(format!("{name}.png"));
        image.save(&path).unwrap();
        println!("wrote {}", path.display());
    }
}
