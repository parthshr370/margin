//! Margin's selection bar (R8 QoL 8): when a text drag ends or a word is picked, a small bar
//! beside the selected text offers Copy, a highlight in each `notes.colors` colour, Underline,
//! Comment, Copy callout and, while Margin's notes are on, Open in Obsidian.
//!
//! Copy and Copy callout work on the selection alone. The first colour or Underline writes one
//! real mark (PdfCraft's `AddAnnotation`, one undo step); while the bar stays open, later colours
//! restyle that same mark (`StyleAnnotation`), so a choice never duplicates it. The bar follows
//! the selection as the page scrolls, stays inside the reading area and hides while its text is
//! scrolled out of view, without closing.
//!
//! It is an [`egui::Area`], not an egui popup: `margin::ui` keeps a click that closes a popup
//! from reaching the page, and a click on the page has to close this bar and start a new
//! selection in the same action. A press outside the bar or Esc (`keys::close_layer`) closes it;
//! a dismissed selection doesn't bring it back.

use std::ops::Range;

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, vec2};
use pdfcraft_engine::{Edit, Markup, NewAnnotation, Shape};
use serde_json::Value;

use super::{SidebarTab, highlights, look, obsidian};
use crate::PdfCraftApp;
use crate::canvas::DocView;
use crate::comments::CommentTool;
use crate::theme::Tokens;

/// The bar's egui id; its layer tells a press on it from a press outside.
const BAR_ID: &str = "margin-selection-bar";
/// A button's side.
const BUTTON: f32 = 28.0;
/// The bar's padding around its buttons, and its distance from the selected text.
const PADDING: u8 = 4;
const OFFSET: f32 = 8.0;
/// The bar's height, for choosing above or below the text before it is laid out.
const BAR_HEIGHT: f32 = BUTTON + 2.0 * PADDING as f32;
/// The divider between groups of buttons: its height and the space either side.
const DIVIDER_HEIGHT: f32 = 16.0;
const DIVIDER_SPACE: f32 = 4.0;

/// Shown when the PDF refuses comment changes.
const NOT_ALLOWED: &str = "This PDF doesn't allow changes to its comments.";

/// The bar's state, per document (in `DocReading`).
#[derive(Debug, Default)]
pub(crate) struct SelectionBar {
    open: Option<Bar>,
    /// The selection the reader closed the bar on: it doesn't open again for it.
    dismissed: Option<(usize, Range<usize>)>,
}

/// An open bar: the selection it was opened for, and the mark it made.
#[derive(Debug)]
struct Bar {
    page: usize,
    range: Range<usize>,
    /// The selected text, as Copy puts it on the clipboard.
    text: String,
    /// The selected lines' boxes in page view space: the bar's anchor. Kept, because a mark
    /// makes the page re-read its text.
    lines: Vec<[f32; 4]>,
    /// The comment index (on `page`) of the mark this bar wrote, if it wrote one.
    mark: Option<usize>,
}

impl SelectionBar {
    /// Close the bar (Esc). `true` if it was open.
    pub(crate) fn dismiss(&mut self) -> bool {
        let Some(bar) = self.open.take() else { return false };
        self.dismissed = Some((bar.page, bar.range));
        true
    }
}

/// What a click on the bar asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    Copy,
    /// The `notes.colors` entry at this index.
    Color(usize),
    Underline,
    Comment,
    CopyCallout,
    OpenInObsidian,
}

/// Open, follow and close the bar of view `i`, then draw it inside `viewport` (the reading
/// area). Called after the page is drawn, so the selection and the page positions are this
/// frame's.
pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context, i: usize, viewport: Rect) {
    let Some(view) = app.views.get_mut(i) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    track(view, &doc.info, ctx);
    let Some(action) = draw(app, ctx, i, viewport) else { return };
    run(app, ctx, i, action);
}

