//! Margin's messages (R8 QoL 23): a dark card above the footer, inside the reading area, in
//! place of PdfCraft's light toast at the window's foot. Routine messages still go through
//! PdfCraft's one slot (`app.notify`, `app.toast`) and its timing; Margin adds only what a plain
//! string cannot carry: Undo for a deleted mark, and a failure that stays until it is dismissed
//! or its action runs, so a routine message never replaces it.

use egui::{Align2, Rect, pos2};
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

/// A failure or question kept on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    text: String,
    action: Option<Action>,
    /// A question's second answer; a question has no dismiss ×, one of its answers must be
    /// picked.
    second: Option<Action>,
}

/// The routine message `text` offers Undo while `doc` is the active document.
#[derive(Clone, Debug, PartialEq, Eq)]
struct UndoOffer {
    text: String,
    doc: DocId,
}

/// What a held message's button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Write the document's highlights to its reading note again ("Write to note").
    WriteNote(DocId),
    /// The document's file changed on disk while it had unsaved edits: show the file's version.
    Reload(DocId),
    /// …or keep the edits; Save then writes them over the file's version.
    KeepMine(DocId),
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Self::WriteNote(_) => "Retry",
            Self::Reload(_) => "Reload",
            Self::KeepMine(_) => "Keep mine",
        }
    }

    fn run(self, app: &mut PdfCraftApp, ctx: &egui::Context) {
        match self {
            Self::WriteNote(doc) => {
                if let Some(view) = app.views.iter_mut().find(|view| view.id == doc) {
                    view.comments.notes.ask(ctx, Request::Write);
                }
            }
            Self::Reload(doc) => super::reload::resolve(app, doc, true),
            Self::KeepMine(doc) => super::reload::resolve(app, doc, false),
        }
    }
}

/// Show `text` until the reader dismisses it or runs `action`; routine messages don't replace it.
pub(crate) fn failed(app: &mut PdfCraftApp, text: impl Into<String>, action: Option<Action>) {
    app.margin_chrome.notices.held = Some(Held { text: text.into(), action, second: None });
}

/// Ask `text` with two answers, kept on screen until one is picked.
pub(crate) fn ask(app: &mut PdfCraftApp, text: impl Into<String>, first: Action, second: Action) {
    app.margin_chrome.notices.held = Some(Held { text: text.into(), action: Some(first), second: Some(second) });
}

/// Take back a question whose first answer is `first`, once it no longer applies.
pub(crate) fn withdraw(app: &mut PdfCraftApp, first: Action) {
    let notices = &mut app.margin_chrome.notices;
    if notices.held.as_ref().is_some_and(|held| held.action == Some(first) && held.second.is_some()) {
        notices.held = None;
    }
}

/// A routine message that offers Undo (a deletion `edit.undo` reverses) while `doc` is active.
pub(crate) fn undoable(app: &mut PdfCraftApp, text: &str, doc: DocId) {
    app.notify(text);
    app.margin_chrome.notices.undo = Some(UndoOffer { text: text.to_owned(), doc });
}

/// Every Margin frame, last: the routine message (with Undo when it offers it) over the held
/// failure, centred above the footer in the reading area, above PdfCraft's progress notice. The
/// Back pill (`pill`) has the area's bottom-left corner: the toast goes above it only in an area
/// too narrow for both side by side. Repaints only until the routine message expires.
pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context, pill: Option<Rect>) {
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
    let max_width = (area.width() - 2.0 * GAP_SECTION).min(MAX_WIDTH);
    if max_width <= 0.0 {
        return;
    }
    // Placed for its widest, so a longer message never lifts it over the pill.
    let widest = (area.center().x - max_width / 2.0)..=(area.center().x + max_width / 2.0);
    let mut bottom = area.bottom() - PILL_LIFT;
    let beside_pill = pill.filter(|pill| pill.right() + GAP >= *widest.start() && pill.left() <= *widest.end()).map(|pill| pill.top());
    for top in [beside_pill, crate::widgets::progress_notice_top(app, ctx)].into_iter().flatten() {
        bottom = bottom.min(top - GAP);
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
                clicked.undo = card(ui, &t, text, max_width, &[action], false).is_some();
            }
            if let Some(held) = &held {
                let labels = [held.action.map(Action::label), held.second.map(Action::label)];
                match card(ui, &t, &held.text, max_width, &labels, held.second.is_none()) {
                    Some(Pressed::Action(0)) => clicked.action = held.action,
                    Some(Pressed::Action(_)) => clicked.action = held.second,
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

/// A button pressed on a card: one of its actions (by position), or its dismiss ×.
enum Pressed {
    Action(usize),
    Dismiss,
}

/// One message: its text, wrapped within `max_width` less its buttons, then the `actions`
/// buttons and, when `dismissable`, a × button.
fn card(ui: &mut egui::Ui, t: &Tokens, text: &str, max_width: f32, actions: &[Option<&str>], dismissable: bool) -> Option<Pressed> {
    let pad = egui::Margin::symmetric(GAP_LARGE as i8, (GAP / 2.0) as i8);
    let close = if dismissable { BUTTON_HEIGHT + GAP } else { 0.0 };
    let buttons: f32 = actions.iter().flatten().map(|label| look::Button::secondary(look::ButtonSize::Compact, label).width(ui) + GAP).sum();
    let reserved = buttons + close + pad.sum().x;
    let wrap = (max_width - reserved).max(BUTTON_HEIGHT);
    let galley = ui.painter().layout(text.to_owned(), theme::regular(TEXT_SIZE), t.text, wrap);
    let mut pressed = None;
    look::floating_frame(t, look::RADIUS_MENU).inner_margin(pad).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.set_min_height(BUTTON_HEIGHT);
            ui.add(egui::Label::new(galley).selectable(false));
            for (index, label) in actions.iter().enumerate() {
                if let Some(label) = label
                    && look::button_secondary(ui, look::ButtonSize::Compact, label).clicked()
                {
                    pressed = Some(Pressed::Action(index));
                }
            }
            if dismissable && look::icon_button(ui, "x", BUTTON_HEIGHT, false, "Dismiss", None).clicked() {
                pressed = Some(Pressed::Dismiss);
            }
        });
    });
    pressed
}

