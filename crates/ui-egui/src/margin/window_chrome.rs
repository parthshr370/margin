//! The window's title bar, drawn by Margin.
//!
//! macOS: the window's content runs to its top edge and the traffic lights sit in the header row
//! (centred on it, at the left of whatever row is there: the sidebar's buttons, the header or the
//! Library's bar).
//!
//! Linux, unless `ui.titlebar` is "system": the window has no decorations. Margin draws minimize,
//! maximize (restore when maximized) and close at the right end of the header row, whichever row
//! is there (the header or the Library's bar), and invisible edges and corners that resize the
//! window. Both stay above sheets, so the window can always be closed, moved aside or resized.
//!
//! On both, the empty parts of those rows move the window and zoom it on a double click, as a
//! title bar does. In full screen (where macOS hides the traffic lights), in focus mode (the
//! window buttons hide with the header) and in an embedder that keeps the system title bar
//! ([`PdfCraftApp::integrated_titlebar`] false), nothing moves and every inset is 0.

use egui::{Color32, CursorIcon, Id, Order, Rect, ResizeDirection, Response, Sense, Vec2, ViewportCommand, pos2, vec2};
use margin_config::{Settings, Titlebar};

use crate::PdfCraftApp;
use crate::icons;
use crate::margin::look;
use crate::margin::motion;
use crate::theme::Tokens;

/// Linux windows open without the desktop's decorations and Margin draws its own buttons and
/// resize edges (`ui.titlebar` "margin"). Elsewhere `ui.titlebar` changes nothing.
const CLIENT_SIDE: bool = cfg!(target_os = "linux");

/// Each window button's width (GNOME's and Zed's); they are as tall as the header row.
const WINDOW_BUTTON_WIDTH: f32 = 46.0;
/// Minimize, maximize or restore, close.
const WINDOW_BUTTON_COUNT: f32 = 3.0;
/// Space between the row's last item and the minimize button.
const GAP_BEFORE_WINDOW_BUTTONS: f32 = look::GAP;
/// The close button's fills under the pointer and while pressed (the red GNOME and Windows use)
/// and its icon on them.
const CLOSE_HOVER: Color32 = Color32::from_rgb(0xe8, 0x11, 0x23);
const CLOSE_PRESSED: Color32 = Color32::from_rgb(0xc5, 0x0f, 0x1f);
const CLOSE_ICON: Color32 = Color32::WHITE;

/// How far in from the window's sides a press resizes it, and how far along the sides from each
/// corner a press resizes both ways.
const RESIZE_EDGE: f32 = 6.0;
const RESIZE_CORNER: f32 = 12.0;

/// The memory key of the room the traffic lights take this frame, in egui points from the
/// window's left edge (0 when they don't show over the content).
fn traffic_lights_id() -> Id {
    Id::new("margin-traffic-lights-width")
}

/// The memory key of the room Margin's window buttons take this frame, in egui points from the
/// window's right edge, the gap before them included (0 when they don't show).
fn window_buttons_id() -> Id {
    Id::new("margin-window-buttons-width")
}

fn stored_width(ctx: &egui::Context, id: Id) -> f32 {
    ctx.data(|data| data.get_temp::<f32>(id)).unwrap_or(0.0)
}

/// How much of a top row starting at `row` the traffic lights cover: what the row leaves empty
/// at its left before its first item. 0 for a row below the header band.
pub(crate) fn traffic_lights_inset(ctx: &egui::Context, row: Rect) -> f32 {
    if row.top() >= super::header::HEIGHT {
        return 0.0;
    }
    (stored_width(ctx, traffic_lights_id()) - row.left()).max(0.0)
}

/// How much of a top row ending at `row`'s right Margin's window buttons cover, the gap before
/// them included: what the row leaves empty at its right after its last item. 0 for a row below
/// the header band.
pub(crate) fn window_buttons_inset(ctx: &egui::Context, row: Rect) -> f32 {
    if row.top() >= super::header::HEIGHT {
        return 0.0;
    }
    let width = stored_width(ctx, window_buttons_id());
    if width <= 0.0 {
        return 0.0;
    }
    (row.right() - (ctx.content_rect().right() - width)).max(0.0)
}

