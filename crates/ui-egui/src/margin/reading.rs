//! Margin's reading state per document (R5): where the reader stopped, how the pages are shown
//! (layout, fit, page colour, trim), page bookmarks and the Obsidian autoadd flag, kept in the
//! document's sidecar (`<file>.margin.json`, see `margin_config::store`); and autosave of the
//! PDF itself through PdfCraft's Save.
//!
//! [`opened`] (from `try_open`, before the first paint) lays the sidecar's `[doc]` settings over
//! `settings.jsonc` and restores the reading position. [`tick`] (every frame, after renders
//! arrive) keeps the reader's place while fit "read" or a trim moves the layout, writes the
//! sidecar [`DEBOUNCE_SECS`] after the last change and autosaves the PDF the same way.
//! [`closing`] and a window close write what is pending. Nothing is written on open.

use std::path::PathBuf;

use egui::pos2;
use margin_config::store::{self, Found};
use margin_config::{Bookmark, Locator, Progress, Sidecar, SidecarDoc, SidecarObsidian};
use pdfcraft_engine::Document;
use pdfcraft_render::DocInfo;
use serde_json::{Map, Value};

use super::display::{DisplayState, SOURCE_NEAR};
use super::trim::{Margins, Trim};
use crate::canvas::{Fit, PT, PageLayout, SIDE};
use crate::{DocView, Flavor, PdfCraftApp};

/// Sidecar writes and autosaves wait this long after the last change.
pub const DEBOUNCE_SECS: f64 = 1.0;
/// Reading positions closer than this share of a page are the same place (layout rounding).
const SAME_PLACE: f32 = 0.01;
/// Fit "read" follows the window when the zoom it wants moves by more than this share.
const READ_ZOOM_STEP: f32 = 0.005;
/// The page view's width before its first frame measures it (`DocView::new`).
const FIRST_FRAME_WIDTH: f32 = 800.0;
/// The page view's zoom limits (`DocView::zoom_at`).
const MIN_ZOOM: f32 = 0.08;
const MAX_ZOOM: f32 = 64.0;
/// The same limits in percent, as `view.zoom` takes them (the settings schema's range).
const MIN_ZOOM_PERCENT: f32 = MIN_ZOOM * PERCENT;
const MAX_ZOOM_PERCENT: f32 = MAX_ZOOM * PERCENT;
/// A saved horizontal place outside this many page widths is not one this reader wrote.
const X_RANGE: std::ops::RangeInclusive<f32> = -1.0..=2.0;
const PERCENT: f32 = 100.0;
/// Page bookmarks kept per document (the sidecar is untrusted input).
pub const MAX_BOOKMARKS: usize = 10_000;
/// The longest bookmark name kept, in characters.
const MAX_BOOKMARK_NAME: usize = 200;
/// The width share a bookmark keeps when the sideways place isn't known: the page's middle.
const CENTRE: f32 = 0.5;
/// The `format` of a PDF's sidecar.
const DOC_FORMAT: &str = "pdf";
/// The `[doc]` keys "Set as default" copies, per Display popover row.
const PAGE_COLOR_KEYS: [&str; 5] = ["page_color", "follow_system", "keep_figures", "brightness", "contrast"];
const LAYOUT_KEYS: [&str; 5] = ["layout", "cover_alone", "fit", "zoom", "reading_width_px"];

/// One document's reading state. Lives in its `DocView`; inert in PdfCraft (no trim, nothing
/// read or written).
#[derive(Default)]
pub struct DocReading {
    pub trim: Trim,
    pub display: DisplayState,
    /// Margin's page bookmarks (not the PDF's outline), in page order.
    pub bookmarks: Vec<Bookmark>,
    /// The page text the Highlights tab's cards quote.
    pub(crate) quotes: super::highlights::Quotes,
    /// The Highlights tab's search, filters and laid-out list.
    pub(crate) list: super::highlights::List,
    /// Highlights and page notes kept in the sidecar (`margin::marks`).
    pub(crate) marks: super::marks::Marks,
    /// A new search's first match, kept on the reading page (`margin::find`).
    pub(crate) find_pick: Option<super::find::AutoPick>,
    /// The horizontal half of a pending `goto`: the share of the page's width to put at the
    /// window's centre. The canvas takes it with `goto` (R8 QoL 4).
    pub(crate) goto_x: Option<f32>,
    /// Back and Forward to exact places, and the Back pill (R8 QoL 6).
    pub(crate) history: super::history::History,
    /// The selection bar: Copy, colours, Underline, Comment, callout (R8 QoL 8).
    pub(crate) selection: super::selection::SelectionBar,
    /// Space / Page Down gliding by a screen (R8 QoL 15).
    pub(crate) screen_step: super::scroll_step::ScreenStep,
    /// Fit "read": the widest page `reading_width_px` wide, narrower windows permitting. Ends
    /// when the reader zooms or picks another fit.
    read_fit: Option<ReadFit>,
    sidecar: Option<SidecarFile>,
    autosave: Autosave,
    /// The page count the trim boxes were learned for.
    pages: usize,
    reader_defaults: Option<(u32, margin_config::FindOptions)>,
    restored_place: bool,
    #[cfg(test)]
    snapshots_built: std::cell::Cell<u64>,
}

#[derive(Clone, Copy)]
struct ReadFit {
    width_px: f32,
    /// The zoom fit "read" set last; any other zoom means the reader changed it.
    zoom: f32,
}

#[derive(Default)]
struct Autosave {
    /// The document's edit generation when it opened: that state is never autosaved.
    opened: u64,
    /// The generation last seen, and when it changed.
    seen: u64,
    changed_at: f64,
    /// Saving this generation failed: wait for the next change.
    failed: Option<u64>,
    /// The PDF file was read-only when it opened: Save as is the only way to keep changes.
    file_read_only: bool,
}

/// The sidecar of a document opened from a file.
struct SidecarFile {
    document: PathBuf,
    data: Option<PathBuf>,
    /// The library's key for the book (`library::opened`): the name of its sidecar in `<data>`
    /// and the sidecar's `doc.book_id` when it has none yet. `None` without a library.
    book_id: Option<String>,
    /// The file as read (or new): highlights, notes and fields from newer versions survive.
    file: Sidecar,
    /// Where it was found or last written.
    path: Option<PathBuf>,
    /// Why it must not be written (a newer version, unreadable).
    locked: Option<String>,
    /// The state as last read or written; taken on the first frame.
    saved: Option<Snapshot>,
    /// A change not written yet, and when it last changed.
    pending: Option<(Snapshot, f64)>,
}

