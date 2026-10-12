//! Margin's Highlights tab: the document's highlights, underlines, strikethroughs and squiggles,
//! and its page notes, as cards (the mockup's `.hl-item`), under the reading-note box
//! ([`obsidian::panel_block`]). Cards come from the PDF's text-markup comments and from the
//! sidecar (`margin::marks`: highlights of a PDF Margin doesn't write, and page notes).
//!
//! Above the cards: a search over the quoted text and the notes, and a filter popover (one
//! `notes.colors` colour, one kind, sorted by page or newest first). The list is virtualised:
//! only the cards in view (plus the selected card while it is revealed and the card whose note
//! is being typed) are laid out, each at an offset from the heights measured when it was last
//! shown; the rows behind them are gathered once per change of the document, the sidecar marks,
//! the quotes or the palette, not every frame.
//!
//! A card shows the colour swatch, its type icon and page, the page text it marks (extracted by
//! the renderer's text layer, the text a callout quotes) and its note. Clicking a card selects
//! the mark and shows it on the page; double-clicking opens its note for typing, as PdfCraft's
//! Comments panel does. Hovering shows its actions (Copy callout, Open in Obsidian, Change
//! colour across `notes.colors`, Delete); right-clicking lists the same, and turns a highlight
//! into an underline or back ([`change_kind`]). The keymap ids [`DELETE_ID`] and [`COLOR_ID`] act
//! on the selected mark, picked here or on the page. A PDF comment's change is an engine [`Edit`]
//! applied through `PdfCraftApp::apply_edit`; a sidecar mark's is a step of `margin::marks`'
//! history. Undo brings either back.
//!
//! Cards are on the spacing scale: padded by the large step, rows the small step apart, the
//! note editor and its Save (accent) and Cancel (text) buttons inside the card.

use std::collections::HashMap;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align, Color32, Layout, Rect, Sense, Stroke, UiBuilder, Vec2, vec2};
use margin_config::marks as records;
use margin_config::{HighlightStyle, Locator};
use pdfcraft_engine::{Edit, Markup, NewAnnotation, Shape};
use pdfcraft_render::{Annotation, DocInfo, PageText};
use serde_json::Value;

use super::look::{self, BUTTON_HEIGHT, ColorDot, GAP, GAP_LARGE};
use super::marks::{self, Marks, NotePlace, Pick};
use super::motion;
use super::obsidian::{self, Notes, Request};
use crate::canvas::DocView;
use crate::comments::{CommentTool, color32};
use crate::comments_panel::{kind_label, subtype_icon, subtype_label};
use crate::panels::Nav;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

/// Delete the selected comment (`Delete` / `Backspace` in the `Annotation` context).
pub const DELETE_ID: &str = "annotation.delete";
/// Give the selected comment a `notes.colors` colour, by name: `{ "color": "red" }`.
pub const COLOR_ID: &str = "annotation.color";
/// Said when the PDF refuses comment changes.
pub(crate) const NOT_ALLOWED: &str = "This PDF doesn't allow changes to its comments.";
/// Why a highlight kept beside the PDF has no Open in Obsidian.
pub(crate) const BESIDE_OPEN_TIP: &str = "This highlight is kept beside the PDF, not in it, so Obsidian can't show it; Copy callout quotes it";

/// The subtype a page note's card shows (its icon is a sticky note's).
const NOTE_SUBTYPE: &str = "Text";
const NOTE_LABEL: &str = "Page note";

/// Space between neighbouring cards: their hover fills nearly touch, so the list reads as one.
const CARD_SPACING: f32 = 2.0;
/// A card's height before it has been laid out once.
const CARD_ESTIMATE: f32 = 96.0;
/// Cards this far above or below the view are laid out too, so a short scroll finds them ready.
const OVERSCAN: f32 = 240.0;
/// The top row of a card: swatch, type icon, page and date, then the actions.
const META_ROW: f32 = 24.0;
const SWATCH: f32 = 10.0;
/// How dark the swatch's edge is, so light colours show on the hover fill.
const SWATCH_EDGE_ALPHA: u8 = 40;
const KIND_ICON: f32 = 14.0;
const META_TEXT: f32 = 11.5;
const QUOTE_TEXT: f32 = 12.5;
const NOTE_TEXT: f32 = 12.0;
/// An unexpanded card shows this many lines of its quote; "Show more" shows all of it.
const QUOTE_ROWS: usize = 3;
/// The note editor's text padding and height in lines.
const EDITOR_MARGIN: egui::Margin = egui::Margin::symmetric(8, 6);
const EDITOR_ROWS: usize = 2;
/// The hover actions: four in-card buttons, this far apart.
const ACTION: f32 = look::SMALL_BUTTON;
const ACTION_GAP: f32 = 2.0;
const ACTION_COUNT: f32 = 4.0;
/// The search field's icon, its height and the filter button beside it (one compact control
/// tall, like the pane's other controls), and the widest count it shows.
const SEARCH_ICON: f32 = 14.0;
const FILTER_BUTTON: f32 = look::BUTTON_HEIGHT;
const COUNT_TEMPLATE: &str = "000 of 000";
/// The filter popover's width.
const FILTER_WIDTH: f32 = 220.0;
/// Characters of a date the cards compare and show: "2026-10-10 12:30".
const DATE_CHARS: usize = 16;

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

/// What a card is: a PDF comment (page, index) or a sidecar mark.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Entry {
    Pdf(usize, usize),
    Mark(Pick),
}

/// Which kind of card the list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum KindFilter {
    #[default]
    All,
    Highlights,
    Underlines,
    Strikethroughs,
    Squiggles,
    Notes,
}

impl KindFilter {
    const ALL: [Self; 6] = [Self::All, Self::Highlights, Self::Underlines, Self::Strikethroughs, Self::Squiggles, Self::Notes];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All kinds",
            Self::Highlights => "Highlights",
            Self::Underlines => "Underlines",
            Self::Strikethroughs => "Strikethroughs",
            Self::Squiggles => "Squiggly underlines",
            Self::Notes => "Page notes",
        }
    }

    /// Whether a card of `subtype` (the PDF's name: "Highlight", "StrikeOut"…) passes.
    fn passes(self, subtype: &str) -> bool {
        match self {
            Self::All => true,
            Self::Highlights => subtype == "Highlight",
            Self::Underlines => subtype == "Underline",
            Self::Strikethroughs => subtype == "StrikeOut",
            Self::Squiggles => subtype == "Squiggly",
            Self::Notes => subtype == NOTE_SUBTYPE,
        }
    }
}

/// The order of the cards.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Sort {
    /// Reading order: page, then top to bottom.
    #[default]
    Page,
    /// Last changed first.
    Newest,
}