/// The progress card's width (fixed, so a changing label or percent never resizes it), its bar's
/// height and the widest its percent gets.
const PROGRESS_WIDTH: f32 = 340.0;
const PROGRESS_BAR: f32 = 4.0;
const PERCENT_TEMPLATE: &str = "100%";
/// How long the bar takes to catch up with the job's progress, and how often it repaints while
/// it does (PdfCraft's notice's 30 frames a second).
const PROGRESS_EASE: f32 = 0.3;
const PROGRESS_REPAINT: std::time::Duration = std::time::Duration::from_millis(33);

/// PdfCraft's progress notice (`app.progress_notice`: an export, recognition) in Margin: a
/// floating card of a fixed width centred where the toasts sit, above the footer in the reading
/// area (the toasts go above it), with the label, the percent in a fixed slot, the bar and a
/// secondary Cancel. Returns whether Cancel was clicked and the card's rect.
pub(crate) fn progress(app: &PdfCraftApp, ctx: &egui::Context, notice: &crate::widgets::ProgressNotice) -> (bool, Rect) {
    let t = Tokens::get(ctx);
    let area = app
        .active
        .and_then(|i| app.views.get(i))
        .map(crate::canvas::DocView::viewport_rect)
        .filter(egui::Rect::is_positive)
        .unwrap_or_else(|| ctx.content_rect());
    let width = PROGRESS_WIDTH.min(area.width() - 2.0 * GAP_SECTION).max(0.0);
    let fraction = if notice.fraction.is_finite() { notice.fraction.clamp(0.0, 1.0) } else { 0.0 };
    let shown = ctx.animate_value_with_time(egui::Id::new("margin-progress-bar"), fraction, PROGRESS_EASE);
    let mut cancel = false;
    let drawn = egui::Area::new(egui::Id::new("margin-progress-notice"))
        .order(egui::Order::Foreground)
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(pos2(area.center().x, area.bottom() - PILL_LIFT))
        .show(ctx, |ui| {
            look::floating_frame(&t, look::RADIUS_MENU).inner_margin(GAP_LARGE).show(ui, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
                ui.horizontal(|ui| {
                    let font = theme::regular(TEXT_SIZE);
                    let label_width = (ui.available_width() - look::template_width(ui, PERCENT_TEMPLATE, font.clone()) - GAP).max(0.0);
                    ui.allocate_ui_with_layout(egui::vec2(label_width, BUTTON_HEIGHT), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_min_width(label_width);
                        ui.add(egui::Label::new(egui::RichText::new(&notice.label).font(font.clone()).color(t.text)).truncate());
                    });
                    let percent = format!("{:.0}%", shown * 100.0);
                    look::reserved_label(ui, &percent, PERCENT_TEMPLATE, font, t.text_muted, egui::Align::Max);
                });
                let (track, resp) = ui.allocate_exact_size(egui::vec2(width, PROGRESS_BAR), egui::Sense::hover());
                resp.widget_info(|| {
                    let mut info = egui::WidgetInfo::new(egui::WidgetType::ProgressIndicator);
                    info.value = Some(f64::from(fraction) * 100.0);
                    info
                });
                let radius = egui::CornerRadius::same((PROGRESS_BAR / 2.0) as u8);
                ui.painter().rect_filled(track, radius, look::Surfaces::of(&t).track);
                let done = Rect::from_min_size(track.min, egui::vec2(track.width() * shown, track.height()));
                ui.painter().rect_filled(done, radius, t.accent);
                if notice.cancellable {
                    ui.allocate_ui_with_layout(egui::vec2(width, BUTTON_HEIGHT), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        cancel = look::button_secondary(ui, look::ButtonSize::Compact, "Cancel").clicked();
                    });
                }
            });
        });
    ctx.request_repaint_after(PROGRESS_REPAINT);
    (cancel, drawn.response.rect)
}
