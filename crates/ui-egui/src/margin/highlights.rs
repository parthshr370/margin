//! Margin's Highlights tab: the document's highlights, underlines, strikethroughs and squiggles
//! as cards in reading order (the mockup's `.hl-item`), under the reading-note box
//! ([`obsidian::panel_block`]).
//!
//! A card shows the comment's colour swatch, its type icon and page, the page text it marks
//! (extracted by the renderer's text layer, the text a callout quotes) and its note. Clicking a
//! card selects the comment and shows it on the page; double-clicking opens its note for typing,
//! as PdfCraft's Comments panel does. Hovering shows its actions (Copy callout, Open in Obsidian,
//! Change colour across `notes.colors`, Delete); right-clicking lists the same, and turns a
//! highlight into an underline or back ([`change_kind`]). The keymap ids
//! [`DELETE_ID`] and [`COLOR_ID`] act on the selected comment, picked here or on the page. Every
//! change is an engine [`Edit`] applied through `PdfCraftApp::apply_edit`, so it lands on the
//! document's undo stack and Undo brings it back.
//!
//! Cards are on the spacing scale: padded by the large step, rows the small step apart, the
//! note editor and its Save (accent) and Cancel (text) buttons inside the card.

use std::collections::HashMap;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align, Color32, Layout, Sense, Stroke, UiBuilder, vec2};
use pdfcraft_engine::{Edit, Markup, NewAnnotation, Shape};
use pdfcraft_render::{Annotation, DocInfo, PageText};
use serde_json::Value;

use super::look::{self, BUTTON_HEIGHT, ColorDot, GAP, GAP_LARGE};
use super::motion;
use super::obsidian::{self, Notes, Request};
use crate::canvas::DocView;
use crate::comments::{CommentTool, color32};
use crate::comments_panel::{kind_label, subtype_icon};
use crate::panels::Nav;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

/// Delete the selected comment (`Delete` / `Backspace` in the `Annotation` context).
pub const DELETE_ID: &str = "annotation.delete";
/// Give the selected comment a `notes.colors` colour, by name: `{ "color": "red" }`.
pub const COLOR_ID: &str = "annotation.color";
/// Said when the PDF refuses comment changes.
pub(crate) const NOT_ALLOWED: &str = "This PDF doesn't allow changes to its comments.";

/// Space between neighbouring cards: their hover fills nearly touch, so the list reads as one.
const CARD_SPACING: f32 = 2.0;
/// The top row of a card: swatch, type icon, page and date, then the actions.
const META_ROW: f32 = 24.0;
const SWATCH: f32 = 10.0;
/// How dark the swatch's edge is, so light colours show on the hover fill.
const SWATCH_EDGE_ALPHA: u8 = 40;
const KIND_ICON: f32 = 14.0;
const META_TEXT: f32 = 11.5;
const QUOTE_TEXT: f32 = 12.5;
const NOTE_TEXT: f32 = 12.0;
/// An unselected card shows this many lines of its quote; the selected one shows all of it.
const QUOTE_ROWS: usize = 3;
/// The note editor's text padding and height in lines.
const EDITOR_MARGIN: egui::Margin = egui::Margin::symmetric(8, 6);
const EDITOR_ROWS: usize = 2;
/// The hover actions: four buttons this size, this far apart.
const ACTION: f32 = 24.0;
const ACTION_GAP: f32 = 2.0;
const ACTION_COUNT: f32 = 4.0;
/// The right-click menu's width.
const MENU_WIDTH: f32 = 200.0;

/// The page text each text-markup comment of one document marks, extracted as the pages' text
/// layers arrive. Lives in the document's `DocReading`.
#[derive(Default)]
pub struct Quotes {
    /// The document's edit generation the quotes were read at: an edit can renumber comments.
    generation: u64,
    text: HashMap<(usize, usize), String>,
    /// Pages with a highlight whose text layer isn't loaded; the canvas asks the renderer for
    /// them ([`want_text`]).
    wanted: Vec<usize>,
}

