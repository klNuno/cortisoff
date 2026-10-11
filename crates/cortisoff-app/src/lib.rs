//! The cortisoff window.
//!
//! Prototype: every plugin's data here is made up, nothing talks to the
//! daemon yet. What it settles is the shape: a sidebar listing every plugin
//! the same way, an overview of what is happening now, and one page per
//! plugin that the plugin draws itself.

pub mod plugins;
pub mod script;
pub mod theme;
pub mod widgets;

use eframe::egui::{self, pos2, vec2, Frame, Margin, ScrollArea, Ui};

use plugins::PluginUi;
use theme::glyph;

/// Which page the main area shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Overview,
    Plugin(usize),
}

pub struct Shell {
    pub page: Page,
    pub plugins: Vec<Box<dyn PluginUi>>,
    installed: bool,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            page: Page::Overview,
            plugins: plugins::mock(),
            installed: false,
        }
    }
}

impl Shell {
    /// Opens on `page`, for a capture.
    pub fn on(page: Page) -> Self {
        Self {
            page,
            ..Self::default()
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        // Fonts set now are bound at the start of the next frame, and a named
        // family used before that panics. The first frame draws nothing.
        if !self.installed {
            theme::install(ui.ctx());
            self.installed = true;
            ui.ctx().request_repaint();
            return;
        }
        for plugin in &mut self.plugins {
            plugin.poll();
        }
        egui::Panel::left("plugins")
            .resizable(false)
            .exact_size(248.0)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(theme::SIDEBAR)
                    .inner_margin(Margin::symmetric(10, 14)),
            )
            .show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::BG))
            .show(ui, |ui| {
                ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    Frame::new()
                        .inner_margin(Margin {
                            left: 32,
                            right: 32,
                            top: 26,
                            bottom: 32,
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width().min(760.0));
                            match self.page {
                                Page::Overview => self.overview(ui),
                                Page::Plugin(i) => self.plugins[i].page(ui),
                            }
                        });
                });
            });
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            logo(ui);
            ui.label(
                egui::RichText::new("cortisoff")
                    .font(theme::strong(16.0))
                    .color(theme::TEXT),
            );
        });
        ui.add_space(14.0);
        if widgets::nav_item(
            ui,
            glyph::HOME,
            "Overview",
            None,
            self.page == Page::Overview,
        )
        .clicked()
        {
            self.page = Page::Overview;
        }
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("Plugins")
                    .font(theme::body(12.5))
                    .color(theme::SUBTLE),
            );
        });
        ui.add_space(4.0);
        for (i, plugin) in self.plugins.iter().enumerate() {
            let status = plugin.status();
            let selected = self.page == Page::Plugin(i);
            if widgets::nav_item(
                ui,
                plugin.icon(),
                plugin.name(),
                Some((&status.line, status.color)),
                selected,
            )
            .clicked()
            {
                self.page = Page::Plugin(i);
            }
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            widgets::nav_item(ui, glyph::SETTINGS, "Settings", None, false);
            widgets::nav_item(ui, glyph::ADD, "Add a plugin", None, false);
        });
    }

    fn overview(&mut self, ui: &mut Ui) {
        widgets::page_header(
            ui,
            glyph::HOME,
            "Overview",
            "What cortisoff is doing on this machine right now.",
            |ui| {
                widgets::button(ui, Some(glyph::PAUSE), "Pause everything");
            },
        );

        widgets::card(ui, Some("Right now"), |ui| {
            for (i, plugin) in self.plugins.iter().enumerate() {
                if i > 0 {
                    widgets::divider(ui);
                }
                let status = plugin.status();
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(plugin.icon())
                            .font(theme::icon(16.0))
                            .color(theme::MUTED),
                    );
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(
                            egui::RichText::new(plugin.name())
                                .font(theme::strong(14.0))
                                .color(theme::TEXT),
                        );
                        ui.label(
                            egui::RichText::new(&status.detail)
                                .font(theme::body(12.5))
                                .color(theme::MUTED),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(&status.line)
                                .font(theme::body(13.0))
                                .color(theme::TEXT),
                        );
                        widgets::dot(ui, status.color);
                    });
                });
                ui.add_space(2.0);
            }
        });
        ui.add_space(12.0);

        widgets::card(ui, Some("What the machine says"), |ui| {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                widgets::chip(ui, Some(glyph::GAME), "firefox.exe in front");
                widgets::chip(ui, Some(glyph::FULLSCREEN), "fills the screen");
                widgets::chip(ui, Some(glyph::VOLUME), "sound playing");
                widgets::chip(ui, Some(glyph::CLOCK), "20:41");
            });
            ui.add_space(4.0);
            widgets::hint(
                ui,
                "Only what an enabled plugin listens to is watched. Nothing else is hooked.",
            );
        });
        ui.add_space(12.0);

        widgets::card(ui, Some("Recent"), |ui| {
            for (time, what) in [
                ("20:00", "Night light started fading to 3200 K"),
                ("20:01", "Night light holds 3200 K on 3 screens"),
                (
                    "19:42",
                    "Display profiles gave cs2.exe's screen back: 144 Hz, vibrance 50 %",
                ),
                (
                    "18:55",
                    "Display profiles applied 240 Hz and vibrance 100 % for cs2.exe",
                ),
                ("18:30", "Wallpaper pause resumed Wallpaper Engine"),
            ] {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(time)
                            .font(theme::body(13.0))
                            .color(theme::SUBTLE),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(what)
                            .font(theme::body(13.5))
                            .color(theme::TEXT),
                    );
                });
            }
        });
    }
}

/// The gauge logo, drawn rather than shipped as an image.
fn logo(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::hover());
    let c = rect.center() + vec2(0.0, 3.0);
    let painter = ui.painter();
    let arc: Vec<_> = (0..=24)
        .map(|i| {
            let a = std::f32::consts::PI * (1.0 + i as f32 / 24.0);
            pos2(c.x + 9.0 * a.cos(), c.y + 9.0 * a.sin())
        })
        .collect();
    painter.add(egui::Shape::line(
        arc,
        egui::Stroke::new(2.5, theme::HOLDING),
    ));
    painter.line_segment(
        [c, c + vec2(-5.0, -5.0)],
        egui::Stroke::new(2.0, theme::TEXT),
    );
}

/// Window size the layout is designed for, and what a capture uses.
pub const SIZE: [f32; 2] = [1040.0, 720.0];

/// The window as eframe runs it, shared by the binary and the captures so a
/// capture is the real root, with no harness margin around it.
pub struct App(pub Shell);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }
}
