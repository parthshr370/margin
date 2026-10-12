//! Highlights and page notes kept in the document's sidecar (`<file>.margin.json`, or Margin's
//! data folder when the PDF's folder can't be written), not in the PDF.
//!
//! Text marks go to the sidecar when Margin can't write the PDF: the file or its folder is
//! read-only, PdfCraft opened it read-only, it forbids comment changes, or it is signed (Margin
//! never writes a signed file unasked). [`take_edit`] catches every way a text mark is made
//! (the selection bar, the H/U/S/Q keys, the rail's tools) before it reaches the engine. Page
//! notes always live in the sidecar, for every PDF.
//!
//! The marks are drawn over the page by [`paint`], listed in the Highlights tab and dotted in
//! the Pages tab and the footer like the PDF's own. Every change is one step of a history kept
//! beside the engine's: [`step`] runs `edit.undo` and `edit.redo` on whichever came last,
//! using the engine's undo depth at the time of each step. A deletion leaves a tombstone, so a
//! second writer of the sidecar merges instead of bringing it back
//! (`margin_config::marks::merge`). "Save a copy with highlights" ([`SAVE_COPY_ID`]) writes them
//! into a new PDF as real annotations.

use egui::{Color32, Pos2, Rect, Stroke, vec2};
use margin_config::marks as records;
use margin_config::{Highlight, HighlightSource, HighlightStyle, Locator, PageNote, Sidecar, store};
use pdfcraft_engine::{Document, Edit, Markup, NewAnnotation, Shape, Style};
use pdfcraft_render::{Annotation, PageInfo};
use serde_json::Map;

use super::obsidian::Notes;
use super::{SidebarTab, reading};
use crate::canvas::{DocView, PageXform};
use crate::comments::CommentTool;
use crate::theme::Tokens;
use crate::{Flavor, PdfCraftApp, icons};

/// "Save a copy with highlights": the sidecar's highlights and notes in a new PDF.
pub const SAVE_COPY_ID: &str = "reader.save_with_highlights";
/// Add a page note at the reading position (or beside the selected text).
pub const NOTE_ADD_ID: &str = "note.add";
/// Edit the selected page note or highlight's note.
pub const NOTE_EDIT_ID: &str = "note.edit";
/// Delete the selected page note.
pub const NOTE_DELETE_ID: &str = "note.delete";
/// Go to the next page note after the reading position, or the previous one.
pub const NOTE_NEXT_ID: &str = "note.next";
pub const NOTE_PREV_ID: &str = "note.prev";
/// Show the page notes in the Highlights tab.
pub const NOTE_LIST_ID: &str = "note.list";

/// Steps of the sidecar history kept per document.
const UNDO_STEPS: usize = 200;
/// A highlight's fill over the page: the colour at this opacity.
const HIGHLIGHT_ALPHA: f32 = 0.38;
/// Underline, strikethrough and squiggle strokes: this share of the line's height, at least
/// [`MIN_STROKE`] points on screen.
const STROKE_SHARE: f32 = 0.08;
const MIN_STROKE: f32 = 1.0;
/// A squiggle's wavelength as a share of the line's height, and its height as a share of that.
const SQUIGGLE_WAVE: f32 = 0.5;
const SQUIGGLE_HEIGHT: f32 = 0.12;
/// A selected mark's outline.
const SELECTED_STROKE: f32 = 1.5;
/// A page note's marker on the page: its side, and its icon inside it.
const NOTE_MARKER: f32 = 20.0;
const NOTE_ICON: f32 = 13.0;
/// A new page note goes this far across the page (the right margin), and this share of the
/// page below the window's top edge, so its marker shows.
const NOTE_ACROSS: f64 = 0.92;
const NOTE_DROP: f64 = 0.03;
/// A jump to a note shows this share of the page above it.
const JUMP_LEAD: f32 = 0.05;
/// The fallback highlight colour when `notes.colors` is empty (PDF++ yellow).
const FALLBACK_RGB: [f32; 3] = [1.0, 0.816, 0.0];

/// Why a document's text marks are kept in its sidecar instead of the PDF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Beside {
    /// The PDF file is read-only.
    FileReadOnly,
    /// The PDF's folder can't be written, so a save can't replace the file.
    FolderReadOnly,
    /// PdfCraft opened it read-only (`Document::read_only_reason`).
    Opened,
    /// The PDF forbids comment changes.
    NoPermission,
    /// The PDF is signed: Margin never writes it unasked.
    Signed,
}

impl Beside {
    /// The reason, for a sentence: "This PDF is read-only, so …".
    pub fn why(self) -> &'static str {
        match self {
            Self::FileReadOnly | Self::FolderReadOnly | Self::Opened => "This PDF is read-only",
            Self::NoPermission => "This PDF doesn't allow changes to its comments",
            Self::Signed => "This PDF is signed",
        }
    }

    /// Whether the PDF can't be written where it is (the footer's read-only state).
    pub fn file_unwritable(self) -> bool {
        matches!(self, Self::FileReadOnly | Self::FolderReadOnly)
    }
}

/// A sidecar mark: a highlight or a page note, by id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Pick {
    Highlight(String),
    Note(String),
}

/// Where a new page note goes: its page and its marker's top-left corner as shares of the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NotePlace {
    pub page: usize,
    pub at: [f64; 2],
}

/// One step of the sidecar history: a record before and after.
#[derive(Clone, Debug)]
enum Change {
    Highlight { id: String, before: Option<Box<Highlight>>, after: Option<Box<Highlight>> },
    Note { id: String, before: Option<Box<PageNote>>, after: Option<Box<PageNote>> },
}

#[derive(Clone, Debug)]
struct Step {
    label: &'static str,
    change: Change,
    /// The engine's undo depth when the step was taken: the steps interleave by it.
    depth: usize,
}