impl Quotes {
    /// Read the quotes the loaded text layers allow and note the pages still missing one.
    fn refresh(&mut self, generation: u64, info: &DocInfo, texts: &HashMap<usize, Arc<PageText>>) {
        if generation != self.generation {
            self.text.clear();
            self.generation = generation;
        }
        self.wanted.clear();
        for a in info.annotations.iter().filter(|a| obsidian::is_markup(a)) {
            let key = (a.page, a.index);
            if self.text.contains_key(&key) {
                continue;
            }
            match (texts.get(&a.page), info.pages.get(a.page)) {
                (Some(text), Some(page)) => {
                    self.text.insert(key, obsidian::marked_text(text, page, a));
                }
                _ if !self.wanted.contains(&a.page) => self.wanted.push(a.page),
                _ => {}
            }
        }
    }
}

/// Add the pages whose highlights still need their text to the canvas's text-layer requests
/// (nothing in PdfCraft, which never shows this list).
pub(crate) fn want_text(view: &DocView, pages: &mut Vec<usize>) {
    for &page in &view.margin.quotes.wanted {
        if !view.texts.contains_key(&page) && !view.text_failed.contains(&page) && !pages.contains(&page) {
            pages.push(page);
        }
    }
}

/// What the cards asked for this frame.
#[derive(Default)]
struct Outcome {
    edit: Option<Edit>,
    /// Turn the comment at (page, index) into this kind ([`change_kind`]).
    kind_change: Option<(usize, usize, Markup)>,
    nav: Option<Nav>,
}

/// What a card's hover buttons and menu do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    CopyCallout,
    OpenInObsidian,
    /// The `notes.colors` entry at this index.
    Color(usize),
    /// Make a highlight an underline, or an underline a highlight.
    ChangeKind(Markup),
    EditNote,
    Delete,
}

/// The keys that run the cards' actions, shown in their tooltips.
struct Keys {
    delete: Option<String>,
    /// One per `notes.colors` entry.
    colors: Vec<Option<String>>,
}

impl Keys {
    fn of(app: &PdfCraftApp) -> Self {
        let colors = app.notes.colors.iter().map(|c| look::shortcut_with(app, COLOR_ID, &color_args(&c.name))).collect();
        Self { delete: look::shortcut(app, DELETE_ID), colors }
    }
}

/// The arguments of [`COLOR_ID`] for the colour named `name`.
pub(crate) fn color_args(name: &str) -> Value {
    serde_json::json!({ "color": name.to_lowercase() })
}

/// What a card shows besides the comment and the view's selection.
struct CardInput<'a> {
    t: &'a Tokens,
    a: &'a Annotation,
    /// The document's comments, `a`'s replies among them.
    info: &'a DocInfo,
    quote: &'a str,
    page_label: &'a str,
    notes: &'a Notes,
    keys: &'a Keys,
    /// The document takes comment changes.
    allowed: bool,
    /// Copy callout and Open in Obsidian are offered (Margin's notes are on).
    notes_on: bool,
    /// The PDF isn't in the vault, so callouts link to its file.
    external: bool,
    /// Why there is no reading note to open (no vault, a bad template), if there is none.
    no_note: Option<&'a str>,
    /// This year (the session's clock): its dates leave the year out.
    this_year: i64,
}

impl CardInput<'_> {
    /// The comment's colour can change and it can be deleted.
    fn editable(&self) -> bool {
        self.allowed && !self.a.locked
    }
}