/// The search and filters of the list.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Filter {
    /// Matched, case-insensitively, against the quote and the note.
    pub query: String,
    /// The `notes.colors` entry the cards must be in.
    pub color: Option<usize>,
    pub kind: KindFilter,
    pub sort: Sort,
}

impl Filter {
    /// A filter other than the search is on.
    fn narrows(&self) -> bool {
        self.color.is_some() || self.kind != KindFilter::All
    }
}

/// One card's contents, gathered when the list is rebuilt.
struct Row {
    entry: Entry,
    page: usize,
    /// Where on its page, in view space: (top, left).
    at: (f32, f32),
    /// The PDF subtype the card shows ("Highlight", "Underline", "StrikeOut", "Squiggly", or
    /// [`NOTE_SUBTYPE`] for a page note).
    subtype: &'static str,
    label: &'static str,
    color: Option<[f32; 3]>,
    /// Its `notes.colors` entry, if its colour is one.
    palette: Option<usize>,
    quote: String,
    note: Option<String>,
    /// When it last changed, as PdfCraft shows dates ("2026-10-10 12:30").
    date: Option<String>,
    /// Where to flash on the page (user space).
    flash: [f32; 4],
    /// Locked in the PDF: no colour change or deletion.
    locked: bool,
    /// Why it can't turn into the other kind, if it can't (PDF comments with replies).
    kind_refused: Option<&'static str>,
    /// The lower-case quote and note, for the search.
    haystack: String,
}

impl Row {
    /// The kind a highlight or an underline can become.
    fn other_kind(&self) -> Option<Markup> {
        match self.subtype {
            "Highlight" => Some(Markup::Underline),
            "Underline" => Some(Markup::Highlight),
            _ => None,
        }
    }

    fn passes(&self, filter: &Filter, query: &str) -> bool {
        filter.kind.passes(self.subtype) && filter.color.is_none_or(|color| self.palette == Some(color)) && self.haystack.contains(query)
    }
}

/// What the rows were gathered from: when any changes, they are gathered again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Source {
    edit_generation: u64,
    marks: u64,
    quotes: usize,
    palette: u64,
}

/// The Highlights tab's list of one document: the filter, the gathered rows, the cards shown and
/// their measured heights. Lives in the document's `DocReading`.
#[derive(Default)]
pub struct List {
    pub(crate) filter: Filter,
    source: Option<Source>,
    rows: Vec<Row>,
    /// Indices into `rows` of the cards shown, in order, for `shown_for`.
    shown: Vec<usize>,
    shown_for: Option<Filter>,
    heights: HashMap<Entry, f32>,
}

impl List {
    /// How many cards the document has, filtered or not.
    pub(crate) fn total(&self) -> usize {
        self.rows.len()
    }
}

/// `note.list`: the list shows the page notes only.
pub(crate) fn show_only_notes(view: &mut DocView) {
    view.margin.list.filter.kind = KindFilter::Notes;
}

/// How many highlights and notes the Highlights tab lists for view `index`.
pub(crate) fn count(app: &PdfCraftApp, index: usize) -> usize {
    let Some(view) = app.views.get(index) else { return 0 };
    let pdf = app.session.get(view.id).map_or(0, |doc| doc.info.annotations.iter().filter(|a| obsidian::is_markup(a)).count());
    pdf + view.margin.marks.highlights().len() + view.margin.marks.notes().len()
}

/// A cheap fingerprint of the palette, so renamed or recoloured `notes.colors` regather rows.
fn palette_key(notes: &Notes) -> u64 {
    notes.colors.iter().fold(notes.colors.len() as u64, |acc, c| {
        let rgb = u64::from(c.rgb[0]) << 16 | u64::from(c.rgb[1]) << 8 | u64::from(c.rgb[2]);
        acc.wrapping_mul(31).wrapping_add(rgb).wrapping_add(c.name.len() as u64)
    })
}

/// A date as cards compare and show it: PdfCraft's "2026-10-10 12:30", or a sidecar's RFC 3339
/// "2026-10-10T12:30:00Z" made the same.
fn card_date(date: &str) -> String {
    date.replacen('T', " ", 1).chars().take(DATE_CHARS).collect()
}

/// The palette index of `rgb`.
fn palette_of(notes: &Notes, rgb: Option<[f32; 3]>) -> Option<usize> {
    let named = match rgb {
        Some(rgb) => notes.color_named(rgb)?,
        None => notes.colors.first()?,
    };
    notes.colors.iter().position(|c| c == named)
}

fn haystack(quote: &str, note: Option<&str>) -> String {
    format!("{quote}\n{}", note.unwrap_or_default()).to_lowercase()
}

/// The subtype and label of a sidecar highlight's style.
fn style_parts(style: HighlightStyle) -> (&'static str, &'static str) {
    match style {
        HighlightStyle::Highlight => ("Highlight", "Highlight"),
        HighlightStyle::Underline => ("Underline", "Underline"),
        HighlightStyle::Strikeout => ("StrikeOut", "Strikethrough"),
        HighlightStyle::Squiggly => ("Squiggly", "Squiggly"),
    }
}

/// The PDF subtype as a `'static` name, for the subtypes the list shows.
fn markup_subtype(subtype: &str) -> &'static str {
    match subtype {
        "Underline" => "Underline",
        "StrikeOut" => "StrikeOut",
        "Squiggly" => "Squiggly",
        _ => "Highlight",
    }
}

