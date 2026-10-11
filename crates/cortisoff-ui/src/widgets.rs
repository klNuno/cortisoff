//! The few controls the window draws itself.
//!
//! egui's own widgets are kept where they read well (combo boxes, scroll
//! areas). A switch, a card, a setting row, a chip and a slider whose track
//! can carry data are drawn here, because they carry most of the window.

use egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, Mesh,
    Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, TextEdit, Ui, UiBuilder, Vec2,
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
/// controls on the right, laid out left to right in the order they are added.
pub fn row(ui: &mut Ui, label: &str, hint: Option<&str>, control: impl FnOnce(&mut Ui)) {
    ui.add_space(4.0);
    let id = ui.next_auto_id().with("row");
    let reserved = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(0.0);
    // The text block's height from the frame before, to centre it on the
    // controls the way they are centred on it.
    let text_id = id.with("text");
    let text_height = ui.data(|d| d.get_temp::<f32>(text_id)).unwrap_or(20.0);
    ui.horizontal(|ui| {
        ui.set_min_height(text_height.max(32.0));
        let text_width = (ui.available_width() - reserved - 24.0).max(96.0);
        let text = ui.allocate_ui_with_layout(
            vec2(text_width, text_height),
            Layout::top_down(Align::Min),
            |ui| {
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
                ui.min_rect().height()
            },
        );
        if (text.inner - text_height).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(text_id, text.inner));
            ui.ctx().request_repaint();
        }
        trailing_with(ui, id, control);
    });
    ui.add_space(4.0);
}

/// Whatever `add` draws, left to right, pushed against the right edge of
/// the space left. Its width is measured on one frame and used on the next,
/// so the block lands in place one frame after it first appears or changes
/// size, and asks for that frame itself.
pub fn trailing<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let id = ui.next_auto_id().with("trailing");
    trailing_with(ui, id, add)
}

fn trailing_with<R>(ui: &mut Ui, id: egui::Id, add: impl FnOnce(&mut Ui) -> R) -> R {
    let width = ui.data(|d| d.get_temp::<f32>(id));
    let avail = ui.available_rect_before_wrap();
    let left = match width {
        Some(w) => (avail.right() - w).max(avail.left()),
        None => avail.left(),
    };
    let rect = Rect::from_min_max(pos2(left, avail.top()), avail.max);
    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt(id)
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    let out = add(&mut child);
    let used = child.min_rect();
    if width.is_none_or(|w| (used.width() - w).abs() > 0.5) {
        ui.data_mut(|d| d.insert_temp(id, used.width()));
        ui.ctx().request_repaint();
    }
    ui.advance_cursor_after_rect(used);
    out
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
    chip_with(ui, icon, text, Sense::hover())
}

/// A tag that answers a click, such as a list entry with a remove glyph.
/// It brightens under the pointer, the only difference from [`chip`].
pub fn chip_button(ui: &mut Ui, icon: Option<char>, text: &str) -> Response {
    chip_with(ui, icon, text, Sense::click())
}