/// The notes box and the cards of view `index` (which has highlights). Edits go to the undo
/// stack; returns where to show the clicked comment.
pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize) -> Option<Nav> {
    let keys = Keys::of(app);
    let this_year = app.session.today().0;
    let view = app.views.get_mut(index)?;
    let doc = app.session.get(view.id)?;
    let info = &doc.info;
    view.margin.quotes.refresh(doc.edit_generation(), info, &view.texts);
    let mut out = Outcome::default();
    let no_note = view.comments.notes.note.clone().err();
    let markup_indices = view.comments.notes.markup_indices.clone();
    egui::ScrollArea::vertical().id_salt(("margin-highlights", view.id.0)).auto_shrink([false, false]).show(ui, |ui| {
        obsidian::panel_block(ui, t, &mut view.comments.notes);
        ui.spacing_mut().item_spacing.y = CARD_SPACING;
        for a in obsidian::cached_markup(info, &markup_indices) {
            let quote = view.margin.quotes.text.get(&(a.page, a.index)).cloned().unwrap_or_default();
            let page_label = info.pages.get(a.page).map_or_else(|| a.page.saturating_add(1).to_string(), |p| p.label.clone());
            let input = CardInput {
                t,
                a,
                info,
                quote: &quote,
                page_label: &page_label,
                notes: &app.notes,
                keys: &keys,
                allowed: doc.allows_annotation(),
                notes_on: view.comments.notes.enabled,
                external: view.comments.notes.external,
                no_note: no_note.as_deref(),
                this_year,
            };
            card(ui, &input, view, &mut out);
        }
    });
    if let Some(edit) = out.edit {
        apply(app, edit);
    }
    if let Some((page, at, kind)) = out.kind_change {
        change_kind(app, index, page, at, kind);
    }
    out.nav
}

/// Runs [`DELETE_ID`] and [`COLOR_ID`] on the active document's selected comment. `false` for
/// any other id. Without a selected comment the key is spent, as PdfCraft's view keys are.
pub(crate) fn run(app: &mut PdfCraftApp, id: &str, args: &Value) -> bool {
    if id != DELETE_ID && id != COLOR_ID {
        return false;
    }
    // A colour key while the selection bar is open marks or restyles the bar's text.
    if id == COLOR_ID && super::selection::color_key(app, args) {
        return true;
    }
    let Some((index, doc_id)) = app.active_ids() else { return true };
    let Some((page, annot)) = app.views.get(index).and_then(|v| v.comments.selected) else { return true };
    let Some(doc) = app.session.get(doc_id) else { return true };
    if !doc.info.annotations.iter().any(|a| a.page == page && a.index == annot && a.in_reply_to.is_none()) {
        return true;
    }
    if !doc.allows_annotation() {
        app.notify(NOT_ALLOWED);
        return true;
    }
    let edit = if id == DELETE_ID {
        Edit::DeleteAnnotation { page, index: annot }
    } else {
        let wanted = args.get("color").and_then(Value::as_str).unwrap_or_default();
        let Some(color) = app.notes.colors.iter().find(|c| c.name.eq_ignore_ascii_case(wanted)) else {
            app.notify(format!("{COLOR_ID}: no colour named \"{wanted}\" in notes.colors"));
            return true;
        };
        Edit::StyleAnnotation { page, index: annot, color: Some(color.rgb_f64()), opacity: None, width: None }
    };
    apply(app, edit);
    true
}

/// Applies a card's or key's edit; a deletion says so with Undo (`edit.undo`).
fn apply(app: &mut PdfCraftApp, edit: Edit) {
    let deleting = matches!(edit, Edit::DeleteAnnotation { .. });
    app.apply_edit(edit);
    if deleting && let Some((_, doc)) = app.active_ids() {
        super::toast::undoable(app, "Highlight deleted", doc);
    }
}

/// The subtype and comment tool of the text marks a reader can switch between.
fn kind_parts(kind: Markup) -> Option<(&'static str, CommentTool)> {
    match kind {
        Markup::Highlight => Some(("Highlight", CommentTool::Highlight)),
        Markup::Underline => Some(("Underline", CommentTool::Underline)),
        Markup::StrikeOut | Markup::Squiggly => None,
    }
}

/// The other kind a highlight or an underline can become; `None` for any other comment.
fn other_kind(a: &Annotation) -> Option<Markup> {
    match a.subtype.as_str() {
        "Highlight" => Some(Markup::Underline),
        "Underline" => Some(Markup::Highlight),
        _ => None,
    }
}

/// Why text mark `a` can't change kind, if it can't: its replies would go with it, or it is
/// locked.
fn kind_change_refused(info: &DocInfo, a: &Annotation) -> Option<&'static str> {
    let has_replies = a.name.as_deref().is_some_and(|name| info.annotations.iter().any(|r| r.in_reply_to.as_deref() == Some(name)));
    if has_replies {
        Some("it has replies, which would be lost with it")
    } else if a.locked {
        Some("it is locked")
    } else {
        None
    }
}

