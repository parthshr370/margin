//! Full screen in Margin. PdfCraft's full screen (`view.full_screen`) shows only the page and
//! leaves on Esc; Margin adds a bar at the top edge with the document's title and a button to
//! leave. The bar shows for a moment on entering, so the way out is seen, then hides until the
//! pointer touches the top edge, and stays while the pointer is over it.

use egui::{Align, Layout, Sense, UiBuilder};

use crate::PdfCraftApp;
use crate::margin::header;
use crate::margin::look::{self, GAP_LARGE};
use crate::margin::motion;
use crate::theme::Tokens;

/// How long the bar stays after entering full screen, in seconds.
const INTRO_SECONDS: f64 = 2.0;
/// The pointer this close to the top edge brings the bar back.
const REVEAL_EDGE: f32 = 8.0;
const BUTTON: f32 = 30.0;

/// When this run of full screen started (`since`) and the last frame it was drawn in; a gap in
/// frames means full screen was left and entered again.
#[derive(Clone, Copy)]
struct Session {
    since: f64,
    last_frame: u64,
}

/// The bar over the full-screen page. Call every full-screen frame.
pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context) {
    motion::sync(ctx, app.margin_keys.settings().ui.reduce_motion);
    let id = egui::Id::new("margin-focus");
    let (now, frame) = (ctx.input(|i| i.time), ctx.cumulative_frame_nr());
    let session = ctx
        .data(|d| d.get_temp::<Session>(id))
        .filter(|s| s.last_frame + 1 >= frame)
        .map_or(Session { since: now, last_frame: frame }, |s| Session { last_frame: frame, ..s });
    ctx.data_mut(|d| d.insert_temp(id, session));

    let screen = ctx.content_rect();
    let bar = egui::Rect::from_min_size(screen.min, egui::vec2(screen.width(), header::HEIGHT));
    let was_shown = ctx.data(|d| d.get_temp::<bool>(id.with("shown"))).unwrap_or(false);
    let pointer = ctx.input(|i| i.pointer.hover_pos());
    let intro_left = INTRO_SECONDS - (now - session.since);
    let shown = intro_left > 0.0 || pointer.is_some_and(|p| p.y <= screen.top() + REVEAL_EDGE || (was_shown && bar.contains(p)));
    ctx.data_mut(|d| d.insert_temp(id.with("shown"), shown));
    if intro_left > 0.0 {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(intro_left));
    }
    // The bar slides down from the top edge and fades in; it slides back up when it goes.
    let how_shown = motion::toggle(ctx, id.with("slide"), shown, motion::FADE);
    if how_shown <= 0.0 {
        return;
    }
    let t = Tokens::get(ctx);
    let top = bar.min - egui::vec2(0.0, (1.0 - how_shown) * header::HEIGHT);
    egui::Area::new(id.with("bar")).order(egui::Order::Foreground).fixed_pos(top).interactable(shown).fade_in(false).show(ctx, |ui| {
        ui.multiply_opacity(how_shown);
        let (rect, _) = ui.allocate_exact_size(bar.size(), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, t.chrome);
        ui.painter().hline(rect.x_range(), rect.bottom(), egui::Stroke::new(1.0, t.divider));
        let inner = rect.shrink2(egui::vec2(GAP_LARGE, 0.0));
        let buttons = UiBuilder::new().max_rect(inner).layout(Layout::right_to_left(Align::Center));
        let right_start = ui
            .scope_builder(buttons, |ui| {
                if exit_button(app, ui).clicked() {
                    app.set_full_screen(ui.ctx(), false);
                }
                ui.min_rect().left()
            })
            .inner;
        header::title(app, ui, &t, inner, inner.left(), right_start);
    });
}

/// The contract button: its tooltip names Esc and the keymap's full-screen keys.
fn exit_button(app: &PdfCraftApp, ui: &mut egui::Ui) -> egui::Response {
    let keys = match look::shortcut(app, "view.full_screen") {
        Some(keys) => format!("Esc · {keys}"),
        None => "Esc".to_owned(),
    };
    look::icon_button(ui, "minimize", BUTTON, false, "Exit full screen", Some(&keys))
}
