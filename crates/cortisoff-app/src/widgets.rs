//! The few controls the window draws itself.
//!
//! egui's own widgets are kept where they read well (combo boxes, scroll
//! areas). A switch, a card, a setting row, a chip and a slider whose track
//! can carry data are drawn here, because they carry most of the window.

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, Frame, Layout, Margin, Mesh, Pos2,
    Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
};

use crate::theme::{self, glyph};

/// An on and off switch, 36 by 20.
pub fn switch(ui: &mut Ui, on: &mut bool) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(36.0, 20.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let track = if *on {
        theme::TEXT
    } else if response.hovered() {
        theme::FIELD_HOVER
    } else {
        theme::FIELD
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(10), track);
    if !*on {
        painter.rect_stroke(
            rect,
            CornerRadius::same(10),
            Stroke::new(1.0, theme::CARD_BORDER),
            StrokeKind::Inside,
        );
    }
    let x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
    let knob = if *on { theme::BG } else { theme::MUTED };
    painter.circle_filled(pos2(x, rect.center().y), 7.0, knob);
    response
}

/// A titled block of settings. Everything on a plugin page sits in one.
pub fn card<R>(ui: &mut Ui, title: Option<&str>, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::CARD_BORDER))
        .corner_radius(theme::RADIUS_CARD)
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(title) = title {
                ui.label(
                    egui::RichText::new(title)
                        .font(theme::strong(14.0))
                        .color(theme::TEXT),
                );
                ui.add_space(4.0);
            }
            add(ui)
        })
        .inner
}

/// One line of a card: what it is on the left, with a hint under it, and the
/// control on the right.
pub fn row(ui: &mut Ui, label: &str, hint: Option<&str>, control: impl FnOnce(&mut Ui)) {
    ui.add_space(4.0);
    egui::Sides::new().height(32.0).show(
        ui,
        |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(
                    egui::RichText::new(label)
                        .font(theme::body(14.0))
                        .color(theme::TEXT),
                );
                if let Some(hint) = hint {
                    ui.label(
                        egui::RichText::new(hint)
                            .font(theme::body(12.5))
                            .color(theme::MUTED),
                    );
                }
            });
        },
        control,
    );
    ui.add_space(4.0);
}

pub fn divider(ui: &mut Ui) {
    let rect = ui.available_rect_before_wrap();
    let y = ui.cursor().top() + 2.0;
    ui.painter()
        .hline(rect.x_range(), y, Stroke::new(1.0, theme::CARD_BORDER));
    ui.add_space(5.0);
}

/// A small rounded tag. `icon` is a glyph from the system icon font.
pub fn chip(ui: &mut Ui, icon: Option<char>, text: &str) -> Response {
    let font = theme::body(12.5);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, theme::TEXT);
    let icon_w = if icon.is_some() { 18.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + 18.0, 24.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(12), theme::FIELD);
    let mut x = rect.left() + 9.0;
    if let Some(icon) = icon {
        painter.text(
            pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            icon,
            theme::icon(12.0),
            theme::MUTED,
        );
        x += icon_w;
    }
    painter.galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        theme::TEXT,
    );
    response
}

/// A button with a quiet fill, the default for anything that is not the one
/// action a page exists for.
pub fn button(ui: &mut Ui, icon: Option<char>, text: &str) -> Response {
    let font = theme::body(13.5);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, theme::TEXT);
    let icon_w = if icon.is_some() { 20.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + 24.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if response.hovered() {
        theme::FIELD_HOVER
    } else {
        theme::FIELD
    };
    let painter = ui.painter();
    painter.rect_filled(rect, theme::RADIUS_CONTROL, fill);
    let mut x = rect.left() + 12.0;
    if let Some(icon) = icon {
        painter.text(
            pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            icon,
            theme::icon(13.0),
            theme::TEXT,
        );
        x += icon_w;
    }
    painter.galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        theme::TEXT,
    );
    response
}

/// A value that reads like a field: a time, a duration, an application.
pub fn field(ui: &mut Ui, text: &str, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    let fill = if response.hovered() {
        theme::FIELD_HOVER
    } else {
        theme::FIELD
    };
    let painter = ui.painter();
    painter.rect_filled(rect, theme::RADIUS_CONTROL, fill);
    painter.text(
        rect.left_center() + vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        text,
        theme::body(14.0),
        theme::TEXT,
    );
    painter.text(
        rect.right_center() - vec2(10.0, 0.0),
        Align2::RIGHT_CENTER,
        glyph::CHEVRON_DOWN,
        theme::icon(10.0),
        theme::MUTED,
    );
    response
}

/// A coloured dot saying what a plugin is doing.
pub fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// What a slider's track shows under the handle.
pub enum Track {
    /// One neutral fill up to the handle.
    Plain,
    /// The black body colour at every point, for a colour temperature, where
    /// the hue is the data.
    Kelvin,
}