/// One document's sidecar marks. Lives in its `DocReading`; inert in PdfCraft.
#[derive(Default)]
pub struct Marks {
    beside: Option<Beside>,
    /// Why nothing can be kept: the document has no file, or its sidecar must not be
    /// overwritten (a newer Margin wrote it, or it is unreadable).
    refused: Option<String>,
    /// Sidecar-only highlights.
    highlights: Vec<Highlight>,
    notes: Vec<PageNote>,
    /// Tombstones of deleted highlights, notes and bookmarks.
    deleted: Vec<String>,
    /// Bumped on every change, for the sidecar writer and the lists' caches.
    generation: u64,
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// The mark picked on the page or in the Highlights tab.
    pub(crate) selected: Option<Pick>,
    /// The Highlights tab scrolls the selected mark's card into view once.
    pub(crate) reveal: bool,
    /// The note being typed into a card.
    pub(crate) editing: Option<(Pick, String)>,
    /// A new page note being typed, not kept until saved.
    pub(crate) draft: Option<(NotePlace, String)>,
    /// The reader has been told once that highlights are kept beside the file.
    told: bool,
    /// Indices of `highlights` per page, for drawing, as of `indexed`.
    by_page: std::collections::BTreeMap<usize, Vec<usize>>,
    indexed: Option<u64>,
}

impl Marks {
    /// The sidecar's marks, as read on open (`file`), kept beside a PDF for `beside`.
    pub(crate) fn load(file: Option<&Sidecar>, beside: Option<Beside>, refused: Option<String>) -> Self {
        let mut marks = Self { beside, refused, ..Self::default() };
        if let Some(file) = file {
            marks.highlights = file.highlights.iter().filter(|h| records::sidecar_only(h)).take(records::MAX_HIGHLIGHTS).cloned().collect();
            marks.notes = file.notes.iter().take(records::MAX_NOTES).cloned().collect();
            marks.deleted = file.deleted.iter().rev().take(records::MAX_TOMBSTONES).rev().cloned().collect();
        }
        marks
    }

    /// Why text marks go to the sidecar, if they do.
    pub fn beside(&self) -> Option<Beside> {
        self.beside
    }

    /// Why nothing can be kept in the sidecar, if so.
    pub fn refused(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    pub fn highlights(&self) -> &[Highlight] {
        &self.highlights
    }

    pub fn notes(&self) -> &[PageNote] {
        &self.notes
    }

    /// Changes with every edit of the marks (and of bookmark tombstones).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn highlight(&self, id: &str) -> Option<&Highlight> {
        self.highlights.iter().find(|h| h.id == id)
    }

    pub fn note(&self, id: &str) -> Option<&PageNote> {
        self.notes.iter().find(|n| n.id == id)
    }

    /// Whether anything is kept for "Save a copy with highlights".
    pub fn has_marks(&self) -> bool {
        !self.highlights.is_empty() || !self.notes.is_empty()
    }

    /// A bookmark was removed: remember it, so a second writer's copy doesn't bring it back.
    pub(crate) fn bookmark_removed(&mut self, id: &str) {
        records::add_tombstone(&mut self.deleted, id);
        self.generation = self.generation.wrapping_add(1);
    }

    /// Nothing more can be kept: the sidecar must be left alone from now on.
    pub(crate) fn refuse(&mut self, reason: String) {
        self.refused = Some(reason);
    }

    /// Put the marks into `out`, the sidecar about to be written: its highlights backed by PDF
    /// annotations stay, the sidecar-only ones are these.
    pub(crate) fn write_into(&self, out: &mut Sidecar) {
        out.highlights.retain(|h| !records::sidecar_only(h));
        out.highlights.extend(self.highlights.iter().cloned());
        out.notes = self.notes.clone();
        out.deleted = self.deleted.clone();
    }

    /// Take the marks of `merged`, the sidecar as merged with another writer's copy.
    pub(crate) fn adopt(&mut self, merged: &Sidecar) {
        self.highlights = merged.highlights.iter().filter(|h| records::sidecar_only(h)).cloned().collect();
        self.notes = merged.notes.clone();
        self.deleted = merged.deleted.clone();
        self.generation = self.generation.wrapping_add(1);
        let selected_gone = match &self.selected {
            Some(Pick::Highlight(id)) => self.highlight(id).is_none(),
            Some(Pick::Note(id)) => self.note(id).is_none(),
            None => false,
        };
        if selected_gone {
            self.selected = None;
        }
    }

    fn put_highlight(&mut self, id: &str, value: Option<Highlight>) {
        let at = self.highlights.iter().position(|h| h.id == id);
        match (value, at) {
            (Some(value), Some(at)) => {
                if let Some(slot) = self.highlights.get_mut(at) {
                    *slot = value;
                }
            }
            (Some(value), None) => {
                records::remove_tombstone(&mut self.deleted, id);
                self.highlights.push(value);
            }
            (None, Some(at)) => {
                self.highlights.remove(at);
                records::add_tombstone(&mut self.deleted, id);
                if self.selected == Some(Pick::Highlight(id.to_owned())) {
                    self.selected = None;
                }
            }
            (None, None) => {}
        }
        self.generation = self.generation.wrapping_add(1);
    }

    fn put_note(&mut self, id: &str, value: Option<PageNote>) {
        let at = self.notes.iter().position(|n| n.id == id);
        match (value, at) {
            (Some(value), Some(at)) => {
                if let Some(slot) = self.notes.get_mut(at) {
                    *slot = value;
                }
            }
            (Some(value), None) => {
                records::remove_tombstone(&mut self.deleted, id);
                self.notes.push(value);
            }
            (None, Some(at)) => {
                self.notes.remove(at);
                records::add_tombstone(&mut self.deleted, id);
                if self.selected == Some(Pick::Note(id.to_owned())) {
                    self.selected = None;
                }
            }
            (None, None) => {}
        }
        self.generation = self.generation.wrapping_add(1);
    }

    /// Apply `change` forwards (its `after`) or backwards (its `before`).
    fn apply(&mut self, change: &Change, forward: bool) {
        match change {
            Change::Highlight { id, before, after } => {
                let value = if forward { after } else { before };
                self.put_highlight(id, value.as_deref().cloned());
            }
            Change::Note { id, before, after } => {
                let value = if forward { after } else { before };
                self.put_note(id, value.as_deref().cloned());
            }
        }
    }