/// Everything the sidecar records, to tell a change.
#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    page: usize,
    /// Share of the page's height at the window's top.
    at: f32,
    /// Share of the page's width at the window's centre, when known.
    x: Option<f32>,
    /// The `[doc]` settings layer (`settings.view`), as [`view_layer`] writes it.
    view: ViewState,
    remember: bool,
    trim: TrimRecord,
    bookmarks: Vec<Bookmark>,
    /// `Marks::generation`: sidecar highlights, page notes and tombstones.
    marks: u64,
    auto_add: bool,
}

/// The persisted part of [`Trim`] (on/off is `view.trim`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct TrimRecord {
    manual: bool,
    separate: bool,
    odd: Margins,
    even: Margins,
}

/// What the `[doc]` settings layer records, without its JSON: equal states write equal layers,
/// so frames compare it without allocating.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ViewState {
    display: DisplayState,
    layout: margin_config::Layout,
    cover: bool,
    fit: &'static str,
    /// Only a zoom fit records its zoom; the others recompute it from the window.
    zoom: Option<u32>,
    read_width: Option<u32>,
    trim: bool,
    page_gap: Option<u32>,
}

impl ViewState {
    fn of(view: &DocView) -> Self {
        let fit = match (view.fit, view.margin.read_fit.is_some()) {
            (Fit::None, true) => "read",
            (Fit::Width, _) => "width",
            (Fit::Page, _) => "page",
            (Fit::Height | Fit::None, _) => "zoom",
        };
        Self {
            display: view.margin.display,
            layout: layout_of(view),
            cover: view.cover,
            fit,
            zoom: (fit == "zoom").then(|| (view.zoom * PERCENT).round().clamp(MIN_ZOOM_PERCENT, MAX_ZOOM_PERCENT) as u32),
            // Fit "read" is only the same fit with the width it was picked at.
            read_width: view.margin.read_fit.map(|read| read.width_px.round() as u32),
            trim: view.margin.trim.on,
            page_gap: view.page_gap_px,
        }
    }
}

impl TrimRecord {
    fn of(trim: &Trim) -> Self {
        Self { manual: trim.manual, separate: trim.separate, odd: trim.odd, even: trim.even }
    }
}

impl Snapshot {
    fn matches(&self, view: &DocView, auto_add: bool, remember: bool) -> bool {
        let Spot { page, y, x } = spot(view);
        let near = |left: f32, right: f32| (left - right).abs() <= SAME_PLACE;
        self.remember == remember
            && (!remember
                || (self.page == page
                    && near(self.at, y)
                    && self.x.zip(x).is_none_or(|(left, right)| near(left, right))
                    && self.view == ViewState::of(view)))
            && self.trim == TrimRecord::of(&view.margin.trim)
            && self.bookmarks == view.margin.bookmarks
            && self.marks == view.margin.marks.generation()
            && self.auto_add == auto_add
    }
}

/// A reading spot: the document point under the window's top centre (a little below the top
/// edge, past the gap between pages). Shares of the whole page, so a trim learned later or a
/// refit keeps the same text there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub page: usize,
    /// Share of the page's height at the window's top.
    pub y: f32,
    /// Share of the page's width at the window's centre; `None` leaves the sideways scroll.
    pub x: Option<f32>,
}

/// Where the document's changes are, for the footer (R8 QoL 7). Read-only and signed come first:
/// they say why Margin doesn't save the PDF by itself even when nothing has changed yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveStatus {
    /// Nothing unsaved; `autosave`: Margin writes changes into the PDF by itself.
    Saved { autosave: bool },
    /// Changed; autosave will write it.
    Saving,
    /// Changed and only Save writes it: autosave is off, the document has no file yet, or the
    /// changes came from before this session (a recovered document).
    Unsaved,
    /// Autosave couldn't write this change: still in the document, unsaved.
    Failed,
    /// Can't be changed or saved in place: PdfCraft opened it read-only
    /// (`Document::read_only_reason`) or the file or its folder is read-only.
    ReadOnly { dirty: bool, marks: MarksKept },
    /// Signed: Margin never writes it unasked; Save adds an incremental update, which keeps the
    /// signatures valid.
    Signed { dirty: bool, marks: MarksKept },
}

/// Where the text marks of a PDF Margin doesn't write go (`margin::marks`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarksKept {
    /// Into the PDF, like any edit: nothing is kept beside it.
    InPdf,
    /// Into its sidecar; `any`: the sidecar holds highlights or notes a copy can take.
    Beside { any: bool },
    /// Nowhere: its sidecar was written by a newer Margin or can't be read.
    Refused,
}

/// The Display popover rows "Set as default" belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultRow {
    PageColor,
    Layout,
}

