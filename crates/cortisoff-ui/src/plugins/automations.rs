//! Automations: the rule engine, as one plugin among the others.
//!
//! What every page before this one was, a condition and what it sets, kept
//! for anything no dedicated plugin covers.

use eframe::egui::{self, Ui};

use super::{PluginUi, Status};
use crate::theme::{self, glyph};
use crate::widgets;

struct Automation {
    name: &'static str,
    when: Vec<&'static str>,
    set: Vec<&'static str>,
    on: bool,
}

pub struct Automations {
    enabled: bool,
    items: Vec<Automation>,
}

impl Default for Automations {
    fn default() -> Self {
        Self {
            enabled: true,
            items: vec![
                Automation {
                    name: "dim the colours on battery",
                    when: vec!["on battery"],
                    set: vec!["vibrance 30 %"],
                    on: true,
                },
                Automation {
                    name: "60 Hz while locked",
                    when: vec!["session locked", "on battery"],
                    set: vec!["refresh 60 Hz"],
                    on: false,
                },
            ],
        }
    }
}

impl PluginUi for Automations {
    fn name(&self) -> &str {
        "Automations"
    }

    fn icon(&self) -> char {
        glyph::BOLT
    }

    fn status(&self) -> Status {
        Status {
            line: "1 of 2 on".into(),
            detail: "Your own rules. None holds right now".into(),
            color: theme::WAITING,
        }
    }

    fn page(&mut self, ui: &mut Ui) {
        let mut enabled = self.enabled;
        widgets::page_header(
            ui,
            glyph::BOLT,
            "Automations",
            "Your own rules, for what no other plugin covers: when this holds, set that.",
            |ui| {
                widgets::switch(ui, &mut enabled);
            },
        );
        self.enabled = enabled;

        for item in &mut self.items {
            widgets::card(ui, None, |ui| {
                egui::Sides::new().show(
                    ui,
                    |ui| {
                        ui.label(
                            egui::RichText::new(item.name)
                                .font(theme::strong(14.0))
                                .color(theme::TEXT),
                        );
                    },
                    |ui| {
                        widgets::switch(ui, &mut item.on);
                    },
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("When")
                            .font(theme::body(13.0))
                            .color(theme::MUTED),
                    );
                    for (i, w) in item.when.iter().enumerate() {
                        if i > 0 {
                            ui.label(
                                egui::RichText::new("and")
                                    .font(theme::body(13.0))
                                    .color(theme::MUTED),
                            );
                        }
                        widgets::chip(ui, None, w);
                    }
                    ui.label(
                        egui::RichText::new(glyph::ARROW)
                            .font(theme::icon(12.0))
                            .color(theme::MUTED),
                    );
                    ui.label(
                        egui::RichText::new("set")
                            .font(theme::body(13.0))
                            .color(theme::MUTED),
                    );
                    for s in &item.set {
                        widgets::chip(ui, None, s);
                    }
                });
            });
            ui.add_space(10.0);
        }
        widgets::button(ui, Some(glyph::ADD), "New automation");
    }
}