/// Follow the page's selection: a finished new one opens the bar, a press outside it closes it,
/// and a bar with no mark closes with its selection.
fn track(view: &mut DocView, info: &pdfcraft_render::DocInfo, ctx: &egui::Context) {
    let selection = view.selection_range();
    let bar_layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new(BAR_ID));
    let pressed_outside = ctx
        .input(|input| input.pointer.any_pressed().then(|| input.pointer.press_origin()).flatten())
        .is_some_and(|origin| ctx.layer_id_at(origin) != Some(bar_layer));
    let state = &mut view.margin.selection;
    if pressed_outside && state.open.is_some() {
        state.dismissed = selection.clone();
        state.open = None;
    }
    // An undone mark is no longer this bar's.
    if let Some(bar) = state.open.as_mut()
        && let Some(index) = bar.mark
        && !info.annotations.iter().any(|a| a.page == bar.page && a.index == index && obsidian::is_markup(a))
    {
        bar.mark = None;
    }
    let dragging = ctx.input(|input| input.pointer.primary_down());
    match selection {
        Some(current) => {
            // A selection that has changed since the bar was closed may bring it back.
            if state.dismissed.as_ref().is_some_and(|closed| *closed != current) {
                state.dismissed = None;
            }
            let shown = state.open.as_ref().is_some_and(|bar| (bar.page, &bar.range) == (current.0, &current.1));
            if !shown && !dragging && state.dismissed.is_none() {
                let text = view.selected_text().unwrap_or_default();
                let (page, range) = current;
                let lines = view.page_text(page).map(|t| t.line_rects(range.clone())).unwrap_or_default();
                view.margin.selection = SelectionBar { open: Some(Bar { page, range, text, lines, mark: None }), dismissed: None };
            }
        }
        None => {
            state.dismissed = None;
            if state.open.as_ref().is_some_and(|bar| bar.mark.is_none()) {
                state.open = None;
            }
        }
    }
}

/// Where the bar goes: centred over the selected text, or under it when there is no room above,
/// or at the top of the reading area when the selection fills it. `None` while the text is out
/// of view or its page isn't drawn.
fn placement(view: &DocView, bar: &Bar, viewport: Rect) -> Option<(egui::Pos2, Align2)> {
    let xform = view.page_xform(bar.page)?;
    let anchor = bar.lines.iter().map(|r| xform.view_rect(*r)).fold(Rect::NOTHING, |acc, r| acc.union(r));
    if !anchor.is_positive() || !anchor.intersects(viewport) {
        return None;
    }
    let x = anchor.center().x;
    Some(if anchor.top() - OFFSET - BAR_HEIGHT >= viewport.top() {
        (egui::pos2(x, anchor.top() - OFFSET), Align2::CENTER_BOTTOM)
    } else if anchor.bottom() + OFFSET + BAR_HEIGHT <= viewport.bottom() {
        (egui::pos2(x, anchor.bottom() + OFFSET), Align2::CENTER_TOP)
    } else {
        (egui::pos2(x, viewport.top() + OFFSET), Align2::CENTER_TOP)
    })
}