/// A document was opened into view `index`: read its sidecar and show it as it was left, or as
/// `settings.jsonc` says for a document Margin hasn't seen.
pub(crate) fn opened(app: &mut PdfCraftApp, index: usize) {
    if app.flavor != Flavor::Margin {
        return;
    }
    let defaults = app.margin_keys.settings_for_open();
    let data = app.margin_keys.data_dir();
    let remember = defaults.reader.remember_per_document;
    let undo_limit = defaults.reader.undo_steps as usize;
    let find_defaults = defaults.reader.find;
    #[cfg(not(target_arch = "wasm32"))]
    let book_id = super::library::book_key(app, index);
    #[cfg(target_arch = "wasm32")]
    let book_id: Option<String> = None;
    let Some(view) = app.views.get_mut(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let document = doc.path.as_ref().map(PathBuf::from);
    let found = match &document {
        Some(path) => {
            let fingerprint = store::fingerprint(&doc.bytes);
            store::find_any(path, &[book_id.as_deref().unwrap_or(&fingerprint), &fingerprint], data.as_deref())
        }
        None => Found::Missing,
    };
    let (file, path, locked) = match found {
        Found::Missing => (Sidecar::new(), None, None),
        Found::Sidecar { sidecar, path } => (*sidecar, Some(path), None),
        Found::Locked { path, reason } => (Sidecar::new(), Some(path), Some(reason)),
    };
    // A sidecar from a newer Margin is never written, but the highlights and notes this
    // version can read still show, read-only.
    let newer = match (&locked, &path) {
        (Some(_), Some(path)) => store::read(path).ok().flatten(),
        _ => None,
    };
    let settings = match (&file.settings, &path, remember) {
        (Some(layer), Some(path), true) => {
            let loaded = defaults.with_layer(layer, &path.display().to_string());
            loaded.problems.iter().for_each(|problem| log::warn!("{problem}"));
            loaded.settings
        }
        _ => defaults,
    };
    let pages = doc.info.pages.len();
    apply_view_settings(view, &settings.view, file.trim.as_ref(), &doc.info);
    view.find_defaults = Some(find_defaults);
    view.margin.reader_defaults = Some((settings.reader.undo_steps, find_defaults));
    if remember && let Some(spot) = file.progress.as_ref().and_then(|progress| spot_of(progress, pages)) {
        view.margin.restored_place = true;
        view.current = spot.page;
        view.page_input = (spot.page + 1).to_string();
        go(view, spot);
    }
    let generation = doc.edit_generation();
    let beside = document.as_deref().and_then(|path| super::marks::beside_reason(doc, path));
    let refused = match (&document, &locked) {
        (None, _) => Some("it has no file yet".to_owned()),
        (Some(_), Some(reason)) => Some(format!("its sidecar can't be changed: {}", reason.trim_end_matches("; it is not changed"))),
        (Some(_), None) => None,
    };
    let reading = &mut view.margin;
    reading.marks = super::marks::Marks::load(newer.as_ref().or(Some(&file)), beside, refused);
    reading.bookmarks = file.bookmarks.iter().take(MAX_BOOKMARKS).cloned().collect();
    reading.bookmarks.sort_by_key(|bookmark| bookmark_page(bookmark).unwrap_or(usize::MAX));
    let file_read_only = document.as_deref().is_some_and(super::marks::path_unwritable);
    reading.autosave = Autosave { opened: generation, seen: generation, file_read_only, ..Autosave::default() };
    reading.pages = pages;
    reading.history.exact = true;
    let auto_add = file.obsidian.as_ref().is_some_and(|obsidian| obsidian.auto_add);
    let notice = locked.as_ref().map(|reason| format!("This document's reading state won't be saved: {reason}"));
    reading.sidecar = document.map(|document| SidecarFile { document, data, book_id, file, path, locked, saved: None, pending: None });
    // The sidecar is where autoadd is remembered: an open document follows it.
    if let Some(path) = doc.path.clone() {
        if auto_add {
            app.notes.autoadd.insert(path);
        } else {
            app.notes.autoadd.remove(&path);
        }
    }
    let id = view.id;
    super::history::set_depth(view, settings.reader.back_stack);
    if let Err(error) = app.session.set_undo_limit(id, undo_limit) {
        log::warn!("Couldn't apply undo depth: {error}");
    }
    if let Some(notice) = notice {
        app.notify(notice);
    }
    #[cfg(not(target_arch = "wasm32"))]
    super::library::opened(app, index);
}

/// The `doc.book_id` in the document's sidecar, when it has one.
pub(crate) fn sidecar_book_id(view: &DocView) -> Option<String> {
    view.margin.sidecar.as_ref().and_then(|file| file.file.doc.as_ref()).map(|doc| doc.book_id.clone()).filter(|id| !id.is_empty())
}

/// The library listed the document as the book with key `book_id`: its sidecar goes under that
/// key in `<data>` from now on.
pub(crate) fn set_book_id(view: &mut DocView, book_id: String) {
    if let Some(file) = view.margin.sidecar.as_mut() {
        file.book_id = Some(book_id);
    }
}

/// Whether the document's sidecar had a reading position to restore.
pub(crate) fn has_saved_place(view: &DocView) -> bool {
    view.margin.restored_place
}

/// Whether fit "read" holds the zoom.
pub fn reads(view: &DocView) -> bool {
    view.margin.read_fit.is_some()
}

/// The reading width fit "read" holds, if it holds the zoom.
pub(crate) fn read_width(view: &DocView) -> Option<f32> {
    view.margin.read_fit.map(|read| read.width_px)
}

/// Put back fit "read" at `width_px` for the zoom shown now (a place in the view history), or
/// end it.
pub(crate) fn hold_read_width(view: &mut DocView, width_px: Option<f32>) {
    view.margin.read_fit = width_px.map(|width_px| ReadFit { width_px, zoom: view.zoom });
}

/// Pick fit "read" (`width_px`: `view.reading_width_px`), keeping the reader's place.
pub fn fit_read(view: &mut DocView, width_px: u32) {
    let spot = spot(view);
    view.fit = Fit::None;
    view.margin.read_fit = Some(ReadFit { width_px: width_px as f32, zoom: view.zoom });
    go(view, spot);
}

/// Pick fit width or page (ending fit "read"), keeping the reading line where it is rather than
/// going to the page's top as `DocView::set_fit` does.
pub fn set_fit(view: &mut DocView, fit: Fit) {
    let spot = spot(view);
    view.fit = fit;
    view.margin.read_fit = None;
    go(view, spot);
}

/// Margin's page display of `view`: Scroll is one page across scrolling, Single one page at a
/// time, Two-up spreads (PdfCraft's two-page view that doesn't scroll shows as Two-up too).
pub(crate) fn layout_of(view: &DocView) -> margin_config::Layout {
    match (view.layout, view.continuous) {
        (PageLayout::TwoUp, _) => margin_config::Layout::TwoUp,
        (PageLayout::Single, true) => margin_config::Layout::Scroll,
        (PageLayout::Single, false) => margin_config::Layout::Single,
    }
}

/// The canvas's base layout and continuous scrolling for Margin's page display `layout`.
fn display_of(layout: margin_config::Layout) -> (PageLayout, bool) {
    match layout {
        margin_config::Layout::Scroll => (PageLayout::Single, true),
        margin_config::Layout::Single => (PageLayout::Single, false),
        margin_config::Layout::TwoUp => (PageLayout::TwoUp, true),
    }
}

/// Show `view` in Margin's page display `layout`, keeping the current page in view.
pub(crate) fn show_layout(view: &mut DocView, layout: margin_config::Layout) {
    let (base, continuous) = display_of(layout);
    view.set_display(base, continuous);
}

/// The canvas's fit width or page just changed the zoom (a resized window or sidebar): keep the
/// reading line. Nothing to keep before a frame is drawn, and Single view fits each page anew as
/// it turns, so a page turn is not pulled back.
pub(crate) fn refitted(view: &mut DocView) {
    if view.goto.is_some() || !view.continuous || view.visible_page_rects().is_empty() {
        return;
    }
    let spot = spot(view);
    go(view, spot);
}

/// `--trim auto|manual|on|off` (and the control channel's `set`); `on` keeps the last mode.
pub fn set_trim_option(view: &mut DocView, value: &str) -> Result<(), String> {
    let manual = match value {
        "auto" => false,
        "manual" => true,
        "on" | "off" => view.margin.trim.manual,
        _ => return Err("trim must be auto, manual, on or off".into()),
    };
    let spot = spot(view);
    view.margin.trim.on = value != "off";
    view.margin.trim.manual = manual;
    go(view, spot);
    Ok(())
}

/// Show `view` as the `[doc]` settings `settings` and the sidecar's trim block say.
fn apply_view_settings(view: &mut DocView, settings: &margin_config::View, trim: Option<&margin_config::Trim>, info: &DocInfo) {
    view.page_gap_px = Some(settings.page_gap_px);
    view.margin.display = DisplayState::from_settings(settings);
    let mut trim_state = Trim::default();
    trim_state.on = settings.trim;
    if let Some(record) = trim {
        let margins = |m: [f64; 4]| super::trim::clamp_margins(m.map(|v| v as f32));
        (trim_state.manual, trim_state.separate) = (record.manual, record.separate);
        (trim_state.odd, trim_state.even) = (margins(record.odd), margins(record.even));
    }
    view.margin.trim = trim_state;
    (view.layout, view.continuous) = display_of(settings.layout);
    view.cover = settings.cover_alone;
    view.margin.read_fit = None;
    match settings.fit {
        margin_config::Fit::Width | margin_config::Fit::Content => view.fit = Fit::Width,
        margin_config::Fit::Page => view.fit = Fit::Page,
        margin_config::Fit::Zoom => {
            view.fit = Fit::None;
            view.zoom = (settings.zoom as f32 / PERCENT).clamp(MIN_ZOOM, MAX_ZOOM);
        }
        margin_config::Fit::Read => {
            view.fit = Fit::None;
            view.margin.read_fit = Some(ReadFit { width_px: settings.reading_width_px as f32, zoom: view.zoom });
            follow_read_fit(view, info);
        }
    }
}

/// The spot a sidecar's progress points at, if it is on a page of this document.
fn spot_of(progress: &Progress, pages: usize) -> Option<Spot> {
    let Locator::Pdf { page, at, .. } = &progress.locator else { return None };
    let page = usize::try_from(*page).ok().filter(|page| *page < pages)?;
    let y = at.map_or(0.0, |[_, y]| y as f32);
    let x = at.map(|[x, _]| x as f32).filter(|x| X_RANGE.contains(x));
    Some(Spot { page, y: if y.is_finite() { y.clamp(0.0, 1.0) } else { 0.0 }, x })
}

/// Fit "read": set the zoom that shows the widest page `reading_width_px` wide, or the window's
/// width when that is narrower. True when the zoom moved.
fn follow_read_fit(view: &mut DocView, info: &DocInfo) -> bool {
    let Some(read) = view.margin.read_fit else { return false };
    if view.fit != Fit::None || view.zoom != read.zoom {
        view.margin.read_fit = None;
        return false;
    }
    let width = view.viewport_rect().width();
    let window = if width.is_finite() && width > 0.0 { width } else { FIRST_FRAME_WIDTH };
    let widest = info.pages.iter().enumerate().map(|(i, p)| view.display_size(i, p).0).fold(1.0, f32::max);
    // Two-up fits the spread: two pages and the gap between them, as `DocView::fit_zoom` lays
    // out a row (a cover shown alone is one page of such a row).
    let per_row = if view.layout == PageLayout::TwoUp { 2.0 } else { 1.0 };
    let page_room = ((window - 2.0 * SIDE - view.page_gap() * (per_row - 1.0)) / per_row).max(1.0);
    let zoom = (read.width_px.min(page_room) / (widest * PT)).clamp(MIN_ZOOM, MAX_ZOOM);
    let moved = (zoom - view.zoom).abs() > view.zoom * READ_ZOOM_STEP;
    if moved {
        view.zoom = zoom;
    }
    view.margin.read_fit = Some(ReadFit { zoom: view.zoom, ..read });
    moved
}

/// Where the reader is: the page at the window's top and how far down it the window starts (a
/// share of the whole page). A jump not drawn yet counts as done.
pub fn place(view: &DocView) -> (usize, f32) {
    let spot = spot(view);
    (spot.page, spot.y)
}

/// The reading spot under the window's top centre. A jump not drawn yet counts as done.
pub fn spot(view: &DocView) -> Spot {
    if let Some((page, y)) = view.goto {
        return Spot { page, y: y.clamp(0.0, 1.0), x: view.margin.goto_x };
    }
    let viewport = view.viewport_rect();
    let probe = pos2(viewport.center().x, viewport.top() + view.page_gap());
    let rects = view.visible_page_rects();
    // The first page with some of it below the probe line; a probe in the gap picks the next page.
    let hit = rects.iter().find(|(_, rect)| rect.bottom() > probe.y).or(rects.first());
    let Some(xf) = hit.and_then(|(page, _)| view.page_xform(*page)) else { return Spot { page: view.current, y: 0.0, x: None } };
    let page = hit.map_or(view.current, |(page, _)| *page);
    // Shares of the page as drawn (rotated or not), widened from the part that frame showed (a
    // trim box learned since has not moved it yet) to the whole page; `canvas` maps them back
    // through the trim it lays out with.
    let [u0, v0, u1, v1] = xf.crop;
    let across = (probe.x - xf.rect.left()) / xf.rect.width().max(f32::EPSILON);
    let down = ((probe.y - xf.rect.top()) / xf.rect.height().max(f32::EPSILON)).clamp(0.0, 1.0);
    let x = (u0 + across * (u1 - u0)).clamp(*X_RANGE.start(), *X_RANGE.end());
    Spot { page, y: v0 + down * (v1 - v0), x: Some(x) }
}

/// Go to `spot` on the next frame: its page share at the window's top and, when it has one,
/// its width share at the window's centre (a page narrower than the window stays centred).
pub fn go(view: &mut DocView, spot: Spot) {
    view.goto = Some((spot.page, spot.y));
    view.margin.goto_x = spot.x;
}

/// The sideways scroll offset that puts `spot_x`, a share of `page`'s width, at the centre of a
/// `viewport_width` window, for `page` laid out at `rect` (content coordinates). The canvas's
/// part of [`go`].
pub(crate) fn goto_x_offset(view: &mut DocView, page: usize, rect: egui::Rect, viewport_width: f32) -> Option<f32> {
    let spot_x = view.margin.goto_x.take()?;
    let [u0, _, u1, _] = view.margin.trim.crop(page);
    let across = (spot_x - u0) / (u1 - u0).max(f32::EPSILON);
    Some((rect.left() + across * rect.width() - viewport_width / 2.0).max(0.0))
}

/// The page a bookmark marks.
pub fn bookmark_page(bookmark: &Bookmark) -> Option<usize> {
    match bookmark.locator {
        Locator::Pdf { page, .. } => usize::try_from(page).ok(),
        Locator::Cfi { .. } | Locator::Text { .. } => None,
    }
}

/// What `reader.bookmark.toggle` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookmarkToggle {
    Added,
    Removed,
    /// Not added: the document has [`MAX_BOOKMARKS`] already.
    Full,
}

