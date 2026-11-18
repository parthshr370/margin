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

/// Every Margin frame, last: the routine message (with Undo when it offers it, or Cancel while
/// a batch recognition runs) over the held failure, centred above the footer in the reading
/// area, above PdfCraft's progress notice. Form script messages waiting their turn
/// (`show_script_messages`) take the routine slot one after another, with their icon and title,
/// as PdfCraft's toast shows them. The Back pill (`pill`) has the area's bottom-left corner: the
/// toast goes above it only in an area too narrow for both side by side. Repaints only until the
/// routine message expires.
pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context, pill: Option<Rect>) {
    let now = ctx.input(|i| i.time);
    if app.toast.is_none() {
        app.toast_script = None;
        // A left-out count whose message was cleared before its time is said again.
        app.script_messages_dropped = app.script_messages_dropped.saturating_add(std::mem::take(&mut app.toast_dropped));
        app.next_script_message();
    }
    let mut routine = app.toast.take().map(|(text, start)| (text, if start == 0.0 { now } else { start }));
    let active = app.active_ids().map(|(_, id)| id);
    let mut undo = app
        .margin_chrome
        .notices
        .undo
        .take()
        .filter(|offer| routine.as_ref().is_some_and(|(text, _)| *text == offer.text) && active == Some(offer.doc));
    let mut life = if undo.is_some() { UNDO_SECS } else { ROUTINE_SECS };
    if routine.as_ref().is_some_and(|(_, start)| now - start > life) {
        // Shown its time: a left-out count it said is done, and the next waiting script message
        // takes its place.
        app.toast_script = None;
        app.toast_dropped = 0;
        undo = None;
        life = ROUTINE_SECS;
        routine = if app.next_script_message() { app.toast.take().map(|(text, _)| (text, now)) } else { None };
    }
    app.toast.clone_from(&routine);
    let notices = &mut app.margin_chrome.notices;
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
    // A running batch keeps its message up (`poll_ocr` says each file again), so it carries the
    // one control a long job needs: Cancel. The file in flight finishes; the rest are not run.
    let batch_running = app.ocr_batch.is_some();
    let script = app.toast_script.as_ref().map(|message| (message.title.clone(), crate::js_ui::message_icon(message.icon)));
    let mut clicked = Clicked::default();
    egui::Area::new(egui::Id::unique("margin-toast"))
        .order(egui::Order::Foreground)
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(pos2(area.center().x, bottom))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = GAP;
            if let Some((text, _)) = &routine {
                let actions = [offers_undo.then_some("Undo"), batch_running.then_some("Cancel")];
                let message = Card {
                    text,
                    title: script.as_ref().and_then(|(title, _)| title.as_deref()),
                    icon: script.as_ref().map(|(_, icon)| *icon),
                    actions: &actions,
                    dismissable: false,
                };
                match card(ui, &t, max_width, &message) {
                    Some(Pressed::Action(0)) => clicked.undo = true,
                    Some(Pressed::Action(_)) => clicked.cancel_batch = true,
                    _ => {}
                }
            }
            if let Some(held) = &held {
                let actions = [held.action.map(Action::label), held.second.map(Action::label)];
                let message = Card { text: &held.text, title: None, icon: None, actions: &actions, dismissable: held.second.is_none() };
                match card(ui, &t, max_width, &message) {
                    Some(Pressed::Action(0)) => clicked.action = held.action,
                    Some(Pressed::Action(_)) => clicked.action = held.second,
                    Some(Pressed::Dismiss) => clicked.dismiss = true,
                    None => {}
                }
            }
        });
    if clicked.cancel_batch {
        app.cancel_ocr_batch();
    }
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
    cancel_batch: bool,
    action: Option<Action>,
    dismiss: bool,
}

/// A button pressed on a card: one of its actions (by position), or its dismiss ×.
enum Pressed {
    Action(usize),
    Dismiss,
}

/// One message card's content.
struct Card<'a> {
    text: &'a str,
    /// A form script message's title, over its text.
    title: Option<&'a str>,
    /// A form script message's icon, before its text.
    icon: Option<&'static str>,
    /// Its buttons' labels, by position; `None` leaves a position out.
    actions: &'a [Option<&'a str>],
    /// Whether it has a × button.
    dismissable: bool,
}

/// The size of a form script message's icon.
const SCRIPT_ICON: f32 = 16.0;