/// Turn text mark `(page, index)` of view `i` into a `kind` mark, a highlight into an underline
/// or back: the same text, colour, note and author, as one undo step (the mark deleted and added
/// again in one `Batch`). The new mark is selected; returns its index (the same index when it is
/// that kind already). Refused, with a message, when the PDF takes no comment changes or the
/// mark has replies or is locked.
pub(crate) fn change_kind(app: &mut PdfCraftApp, i: usize, page: usize, index: usize, kind: Markup) -> Option<usize> {
    let (subtype, tool) = kind_parts(kind)?;
    let doc = app.session.get(app.views.get(i)?.id)?;
    let a = doc.info.annotations.iter().find(|a| a.page == page && a.index == index && obsidian::is_markup(a))?;
    if a.subtype == subtype {
        return Some(index);
    }
    other_kind(a)?;
    if !doc.allows_annotation() {
        app.notify(NOT_ALLOWED);
        return None;
    }
    if let Some(why) = kind_change_refused(&doc.info, a) {
        let message = format!("Can't change this {}: {why}.", kind_label(a).to_lowercase());
        app.notify(message);
        return None;
    }
    let mut style = app.comment_prefs.style(tool);
    if let Some(color) = a.color {
        style.color = color.map(f64::from);
    }
    let mark = NewAnnotation {
        page,
        shape: Shape::TextMarkup { kind, quads: a.quads.iter().map(|quad| quad.map(f64::from)).collect() },
        style,
        contents: a.contents.clone().unwrap_or_default(),
        author: a.author.clone().unwrap_or_else(|| app.comment_prefs.author.clone()),
    };
    let edit = Edit::Batch {
        label: format!("Change to {}", subtype.to_lowercase()),
        edits: vec![Edit::DeleteAnnotation { page, index }, Edit::AddAnnotation(mark)],
    };
    if !app.apply_edit(edit) {
        return None;
    }
    // Added last, so the page's last comment.
    let view = app.views.get_mut(i)?;
    let doc = app.session.get(view.id)?;
    let new = doc.info.annotations.iter().filter(|a| a.page == page && a.in_reply_to.is_none()).map(|a| a.index).max()?;
    view.comments.selected = Some((page, new));
    view.comments.reveal = true;
    Some(new)
}

/// One comment's card (§2): the card fill and edge at rest, the hover fill and the hover edge
/// under the pointer, the selected fill and edge and the selected label colour on the quote when
/// selected. The whole card is one click target under its own widgets.
fn card(ui: &mut egui::Ui, input: &CardInput<'_>, view: &mut DocView, out: &mut Outcome) {
    let (t, a) = (input.t, input.a);
    let key = (a.page, a.index);
    let selected = view.comments.selected == Some(key);
    let editing = view.comments.editing.as_ref().is_some_and(|(page, index, _)| (*page, *index) == key);
    let mut action = None;
    let scope = ui.scope_builder(UiBuilder::new().id_salt(("margin-highlight", key)).sense(Sense::click()), |ui| {
        let resp = ui.response();
        let shown = motion::hover(ui.ctx(), resp.id, resp.contains_pointer());
        let actions_shown = motion::hover(ui.ctx(), resp.id.with("actions"), resp.contains_pointer() || selected);
        let surfaces = look::Surfaces::of(t);
        let (fill, edge) = if selected {
            (t.selected, surfaces.selected_edge)
        } else {
            (t.card.lerp_to_gamma(t.hover, shown), t.border.lerp_to_gamma(surfaces.edge_hover, shown))
        };
        look::card_frame(t).fill(fill).stroke(look::edge_stroke(edge)).inner_margin(GAP_LARGE).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(GAP, GAP);
            action = meta_row(ui, input, actions_shown);
            body(ui, input, selected, editing);
            if editing {
                editor(ui, t, key, view, out);
            }
        });
    });
    let resp = scope.response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let colour = color_name(input.notes, a.color);
    let label = format!("{colour} {}, p. {}: {}", kind_label(a).to_lowercase(), input.page_label, input.quote);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    resp.context_menu(|ui| {
        if let Some(picked) = menu(ui, input) {
            action = Some(picked);
        }
    });
    if selected && std::mem::take(&mut view.comments.reveal) {
        resp.scroll_to_me(Some(Align::Center));
    }
    if resp.clicked() {
        view.comments.selected = Some(key);
        out.nav = Some(Nav::Flash(a.page, a.rect));
    }
    if resp.double_clicked() && input.allowed && !editing {
        action = Some(Action::EditNote);
    }
    if let Some(action) = action {
        act(ui.ctx(), action, input, view, out);
    }
}

