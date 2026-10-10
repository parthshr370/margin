//! Margin's Highlights tab: the document's highlights, underlines, strikethroughs and squiggles
//! as cards in reading order (the mockup's `.hl-item`), under the reading-note box
//! ([`obsidian::panel_block`]).
//!
//! A card shows the comment's colour swatch, its type icon and page, the page text it marks
//! (extracted by the renderer's text layer, the text a callout quotes) and its note. Clicking a
//! card selects the comment and shows it on the page; double-clicking opens its note for typing,
//! as PdfCraft's Comments panel does. Hovering shows its actions (Copy callout, Open in Obsidian,
//! Change colour across `notes.colors`, Delete); right-clicking lists the same. The keymap ids
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
use pdfcraft_engine::Edit;
use pdfcraft_render::{Annotation, DocInfo, PageText};
use serde_json::Value;

use super::look::{self, BUTTON_HEIGHT, ColorDot, GAP, GAP_LARGE};
use super::motion;
use super::obsidian::{self, Notes, Request};
use crate::canvas::DocView;
use crate::comments::color32;
use crate::comments_panel::{kind_label, subtype_icon};
use crate::panels::Nav;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

/// Delete the selected comment (`Delete` / `Backspace` in the `Annotation` context).
pub const DELETE_ID: &str = "annotation.delete";
/// Give the selected comment a `notes.colors` colour, by name: `{ "color": "red" }`.
pub const COLOR_ID: &str = "annotation.color";

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
    nav: Option<Nav>,
}

/// What a card's hover buttons and menu do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    CopyCallout,
    OpenInObsidian,
    /// The `notes.colors` entry at this index.
    Color(usize),
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
    let view = app.views.get_mut(index)?;
    let doc = app.session.get(view.id)?;
    let info = &doc.info;
    view.margin.quotes.refresh(doc.edit_generation(), info, &view.texts);
    let mut out = Outcome::default();
    let no_note = view.comments.notes.note.clone().err();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        obsidian::panel_block(ui, t, &mut view.comments.notes);
        ui.spacing_mut().item_spacing.y = CARD_SPACING;
        for a in obsidian::markup_comments(info) {
            let quote = view.margin.quotes.text.get(&(a.page, a.index)).cloned().unwrap_or_default();
            let page_label = info.pages.get(a.page).map_or_else(|| a.page.saturating_add(1).to_string(), |p| p.label.clone());
            let input = CardInput {
                t,
                a,
                quote: &quote,
                page_label: &page_label,
                notes: &app.notes,
                keys: &keys,
                allowed: doc.allows_annotation(),
                notes_on: view.comments.notes.enabled,
                external: view.comments.notes.external,
                no_note: no_note.as_deref(),
            };
            card(ui, &input, view, &mut out);
        }
    });
    if let Some(edit) = out.edit {
        app.apply_edit(edit);
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
        app.notify("This PDF doesn't allow changes to its comments.");
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
    app.apply_edit(edit);
    true
}

/// One comment's card. The whole card is one click target under its own widgets.
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
        let fill = if selected { t.selected } else { t.hover.gamma_multiply(shown) };
        egui::Frame::NONE.fill(fill).corner_radius(t.radius).inner_margin(GAP_LARGE).show(ui, |ui| {
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

/// Swatch, type icon, page and date; then the actions, faded in by `actions_shown` (they keep
/// their place, so the date's width never jumps).
fn meta_row(ui: &mut egui::Ui, input: &CardInput<'_>, actions_shown: f32) -> Option<Action> {
    let (t, a) = (input.t, input.a);
    ui.allocate_ui_with_layout(vec2(ui.available_width(), META_ROW), Layout::left_to_right(Align::Center), |ui| {
        let (rect, swatch) = ui.allocate_exact_size(vec2(SWATCH, SWATCH), Sense::hover());
        let colour = color_name(input.notes, a.color);
        swatch.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("Colour: {colour}")));
        let edge = Stroke::new(1.0, Color32::from_black_alpha(SWATCH_EDGE_ALPHA));
        ui.painter().circle(rect.center(), SWATCH / 2.0, shown_color(input.notes, a.color), edge);
        look::tooltip(ui.add(icons::image(subtype_icon(&a.subtype), KIND_ICON, t.text_muted)), kind_label(a), None);
        let page = egui::RichText::new(format!("p. {}", input.page_label)).font(theme::regular(META_TEXT)).color(t.text_muted);
        ui.add(egui::Label::new(page).selectable(false));
        let actions_width = ACTION_COUNT * ACTION + (ACTION_COUNT - 1.0) * ACTION_GAP;
        let date_width = (ui.available_width() - actions_width - GAP).max(0.0);
        ui.allocate_ui_with_layout(vec2(date_width, META_ROW), Layout::left_to_right(Align::Center), |ui| {
            ui.set_width(date_width);
            if let Some(date) = &a.modified {
                let date = egui::RichText::new(date).font(theme::regular(META_TEXT)).color(t.text_faint);
                ui.add(egui::Label::new(date).truncate().selectable(false));
            }
        });
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = ACTION_GAP;
            ui.multiply_opacity(actions_shown);
            actions(ui, input)
        })
        .inner
    })
    .inner
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

/// The right-click menu: what the comment is, its colours, the notes actions, its note and
/// Delete with its keys.
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
        text_block(ui, quote, TextFormat { font_id: theme::regular(QUOTE_TEXT), color: t.text, ..Default::default() }, rows);
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
    let field = egui::TextEdit::multiline(text)
        .desired_rows(EDITOR_ROWS)
        .desired_width(ui.available_width())
        .margin(EDITOR_MARGIN)
        .font(theme::regular(NOTE_TEXT))
        .text_color(t.text)
        .hint_text("Add a note")
        .id_salt(("margin-highlight-note", key));
    let resp = ui.add(field);
    // A new highlight opens its note for typing, as in PdfCraft.
    if !resp.has_focus() && !ui.memory(|m| m.focused().is_some()) {
        resp.request_focus();
    }
    let (save, cancel) = ui
        .allocate_ui_with_layout(vec2(ui.available_width(), BUTTON_HEIGHT), Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            let save = look::accent_button(ui, "Save").clicked();
            (save, look::plain_button(ui, "Cancel").clicked())
        })
        .inner;
    if save && let Some((page, index, text)) = view.comments.editing.take() {
        out.edit = Some(Edit::SetAnnotationContents { page, index, text });
    } else if cancel || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        view.comments.editing = None;
    }
}