/// One message: its icon and title when it has them, its text wrapped within `max_width` less
/// its buttons, then its action buttons and, when dismissable, a × button.
fn card(ui: &mut egui::Ui, t: &Tokens, max_width: f32, message: &Card<'_>) -> Option<Pressed> {
    let pad = egui::Margin::symmetric(GAP_LARGE as i8, (GAP / 2.0) as i8);
    let close = if message.dismissable { BUTTON_HEIGHT + GAP } else { 0.0 };
    let icon = if message.icon.is_some() { SCRIPT_ICON + GAP } else { 0.0 };
    let buttons: f32 = message.actions.iter().flatten().map(|label| look::Button::secondary(look::ButtonSize::Compact, label).width(ui) + GAP).sum();
    let reserved = buttons + close + icon + pad.sum().x;
    let wrap = (max_width - reserved).max(BUTTON_HEIGHT);
    let galley = ui.painter().layout(message.text.to_owned(), theme::regular(TEXT_SIZE), t.text, wrap);
    let title = message.title.map(|title| ui.painter().layout(title.to_owned(), theme::semibold(TEXT_SIZE), t.text, wrap));
    let mut pressed = None;
    look::floating_frame(t, look::RADIUS_MENU).inner_margin(pad).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.set_min_height(BUTTON_HEIGHT);
            if let Some(icon) = message.icon {
                ui.add(crate::icons::image(icon, SCRIPT_ICON, t.text));
            }
            match title {
                Some(title) => {
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(title).selectable(false));
                        ui.add(egui::Label::new(galley).selectable(false));
                    });
                }
                None => {
                    ui.add(egui::Label::new(galley).selectable(false));
                }
            }
            for (index, label) in message.actions.iter().enumerate() {
                if let Some(label) = label
                    && look::button_secondary(ui, look::ButtonSize::Compact, label).clicked()
                {
                    pressed = Some(Pressed::Action(index));
                }
            }
            if message.dismissable && look::icon_button(ui, "x", BUTTON_HEIGHT, false, "Dismiss", None).clicked() {
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
    let shown = ctx.animate_value_with_time(egui::Id::unique("margin-progress-bar"), fraction, PROGRESS_EASE);
    let mut cancel = false;
    let drawn = egui::Area::new(egui::Id::unique("margin-progress-notice"))
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
                    let mut info = egui::WidgetInfo::new(egui::Role::ProgressIndicator);
                    info.value = Some(f64::from(fraction) * 100.0);
                    info
                });
                let radius = egui::CornerRadius::same((PROGRESS_BAR / 2.0) as u8);
                ui.painter().rect_filled(track, radius, t.surfaces.track);
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;
    use pdfcraft_engine::js::Message;

    use super::*;
    use crate::Flavor;

    const STEP_SECS: f32 = 0.1;
    /// Longer than a routine message stays.
    const PAST_A_MESSAGE_SECS: f64 = ROUTINE_SECS + 0.5;

    /// Margin's window reading a blank page, with its settings in a temporary folder.
    fn window(name: &str) -> (Harness<'static, PdfCraftApp>, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("margin-toast-{name}-{}", std::process::id()));
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.margin_keys.set_config_dir(root.join("config"));
        app.margin_keys.set_data_dir(root.join("data"));
        let blank = pdfcraft_engine::Session::new().create_blank(300.0, 400.0, 1).unwrap();
        app.open_bytes("blank.pdf", None, (*blank).clone()).unwrap();
        let window = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).with_step_dt(STEP_SECS).build_eframe(move |_cc| app);
        (window, root)
    }

    fn remove(root: std::path::PathBuf) {
        if root.exists() {
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    fn shown(window: &Harness<'static, PdfCraftApp>) -> Option<String> {
        window.state().toast.as_ref().map(|(text, _)| text.clone())
    }

    /// M4: form script messages from one run show one after another, as PdfCraft's toast shows
    /// them, instead of the first alone with the rest left waiting.
    #[test]
    fn form_script_messages_show_one_after_another() {
        let (mut window, root) = window("script-queue");
        window.run_steps(2);
        window.state_mut().show_script_messages(vec![Message::plain("first alert"), Message::plain("second alert")]);
        window.run_steps(2);
        assert_eq!(shown(&window).as_deref(), Some("first alert"));
        window.get_by_label("first alert");
        window.run_steps((PAST_A_MESSAGE_SECS / f64::from(STEP_SECS)).ceil() as usize);
        assert_eq!(shown(&window).as_deref(), Some("second alert"), "the next message takes the toast");
        window.get_by_label("second alert");
        drop(window);
        remove(root);
    }

    /// M4: a batch recognition's message carries Cancel, the one control a long job needs.
    #[test]
    fn a_batch_recognition_can_be_cancelled_from_its_message() {
        let (mut window, root) = window("batch-cancel");
        window.run_steps(2);
        let batch = Arc::new(Mutex::new(crate::ocr_ui::BatchProgress { done: 0, total: 3, message: None, cancel: false }));
        window.state_mut().ocr_batch = Some(batch.clone());
        window.run_steps(2);
        window.get_by_label("Cancel").click();
        window.run_steps(2);
        assert!(batch.lock().unwrap().cancel, "the files not yet read are not run");
        drop(window);
        remove(root);
    }
}
