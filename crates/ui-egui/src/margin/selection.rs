//! Margin's selection bar (R8 QoL 8): when a text drag ends or a word is picked, a small bar
//! beside the selected text offers Copy, a highlight in each `notes.colors` colour, Underline,
//! Comment, Copy callout and, while Margin's notes are on, Open in Obsidian.
//!
//! Copy and Copy callout work on the selection alone. The first colour or Underline writes one
//! real mark (PdfCraft's `AddAnnotation`, one undo step); while the bar stays open, later colours
//! restyle that same mark (`StyleAnnotation`), so a choice never duplicates it, and Underline
//! turns it from a highlight into an underline and back (`highlights::change_kind`). The bar
//! follows the selection as the page scrolls, stays inside the reading area and hides while its
//! text is scrolled out of view, without closing. On a PDF Margin doesn't write, the mark is a
//! highlight kept in the sidecar instead (`margin::marks`), restyled the same way.
//!
//! It is an [`egui::Area`], not an egui popup: `margin::ui` keeps a click that closes a popup
//! from reaching the page, and a click on the page has to close this bar and start a new
//! selection in the same action. A press outside the bar or Esc (`keys::close_layer`) closes it;
//! a dismissed selection doesn't bring it back.

use egui::{Align2, Color32, Rect, Sense, Stroke, vec2};
use margin_config::HighlightStyle;
use pdfcraft_engine::{Edit, Markup, NewAnnotation, Shape};
use serde_json::Value;

use super::highlights::NOT_ALLOWED;
use super::marks::{self, Pick};
use super::{SidebarTab, highlights, look, obsidian};
use crate::PdfCraftApp;
use crate::canvas::{DocView, TextSpan};
use crate::comments::CommentTool;
use crate::theme::Tokens;

/// The bar's egui id; its layer tells a press on it from a press outside.
const BAR_ID: &str = "margin-selection-bar";
/// A button's side, and the colour dots' cells beside them: one height for the whole bar.
const BUTTON: f32 = look::BUTTON_HEIGHT;
/// The bar's padding around its buttons, and its distance from the selected text.
const PADDING: u8 = 4;
const OFFSET: f32 = 8.0;
/// The bar's height, for choosing above or below the text before it is laid out.
const BAR_HEIGHT: f32 = BUTTON + 2.0 * PADDING as f32;
/// The divider between groups of buttons: its height and the space either side.
const DIVIDER_HEIGHT: f32 = 16.0;
const DIVIDER_SPACE: f32 = 4.0;

/// The bar's state, per document (in `DocReading`).
#[derive(Debug, Default)]
pub(crate) struct SelectionBar {
    open: Option<Bar>,
    /// The selection the reader closed the bar on: it doesn't open again for it.
    dismissed: Option<TextSpan>,
}

/// An open bar: the selection it was opened for, on one page or across pages, and the mark it
/// made.
#[derive(Debug)]
struct Bar {
    span: TextSpan,
    /// The page the selection ends on, which a mark across pages follows up.
    head: usize,
    /// The selected text, as Copy puts it on the clipboard.
    text: String,
    /// The selection on each of its pages, first to last.
    parts: Vec<Part>,
    /// The mark this bar wrote, if it wrote one.
    mark: Option<BarMark>,
}

/// A page's part of the bar's selection.
#[derive(Debug)]
struct Part {
    page: usize,
    /// The text selected on the page, for its callout.
    text: String,
    /// The selected lines' boxes in page view space: the bar's anchor. Kept, because a mark
    /// makes the page re-read its text.
    lines: Vec<[f32; 4]>,
}

/// The mark a bar wrote, one per page of its selection: PDF comments (page, index on the page)
/// or highlights kept in the sidecar (`margin::marks`). Never empty.
#[derive(Clone, Debug, PartialEq, Eq)]
enum BarMark {
    Pdf(Vec<(usize, usize)>),
    Sidecar(Vec<String>),
}

impl BarMark {
    /// The first page's mark: the one Comment and Open in Obsidian take.
    fn first(&self) -> Option<First<'_>> {
        match self {
            Self::Pdf(marks) => marks.first().map(|&(page, index)| First::Pdf(page, index)),
            Self::Sidecar(ids) => ids.first().map(|id| First::Sidecar(id)),
        }
    }
}

/// [`BarMark::first`].
enum First<'a> {
    Pdf(usize, usize),
    Sidecar(&'a str),
}

