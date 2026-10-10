//! `cortisoff-ui`: the window, on its own process, opened on demand.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use cortisoff_ui::{App, Shell, SIZE};
use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("cortisoff")
            .with_inner_size(SIZE)
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "cortisoff",
        options,
        Box::new(|_| Ok(Box::new(App(Shell::default())))),
    )
}