/// Gather every card's row of a document, in reading order.
fn gather(info: &DocInfo, quotes: &Quotes, marks: &Marks, notes: &Notes) -> Vec<Row> {
    let mut rows = Vec::new();
    for a in info.annotations.iter().filter(|a| obsidian::is_markup(a)) {
        let quote = quotes.text.get(&(a.page, a.index)).cloned().unwrap_or_default();
        let note = a.contents.clone().filter(|note| !note.trim().is_empty());
        let at = info.pages.get(a.page).map_or((0.0, 0.0), |page| {
            let [x0, y0, x1, y1] = a.rect;
            let (p, q) = (page.user_to_view(x0, y0), page.user_to_view(x1, y1));
            (p[1].min(q[1]), p[0].min(q[0]))
        });
        let subtype = markup_subtype(&a.subtype);
        rows.push(Row {
            entry: Entry::Pdf(a.page, a.index),
            page: a.page,
            at,
            subtype,
            label: subtype_label(subtype),
            color: a.color,
            palette: palette_of(notes, a.color),
            haystack: haystack(&quote, note.as_deref()),
            quote,
            note,
            date: a.modified.as_deref().map(card_date),
            flash: a.rect,
            locked: a.locked,
            kind_refused: kind_change_refused(info, a),
        });
    }
    for h in marks.highlights() {
        let Locator::Pdf { page, quads, .. } = &h.locator else { continue };
        let Some((page, info_page)) = usize::try_from(*page).ok().and_then(|page| Some((page, info.pages.get(page)?))) else { continue };
        let bounds = quads
            .iter()
            .flat_map(|q| [(q[0], q[1]), (q[2], q[3]), (q[4], q[5]), (q[6], q[7])])
            .fold(None, |acc: Option<(f64, f64)>, (x, y)| Some(acc.map_or((y, x), |(top, left)| (top.min(y), left.min(x)))));
        let rgb = marks::shown_rgb(notes, h);
        let quote = h.text.clone().unwrap_or_default();
        let (subtype, label) = style_parts(h.style);
        rows.push(Row {
            entry: Entry::Mark(Pick::Highlight(h.id.clone())),
            page,
            at: bounds.map_or((0.0, 0.0), |(top, left)| (top as f32, left as f32)),
            subtype,
            label,
            color: Some(rgb),
            palette: palette_of(notes, Some(rgb)),
            haystack: haystack(&quote, h.note.as_deref()),
            quote,
            note: h.note.clone(),
            date: Some(card_date(h.modified.as_deref().unwrap_or(&h.created))),
            flash: marks::user_rect(info_page, h).unwrap_or_default(),
            locked: false,
            kind_refused: None,
        });
    }
    for n in marks.notes() {
        let Some((page, info_page)) = records::locator_page(&n.locator).and_then(|page| Some((page, info.pages.get(page)?))) else { continue };
        let [across, down] = marks::note_at(n);
        rows.push(Row {
            entry: Entry::Mark(Pick::Note(n.id.clone())),
            page,
            at: (down * info_page.height, across * info_page.width),
            subtype: NOTE_SUBTYPE,
            label: NOTE_LABEL,
            color: None,
            palette: None,
            haystack: haystack("", Some(&n.text)),
            quote: String::new(),
            note: Some(n.text.clone()),
            date: Some(card_date(n.modified.as_deref().unwrap_or(&n.created))),
            flash: marks::note_user_rect(info_page, n),
            locked: false,
            kind_refused: None,
        });
    }
    rows.sort_by(|a, b| a.page.cmp(&b.page).then(a.at.0.total_cmp(&b.at.0)).then(a.at.1.total_cmp(&b.at.1)));
    rows
}

/// The rows `filter` shows, in its order.
fn select(rows: &[Row], filter: &Filter) -> Vec<usize> {
    let query = filter.query.trim().to_lowercase();
    let mut shown: Vec<usize> = rows.iter().enumerate().filter(|(_, row)| row.passes(filter, &query)).map(|(i, _)| i).collect();
    if filter.sort == Sort::Newest {
        // Stable: equal dates keep reading order; undated cards go last.
        shown.sort_by(|a, b| {
            let date = |i: &usize| rows.get(*i).and_then(|row| row.date.clone());
            date(b).cmp(&date(a))
        });
    }
    shown
}

/// What the cards asked for this frame.
#[derive(Default)]
struct Outcome {
    edit: Option<Edit>,
    /// Turn the comment at (page, index) into this kind ([`change_kind`]).
    kind_change: Option<(usize, usize, Markup)>,
    nav: Option<Nav>,
    mark: Option<MarkAction>,
    /// Copy this sidecar highlight as a callout.
    copy: Option<String>,
}

/// A change to a sidecar mark, carried out after the list is drawn.
enum MarkAction {
    Color(String, usize),
    Style(String, HighlightStyle),
    Delete(Pick),
    SaveNote(Pick, String),
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

/// What every card of the list shares.
struct Shared<'a> {
    t: &'a Tokens,
    notes: &'a Notes,
    keys: &'a Keys,
    /// The document takes comment changes.
    pdf_allowed: bool,
    /// Sidecar marks can be kept.
    marks_allowed: bool,
    /// Copy callout and Open in Obsidian are offered (Margin's notes are on).
    notes_on: bool,
    /// The PDF isn't in the vault, so callouts link to its file.
    external: bool,
    /// Why there is no reading note to open (no vault, a bad template), if there is none.
    no_note: Option<&'a str>,
    /// This year (the session's clock): its dates leave the year out.
    this_year: i64,
}

/// What one card shows besides the view's selection.
struct CardInput<'a> {
    shared: &'a Shared<'a>,
    row: &'a Row,
    page_label: String,
}

impl CardInput<'_> {
    /// The mark's note can be edited.
    fn allowed(&self) -> bool {
        match self.row.entry {
            Entry::Pdf(..) => self.shared.pdf_allowed,
            Entry::Mark(_) => self.shared.marks_allowed,
        }
    }

    /// The mark's colour can change and it can be deleted.
    fn editable(&self) -> bool {
        self.allowed() && !self.row.locked
    }

    /// Why Open in Obsidian is off, if it is.
    fn open_refused(&self) -> Option<&str> {
        match self.row.entry {
            Entry::Pdf(..) => self.shared.no_note,
            Entry::Mark(Pick::Highlight(_)) => Some(BESIDE_OPEN_TIP),
            Entry::Mark(Pick::Note(_)) => Some("A page note isn't a highlight, so it has no callout"),
        }
    }

    /// Copy callout applies: a highlight, with Margin's notes on.
    fn can_copy(&self) -> bool {
        self.shared.notes_on && !matches!(self.row.entry, Entry::Mark(Pick::Note(_)))
    }
}