fn chip_with(ui: &mut Ui, icon: Option<char>, text: &str, sense: Sense) -> Response {
    let font = theme::body(12.5);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, theme::TEXT);
    let icon_w = if icon.is_some() { 18.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + 18.0, 24.0);
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let hot = sense.senses_click() && response.hovered();
    let painter = ui.painter();
    let fill = if hot {
        theme::FIELD_HOVER
    } else {
        theme::FIELD
    };
    painter.rect_filled(rect, CornerRadius::same(12), fill);
    let mut x = rect.left() + 9.0;
    if let Some(icon) = icon {
        painter.text(
            pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            icon,
            theme::icon(12.0),
            if hot { theme::TEXT } else { theme::MUTED },
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

/// A line of text the user types, on the same quiet fill as [`field`]. A
/// faint outline shows where the caret is.
pub fn text_field(
    ui: &mut Ui,
    value: &mut String,
    hint: Option<&str>,
    width: f32,
    mono: bool,
) -> Response {
    let (rect, base) = ui.allocate_exact_size(vec2(width, 30.0), Sense::hover());
    let bg = ui.painter().add(Shape::Noop);
    let font = if mono {
        FontId::monospace(13.5)
    } else {
        theme::body(14.0)
    };
    let inner = rect.shrink2(vec2(10.0, 0.0));
    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt(base.id)
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
    );
    let mut edit = TextEdit::singleline(value)
        .id(base.id.with("text"))
        .frame(Frame::NONE)
        .margin(Margin::ZERO)
        .min_size(vec2(inner.width(), 30.0))
        .vertical_align(Align::Center)
        .desired_width(inner.width())
        .font(font.clone())
        .text_color(theme::TEXT);
    if let Some(hint) = hint {
        edit = edit.hint_text(egui::RichText::new(hint).font(font).color(theme::SUBTLE));
    }
    let response = child.add(edit);
    let focused = response.has_focus();
    let fill = if focused || response.hovered() {
        theme::FIELD_HOVER
    } else {
        theme::FIELD
    };
    ui.painter()
        .set(bg, Shape::rect_filled(rect, theme::RADIUS_CONTROL, fill));
    if focused {
        ui.painter().rect_stroke(
            rect,
            theme::RADIUS_CONTROL,
            Stroke::new(1.0, theme::SUBTLE),
            StrokeKind::Inside,
        );
    }
    response
}

/// One of a few choices, shown like [`field`], opening a list under it.
/// `selected` is the entry of `labels` showing now, `None` showing `fallback`
/// instead. Returns the entry picked this frame.
pub fn select(
    ui: &mut Ui,
    labels: &[String],
    selected: Option<usize>,
    fallback: &str,
) -> Option<usize> {
    let font = theme::body(14.0);
    let widest = labels
        .iter()
        .map(String::as_str)
        .chain([fallback])
        .map(|l| {
            ui.painter()
                .layout_no_wrap(l.to_owned(), font.clone(), theme::TEXT)
                .size()
                .x
        })
        .fold(0.0, f32::max);
    let width = (widest + 46.0).max(120.0);
    let shown = selected
        .and_then(|i| labels.get(i))
        .map_or(fallback, String::as_str);
    let response = field(ui, shown, width);
    let mut picked = None;
    egui::Popup::menu(&response)
        .width(width)
        .gap(4.0)
        .frame(
            Frame::new()
                .fill(theme::CARD)
                .stroke(Stroke::new(1.0, theme::CARD_BORDER))
                .corner_radius(theme::RADIUS_CONTROL)
                .inner_margin(Margin::same(4)),
        )
        .show(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (i, label) in labels.iter().enumerate() {
                let (rect, item) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
                let painter = ui.painter();
                if item.hovered() {
                    painter.rect_filled(rect, CornerRadius::same(4), theme::FIELD_HOVER);
                }
                let current = selected == Some(i);
                painter.text(
                    rect.left_center() + vec2(10.0, 0.0),
                    Align2::LEFT_CENTER,
                    label,
                    if current {
                        theme::strong(14.0)
                    } else {
                        theme::body(14.0)
                    },
                    if current { theme::TEXT } else { theme::MUTED },
                );
                if current {
                    painter.text(
                        rect.right_center() - vec2(10.0, 0.0),
                        Align2::RIGHT_CENTER,
                        glyph::CHECK,
                        theme::icon(11.0),
                        theme::TEXT,
                    );
                }
                if item.clicked() {
                    picked = Some(i);
                }
            }
        });
    picked
}