impl BookmarkToggle {
    /// What to tell the reader about page `page_number` (1-based).
    pub fn message(self, page_number: usize) -> String {
        match self {
            Self::Added => format!("Bookmarked page {page_number}"),
            Self::Removed => format!("Removed the bookmark on page {page_number}"),
            Self::Full => format!("Can't bookmark page {page_number}: this document already has {MAX_BOOKMARKS} bookmarks"),
        }
    }
}

/// `reader.bookmark.toggle`: bookmark the current page at the reading spot, or remove its
/// bookmark.
pub fn toggle_bookmark(view: &mut DocView) -> BookmarkToggle {
    let page = view.current;
    if let Some(id) = view.margin.bookmarks.iter().find(|bookmark| bookmark_page(bookmark) == Some(page)).map(|b| b.id.clone()) {
        remove_bookmark(view, &id);
        return BookmarkToggle::Removed;
    }
    if view.margin.bookmarks.len() >= MAX_BOOKMARKS {
        return BookmarkToggle::Full;
    }
    // The exact spot when the window's top is on this page; else the page's top.
    let here = spot(view);
    let at = if here.page == page { [f64::from(here.x.unwrap_or(CENTRE)), f64::from(here.y)] } else { [f64::from(CENTRE), 0.0] };
    let bookmark = Bookmark {
        id: store::new_ulid(),
        locator: Locator::Pdf { page: u32::try_from(page).unwrap_or(u32::MAX), quads: Vec::new(), at: Some(at) },
        name: String::new(),
        modified: None,
        created: store::now_rfc3339(),
        extra: Map::new(),
    };
    let bookmarks = &mut view.margin.bookmarks;
    let index = bookmarks.partition_point(|other| bookmark_page(other).is_some_and(|p| p < page));
    bookmarks.insert(index, bookmark);
    BookmarkToggle::Added
}

