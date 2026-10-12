//! The window's own title bar. The system frame is off everywhere but on
//! macOS, so the window draws its caption buttons, moves from its top band
//! and resizes from its edges itself. macOS keeps its traffic lights over a
//! transparent title bar, and its own edges.
//!
//! On Windows the system still does the moving and the resizing: each frame
//! tells it which pixels are the band and which are the edges, and its hit
//! test answers from the press on (see `cortisoff_win::frame`). Elsewhere a
//! press on them asks the window manager to start a move or a resize.

use eframe::egui::{
    self, pos2, vec2, Color32, CursorIcon, Id, LayerId, Order, PointerButton, Rect,
    ResizeDirection, Response, Sense, Stroke, Ui, UiBuilder, ViewportBuilder, ViewportCommand,
};

use cortisoff_ui::theme;

/// The band a press moves the window from: Windows 11's caption height, and
/// macOS's title bar where its traffic lights sit.
pub const TITLE_BAR: f32 = if cfg!(target_os = "macos") {
    28.0
} else {
    32.0
};

/// Room above the sidebar's first item. On macOS the traffic lights take the
/// top-left corner.
pub const SIDEBAR_TOP: f32 = if cfg!(target_os = "macos") {
    40.0
} else {
    14.0
};

/// Whether the window draws its own caption buttons and resize edges.
const OWN_FRAME: bool = !cfg!(target_os = "macos");

/// A caption button, as wide as Windows 11 draws them.
const BUTTON: egui::Vec2 = vec2(46.0, 32.0);
/// Windows 11's close-button red, and the shade it takes while pressed.
const CLOSE_HOVER: Color32 = Color32::from_rgb(0xc4, 0x2b, 0x1c);
const CLOSE_DOWN: Color32 = Color32::from_rgb(0xb2, 0x27, 0x1a);

/// How far in from the window's edge a press resizes it, and how far along
/// an edge a corner reaches.
const EDGE: f32 = 5.0;
const CORNER: f32 = 12.0;

