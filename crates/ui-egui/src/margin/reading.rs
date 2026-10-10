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
use crate::canvas::{Fit, GAP, PT, PageLayout, SIDE};
use crate::{DocView, Flavor, PdfCraftApp, SaveTarget};

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
/// `view.zoom`'s range in the settings schema, percent.
const MIN_ZOOM_PERCENT: f32 = 50.0;
const MAX_ZOOM_PERCENT: f32 = 300.0;
const PERCENT: f32 = 100.0;
/// Page bookmarks kept per document (the sidecar is untrusted input).
const MAX_BOOKMARKS: usize = 10_000;
/// The `format` of a PDF's sidecar.
const DOC_FORMAT: &str = "pdf";
/// The `[doc]` keys "Set as default" copies, per Display popover row.
const PAGE_COLOR_KEYS: [&str; 5] = ["page_color", "follow_system", "keep_figures", "brightness", "contrast"];
const LAYOUT_KEYS: [&str; 4] = ["layout", "cover_alone", "fit", "zoom"];

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
    /// Fit "read": the widest page `reading_width_px` wide, narrower windows permitting. Ends
    /// when the reader zooms or picks another fit.
    read_fit: Option<ReadFit>,
    sidecar: Option<SidecarFile>,
    autosave: Autosave,
    /// The page count the trim boxes were learned for.
    pages: usize,
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
}

/// The sidecar of a document opened from a file.
struct SidecarFile {
    document: PathBuf,
    data: Option<PathBuf>,
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
    /// The `[doc]` settings layer (`settings.view`).
    view: Map<String, Value>,
    trim: TrimRecord,
    bookmarks: Vec<Bookmark>,
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

impl TrimRecord {
    fn of(trim: &Trim) -> Self {
        Self { manual: trim.manual, separate: trim.separate, odd: trim.odd, even: trim.even }
    }
}

impl Snapshot {
    fn same(&self, other: &Self) -> bool {
        self.page == other.page
            && (self.at - other.at).abs() <= SAME_PLACE
            && self.view == other.view
            && self.trim == other.trim
            && self.bookmarks == other.bookmarks
            && self.auto_add == other.auto_add
    }
}

/// How far the footer's save hint is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveStatus {
    Saved,
    /// Changed; autosave will write it.
    Saving,
    /// Changed; only Ctrl+S saves it (autosave off, read-only, signed or failed).
    Unsaved,
}

impl SaveStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Saved => "Saved",
            Self::Saving => "Saving…",
            Self::Unsaved => "Unsaved",
        }
    }
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
    let Some(view) = app.views.get_mut(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let document = doc.path.as_ref().map(PathBuf::from);
    let found = match &document {
        Some(path) => store::find(path, &store::fingerprint(&doc.bytes), data.as_deref()),
        None => Found::Missing,
    };
    let (file, path, locked) = match found {
        Found::Missing => (Sidecar::new(), None, None),
        Found::Sidecar { sidecar, path } => (*sidecar, Some(path), None),
        Found::Locked { path, reason } => (Sidecar::new(), Some(path), Some(reason)),
    };
    let settings = match (&file.settings, &path) {
        (Some(layer), Some(path)) => {
            let loaded = defaults.with_layer(layer, &path.display().to_string());
            loaded.problems.iter().for_each(|problem| log::warn!("{problem}"));
            loaded.settings
        }
        _ => defaults,
    };
    let pages = doc.info.pages.len();
    apply_view_settings(view, &settings.view, file.trim.as_ref(), &doc.info);
    if let Some((page, at)) = file.progress.as_ref().and_then(|progress| place_of(progress, pages)) {
        view.current = page;
        view.page_input = (page + 1).to_string();
        view.goto = Some((page, at));
    }
    let generation = doc.edit_generation();
    let reading = &mut view.margin;
    reading.bookmarks = file.bookmarks.iter().take(MAX_BOOKMARKS).cloned().collect();
    reading.bookmarks.sort_by_key(|bookmark| bookmark_page(bookmark).unwrap_or(usize::MAX));
    reading.autosave = Autosave { opened: generation, seen: generation, ..Autosave::default() };
    reading.pages = pages;
    let auto_add = file.obsidian.as_ref().is_some_and(|obsidian| obsidian.auto_add);
    let notice = locked.as_ref().map(|reason| format!("This document's reading state won't be saved: {reason}"));
    reading.sidecar = document.map(|document| SidecarFile { document, data, file, path, locked, saved: None, pending: None });
    // The sidecar is where autoadd is remembered: an open document follows it.
    if let Some(path) = doc.path.clone() {
        if auto_add {
            app.notes.autoadd.insert(path);
        } else {
            app.notes.autoadd.remove(&path);
        }
    }
    if let Some(notice) = notice {
        app.notify(notice);
    }
    #[cfg(not(target_arch = "wasm32"))]
    super::library::opened(app, index);
}

