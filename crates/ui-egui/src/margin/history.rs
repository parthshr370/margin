//! Back means the exact place before a jump (R8 QoL 6). PdfCraft's `DocView` view history
//! (`back`, `forward`, `go_to_page`, `view_history`) stays the one history; in Margin each
//! entry also carries the reading spot and the zoom ([`Place`]), so Back, Forward, Alt+←/→, the
//! header's arrows and the mouse's back/forward buttons return to the paragraph that was being
//! read. Page turns stay out of it, a search keeps the reader's place once however many matches
//! it steps through, and a quiet "Back to p. N" pill offers the way back for a few seconds
//! after each jump (the mockup's `.backpill`).

use egui::{Align2, PointerButton, pos2};

use super::look::{self, BUTTON_HEIGHT, GAP};
use super::motion;
use super::reading::{self, Spot};
use crate::PdfCraftApp;
use crate::canvas::{DocView, Fit, ViewPlace};
use crate::theme::{self, Tokens};

/// How long the Back pill stays after a jump (the mockup's 6 s).
const PILL_SECS: f64 = 6.0;
/// The pill's bottom edge above the page area's (the mockup's 56 px less its 36 px footer);
/// Margin's toasts sit on the same line.
pub(crate) const PILL_LIFT: f32 = 20.0;
/// The pill's padding around its button, and the shortcut's text size.
const PILL_PAD: f32 = 4.0;
const SHORTCUT_TEXT: f32 = 11.0;

/// The exact place a history entry returns to: the reading spot, and the zoom and fit it was
/// read at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    spot: Spot,
    zoom: f32,
    fit: Fit,
    /// Fit "read"'s width, when it held the zoom.
    reading_width: Option<f32>,
}

/// A document's history state in Margin (`DocReading::history`).
#[derive(Default)]
pub(crate) struct History {
    /// Margin opened this document (`reading::opened`): its history keeps exact places and its
    /// page turns stay out of it. False in PdfCraft.
    pub(crate) exact: bool,
    /// The open search has kept the reader's place already.
    search_origin_kept: bool,
    /// When the Back pill appeared, while it shows.
    pill_since: Option<f64>,
    /// The back stack's length and newest entry last frame, to tell a jump from Back.
    seen_len: usize,
    seen_top: Option<ViewPlace>,
}

/// Whether `view` keeps exact places (Margin) rather than PdfCraft's pages.
pub(crate) fn exact(view: &DocView) -> bool {
    view.margin.history.exact
}

/// The exact place the reader is at, in Margin.
pub(crate) fn here(view: &DocView) -> Option<Place> {
    exact(view).then(|| Place { spot: reading::spot(view), zoom: view.zoom, fit: view.fit, reading_width: reading::read_width(view) })
}

/// Return to `place` (`view_history` has shown its page already).
pub(crate) fn restore(view: &mut DocView, place: Place) {
    view.fit = place.fit;
    view.zoom = place.zoom;
    reading::hold_read_width(view, place.reading_width);
    reading::go(view, Spot { page: view.current, ..place.spot });
}

/// A search is about to jump to a match: the first such jump of a search keeps the reader's
/// place, so Back leaves the search however many matches it stepped through.
pub(crate) fn search_jump(view: &mut DocView) {
    if !exact(view) || view.margin.history.search_origin_kept {
        return;
    }
    view.margin.history.search_origin_kept = true;
    let here = view.here();
    view.remember(here);
}

/// Every Margin frame, after the page is drawn: the mouse's back/forward buttons, the end of a
/// search, and the Back pill. Returns the pill's top edge while it shows, so a toast can sit
/// above it.
pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context) -> Option<f32> {
    let now = ctx.input(|i| i.time);
    for view in &mut app.views {
        if view.find.is_none() {
            view.margin.history.search_origin_kept = false;
        }
        let history = &mut view.margin.history;
        let (len, top) = (view.back.len(), view.back.last().copied());
        if top != history.seen_top {
            // A jump or Forward puts a new place on top; Back takes one off.
            history.pill_since = (len >= history.seen_len && top.is_some()).then_some(now);
        }
        (history.seen_len, history.seen_top) = (len, top);
    }
    let index = app.active?;
    let unobstructed = app.dialog.is_none() && app.close_request.is_none() && !app.palette_open;
    let (back_pressed, forward_pressed) =
        ctx.input(|i| (i.pointer.button_pressed(PointerButton::Extra1), i.pointer.button_pressed(PointerButton::Extra2)));
    if unobstructed && let Some(view) = app.views.get_mut(index) {
        if back_pressed {
            view.view_history(false);
        } else if forward_pressed {
            view.view_history(true);
        }
    }
    pill(app, ctx, index, now)
}

/// "‹ Back to p. N  Alt+←" above the footer, for [`PILL_SECS`] after a jump; it fades in and out.
/// Returns its top edge while it shows.
fn pill(app: &mut PdfCraftApp, ctx: &egui::Context, index: usize, now: f64) -> Option<f32> {
    let key = look::shortcut(app, "reader.back");
    let view = app.views.get_mut(index)?;
    let doc = app.session.get(view.id)?;
    let left = view.margin.history.pill_since.map_or(0.0, |since| PILL_SECS - (now - since));
    let showing = left > 0.0 && !view.back.is_empty();
    if !showing {
        view.margin.history.pill_since = None;
    }
    let id = egui::Id::new(("margin-back-pill", view.id));
    let shown = motion::toggle(ctx, id, showing, motion::FADE);
    let area = view.viewport_rect();
    let place = view.back.last()?;
    if shown <= 0.0 || !area.is_positive() {
        return None;
    }
    if showing {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
    }
    let label = doc
        .info
        .pages
        .get(place.page)
        .map(|p| p.label.as_str())
        .filter(|l| !l.is_empty())
        .map_or_else(|| (place.page + 1).to_string(), str::to_owned);
    let text = format!("Back to p. {label}");
    let t = Tokens::get(ctx);
    let anchor = pos2(area.center().x, area.bottom() - PILL_LIFT + (1.0 - shown) * motion::RISE);
    let mut back = false;
    let drawn = egui::Area::new(id).pivot(Align2::CENTER_BOTTOM).fixed_pos(anchor).interactable(showing).show(ctx, |ui| {
        ui.set_opacity(shown);
        look::floating_frame(&t, look::RADIUS_MENU).inner_margin(egui::Margin::symmetric(PILL_PAD as i8, PILL_PAD as i8)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP / 2.0;
                back = look::text_button(ui, "chevron-left", &text, BUTTON_HEIGHT, false, "Return to where you were before the jump").clicked();
                if let Some(key) = &key {
                    ui.label(egui::RichText::new(key).font(theme::regular(SHORTCUT_TEXT)).color(t.text_faint));
                    ui.add_space(GAP / 2.0);
                }
            });
        });
    });
    if back && let Some(view) = app.views.get_mut(index) {
        view.view_history(false);
    }
    Some(drawn.response.rect.top())
}