/// Whether the system answers the window's hit test, see [`adopt`].
#[cfg(windows)]
static NATIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn native() -> bool {
    #[cfg(windows)]
    {
        NATIVE.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Hands the window's hit test to the system, once the window exists.
#[cfg(windows)]
pub fn adopt(cc: &eframe::CreationContext<'_>) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = cc.window_handle() else {
        return;
    };
    if let RawWindowHandle::Win32(window) = handle.as_raw() {
        let adopted = cortisoff_win::frame::adopt(window.hwnd.get());
        NATIVE.store(adopted, std::sync::atomic::Ordering::Relaxed);
    }
}

/// What this frame drew in the top band: where a press moves the window,
/// and the widgets over it that take their own.
#[derive(Clone, Default)]
struct Band {
    caption: Vec<Rect>,
    buttons: Vec<Rect>,
}

fn band(ui: &Ui, f: impl FnOnce(&mut Band)) {
    ui.ctx()
        .data_mut(|d| f(d.get_temp_mut_or_default::<Band>(Id::new("cortisoff-band"))));
}

/// Marks a widget drawn over the top band: a press on it is its own, not a
/// move.
pub fn over_band(ui: &Ui, rect: Rect) {
    band(ui, |b| b.buttons.push(rect));
}

/// The frame the window asks the system for.
pub fn viewport(builder: ViewportBuilder) -> ViewportBuilder {
    if cfg!(target_os = "macos") {
        builder
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
    } else {
        // egui-winit keeps the system's drop shadow on an undecorated
        // window, and Windows 11 still rounds its corners and snaps it.
        builder.with_decorations(false)
    }
}

/// The top band of the main area: it moves the window, and holds the caption
/// buttons where the window draws its own.
pub fn title_bar(ui: &mut Ui) {
    let width = ui.available_width();
    let (rect, bar) = ui.allocate_exact_size(vec2(width, TITLE_BAR), Sense::click_and_drag());
    band(ui, |b| b.caption.push(rect));
    if OWN_FRAME {
        caption_buttons(ui, rect);
    }
    drag(ui, &bar);
}

/// The sidebar's share of the top band, behind whatever the sidebar draws
/// there afterwards. `ui` is the sidebar's, `margin` its side margin.
pub fn sidebar_band(ui: &mut Ui, margin: f32) {
    let top = ui.max_rect();
    let band = Rect::from_min_max(
        pos2(top.left() - margin, top.top()),
        pos2(top.right() + margin, top.top() + TITLE_BAR),
    );
    let response = ui.interact(
        band,
        Id::new("cortisoff-sidebar-band"),
        Sense::click_and_drag(),
    );
    self::band(ui, |b| b.caption.push(band));
    drag(ui, &response);
}

/// What a native title bar does with a press: a drag moves the window, a
/// double click maximises it or puts it back.
fn drag(ui: &Ui, bar: &Response) {
    if bar.double_clicked() {
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx()
            .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    } else if bar.drag_started_by(PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Caption {
    Minimize,
    Maximize,
    Restore,
    Close,
}

fn caption_buttons(ui: &mut Ui, bar: Rect) {
    let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
    let middle = if maximized {
        Caption::Restore
    } else {
        Caption::Maximize
    };
    let mut right = bar.right();
    for kind in [Caption::Close, middle, Caption::Minimize] {
        let rect = Rect::from_min_size(pos2(right - BUTTON.x, bar.top()), BUTTON);
        right -= BUTTON.x;
        over_band(ui, rect);
        if caption_button(ui, rect, kind).clicked() {
            ui.ctx().send_viewport_cmd(match kind {
                Caption::Minimize => ViewportCommand::Minimized(true),
                Caption::Maximize => ViewportCommand::Maximized(true),
                Caption::Restore => ViewportCommand::Maximized(false),
                Caption::Close => ViewportCommand::Close,
            });
        }
    }
}

fn caption_button(ui: &mut Ui, rect: Rect, kind: Caption) -> Response {
    let id = Id::new(("cortisoff-caption", kind as u8));
    let response = ui.interact(rect, id, Sense::click());
    let (label, tip) = match kind {
        Caption::Minimize => ("Minimise", "Minimise"),
        Caption::Maximize => ("Maximise", "Maximise"),
        Caption::Restore => ("Restore", "Restore down"),
        Caption::Close => ("Close", "Close"),
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));

    let down = response.is_pointer_button_down_on();
    let hovered = response.hovered();
    let (fill, ink) = match (kind, down, hovered) {
        (Caption::Close, true, _) => (CLOSE_DOWN, Color32::WHITE),
        (Caption::Close, false, true) => (CLOSE_HOVER, Color32::WHITE),
        (_, true, _) => (Color32::from_white_alpha(10), theme::MUTED),
        (_, false, true) => (Color32::from_white_alpha(16), theme::TEXT),
        _ => (Color32::TRANSPARENT, theme::MUTED),
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, fill);

    // Ten points wide like Windows 11's glyphs, on whole pixels so a one-pixel
    // line stays one pixel.
    let stroke = Stroke::new(1.0, ink);
    let c = pos2(
        painter.round_to_pixel_center(rect.center().x),
        painter.round_to_pixel_center(rect.center().y),
    );
    let h = 5.0;
    match kind {
        Caption::Minimize => {
            painter.hline(c.x - h..=c.x + h, c.y, stroke);
        }
        Caption::Maximize => {
            let square = Rect::from_center_size(c, vec2(2.0 * h, 2.0 * h));
            painter.rect_stroke(square, 1.0, stroke, egui::StrokeKind::Middle);
        }
        Caption::Restore => {
            let front = Rect::from_min_size(pos2(c.x - h, c.y - h + 2.0), vec2(8.0, 8.0));
            painter.rect_stroke(front, 1.0, stroke, egui::StrokeKind::Middle);
            // The window behind shows only above and to the right of the
            // one in front.
            painter.add(egui::Shape::line(
                vec![
                    pos2(c.x - h + 2.0, c.y - h + 2.0),
                    pos2(c.x - h + 2.0, c.y - h),
                    pos2(c.x + h, c.y - h),
                    pos2(c.x + h, c.y + h - 2.0),
                    pos2(c.x + h - 2.0, c.y + h - 2.0),
                ],
                stroke,
            ));
        }
        Caption::Close => {
            painter.line_segment([c + vec2(-h, -h), c + vec2(h, h)], stroke);
            painter.line_segment([c + vec2(-h, h), c + vec2(h, -h)], stroke);
        }
    }
    response.on_hover_text_at_pointer(tip)
}

/// Resizes the window from its edges, which an undecorated window no longer
/// has. Drawn above everything, so a press on an edge never reaches the
/// widget under it. Nothing while maximised. Called last in the frame: on
/// Windows it hands the band and the edges to the system's hit test.
pub fn resize_edges(ui: &mut Ui) {
    let drawn = ui
        .ctx()
        .data_mut(|d| d.remove_temp::<Band>(Id::new("cortisoff-band")))
        .unwrap_or_default();
    if !OWN_FRAME {
        return;
    }
    let (maximized, fullscreen) = ui.input(|i| {
        (
            i.viewport().maximized.unwrap_or(false),
            i.viewport().fullscreen.unwrap_or(false),
        )
    });
    #[cfg(windows)]
    if native() {
        let ppp = ui.ctx().pixels_per_point();
        let px = |r: &Rect| {
            [
                (r.left() * ppp).round() as i32,
                (r.top() * ppp).round() as i32,
                (r.right() * ppp).round() as i32,
                (r.bottom() * ppp).round() as i32,
            ]
        };
        let resizable = !(maximized || fullscreen);
        cortisoff_win::frame::set(cortisoff_win::frame::Regions {
            caption: drawn.caption.iter().map(px).collect(),
            buttons: drawn.buttons.iter().map(px).collect(),
            edge: if resizable {
                (EDGE * ppp).round() as i32
            } else {
                0
            },
            corner: (CORNER * ppp).round() as i32,
        });
    }
    #[cfg(not(windows))]
    let _ = drawn;
    if maximized || fullscreen || native() {
        return;
    }
    let window = ui.ctx().content_rect();
    let layer = LayerId::new(Order::Foreground, Id::new("cortisoff-resize"));
    let edges = ui.new_child(UiBuilder::new().layer_id(layer).max_rect(window));
    // Windows draws a border and a shadow around any window with a frame
    // style; a Wayland or X11 window without decorations gets neither, and
    // its dark edge would melt into a dark desktop.
    if cfg!(all(unix, not(target_os = "macos"))) {
        edges.painter().rect_stroke(
            window,
            0.0,
            Stroke::new(1.0, Color32::from_white_alpha(24)),
            egui::StrokeKind::Inside,
        );
    }
    let (l, r, t, b) = (window.left(), window.right(), window.top(), window.bottom());
    let zones = [
        (ResizeDirection::NorthWest, [l, t, l + CORNER, t + CORNER]),
        (ResizeDirection::NorthEast, [r - CORNER, t, r, t + CORNER]),
        (ResizeDirection::SouthWest, [l, b - CORNER, l + CORNER, b]),
        (ResizeDirection::SouthEast, [r - CORNER, b - CORNER, r, b]),
        (ResizeDirection::North, [l, t, r, t + EDGE]),
        (ResizeDirection::South, [l, b - EDGE, r, b]),
        (ResizeDirection::West, [l, t, l + EDGE, b]),
        (ResizeDirection::East, [r - EDGE, t, r, b]),
    ];
    // Added last, the corners sit on top of the two edges they overlap.
    for (n, (dir, [x0, y0, x1, y1])) in zones.into_iter().rev().enumerate() {
        let rect = Rect::from_min_max(pos2(x0, y0), pos2(x1, y1));
        let response = edges.interact(rect, Id::new(("cortisoff-resize", n)), Sense::drag());
        if response.hovered() {
            edges.ctx().set_cursor_icon(cursor(dir));
            if edges.input(|i| i.pointer.primary_pressed()) {
                edges
                    .ctx()
                    .send_viewport_cmd(ViewportCommand::BeginResize(dir));
            }
        }
    }
}

fn cursor(dir: ResizeDirection) -> CursorIcon {
    match dir {
        ResizeDirection::North => CursorIcon::ResizeNorth,
        ResizeDirection::South => CursorIcon::ResizeSouth,
        ResizeDirection::East => CursorIcon::ResizeEast,
        ResizeDirection::West => CursorIcon::ResizeWest,
        ResizeDirection::NorthEast => CursorIcon::ResizeNorthEast,
        ResizeDirection::NorthWest => CursorIcon::ResizeNorthWest,
        ResizeDirection::SouthEast => CursorIcon::ResizeSouthEast,
        ResizeDirection::SouthWest => CursorIcon::ResizeSouthWest,
    }
}