/// Whether the document's sidecar had a reading position to restore.
pub(crate) fn has_saved_place(view: &DocView) -> bool {
    view.margin.sidecar.as_ref().is_some_and(|file| file.file.progress.is_some())
}

/// Whether fit "read" holds the zoom.
pub fn reads(view: &DocView) -> bool {
    view.margin.read_fit.is_some()
}

/// Pick fit "read" (`width_px`: `view.reading_width_px`), keeping the reader's place.
pub fn fit_read(view: &mut DocView, width_px: u32) {
    let place = place(view);
    view.fit = Fit::None;
    view.margin.read_fit = Some(ReadFit { width_px: width_px as f32, zoom: view.zoom });
    view.goto = Some(place);
}

/// `--trim auto|manual|on|off` (and the control channel's `set`); `on` keeps the last mode.
pub fn set_trim_option(view: &mut DocView, value: &str) -> Result<(), String> {
    let manual = match value {
        "auto" => false,
        "manual" => true,
        "on" | "off" => view.margin.trim.manual,
        _ => return Err("trim must be auto, manual, on or off".into()),
    };
    let place = place(view);
    view.margin.trim.on = value != "off";
    view.margin.trim.manual = manual;
    view.goto = Some(place);
    Ok(())
}

/// Show `view` as the `[doc]` settings `settings` and the sidecar's trim block say.
fn apply_view_settings(view: &mut DocView, settings: &margin_config::View, trim: Option<&margin_config::Trim>, info: &DocInfo) {
    view.margin.display = DisplayState::from_settings(settings);
    let mut trim_state = Trim::default();
    trim_state.on = settings.trim;
    if let Some(record) = trim {
        let margins = |m: [f64; 4]| super::trim::clamp_margins(m.map(|v| v as f32));
        (trim_state.manual, trim_state.separate) = (record.manual, record.separate);
        (trim_state.odd, trim_state.even) = (margins(record.odd), margins(record.even));
    }
    view.margin.trim = trim_state;
    view.layout = match settings.layout {
        margin_config::Layout::Scroll => PageLayout::Continuous,
        margin_config::Layout::Single => PageLayout::Single,
        margin_config::Layout::TwoUp => PageLayout::TwoUp,
    };
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

/// The page and share of it a sidecar's progress points at, if it is a page of this document.
fn place_of(progress: &Progress, pages: usize) -> Option<(usize, f32)> {
    let Locator::Pdf { page, at, .. } = &progress.locator else { return None };
    let page = usize::try_from(*page).ok().filter(|page| *page < pages)?;
    let at = at.map_or(0.0, |[_, y]| y as f32);
    Some((page, if at.is_finite() { at.clamp(0.0, 1.0) } else { 0.0 }))
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
    let shown = read.width_px.min((window - 2.0 * SIDE).max(1.0));
    let zoom = (shown / (widest * PT)).clamp(MIN_ZOOM, MAX_ZOOM);
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
    if let Some((page, at)) = view.goto {
        return (page, at.clamp(0.0, 1.0));
    }
    let top = view.viewport_rect().top() + GAP;
    let rects = view.visible_page_rects();
    // The first page with some of it below the probe line; a probe in the gap picks the next page.
    let hit = rects.iter().find(|(_, rect)| rect.bottom() > top).or(rects.first());
    match hit.and_then(|(page, _)| Some((*page, view.page_xform(*page)?))) {
        Some((page, xf)) => (page, xf.screen_to_norm(pos2(xf.rect.center().x, top)).1.clamp(0.0, 1.0)),
        None => (view.current, 0.0),
    }
}

/// The page a bookmark marks.
pub fn bookmark_page(bookmark: &Bookmark) -> Option<usize> {
    match bookmark.locator {
        Locator::Pdf { page, .. } => usize::try_from(page).ok(),
        Locator::Cfi { .. } | Locator::Text { .. } => None,
    }
}

/// `reader.bookmark.toggle`: bookmark the current page, or remove its bookmark. True when added.
pub fn toggle_bookmark(view: &mut DocView) -> bool {
    let page = view.current;
    let bookmarks = &mut view.margin.bookmarks;
    if let Some(i) = bookmarks.iter().position(|bookmark| bookmark_page(bookmark) == Some(page)) {
        bookmarks.remove(i);
        return false;
    }
    if bookmarks.len() >= MAX_BOOKMARKS {
        return false;
    }
    let bookmark = Bookmark {
        id: store::new_ulid(),
        locator: Locator::Pdf { page: u32::try_from(page).unwrap_or(u32::MAX), quads: Vec::new(), at: None },
        name: String::new(),
        created: store::now_rfc3339(),
        extra: Map::new(),
    };
    let at = bookmarks.partition_point(|other| bookmark_page(other).is_some_and(|p| p < page));
    bookmarks.insert(at, bookmark);
    true
}

/// `view.trim.toggle`: trim on or off, keeping the reader's place.
pub fn toggle_trim(view: &mut DocView) {
    let place = place(view);
    view.margin.trim.on = !view.margin.trim.on;
    view.goto = Some(place);
}

/// The `[doc]` settings layer of `view` as shown now (`settings.view` in the sidecar).
fn view_layer(view: &DocView) -> Map<String, Value> {
    let mut layer = view.margin.display.settings_values();
    let layout = match view.layout {
        PageLayout::Continuous => "scroll",
        PageLayout::Single => "single",
        PageLayout::TwoUp => "two_up",
    };
    let fit = match (view.fit, view.margin.read_fit.is_some()) {
        (Fit::None, true) => "read",
        (Fit::Width, _) => "width",
        (Fit::Page, _) => "page",
        (Fit::Height | Fit::None, _) => "zoom",
    };
    layer.insert("layout".into(), layout.into());
    layer.insert("cover_alone".into(), view.cover.into());
    layer.insert("fit".into(), fit.into());
    if fit == "zoom" {
        let percent = (view.zoom * PERCENT).round().clamp(MIN_ZOOM_PERCENT, MAX_ZOOM_PERCENT) as u32;
        layer.insert("zoom".into(), percent.into());
    }
    layer.insert("trim".into(), view.margin.trim.on.into());
    layer
}

fn snapshot(view: &DocView, auto_add: bool) -> Snapshot {
    let (page, at) = place(view);
    Snapshot { page, at, view: view_layer(view), trim: TrimRecord::of(&view.margin.trim), bookmarks: view.margin.bookmarks.clone(), auto_add }
}

/// Every frame, after renders arrive: keep places, write sidecars and autosave (Margin only).
pub(crate) fn tick(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if app.flavor != Flavor::Margin {
        return;
    }
    let now = ctx.input(|i| i.time);
    let closing = ctx.input(|i| i.viewport().close_requested());
    let autosave_on = app.margin_keys.settings().reader.autosave;
    let mut save = Vec::new();
    let mut failures = Vec::new();
    let mut wake: Option<f64> = None;
    for (index, view) in app.views.iter_mut().enumerate() {
        let Some(doc) = app.session.get(view.id) else { continue };
        let pages = doc.info.pages.len();
        if pages != view.margin.pages {
            view.margin.trim.forget_boxes();
            view.margin.pages = pages;
        }
        // A trim box learned or a refit moves the pages: stay where the reader was.
        let place = place(view);
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
            view.goto = Some(place);
        }
        let auto_add = doc.path.as_ref().is_some_and(|path| app.notes.autoadd.contains(path));
        match sidecar_step(view, doc, auto_add, now, closing) {
            Step::Idle => {}
            Step::WakeAt(at) => wake = Some(wake.map_or(at, |w| w.min(at))),
            Step::Failed(message) => failures.push(message),
        }
        let generation = doc.edit_generation();
        let state = &mut view.margin.autosave;
        if generation != state.seen {
            state.seen = generation;
            state.changed_at = now;
        }
        if autosave_on && autosave_possible(doc) && doc.dirty && generation != state.opened && state.failed != Some(generation) {
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
        // PdfCraft's Save: incremental, atomic, and it clears the recovery entry.
        if !app.save_view(index, SaveTarget::InPlace)
            && let Some(view) = app.views.get_mut(index)
        {
            view.margin.autosave.failed = Some(generation);
        }
    }
    for message in failures {
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

/// Whether autosave may write this document: it has a file, may be edited and isn't signed
/// (an incremental save keeps signatures valid, but Margin never writes a signed file unasked).
fn autosave_possible(doc: &Document) -> bool {
    doc.path.is_some() && doc.read_only_reason.is_none() && !doc.is_signed()
}

/// The footer's save hint for view `index`.
pub fn save_status(app: &PdfCraftApp, index: usize) -> SaveStatus {
    let Some(view) = app.views.get(index) else { return SaveStatus::Saved };
    let Some(doc) = app.session.get(view.id) else { return SaveStatus::Saved };
    if !doc.dirty {
        return SaveStatus::Saved;
    }
    let state = &view.margin.autosave;
    let generation = doc.edit_generation();
    let autosaves =
        app.margin_keys.settings().reader.autosave && autosave_possible(doc) && generation != state.opened && state.failed != Some(generation);
    if autosaves { SaveStatus::Saving } else { SaveStatus::Unsaved }
}

enum Step {
    Idle,
    /// A change waits; look again at this time.
    WakeAt(f64),
    /// Writing failed (said once; the next change tries again).
    Failed(String),
}

/// The sidecar's part of a frame: note changes, and write once they have held still for
/// [`DEBOUNCE_SECS`] (at once when `flush`).
fn sidecar_step(view: &mut DocView, doc: &Document, auto_add: bool, now: f64, flush: bool) -> Step {
    if view.margin.sidecar.is_none() {
        return Step::Idle;
    }
    let snap = snapshot(view, auto_add);
    let Some(file) = view.margin.sidecar.as_mut() else { return Step::Idle };
    let Some(saved) = &file.saved else {
        // The first frame: the state as opened, never written for itself.
        file.saved = Some(snap);
        return Step::Idle;
    };
    if file.locked.is_some() || saved.same(&snap) {
        file.pending = None;
        return Step::Idle;
    }
    let since = match &file.pending {
        Some((pending, since)) if pending.same(&snap) => *since,
        _ => now,
    };
    file.pending = Some((snap.clone(), since));
    if !flush && now - since < DEBOUNCE_SECS {
        return Step::WakeAt(since + DEBOUNCE_SECS);
    }
    let result = write_sidecar(file, &snap, doc);
    // Either way this state is done: a failure is reported once, the next change retries.
    file.saved = Some(snap);
    file.pending = None;
    match result {
        Ok(()) => Step::Idle,
        Err(err) => {
            log::warn!("sidecar: {err}");
            Step::Failed(format!("Couldn't save the reading state: {err}"))
        }
    }
}

/// Write `snap` into the sidecar, keeping everything else in it.
fn write_sidecar(file: &mut SidecarFile, snap: &Snapshot, doc: &Document) -> Result<(), String> {
    let mut out = file.file.clone();
    let fingerprint = store::fingerprint(&doc.bytes);
    let pages = doc.info.pages.len();
    let old = out.doc.take();
    let size = u64::try_from(doc.bytes.len()).unwrap_or(u64::MAX);
    let unchanged = old.as_ref().is_some_and(|d| d.sidecar_fingerprint == fingerprint && d.size == size);
    out.doc = Some(SidecarDoc {
        book_id: old.as_ref().map(|d| d.book_id.clone()).filter(|id| !id.is_empty()).unwrap_or_else(store::new_ulid),
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
    let old = out.progress.take();
    let fraction = if pages == 0 { 0.0 } else { ((snap.page as f64 + f64::from(snap.at)) / pages as f64).clamp(0.0, 1.0) };
    out.progress = Some(Progress {
        locator: Locator::Pdf { page: u32::try_from(snap.page).unwrap_or(u32::MAX), quads: Vec::new(), at: Some([0.0, f64::from(snap.at)]) },
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
        view.extend(snap.view.clone());
    }
    out.settings = Some(Value::Object(settings));
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
    let written = store::write(&mut out, &file.document, &fingerprint, file.data.as_deref(), file.path.as_deref())?;
    file.path = Some(written);
    file.file = out;
    Ok(())
}

/// Tab `index` is closing: write its pending reading state now.
pub(crate) fn closing(app: &mut PdfCraftApp, index: usize) {
    if app.flavor != Flavor::Margin {
        return;
    }
    let Some(view) = app.views.get_mut(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let auto_add = doc.path.as_ref().is_some_and(|path| app.notes.autoadd.contains(path));
    if let Step::Failed(message) = sidecar_step(view, doc, auto_add, 0.0, true) {
        app.notify(message);
    }
    #[cfg(not(target_arch = "wasm32"))]
    super::library::closing(app, index);
}

/// Marks the document open in view `index` finished (or not) in its sidecar, written now.
pub(crate) fn set_finished(app: &mut PdfCraftApp, index: usize, finished: bool) -> Result<(), String> {
    let Some(view) = app.views.get_mut(index) else { return Ok(()) };
    let Some(doc) = app.session.get(view.id) else { return Ok(()) };
    let auto_add = doc.path.as_ref().is_some_and(|path| app.notes.autoadd.contains(path));
    let snap = snapshot(view, auto_add);
    let Some(file) = view.margin.sidecar.as_mut() else { return Ok(()) };
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
    write_sidecar(file, &snap, doc)?;
    file.saved = Some(snap);
    file.pending = None;
    Ok(())
}

/// Write every open document's pending reading state now (tests, embedders shutting down).
pub fn flush(app: &mut PdfCraftApp) {
    for index in 0..app.views.len() {
        closing(app, index);
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
    let layer = view_layer(view);
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
