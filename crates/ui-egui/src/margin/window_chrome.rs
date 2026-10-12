//! The macOS title bar, drawn by Margin: the window's content runs to its top edge, the traffic
//! lights sit in the header row (centred on it, at the left of whatever row is there: the
//! sidebar's buttons, the header or the Library's bar), and the empty parts of those rows move
//! the window and zoom it on a double click, as a title bar does.
//!
//! Off macOS, in full screen (where the system hides the traffic lights) and in an embedder that
//! keeps the system title bar ([`PdfCraftApp::integrated_titlebar`] false), nothing moves and
//! every inset is 0.

use egui::{Id, Rect, Sense};

use crate::PdfCraftApp;

/// The memory key of the room the traffic lights take this frame, in egui points from the
/// window's left edge (0 when they don't show over the content).
fn width_id() -> Id {
    Id::new("margin-traffic-lights-width")
}

/// How much of a top row starting at `row` the traffic lights cover: what the row leaves empty
/// at its left before its first item. 0 for a row below the header band.
pub(crate) fn traffic_lights_inset(ctx: &egui::Context, row: Rect) -> f32 {
    if row.top() >= super::header::HEIGHT {
        return 0.0;
    }
    let width = ctx.data(|data| data.get_temp::<f32>(width_id())).unwrap_or(0.0);
    (width - row.left()).max(0.0)
}

/// Whether the traffic lights show over the content this frame.
pub(crate) fn traffic_lights_shown(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<f32>(width_id())).is_some_and(|width| width > 0.0)
}

/// Makes `rect`, the empty part of a top row, move the window when dragged and zoom it on a
/// double click. Call it before drawing the row's items, so they stay on top of it.
pub(crate) fn drag_area(app: &PdfCraftApp, ui: &egui::Ui, rect: Rect, id: Id) {
    if !app.integrated_titlebar {
        return;
    }
    let response = ui.interact(rect, id, Sense::click_and_drag());
    if response.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if response.double_clicked() {
        mac::toggle_zoom(ui.ctx());
    }
}

/// Centres the traffic lights on the header row and records the room they take
/// ([`traffic_lights_inset`]). Once per frame: AppKit lays them out again on a resize.
pub(crate) fn place_traffic_lights(app: &PdfCraftApp, ctx: &egui::Context) {
    let full_screen = ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
    let width = if app.integrated_titlebar && !full_screen { mac::centre_traffic_lights(ctx, super::header::HEIGHT / 2.0) } else { 0.0 };
    ctx.data_mut(|data| data.insert_temp(width_id(), width));
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