/// The search, filters, draft note and cards of view `index`. Edits go to the undo stacks;
/// returns where to show the clicked mark. With no marks yet, `empty_lines` say how to make one,
/// inside the list under the notes box, and the search is off.
pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize, empty_lines: &[&str]) -> Option<Nav> {
    let keys = Keys::of(app);
    let this_year = app.session.today().0;
    let palette = palette_key(&app.notes);
    let view = app.views.get_mut(index)?;
    let doc = app.session.get(view.id)?;
    let info = &doc.info;
    view.margin.quotes.refresh(doc.edit_generation(), info, &view.texts);
    let mut list = std::mem::take(&mut view.margin.list);
    let source =
        Source { edit_generation: doc.edit_generation(), marks: view.margin.marks.generation(), quotes: view.margin.quotes.text.len(), palette };
    if list.source != Some(source) {
        list.rows = gather(info, &view.margin.quotes, &view.margin.marks, &app.notes);
        list.source = Some(source);
        list.shown_for = None;
    }
    ui.add_enabled_ui(list.total() > 0, |ui| controls(ui, t, &mut list, &app.notes));
    if list.shown_for.as_ref() != Some(&list.filter) {
        list.shown = select(&list.rows, &list.filter);
        list.shown_for = Some(list.filter.clone());
    }
    let mut out = Outcome::default();
    let draft_saved = draft(ui, t, info, view);
    let no_note = view.comments.notes.note.clone().err();
    let shared = Shared {
        t,
        notes: &app.notes,
        keys: &keys,
        pdf_allowed: doc.allows_annotation(),
        marks_allowed: view.margin.marks.refused().is_none(),
        notes_on: view.comments.notes.enabled,
        external: view.comments.notes.external,
        no_note: no_note.as_deref(),
        this_year,
    };
    let no_matches = list.shown.is_empty() && list.total() > 0;
    let no_marks = list.total() == 0 && view.margin.marks.draft.is_none();
    egui::ScrollArea::vertical().id_salt(("margin-highlights", view.id.0)).auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
        let origin = ui.max_rect().min;
        let width = ui.available_width();
        obsidian::panel_block(ui, t, &mut view.comments.notes);
        if no_matches {
            super::sidebar::empty(ui, t, &["No highlights match."]);
        } else if no_marks {
            super::sidebar::empty(ui, t, empty_lines);
        }
        let mut top = ui.min_rect().bottom() - origin.y;
        for &row_index in &list.shown {
            let Some(row) = list.rows.get(row_index) else { continue };
            let height = list.heights.get(&row.entry).copied().unwrap_or(CARD_ESTIMATE);
            let shown = top + height >= viewport.top() - OVERSCAN && top <= viewport.bottom() + OVERSCAN;
            if shown || must_draw(view, &row.entry) {
                let page_label = info.pages.get(row.page).map_or_else(|| row.page.saturating_add(1).to_string(), |p| p.label.clone());
                let input = CardInput { shared: &shared, row, page_label };
                let at = Rect::from_min_size(origin + vec2(0.0, top), vec2(width, f32::INFINITY));
                let mut child = ui.new_child(UiBuilder::new().max_rect(at).layout(Layout::top_down(Align::Min)));
                card(&mut child, &input, view, &mut out);
                let measured = child.min_rect().height();
                if (measured - height).abs() > 0.5 {
                    list.heights.insert(row.entry.clone(), measured);
                    ui.ctx().request_repaint();
                }
                top += measured + CARD_SPACING;
            } else {
                top += height + CARD_SPACING;
            }
        }
        ui.expand_to_include_rect(Rect::from_min_size(origin, vec2(width, top.max(0.0))));
    });
    view.margin.list = list;
    if draft_saved {
        marks::save_draft(app, index);
    }
    if let Some(edit) = out.edit {
        apply(app, edit);
    }
    if let Some((page, at, kind)) = out.kind_change {
        change_kind(app, index, page, at, kind);
    }
    if let Some(action) = out.mark {
        match action {
            MarkAction::Color(id, i) => {
                if let Some(rgb) = app.notes.colors.get(i).map(obsidian::NoteColor::rgb_f64) {
                    marks::recolor(app, index, &id, rgb);
                }
            }
            MarkAction::Style(id, style) => marks::restyle(app, index, &id, style),
            MarkAction::Delete(pick) => marks::delete(app, index, &pick),
            MarkAction::SaveNote(pick, text) => marks::set_note(app, index, &pick, text),
        }
    }
    if let Some(id) = out.copy {
        copy_callout(app, ui.ctx(), index, &id);
    }
    out.nav
}

/// Whether a card is laid out even out of view: its note is being typed (so the field keeps the
/// keyboard), or it is selected and waiting to be scrolled to.
fn must_draw(view: &DocView, entry: &Entry) -> bool {
    match entry {
        Entry::Pdf(page, index) => {
            let key = (*page, *index);
            view.comments.editing.as_ref().is_some_and(|(p, i, _)| (*p, *i) == key) || (view.comments.reveal && view.comments.selected == Some(key))
        }
        Entry::Mark(pick) => {
            let marks = &view.margin.marks;
            marks.editing.as_ref().is_some_and(|(editing, _)| editing == pick) || (marks.reveal && marks.selected.as_ref() == Some(pick))
        }
    }
}

/// The search field (with how many cards the filters leave, and a clear button, inside it on the
/// right while a filter is on) and the filter button, on one compact row that never changes
/// height.
fn controls(ui: &mut egui::Ui, t: &Tokens, list: &mut List, notes: &Notes) {
    let id = egui::Id::new("margin-highlights-search");
    let filtering = !list.filter.query.trim().is_empty() || list.filter.narrows();
    let shown = filtering.then(|| {
        list.shown_for.as_ref().filter(|shown| **shown == list.filter).map_or_else(|| select(&list.rows, &list.filter).len(), |_| list.shown.len())
    });
    let mut clear = false;
    ui.allocate_ui_with_layout(vec2(ui.available_width(), look::BUTTON_HEIGHT), Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        let field_width = (ui.available_width() - FILTER_BUTTON - GAP).max(FILTER_BUTTON);
        look::field_well(ui, id, vec2(field_width, look::BUTTON_HEIGHT), |ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            let (icon, _) = ui.allocate_exact_size(vec2(SEARCH_ICON, SEARCH_ICON), Sense::hover());
            icons::paint(ui, icon, "search", SEARCH_ICON, t.text_muted);
            let trailing = match shown {
                Some(_) => look::template_width(ui, COUNT_TEMPLATE, theme::regular(META_TEXT)) + look::SMALL_BUTTON + 2.0 * GAP,
                None => 0.0,
            };
            let field = egui::TextEdit::singleline(&mut list.filter.query)
                .id(id)
                .frame(egui::Frame::NONE)
                .margin(Vec2::ZERO)
                .hint_text("Search highlights and notes")
                .desired_width((ui.available_width() - trailing).max(0.0));
            ui.add(field);
            if let Some(shown) = shown {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    clear = look::icon_button(ui, "x", look::SMALL_BUTTON, false, "Clear filters", None).clicked();
                    let words = format!("{shown} of {}", list.total());
                    look::reserved_label(ui, &words, COUNT_TEMPLATE, theme::regular(META_TEXT), t.text_muted, Align::Max);
                });
            }
        });
        let button = look::icon_button(ui, "filter", FILTER_BUTTON, list.filter.narrows(), "Filter and sort", None);
        look::popover(&button, id.with("filters")).show(|ui| filter_menu(ui, t, &mut list.filter, notes));
    });
    if clear {
        list.filter = Filter { sort: list.filter.sort, ..Filter::default() };
    }
    ui.add_space(GAP);
}