/// Remove bookmark `id`, leaving a tombstone so another writer's copy doesn't bring it back.
pub fn remove_bookmark(view: &mut DocView, id: &str) {
    let reading = &mut view.margin;
    let before = reading.bookmarks.len();
    reading.bookmarks.retain(|bookmark| bookmark.id != id);
    if reading.bookmarks.len() != before {
        reading.marks.bookmark_removed(id);
    }
}

/// Name bookmark `id` `name` ("" shows its section again).
pub fn rename_bookmark(view: &mut DocView, id: &str, name: &str) {
    let name: String = name.trim().chars().take(MAX_BOOKMARK_NAME).collect();
    if let Some(bookmark) = view.margin.bookmarks.iter_mut().find(|bookmark| bookmark.id == id && bookmark.name != name) {
        bookmark.name = name;
        bookmark.modified = Some(store::now_rfc3339());
    }
}

/// Go to bookmark `id`: the spot it was added at, or its page's top for one without.
pub fn go_to_bookmark(view: &mut DocView, id: &str) {
    let Some(bookmark) = view.margin.bookmarks.iter().find(|bookmark| bookmark.id == id) else { return };
    let Some(page) = bookmark_page(bookmark).filter(|page| *page < view.margin.pages) else { return };
    let at = match bookmark.locator {
        Locator::Pdf { at: Some([x, y]), .. } if x.is_finite() && y.is_finite() => Some((x as f32, y as f32)),
        _ => None,
    };
    let here = view.here();
    view.remember(here);
    view.current = page;
    view.page_input = (page + 1).to_string();
    let (x, y) = at.map_or((None, 0.0), |(x, y)| (Some(x).filter(|x| X_RANGE.contains(x)), y.clamp(0.0, 1.0)));
    go(view, Spot { page, y, x });
}

/// `view.trim.toggle`: trim on or off, keeping the reader's place.
pub fn toggle_trim(view: &mut DocView) {
    let spot = spot(view);
    view.margin.trim.on = !view.margin.trim.on;
    go(view, spot);
}

/// The `[doc]` settings layer of a view state (`settings.view` in the sidecar).
fn view_layer(state: &ViewState) -> Map<String, Value> {
    let mut layer = state.display.settings_values();
    let layout = match state.layout {
        margin_config::Layout::Scroll => "scroll",
        margin_config::Layout::Single => "single",
        margin_config::Layout::TwoUp => "two_up",
    };
    layer.insert("layout".into(), layout.into());
    layer.insert("cover_alone".into(), state.cover.into());
    layer.insert("fit".into(), state.fit.into());
    if let Some(percent) = state.zoom {
        layer.insert("zoom".into(), percent.into());
    }
    if let Some(width) = state.read_width {
        layer.insert("reading_width_px".into(), width.into());
    }
    layer.insert("trim".into(), state.trim.into());
    if let Some(gap) = state.page_gap {
        layer.insert("page_gap_px".into(), gap.into());
    }
    layer
}

fn snapshot(view: &DocView, auto_add: bool, remember: bool) -> Snapshot {
    #[cfg(test)]
    view.margin.snapshots_built.set(view.margin.snapshots_built.get().saturating_add(1));
    let Spot { page, y, x } = spot(view);
    Snapshot {
        page,
        at: y,
        x,
        view: ViewState::of(view),
        remember,
        trim: TrimRecord::of(&view.margin.trim),
        bookmarks: view.margin.bookmarks.clone(),
        marks: view.margin.marks.generation(),
        auto_add,
    }
}