    /// Take `change` as a new step: apply it, and forget the way forward.
    fn record(&mut self, label: &'static str, change: Change, depth: usize) {
        self.apply(&change, true);
        self.undo.push(Step { label, change, depth });
        if self.undo.len() > UNDO_STEPS {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// The highlights on `page`, as indices into [`Self::highlights`].
    fn on_page(&mut self, page: usize) -> &[usize] {
        if self.indexed != Some(self.generation) {
            self.by_page.clear();
            for (i, h) in self.highlights.iter().enumerate() {
                if let Some(page) = records::locator_page(&h.locator) {
                    self.by_page.entry(page).or_default().push(i);
                }
            }
            self.indexed = Some(self.generation);
        }
        self.by_page.get(&page).map_or(&[], Vec::as_slice)
    }
}

/// Whether the file at `path` or its folder is read-only, so a save can't replace it.
pub(crate) fn path_unwritable(path: &std::path::Path) -> bool {
    file_read_only(path) || folder_read_only(path)
}

fn file_read_only(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.permissions().readonly())
}

fn folder_read_only(path: &std::path::Path) -> bool {
    path.parent().filter(|dir| !dir.as_os_str().is_empty()).is_some_and(file_read_only)
}

/// Why `doc`, opened from `path`, keeps its text marks in the sidecar, if it does.
pub(crate) fn beside_reason(doc: &Document, path: &std::path::Path) -> Option<Beside> {
    if doc.read_only_reason.is_some() || !doc.editable() {
        Some(Beside::Opened)
    } else if !doc.allows_annotation() {
        Some(Beside::NoPermission)
    } else if file_read_only(path) {
        Some(Beside::FileReadOnly)
    } else if folder_read_only(path) {
        Some(Beside::FolderReadOnly)
    } else if doc.is_signed() {
        Some(Beside::Signed)
    } else {
        None
    }
}

/// The sidecar marks of view `index` (empty in PdfCraft): Obsidian's and the lists' data.
pub fn sidecar_highlights(view: &DocView) -> &[Highlight] {
    view.margin.marks.highlights()
}

pub fn page_notes(view: &DocView) -> &[PageNote] {
    view.margin.marks.notes()
}

/// Changes with every sidecar mark edit of `view`.
pub fn generation(view: &DocView) -> u64 {
    view.margin.marks.generation()
}

/// The colour a sidecar highlight shows: its `notes.colors` entry by name, else the colour it was
/// made in, else the palette's first.
pub(crate) fn shown_rgb(notes: &Notes, highlight: &Highlight) -> [f32; 3] {
    let named = notes.colors.iter().find(|c| c.name.eq_ignore_ascii_case(&highlight.color)).map(|c| c.rgb.map(|v| f32::from(v) / 255.0));
    let made = || highlight.hex.as_deref().and_then(records::hex_rgb).map(|rgb| rgb.map(|v| v as f32));
    let first = || notes.colors.first().map(|c| c.rgb.map(|v| f32::from(v) / 255.0));
    named.or_else(made).or_else(first).unwrap_or(FALLBACK_RGB)
}

/// `rgb` as a highlight's `color` (a `notes.colors` name, lower case, or "custom") and `hex`.
fn color_fields(notes: &Notes, rgb: [f64; 3]) -> (String, String) {
    let name = notes.color_named(rgb.map(|v| v as f32)).map_or_else(|| "custom".to_owned(), |c| c.name.to_lowercase());
    (name, records::rgb_hex(rgb))
}

/// The outline section `page` is in, for a highlight's `section`.
fn section_at(doc: &Document, page: usize) -> Option<String> {
    let outline = &doc.info.outline;
    super::current_section(outline, page).and_then(|path| super::outline_at(outline, &path)).map(|item| item.title.trim().to_owned())
}

/// A view-space rectangle as a view quad (top-left, top-right, bottom-left, bottom-right).
pub(crate) fn rect_quad([x0, y0, x1, y1]: [f32; 4]) -> [f64; 8] {
    [x0, y0, x1, y0, x0, y1, x1, y1].map(f64::from)
}

/// A user-space quad (as edits carry them) in view space.
fn view_quad(page: &PageInfo, quad: &[f64; 8]) -> [f64; 8] {
    let mut out = [0.0; 8];
    for corner in 0..4 {
        let (x, y) = (quad.get(corner * 2).copied().unwrap_or(0.0), quad.get(corner * 2 + 1).copied().unwrap_or(0.0));
        let [vx, vy] = page.user_to_view(x as f32, y as f32);
        if let Some(slot) = out.get_mut(corner * 2) {
            *slot = f64::from(vx);
        }
        if let Some(slot) = out.get_mut(corner * 2 + 1) {
            *slot = f64::from(vy);
        }
    }
    out
}

/// The bounding box `[x0, y0, x1, y1]` of view quads.
fn view_bounds(quads: &[[f64; 8]]) -> Option<[f32; 4]> {
    let mut bounds: Option<[f32; 4]> = None;
    for quad in quads {
        for &[x, y] in quad.as_chunks::<2>().0 {
            let (x, y) = (x as f32, y as f32);
            if !x.is_finite() || !y.is_finite() {
                continue;
            }
            bounds = Some(match bounds {
                Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
                None => [x, y, x, y],
            });
        }
    }
    bounds
}

/// Where a sidecar highlight is in PDF user space, for the canvas's flash.
pub(crate) fn user_rect(page: &PageInfo, highlight: &Highlight) -> Option<[f32; 4]> {
    let Locator::Pdf { quads, .. } = &highlight.locator else { return None };
    let [x0, y0, x1, y1] = view_bounds(quads)?;
    let [ax, ay] = page.view_to_user(x0, y0);
    let [bx, by] = page.view_to_user(x1, y1);
    Some([ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)])
}

/// Where a page note's marker is in PDF user space, for the canvas's flash.
pub(crate) fn note_user_rect(page: &PageInfo, note: &PageNote) -> [f32; 4] {
    let [across, down] = note_at(note);
    let (x, y) = (across * page.width, down * page.height);
    let [ax, ay] = page.view_to_user(x, y);
    let [bx, by] = page.view_to_user(x + NOTE_MARKER, y + NOTE_MARKER);
    [ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)]
}

/// A page note's marker position as shares of the page, kept on the page.
pub(crate) fn note_at(note: &PageNote) -> [f32; 2] {
    let at = match &note.locator {
        Locator::Pdf { at: Some(at), .. } => *at,
        _ => [NOTE_ACROSS, 0.0],
    };
    at.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) as f32 } else { 0.0 })
}

