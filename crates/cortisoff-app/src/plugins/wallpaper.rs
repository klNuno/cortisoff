//! Wallpaper pause: an animated wallpaper stops when nobody is looking.

use eframe::egui::{self, vec2, Sense, Ui};

use super::{PluginUi, Status};
use crate::theme::{self, glyph};
use crate::widgets::{self, Track};

pub struct WallpaperPause {
    enabled: bool,
    after_minutes: f32,
    keep_for_sound: bool,
    keep_for_fullscreen: bool,
    on_battery: bool,
}

impl Default for WallpaperPause {
    fn default() -> Self {
        Self {
            enabled: true,
            after_minutes: 5.0,
            keep_for_sound: true,
            keep_for_fullscreen: true,
            on_battery: false,
        }
    }
}

impl PluginUi for WallpaperPause {
    fn name(&self) -> &str {
        "Wallpaper pause"
    }

    fn icon(&self) -> char {
        glyph::PICTURE
    }

    fn status(&self) -> Status {
        Status {
            line: "playing".into(),
            detail: format!(
                "Wallpaper Engine plays. It pauses after {:.0} min without input",
                self.after_minutes
            ),
            color: theme::WAITING,
        }
    }

    fn page(&mut self, ui: &mut Ui) {
        let mut enabled = self.enabled;
        widgets::page_header(
            ui,
            glyph::PICTURE,
            "Wallpaper pause",
            "Stops an animated wallpaper while nobody is looking at it.",
            |ui| {
                widgets::switch(ui, &mut enabled);
            },
        );
        self.enabled = enabled;

        widgets::card(ui, None, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                ui.painter()
                    .circle_filled(rect.center(), 18.0, theme::FIELD);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    glyph::PICTURE,
                    theme::icon(16.0),
                    theme::TEXT,
                );
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        egui::RichText::new("Playing")
                            .font(theme::strong(20.0))
                            .color(theme::TEXT),
                    );
                    widgets::hint(ui, "Wallpaper Engine found. Lively is not installed.");
                });
            });
        });
        ui.add_space(12.0);

        widgets::card(ui, Some("When to pause"), |ui| {
            widgets::row(ui, "After no input for", None, |ui| {
                widgets::slider(
                    ui,
                    &mut self.after_minutes,
                    1.0..=60.0,
                    1.0,
                    Track::Plain,
                    |v| format!("{v:.0} min"),
                );
            });
            widgets::divider(ui);
            widgets::row(
                ui,
                "Also on battery",
                Some("Paused as long as the charger is out."),
                |ui| {
                    widgets::switch(ui, &mut self.on_battery);
                },
            );
        });
        ui.add_space(12.0);

        widgets::card(ui, Some("Keep it playing"), |ui| {
            widgets::row(
                ui,
                "While sound plays",
                Some("A film in the background is not someone away."),
                |ui| {
                    widgets::switch(ui, &mut self.keep_for_sound);
                },
            );
            widgets::divider(ui);
            widgets::row(ui, "While an app fills the screen", None, |ui| {
                widgets::switch(ui, &mut self.keep_for_fullscreen);
            });
        });
    }
}