/// Every frame, after renders arrive: keep places, write sidecars and autosave (Margin only).
pub(crate) fn tick(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if app.flavor != Flavor::Margin {
        return;
    }
    let now = ctx.input(|i| i.time);
    let closing = ctx.input(|i| i.viewport().close_requested());
    let autosave_on = app.margin_keys.settings().reader.autosave;
    let remember = app.margin_keys.settings().reader.remember_per_document;
    let undo_limit = app.margin_keys.settings().reader.undo_steps as usize;
    let find_defaults = app.margin_keys.settings().reader.find;
    for view in &mut app.views {
        if view.margin.reader_defaults != Some((undo_limit as u32, find_defaults)) {
            view.margin.reader_defaults = Some((undo_limit as u32, find_defaults));
            view.find_defaults = Some(find_defaults);
            if let Err(error) = app.session.set_undo_limit(view.id, undo_limit) {
                log::warn!("Couldn't apply undo depth: {error}");
            }
        }
    }
    let mut save = Vec::new();
    let mut notices = Vec::new();
    let mut wake: Option<f64> = None;
    for (index, view) in app.views.iter_mut().enumerate() {
        let Some(doc) = app.session.get(view.id) else { continue };
        let pages = doc.info.pages.len();
        if pages != view.margin.pages {
            view.margin.trim.forget_boxes();
            view.margin.pages = pages;
        }
        // A trim box learned or a refit moves the pages: stay where the reader was.
        let spot = spot(view);
        // Pages shown before auto trim was on have their pixels kept near the current page.
        if view.margin.trim.wants_boxes() {
            for page in view.current.saturating_sub(SOURCE_NEAR)..=view.current.saturating_add(SOURCE_NEAR) {
                if !view.margin.trim.knows(page)
                    && let Some((size, rgba)) = view.page_color.source(page)
                {
                    view.margin.trim.observe(page, true, size, rgba);
                }
            }
        }
        let learned = view.margin.trim.take_learned().is_some();
        if (follow_read_fit(view, &doc.info) || learned) && view.goto.is_none() {
            go(view, spot);
        }
        let auto_add = doc.path.as_ref().is_some_and(|path| app.notes.autoadd.contains(path));
        match sidecar_step(view, doc, auto_add, remember, now, closing) {
            Step::Idle => {}
            Step::WakeAt(at) => wake = Some(wake.map_or(at, |w| w.min(at))),
            Step::Tell(message) => notices.push(message),
        }
        let generation = doc.edit_generation();
        let state = &mut view.margin.autosave;
        if generation != state.seen {
            state.seen = generation;
            state.changed_at = now;
        }
        // A change on disk waits to be compared with these edits, or for the reader's answer
        // (`margin::reload`): saving now would write over the other program's version.
        let held = app.margin_reload.holds(view.id);
        if autosave_on && autosave_possible(doc, state) && doc.dirty && generation != state.opened && state.failed != Some(generation) && !held {
            #[cfg(not(target_arch = "wasm32"))]
            if app.margin_saves.contains(view.id) {
                continue;
            }
            let ready = view.pending_edit.is_none() && view.forms.focus.is_none();
            if ready && now - state.changed_at >= DEBOUNCE_SECS {
                save.push((index, generation));
            } else {
                let at = state.changed_at + DEBOUNCE_SECS;
                wake = Some(wake.map_or(at, |w| w.min(at)));
            }
        }
    }
    for (index, generation) in save {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some((id, path)) =
            app.views.get(index).and_then(|view| app.session.get(view.id)).and_then(|doc| doc.path.clone().map(|path| (doc.id, path)))
            && !app.queue_margin_save(id, path, None)
            && let Some(view) = app.views.get_mut(index)
        {
            view.margin.autosave.failed = Some(generation);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (index, generation);
    }
    for message in notices {
        app.notify(message);
    }
    if let Some(at) = wake {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((at - now).max(0.0)));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        super::stats::tick(app, ctx);
        super::library::tick(app, ctx);
    }
}

/// Whether autosave may write this document: it has a writable file, may be edited and isn't
/// signed (an incremental save keeps signatures valid, but Margin never writes a signed file
/// unasked).
fn autosave_possible(doc: &Document, state: &Autosave) -> bool {
    doc.path.is_some() && doc.read_only_reason.is_none() && !state.file_read_only && !doc.is_signed()
}

/// The footer's save state for view `index`.
pub fn save_status(app: &PdfCraftApp, index: usize) -> SaveStatus {
    let autosave_on = app.margin_keys.settings().reader.autosave;
    let Some(view) = app.views.get(index) else { return SaveStatus::Saved { autosave: autosave_on } };
    let Some(doc) = app.session.get(view.id) else { return SaveStatus::Saved { autosave: autosave_on } };
    let state = &view.margin.autosave;
    #[cfg(not(target_arch = "wasm32"))]
    if app.margin_save_pending(doc.id) {
        return SaveStatus::Saving;
    }
    let marks = &view.margin.marks;
    let kept = match (marks.beside(), marks.refused()) {
        (_, Some(_)) => MarksKept::Refused,
        (Some(_), None) => MarksKept::Beside { any: marks.has_marks() },
        (None, None) => MarksKept::InPdf,
    };
    if doc.read_only_reason.is_some() || state.file_read_only {
        return SaveStatus::ReadOnly { dirty: doc.dirty, marks: kept };
    }
    if doc.is_signed() {
        return SaveStatus::Signed { dirty: doc.dirty, marks: kept };
    }
    let autosave = autosave_on && autosave_possible(doc, state);
    let generation = doc.edit_generation();
    if !doc.dirty {
        SaveStatus::Saved { autosave }
    } else if state.failed == Some(generation) {
        SaveStatus::Failed
    } else if autosave && generation != state.opened {
        SaveStatus::Saving
    } else {
        SaveStatus::Unsaved
    }
}

/// A write of document `id` failed: the footer says so until the next change. A file made
/// read-only since it opened shows as read-only, and autosave stops trying it.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_failed(app: &mut PdfCraftApp, id: pdfcraft_engine::DocId, generation: u64) {
    let file_read_only = app
        .session
        .get(id)
        .and_then(|doc| doc.path.as_ref())
        .and_then(|path| std::fs::metadata(path).ok())
        .is_some_and(|meta| meta.permissions().readonly());
    if let Some(view) = app.views.iter_mut().find(|view| view.id == id) {
        view.margin.autosave.failed = Some(generation);
        view.margin.autosave.file_read_only |= file_read_only;
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn saved_pdf(app: &mut PdfCraftApp, id: pdfcraft_engine::DocId) {
    if let Some(view) = app.views.iter_mut().find(|view| view.id == id) {
        // Atomic replacement creates a writable file, including after Save As from read-only.
        view.margin.autosave.file_read_only = false;
        view.margin.autosave.failed = None;
    }
}

enum Step {
    Idle,
    /// A change waits; look again at this time.
    WakeAt(f64),
    /// Something to tell the reader once: writing failed (the next change tries again), or the
    /// sidecar had to go to Margin's data folder.
    Tell(String),
}

/// The sidecar's part of a frame: note changes, and write once they have held still for
/// [`DEBOUNCE_SECS`] (at once when `flush`).
fn sidecar_step(view: &mut DocView, doc: &Document, auto_add: bool, remember: bool, now: f64, flush: bool) -> Step {
    let Some(file) = view.margin.sidecar.as_ref() else { return Step::Idle };
    if file.locked.is_some() {
        return Step::Idle;
    }
    if file.saved.is_none() {
        // The first frame: the state as opened, never written for itself.
        let snap = snapshot(view, auto_add, remember);
        if let Some(file) = view.margin.sidecar.as_mut() {
            file.saved = Some(snap);
        }
        return Step::Idle;
    }
    if file.saved.as_ref().is_some_and(|saved| saved.matches(view, auto_add, remember)) {
        if let Some(file) = view.margin.sidecar.as_mut() {
            file.pending = None;
        }
        return Step::Idle;
    }
    let unchanged = file.pending.as_ref().is_some_and(|(pending, _)| pending.matches(view, auto_add, remember));
    if !unchanged {
        let snap = snapshot(view, auto_add, remember);
        if let Some(file) = view.margin.sidecar.as_mut() {
            file.pending = Some((snap, now));
        }
    }
    let reading = &mut view.margin;
    let Some(file) = reading.sidecar.as_mut() else { return Step::Idle };
    let Some((_, since)) = file.pending.as_ref() else { return Step::Idle };
    if !flush && now - since < DEBOUNCE_SECS {
        return Step::WakeAt(since + DEBOUNCE_SECS);
    }
    let Some((mut snap, _)) = file.pending.take() else { return Step::Idle };
    let result = write_sidecar(file, &mut snap, doc, Records { marks: &mut reading.marks, bookmarks: &mut reading.bookmarks });
    // Either way this state is done: a failure is reported once, the next change retries.
    file.saved = Some(snap);
    match result {
        Ok(None) => Step::Idle,
        Ok(Some(moved)) => Step::Tell(moved),
        Err(err) => {
            log::warn!("sidecar: {err}");
            Step::Tell(format!("Couldn't save the reading state: {err}"))
        }
    }
}

/// The records a sidecar write carries besides the reading state; a write that merges another
/// writer's copy puts the merged records back.
struct Records<'a> {
    marks: &'a mut super::marks::Marks,
    bookmarks: &'a mut Vec<Bookmark>,
}