/// The bar itself; returns the clicked action.
fn draw(app: &PdfCraftApp, ctx: &egui::Context, i: usize, viewport: Rect) -> Option<Action> {
    let view = app.views.get(i)?;
    let bar = view.margin.selection.open.as_ref()?;
    let (pos, pivot) = placement(view, bar, viewport)?;
    let doc = app.session.get(view.id)?;
    let mark = bar.mark.and_then(|index| doc.info.annotations.iter().find(|a| a.page == bar.page && a.index == index));
    let mark_color = mark.and_then(|a| a.color).and_then(|rgb| app.notes.color_named(rgb)).map(|c| c.name.as_str());
    let underlined = mark.is_some_and(|a| a.subtype == "Underline");
    let can_mark = doc.allows_annotation();
    let notes = &view.comments.notes;
    let t = Tokens::get(ctx);
    let mut clicked = None;
    egui::Area::new(egui::Id::new(BAR_ID))
        .order(egui::Order::Foreground)
        .pivot(pivot)
        .fixed_pos(pos)
        .constrain_to(viewport.shrink(OFFSET))
        .fade_in(false)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(t.card)
                .stroke(Stroke::new(1.0, t.border))
                .corner_radius(CornerRadius::same(t.radius + PADDING))
                .shadow(egui::Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(if t.dark() { 128 } else { 30 }) })
                .inner_margin(egui::Margin::same(PADDING as i8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if look::icon_button(ui, "copy", BUTTON, false, "Copy", Some("Ctrl+C")).clicked() {
                            clicked = Some(Action::Copy);
                        }
                        divider(ui, &t);
                        ui.add_enabled_ui(can_mark, |ui| {
                            let shortcuts: Vec<Option<String>> = app
                                .notes
                                .colors
                                .iter()
                                .map(|c| look::shortcut_with(app, highlights::COLOR_ID, &highlights::color_args(&c.name)))
                                .collect();
                            let dots: Vec<look::ColorDot<'_>> = app
                                .notes
                                .colors
                                .iter()
                                .zip(&shortcuts)
                                .map(|(c, keys)| look::ColorDot {
                                    name: &c.name,
                                    color: Color32::from_rgb(c.rgb[0], c.rgb[1], c.rgb[2]),
                                    selected: mark_color == Some(c.name.as_str()),
                                    shortcut: keys.as_deref(),
                                })
                                .collect();
                            if let Some(index) = look::color_dots(ui, &dots) {
                                clicked = Some(Action::Color(index));
                            }
                            // A highlight can't become an underline; Undo takes it back first.
                            let underline_free = mark.is_none() || underlined;
                            let (label, keys) = if underline_free {
                                ("Underline".to_string(), look::shortcut(app, "comment.underline"))
                            } else {
                                ("Underline: undo the highlight first (Ctrl+Z)".to_string(), None)
                            };
                            let underline = ui
                                .add_enabled_ui(underline_free, |ui| look::icon_button(ui, "underline", BUTTON, underlined, &label, keys.as_deref()))
                                .inner;
                            if underline.clicked() && !underlined {
                                clicked = Some(Action::Underline);
                            }
                            if look::icon_button(ui, "message-square-plus", BUTTON, false, "Comment", None).clicked() {
                                clicked = Some(Action::Comment);
                            }
                        });
                        if notes.enabled {
                            divider(ui, &t);
                            let tip = if notes.external { obsidian::EXTERNAL_TIP } else { "Copy callout" };
                            if look::icon_button(ui, "message-square-quote", BUTTON, false, tip, None).clicked() {
                                clicked = Some(Action::CopyCallout);
                            }
                            let open_reason = match (&notes.note, can_mark) {
                                (Err(why), _) => Some(why.as_str()),
                                (Ok(_), false) => Some(NOT_ALLOWED),
                                (Ok(_), true) => None,
                            };
                            let label = open_reason.map_or("Open in Obsidian: highlight and add to the reading note", |why| why);
                            let open = ui
                                .add_enabled_ui(open_reason.is_none(), |ui| look::icon_button(ui, "external-link", BUTTON, false, label, None))
                                .inner;
                            if open.clicked() {
                                clicked = Some(Action::OpenInObsidian);
                            }
                        }
                    });
                });
        });
    clicked
}

/// A short vertical rule between groups of buttons.
fn divider(ui: &mut egui::Ui, t: &Tokens) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0 + 2.0 * DIVIDER_SPACE, BUTTON), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        egui::Rangef::new(rect.center().y - DIVIDER_HEIGHT / 2.0, rect.center().y + DIVIDER_HEIGHT / 2.0),
        Stroke::new(1.0, t.divider),
    );
}

fn run(app: &mut PdfCraftApp, ctx: &egui::Context, i: usize, action: Action) {
    match action {
        Action::Copy => {
            let Some(text) = bar(app, i).map(|bar| bar.text.clone()) else { return };
            ctx.copy_text(text);
            app.notify("Copied");
            close(app, i);
        }
        Action::Color(index) => {
            let Some(color) = app.notes.colors.get(index).map(obsidian::NoteColor::rgb_f64) else { return };
            color_mark(app, i, color);
        }
        Action::Underline => {
            mark(app, i, Markup::Underline, None);
        }
        Action::Comment => {
            let Some(index) = ensure_mark(app, i) else { return };
            let Some(page) = bar(app, i).map(|bar| bar.page) else { return };
            let Some(view) = app.views.get_mut(i) else { return };
            let contents = app
                .session
                .get(view.id)
                .and_then(|doc| doc.info.annotations.iter().find(|a| a.page == page && a.index == index).and_then(|a| a.contents.clone()))
                .unwrap_or_default();
            view.comments.selected = Some((page, index));
            view.comments.reveal = true;
            view.comments.editing = Some((page, index, contents));
            close(app, i);
            super::open_tab(app, SidebarTab::Highlights);
        }
        Action::CopyCallout => {
            let Some((page, text, mark)) = bar(app, i).map(|bar| (bar.page, bar.text.clone(), bar.mark)) else { return };
            let color = mark.and_then(|index| {
                let doc = app.session.get(app.views.get(i)?.id)?;
                doc.info.annotations.iter().find(|a| a.page == page && a.index == index).and_then(|a| a.color)
            });
            match obsidian::selection_callout(app, i, page, &text, color) {
                Ok(callout) => {
                    ctx.copy_text(callout);
                    app.notify("Copied callout");
                    close(app, i);
                }
                Err(why) => app.notify(why),
            }
        }
        Action::OpenInObsidian => {
            let Some(index) = ensure_mark(app, i) else { return };
            let Some(page) = bar(app, i).map(|bar| bar.page) else { return };
            if let Some(view) = app.views.get_mut(i) {
                view.comments.notes.ask(ctx, obsidian::Request::Open(Some((page, index))));
            }
            close(app, i);
        }
    }
}