/// Whether window buttons (the traffic lights or Margin's own) sit in the header row this frame,
/// so a top row shares that row with them.
pub(crate) fn window_buttons_shown(ctx: &egui::Context) -> bool {
    stored_width(ctx, traffic_lights_id()) > 0.0 || stored_width(ctx, window_buttons_id()) > 0.0
}

/// Whether Margin draws the title bar: the embedder lets it, and on Linux `ui.titlebar` is
/// "margin".
pub(crate) fn integrated(app: &PdfCraftApp) -> bool {
    app.integrated_titlebar && (!CLIENT_SIDE || app.margin_keys.settings().ui.titlebar == Titlebar::Margin)
}

/// Whether Margin's window opens without the desktop's decorations: on Linux, unless `ui.titlebar`
/// in `<config>/settings.jsonc` is "system". Read before the window opens, so the system's title
/// bar never shows for a frame; [`place`] follows later changes to the setting.
pub fn starts_undecorated() -> bool {
    CLIENT_SIDE && margin_config::config_dir().is_none_or(|dir| Settings::load(&dir).settings.ui.titlebar == Titlebar::Margin)
}

/// Makes `rect`, the empty part of a top row, move the window when dragged and zoom it on a
/// double click. Call it before drawing the row's items, so they stay on top of it.
pub(crate) fn drag_area(app: &PdfCraftApp, ui: &egui::Ui, rect: Rect, id: Id) {
    if !integrated(app) {
        return;
    }
    let response = ui.interact(rect, id, Sense::click_and_drag());
    if response.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if response.double_clicked() {
        mac::toggle_zoom(ui.ctx());
    }
}

/// Records the room the window buttons take this frame ([`traffic_lights_inset`],
/// [`window_buttons_inset`]) and centres the traffic lights on the header row; on Linux, asks
/// for the desktop's decorations when `ui.titlebar` turns to "system" and drops them when it
/// turns back. `top_row_shown`: the header or the Library's bar shows (not focus mode over a
/// document). Once per frame, before the top rows: AppKit lays the traffic lights out again on a
/// resize.
pub(crate) fn place(app: &PdfCraftApp, ctx: &egui::Context, top_row_shown: bool) {
    let full_screen = app.full_screen || ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
    let integrated = integrated(app);
    let traffic_lights = if integrated && !full_screen { mac::centre_traffic_lights(ctx, super::header::HEIGHT / 2.0) } else { 0.0 };
    let window_buttons = if CLIENT_SIDE && integrated && !full_screen && top_row_shown {
        WINDOW_BUTTON_COUNT * WINDOW_BUTTON_WIDTH + GAP_BEFORE_WINDOW_BUTTONS
    } else {
        0.0
    };
    ctx.data_mut(|data| {
        data.insert_temp(traffic_lights_id(), traffic_lights);
        data.insert_temp(window_buttons_id(), window_buttons);
    });
    if CLIENT_SIDE && app.integrated_titlebar {
        follow_titlebar_setting(ctx, integrated);
    }
}

/// Sends the window its decorations when `undecorated` differs from what was last sent. The
/// first frame sends it too: the window opened as [`starts_undecorated`] read the file, and this
/// frame's settings are the ones that count.
fn follow_titlebar_setting(ctx: &egui::Context, undecorated: bool) {
    let id = Id::new("margin-window-undecorated");
    if ctx.data(|data| data.get_temp::<bool>(id)) == Some(undecorated) {
        return;
    }
    ctx.data_mut(|data| data.insert_temp(id, undecorated));
    ctx.send_viewport_cmd(ViewportCommand::Decorations(!undecorated));
}

/// Margin's window buttons at the right end of the header row and the resize edges round the
/// window (Linux, `ui.titlebar` "margin"). Call once per frame after [`place`]; they sit above
/// everything, sheets too, but only over their own small rects.
pub(crate) fn show_window_controls(app: &PdfCraftApp, ctx: &egui::Context) {
    if !CLIENT_SIDE || !integrated(app) {
        return;
    }
    if stored_width(ctx, window_buttons_id()) > 0.0 {
        window_buttons(ctx);
    }
    let (maximized, full_screen) = ctx.input(|input| (input.viewport().maximized.unwrap_or(false), input.viewport().fullscreen.unwrap_or(false)));
    if !maximized && !full_screen && !app.full_screen {
        resize_edges(ctx);
    }
}