/// The sidecar on disk as `file` must take it into account before writing over it: `Ok(None)`
/// when it is the copy last read or written (or gone), `Ok(Some(theirs))` when another writer
/// saved it since, and an error when it must be left alone (a newer Margin wrote it, or it can't
/// be read any more).
fn on_disk(file: &SidecarFile) -> Result<Option<Sidecar>, String> {
    let Some(path) = file.path.as_deref() else { return Ok(None) };
    match store::read(path)? {
        Some(theirs) if theirs.version > store::SIDECAR_VERSION => {
            Err(format!("{} was written by a newer Margin (version {}) since it was opened; it is not changed", path.display(), theirs.version))
        }
        Some(theirs) if theirs.revision > file.file.revision => Ok(Some(theirs)),
        Some(_) | None => Ok(None),
    }
}

/// Write `snap` and `records` into the sidecar, keeping everything else in it, merged with
/// what another writer saved there since (`margin_config::marks::merge`): `snap` and `records`
/// then hold the merged records. Returns what to tell the reader when it newly went to the data
/// folder because the document's folder can't be written.
fn write_sidecar(file: &mut SidecarFile, snap: &mut Snapshot, doc: &Document, records: Records<'_>) -> Result<Option<String>, String> {
    let theirs = match on_disk(file) {
        Ok(theirs) => theirs,
        Err(reason) => {
            records.marks.refuse(format!("its sidecar can't be changed: {}", reason.trim_end_matches("; it is not changed")));
            file.locked = Some(reason.clone());
            return Err(reason);
        }
    };
    let mut out = file.file.clone();
    let fingerprint = store::fingerprint(&doc.bytes);
    let pages = doc.info.pages.len();
    let old = out.doc.take();
    let size = u64::try_from(doc.bytes.len()).unwrap_or(u64::MAX);
    let unchanged = old.as_ref().is_some_and(|d| d.sidecar_fingerprint == fingerprint && d.size == size);
    out.doc = Some(SidecarDoc {
        book_id: old.as_ref().map(|d| d.book_id.clone()).filter(|id| !id.is_empty()).or_else(|| file.book_id.clone()).unwrap_or_else(store::new_ulid),
        sidecar_fingerprint: fingerprint.clone(),
        size,
        // The whole-file hash of another revision would be wrong now.
        file_revision_hash: old.as_ref().and_then(|d| d.file_revision_hash.clone()).filter(|_| unchanged),
        format: DOC_FORMAT.to_owned(),
        pages: u32::try_from(pages).ok(),
        title: Some(doc.display_name()),
        path_hint: Some(file.document.display().to_string()),
        extra: old.map(|d| d.extra).unwrap_or_default(),
    });
    if snap.remember {
        let old = out.progress.take();
        let fraction = if pages == 0 { 0.0 } else { ((snap.page as f64 + f64::from(snap.at)) / pages as f64).clamp(0.0, 1.0) };
        // Unknown sideways (a jump not drawn yet): keep what was saved; 0 restores as before R8.
        let old_x = old.as_ref().and_then(|p| match p.locator {
            Locator::Pdf { at: Some([x, _]), .. } => Some(x),
            _ => None,
        });
        let x = snap.x.map(f64::from).or(old_x).unwrap_or(0.0);
        out.progress = Some(Progress {
            locator: Locator::Pdf { page: u32::try_from(snap.page).unwrap_or(u32::MAX), quads: Vec::new(), at: Some([x, f64::from(snap.at)]) },
            fraction,
            finished: old.as_ref().is_some_and(|p| p.finished),
            updated: store::now_rfc3339(),
            extra: old.map(|p| p.extra).unwrap_or_default(),
        });
        // Other `[doc]` keys someone wrote into the layer stay.
        let mut settings = match out.settings.take() {
            Some(Value::Object(settings)) => settings,
            _ => Map::new(),
        };
        let view = settings.entry("view").or_insert_with(|| Value::Object(Map::new()));
        if !view.is_object() {
            *view = Value::Object(Map::new());
        }
        if let Value::Object(view) = view {
            view.extend(view_layer(&snap.view));
        }
        out.settings = Some(Value::Object(settings));
    }
    let old = out.trim.take();
    let wide = |m: Margins| m.map(f64::from);
    out.trim = Some(margin_config::Trim {
        manual: snap.trim.manual,
        odd: wide(snap.trim.odd),
        even: wide(snap.trim.even),
        separate: snap.trim.separate,
        extra: old.map(|t| t.extra).unwrap_or_default(),
    });
    let old = out.obsidian.take();
    out.obsidian =
        (snap.auto_add || old.is_some()).then(|| SidecarObsidian { auto_add: snap.auto_add, extra: old.map(|o| o.extra).unwrap_or_default() });
    out.bookmarks = snap.bookmarks.clone();
    records.marks.write_into(&mut out);
    if let Some(theirs) = &theirs {
        margin_config::marks::merge(&mut out, theirs);
        out.bookmarks.sort_by_key(|bookmark| bookmark_page(bookmark).unwrap_or(usize::MAX));
        out.bookmarks.truncate(MAX_BOOKMARKS);
        records.marks.adopt(&out);
        records.bookmarks.clone_from(&out.bookmarks);
        snap.bookmarks.clone_from(&out.bookmarks);
        snap.marks = records.marks.generation();
    }
    let key = file.book_id.as_deref().unwrap_or(&fingerprint);
    let written = store::write(&mut out, &file.document, key, file.data.as_deref(), file.path.as_deref())?;
    let beside = store::beside_path(&file.document);
    let moved = (file.path.as_ref() != Some(&written) && beside.as_ref() != Some(&written))
        .then(|| "This PDF's folder can't be written, so its reading state is kept in Margin's data folder; the PDF itself is unchanged".to_owned());
    file.path = Some(written);
    file.file = out;
    Ok(moved)
}