/// The text a set of view quads covers on `page`, as a callout quotes it.
fn quoted(view: &DocView, doc: &Document, page: usize, user_quads: &[[f64; 8]]) -> Option<String> {
    let text = view.page_text(page)?;
    let info = doc.info.pages.get(page)?;
    let probe = Annotation {
        page,
        subtype: "Highlight".to_owned(),
        author: None,
        contents: None,
        modified: None,
        name: None,
        in_reply_to: None,
        rect: [0.0; 4],
        color: None,
        index: 0,
        state: None,
        quads: user_quads.iter().map(|quad| quad.map(|v| v as f32)).collect(),
        locked: false,
        intent: None,
        fill_sign: false,
    };
    Some(super::obsidian::marked_text(&text, info, &probe)).filter(|quote| !quote.is_empty())
}

/// The engine's undo depth of view `i`'s document.
fn engine_depth(app: &PdfCraftApp, i: usize) -> usize {
    app.views.get(i).and_then(|view| app.session.get(view.id)).map_or(0, Document::undo_depth)
}

/// Says why nothing can be kept, once per attempt. `true` when marks can be kept.
fn can_keep(app: &mut PdfCraftApp, i: usize) -> bool {
    let Some(refused) = app.views.get(i).and_then(|view| view.margin.marks.refused.clone()) else { return true };
    app.notify(format!("Can't keep highlights or notes for this document: {refused}"));
    false
}

/// A new sidecar text mark.
pub(crate) struct NewMark {
    pub page: usize,
    /// View-space quads.
    pub quads: Vec<[f64; 8]>,
    pub text: Option<String>,
    pub kind: Markup,
    pub rgb: [f64; 3],
    pub note: String,
}

/// Keep `mark` in view `i`'s sidecar as one undoable step and select it. Returns its id.
pub(crate) fn add(app: &mut PdfCraftApp, i: usize, mark: NewMark) -> Option<String> {
    if !can_keep(app, i) {
        return None;
    }
    let depth = engine_depth(app, i);
    let (color, hex) = color_fields(&app.notes, mark.rgb);
    let view = app.views.get(i)?;
    let doc = app.session.get(view.id)?;
    let page = doc.info.pages.get(mark.page)?;
    let quads: Vec<[f64; 8]> = mark.quads.into_iter().take(records::MAX_QUADS).collect();
    if quads.is_empty() || view.margin.marks.highlights.len() >= records::MAX_HIGHLIGHTS {
        return None;
    }
    let highlight = Highlight {
        id: store::new_ulid(),
        locator: Locator::Pdf { page: u32::try_from(mark.page).ok()?, quads, at: None },
        geom: Some(geometry(page)),
        text: mark.text.filter(|text| !text.trim().is_empty()),
        style: records::style_of(mark.kind),
        color,
        hex: Some(hex),
        note: Some(mark.note).filter(|note| !note.trim().is_empty()),
        section: section_at(doc, mark.page),
        source: HighlightSource::Margin,
        created: store::now_rfc3339(),
        modified: None,
        pdf_annot: None,
        obsidian: None,
        state: None,
        extra: Map::new(),
    };
    let id = highlight.id.clone();
    let label = label_of(highlight.style);
    let view = app.views.get_mut(i)?;
    let marks = &mut view.margin.marks;
    marks.record(label, Change::Highlight { id: id.clone(), before: None, after: Some(Box::new(highlight)) }, depth);
    marks.selected = Some(Pick::Highlight(id.clone()));
    marks.reveal = true;
    view.comments.selected = None;
    tell_once(app, i);
    Some(id)
}

/// `geom` of a record made on `page`: its crop box, rotation and UserUnit (1: view pt are
/// points as the page is shown).
fn geometry(page: &PageInfo) -> [f64; 6] {
    let [x0, y0, x1, y1] = page.crop.map(f64::from);
    [x0, y0, x1, y1, f64::from(page.rotation), 1.0]
}

/// The undo label of a new mark of `style`.
fn label_of(style: HighlightStyle) -> &'static str {
    match style {
        HighlightStyle::Highlight => "Add highlight",
        HighlightStyle::Underline => "Add underline",
        HighlightStyle::Strikeout => "Add strikethrough",
        HighlightStyle::Squiggly => "Add squiggly underline",
    }
}

/// Tell the reader once per document that its highlights are kept beside the file.
fn tell_once(app: &mut PdfCraftApp, i: usize) {
    let Some(view) = app.views.get_mut(i) else { return };
    let marks = &mut view.margin.marks;
    let Some(beside) = marks.beside.filter(|_| !marks.told) else { return };
    marks.told = true;
    let where_kept = match reading::sidecar_path(app, i).zip(app.views.get(i).and_then(|v| app.session.get(v.id)).and_then(|d| d.path.clone())) {
        Some((sidecar, pdf)) if store::beside_path(std::path::Path::new(&pdf)).as_deref() != Some(sidecar.as_path()) => "in Margin's data folder",
        _ => "beside it",
    };
    app.notify(format!(
        "{}, so your highlights are kept {where_kept}, not in the PDF. \"Save a copy with highlights\" writes them into a new PDF.",
        beside.why()
    ));
}

/// Change sidecar highlight `id` of view `i` with `edit`, as one undoable step labelled `label`.
fn change_highlight(app: &mut PdfCraftApp, i: usize, id: &str, label: &'static str, edit: impl FnOnce(&mut Highlight)) {
    if !can_keep(app, i) {
        return;
    }
    let depth = engine_depth(app, i);
    let Some(view) = app.views.get_mut(i) else { return };
    let marks = &mut view.margin.marks;
    let Some(before) = marks.highlight(id).cloned() else { return };
    let mut after = before.clone();
    edit(&mut after);
    if after == before {
        return;
    }
    after.modified = Some(store::now_rfc3339());
    marks.record(label, Change::Highlight { id: id.to_owned(), before: Some(Box::new(before)), after: Some(Box::new(after)) }, depth);
}

/// Give sidecar highlight `id` the colour `rgb`.
pub(crate) fn recolor(app: &mut PdfCraftApp, i: usize, id: &str, rgb: [f64; 3]) {
    let (color, hex) = color_fields(&app.notes, rgb);
    change_highlight(app, i, id, "Change colour", |h| {
        h.color = color;
        h.hex = Some(hex);
    });
}