/// A time of day as `HH:MM`. Click to type one (`21:30`, `2130`, `9`), or
/// drag sideways to move it by five minutes every few pixels. `minute` is
/// the minute of the day.
pub fn time_field(ui: &mut Ui, minute: &mut f64) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(92.0, 30.0), Sense::click_and_drag());
    let edit_id = response.id.with("edit");
    let drag_id = response.id.with("drag");
    let mut editing: Option<String> = ui.data(|d| d.get_temp(edit_id));
    if editing.is_none() && response.clicked() {
        editing = Some(clock(*minute));
        ui.memory_mut(|m| m.request_focus(edit_id.with("text")));
    }
    let bg = ui.painter().add(Shape::Noop);
    let mut active = false;
    if let Some(mut text) = editing {
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt(edit_id)
                .max_rect(rect.shrink2(vec2(10.0, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        let edit = child.add(
            TextEdit::singleline(&mut text)
                .id(edit_id.with("text"))
                .frame(Frame::NONE)
                .margin(Margin::ZERO)
                .min_size(vec2(rect.width() - 20.0, 30.0))
                .vertical_align(Align::Center)
                .desired_width(rect.width() - 20.0)
                .font(theme::body(14.0))
                .text_color(theme::TEXT),
        );
        active = true;
        if edit.lost_focus() {
            if let Some(m) = parse_clock(&text) {
                if m != *minute {
                    *minute = m;
                    response.mark_changed();
                }
            }
            ui.data_mut(|d| d.remove::<String>(edit_id));
        } else {
            ui.data_mut(|d| d.insert_temp(edit_id, text));
        }
    } else {
        if response.drag_started() {
            ui.data_mut(|d| d.insert_temp(drag_id, (*minute, 0.0_f32)));
        }
        if response.dragged() {
            if let Some((start, moved)) = ui.data(|d| d.get_temp::<(f64, f32)>(drag_id)) {
                let moved = moved + response.drag_delta().x;
                ui.data_mut(|d| d.insert_temp(drag_id, (start, moved)));
                let steps = (moved / 6.0).round() as f64;
                let m = ((start / 5.0).round() * 5.0 + steps * 5.0).rem_euclid(1440.0);
                if m != *minute {
                    *minute = m;
                    response.mark_changed();
                }
            }
        }
        let painter = ui.painter();
        painter.text(
            rect.left_center() + vec2(10.0, 0.0),
            Align2::LEFT_CENTER,
            clock(*minute),
            theme::body(14.0),
            theme::TEXT,
        );
        painter.text(
            rect.right_center() - vec2(10.0, 0.0),
            Align2::RIGHT_CENTER,
            glyph::CLOCK,
            theme::icon(12.0),
            theme::MUTED,
        );
    }
    let fill = if active || response.hovered() || response.dragged() {
        theme::FIELD_HOVER
    } else {
        theme::FIELD
    };
    ui.painter()
        .set(bg, Shape::rect_filled(rect, theme::RADIUS_CONTROL, fill));
    if active {
        ui.painter().rect_stroke(
            rect,
            theme::RADIUS_CONTROL,
            Stroke::new(1.0, theme::SUBTLE),
            StrokeKind::Inside,
        );
    }
    response
}

/// `HH:MM` for a minute of the day.
pub fn clock(minute: f64) -> String {
    let m = (minute.round() as i64).rem_euclid(1440);
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// A typed time: `21:30`, `21.30`, `21h30`, `2130`, `930`, `21`.
pub fn parse_clock(text: &str) -> Option<f64> {
    let t = text.trim().to_ascii_lowercase();
    let (h, m) = match t.find([':', '.', 'h', ' ']) {
        Some(i) => {
            let (h, m) = (&t[..i], t[i + 1..].trim());
            (h.trim(), if m.is_empty() { "0" } else { m })
        }
        None if t.len() > 2 => t.split_at(t.len() - 2),
        None => (t.as_str(), "0"),
    };
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    (h < 24 && m < 60).then(|| f64::from(h * 60 + m))
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
            // Under the handle anyway, and a sliver narrower than its own
            // rounding draws as a stray mark beside it.
            if filled.width() >= 6.0 {
                painter.rect_filled(filled, CornerRadius::same(3), theme::MUTED);
            }
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