/// Tab `index` is closing: write its pending reading state now.
pub(crate) fn closing(app: &mut PdfCraftApp, index: usize) {
    if app.flavor != Flavor::Margin {
        return;
    }
    let Some(id) = app.views.get(index).map(|view| view.id) else { return };
    #[cfg(not(target_arch = "wasm32"))]
    app.wait_margin_saves();
    let Some(index) = app.views.iter().position(|view| view.id == id) else { return };
    let remember = app.margin_keys.settings().reader.remember_per_document;
    let Some(view) = app.views.get_mut(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let auto_add = doc.path.as_ref().is_some_and(|path| app.notes.autoadd.contains(path));
    if let Step::Tell(message) = sidecar_step(view, doc, auto_add, remember, 0.0, true) {
        app.notify(message);
    }
    #[cfg(not(target_arch = "wasm32"))]
    super::library::closing(app, index);
}

/// Marks the document open in view `index` finished (or not) in its sidecar, written now.
pub(crate) fn set_finished(app: &mut PdfCraftApp, index: usize, finished: bool) -> Result<(), String> {
    let remember = app.margin_keys.settings().reader.remember_per_document;
    let Some(view) = app.views.get_mut(index) else { return Ok(()) };
    let Some(doc) = app.session.get(view.id) else { return Ok(()) };
    let auto_add = doc.path.as_ref().is_some_and(|path| app.notes.autoadd.contains(path));
    let mut snap = snapshot(view, auto_add, remember);
    let reading = &mut view.margin;
    let Some(file) = reading.sidecar.as_mut() else { return Ok(()) };
    if let Some(reason) = &file.locked {
        return Err(reason.clone());
    }
    let progress = file.file.progress.get_or_insert_with(|| Progress {
        locator: Locator::Pdf { page: 0, quads: Vec::new(), at: None },
        fraction: 0.0,
        finished,
        updated: store::now_rfc3339(),
        extra: Map::new(),
    });
    progress.finished = finished;
    write_sidecar(file, &mut snap, doc, Records { marks: &mut reading.marks, bookmarks: &mut reading.bookmarks })?;
    file.saved = Some(snap);
    file.pending = None;
    Ok(())
}

/// Write every open document's pending reading state now (tests, embedders shutting down).
pub fn flush(app: &mut PdfCraftApp) {
    let documents: Vec<_> = app.views.iter().map(|view| view.id).collect();
    for id in documents {
        if let Some(index) = app.views.iter().position(|view| view.id == id) {
            closing(app, index);
        }
    }
}

/// Where view `index`'s sidecar is, once found or written.
pub fn sidecar_path(app: &PdfCraftApp, index: usize) -> Option<PathBuf> {
    app.views.get(index)?.margin.sidecar.as_ref()?.path.clone()
}

/// "Set as default": copy row `row`'s values of view `index` into `settings.jsonc`, keeping the
/// file's comments. Returns what to tell the user.
pub fn set_as_default(app: &mut PdfCraftApp, index: usize, row: DefaultRow) -> String {
    let Some(view) = app.views.get(index) else { return "No document is open.".to_owned() };
    let layer = view_layer(&ViewState::of(view));
    let keys: &[&str] = match row {
        DefaultRow::PageColor => &PAGE_COLOR_KEYS,
        DefaultRow::Layout => &LAYOUT_KEYS,
    };
    let values: Vec<(String, Value)> = keys.iter().filter_map(|key| layer.get(*key).map(|value| (format!("view.{key}"), value.clone()))).collect();
    let pairs: Vec<(&str, Value)> = values.iter().map(|(key, value)| (key.as_str(), value.clone())).collect();
    let Some(dir) = app.margin_keys.settings_dir() else { return "This system has no settings folder.".to_owned() };
    match margin_config::set_user_values(&dir, &pairs) {
        Ok(()) => match row {
            DefaultRow::PageColor => "Page color saved as the default for new documents".to_owned(),
            DefaultRow::Layout => "Layout saved as the default for new documents".to_owned(),
        },
        Err(err) => format!("Couldn't set the default: {err}"),
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn idle_and_pending_frames_keep_the_existing_snapshot_allocations() {
        let mut session = pdfcraft_engine::Session::new();
        let bytes = session.create_blank(300.0, 400.0, 2).unwrap();
        let id = session.open("synthetic.pdf", None, bytes, None).unwrap();
        let doc = session.get(id).unwrap();
        let mut view = DocView::new(id, &doc.info, Default::default());
        view.margin.sidecar = Some(SidecarFile {
            document: PathBuf::from("synthetic.pdf"),
            data: None,
            book_id: None,
            file: Sidecar::new(),
            path: None,
            locked: None,
            saved: None,
            pending: None,
        });
        assert_eq!(toggle_bookmark(&mut view), BookmarkToggle::Added);
        assert!(matches!(sidecar_step(&mut view, doc, false, true, 0.0, false), Step::Idle));
        assert_eq!(view.margin.snapshots_built.get(), 1);
        let bookmark_allocation = view.margin.sidecar.as_ref().unwrap().saved.as_ref().unwrap().bookmarks.as_ptr();
        for frame in 1..20 {
            assert!(matches!(sidecar_step(&mut view, doc, false, true, f64::from(frame), false), Step::Idle));
            assert_eq!(view.margin.sidecar.as_ref().unwrap().saved.as_ref().unwrap().bookmarks.as_ptr(), bookmark_allocation);
            assert_eq!(view.margin.snapshots_built.get(), 1, "idle frames must not rebuild JSON or clone bookmarks");
        }
        // A fitted zoom follows the window and is not recorded, so it is no change to write.
        view.zoom = 1.5;
        assert!(matches!(sidecar_step(&mut view, doc, false, true, 20.0, false), Step::Idle));
        assert_eq!(view.margin.snapshots_built.get(), 1);
        view.layout = PageLayout::TwoUp;
        assert!(matches!(sidecar_step(&mut view, doc, false, true, 20.0, false), Step::WakeAt(_)));
        let pending_allocation = view.margin.sidecar.as_ref().unwrap().pending.as_ref().unwrap().0.bookmarks.as_ptr();
        assert!(matches!(sidecar_step(&mut view, doc, false, true, 20.5, false), Step::WakeAt(_)));
        assert_eq!(view.margin.sidecar.as_ref().unwrap().pending.as_ref().unwrap().0.bookmarks.as_ptr(), pending_allocation);
        assert_eq!(view.margin.snapshots_built.get(), 2, "a pending unchanged state is captured once");
    }
}