/// The filter popover: one colour (click it again for all), one kind, and the order.
fn filter_menu(ui: &mut egui::Ui, t: &Tokens, filter: &mut Filter, notes: &Notes) {
    ui.set_width(FILTER_WIDTH);
    ui.spacing_mut().item_spacing.y = GAP;
    look::section_label(ui, t, "Colour", None);
    let dots: Vec<ColorDot<'_>> = notes
        .colors
        .iter()
        .enumerate()
        .map(|(i, c)| ColorDot {
            name: &c.name,
            color: Color32::from_rgb(c.rgb[0], c.rgb[1], c.rgb[2]),
            selected: filter.color == Some(i),
            shortcut: None,
        })
        .collect();
    if let Some(i) = look::color_dots(ui, &dots) {
        filter.color = if filter.color == Some(i) { None } else { Some(i) };
    }
    look::section_label(ui, t, "Kind", None);
    for kind in KindFilter::ALL {
        if look::menu_row(ui, None, kind.label(), None, filter.kind == kind).clicked() {
            filter.kind = kind;
        }
    }
    look::section_label(ui, t, "Sort", None);
    let segments = [
        look::Segment { label: "Page", selected: filter.sort == Sort::Page, tip: Some("In reading order") },
        look::Segment { label: "Newest", selected: filter.sort == Sort::Newest, tip: Some("Last changed first") },
    ];
    match look::segmented(ui, &segments) {
        Some(0) => filter.sort = Sort::Page,
        Some(_) => filter.sort = Sort::Newest,
        None => {}
    }
}

/// The page note being written (`note.add`), on a card above the list. Its height grows from
/// nothing as it appears and shrinks away when it goes (`look::collapsible`), so the list under
/// it slides instead of jumping. `true` when Save was pressed.
fn draft(ui: &mut egui::Ui, t: &Tokens, info: &DocInfo, view: &mut DocView) -> bool {
    let editor_id = egui::Id::new("margin-new-page-note");
    let open = view.margin.marks.draft.is_some();
    let outcome = look::collapsible(ui, editor_id.with("margin-draft-card"), open, |ui| {
        // While it slides away the note is gone: an empty card shrinks.
        let Some((NotePlace { page, .. }, _)) = view.margin.marks.draft else { return Edited::Typing };
        let label = info.pages.get(page).map_or_else(|| page.saturating_add(1).to_string(), |p| p.label.clone());
        let mut outcome = Edited::Typing;
        look::card_frame(t).inner_margin(GAP_LARGE).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(GAP, GAP);
            ui.horizontal(|ui| {
                ui.add(icons::image("sticky-note", KIND_ICON, t.text_muted));
                ui.label(egui::RichText::new(format!("New page note · p. {label}")).font(theme::regular(META_TEXT)).color(t.text_muted));
            });
            if let Some((_, text)) = view.margin.marks.draft.as_mut() {
                outcome = editor(ui, t, editor_id, text, "Write a note for this page");
            }
        });
        ui.add_space(GAP);
        outcome
    });
    match outcome.unwrap_or(Edited::Typing) {
        Edited::Typing => false,
        Edited::Saved => true,
        Edited::Cancelled => {
            view.margin.marks.draft = None;
            false
        }
    }
}

/// Runs [`DELETE_ID`] and [`COLOR_ID`] on the active document's selected mark (a sidecar mark
/// first, else the selected comment). `false` for any other id. Without a selected mark the key
/// is spent, as PdfCraft's view keys are.
pub(crate) fn run(app: &mut PdfCraftApp, id: &str, args: &Value) -> bool {
    if id != DELETE_ID && id != COLOR_ID {
        return false;
    }
    // A colour key while the selection bar is open marks or restyles the bar's text.
    if id == COLOR_ID && super::selection::color_key(app, args) {
        return true;
    }
    let Some((index, doc_id)) = app.active_ids() else { return true };
    let wanted = args.get("color").and_then(Value::as_str).unwrap_or_default().to_owned();
    let color = app.notes.colors.iter().find(|c| c.name.eq_ignore_ascii_case(&wanted)).map(obsidian::NoteColor::rgb_f64);
    if let Some(pick) = app.views.get(index).and_then(|view| view.margin.marks.selected.clone()) {
        match (id, pick, color) {
            (DELETE_ID, pick, _) => marks::delete(app, index, &pick),
            (_, Pick::Highlight(mark), Some(rgb)) => marks::recolor(app, index, &mark, rgb),
            (_, Pick::Highlight(_), None) => app.notify(format!("{COLOR_ID}: no colour named \"{wanted}\" in notes.colors")),
            (_, Pick::Note(_), _) => {}
        }
        return true;
    }
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
        let Some(rgb) = color else {
            app.notify(format!("{COLOR_ID}: no colour named \"{wanted}\" in notes.colors"));
            return true;
        };
        Edit::StyleAnnotation { page, index: annot, color: Some(rgb), opacity: None, width: None, endings: None }
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

/// Copy sidecar highlight `id` of view `index` as a callout.
fn copy_callout(app: &mut PdfCraftApp, ctx: &egui::Context, index: usize, id: &str) {
    let Some((page, text, rgb)) =
        app.views.get(index).and_then(|view| view.margin.marks.highlight(id)).and_then(|h| marks::callout_parts(&app.notes, h))
    else {
        return;
    };
    match obsidian::selection_callout(app, index, page, &text, Some(rgb)) {
        Ok(callout) => {
            ctx.copy_text(callout);
            app.notify("Copied callout");
        }
        Err(why) => super::toast::failed(app, why, None),
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

/// Whether `entry` is the selected mark of `view`.
fn is_selected(view: &DocView, entry: &Entry) -> bool {
    match entry {
        Entry::Pdf(page, index) => view.comments.selected == Some((*page, *index)),
        Entry::Mark(pick) => view.margin.marks.selected.as_ref() == Some(pick),
    }
}

/// Whether `entry`'s note is being typed.
fn is_editing(view: &DocView, entry: &Entry) -> bool {
    match entry {
        Entry::Pdf(page, index) => view.comments.editing.as_ref().is_some_and(|(p, i, _)| (*p, *i) == (*page, *index)),
        Entry::Mark(pick) => view.margin.marks.editing.as_ref().is_some_and(|(editing, _)| editing == pick),
    }
}

/// Select `entry` (one selection across the PDF's comments and the sidecar's marks).
fn select_entry(view: &mut DocView, entry: &Entry) {
    match entry {
        Entry::Pdf(page, index) => {
            view.comments.selected = Some((*page, *index));
            view.margin.marks.selected = None;
        }
        Entry::Mark(pick) => {
            view.margin.marks.selected = Some(pick.clone());
            view.comments.selected = None;
        }
    }
}

/// One mark's card (§2): the card fill and edge at rest, the hover fill and the hover edge under
/// the pointer, the selected fill and edge and the selected label colour on the quote when
/// selected. The whole card is one click target under its own widgets.
fn card(ui: &mut egui::Ui, input: &CardInput<'_>, view: &mut DocView, out: &mut Outcome) {
    let (t, row) = (input.shared.t, input.row);
    let selected = is_selected(view, &row.entry);
    let editing = is_editing(view, &row.entry);
    let mut action = None;
    let scope = ui.scope_builder(UiBuilder::new().id_salt(("margin-highlight", &row.entry)).sense(Sense::click()), |ui| {
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
                card_editor(ui, t, &row.entry, view, out);
            }
        });
    });
    let resp = scope.response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let colour = color_name(input.shared.notes, row.color);
    let label = match row.entry {
        Entry::Mark(Pick::Note(_)) => format!("Page note, p. {}: {}", input.page_label, row.note.as_deref().unwrap_or_default()),
        _ => format!("{colour} {}, p. {}: {}", row.label.to_lowercase(), input.page_label, row.quote),
    };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    resp.context_menu(|ui| {
        if let Some(picked) = menu(ui, input) {
            action = Some(picked);
        }
    });
    if selected && reveal_taken(view, &row.entry) {
        resp.scroll_to_me(Some(Align::Center));
    }
    if resp.clicked() {
        select_entry(view, &row.entry);
        out.nav = Some(Nav::Flash(row.page, row.flash));
    }
    if resp.double_clicked() && input.allowed() && !editing {
        action = Some(Action::EditNote);
    }
    if let Some(action) = action {
        act(ui.ctx(), action, input, view, out);
    }
}