/// Make sidecar highlight `id` a `style` mark (a highlight into an underline or back).
pub(crate) fn restyle(app: &mut PdfCraftApp, i: usize, id: &str, style: HighlightStyle) {
    let label = if style == HighlightStyle::Underline { "Change to underline" } else { "Change to highlight" };
    change_highlight(app, i, id, label, |h| h.style = style);
}

/// Save `text` as the note of `pick` (a page note left empty is deleted).
pub(crate) fn set_note(app: &mut PdfCraftApp, i: usize, pick: &Pick, text: String) {
    match pick {
        Pick::Highlight(id) => {
            let note = Some(text).filter(|text| !text.trim().is_empty());
            change_highlight(app, i, id, "Edit note", |h| h.note = note);
        }
        Pick::Note(id) if text.trim().is_empty() => delete(app, i, pick),
        Pick::Note(id) => {
            if !can_keep(app, i) {
                return;
            }
            let depth = engine_depth(app, i);
            let Some(view) = app.views.get_mut(i) else { return };
            let marks = &mut view.margin.marks;
            let Some(before) = marks.note(id).cloned() else { return };
            if before.text == text {
                return;
            }
            let after = PageNote { text, modified: Some(store::now_rfc3339()), ..before.clone() };
            marks.record("Edit note", Change::Note { id: id.clone(), before: Some(Box::new(before)), after: Some(Box::new(after)) }, depth);
        }
    }
}

/// Delete `pick` as one undoable step, saying so with Undo.
pub(crate) fn delete(app: &mut PdfCraftApp, i: usize, pick: &Pick) {
    if !can_keep(app, i) {
        return;
    }
    let depth = engine_depth(app, i);
    let Some(view) = app.views.get_mut(i) else { return };
    let doc = view.id;
    let marks = &mut view.margin.marks;
    let said = match pick {
        Pick::Highlight(id) => {
            let Some(before) = marks.highlight(id).cloned() else { return };
            marks.record("Delete highlight", Change::Highlight { id: id.clone(), before: Some(Box::new(before)), after: None }, depth);
            "Highlight deleted"
        }
        Pick::Note(id) => {
            let Some(before) = marks.note(id).cloned() else { return };
            marks.record("Delete note", Change::Note { id: id.clone(), before: Some(Box::new(before)), after: None }, depth);
            "Note deleted"
        }
    };
    if marks.editing.as_ref().is_some_and(|(editing, _)| editing == pick) {
        marks.editing = None;
    }
    super::toast::undoable(app, said, doc);
}

/// Keep a new page note with `text` at `place`; returns its id.
fn add_note(app: &mut PdfCraftApp, i: usize, place: NotePlace, text: String) -> Option<String> {
    if text.trim().is_empty() || !can_keep(app, i) {
        return None;
    }
    let depth = engine_depth(app, i);
    let view = app.views.get(i)?;
    let doc = app.session.get(view.id)?;
    let page = doc.info.pages.get(place.page)?;
    if view.margin.marks.notes.len() >= records::MAX_NOTES {
        app.notify(format!("This document already has {} notes", records::MAX_NOTES));
        return None;
    }
    let note = PageNote {
        id: store::new_ulid(),
        locator: Locator::Pdf { page: u32::try_from(place.page).ok()?, quads: Vec::new(), at: Some(place.at) },
        geom: Some(geometry(page)),
        text,
        created: store::now_rfc3339(),
        modified: None,
        extra: Map::new(),
    };
    let id = note.id.clone();
    let view = app.views.get_mut(i)?;
    let marks = &mut view.margin.marks;
    marks.record("Add note", Change::Note { id: id.clone(), before: None, after: Some(Box::new(note)) }, depth);
    marks.selected = Some(Pick::Note(id.clone()));
    marks.reveal = true;
    Some(id)
}

/// Save the draft page note of view `i` (an empty one is dropped).
pub(crate) fn save_draft(app: &mut PdfCraftApp, i: usize) {
    let Some((place, text)) = app.views.get_mut(i).and_then(|view| view.margin.marks.draft.take()) else { return };
    add_note(app, i, place, text);
}

/// Margin's `apply_edit` hook: a text mark on a document that keeps them beside the file goes
/// to its sidecar instead of the engine. `Some(kept)` when the edit was taken, `None` to let
/// the engine apply it.
pub(crate) fn take_edit(app: &mut PdfCraftApp, edit: &Edit) -> Option<bool> {
    if app.flavor != Flavor::Margin {
        return None;
    }
    let Edit::AddAnnotation(NewAnnotation { page, shape: Shape::TextMarkup { kind, quads }, style, contents, .. }) = edit else { return None };
    let (i, _) = app.active_ids()?;
    let view = app.views.get(i)?;
    view.margin.marks.beside?;
    let doc = app.session.get(view.id)?;
    let info = doc.info.pages.get(*page)?;
    let text = quoted(view, doc, *page, quads);
    let quads = quads.iter().map(|quad| view_quad(info, quad)).collect();
    let mark = NewMark { page: *page, quads, text, kind: *kind, rgb: style.color, note: contents.clone() };
    let kept = add(app, i, mark).is_some();
    // As a mark the engine takes: a one-shot comment tool goes back to Select.
    let one_shot = app.views.get_mut(i).is_some_and(|view| std::mem::take(&mut view.comments.tool_done));
    if kept && one_shot && !app.comment_prefs.pinned {
        app.quick_tool = crate::QuickTool::Select;
    }
    Some(kept)
}

/// The sidecar's part of `edit.undo` (`undo`) or `edit.redo` on the active document: takes the
/// step when the sidecar's was the later one. `true` when it did.
pub(crate) fn step(app: &mut PdfCraftApp, undo: bool) -> bool {
    if app.flavor != Flavor::Margin || app.combine_showing() {
        return false;
    }
    let Some((i, id)) = app.active_ids() else { return false };
    let Some(doc) = app.session.get(id) else { return false };
    let depth = doc.undo_depth();
    let engine_can = if undo { doc.can_undo().is_some() } else { doc.can_redo().is_some() };
    let Some(view) = app.views.get_mut(i) else { return false };
    let marks = &mut view.margin.marks;
    let stack = if undo { &marks.undo } else { &marks.redo };
    // An undo takes the sidecar's step made at or after the engine's last one; a redo the one
    // made at or before where the engine's history stands.
    let ours = stack.last().is_some_and(|step| !engine_can || if undo { step.depth >= depth } else { step.depth <= depth });
    if !ours {
        return false;
    }
    let popped = if undo { marks.undo.pop() } else { marks.redo.pop() };
    let Some(taken) = popped else { return false };
    marks.apply(&taken.change, !undo);
    let label = taken.label;
    if undo {
        marks.redo.push(taken);
    } else {
        marks.undo.push(taken);
    }
    let label = label.to_lowercase();
    app.notify(if undo { format!("Undid {label}") } else { format!("Redid {label}") });
    true
}