/// Carry out a card's action: notes requests go to `margin::obsidian::tick`, edits to the undo
/// stack.
fn act(ctx: &egui::Context, action: Action, input: &CardInput<'_>, view: &mut DocView, out: &mut Outcome) {
    let a = input.a;
    let (page, index) = (a.page, a.index);
    match action {
        Action::CopyCallout => view.comments.notes.ask(ctx, Request::CopyCallout(page, index)),
        Action::OpenInObsidian => view.comments.notes.ask(ctx, Request::Open(Some((page, index)))),
        Action::Color(i) => {
            if let Some(color) = input.notes.colors.get(i) {
                out.edit = Some(Edit::StyleAnnotation { page, index, color: Some(color.rgb_f64()), opacity: None, width: None });
            }
        }
        Action::EditNote => view.comments.editing = Some((page, index, a.contents.clone().unwrap_or_default())),
        Action::ChangeKind(kind) => out.kind_change = Some((page, index, kind)),
        Action::Delete => out.edit = Some(Edit::DeleteAnnotation { page, index }),
    }
}

/// The comment's colour as shown: its own, else the palette's first (the colour a callout gets).
fn shown_color(notes: &Notes, color: Option<[f32; 3]>) -> Color32 {
    match color {
        Some(c) => color32(c.map(f64::from)),
        None => notes.colors.first().map_or(Color32::GRAY, |c| Color32::from_rgb(c.rgb[0], c.rgb[1], c.rgb[2])),
    }
}

/// The palette name of a comment's colour ("Yellow"), or "Custom".
fn color_name(notes: &Notes, color: Option<[f32; 3]>) -> String {
    let named = match color {
        Some(c) => notes.color_named(c),
        None => notes.colors.first(),
    };
    named.map_or_else(|| "Custom".to_owned(), |c| c.name.clone())
}

/// Swatch, type icon, page and a short date ("10 Oct"); then the actions, faded in by
/// `actions_shown` (they keep their place, so nothing beside them moves on hover). A date that
/// doesn't fit beside the page label (the 240-pt sidebar, a long label) goes on a line of its
/// own under the label, never cut or left out; the page label's tooltip has the full metadata,
/// the date's its full timestamp.
fn meta_row(ui: &mut egui::Ui, input: &CardInput<'_>, actions_shown: f32) -> Option<Action> {
    let (t, a) = (input.t, input.a);
    let font = theme::regular(META_TEXT);
    let date = a.modified.as_deref().and_then(|full| ui.ctx().memory_mut(|m| m.caches.cache::<ShortDates>().get((full, input.this_year)).clone()));
    let (action, date_below) = ui
        .allocate_ui_with_layout(vec2(ui.available_width(), META_ROW), Layout::left_to_right(Align::Center), |ui| {
            let (rect, swatch) = ui.allocate_exact_size(vec2(SWATCH, SWATCH), Sense::hover());
            let colour = color_name(input.notes, a.color);
            swatch.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("Colour: {colour}")));
            let edge = Stroke::new(1.0, Color32::from_black_alpha(SWATCH_EDGE_ALPHA));
            ui.painter().circle(rect.center(), SWATCH / 2.0, shown_color(input.notes, a.color), edge);
            look::tooltip(ui.add(icons::image(subtype_icon(&a.subtype), KIND_ICON, t.text_muted)), kind_label(a), None);
            let actions_width = ACTION_COUNT * ACTION + (ACTION_COUNT - 1.0) * ACTION_GAP;
            let room = (ui.available_width() - actions_width - GAP).max(0.0);
            let page = format!("p. {}", input.page_label);
            let width = |text: &str| ui.painter().layout_no_wrap(text.to_owned(), font.clone(), t.text).size().x;
            let (date_beside, date_below) = match date {
                Some(date) if width(&page) + GAP + width(&date) <= room => (Some(date), None),
                other => (None, other),
            };
            ui.allocate_ui_with_layout(vec2(room, META_ROW), Layout::left_to_right(Align::Center), |ui| {
                ui.set_width(room);
                let label = ui.add(egui::Label::new(egui::RichText::new(&page).font(font.clone()).color(t.text_muted)).truncate().selectable(false));
                let full = match &a.modified {
                    Some(modified) => format!("Page {} · {} · {modified}", input.page_label, kind_label(a)),
                    None => format!("Page {} · {}", input.page_label, kind_label(a)),
                };
                look::tooltip(label, &full, None);
                if let Some(date) = date_beside {
                    date_label(ui, input, date, font.clone());
                }
            });
            let action = ui
                .scope(|ui| {
                    ui.spacing_mut().item_spacing.x = ACTION_GAP;
                    ui.multiply_opacity(actions_shown);
                    actions(ui, input)
                })
                .inner;
            (action, date_below)
        })
        .inner;
    if let Some(date) = date_below {
        ui.horizontal(|ui| {
            // Under the page label: past the swatch and the type icon.
            ui.add_space(SWATCH + GAP + KIND_ICON + GAP);
            date_label(ui, input, date, font);
        });
    }
    action
}