/// Take the "scroll to the selected card" request for `entry`.
fn reveal_taken(view: &mut DocView, entry: &Entry) -> bool {
    match entry {
        Entry::Pdf(..) => std::mem::take(&mut view.comments.reveal),
        Entry::Mark(_) => std::mem::take(&mut view.margin.marks.reveal),
    }
}

/// Carry out a card's action: notes requests go to `margin::obsidian::tick`, PDF edits to the
/// undo stack, sidecar changes to `margin::marks` once the list is drawn.
fn act(ctx: &egui::Context, action: Action, input: &CardInput<'_>, view: &mut DocView, out: &mut Outcome) {
    let row = input.row;
    match (&row.entry, action) {
        (Entry::Pdf(page, index), Action::CopyCallout) => view.comments.notes.ask(ctx, Request::CopyCallout(*page, *index)),
        (Entry::Pdf(page, index), Action::OpenInObsidian) => view.comments.notes.ask(ctx, Request::Open(Some((*page, *index)))),
        (Entry::Pdf(page, index), Action::Color(i)) => {
            if let Some(color) = input.shared.notes.colors.get(i) {
                out.edit = Some(Edit::StyleAnnotation {
                    page: *page,
                    index: *index,
                    color: Some(color.rgb_f64()),
                    opacity: None,
                    width: None,
                    endings: None,
                });
            }
        }
        (Entry::Pdf(page, index), Action::EditNote) => view.comments.editing = Some((*page, *index, row.note.clone().unwrap_or_default())),
        (Entry::Pdf(page, index), Action::ChangeKind(kind)) => out.kind_change = Some((*page, *index, kind)),
        (Entry::Pdf(page, index), Action::Delete) => out.edit = Some(Edit::DeleteAnnotation { page: *page, index: *index }),
        (Entry::Mark(Pick::Highlight(id)), Action::CopyCallout) => out.copy = Some(id.clone()),
        (Entry::Mark(Pick::Highlight(id)), Action::Color(i)) => out.mark = Some(MarkAction::Color(id.clone(), i)),
        (Entry::Mark(Pick::Highlight(id)), Action::ChangeKind(kind)) => {
            let style = if kind == Markup::Underline { HighlightStyle::Underline } else { HighlightStyle::Highlight };
            out.mark = Some(MarkAction::Style(id.clone(), style));
        }
        (Entry::Mark(pick), Action::EditNote) => view.margin.marks.editing = Some((pick.clone(), row.note.clone().unwrap_or_default())),
        (Entry::Mark(pick), Action::Delete) => out.mark = Some(MarkAction::Delete(pick.clone())),
        (Entry::Mark(_), Action::OpenInObsidian | Action::CopyCallout | Action::Color(_) | Action::ChangeKind(_)) => {}
    }
}

/// The mark's colour as shown: its own, else the palette's first (the colour a callout gets).
fn shown_color(notes: &Notes, color: Option<[f32; 3]>) -> Color32 {
    match color {
        Some(c) => color32(c.map(f64::from)),
        None => notes.colors.first().map_or(Color32::GRAY, |c| Color32::from_rgb(c.rgb[0], c.rgb[1], c.rgb[2])),
    }
}

/// The palette name of a mark's colour ("Yellow"), or "Custom".
fn color_name(notes: &Notes, color: Option<[f32; 3]>) -> String {
    let named = match color {
        Some(c) => notes.color_named(c),
        None => notes.colors.first(),
    };
    named.map_or_else(|| "Custom".to_owned(), |c| c.name.clone())
}

