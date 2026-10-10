//! Margin's messages (R8 QoL 23): a dark card above the footer, inside the reading area, in
//! place of PdfCraft's light toast at the window's foot. Routine messages still go through
//! PdfCraft's one slot (`app.notify`, `app.toast`) and its timing; Margin adds only what a plain
//! string cannot carry: Undo for a deleted mark, and a failure that stays until it is dismissed
//! or its action runs, so a routine message never replaces it.

use egui::{Align2, pos2};
use pdfcraft_engine::DocId;

use super::history::PILL_LIFT;
use super::look::{self, BUTTON_HEIGHT, GAP, GAP_LARGE, GAP_SECTION};
use super::obsidian::Request;
use crate::PdfCraftApp;
use crate::theme::{self, Tokens};

/// How long a routine message stays: PdfCraft's toast timing.
const ROUTINE_SECS: f64 = 3.5;
/// How long a message offering Undo stays: long enough to reach the button (the Back pill's 6 s).
const UNDO_SECS: f64 = 6.0;
/// The widest a card grows before its text wraps.
const MAX_WIDTH: f32 = 480.0;
const TEXT_SIZE: f32 = 13.0;

/// What Margin's messages carry beyond PdfCraft's plain string (`ReaderChrome::notices`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notices {
    held: Option<Held>,
    undo: Option<UndoOffer>,
}

impl Notices {
    /// The failure on screen until it is dismissed or its action runs.
    pub fn held(&self) -> Option<&str> {
        self.held.as_ref().map(|held| held.text.as_str())
    }

    /// Whether the routine message showing now offers Undo.
    pub fn offers_undo(&self) -> bool {
        self.undo.is_some()
    }
}

/// A failure kept on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    text: String,
    action: Option<Action>,
}

/// The routine message `text` offers Undo while `doc` is the active document.
#[derive(Clone, Debug, PartialEq, Eq)]
struct UndoOffer {
    text: String,
    doc: DocId,
}

/// What a held failure's button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Write the document's highlights to its reading note again ("Write to note").
    WriteNote(DocId),
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Self::WriteNote(_) => "Retry",
        }
    }

    fn run(self, app: &mut PdfCraftApp, ctx: &egui::Context) {
        match self {
            Self::WriteNote(doc) => {
                if let Some(view) = app.views.iter_mut().find(|view| view.id == doc) {
                    view.comments.notes.ask(ctx, Request::Write);
                }
            }
        }
    }
}

/// Show `text` until the reader dismisses it or runs `action`; routine messages don't replace it.
pub(crate) fn failed(app: &mut PdfCraftApp, text: impl Into<String>, action: Option<Action>) {
    app.margin_chrome.notices.held = Some(Held { text: text.into(), action });
}

/// A routine message that offers Undo (a deletion `edit.undo` reverses) while `doc` is active.
pub(crate) fn undoable(app: &mut PdfCraftApp, text: &str, doc: DocId) {
    app.notify(text);
    app.margin_chrome.notices.undo = Some(UndoOffer { text: text.to_owned(), doc });
}

/// Every Margin frame, last: the routine message (with Undo when it offers it) over the held
/// failure, centred above the footer in the reading area, above the Back pill (`pill_top`) and
/// PdfCraft's progress notice. Repaints only until the routine message expires.
pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context, pill_top: Option<f32>) {
    let now = ctx.input(|i| i.time);
    let routine = app.toast.take().map(|(text, start)| (text, if start == 0.0 { now } else { start }));
    let active = app.active_ids().map(|(_, id)| id);
    let notices = &mut app.margin_chrome.notices;
    let undo = notices.undo.take().filter(|offer| routine.as_ref().is_some_and(|(text, _)| *text == offer.text) && active == Some(offer.doc));
    let life = if undo.is_some() { UNDO_SECS } else { ROUTINE_SECS };
    let routine = routine.filter(|(_, start)| now - start <= life);
    app.toast.clone_from(&routine);
    notices.undo = undo.filter(|_| routine.is_some());
    let held = notices.held.clone();
    if routine.is_none() && held.is_none() {
        return;
    }
    let area = app
        .active
        .and_then(|i| app.views.get(i))
        .map(crate::canvas::DocView::viewport_rect)
        .filter(egui::Rect::is_positive)
        .unwrap_or_else(|| ctx.content_rect());
    let mut bottom = area.bottom() - PILL_LIFT;
    for top in [pill_top, crate::widgets::progress_notice_top(app, ctx)].into_iter().flatten() {
        bottom = bottom.min(top - GAP);
    }
    let max_width = (area.width() - 2.0 * GAP_SECTION).min(MAX_WIDTH);
    if max_width <= 0.0 {
        return;
    }
    let t = Tokens::get(ctx);
    let offers_undo = app.margin_chrome.notices.undo.is_some();
    let mut clicked = Clicked::default();
    egui::Area::new(egui::Id::new("margin-toast"))
        .order(egui::Order::Foreground)
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(pos2(area.center().x, bottom))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = GAP;
            if let Some((text, _)) = &routine {
                let action = offers_undo.then_some("Undo");
                clicked.undo = card(ui, &t, text, max_width, action, false).is_some();
            }
            if let Some(held) = &held {
                match card(ui, &t, &held.text, max_width, held.action.map(Action::label), true) {
                    Some(Pressed::Action) => clicked.action = held.action,
                    Some(Pressed::Dismiss) => clicked.dismiss = true,
                    None => {}
                }
            }
        });
    if clicked.undo {
        app.toast = None;
        app.margin_chrome.notices.undo = None;
        app.execute("edit.undo");
    }
    if let Some(action) = clicked.action {
        app.margin_chrome.notices.held = None;
        action.run(app, ctx);
    } else if clicked.dismiss {
        app.margin_chrome.notices.held = None;
    }
    if let Some((_, start)) = &app.toast {
        let left = life - (now - start);
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(left.max(0.0)));
    }
}

/// What the reader pressed on the cards this frame.
#[derive(Default)]
struct Clicked {
    undo: bool,
    action: Option<Action>,
    dismiss: bool,
}

/// A button pressed on a card: its action, or its dismiss ×.
enum Pressed {
    Action,
    Dismiss,
}

/// One message: its text, wrapped within `max_width` less its buttons, then the `action` button
/// and, when `dismissable`, a × button.
fn card(ui: &mut egui::Ui, t: &Tokens, text: &str, max_width: f32, action: Option<&str>, dismissable: bool) -> Option<Pressed> {
    let pad = egui::Margin::symmetric(GAP_LARGE as i8, (GAP / 2.0) as i8);
    let close = if dismissable { BUTTON_HEIGHT + GAP } else { 0.0 };
    let reserved = action.map_or(0.0, |label| look::Button::secondary(look::ButtonSize::Compact, label).width(ui) + GAP) + close + pad.sum().x;
    let wrap = (max_width - reserved).max(BUTTON_HEIGHT);
    let galley = ui.painter().layout(text.to_owned(), theme::regular(TEXT_SIZE), t.text, wrap);
    let mut pressed = None;
    look::floating_frame(t, look::RADIUS_MENU).inner_margin(pad).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.set_min_height(BUTTON_HEIGHT);
            ui.add(egui::Label::new(galley).selectable(false));
            if let Some(label) = action
                && look::button_secondary(ui, look::ButtonSize::Compact, label).clicked()
            {
                pressed = Some(Pressed::Action);
            }
            if dismissable && look::icon_button(ui, "x", BUTTON_HEIGHT, false, "Dismiss", None).clicked() {
                pressed = Some(Pressed::Dismiss);
            }
        });
    });
    pressed
}