/// Runs `annotation.color` on the open bar of the active document: its selection is
/// highlighted, or its mark restyled, in the colour named in `args`. `false` without an open
/// bar, so the key goes on to the selected comment.
pub(crate) fn color_key(app: &mut PdfCraftApp, args: &Value) -> bool {
    let Some(i) = app.active else { return false };
    if bar(app, i).is_none() {
        return false;
    }
    let wanted = args.get("color").and_then(Value::as_str).unwrap_or_default();
    match app.notes.colors.iter().find(|c| c.name.eq_ignore_ascii_case(wanted)).map(obsidian::NoteColor::rgb_f64) {
        Some(color) => color_mark(app, i, color),
        None => app.notify(format!("{}: no colour named \"{wanted}\" in notes.colors", highlights::COLOR_ID)),
    }
    true
}

fn bar(app: &PdfCraftApp, i: usize) -> Option<&Bar> {
    app.views.get(i)?.margin.selection.open.as_ref()
}

fn close(app: &mut PdfCraftApp, i: usize) {
    if let Some(view) = app.views.get_mut(i) {
        view.margin.selection.dismiss();
    }
}

/// Highlight the selection in `color`, or restyle the bar's mark.
fn color_mark(app: &mut PdfCraftApp, i: usize, color: [f64; 3]) {
    let Some(bar) = bar(app, i) else { return };
    match bar.mark {
        Some(index) => {
            let page = bar.page;
            if app.session.get(app.views[i].id).is_some_and(|doc| !doc.allows_annotation()) {
                app.notify(NOT_ALLOWED);
                return;
            }
            app.apply_edit(Edit::StyleAnnotation { page, index, color: Some(color), opacity: None, width: None });
        }
        None => {
            mark(app, i, Markup::Highlight, Some(color));
        }
    }
}

/// The bar's mark, highlighting the selection in the tool's colour first when there is none.
fn ensure_mark(app: &mut PdfCraftApp, i: usize) -> Option<usize> {
    match bar(app, i)?.mark {
        Some(index) => Some(index),
        None => mark(app, i, Markup::Highlight, None),
    }
}

/// Mark the bar's selection as `kind`, in `color` or the tool's own: one `AddAnnotation` on the
/// undo stack. The page's selection gives way to the mark; the bar keeps it, so later colours
/// restyle it. Returns the mark's comment index.
fn mark(app: &mut PdfCraftApp, i: usize, kind: Markup, color: Option<[f64; 3]>) -> Option<usize> {
    let view = app.views.get(i)?;
    let doc = app.session.get(view.id)?;
    if !doc.allows_annotation() {
        app.notify(NOT_ALLOWED);
        return None;
    }
    let (page, quads) = view.selection_quads(&doc.info)?;
    let tool = if kind == Markup::Underline { CommentTool::Underline } else { CommentTool::Highlight };
    let mut style = app.comment_prefs.style(tool);
    if let Some(color) = color {
        style.color = color;
    }
    let author = app.comment_prefs.author.clone();
    if !app.apply_edit(Edit::AddAnnotation(NewAnnotation { page, shape: Shape::TextMarkup { kind, quads }, style, contents: String::new(), author }))
    {
        return None;
    }
    let view = app.views.get_mut(i)?;
    let index = view.comments.selected.filter(|(selected_page, _)| *selected_page == page).map(|(_, index)| index)?;
    // PdfCraft opens a new highlight's note for typing; from the bar only Comment does.
    if view.comments.editing.as_ref().is_some_and(|(p, n, typed)| (*p, *n) == (page, index) && typed.is_empty()) {
        view.comments.editing = None;
    }
    view.clear_selection();
    if let Some(bar) = view.margin.selection.open.as_mut() {
        bar.mark = Some(index);
    }
    Some(index)
}