/// Swatch (none for a page note), type icon, page and a short date ("10 Oct"); then the actions,
/// faded in by `actions_shown` (they keep their place, so nothing beside them moves on hover). A
/// date that doesn't fit beside the page label (the 240-pt sidebar, a long label) goes on a line
/// of its own under the label, never cut or left out; the page label's tooltip has the full
/// metadata, the date's its full timestamp.
fn meta_row(ui: &mut egui::Ui, input: &CardInput<'_>, actions_shown: f32) -> Option<Action> {
    let (t, row) = (input.shared.t, input.row);
    let font = theme::regular(META_TEXT);
    let date =
        row.date.as_deref().and_then(|full| ui.ctx().memory_mut(|m| m.caches.cache::<ShortDates>().get((full, input.shared.this_year)).clone()));
    let (action, date_below) = ui
        .allocate_ui_with_layout(vec2(ui.available_width(), META_ROW), Layout::left_to_right(Align::Center), |ui| {
            let (rect, swatch) = ui.allocate_exact_size(vec2(SWATCH, SWATCH), Sense::hover());
            if row.subtype != NOTE_SUBTYPE {
                let colour = color_name(input.shared.notes, row.color);
                swatch.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("Colour: {colour}")));
                let edge = Stroke::new(1.0, Color32::from_black_alpha(SWATCH_EDGE_ALPHA));
                ui.painter().circle(rect.center(), SWATCH / 2.0, shown_color(input.shared.notes, row.color), edge);
            }
            let icon = if row.subtype == NOTE_SUBTYPE { "sticky-note" } else { subtype_icon(row.subtype) };
            look::tooltip(ui.add(icons::image(icon, KIND_ICON, t.text_muted)), row.label, None);
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
                let kept = if matches!(row.entry, Entry::Mark(_)) { " · kept beside the PDF" } else { "" };
                let full = match &row.date {
                    Some(date) => format!("Page {} · {}{kept} · {date}", input.page_label, row.label),
                    None => format!("Page {} · {}{kept}", input.page_label, row.label),
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
    let shown = ui.add(egui::Label::new(egui::RichText::new(date).font(font).color(input.shared.t.text_faint)).selectable(false));
    if let Some(full) = &input.row.date {
        look::tooltip(shown, full, None);
    }
}

/// The hover actions, left to right: Copy callout, Open in Obsidian, Change colour, Delete.
fn actions(ui: &mut egui::Ui, input: &CardInput<'_>) -> Option<Action> {
    let row = input.row;
    let mut action = None;
    ui.add_enabled_ui(input.can_copy(), |ui| {
        let copy_tip = if input.shared.external { obsidian::EXTERNAL_TIP } else { "Copy callout" };
        if look::icon_button(ui, "copy", ACTION, false, copy_tip, None).clicked() {
            action = Some(Action::CopyCallout);
        }
    });
    ui.add_enabled_ui(input.shared.notes_on, |ui| {
        let (open_tip, can_open) = match input.open_refused() {
            Some(why) => (format!("Open in Obsidian: {why}"), false),
            None => ("Open in Obsidian".to_owned(), true),
        };
        if ui.add_enabled_ui(can_open, |ui| look::icon_button(ui, "external-link", ACTION, false, &open_tip, None)).inner.clicked() {
            action = Some(Action::OpenInObsidian);
        }
    });
    ui.add_enabled_ui(input.editable(), |ui| {
        if row.subtype == NOTE_SUBTYPE {
            ui.add_space(ACTION);
        } else {
            let colour = look::color_button(ui, ACTION, shown_color(input.shared.notes, row.color), false, "Change colour", None);
            look::popover(&colour, egui::Id::new(("margin-highlight-colour", &row.entry))).show(|ui| {
                if let Some(i) = color_row(ui, input) {
                    action = Some(Action::Color(i));
                }
            });
        }
        let tip = if row.subtype == NOTE_SUBTYPE { "Delete note" } else { "Delete highlight" };
        if look::icon_button(ui, "trash-2", ACTION, false, tip, input.shared.keys.delete.as_deref()).clicked() {
            action = Some(Action::Delete);
        }
    });
    action
}

/// The `notes.colors` palette as a row of dots, the mark's own ringed; returns the clicked
/// colour's index.
fn color_row(ui: &mut egui::Ui, input: &CardInput<'_>) -> Option<usize> {
    let notes = input.shared.notes;
    let dots: Vec<ColorDot<'_>> = notes
        .colors
        .iter()
        .enumerate()
        .map(|(i, c)| ColorDot {
            name: &c.name,
            color: Color32::from_rgb(c.rgb[0], c.rgb[1], c.rgb[2]),
            selected: input.row.palette == Some(i),
            shortcut: input.shared.keys.colors.get(i).and_then(Option::as_deref),
        })
        .collect();
    look::color_dots(ui, &dots)
}

/// The right-click menu: what the mark is, its colours, Change to underline (or highlight),
/// the notes actions, its note and Delete with its keys.
fn menu(ui: &mut egui::Ui, input: &CardInput<'_>) -> Option<Action> {
    let (t, row) = (input.shared.t, input.row);
    look::menu_body(ui);
    let title = match row.entry {
        Entry::Mark(Pick::Note(_)) => format!("Page note · p. {}", input.page_label),
        _ => format!("{} {} · p. {}", color_name(input.shared.notes, row.color), row.label.to_lowercase(), input.page_label),
    };
    ui.label(egui::RichText::new(title).font(theme::semibold(META_TEXT)).color(t.text_faint));
    let mut action = None;
    if input.editable()
        && row.subtype != NOTE_SUBTYPE
        && let Some(i) = color_row(ui, input)
    {
        action = Some(Action::Color(i));
    }
    if let Some(kind) = row.other_kind() {
        let refused = if input.allowed() { row.kind_refused } else { Some(NOT_ALLOWED) };
        let label = if kind == Markup::Underline { "Change to underline" } else { "Change to highlight" };
        let change = ui.add_enabled_ui(refused.is_none(), |ui| look::menu_row(ui, Some("replace"), label, None, false)).inner;
        let change = match refused {
            Some(why) => look::tooltip(change, &format!("{label}: {why}"), None),
            None => change,
        };
        if change.clicked() {
            action = Some(Action::ChangeKind(kind));
        }
    }
    ui.separator();
    if input.can_copy() {
        let copy = look::menu_row(ui, Some("copy"), "Copy callout", None, false);
        let copy = if input.shared.external { look::tooltip(copy, obsidian::EXTERNAL_TIP, None) } else { copy };
        if copy.clicked() {
            action = Some(Action::CopyCallout);
        }
    }
    if input.shared.notes_on && row.subtype != NOTE_SUBTYPE {
        let refused = input.open_refused();
        let open = ui.add_enabled_ui(refused.is_none(), |ui| look::menu_row(ui, Some("external-link"), "Open in Obsidian", None, false)).inner;
        let tip = refused.map_or_else(|| "Add this highlight to the reading note and open it in Obsidian".to_owned(), str::to_owned);
        if look::tooltip(open, &tip, None).clicked() {
            action = Some(Action::OpenInObsidian);
        }
    }
    let has_note = row.note.as_deref().is_some_and(|c| !c.trim().is_empty());
    let note_label = if has_note { "Edit note" } else { "Add note" };
    if ui.add_enabled_ui(input.allowed(), |ui| look::menu_row(ui, Some("pen-line"), note_label, None, false)).inner.clicked() {
        action = Some(Action::EditNote);
    }
    ui.separator();
    let delete_keys = input.shared.keys.delete.as_deref();
    if ui.add_enabled_ui(input.editable(), |ui| look::menu_row(ui, Some("trash-2"), "Delete", delete_keys, false)).inner.clicked() {
        action = Some(Action::Delete);
    }
    if action.is_some() {
        ui.close();
    }
    action
}

/// The quote (three lines until "Show more" is clicked) and the note. Without a quote, the note
/// alone; with neither, the mark's type in muted italics. A page note's text is its body.
/// Selecting a card never grows it: only its own "Show more" does, and only the cards below it
/// move.
fn body(ui: &mut egui::Ui, input: &CardInput<'_>, selected: bool, editing: bool) {
    let (t, row) = (input.shared.t, input.row);
    let quote = row.quote.trim();
    let note = row.note.as_deref().map(str::trim).filter(|note| !note.is_empty() && *note != quote);
    if row.subtype == NOTE_SUBTYPE {
        if let Some(note) = note.filter(|_| !editing) {
            let color = look::row_text(t, selected, t.text);
            text_block(ui, note, TextFormat { font_id: theme::regular(QUOTE_TEXT), color, ..Default::default() }, usize::MAX);
        }
        return;
    }
    if !quote.is_empty() {
        let expanded_id = ui.id().with("margin-quote-expanded");
        let expanded = ui.data(|d| d.get_temp::<bool>(expanded_id)).unwrap_or(false);
        let rows = if expanded { usize::MAX } else { QUOTE_ROWS };
        let color = look::row_text(t, selected, t.text);
        let cut = text_block(ui, quote, TextFormat { font_id: theme::regular(QUOTE_TEXT), color, ..Default::default() }, rows);
        if (cut || expanded) && look::button_ghost(ui, look::ButtonSize::Compact, if expanded { "Show less" } else { "Show more" }).clicked() {
            ui.data_mut(|d| d.insert_temp(expanded_id, !expanded));
        }
    } else if note.is_none() && !editing {
        let format = TextFormat { font_id: theme::regular(QUOTE_TEXT), color: t.text_faint, italics: true, ..Default::default() };
        text_block(ui, row.label, format, 1);
    }
    if let Some(note) = note.filter(|_| !editing) {
        text_block(ui, note, TextFormat { font_id: theme::regular(NOTE_TEXT), color: t.text_muted, italics: true, ..Default::default() }, usize::MAX);
    }
}

/// Text wrapped to the card's width, cut to `rows` lines with an ellipsis. True when it was cut.
fn text_block(ui: &mut egui::Ui, text: &str, format: TextFormat, rows: usize) -> bool {
    let mut job = LayoutJob::single_section(text.to_owned(), format);
    job.wrap = TextWrapping { max_width: ui.available_width(), max_rows: rows, break_anywhere: false, overflow_character: Some('…') };
    let galley = ui.painter().layout_job(job);
    let cut = galley.elided;
    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
    ui.painter().galley(rect.min, galley, Color32::PLACEHOLDER);
    cut
}

/// What the reader did with a note editor this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edited {
    Typing,
    Saved,
    Cancelled,
}

