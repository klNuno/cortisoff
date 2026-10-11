//! Display profiles: what the screen does while one application is in front.

use eframe::egui::{self, vec2, Align2, CornerRadius, Sense, Ui};

use super::{PluginUi, Status};
use crate::theme::{self, glyph};
use crate::widgets::{self, Track};

struct Profile {
    app: String,
    refresh: &'static str,
    resolution: &'static str,
    vibrance: f32,
    active: bool,
}

pub struct DisplayProfiles {
    enabled: bool,
    profiles: Vec<Profile>,
    open: usize,
}

impl Default for DisplayProfiles {
    fn default() -> Self {
        Self {
            enabled: true,
            profiles: vec![
                Profile {
                    app: "cs2.exe".into(),
                    refresh: "240 Hz",
                    resolution: "Keep current",
                    vibrance: 100.0,
                    active: false,
                },
                Profile {
                    app: "valorant.exe".into(),
                    refresh: "240 Hz",
                    resolution: "1920 x 1080",
                    vibrance: 80.0,
                    active: false,
                },
                Profile {
                    app: "mpc-hc64.exe".into(),
                    refresh: "Keep current",
                    resolution: "Keep current",
                    vibrance: 50.0,
                    active: false,
                },
            ],
            open: 0,
        }
    }
}

impl PluginUi for DisplayProfiles {
    fn name(&self) -> &str {
        "Display profiles"
    }

    fn icon(&self) -> char {
        glyph::MONITOR
    }

    fn status(&self) -> Status {
        Status {
            line: "waiting for 3 apps".into(),
            detail: "None of cs2.exe, valorant.exe or mpc-hc64.exe is in front".into(),
            color: theme::WAITING,
        }
    }

    fn page(&mut self, ui: &mut Ui) {
        let mut enabled = self.enabled;
        widgets::page_header(
            ui,
            glyph::MONITOR,
            "Display profiles",
            "Changes the screen while an app is in front, and puts it back when you leave.",
            |ui| {
                widgets::switch(ui, &mut enabled);
            },
        );
        self.enabled = enabled;

        let count = self.profiles.len();
        for i in 0..count {
            let open = self.open == i;
            let p = &mut self.profiles[i];
            let clicked = widgets::card(ui, None, |ui| {
                let header = ui
                    .horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(8), theme::FIELD);
                        let letter = p.app.chars().next().unwrap_or('?').to_ascii_uppercase();
                        ui.painter().text(
                            rect.center(),
                            Align2::CENTER_CENTER,
                            letter,
                            theme::strong(14.0),
                            theme::TEXT,
                        );
                        ui.add_space(4.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(
                                egui::RichText::new(&p.app)
                                    .font(theme::strong(14.0))
                                    .color(theme::TEXT),
                            );
                            let mut parts = vec![];
                            if p.refresh != "Keep current" {
                                parts.push(p.refresh.to_string());
                            }
                            if p.resolution != "Keep current" {
                                parts.push(p.resolution.to_string());
                            }
                            parts.push(format!("vibrance {:.0} %", p.vibrance));
                            widgets::hint(ui, &parts.join("  ·  "));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let chevron = if open {
                                glyph::CHEVRON_DOWN
                            } else {
                                glyph::CHEVRON_RIGHT
                            };
                            ui.label(
                                egui::RichText::new(chevron)
                                    .font(theme::icon(12.0))
                                    .color(theme::MUTED),
                            );
                            ui.add_space(8.0);
                            let (text, color) = if p.active {
                                ("in front", theme::HOLDING)
                            } else {
                                ("waiting", theme::WAITING)
                            };
                            ui.label(
                                egui::RichText::new(text)
                                    .font(theme::body(13.0))
                                    .color(theme::MUTED),
                            );
                            widgets::dot(ui, color);
                        });
                    })
                    .response
                    .interact(Sense::click());
                if open {
                    ui.add_space(8.0);
                    widgets::divider(ui);
                    widgets::row(
                        ui,
                        "Refresh rate",
                        Some("Every screen that can do it."),
                        |ui| {
                            widgets::field(ui, p.refresh, 150.0);
                        },
                    );
                    widgets::divider(ui);
                    widgets::row(ui, "Resolution", None, |ui| {
                        widgets::field(ui, p.resolution, 150.0);
                    });
                    widgets::divider(ui);
                    widgets::row(ui, "Vibrance", Some("NVIDIA digital vibrance."), |ui| {
                        widgets::slider(ui, &mut p.vibrance, 0.0..=100.0, 1.0, Track::Plain, |v| {
                            format!("{v:.0} %")
                        });
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        widgets::button(ui, None, "Try it now");
                        widgets::button(ui, Some(glyph::CLOSE), "Remove");
                    });
                }
                header.clicked()
            });
            if clicked {
                self.open = i;
            }
            ui.add_space(10.0);
        }
        widgets::button(ui, Some(glyph::ADD), "Add an app");
    }
}