/// Whether the sidecar has a step for `edit.undo` (`undo`) or `edit.redo` on the active document.
pub(crate) fn can_step(app: &PdfCraftApp, undo: bool) -> bool {
    app.flavor == Flavor::Margin
        && !app.combine_showing()
        && app.active.and_then(|i| app.views.get(i)).is_some_and(|view| {
            let marks = &view.margin.marks;
            if undo { !marks.undo.is_empty() } else { !marks.redo.is_empty() }
        })
}

/// A sidecar highlight's screen corners on a page drawn at `xform`: top-left, top-right,
/// bottom-right, bottom-left per quad.
fn screen_quads(xform: &PageXform, quads: &[[f64; 8]]) -> Vec<[Pos2; 4]> {
    let corner = |quad: &[f64; 8], k: usize| {
        let (x, y) = (quad.get(k * 2).copied().unwrap_or(0.0) as f32, quad.get(k * 2 + 1).copied().unwrap_or(0.0) as f32);
        xform.norm_to_screen(x / xform.pw.max(f32::EPSILON), y / xform.ph.max(f32::EPSILON))
    };
    quads
        .iter()
        .filter(|quad| quad.iter().all(|v| v.is_finite()))
        .map(|quad| [corner(quad, 0), corner(quad, 1), corner(quad, 3), corner(quad, 2)])
        .collect()
}

/// Draw one mark's quads in `style` and `color`.
fn paint_quads(painter: &egui::Painter, quads: &[[Pos2; 4]], style: HighlightStyle, color: Color32) {
    for &[tl, tr, br, bl] in quads {
        let height = tl.distance(bl).max(1.0);
        let stroke = Stroke::new((height * STROKE_SHARE).max(MIN_STROKE), color);
        match style {
            HighlightStyle::Highlight => {
                painter.add(egui::Shape::convex_polygon(vec![tl, tr, br, bl], color.gamma_multiply(HIGHLIGHT_ALPHA), Stroke::NONE));
            }
            HighlightStyle::Underline => {
                painter.line_segment([bl, br], stroke);
            }
            HighlightStyle::Strikeout => {
                painter.line_segment([tl.lerp(bl, 0.5), tr.lerp(br, 0.5)], stroke);
            }
            HighlightStyle::Squiggly => {
                let length = bl.distance(br);
                let wave = (height * SQUIGGLE_WAVE).max(2.0);
                let steps = ((length / (wave / 2.0)).ceil() as usize).clamp(1, 4_096);
                let up = (tl - bl).normalized() * height * SQUIGGLE_HEIGHT;
                let points: Vec<Pos2> =
                    (0..=steps).map(|k| bl.lerp(br, k as f32 / steps as f32) + if k % 2 == 0 { egui::Vec2::ZERO } else { up }).collect();
                painter.add(egui::Shape::line(points, stroke));
            }
        }
    }
}

/// Draw view `i`'s sidecar highlights and page note markers over its pages, and pick the one a
/// click on the page lands on. Called after the canvas, inside the reading area.
pub(crate) fn paint(app: &mut PdfCraftApp, ui: &egui::Ui, i: usize) {
    let Some(view) = app.views.get_mut(i) else { return };
    if !view.margin.marks.has_marks() && view.margin.marks.selected.is_none() {
        return;
    }
    let Some(doc) = app.session.get(view.id) else { return };
    if doc.comments_hidden() {
        return;
    }
    let clip = view.viewport_rect().intersect(ui.clip_rect());
    let painter = ui.painter().with_clip_rect(clip);
    let t = Tokens::get(ui.ctx());
    let click = ui.input(|input| input.pointer.primary_clicked().then(|| input.pointer.interact_pos()).flatten());
    let click = click.filter(|at| clip.contains(*at) && ui.ctx().layer_id_at(*at) == Some(ui.layer_id()));
    let pages: Vec<(usize, PageXform)> = view.visible_page_rects().iter().filter_map(|(page, _)| Some((*page, view.page_xform(*page)?))).collect();
    let notes = &app.notes;
    let marks = &mut view.margin.marks;
    let mut hit = None;
    for &(page, xform) in &pages {
        let indices = marks.on_page(page).to_vec();
        for index in indices {
            let Some(h) = marks.highlights.get(index) else { continue };
            let Locator::Pdf { quads, .. } = &h.locator else { continue };
            let shapes = screen_quads(&xform, quads);
            let [r, g, b] = shown_rgb(notes, h).map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
            paint_quads(&painter, &shapes, h.style, Color32::from_rgb(r, g, b));
            let bounds = shapes.iter().flatten().fold(Rect::NOTHING, |acc, p| acc.union(Rect::from_min_max(*p, *p)));
            if marks.selected.as_ref() == Some(&Pick::Highlight(h.id.clone())) {
                painter.rect_stroke(bounds.expand(2.0), 2.0, Stroke::new(SELECTED_STROKE, t.accent), egui::StrokeKind::Outside);
            }
            if click.is_some_and(|at| bounds.contains(at)) {
                hit = Some(Pick::Highlight(h.id.clone()));
            }
        }
        for note in marks.notes.iter().filter(|note| records::locator_page(&note.locator) == Some(page)) {
            let [across, down] = note_at(note);
            let corner = xform.norm_to_screen(across, down);
            let marker = Rect::from_min_size(corner, vec2(NOTE_MARKER, NOTE_MARKER));
            let selected = marks.selected.as_ref() == Some(&Pick::Note(note.id.clone()));
            let edge = if selected { Stroke::new(SELECTED_STROKE, t.accent) } else { Stroke::new(1.0, Color32::from_black_alpha(60)) };
            painter.rect(marker, 4.0, Color32::from_rgb(0xff, 0xe0, 0x80), edge, egui::StrokeKind::Inside);
            if clip.contains_rect(marker) {
                icons::paint(ui, marker, "sticky-note", NOTE_ICON, Color32::from_rgb(0x5a, 0x48, 0x10));
            }
            if click.is_some_and(|at| marker.contains(at)) {
                hit = Some(Pick::Note(note.id.clone()));
            }
        }
    }
    if click.is_none() {
        return;
    }
    match hit {
        Some(pick) => {
            let picked_note = matches!(pick, Pick::Note(_));
            marks.selected = Some(pick);
            marks.reveal = true;
            view.comments.selected = None;
            if picked_note {
                super::open_tab(app, SidebarTab::Highlights);
            }
        }
        None => marks.selected = None,
    }
}