/// A number on a track, with its value printed to the right.
pub fn slider(
    ui: &mut Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f32,
    track: Track,
    text: impl Fn(f32) -> String,
) -> Response {
    let width = 220.0;
    let (rect, mut response) =
        ui.allocate_exact_size(vec2(width + 70.0, 30.0), Sense::click_and_drag());
    let rail = Rect::from_min_size(
        pos2(rect.left() + 8.0, rect.center().y - 3.0),
        vec2(width - 16.0, 6.0),
    );
    if let Some(p) = response.interact_pointer_pos() {
        let t = ((p.x - rail.left()) / rail.width()).clamp(0.0, 1.0);
        let raw = egui::lerp(range.clone(), t);
        let snapped = ((raw - range.start()) / step).round() * step + range.start();
        if snapped != *value {
            *value = snapped;
            response.mark_changed();
        }
    }
    let t = egui::remap_clamp(*value, range.clone(), 0.0..=1.0);
    let painter = ui.painter();
    match track {
        Track::Plain => {
            painter.rect_filled(rail, CornerRadius::same(3), theme::FIELD);
            let filled =
                Rect::from_min_max(rail.min, pos2(egui::lerp(rail.x_range(), t), rail.max.y));
            painter.rect_filled(filled, CornerRadius::same(3), theme::MUTED);
        }
        Track::Kelvin => {
            gradient(painter, rail, 48, |x| {
                theme::kelvin(egui::lerp(range.clone(), x))
            });
        }
    }
    let handle = pos2(egui::lerp(rail.x_range(), t), rail.center().y);
    painter.circle_filled(handle, 8.0, theme::TEXT);
    painter.circle_stroke(handle, 8.0, Stroke::new(2.0, theme::CARD));
    painter.text(
        pos2(rect.right(), rect.center().y),
        Align2::RIGHT_CENTER,
        text(*value),
        theme::strong(14.0),
        theme::TEXT,
    );
    response
}

/// A horizontal gradient, sampled `steps` times, with rounded ends.
pub fn gradient(painter: &egui::Painter, rect: Rect, steps: usize, color: impl Fn(f32) -> Color32) {
    let mut mesh = Mesh::default();
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = egui::lerp(rect.x_range(), t);
        let c = color(t);
        mesh.colored_vertex(pos2(x, rect.top()), c);
        mesh.colored_vertex(pos2(x, rect.bottom()), c);
        if i > 0 {
            let n = (i * 2) as u32;
            mesh.add_triangle(n - 2, n - 1, n);
            mesh.add_triangle(n - 1, n, n + 1);
        }
    }
    painter.add(Shape::mesh(mesh));
    let r = rect.height() / 2.0;
    painter.circle_filled(pos2(rect.left(), rect.center().y), r, color(0.0));
    painter.circle_filled(pos2(rect.right(), rect.center().y), r, color(1.0));
}

/// One entry of the sidebar: an icon, a name, a status line and a dot.
pub fn nav_item(
    ui: &mut Ui,
    icon: char,
    label: &str,
    status: Option<(&str, Color32)>,
    selected: bool,
) -> Response {
    let height = if status.is_some() { 48.0 } else { 36.0 };
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, theme::RADIUS_CONTROL, theme::SELECTED);
        let bar = Rect::from_min_size(
            pos2(rect.left(), rect.top() + 12.0),
            vec2(3.0, rect.height() - 24.0),
        );
        painter.rect_filled(bar, CornerRadius::same(2), theme::TEXT);
    } else if response.hovered() {
        painter.rect_filled(
            rect,
            theme::RADIUS_CONTROL,
            Color32::from_rgb(0x1a, 0x1b, 0x1f),
        );
    }
    let fg = if selected { theme::TEXT } else { theme::MUTED };
    painter.text(
        pos2(rect.left() + 16.0, rect.center().y),
        Align2::LEFT_CENTER,
        icon,
        theme::icon(16.0),
        fg,
    );
    let x = rect.left() + 44.0;
    match status {
        Some((line, color)) => {
            painter.text(
                pos2(x, rect.top() + 15.0),
                Align2::LEFT_CENTER,
                label,
                theme::strong(13.5),
                theme::TEXT,
            );
            painter.circle_filled(pos2(x + 3.0, rect.top() + 33.0), 3.0, color);
            let clip = Rect::from_min_max(
                pos2(x + 10.0, rect.top()),
                pos2(rect.right() - 8.0, rect.bottom()),
            );
            painter.with_clip_rect(clip).text(
                pos2(x + 11.0, rect.top() + 33.0),
                Align2::LEFT_CENTER,
                line,
                theme::body(12.5),
                theme::MUTED,
            );
        }
        None => {
            painter.text(
                pos2(x, rect.center().y),
                Align2::LEFT_CENTER,
                label,
                theme::body(14.0),
                fg,
            );
        }
    }
    response
}

/// The block every plugin page starts with: icon, name, what it does, and
/// the controls that apply to the whole plugin.
pub fn page_header(
    ui: &mut Ui,
    icon: char,
    title: &str,
    summary: &str,
    right: impl FnOnce(&mut Ui),
) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::same(10), theme::FIELD);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            icon,
            theme::icon(20.0),
            theme::TEXT,
        );
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                egui::RichText::new(title)
                    .font(theme::strong(22.0))
                    .color(theme::TEXT),
            );
            ui.label(
                egui::RichText::new(summary)
                    .font(theme::body(13.5))
                    .color(theme::MUTED),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Center), right);
    });
    ui.add_space(14.0);
}

/// A muted line of text.
pub fn hint(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::body(12.5))
            .color(theme::MUTED),
    );
}

pub fn point(rect: Rect, x: f32, y: f32) -> Pos2 {
    pos2(egui::lerp(rect.x_range(), x), egui::lerp(rect.y_range(), y))
}