/// One of Margin's window buttons.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowButton {
    Minimize,
    Maximize,
    Restore,
    Close,
}

impl WindowButton {
    /// Lucide's glyph: a bar, a square, two overlapping squares, a cross.
    fn icon(self) -> &'static str {
        match self {
            Self::Minimize => "minus",
            Self::Maximize => "square",
            Self::Restore => "copy",
            Self::Close => "x",
        }
    }

    /// The accessible name screen readers and tests see.
    fn label(self) -> &'static str {
        match self {
            Self::Minimize => "Minimize",
            Self::Maximize => "Maximize",
            Self::Restore => "Restore",
            Self::Close => "Close",
        }
    }
}

/// Minimize, maximize or restore and close, flush with the window's top right corner.
fn window_buttons(ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let origin = pos2(screen.right() - WINDOW_BUTTON_COUNT * WINDOW_BUTTON_WIDTH, screen.top());
    let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
    let zoom = if maximized { WindowButton::Restore } else { WindowButton::Maximize };
    egui::Area::new(Id::new("margin-window-buttons")).order(Order::Tooltip).fixed_pos(origin).sense(Sense::hover()).fade_in(false).show(ctx, |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.horizontal(|ui| {
            if window_button(ui, &t, WindowButton::Minimize).clicked() {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
            }
            if window_button(ui, &t, zoom).clicked() {
                mac::toggle_zoom(ctx);
            }
            if window_button(ui, &t, WindowButton::Close).clicked() {
                // The same request as the desktop's close button: unsaved edits are asked about.
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        });
    });
}

/// A flat button the header row's height: the theme's hover fill under the pointer (red with a
/// white icon for close), the pressed fill while held.
fn window_button(ui: &mut egui::Ui, t: &Tokens, button: WindowButton) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(WINDOW_BUTTON_WIDTH, super::header::HEIGHT), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, button.label()));
    let pressed = response.is_pointer_button_down_on();
    let lit = motion::hover(ui.ctx(), response.id, response.hovered() || pressed);
    let is_close = button == WindowButton::Close;
    let fill = match (is_close, pressed) {
        (true, true) => CLOSE_PRESSED,
        (true, false) => CLOSE_HOVER.gamma_multiply(lit),
        (false, true) => t.pressed,
        (false, false) => t.hover.gamma_multiply(lit),
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    look::focus_ring(ui, t, rect, &response, 0);
    let tint = if is_close && lit > 0.0 { CLOSE_ICON } else { look::icon_tint(t, false, response.hovered()) };
    icons::paint(ui, rect, button.icon(), look::ICON_SMALL, tint);
    response
}

/// The window's sides and corners as resize grips: a strip [`RESIZE_EDGE`] deep along each side,
/// a [`RESIZE_CORNER`] square at each corner.
fn resize_edges(ctx: &egui::Context) {
    let screen = ctx.content_rect();
    let (left, right, top, bottom) = (screen.left(), screen.right(), screen.top(), screen.bottom());
    let corner = Vec2::splat(RESIZE_CORNER);
    let along_top_and_bottom = (left + RESIZE_CORNER)..=(right - RESIZE_CORNER);
    let along_sides = (top + RESIZE_CORNER)..=(bottom - RESIZE_CORNER);
    let grips = [
        (ResizeDirection::North, CursorIcon::ResizeNorth, Rect::from_x_y_ranges(along_top_and_bottom.clone(), top..=top + RESIZE_EDGE)),
        (ResizeDirection::South, CursorIcon::ResizeSouth, Rect::from_x_y_ranges(along_top_and_bottom, bottom - RESIZE_EDGE..=bottom)),
        (ResizeDirection::West, CursorIcon::ResizeWest, Rect::from_x_y_ranges(left..=left + RESIZE_EDGE, along_sides.clone())),
        (ResizeDirection::East, CursorIcon::ResizeEast, Rect::from_x_y_ranges(right - RESIZE_EDGE..=right, along_sides)),
        (ResizeDirection::NorthWest, CursorIcon::ResizeNorthWest, Rect::from_min_size(pos2(left, top), corner)),
        (ResizeDirection::NorthEast, CursorIcon::ResizeNorthEast, Rect::from_min_size(pos2(right - RESIZE_CORNER, top), corner)),
        (ResizeDirection::SouthWest, CursorIcon::ResizeSouthWest, Rect::from_min_size(pos2(left, bottom - RESIZE_CORNER), corner)),
        (ResizeDirection::SouthEast, CursorIcon::ResizeSouthEast, Rect::from_min_size(pos2(right - RESIZE_CORNER, bottom - RESIZE_CORNER), corner)),
    ];
    for (index, (direction, cursor, rect)) in grips.into_iter().enumerate() {
        if rect.is_positive() {
            resize_grip(ctx, Id::new("margin-resize-grip").with(index), direction, cursor, rect);
        }
    }
}

