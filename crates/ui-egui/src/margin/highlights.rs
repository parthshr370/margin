//! Margin's Highlights tab: the document's highlights, underlines, strikethroughs and squiggles
//! as cards in reading order (the mockup's `.hl-item`), under the reading-note box
//! ([`obsidian::panel_block`]).
//!
//! A card shows the comment's colour swatch, its type icon and page, the page text it marks
//! (extracted by the renderer's text layer, the text a callout quotes) and its note. Clicking a
//! card selects the comment and shows it on the page; double-clicking opens its note for typing,
//! as PdfCraft's Comments panel does. Every change is an engine [`Edit`] applied through
//! `PdfCraftApp::apply_edit`, so it lands on the document's undo stack.
//!
//! Cards are on the spacing scale: padded by the large step, rows the small step apart, the
//! note editor and its Save (accent) and Cancel (text) buttons inside the card.

use std::collections::HashMap;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align, Color32, Layout, Sense, Stroke, UiBuilder, vec2};
use pdfcraft_engine::Edit;
use pdfcraft_render::{Annotation, DocInfo, PageText};

use super::look::{self, BUTTON_HEIGHT, GAP, GAP_LARGE};
use super::motion;
use super::obsidian::{self, Notes};
use crate::canvas::DocView;
use crate::comments::color32;
use crate::comments_panel::{kind_label, subtype_icon};
use crate::panels::Nav;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

/// Space between neighbouring cards: their hover fills nearly touch, so the list reads as one.
const CARD_SPACING: f32 = 2.0;
/// The top row of a card: swatch, type icon, page and date.
const META_ROW: f32 = 24.0;
const SWATCH: f32 = 10.0;
const KIND_ICON: f32 = 14.0;
const META_TEXT: f32 = 11.5;
const QUOTE_TEXT: f32 = 12.5;
const NOTE_TEXT: f32 = 12.0;
/// An unselected card shows this many lines of its quote; the selected one shows all of it.
const QUOTE_ROWS: usize = 3;
/// The note editor's text padding and height in lines.
const EDITOR_MARGIN: egui::Margin = egui::Margin::symmetric(8, 6);
const EDITOR_ROWS: usize = 2;

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

/// What a card shows besides the comment and the view's selection.
struct CardInput<'a> {
    t: &'a Tokens,
    a: &'a Annotation,
    quote: &'a str,
    page_label: &'a str,
    notes: &'a Notes,
    allowed: bool,
}

/// The notes box and the cards of view `index` (which has highlights). Edits go to the undo
/// stack; returns where to show the clicked comment.
pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize) -> Option<Nav> {
    let view = app.views.get_mut(index)?;
    let doc = app.session.get(view.id)?;
    let info = &doc.info;
    view.margin.quotes.refresh(doc.edit_generation(), info, &view.texts);
    let mut out = Outcome::default();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        obsidian::panel_block(ui, t, &mut view.comments.notes);
        ui.spacing_mut().item_spacing.y = CARD_SPACING;
        for a in obsidian::markup_comments(info) {
            let quote = view.margin.quotes.text.get(&(a.page, a.index)).cloned().unwrap_or_default();
            let page_label = info.pages.get(a.page).map_or_else(|| a.page.saturating_add(1).to_string(), |p| p.label.clone());
            let input = CardInput { t, a, quote: &quote, page_label: &page_label, notes: &app.notes, allowed: doc.allows_annotation() };
            card(ui, &input, view, &mut out);
        }
    });
    if let Some(edit) = out.edit {
        app.apply_edit(edit);
    }
    out.nav
}

/// One comment's card. The whole card is one click target under its own widgets.
fn card(ui: &mut egui::Ui, input: &CardInput<'_>, view: &mut DocView, out: &mut Outcome) {
    let (t, a) = (input.t, input.a);
    let key = (a.page, a.index);
    let selected = view.comments.selected == Some(key);
    let editing = view.comments.editing.as_ref().is_some_and(|(page, index, _)| (*page, *index) == key);
    let scope = ui.scope_builder(UiBuilder::new().id_salt(("margin-highlight", key)).sense(Sense::click()), |ui| {
        let resp = ui.response();
        let shown = motion::hover(ui.ctx(), resp.id, resp.contains_pointer());
        let fill = if selected { t.selected } else { t.hover.gamma_multiply(shown) };
        egui::Frame::NONE.fill(fill).corner_radius(t.radius).inner_margin(GAP_LARGE).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(GAP, GAP);
            meta_row(ui, input);
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
    if selected && std::mem::take(&mut view.comments.reveal) {
        resp.scroll_to_me(Some(Align::Center));
    }
    if resp.clicked() {
        view.comments.selected = Some(key);
        out.nav = Some(Nav::Flash(a.page, a.rect));
    }
    if resp.double_clicked() && input.allowed && !editing {
        view.comments.editing = Some((a.page, a.index, a.contents.clone().unwrap_or_default()));
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

/// Swatch, type icon, page and date.
fn meta_row(ui: &mut egui::Ui, input: &CardInput<'_>) {
    let (t, a) = (input.t, input.a);
    ui.allocate_ui_with_layout(vec2(ui.available_width(), META_ROW), Layout::left_to_right(Align::Center), |ui| {
        let (rect, swatch) = ui.allocate_exact_size(vec2(SWATCH, SWATCH), Sense::hover());
        let colour = color_name(input.notes, a.color);
        swatch.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("Colour: {colour}")));
        ui.painter().circle(rect.center(), SWATCH / 2.0, shown_color(input.notes, a.color), Stroke::new(1.0, Color32::from_black_alpha(40)));
        ui.add(icons::image(subtype_icon(&a.subtype), KIND_ICON, t.text_muted)).on_hover_text(kind_label(a));
        let page = egui::RichText::new(format!("p. {}", input.page_label)).font(theme::regular(META_TEXT)).color(t.text_muted);
        ui.add(egui::Label::new(page).selectable(false));
        if let Some(date) = &a.modified {
            let date = egui::RichText::new(date).font(theme::regular(META_TEXT)).color(t.text_faint);
            ui.add(egui::Label::new(date).truncate().selectable(false));
        }
    });
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