/// A card's short date, its full timestamp in the tooltip.
fn date_label(ui: &mut egui::Ui, input: &CardInput<'_>, date: String, font: egui::FontId) {
    let shown = ui.add(egui::Label::new(egui::RichText::new(date).font(font).color(input.t.text_faint)).selectable(false));
    if let Some(modified) = &input.a.modified {
        look::tooltip(shown, modified, None);
    }
}

/// The hover actions, left to right: Copy callout, Open in Obsidian, Change colour, Delete.
fn actions(ui: &mut egui::Ui, input: &CardInput<'_>) -> Option<Action> {
    let a = input.a;
    let mut action = None;
    ui.add_enabled_ui(input.notes_on, |ui| {
        let copy_tip = if input.external { obsidian::EXTERNAL_TIP } else { "Copy callout" };
        if look::icon_button(ui, "copy", ACTION, false, copy_tip, None).clicked() {
            action = Some(Action::CopyCallout);
        }
        let (open_tip, can_open) = match input.no_note {
            Some(why) => (format!("Open in Obsidian: {why}"), false),
            None => ("Open in Obsidian".to_owned(), true),
        };
        if ui.add_enabled_ui(can_open, |ui| look::icon_button(ui, "external-link", ACTION, false, &open_tip, None)).inner.clicked() {
            action = Some(Action::OpenInObsidian);
        }
    });
    ui.add_enabled_ui(input.editable(), |ui| {
        let colour = look::color_button(ui, ACTION, shown_color(input.notes, a.color), false, "Change colour", None);
        look::popover(&colour, egui::Id::new(("margin-highlight-colour", a.page, a.index))).show(|ui| {
            if let Some(i) = color_row(ui, input) {
                action = Some(Action::Color(i));
            }
        });
        if look::icon_button(ui, "trash-2", ACTION, false, "Delete highlight", input.keys.delete.as_deref()).clicked() {
            action = Some(Action::Delete);
        }
    });
    action
}

/// The `notes.colors` palette as a row of dots, the comment's own ringed; returns the clicked
/// colour's index.
fn color_row(ui: &mut egui::Ui, input: &CardInput<'_>) -> Option<usize> {
    let current = input.a.color.and_then(|c| input.notes.color_named(c)).or_else(|| input.notes.colors.first().filter(|_| input.a.color.is_none()));
    let dots: Vec<ColorDot<'_>> = input
        .notes
        .colors
        .iter()
        .enumerate()
        .map(|(i, c)| ColorDot {
            name: &c.name,
            color: Color32::from_rgb(c.rgb[0], c.rgb[1], c.rgb[2]),
            selected: current == Some(c),
            shortcut: input.keys.colors.get(i).and_then(Option::as_deref),
        })
        .collect();
    look::color_dots(ui, &dots)
}