/// Margin's `note.*` ids and [`SAVE_COPY_ID`]. `false` for any other id.
pub(crate) fn run(app: &mut PdfCraftApp, id: &str) -> bool {
    let Some(i) = app.active else {
        return matches!(id, NOTE_ADD_ID | NOTE_EDIT_ID | NOTE_DELETE_ID | NOTE_NEXT_ID | NOTE_PREV_ID | NOTE_LIST_ID | SAVE_COPY_ID);
    };
    match id {
        NOTE_ADD_ID => start_note(app, i),
        NOTE_EDIT_ID => edit_selected(app, i),
        NOTE_DELETE_ID => match app.views.get(i).and_then(|view| view.margin.marks.selected.clone()) {
            Some(pick @ Pick::Note(_)) => delete(app, i, &pick),
            _ => app.notify("Select a page note to delete it"),
        },
        NOTE_NEXT_ID => jump_to_note(app, i, true),
        NOTE_PREV_ID => jump_to_note(app, i, false),
        NOTE_LIST_ID => {
            if let Some(view) = app.views.get_mut(i) {
                super::highlights::show_only_notes(view);
            }
            super::open_tab(app, SidebarTab::Highlights);
        }
        SAVE_COPY_ID => save_copy(app, i),
        _ => return false,
    }
    true
}

/// `note.add`: a draft page note beside the selected text, else at the reading line, typed in
/// the Highlights tab.
fn start_note(app: &mut PdfCraftApp, i: usize) {
    if !can_keep(app, i) {
        return;
    }
    let Some(view) = app.views.get_mut(i) else { return };
    let selected = view.selection_range().and_then(|(page, range)| {
        let text = view.page_text(page)?;
        let first = text.line_rects(range).into_iter().next()?;
        let size = view.page_xform(page).map(|xf| (xf.pw, xf.ph))?;
        Some((page, [f64::from(first[2] / size.0.max(1.0)), f64::from(first[1] / size.1.max(1.0))]))
    });
    let place = match selected {
        Some((page, at)) => NotePlace { page, at: at.map(|v| v.clamp(0.0, 1.0)) },
        None => {
            let spot = reading::spot(view);
            NotePlace { page: spot.page, at: [NOTE_ACROSS, (f64::from(spot.y) + NOTE_DROP).clamp(0.0, 1.0)] }
        }
    };
    let marks = &mut view.margin.marks;
    marks.draft = Some((place, String::new()));
    marks.editing = None;
    super::open_tab(app, SidebarTab::Highlights);
}

/// `note.edit`: open the selected sidecar mark's note, or the selected comment's, for typing.
fn edit_selected(app: &mut PdfCraftApp, i: usize) {
    let Some(view) = app.views.get_mut(i) else { return };
    if let Some(pick) = view.margin.marks.selected.clone() {
        let text = match &pick {
            Pick::Highlight(id) => view.margin.marks.highlight(id).and_then(|h| h.note.clone()),
            Pick::Note(id) => view.margin.marks.note(id).map(|n| n.text.clone()),
        };
        view.margin.marks.editing = Some((pick, text.unwrap_or_default()));
    } else if let Some((page, index)) = view.comments.selected {
        let contents = app
            .session
            .get(view.id)
            .and_then(|doc| doc.info.annotations.iter().find(|a| a.page == page && a.index == index).and_then(|a| a.contents.clone()));
        view.comments.editing = Some((page, index, contents.unwrap_or_default()));
        view.comments.reveal = true;
    } else {
        app.notify("Select a note or a highlight to edit its note");
        return;
    }
    super::open_tab(app, SidebarTab::Highlights);
}

/// `note.next` / `note.prev`: the nearest page note after (or before) the reading line.
fn jump_to_note(app: &mut PdfCraftApp, i: usize, forward: bool) {
    let Some(view) = app.views.get_mut(i) else { return };
    let here = reading::spot(view);
    let here = (here.page, here.y);
    let place = |note: &PageNote| records::locator_page(&note.locator).map(|page| (page, note_at(note)[1], note.id.clone()));
    let mut places: Vec<(usize, f32, String)> = view.margin.marks.notes.iter().filter_map(place).collect();
    places.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let after = |p: &&(usize, f32, String)| (p.0, p.1) > (here.0, here.1 + JUMP_LEAD + f32::EPSILON);
    let before = |p: &&(usize, f32, String)| (p.0, p.1) < (here.0, here.1 + JUMP_LEAD - f32::EPSILON);
    let target = if forward { places.iter().find(after) } else { places.iter().rev().find(before) };
    let Some((page, down, id)) = target.cloned() else {
        app.notify(if forward { "No page note after this place" } else { "No page note before this place" });
        return;
    };
    view.margin.marks.selected = Some(Pick::Note(id));
    view.margin.marks.reveal = true;
    jump(view, page, down);
}

/// Go to `down` (a share of the page's height) on `page`, a little above it, on the history.
pub(crate) fn jump(view: &mut DocView, page: usize, down: f32) {
    let here = view.here();
    view.remember(here);
    view.current = page;
    view.page_input = (page + 1).to_string();
    reading::go(view, reading::Spot { page, y: (down - JUMP_LEAD).clamp(0.0, 1.0), x: None });
}

/// How each sidecar highlight looks as a PDF annotation: its colour, at the comment tool's
/// opacity and width.
fn annotation_style(app: &PdfCraftApp, highlight: &Highlight) -> Style {
    let tool = match highlight.style {
        HighlightStyle::Highlight => CommentTool::Highlight,
        HighlightStyle::Underline => CommentTool::Underline,
        HighlightStyle::Strikeout => CommentTool::StrikeOut,
        HighlightStyle::Squiggly => CommentTool::Squiggly,
    };
    Style { color: shown_rgb(&app.notes, highlight).map(f64::from), ..app.comment_prefs.style(tool) }
}