impl SelectionBar {
    /// Close the bar (Esc). `true` if it was open.
    pub(crate) fn dismiss(&mut self) -> bool {
        let Some(bar) = self.open.take() else { return false };
        self.dismissed = Some(bar.span);
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
    if !app.margin_keys.settings().reader.selection_popover {
        return;
    }
    let Some(view) = app.views.get_mut(i) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    track(view, &doc.info, ctx);
    let Some(action) = draw(app, ctx, i, viewport) else { return };
    run(app, ctx, i, action);
}

/// Follow the page's selection: a finished new one opens the bar, a press outside it closes it,
/// and a bar with no mark closes with its selection. A selection across pages opens it once
/// every page's text is read.
fn track(view: &mut DocView, info: &pdfcraft_render::DocInfo, ctx: &egui::Context) {
    let selection = view.selection_span();
    let bar_layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique(BAR_ID));
    let pressed_outside = ctx
        .input(|input| input.pointer.any_pressed().then(|| input.pointer.press_origin()).flatten())
        .is_some_and(|origin| ctx.layer_id_at(origin) != Some(bar_layer));
    let state = &mut view.margin.selection;
    if pressed_outside && state.open.is_some() {
        state.dismissed = selection.map(|(span, _)| span);
        state.open = None;
    }
    // An undone mark is no longer this bar's.
    let gone = match state.open.as_ref().and_then(|bar| bar.mark.as_ref()) {
        Some(BarMark::Pdf(made)) => {
            made.iter().any(|&(page, index)| !info.annotations.iter().any(|a| a.page == page && a.index == index && obsidian::is_markup(a)))
        }
        Some(BarMark::Sidecar(ids)) => ids.iter().any(|id| view.margin.marks.highlight(id).is_none()),
        None => false,
    };
    if gone && let Some(bar) = view.margin.selection.open.as_mut() {
        bar.mark = None;
    }
    let state = &mut view.margin.selection;
    let dragging = ctx.input(|input| input.pointer.primary_down());
    match selection {
        Some((span, head)) => {
            // A selection that has changed since the bar was closed may bring it back.
            if state.dismissed.is_some_and(|closed| closed != span) {
                state.dismissed = None;
            }
            let shown = state.open.as_ref().is_some_and(|bar| bar.span == span);
            if !shown
                && !dragging
                && state.dismissed.is_none()
                && let Some(text) = view.selected_text()
            {
                let parts = parts(view, span, &text);
                view.margin.selection = SelectionBar { open: Some(Bar { span, head, text, parts, mark: None }), dismissed: None };
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

/// The selection `span` on each of its pages with text selected. On one page its text is the
/// selection's (`text`, as Copy has it); across pages each page's own.
fn parts(view: &DocView, span: TextSpan, text: &str) -> Vec<Part> {
    let one_page = span.start.0 == span.end.0;
    (span.start.0..=span.end.0)
        .filter_map(|page| {
            let page_text = view.page_text(page)?;
            let range = span.on(page, page_text.glyphs.len())?;
            let lines = page_text.line_rects(range.clone());
            let text = if one_page { text.to_owned() } else { page_text.text_of(range) };
            (!lines.is_empty()).then_some(Part { page, text, lines })
        })
        .collect()
}

/// Where the bar goes: centred over the selected text on screen, or under it when there is no
/// room above, or at the top of the reading area when the selection fills it. `None` while the
/// text is out of view or its pages aren't drawn.
fn placement(view: &DocView, bar: &Bar, viewport: Rect) -> Option<(egui::Pos2, Align2)> {
    let anchor = bar
        .parts
        .iter()
        .filter_map(|part| Some((view.page_xform(part.page)?, &part.lines)))
        .flat_map(|(xform, lines)| lines.iter().map(move |r| xform.view_rect(*r)))
        .fold(Rect::NOTHING, |acc, r| acc.union(r));
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
    // Every page's mark has the same colour and kind: the first stands for them.
    let (mark_rgb, underlined) = match bar.mark.as_ref().and_then(BarMark::first) {
        Some(First::Pdf(page, index)) => {
            let mark = doc.info.annotations.iter().find(|a| a.page == page && a.index == index);
            (mark.and_then(|a| a.color), mark.is_some_and(|a| a.subtype == "Underline"))
        }
        Some(First::Sidecar(id)) => {
            let mark = view.margin.marks.highlight(id);
            (mark.map(|h| marks::shown_rgb(&app.notes, h)), mark.is_some_and(|h| h.style == HighlightStyle::Underline))
        }
        None => (None, false),
    };
    let mark_color = mark_rgb.and_then(|rgb| app.notes.color_named(rgb)).map(|c| c.name.as_str());
    let beside = view.margin.marks.beside().is_some();
    let can_mark = doc.allows_annotation() || beside;
    let notes = &view.comments.notes;
    let t = Tokens::get(ctx);
    let mut clicked = None;
    egui::Area::new(egui::Id::unique(BAR_ID))
        .order(egui::Order::Foreground)
        .pivot(pivot)
        .fixed_pos(pos)
        .constrain_to(viewport.shrink(OFFSET))
        .fade_in(false)
        .show(ctx, |ui| {
            look::floating_frame(&t, look::RADIUS_FLOATING).inner_margin(egui::Margin::same(PADDING as i8)).show(ui, |ui| {
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
                        if let Some(index) = look::color_dots_in(ui, &dots, BUTTON) {
                            clicked = Some(Action::Color(index));
                        }
                        // A toggle: it underlines the selection, turns the bar's highlight
                        // into an underline, and an underline back into a highlight.
                        let (label, keys) = if underlined {
                            ("Underline: on, click for a highlight", None)
                        } else {
                            ("Underline", look::shortcut(app, "comment.underline"))
                        };
                        if look::icon_button(ui, "underline", BUTTON, underlined, label, keys.as_deref()).clicked() {
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
                            (Ok(_), true) if beside => Some(highlights::BESIDE_OPEN_TIP),
                            (Ok(_), true) => None,
                        };
                        let label = open_reason.map_or("Open in Obsidian: highlight and add to the reading note", |why| why);
                        let open =
                            ui.add_enabled_ui(open_reason.is_none(), |ui| look::icon_button(ui, "external-link", BUTTON, false, label, None)).inner;
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
        Action::Underline => toggle_underline(app, i),
        Action::Comment => {
            // Across pages, the note goes on the first page's mark.
            let Some(mark) = ensure_mark(app, i) else { return };
            let Some(view) = app.views.get_mut(i) else { return };
            match mark.first() {
                Some(First::Pdf(page, index)) => {
                    let contents = app
                        .session
                        .get(view.id)
                        .and_then(|doc| doc.info.annotations.iter().find(|a| a.page == page && a.index == index).and_then(|a| a.contents.clone()))
                        .unwrap_or_default();
                    view.comments.selected = Some((page, index));
                    view.comments.reveal = true;
                    view.comments.editing = Some((page, index, contents));
                }
                Some(First::Sidecar(id)) => {
                    let note = view.margin.marks.highlight(id).and_then(|h| h.note.clone()).unwrap_or_default();
                    view.margin.marks.selected = Some(Pick::Highlight(id.to_owned()));
                    view.margin.marks.editing = Some((Pick::Highlight(id.to_owned()), note));
                }
                None => return,
            }
            close(app, i);
            super::open_tab(app, SidebarTab::Highlights);
        }
        Action::CopyCallout => match callouts(app, i) {
            Ok(callouts) => {
                ctx.copy_text(callouts);
                app.notify("Copied callout");
                close(app, i);
            }
            Err(why) => super::toast::failed(app, why, None),
        },
        Action::OpenInObsidian => {
            let Some(First::Pdf(page, index)) = ensure_mark(app, i).as_ref().and_then(BarMark::first) else { return };
            if let Some(view) = app.views.get_mut(i) {
                view.comments.notes.ask(ctx, obsidian::Request::Open(Some((page, index))));
            }
            close(app, i);
        }
    }
}

/// The bar's selection as callouts, one per page (each links to its own page), in the colour of
/// the bar's mark when it made one.
fn callouts(app: &PdfCraftApp, i: usize) -> Result<String, String> {
    let view = app.views.get(i).ok_or("No document is open.")?;
    let bar = view.margin.selection.open.as_ref().ok_or("Nothing is selected.")?;
    let color = match bar.mark.as_ref().and_then(BarMark::first) {
        Some(First::Pdf(page, index)) => {
            app.session.get(view.id).and_then(|doc| doc.info.annotations.iter().find(|a| a.page == page && a.index == index)).and_then(|a| a.color)
        }
        Some(First::Sidecar(id)) => view.margin.marks.highlight(id).map(|h| marks::shown_rgb(&app.notes, h)),
        None => None,
    };
    let callouts = bar.parts.iter().map(|part| obsidian::selection_callout(app, i, part.page, &part.text, color)).collect::<Result<Vec<_>, _>>()?;
    Ok(callouts.join("\n\n"))
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

/// Highlight the selection in `color`, or restyle the bar's marks (every page's, one undo step).
fn color_mark(app: &mut PdfCraftApp, i: usize, color: [f64; 3]) {
    let Some(bar) = bar(app, i) else { return };
    match bar.mark.clone() {
        Some(BarMark::Pdf(made)) => {
            if app.views.get(i).and_then(|view| app.session.get(view.id)).is_some_and(|doc| !doc.allows_annotation()) {
                app.notify(NOT_ALLOWED);
                return;
            }
            let edits = made
                .into_iter()
                .map(|(page, index)| Edit::StyleAnnotation { page, index, color: Some(color), opacity: None, width: None, endings: None })
                .collect();
            if let Some(edit) = crate::canvas::one_undo_step(edits) {
                app.apply_edit(edit);
            }
        }
        Some(BarMark::Sidecar(ids)) => marks::recolor(app, i, &ids, color),
        None => {
            mark(app, i, Markup::Highlight, Some(color));
        }
    }
}

/// The bar's Underline: underline the selection, or turn the bar's marks from highlights into
/// underlines or back (`highlights::change_kinds`, one undo step); the bar keeps the new marks.
fn toggle_underline(app: &mut PdfCraftApp, i: usize) {
    let Some(bar_mark) = bar(app, i).map(|bar| bar.mark.clone()) else { return };
    let made = match bar_mark {
        None => {
            mark(app, i, Markup::Underline, None);
            return;
        }
        Some(BarMark::Sidecar(ids)) => {
            let underlined =
                app.views.get(i).and_then(|view| view.margin.marks.highlight(ids.first()?)).is_some_and(|h| h.style == HighlightStyle::Underline);
            let style = if underlined { HighlightStyle::Highlight } else { HighlightStyle::Underline };
            marks::restyle(app, i, &ids, style);
            return;
        }
        Some(BarMark::Pdf(made)) => made,
    };
    let underlined = app.views.get(i).and_then(|view| app.session.get(view.id)).is_some_and(|doc| {
        made.first().is_some_and(|&(page, index)| doc.info.annotations.iter().any(|a| a.page == page && a.index == index && a.subtype == "Underline"))
    });
    let kind = if underlined { Markup::Highlight } else { Markup::Underline };
    let Some(new) = highlights::change_kinds(app, i, &made, kind) else { return };
    if let Some(bar) = app.views.get_mut(i).and_then(|view| view.margin.selection.open.as_mut()) {
        bar.mark = Some(BarMark::Pdf(new));
    }
}

/// The bar's mark, highlighting the selection in the tool's colour first when there is none.
fn ensure_mark(app: &mut PdfCraftApp, i: usize) -> Option<BarMark> {
    match bar(app, i)?.mark.clone() {
        Some(mark) => Some(mark),
        None => mark(app, i, Markup::Highlight, None),
    }
}

/// Mark the bar's selection as `kind`, in `color` or the tool's own: one mark per page of the
/// selection, as one undo step (an `AddAnnotation`, or a batch of them across pages), or sidecar
/// highlights when the PDF keeps its marks beside it (`margin::marks`). The page's selection
/// gives way to the marks; the bar keeps them, so later colours restyle them. Returns them.
fn mark(app: &mut PdfCraftApp, i: usize, kind: Markup, color: Option<[f64; 3]>) -> Option<BarMark> {
    let view = app.views.get(i)?;
    let doc = app.session.get(view.id)?;
    let tool = if kind == Markup::Underline { CommentTool::Underline } else { CommentTool::Highlight };
    let mut style = app.comment_prefs.style(tool);
    if let Some(color) = color {
        style.color = color;
    }
    let made = if view.margin.marks.beside().is_some() {
        let bar = view.margin.selection.open.as_ref()?;
        let new = bar
            .parts
            .iter()
            .map(|part| marks::NewMark {
                page: part.page,
                quads: part.lines.iter().map(|line| marks::rect_quad(*line)).collect(),
                text: Some(part.text.clone()),
                kind,
                rgb: style.color,
                note: String::new(),
            })
            .collect();
        BarMark::Sidecar(marks::add_all(app, i, new)?)
    } else {
        if !doc.allows_annotation() {
            app.notify(NOT_ALLOWED);
            return None;
        }
        let head = view.margin.selection.open.as_ref()?.head;
        let author = app.comment_prefs.author.clone();
        let per_page = view.selection_quads(&doc.info);
        let pages: Vec<usize> = per_page.iter().map(|(page, _)| *page).collect();
        let edits = per_page
            .into_iter()
            .map(|(page, quads)| {
                let shape = Shape::TextMarkup { kind, quads };
                Edit::AddAnnotation(NewAnnotation { page, shape, style: style.clone(), contents: String::new(), author: author.clone() })
            })
            .collect();
        let edit = app.views.get_mut(i)?.selection_marks_edit(edits, head)?;
        if !app.apply_edit(edit) {
            return None;
        }
        // Each page's mark was added last, so it is that page's last comment.
        let doc = app.session.get(app.views.get(i)?.id)?;
        let last_on = |page: usize| doc.info.annotations.iter().filter(|a| a.page == page && a.in_reply_to.is_none()).map(|a| a.index).max();
        BarMark::Pdf(pages.into_iter().map(|page| Some((page, last_on(page)?))).collect::<Option<_>>()?)
    };
    let view = app.views.get_mut(i)?;
    view.clear_selection();
    if let Some(bar) = view.margin.selection.open.as_mut() {
        bar.mark = Some(made.clone());
    }
    Some(made)
}