/// The right-click menu: what the comment is, its colours, Change to underline (or highlight),
/// the notes actions, its note and Delete with its keys.
fn menu(ui: &mut egui::Ui, input: &CardInput<'_>) -> Option<Action> {
    let (t, a) = (input.t, input.a);
    ui.set_min_width(MENU_WIDTH);
    let title = format!("{} {} · p. {}", color_name(input.notes, a.color), kind_label(a).to_lowercase(), input.page_label);
    ui.label(egui::RichText::new(title).font(theme::semibold(META_TEXT)).color(t.text_faint));
    let mut action = None;
    if input.editable()
        && let Some(i) = color_row(ui, input)
    {
        action = Some(Action::Color(i));
    }
    if let Some(kind) = other_kind(a) {
        let refused = if input.allowed { kind_change_refused(input.info, a) } else { Some(NOT_ALLOWED) };
        let label = if kind == Markup::Underline { "Change to underline" } else { "Change to highlight" };
        let change = ui.add_enabled(refused.is_none(), egui::Button::new(label));
        let change = match refused {
            Some(why) => look::tooltip(change, &format!("{label}: {why}"), None),
            None => change,
        };
        if change.clicked() {
            action = Some(Action::ChangeKind(kind));
        }
    }
    ui.separator();
    if input.notes_on {
        let copy = ui.button("Copy callout");
        let copy = if input.external { look::tooltip(copy, obsidian::EXTERNAL_TIP, None) } else { copy };
        if copy.clicked() {
            action = Some(Action::CopyCallout);
        }
        let open = ui.add_enabled(input.no_note.is_none(), egui::Button::new("Open in Obsidian"));
        let tip = input.no_note.map_or_else(|| "Add this highlight to the reading note and open it in Obsidian".to_owned(), str::to_owned);
        if look::tooltip(open, &tip, None).clicked() {
            action = Some(Action::OpenInObsidian);
        }
    }
    let has_note = a.contents.as_deref().is_some_and(|c| !c.trim().is_empty());
    if ui.add_enabled(input.allowed, egui::Button::new(if has_note { "Edit note" } else { "Add note" })).clicked() {
        action = Some(Action::EditNote);
    }
    ui.separator();
    let delete = egui::Button::new("Delete").shortcut_text(input.keys.delete.clone().unwrap_or_default());
    if ui.add_enabled(input.editable(), delete).clicked() {
        action = Some(Action::Delete);
    }
    if action.is_some() {
        ui.close();
    }
    action
}

/// The quote (three lines unless selected) and the note. Without a quote, the note alone; with
/// neither, the comment type in muted italics.
fn body(ui: &mut egui::Ui, input: &CardInput<'_>, selected: bool, editing: bool) {
    let (t, a) = (input.t, input.a);
    let quote = input.quote.trim();
    let note = a.contents.as_deref().map(str::trim).filter(|note| !note.is_empty() && *note != quote);
    if !quote.is_empty() {
        let rows = if selected { usize::MAX } else { QUOTE_ROWS };
        let color = look::row_text(t, selected, t.text);
        text_block(ui, quote, TextFormat { font_id: theme::regular(QUOTE_TEXT), color, ..Default::default() }, rows);
    } else if note.is_none() && !editing {
        let format = TextFormat { font_id: theme::regular(QUOTE_TEXT), color: t.text_faint, italics: true, ..Default::default() };
        text_block(ui, kind_label(a), format, 1);
    }
    if let Some(note) = note.filter(|_| !editing) {
        text_block(ui, note, TextFormat { font_id: theme::regular(NOTE_TEXT), color: t.text_muted, italics: true, ..Default::default() }, usize::MAX);
    }
}

/// Text wrapped to the card's width, cut to `rows` lines with an ellipsis.
fn text_block(ui: &mut egui::Ui, text: &str, format: TextFormat, rows: usize) {
    let mut job = LayoutJob::single_section(text.to_owned(), format);
    job.wrap = TextWrapping { max_width: ui.available_width(), max_rows: rows, break_anywhere: false, overflow_character: Some('…') };
    let galley = ui.painter().layout_job(job);
    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
    ui.painter().galley(rect.min, galley, Color32::PLACEHOLDER);
}