/// One grip: its own small area over everything, so the content under the rest of the window
/// keeps its pointer, the resize cursor under the pointer, and a drag hands the resize to the
/// window manager.
fn resize_grip(ctx: &egui::Context, id: Id, direction: ResizeDirection, cursor: CursorIcon, rect: Rect) {
    egui::Area::new(id).order(Order::Tooltip).fixed_pos(rect.min).sense(Sense::hover()).fade_in(false).show(ctx, |ui| {
        let (_, response) = ui.allocate_exact_size(rect.size(), Sense::drag());
        if response.drag_started() {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
        }
        response.on_hover_cursor(cursor);
    });
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplication, NSWindow, NSWindowButton, NSWindowStyleMask};
    use objc2_foundation::NSPoint;

    /// Space between the zoom button and the first item of the row, in egui points.
    const GAP_AFTER_BUTTONS: f32 = 10.0;
    /// Below this difference a button is where it should be (AppKit points).
    const PLACED_WITHIN: f64 = 0.25;

    /// Moves the three buttons of Margin's window so their centres sit `centre_y` egui points
    /// below its top, and returns how far right they reach plus a gap, in egui points (0 when
    /// there is no such window, or this is not the main thread).
    pub(super) fn centre_traffic_lights(ctx: &egui::Context, centre_y: f32) -> f32 {
        // egui's points are the system's divided by the zoom factor.
        let zoom = f64::from(ctx.zoom_factor().max(0.01));
        let Some(window) = margin_window() else { return 0.0 };
        let buttons = [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton];
        let mut right_edge = 0.0f64;
        for kind in buttons {
            let Some(button) = window.standardWindowButton(kind) else { continue };
            let frame = button.frame();
            // The buttons' own title bar view is as tall as the system title bar and starts at the
            // window's top; its y axis points up.
            let title_bar_height = window.frame().size.height - window.contentLayoutRect().size.height;
            let y = title_bar_height - f64::from(centre_y) * zoom - frame.size.height / 2.0;
            if (frame.origin.y - y).abs() > PLACED_WITHIN {
                button.setFrameOrigin(NSPoint::new(frame.origin.x, y));
            }
            right_edge = right_edge.max(frame.origin.x + frame.size.width);
        }
        if right_edge <= 0.0 {
            return 0.0;
        }
        (right_edge / zoom) as f32 + GAP_AFTER_BUTTONS
    }

    /// Zooms Margin's window or returns it to its size, as a double click on a title bar does.
    /// AppKit's own toggle: egui reads `maximized` only at start on macOS (egui#3494), so
    /// `ViewportCommand::Maximized(!maximized)` would only ever zoom.
    pub(super) fn toggle_zoom(_ctx: &egui::Context) {
        if let Some(window) = margin_window() {
            window.zoom(None);
        }
    }

    /// Margin's window: the one whose content runs under a transparent title bar (file panels
    /// and alerts keep the system's). `None` off the main thread.
    fn margin_window() -> Option<Retained<NSWindow>> {
        let main_thread = MainThreadMarker::new()?;
        let windows = NSApplication::sharedApplication(main_thread).windows();
        windows.iter().find(|window| window.titlebarAppearsTransparent() && window.styleMask().contains(NSWindowStyleMask::FullSizeContentView))
    }
}

#[cfg(not(target_os = "macos"))]
mod mac {
    /// Only macOS draws its window buttons over the content.
    pub(super) fn centre_traffic_lights(_ctx: &egui::Context, _centre_y: f32) -> f32 {
        0.0
    }

    /// Maximizes the window or restores it (egui keeps `maximized` current off macOS).
    pub(super) fn toggle_zoom(ctx: &egui::Context) {
        let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }
}