/// "Save a copy with highlights": the document as it is now, with its sidecar highlights and
/// page notes written as real annotations, to a new file the reader picks (`save_override` in
/// tests and automation). The open document and its sidecar are unchanged.
pub(crate) fn save_copy(app: &mut PdfCraftApp, i: usize) {
    let Some(view) = app.views.get(i) else { return };
    let marks = &view.margin.marks;
    if !marks.has_marks() {
        app.notify("There are no highlights or notes kept beside this PDF to write");
        return;
    }
    let Some(doc) = app.session.get(view.id) else { return };
    let bytes = match app.session.save_bytes(view.id) {
        Ok(bytes) => bytes,
        Err(err) => {
            app.notify(format!("Couldn't make the copy: {err}"));
            return;
        }
    };
    let styles: std::collections::HashMap<String, Style> = marks.highlights.iter().map(|h| (h.id.clone(), annotation_style(app, h))).collect();
    let note_look = app.comment_prefs.style(CommentTool::Note);
    let (highlights, notes) = (marks.highlights.clone(), marks.notes.clone());
    let (name, password, author) = (doc.display_name(), doc.password.clone(), app.comment_prefs.author.clone());
    let stem = name.trim_end_matches(".pdf").trim_end_matches(".PDF").to_owned();
    let make = move || {
        let look = |h: &Highlight| styles.get(&h.id).cloned().unwrap_or_default();
        let copy = records::MarksCopy {
            name: &name,
            bytes,
            password: password.as_deref(),
            highlights: &highlights,
            notes: &notes,
            look: &look,
            note_look,
            author: &author,
        };
        records::copy_with_marks(&copy)
    };
    let file_name = format!("{stem} (highlights).pdf");
    #[cfg(not(target_arch = "wasm32"))]
    {
        let write = move |app: &mut PdfCraftApp, path: std::path::PathBuf| {
            let written = make().and_then(|made| {
                crate::editing::write_atomically(&path.to_string_lossy(), &made.bytes).map_err(|err| err.to_string()).map(|()| made)
            });
            match written {
                Ok(made) if made.skipped > 0 => {
                    app.notify(format!("Saved a copy with {} highlights and notes to {} ({} left out)", made.written, path.display(), made.skipped))
                }
                Ok(made) => app.notify(format!("Saved a copy with {} highlights and notes to {}", made.written, path.display())),
                Err(err) => app.notify(format!("Couldn't save the copy: {err}")),
            }
        };
        match app.save_override.clone() {
            Some(path) => write(app, path.into()),
            None => {
                let dialog =
                    rfd::AsyncFileDialog::new().set_title("Save a copy with highlights").add_filter("PDF", &["pdf"]).set_file_name(&file_name);
                app.ask_one(crate::pickers::Ask::Save(dialog), None, write);
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    match make().and_then(|made| crate::editing::download(&file_name, &made.bytes)) {
        Ok(()) => app.notify("Downloaded a copy with highlights"),
        Err(err) => app.notify(format!("Couldn't make the copy: {err}")),
    }
}

/// What Copy callout quotes for a sidecar highlight: its page, text and colour.
pub(crate) fn callout_parts(notes: &Notes, highlight: &Highlight) -> Option<(usize, String, [f32; 3])> {
    let page = records::locator_page(&highlight.locator)?;
    Some((page, highlight.text.clone().unwrap_or_default(), shown_rgb(notes, highlight)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn highlight(id: &str) -> Highlight {
        Highlight {
            id: id.to_owned(),
            locator: Locator::Pdf { page: 1, quads: vec![[10.0, 20.0, 50.0, 20.0, 10.0, 30.0, 50.0, 30.0]], at: None },
            geom: None,
            text: Some("quoted".to_owned()),
            style: HighlightStyle::Highlight,
            color: "red".to_owned(),
            hex: Some("#ea5252".to_owned()),
            note: None,
            section: None,
            source: HighlightSource::Margin,
            created: "2026-10-10T10:00:00Z".to_owned(),
            modified: None,
            pdf_annot: None,
            obsidian: None,
            state: None,
            extra: Map::new(),
        }
    }

    #[test]
    fn steps_undo_and_redo_with_tombstones() {
        let mut marks = Marks::default();
        marks.record("Add highlight", Change::Highlight { id: "A".into(), before: None, after: Some(Box::new(highlight("A"))) }, 0);
        let before = marks.highlight("A").cloned();
        let mut changed = highlight("A");
        changed.note = Some("why".into());
        marks.record("Edit note", Change::Highlight { id: "A".into(), before: before.map(Box::new), after: Some(Box::new(changed)) }, 0);
        marks.record("Delete highlight", Change::Highlight { id: "A".into(), before: marks.highlight("A").cloned().map(Box::new), after: None }, 0);
        assert!(marks.highlights().is_empty());
        assert_eq!(marks.deleted, ["A"]);
        let step = marks.undo.pop().unwrap();
        marks.apply(&step.change, false);
        assert_eq!(marks.highlight("A").and_then(|h| h.note.as_deref()), Some("why"));
        assert!(marks.deleted.is_empty(), "an undone deletion leaves no tombstone");
        let mut written = Sidecar::new();
        written.highlights.push(Highlight { pdf_annot: Some(margin_config::PdfAnnot { nm: "x".into(), obj: None, r#gen: None }), ..highlight("P") });
        marks.write_into(&mut written);
        assert_eq!(written.highlights.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), ["P", "A"], "PDF-backed records stay");
        let loaded = Marks::load(Some(&written), None, None);
        assert_eq!(loaded.highlights().len(), 1, "only sidecar-only highlights are this layer's");
    }

    #[test]
    fn colours_resolve_by_name_then_creation_hex() {
        let notes = Notes::default();
        let red = shown_rgb(&notes, &highlight("A"));
        assert!((red[0] - 0xea as f32 / 255.0).abs() < 0.01, "{red:?}");
        let gone = Highlight { color: "violet".into(), hex: Some("#102030".into()), ..highlight("B") };
        assert!((shown_rgb(&notes, &gone)[2] - 0x30 as f32 / 255.0).abs() < 0.01);
    }
}