/// The note being typed into this card, with Cancel and Save under it on the right.
fn editor(ui: &mut egui::Ui, t: &Tokens, key: (usize, usize), view: &mut DocView, out: &mut Outcome) {
    let Some((_, _, text)) = view.comments.editing.as_mut() else { return };
    let id = ui.make_persistent_id(("margin-highlight-note", key));
    // Focus it when it appears, not every frame: a press on the page takes the keys back, so a
    // note left open never swallows V, H or 1-4 meant for the reader.
    let appearing = ui.ctx().read_response(id).is_none();
    let field = egui::TextEdit::multiline(text)
        .desired_rows(EDITOR_ROWS)
        .desired_width(ui.available_width())
        .margin(EDITOR_MARGIN)
        .font(theme::regular(NOTE_TEXT))
        .text_color(t.text)
        .hint_text("Add a note")
        .id(id);
    let resp = ui.add(field);
    if appearing {
        resp.request_focus();
    }
    let (save, cancel) = ui
        .allocate_ui_with_layout(vec2(ui.available_width(), BUTTON_HEIGHT), Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            let save = look::button_primary(ui, look::ButtonSize::Compact, "Save").clicked();
            (save, look::button_secondary(ui, look::ButtonSize::Compact, "Cancel").clicked())
        })
        .inner;
    if save && let Some((page, index, text)) = view.comments.editing.take() {
        out.edit = Some(Edit::SetAnnotationContents { page, index, text });
    } else if cancel || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        view.comments.editing = None;
    }
}

/// The months as a card's short date names them.
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Short dates for the cards, each formatted once while it is shown (egui's frame cache, keyed
/// by the annotation's date as PdfCraft gives it).
type ShortDates = egui::cache::FrameCache<Option<String>, ShortDate>;

#[derive(Default)]
struct ShortDate;

impl egui::cache::ComputerMut<(&str, i64), Option<String>> for ShortDate {
    fn compute(&mut self, (modified, this_year): (&str, i64)) -> Option<String> {
        short_date(modified, this_year)
    }
}

/// `modified` as PdfCraft shows an annotation's `/M` ("2026-10-10 12:30"), shortened: "10 Oct"
/// in `this_year`, "10 Oct 2025" otherwise. `None` for anything else (a date PdfCraft couldn't
/// read stays whole in the tooltip, never cut on the card).
fn short_date(modified: &str, this_year: i64) -> Option<String> {
    let ymd = modified.get(..10)?;
    let digits = |range: std::ops::Range<usize>| ymd.get(range).filter(|s| s.bytes().all(|b| b.is_ascii_digit()));
    if ymd.as_bytes().get(4) != Some(&b'-') || ymd.as_bytes().get(7) != Some(&b'-') {
        return None;
    }
    let year = digits(0..4)?.parse::<i64>().ok()?;
    let month = MONTHS.get(digits(5..7)?.parse::<usize>().ok()?.checked_sub(1)?)?;
    let day = digits(8..10)?.parse::<u8>().ok().filter(|day| (1..=31).contains(day))?;
    Some(if year == this_year { format!("{day} {month}") } else { format!("{day} {month} {year}") })
}

#[cfg(test)]
mod tests {
    use super::short_date;

    #[test]
    fn short_dates_drop_this_year_and_refuse_what_they_cannot_read() {
        assert_eq!(short_date("2026-10-10 12:30", 2026).as_deref(), Some("10 Oct"));
        assert_eq!(short_date("2025-01-05 09:00", 2026).as_deref(), Some("5 Jan 2025"));
        assert_eq!(short_date("2026-10-10", 2026).as_deref(), Some("10 Oct"));
        for unreadable in ["Tuesday", "2026-13-01 00:00", "2026-00-10", "2026-10-32", "D:2026101012", "", "20261010"] {
            assert_eq!(short_date(unreadable, 2026), None, "{unreadable}");
        }
    }
}