/// A note editor: the text field (focused when it appears), Cancel and Save under it on the
/// right. Esc cancels.
fn editor(ui: &mut egui::Ui, t: &Tokens, id: egui::Id, text: &mut String, hint: &str) -> Edited {
    // Focus it when it appears, not every frame: a press on the page takes the keys back, so a
    // note left open never swallows V, H or 1-4 meant for the reader.
    let appearing = ui.ctx().read_response(id).is_none();
    let field = egui::TextEdit::multiline(text)
        .desired_rows(EDITOR_ROWS)
        .desired_width(ui.available_width())
        .margin(EDITOR_MARGIN)
        .font(theme::regular(NOTE_TEXT))
        .text_color(t.text)
        .hint_text(hint)
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
    if save {
        Edited::Saved
    } else if cancel || (resp.has_focus() || resp.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        Edited::Cancelled
    } else {
        Edited::Typing
    }
}

/// The note being typed into a card: saved to the PDF comment or the sidecar mark on Save.
fn card_editor(ui: &mut egui::Ui, t: &Tokens, entry: &Entry, view: &mut DocView, out: &mut Outcome) {
    let id = ui.make_persistent_id(("margin-highlight-note", entry));
    let text = match entry {
        Entry::Pdf(..) => view.comments.editing.as_mut().map(|(_, _, text)| text),
        Entry::Mark(_) => view.margin.marks.editing.as_mut().map(|(_, text)| text),
    };
    let Some(text) = text else { return };
    match (editor(ui, t, id, text, "Add a note"), entry) {
        (Edited::Typing, _) => {}
        (Edited::Saved, Entry::Pdf(..)) => {
            if let Some((page, index, text)) = view.comments.editing.take() {
                out.edit = Some(Edit::SetAnnotationContents { page, index, text });
            }
        }
        (Edited::Saved, Entry::Mark(_)) => {
            if let Some((pick, text)) = view.margin.marks.editing.take() {
                out.mark = Some(MarkAction::SaveNote(pick, text));
            }
        }
        (Edited::Cancelled, Entry::Pdf(..)) => view.comments.editing = None,
        (Edited::Cancelled, Entry::Mark(_)) => view.margin.marks.editing = None,
    }
}

/// The months as a card's short date names them.
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Short dates for the cards, each formatted once while it is shown (egui's frame cache, keyed
/// by the date as the card holds it).
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
    use super::*;

    #[test]
    fn short_dates_drop_this_year_and_refuse_what_they_cannot_read() {
        assert_eq!(short_date("2026-10-10 12:30", 2026).as_deref(), Some("10 Oct"));
        assert_eq!(short_date("2025-01-05 09:00", 2026).as_deref(), Some("5 Jan 2025"));
        assert_eq!(short_date("2026-10-10", 2026).as_deref(), Some("10 Oct"));
        for unreadable in ["Tuesday", "2026-13-01 00:00", "2026-00-10", "2026-10-32", "D:2026101012", "", "20261010"] {
            assert_eq!(short_date(unreadable, 2026), None, "{unreadable}");
        }
    }

    fn row(entry: Entry, page: usize, subtype: &'static str, palette: Option<usize>, quote: &str, note: Option<&str>, date: &str) -> Row {
        Row {
            entry,
            page,
            at: (0.0, 0.0),
            subtype,
            label: subtype,
            color: None,
            palette,
            quote: quote.to_owned(),
            note: note.map(str::to_owned),
            date: Some(card_date(date)),
            flash: [0.0; 4],
            locked: false,
            kind_refused: None,
            haystack: haystack(quote, note),
        }
    }

    #[test]
    fn search_filters_and_sort_pick_the_cards() {
        let rows = vec![
            row(Entry::Pdf(0, 0), 0, "Highlight", Some(0), "The Quick brown fox", None, "2026-10-01 10:00"),
            row(Entry::Mark(Pick::Highlight("a".into())), 1, "Underline", Some(1), "lazy dog", Some("check the FOX"), "2026-10-09T08:00:00Z"),
            row(Entry::Mark(Pick::Note("n".into())), 2, NOTE_SUBTYPE, None, "", Some("chapter summary"), "2026-10-05T08:00:00Z"),
        ];
        let all = Filter::default();
        assert_eq!(select(&rows, &all), [0, 1, 2]);
        let fox = Filter { query: "  fox ".into(), ..Filter::default() };
        assert_eq!(select(&rows, &fox), [0, 1], "quote and note match, case-insensitively");
        let red = Filter { color: Some(1), ..Filter::default() };
        assert_eq!(select(&rows, &red), [1]);
        let notes = Filter { kind: KindFilter::Notes, ..Filter::default() };
        assert_eq!(select(&rows, &notes), [2]);
        let newest = Filter { sort: Sort::Newest, ..Filter::default() };
        assert_eq!(select(&rows, &newest), [1, 2, 0], "RFC 3339 and PdfCraft dates sort together");
    }
}
