//! Margin's library (R6): the Home view of the Margin flavor. Books come from
//! `<data>/library.sqlite` (`margin_config::library`): every PDF Margin opened, and every PDF in
//! the watched folders (`library.folders`), scanned on start, when the setting changes and on
//! Rescan, on a worker thread. A "Continue reading" card opens the last book being read at its
//! page (the sidecar restores it, as any open does); below it the shelves (Recent, Reading,
//! Finished, Folders, Collections, Tags) as a cover grid or a list, with search and sort. Both
//! views are virtualised: only the rows on screen are laid out, and only their covers are loaded.
//!
//! Covers are the first page through PdfCraft's renderer, cached as JPEG in
//! `<data>/cache/thumbs/`, decoded on a worker thread and uploaded a few per frame
//! ([`covers`]). The shelf's order is kept between frames and made again only when the books,
//! the search, the sort or the shelf change, so a frame costs the rows on screen, not the
//! library's size.
//!
//! A book's tags show on its card and row and are found by the search, which also matches the
//! details an import or a lookup filled; "In text" searches the books' pages instead ([`text`]).
//! A book's menu tags it and opens its details sheet ([`details`]); the app menu imports a
//! BibTeX file or a Zotero library ([`import`]).

mod covers;
mod details;
mod import;
mod text;

pub(crate) use import::item as import_item;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, TextureHandle, Vec2, pos2, vec2};
use margin_config::library::{self as store, Book, Collection, LibraryDb, Opened, ScanJob, ScanRequest, Tag};
use margin_config::library_meta::ImportJob;
use margin_config::{LibrarySort, LibraryView};

use super::look::{self, ButtonSize, GAP, GAP_LARGE};
use super::window_chrome;
use crate::theme::{self, Tokens};
use crate::{Flavor, PdfCraftApp, icons};

/// The page's content width, and its side padding (the mockup's `.lib-wrap`).
const MAX_WIDTH: f32 = 1120.0;
const SIDE_PAD: f32 = 40.0;
const TOP_PAD: f32 = 22.0;
/// A grid cell's narrowest width, and the gaps between cells.
const CELL_MIN: f32 = 150.0;
const CELL_GAP_X: f32 = 22.0;
const CELL_GAP_Y: f32 = 26.0;
/// Covers are 3:4.
const COVER_ASPECT: f32 = 4.0 / 3.0;
/// Under a cover: the gap, two title lines and the status line.
const CELL_TEXT: f32 = 10.0 + 2.0 * 17.0 + 6.0 + 16.0;
/// A grid cell is one card: the cover and its text inset by this much.
const CELL_PAD: f32 = 8.0;
const LIST_ROW: f32 = 52.0;
const LIST_COVER: Vec2 = vec2(30.0, 40.0);
const HERO_COVER: f32 = 120.0;
/// The Continue reading card's padding (its radius is every card's, `look::RADIUS_CARD`).
const HERO_PAD: i8 = 16;
/// A cover's own radius and its faint light edge, so a dark first page keeps its outline.
const COVER_RADIUS: u8 = 4;
const COVER_EDGE: Color32 = Color32::from_rgba_premultiplied(26, 26, 26, 26);
/// The search field's widest and narrowest width: it gives way before it overlaps the title.
const SEARCH_WIDTH: f32 = 300.0;
const SEARCH_MIN_WIDTH: f32 = 160.0;
/// The collection name field's width (in the Collections dropdown).
const COLLECTION_FIELD_WIDTH: f32 = 192.0;
/// The top bar's controls: one control height (`look::CONTROL_ROW`).
const BUTTON: f32 = look::CONTROL_ROW;
const BAR_WIDTH: f32 = 70.0;
/// The scan's progress line under the top bar: painted over the gap, so it takes no room.
const SCAN_LINE: f32 = 2.0;
/// The shelf filter dropdown's widest label width ("Folder: …", "Tag: …").
const FILTER_MAX_WIDTH: f32 = 220.0;
/// The grid keeps its column count until the width is this far past a change point, so a
/// window edge dragged across one doesn't flip the whole grid back and forth.
const COLUMN_HYSTERESIS: f32 = 2.0;
/// Rows laid out above and below the visible part of the page, so a fast scroll never shows a
/// blank row for a frame.
const OVERSCAN: f32 = 200.0;
/// The search field's id (`library.search` focuses it).
const SEARCH_ID: &str = "margin-library-search";
/// A tag pill on a card or a row: its text size, padding, height, radius (round ends) and the
/// gap between two.
const PILL_TEXT: f32 = 10.5;
const PILL_PAD: f32 = 6.0;
const PILL_HEIGHT: f32 = 17.0;
const PILL_RADIUS: u8 = 8;
const PILL_GAP: f32 = 4.0;
/// The widest the tag pills of a list row grow.
const LIST_PILLS_WIDTH: f32 = 220.0;
/// The new tag field's width in a book's menu.
const TAG_FIELD_WIDTH: f32 = 160.0;

/// The shelves, in the order of their tab strip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shelf {
    #[default]
    Recent,
    Reading,
    Finished,
    Folders,
    Collections,
    Tags,
}

impl Shelf {
    pub const ALL: [Self; 6] = [Self::Recent, Self::Reading, Self::Finished, Self::Folders, Self::Collections, Self::Tags];

    pub fn label(self) -> &'static str {
        match self {
            Self::Recent => "Recent",
            Self::Reading => "Reading",
            Self::Finished => "Finished",
            Self::Folders => "Folders",
            Self::Collections => "Collections",
            Self::Tags => "Tags",
        }
    }
}

/// The library's state. Opens nothing in the PdfCraft flavor.
#[derive(Default)]
pub struct LibraryState {
    db: Option<LibraryDb>,
    /// Opening the database failed (said once; the library is empty this run).
    failed: Option<String>,
    books: Vec<Book>,
    collections: Vec<Collection>,
    /// The tags, by name, with how many listed books carry each.
    tags: Vec<Tag>,
    /// Tag names by id, for cards, rows and the search.
    tag_names: HashMap<i64, String>,
    /// Each book's index in `books`, by row id.
    index_of: HashMap<i64, usize>,
    /// The database changed: read the books again before the next frame shows them.
    stale: bool,
    scan: Option<ScanJob>,
    /// Files to scan (covers of documents just opened) once the running scan ends.
    pending_files: Vec<PathBuf>,
    /// The folders the last folder scan covered (a settings change scans again).
    scanned_folders: Option<Vec<String>>,
    /// View and sort came from `settings.jsonc` once.
    seeded: bool,
    pub query: String,
    pub view: LibraryView,
    pub sort: LibrarySort,
    pub shelf: Shelf,
    /// The folder or collection a Folders or Collections shelf shows.
    pub folder: Option<String>,
    pub collection: Option<i64>,
    /// The tag the Tags shelf shows.
    pub tag: Option<i64>,
    /// Search the books' text rather than their titles, authors, tags and details.
    pub in_text: bool,
    text: text::TextState,
    /// The open details sheet.
    details: Option<details::Sheet>,
    /// A running online lookup (the details sheet's, or `library.metadata_lookup`'s).
    lookup: Option<details::Lookup>,
    /// Books may have new ids to look up (`library.metadata_lookup`).
    lookup_due: bool,
    /// A running BibTeX or Zotero import.
    import: Option<ImportJob>,
    /// `library.search` asked for the keyboard.
    focus_search: bool,
    /// The name typed for a new collection.
    new_collection: String,
    /// The name typed for a new tag in a book's menu.
    new_tag: String,
    covers: covers::Covers,
    /// Bumped each time the books are read again; what is kept between frames remembers it.
    generation: u64,
    /// Each book's search text, lower case, in the order of `books`.
    haystacks: Vec<String>,
    /// The shelf as last ordered, and the Continue reading book.
    ordered: Option<Ordered>,
    /// The Continue reading line for (generation, book).
    hero_line: Option<(u64, usize, String)>,
    /// How many times the shelf was ordered (tests check it isn't every frame).
    orders_built: usize,
    /// The tab to go back to from the library (`library.open` toggles).
    back_to: Option<usize>,
    /// The library showed last frame.
    shown: bool,
    /// The page a book opening from the library asked for, shown once its tab opens (a file is
    /// read and prepared in the background, so the tab may open on a later frame).
    asked_page: Option<AskedPage>,
    /// Run scans on this thread (tests).
    pub run_inline: bool,
}

impl LibraryState {
    /// The books, as last read from the database (unordered).
    pub fn books(&self) -> &[Book] {
        &self.books
    }

    pub fn collections(&self) -> &[Collection] {
        &self.collections
    }

    pub fn tags(&self) -> &[Tag] {
        &self.tags
    }

    /// Whether the details sheet is open (the keymap's Dialog context).
    pub fn sheet_open(&self) -> bool {
        self.details.is_some()
    }

    /// Whether a scan is running.
    pub fn scanning(&self) -> bool {
        self.scan.is_some()
    }

    /// Covers decoded and ready to draw.
    pub fn covers_loaded(&self) -> usize {
        self.covers.loaded()
    }

    /// How many times the shelf has been filtered and sorted.
    pub fn orders_built(&self) -> usize {
        self.orders_built
    }

    /// The database, once opened.
    pub fn db(&self) -> Option<&LibraryDb> {
        self.db.as_ref()
    }
}

/// Opens `<data>/library.sqlite` if needed. False when there is none (said once).
fn ensure_db(app: &mut PdfCraftApp) -> bool {
    if app.margin_library.db.is_some() {
        return true;
    }
    if app.margin_library.failed.is_some() {
        return false;
    }
    let opened = match app.margin_keys.data_dir() {
        Some(data) => LibraryDb::open(&data),
        None => Err("no data folder on this system".to_owned()),
    };
    match opened {
        Ok(db) => {
            app.margin_library.db = Some(db);
            app.margin_library.stale = true;
            // Read the text of what changed while Margin was closed; look up new ids if asked.
            app.margin_library.text.read_again();
            app.margin_library.lookup_due = true;
            true
        }
        Err(err) => {
            log::warn!("library: {err}");
            app.notify(format!("The library is unavailable: {err}"));
            app.margin_library.failed = Some(err);
            false
        }
    }
}

/// Reads the books, collections and tags again if the database changed.
fn reload(app: &mut PdfCraftApp) {
    let state = &mut app.margin_library;
    if !state.stale {
        return;
    }
    state.stale = false;
    let Some(db) = &state.db else { return };
    match db.tags() {
        Ok(tags) => {
            state.tag_names = tags.iter().map(|tag| (tag.id, tag.name.clone())).collect();
            state.tags = tags;
        }
        Err(err) => log::warn!("library: {err}"),
    }
    match db.books() {
        Ok(books) => {
            state.haystacks = books.iter().map(|book| haystack(book, &state.tag_names)).collect();
            state.index_of = books.iter().enumerate().map(|(index, book)| (book.id, index)).collect();
            state.books = books;
            state.generation = state.generation.wrapping_add(1);
        }
        Err(err) => log::warn!("library: {err}"),
    }
    match db.collections() {
        Ok(collections) => state.collections = collections,
        Err(err) => log::warn!("library: {err}"),
    }
}

/// The books, read again if the database changed, for the Stats sheet (which can open before
/// the library ever showed).
pub(crate) fn refresh(app: &mut PdfCraftApp) {
    if ensure_db(app) {
        reload(app);
    }
}

/// The library's key for the book open in view `index`, looked up by its path or content before
/// its sidecar is read (`reading::opened`), so a sidecar kept in `<data>` is found under it.
pub(crate) fn book_key(app: &mut PdfCraftApp, index: usize) -> Option<String> {
    if app.flavor != Flavor::Margin || !ensure_db(app) {
        return None;
    }
    let doc = app.views.get(index).and_then(|view| app.session.get(view.id))?;
    let path = doc.path.as_deref()?;
    let fingerprint = margin_config::store::fingerprint(&doc.bytes);
    let size = u64::try_from(doc.bytes.len()).unwrap_or(u64::MAX);
    let db = app.margin_library.db.as_ref()?;
    db.book_key(path, &fingerprint, size).unwrap_or_else(|err| {
        log::warn!("library: {err}");
        None
    })
}

/// A document opened into view `index` (from `reading::opened`): it joins the library, its
/// cover is scanned if missing, its stats key is set, and it shows the page the library asked for.
pub(crate) fn opened(app: &mut PdfCraftApp, index: usize) {
    if app.flavor != Flavor::Margin {
        return;
    }
    let Some(view) = app.views.get(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let doc_id = view.id;
    let sidecar_id = super::reading::sidecar_book_id(view);
    let actual = margin_config::store::fingerprint(&doc.bytes);
    let pages = u32::try_from(doc.info.pages.len()).unwrap_or(u32::MAX);
    let path = doc.path.clone();
    let title = doc
        .info
        .title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| path.as_deref().map_or_else(|| doc.display_name(), |path| store::title_from_path(Path::new(path))));
    let author = doc.info.author.clone().unwrap_or_default();
    let size = u64::try_from(doc.bytes.len()).unwrap_or(u64::MAX);
    let mut book = actual.clone();
    if let Some(path) = &path
        && ensure_db(app)
        && let Some(db) = &app.margin_library.db
    {
        let opened = Opened { path, fingerprint: &actual, title: &title, author: &author, pages, size, sidecar_id: sidecar_id.as_deref() };
        match db.opened(&opened, store::now_unix()) {
            Ok(listed) => {
                book = listed.book_id;
                if listed.needs_cover && !app.margin_library.pending_files.iter().any(|p| p == Path::new(path)) {
                    app.margin_library.pending_files.push(PathBuf::from(path));
                }
                app.margin_library.stale = true;
                if let Some(view) = app.views.get_mut(index) {
                    super::reading::set_book_id(view, book.clone());
                }
            }
            Err(err) => log::warn!("library: {err}"),
        }
    }
    super::stats::opened(app, doc_id, book, &title, pages);
    show_asked_page(app, index);
}

/// View `index` is closing (from `reading::closing`): keep where the reader stopped.
pub(crate) fn closing(app: &mut PdfCraftApp, index: usize) {
    if app.flavor != Flavor::Margin {
        return;
    }
    save_progress(app, index);
}

/// The reader's place in view `index`, written to the library row.
fn save_progress(app: &mut PdfCraftApp, index: usize) {
    let Some(view) = app.views.get(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let Some(path) = doc.path.as_deref() else { return };
    let Some(db) = &app.margin_library.db else { return };
    let pages = doc.info.pages.len();
    let (page, at) = super::reading::place(view);
    let progress = if pages == 0 { 0.0 } else { ((page as f64 + f64::from(at)) / pages as f64).clamp(0.0, 1.0) };
    if let Err(err) = db.set_progress(path, u32::try_from(page).unwrap_or(u32::MAX), progress) {
        log::warn!("library: {err}");
    }
    app.margin_library.stale = true;
}

/// Every frame (from `reading::tick`): scan on start and when the folders change, take a scan's
/// results, scan pending covers; take an import's and a lookup's results; read the text of what
/// changed once no scan runs.
pub(crate) fn tick(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if app.flavor != Flavor::Margin || !ensure_db(app) {
        return;
    }
    forget_failed_asked_page(app);
    if !app.margin_library.seeded {
        let settings = &app.margin_keys.settings().library;
        let (view, sort) = (settings.view, settings.sort);
        let state = &mut app.margin_library;
        state.seeded = true;
        state.view = view;
        state.sort = sort;
    }
    poll_scan(app);
    let folders = app.margin_keys.settings().library.folders.clone();
    let state = &app.margin_library;
    if state.scan.is_none() {
        if state.scanned_folders.as_ref() != Some(&folders) {
            start_scan(app, ctx, folders, Vec::new());
        } else if !state.pending_files.is_empty() {
            let files = std::mem::take(&mut app.margin_library.pending_files);
            start_scan(app, ctx, Vec::new(), files);
        }
    }
    import::poll(app);
    details::poll(app);
    if app.margin_library.scan.is_none() {
        details::start_automatic(app);
    }
    text::tick(app, ctx);
}

/// The folders from settings, as paths a scan can walk: absolute ones only (`~/` is the home
/// folder), others are reported once in the log.
fn folder_paths(folders: &[String]) -> Vec<PathBuf> {
    folders
        .iter()
        .filter_map(|folder| {
            let path = match folder.strip_prefix("~/") {
                Some(rest) => dirs_home()?.join(rest),
                None => PathBuf::from(folder),
            };
            if path.is_absolute() {
                Some(path)
            } else {
                log::warn!("library.folders: {folder} is not an absolute path; skipped");
                None
            }
        })
        .collect()
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|home| !home.is_empty()).map(PathBuf::from)
}

/// Starts a scan of `folders` (a folder scan, remembered as such) or of `files` only.
fn start_scan(app: &mut PdfCraftApp, ctx: &egui::Context, folders: Vec<String>, files: Vec<PathBuf>) {
    let Some(data) = app.margin_library.db.as_ref().map(|db| db.data_dir().to_path_buf()) else { return };
    let folder_scan = files.is_empty();
    let request = ScanRequest { data, folders: folder_paths(&folders), files, threads: 0 };
    if folder_scan {
        app.margin_library.scanned_folders = Some(folders);
        if request.folders.is_empty() {
            return;
        }
    }
    app.margin_library.text.scan_started();
    let job = if app.margin_library.run_inline || app.run_inline {
        Some(ScanJob::run_inline(&request))
    } else {
        let wake = ctx.clone();
        match ScanJob::spawn(request, move || wake.request_repaint()) {
            Ok(job) => Some(job),
            Err(err) => {
                log::warn!("library: can't start the scan: {err}");
                None
            }
        }
    };
    app.margin_library.scan = job;
    poll_scan(app);
}

/// Takes what the running scan committed, and its report once it ends.
fn poll_scan(app: &mut PdfCraftApp) {
    let Some(job) = &app.margin_library.scan else { return };
    if job.take_committed() > 0 {
        app.margin_library.stale = true;
    }
    let Some(report) = job.take_report() else { return };
    app.margin_library.scan = None;
    app.margin_library.stale = true;
    // Covers may have been rewritten: decode them again.
    app.margin_library.covers.scan_ended();
    // New and changed books: their text is read, their ids looked up if asked.
    app.margin_library.text.read_again();
    app.margin_library.lookup_due = true;
    match report {
        Ok(report) if report.cancelled => app.notify("Library scan stopped"),
        Ok(report) => log::info!("library: {} PDFs, {} read, {} covers, {} missing", report.found, report.indexed, report.covers, report.missing),
        Err(err) => {
            log::warn!("library: {err}");
            app.notify(format!("The library scan failed: {err}"));
        }
    }
}

/// Rescan: the watched folders again (or stop the running scan).
pub fn rescan(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if let Some(job) = &app.margin_library.scan {
        job.cancel();
        return;
    }
    if !ensure_db(app) {
        return;
    }
    let folders = app.margin_keys.settings().library.folders.clone();
    if folders.is_empty() {
        super::welcome::add_library_folder(app);
        return;
    }
    start_scan(app, ctx, folders, Vec::new());
}

/// Margin's library ids: `library.open` (show the library, or go back to the book),
/// `library.search` (focus the search field), `library.import` (import a BibTeX file or a Zotero
/// library), `library.add_folder` (pick a folder to scan) and `app.stats` (the Stats sheet).
/// `false` for another id.
pub(crate) fn run(app: &mut PdfCraftApp, id: &str) -> bool {
    match id {
        "library.open" => match app.active {
            Some(index) => {
                save_progress(app, index);
                app.margin_library.back_to = Some(index);
                app.active = None;
            }
            None => {
                if let Some(index) = app.margin_library.back_to.filter(|index| *index < app.views.len()) {
                    app.active = Some(index);
                }
            }
        },
        "library.search" => {
            if app.active.is_some() {
                app.margin_library.back_to = app.active;
                app.active = None;
            }
            app.margin_library.focus_search = true;
        }
        "library.import" => import::pick(app),
        "library.add_folder" => super::welcome::add_library_folder(app),
        "app.stats" => app.margin_stats.toggle_sheet(),
        _ => return false,
    }
    true
}

/// What a click in the library asks for, applied after drawing.
enum Action {
    Open(String),
    ShowInFolder(String),
    Finished(i64, bool),
    Remove(i64),
    AddTo(i64, i64),
    NewCollectionWith(i64),
    RemoveFrom(i64, i64),
    DeleteCollection(i64),
    CreateCollection,
    /// Tag the book with this name (a new tag or one that exists).
    TagWith(i64, String),
    Untag(i64, i64),
    DeleteTag(i64),
    Details(i64),
    /// Open the book at this page (0-based), from a text search hit.
    OpenAt(String, u32),
    Rescan,
    Stats,
    Back,
}

/// The library view (Margin's Home).
pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    if !app.margin_library.shown {
        // Back from reading: the open books' places are the newest.
        for index in 0..app.views.len() {
            save_progress(app, index);
        }
    }
    app.margin_library.shown = true;
    tick(app, &ctx);
    reload(app);
    begin_cover_pass(&mut app.margin_library, &ctx);
    let t = Tokens::get(&ctx);
    let (order, hero) = ordered(&mut app.margin_library);
    let hero_line = hero.map(|index| hero_meta(app, index));
    let mut actions = Vec::new();
    let full = ui.available_rect_before_wrap();
    window_chrome::drag_area(app, ui, Rect::from_min_size(full.min, vec2(full.width(), super::header::HEIGHT)), ui.id().with("margin-library-drag"));
    // With window buttons over the window, the bar shares their row (the reader's header row).
    let top_pad = if window_chrome::window_buttons_shown(&ctx) { (super::header::HEIGHT - BUTTON) / 2.0 } else { TOP_PAD };
    let width = (full.width() - 2.0 * SIDE_PAD).clamp(0.0, MAX_WIDTH);
    let left = full.left() + (full.width() - width) / 2.0;
    let content = Rect::from_min_max(pos2(left, full.top() + top_pad), pos2(left + width, full.bottom()));
    let insets = BarInsets {
        traffic_lights: window_chrome::traffic_lights_inset(&ctx, content),
        window_buttons: window_chrome::window_buttons_inset(&ctx, content),
    };
    let mut page = ui.new_child(egui::UiBuilder::new().max_rect(content).layout(egui::Layout::top_down(egui::Align::Min)));
    let bar = top_bar(app, &mut page, &t, insets, &mut actions);
    scan_line(app, &page, &t, bar);
    page.add_space(GAP_LARGE * 2.0);
    // Everything under the top bar scrolls as one page, so what comes and goes above the shelf
    // (the setup card, Continue reading, Open now) never squeezes the grid. The scroll area
    // reaches into the right padding, so its floating bar never covers the last column.
    let scroll_rect = Rect::from_min_max(page.cursor().min, pos2(content.right() + SIDE_PAD / 2.0, content.bottom()));
    let mut scroller = page.new_child(egui::UiBuilder::new().max_rect(scroll_rect).layout(egui::Layout::top_down(egui::Align::Min)));
    egui::ScrollArea::vertical().id_salt("margin-library-page").auto_shrink([false, false]).show_viewport(&mut scroller, |ui, viewport| {
        ui.set_max_width(width);
        let origin = ui.max_rect().min;
        super::welcome::card(app, ui, &t);
        if app.margin_library.failed.is_some() {
            ui.label(egui::RichText::new("The library database couldn't be opened; see the log.").color(t.text_muted));
        }
        // Kept while searching: the results replace only the shelf below.
        if let (Some(index), Some(meta)) = (hero, &hero_line) {
            hero_card(app, ui, &t, index, meta, &mut actions);
            ui.add_space(GAP_LARGE * 2.0);
        }
        super::tabs::open_now(app, ui, &t);
        shelf_header(app, ui, &t, order.len(), &mut actions);
        ui.add_space(GAP_LARGE);
        let view = Viewport { origin, visible: viewport };
        if text::active(&app.margin_library) {
            text::show(app, ui, &t, &order, &mut actions);
        } else if order.is_empty() {
            empty_state(app, ui, &t);
        } else {
            match app.margin_library.view {
                LibraryView::Grid => grid(app, ui, &t, &order, view, &mut actions),
                LibraryView::List => list(app, ui, &t, &order, view, &mut actions),
            }
        }
        ui.add_space(GAP_LARGE * 2.0);
    });
    ui.allocate_rect(full, Sense::hover());
    app.margin_library.covers.trim();
    apply(app, &ctx, actions);
    details::show(app, &ctx);
}

/// Where the page's scroll content starts on screen, and the part of it on screen (in content
/// coordinates): what the grid and the list need to lay out only their visible rows.
#[derive(Clone, Copy)]
struct Viewport {
    origin: egui::Pos2,
    visible: Rect,
}

impl Viewport {
    /// The first and one past the last of `count` rows `pitch` apart from screen y `top` that are
    /// on screen (with [`OVERSCAN`]).
    fn rows(self, top: f32, pitch: f32, count: usize) -> std::ops::Range<usize> {
        if pitch <= 0.0 || count == 0 {
            return 0..0;
        }
        let start = top - self.origin.y;
        let first = ((self.visible.top() - OVERSCAN - start) / pitch).floor().max(0.0) as usize;
        let last = ((self.visible.bottom() + OVERSCAN - start) / pitch).ceil().max(0.0) as usize;
        first.min(count)..last.saturating_add(1).min(count)
    }
}

/// The scan's progress as a thin line just under the top bar, painted over the gap there, so a
/// scan never moves the bar's controls.
fn scan_line(app: &PdfCraftApp, ui: &egui::Ui, t: &Tokens, bar: Rect) {
    let Some(progress) = app.margin_library.scan.as_ref().map(ScanJob::progress) else { return };
    let done = if progress.to_read == 0 { 0.0 } else { (progress.done as f32 / progress.to_read as f32).clamp(0.0, 1.0) };
    let track = Rect::from_min_size(pos2(bar.left(), bar.bottom() + GAP), vec2(bar.width(), SCAN_LINE));
    let radius = CornerRadius::same(1);
    ui.painter().rect_filled(track, radius, t.surfaces.track);
    ui.painter().rect_filled(Rect::from_min_size(track.min, vec2(track.width() * done, SCAN_LINE)), radius, t.accent);
}

/// The library stopped showing (a book opened).
pub(crate) fn hidden(app: &mut PdfCraftApp) {
    app.margin_library.shown = false;
}

/// What the shelf was ordered for: the books' generation and every choice that filters or sorts.
struct Ordered {
    generation: u64,
    query: String,
    sort: LibrarySort,
    shelf: Shelf,
    folder: Option<String>,
    collection: Option<i64>,
    tag: Option<i64>,
    in_text: bool,
    /// The books the shelf shows, in order (indices into `books`), and their row ids (indices
    /// change when the books are read again; ids don't).
    order: Arc<[usize]>,
    ids: Vec<i64>,
    /// The Continue reading book.
    hero: Option<usize>,
}

impl Ordered {
    fn holds_for(&self, state: &LibraryState) -> bool {
        self.generation == state.generation && self.same_choices(state)
    }

    /// Everything but the books themselves is as it was.
    fn same_choices(&self, state: &LibraryState) -> bool {
        self.query == state.query
            && self.sort == state.sort
            && self.shelf == state.shelf
            && self.folder == state.folder
            && self.collection == state.collection
            && self.tag == state.tag
            && self.in_text == state.in_text
    }
}

/// The text a book is found by: title, author, file name and tag names, lower case.
fn haystack(book: &Book, tag_names: &HashMap<i64, String>) -> String {
    let name = Path::new(&book.path).file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    let mut text = format!("{}\n{}\n{name}", book.title, book.author);
    for tag in book.tags.iter().filter_map(|tag| tag_names.get(tag)) {
        text.push('\n');
        text.push_str(tag);
    }
    text.to_lowercase()
}

/// The shelf's books in order and the Continue reading book: kept, and made again only when the
/// books, the search, the sort, the shelf or its folder or collection changed. While a scan adds
/// books, the books already shown keep their places and new ones go after them, so the grid
/// doesn't reshuffle under the reader for the length of the scan; the next order after it ends
/// sorts everything.
fn ordered(state: &mut LibraryState) -> (Arc<[usize]>, Option<usize>) {
    if let Some(kept) = state.ordered.as_ref().filter(|kept| kept.holds_for(state)) {
        return (kept.order.clone(), kept.hero);
    }
    let mut order = shelf_order(state);
    if state.scan.is_some()
        && let Some(kept) = state.ordered.as_ref().filter(|kept| kept.same_choices(state))
    {
        let place: HashMap<i64, usize> = kept.ids.iter().enumerate().map(|(at, id)| (*id, at)).collect();
        // A stable sort: kept books by their old place, the new ones after them in shelf order.
        order.sort_by_key(|index| state.books.get(*index).and_then(|book| place.get(&book.id).copied()).unwrap_or(usize::MAX));
    }
    let ids = order.iter().filter_map(|index| state.books.get(*index).map(|book| book.id)).collect();
    let order: Arc<[usize]> = order.into();
    let hero = hero_book(state);
    state.orders_built = state.orders_built.saturating_add(1);
    state.ordered = Some(Ordered {
        generation: state.generation,
        query: state.query.clone(),
        sort: state.sort,
        shelf: state.shelf,
        folder: state.folder.clone(),
        collection: state.collection,
        tag: state.tag,
        in_text: state.in_text,
        order: order.clone(),
        ids,
        hero,
    });
    (order, hero)
}

/// The indices of the books the shelf, folder, collection or tag and search show, sorted. The
/// search matches the title, author, file name and tags as typed, and (through the full-text
/// index) the words of the details an import or a lookup filled: year, DOI, venue, abstract. In
/// "In text" mode the shelf isn't searched: the text results show instead.
fn shelf_order(state: &LibraryState) -> Vec<usize> {
    let query = if state.in_text { String::new() } else { state.query.trim().to_lowercase() };
    let in_details: HashSet<i64> = match (&state.db, query.is_empty()) {
        (Some(db), false) => db.find_books(&query).unwrap_or_else(|err| {
            log::warn!("library: {err}");
            Vec::new()
        }),
        _ => Vec::new(),
    }
    .into_iter()
    .collect();
    let mut order: Vec<usize> = state
        .books
        .iter()
        .enumerate()
        .filter(|(_, book)| match state.shelf {
            Shelf::Recent => true,
            Shelf::Reading => book.is_reading(),
            Shelf::Finished => book.finished,
            Shelf::Folders => state.folder.as_deref().is_none_or(|folder| Path::new(&book.path).starts_with(folder)),
            Shelf::Collections => state.collection.is_none_or(|collection| book.collections.contains(&collection)),
            Shelf::Tags => state.tag.map_or(!book.tags.is_empty(), |tag| book.tags.contains(&tag)),
        })
        .filter(|(index, book)| {
            query.is_empty() || in_details.contains(&book.id) || state.haystacks.get(*index).is_some_and(|text| text.contains(&query))
        })
        .map(|(index, _)| index)
        .collect();
    let books = &state.books;
    let title = |index: &usize| books.get(*index).map(|book| book.title.to_lowercase()).unwrap_or_default();
    match state.sort {
        LibrarySort::Recent => order.sort_by_key(|index| {
            books
                .get(*index)
                .map(|book| std::cmp::Reverse((book.last_opened.unwrap_or(0), book.added, book.id)))
                .unwrap_or(std::cmp::Reverse((0, 0, 0)))
        }),
        LibrarySort::Title => order.sort_by_cached_key(title),
        LibrarySort::Author => {
            order.sort_by_cached_key(|index| (books.get(*index).map(|book| book.author.to_lowercase()).unwrap_or_default(), title(index)))
        }
        LibrarySort::Progress => order.sort_by(|a, b| {
            let progress = |index: &usize| books.get(*index).map_or(0.0, |book| if book.finished { 1.0 } else { book.progress });
            progress(b).total_cmp(&progress(a))
        }),
        LibrarySort::Added => order.sort_by_key(|index| std::cmp::Reverse(books.get(*index).map_or((0, 0), |book| (book.added, book.id)))),
    }
    order
}

/// The book "Continue reading" offers: the last one opened that is started and not finished.
fn hero_book(state: &LibraryState) -> Option<usize> {
    state
        .books
        .iter()
        .enumerate()
        .filter(|(_, book)| book.is_reading() && book.last_opened.is_some())
        .max_by_key(|(_, book)| (book.last_opened, book.id))
        .map(|(index, _)| index)
}

/// "p. 6 of 14 · 43% · ~34 min left", kept until the books are read again.
fn hero_meta(app: &mut PdfCraftApp, index: usize) -> String {
    let generation = app.margin_library.generation;
    if let Some((_, _, line)) = app.margin_library.hero_line.as_ref().filter(|(at, book, _)| *at == generation && *book == index) {
        return line.clone();
    }
    let Some(book) = app.margin_library.books.get(index) else { return String::new() };
    let (page, pages, progress, book_id) = (book.page, book.pages, book.progress, book.book_id.clone());
    let mut parts = Vec::new();
    if pages > 0 {
        parts.push(format!("p. {} of {pages}", page.saturating_add(1).min(pages)));
    }
    parts.push(format!("{}%", (progress * 100.0).round() as u32));
    let left = pages.saturating_sub(page.saturating_add(1));
    if super::stats::ensure_open(app)
        && let Some(summary) = super::stats::summary(app, &book_id)
        && let Some(seconds) = summary.seconds_left(left)
    {
        parts.push(format!("~{} left", margin_config::stats::format_duration(seconds)));
    }
    let line = parts.join("   ");
    app.margin_library.hero_line = Some((generation, index, line.clone()));
    line
}

/// What the window buttons cover of the top bar's row: the traffic lights at its left, Margin's
/// own window buttons at its right (`window_chrome`).
#[derive(Clone, Copy)]
struct BarInsets {
    traffic_lights: f32,
    window_buttons: f32,
}

/// The top bar on one [`BUTTON`]-tall row: Back (its slot kept when there is no book to go back
/// to, so the title never moves), the title, then on the right the search field, "In text" and
/// the icon buttons, every control one height. Nothing in it changes width with the state: a
/// scan shows under the bar ([`scan_line`]) and in the Rescan button's tooltip. Returns the
/// row's rect.
fn top_bar(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, insets: BarInsets, actions: &mut Vec<Action>) -> Rect {
    look::bar_row(ui, BUTTON, |ui| {
        ui.spacing_mut().item_spacing.x = GAP_LARGE;
        // The gap after the space is the row's own item spacing.
        if insets.traffic_lights > 0.0 {
            ui.add_space((insets.traffic_lights - GAP_LARGE).max(0.0));
        }
        let back = app.margin_library.back_to.filter(|index| *index < app.views.len());
        if back.is_some() {
            let keys = look::shortcut(app, "library.open");
            if look::icon_button(ui, "chevron-left", BUTTON, false, "Back to reading", keys.as_deref()).clicked() {
                actions.push(Action::Back);
            }
        } else {
            ui.allocate_exact_size(Vec2::splat(BUTTON), Sense::hover());
        }
        ui.label(egui::RichText::new("Library").font(theme::semibold(18.0)).color(t.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = GAP / 2.0;
            if insets.window_buttons > 0.0 {
                ui.add_space(insets.window_buttons);
            }
            super::header::app_menu_button(app, ui, BUTTON);
            let open_keys = look::shortcut(app, "file.open");
            if look::icon_button(ui, "plus", BUTTON, false, "Open PDF…", open_keys.as_deref()).clicked() {
                app.open_dialog();
            }
            let stats_keys = look::shortcut(app, "app.stats");
            if look::icon_button(ui, "clock", BUTTON, app.margin_stats.sheet_open(), "Reading stats", stats_keys.as_deref()).clicked() {
                actions.push(Action::Stats);
            }
            let scanning = app.margin_library.scan.as_ref().map(ScanJob::progress);
            let tip = match &scanning {
                Some(progress) => format!("Stop scanning ({} of {})", progress.done, progress.to_read),
                None => "Rescan folders".to_owned(),
            };
            if look::icon_button(ui, if scanning.is_some() { "x" } else { "rotate-cw" }, BUTTON, scanning.is_some(), &tip, None).clicked() {
                actions.push(Action::Rescan);
            }
            let list = app.margin_library.view == LibraryView::List;
            if look::icon_button(ui, "list", BUTTON, list, "List", None).clicked() {
                app.margin_library.view = LibraryView::List;
            }
            if look::icon_button(ui, "layout-grid", BUTTON, !list, "Grid", None).clicked() {
                app.margin_library.view = LibraryView::Grid;
            }
            let in_text = app.margin_library.in_text;
            let tip = "Search the pages and highlights of the books instead of their titles, authors, tags and details";
            if look::Button::secondary(ButtonSize::Standard, "In text").selected(in_text).tip(tip).show(ui).clicked() {
                app.margin_library.in_text = !in_text;
            }
            // What is left of the row past the title, less a gap: the field narrows before it
            // would ever run over the title.
            let search_width = (ui.available_width() - GAP_LARGE).clamp(SEARCH_MIN_WIDTH, SEARCH_WIDTH);
            search_field(app, ui, t, search_width);
        });
    })
    .response
    .rect
}

fn search_field(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, width: f32) {
    let id = egui::Id::new(SEARCH_ID);
    look::field_well(ui, id, vec2(width, BUTTON), |ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        let (icon, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
        icons::paint(ui, icon, "search", 15.0, t.text_muted);
        let keys = look::shortcut(app, "library.search").map(|keys| format!("  ({keys})")).unwrap_or_default();
        let what = if app.margin_library.in_text { "Search the books' text" } else { "Search titles" };
        let field = egui::TextEdit::singleline(&mut app.margin_library.query)
            .id(id)
            .frame(egui::Frame::NONE)
            .margin(Vec2::ZERO)
            .hint_text(format!("{what}{keys}"))
            .desired_width(ui.available_width());
        let resp = ui.add(field);
        if std::mem::take(&mut app.margin_library.focus_search) {
            resp.request_focus();
        }
    });
}

/// The Continue reading card. Its author line is there even when the book has no author, so the
/// card is one height for every book.
fn hero_card(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize, meta: &str, actions: &mut Vec<Action>) {
    let Some(book) = app.margin_library.books.get(index).cloned() else { return };
    let frame = look::card_frame(t).shadow(t.surfaces.shadow_card).inner_margin(HERO_PAD);
    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 22.0;
            let (cover, cover_resp) = ui.allocate_exact_size(vec2(HERO_COVER, HERO_COVER * COVER_ASPECT * 0.92), Sense::click());
            paint_cover(app, ui, t, cover, &book);
            if cover_resp.clicked() {
                actions.push(Action::Open(book.path.clone()));
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(egui::RichText::new("CONTINUE READING").font(theme::semibold(11.0)).color(t.accent).extra_letter_spacing(0.9));
                ui.add_space(2.0);
                ui.add(egui::Label::new(egui::RichText::new(&book.title).font(theme::semibold(20.0)).color(t.text)).truncate());
                // A space keeps the line's height when there is no author.
                let author = if book.author.is_empty() { " " } else { book.author.as_str() };
                ui.add(egui::Label::new(egui::RichText::new(author).font(theme::regular(13.0)).color(t.text_muted)).truncate());
                ui.add_space(GAP);
                ui.label(egui::RichText::new(meta).font(theme::regular(12.5)).color(t.text_muted));
                ui.add_space(GAP * 2.0);
                if look::Button::primary(ButtonSize::Prominent, "Continue").trailing("chevron-right").show(ui).clicked() {
                    actions.push(Action::Open(book.path.clone()));
                }
            });
        });
    });
}

/// The shelf's name and count, then the shelves as a segmented group, Sort and (on Folders,
/// Collections and Tags) the shelf's filter on the right, on one [`look::CONTROL_ROW`] row. The
/// filter is a dropdown, not a row of chips, so switching shelves never inserts a row.
fn shelf_header(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, count: usize, actions: &mut Vec<Action>) {
    look::control_row(ui, |ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        ui.add(egui::Label::new(look::section_caps(t, app.margin_library.shelf.label(), Some(count))).selectable(false));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let current = app.margin_library.shelf;
            let segments = Shelf::ALL.map(|shelf| look::Segment { label: shelf.label(), selected: shelf == current, tip: None });
            if let Some(shelf) = look::segmented(ui, &segments).and_then(|i| Shelf::ALL.get(i)) {
                app.margin_library.shelf = *shelf;
            }
            ui.add_space(GAP_LARGE);
            sort_menu(app, ui);
            if matches!(app.margin_library.shelf, Shelf::Folders | Shelf::Collections | Shelf::Tags) {
                shelf_filter(app, ui, t, actions);
            }
        });
    });
}

/// The sort orders, in the menu's order, with their labels.
const SORTS: [(LibrarySort, &str); 5] = [
    (LibrarySort::Recent, "Recent"),
    (LibrarySort::Title, "Title"),
    (LibrarySort::Author, "Author"),
    (LibrarySort::Progress, "Progress"),
    (LibrarySort::Added, "Added"),
];

/// Sort: a compact secondary button with a chevron, as wide as its widest label, opening the
/// sort orders as a menu.
fn sort_menu(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let current = SORTS.iter().find(|(sort, _)| *sort == app.margin_library.sort).map_or("Recent", |(_, label)| *label);
    let text = format!("Sort: {current}");
    let labels: Vec<String> = SORTS.iter().map(|(_, label)| format!("Sort: {label}")).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let button = look::Button::secondary(ButtonSize::Compact, &text).trailing("chevron-down");
    let width = button.widest(ui, &labels);
    let button = button.fixed_width(width).show(ui);
    egui::Popup::menu(&button).show(|ui| {
        look::menu_body(ui);
        for (sort, label) in SORTS {
            if look::menu_row(ui, None, label, None, app.margin_library.sort == sort).clicked() {
                app.margin_library.sort = sort;
            }
        }
    });
}

/// The Folders shelf's folder, the Collections shelf's collection or the Tags shelf's tag, as
/// one dropdown beside Sort: "All" and every choice (the chosen one checked), then what the
/// shelf can do: add a library folder, create a collection, delete the chosen collection or tag.
fn shelf_filter(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, actions: &mut Vec<Action>) {
    let state = &app.margin_library;
    let (noun, current) = match state.shelf {
        Shelf::Folders => ("Folder", state.folder.as_deref().map(crate::folders_ui::folder_name)),
        Shelf::Collections => ("Collection", state.collection.and_then(|id| state.collections.iter().find(|c| c.id == id)).map(|c| c.name.clone())),
        _ => ("Tag", state.tag.and_then(|id| state.tags.iter().find(|tag| tag.id == id)).map(|tag| tag.name.clone())),
    };
    let text = format!("{noun}: {}", current.as_deref().unwrap_or("All"));
    let button = look::Button::secondary(ButtonSize::Compact, &text).trailing("chevron-down").tip(&text);
    let width = button.width(ui).min(FILTER_MAX_WIDTH);
    let button = button.fixed_width(width).show(ui);
    look::popover(&button, egui::Id::new(("margin-shelf-filter", noun))).show(|ui| {
        look::menu_body(ui);
        match app.margin_library.shelf {
            Shelf::Folders => folder_choices(app, ui),
            Shelf::Collections => collection_choices(app, ui, actions),
            _ => tag_choices(app, ui, t, actions),
        }
    });
}

/// The Folders dropdown: All and each watched folder, or only "Add a library folder…" while
/// there are none.
fn folder_choices(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let folders: Vec<PathBuf> = folder_paths(&app.margin_keys.settings().library.folders);
    if folders.is_empty() {
        if look::menu_row(ui, Some("folder-plus"), "Add a library folder…", None, false).clicked() {
            ui.close();
            super::welcome::add_library_folder(app);
        }
        return;
    }
    if look::menu_row(ui, None, "All", None, app.margin_library.folder.is_none()).clicked() {
        app.margin_library.folder = None;
        ui.close();
    }
    for folder in folders {
        let folder = folder.to_string_lossy().into_owned();
        let on = app.margin_library.folder.as_deref() == Some(folder.as_str());
        let name = crate::folders_ui::folder_name(&folder);
        if look::menu_row(ui, None, &name, None, on).on_hover_text(&folder).clicked() {
            app.margin_library.folder = Some(folder);
            ui.close();
        }
    }
}

/// The Collections dropdown: All and each collection with its count, Delete for the chosen one,
/// and the field that creates one.
fn collection_choices(app: &mut PdfCraftApp, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    if look::menu_row(ui, None, "All", None, app.margin_library.collection.is_none()).clicked() {
        app.margin_library.collection = None;
        ui.close();
    }
    let collections = app.margin_library.collections.clone();
    for collection in &collections {
        let on = app.margin_library.collection == Some(collection.id);
        if look::menu_row(ui, None, &format!("{}  {}", collection.name, collection.count), None, on).clicked() {
            app.margin_library.collection = Some(collection.id);
            ui.close();
        }
    }
    if let Some(chosen) = collections.iter().find(|c| Some(c.id) == app.margin_library.collection) {
        ui.separator();
        if look::menu_row(ui, Some("trash-2"), &format!("Delete collection “{}”", chosen.name), None, false).clicked() {
            actions.push(Action::DeleteCollection(chosen.id));
            ui.close();
        }
    }
    ui.separator();
    new_collection_row(app, ui, actions);
}

/// The Tags dropdown: All and each tag with its count (a hint while there are none), and Delete
/// for the chosen one.
fn tag_choices(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, actions: &mut Vec<Action>) {
    let tags = app.margin_library.tags.clone();
    if tags.is_empty() {
        ui.label(egui::RichText::new("No tags yet. Right-click a book to tag it.").color(t.text_muted));
        return;
    }
    if look::menu_row(ui, None, "All", None, app.margin_library.tag.is_none()).clicked() {
        app.margin_library.tag = None;
        ui.close();
    }
    for tag in &tags {
        let on = app.margin_library.tag == Some(tag.id);
        if look::menu_row(ui, None, &format!("{}  {}", tag.name, tag.count), None, on).clicked() {
            app.margin_library.tag = Some(tag.id);
            ui.close();
        }
    }
    if let Some(chosen) = tags.iter().find(|tag| Some(tag.id) == app.margin_library.tag) {
        ui.separator();
        if look::menu_row(ui, Some("trash-2"), &format!("Delete tag “{}”", chosen.name), None, false).clicked() {
            actions.push(Action::DeleteTag(chosen.id));
            ui.close();
        }
    }
}

/// The "Collection name" field and its primary Add, off while the name is blank.
fn new_collection_row(app: &mut PdfCraftApp, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    look::control_row(ui, |ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        let id = egui::Id::new("margin-new-collection");
        let resp = look::text_field(ui, id, &mut app.margin_library.new_collection, "New collection", COLLECTION_FIELD_WIDTH);
        ui.ctx().accesskit_node_builder(resp.id, |node| node.set_label("Collection name"));
        let named = !app.margin_library.new_collection.trim().is_empty();
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let add = ui.add_enabled_ui(named, |ui| look::button_primary(ui, ButtonSize::Standard, "Add")).inner.clicked();
        if named && (enter || add) {
            actions.push(Action::CreateCollection);
        }
    });
}

fn empty_state(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let text = if !app.margin_library.query.trim().is_empty() {
        "No book matches the search."
    } else if app.margin_library.books.is_empty() {
        "Books you open appear here, and the PDFs in your library folders."
    } else {
        "Nothing on this shelf yet."
    };
    ui.add_space(GAP_LARGE * 2.0);
    ui.label(egui::RichText::new(text).font(theme::regular(13.0)).color(t.text_muted));
    if app.margin_library.books.is_empty() && app.margin_keys.settings().library.folders.is_empty() && !super::welcome::wanted(app) {
        ui.add_space(GAP);
        if look::Button::secondary(ButtonSize::Standard, "Add a library folder…").icon("folder-plus").show(ui).clicked() {
            super::welcome::add_library_folder(app);
        }
    }
}

/// How many columns of at least [`CELL_MIN`] fit `width`, kept from the last frame while the
/// width is within [`COLUMN_HYSTERESIS`] of where the count changes.
fn grid_columns(ctx: &egui::Context, width: f32) -> usize {
    let id = egui::Id::new("margin-library-columns");
    let fit = |width: f32| (((width + CELL_GAP_X) / (CELL_MIN + CELL_GAP_X)).floor().max(0.0) as usize).max(1);
    let fresh = fit(width);
    let columns = match ctx.data(|d| d.get_temp::<usize>(id)) {
        Some(last) if last != fresh && fit(width - COLUMN_HYSTERESIS) <= last && last <= fit(width + COLUMN_HYSTERESIS) => last,
        _ => fresh,
    };
    ctx.data_mut(|d| d.insert_temp(id, columns));
    columns
}

/// The cover grid in the page's scroll area: its whole height is taken, but only the rows on
/// screen are laid out.
fn grid(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, order: &[usize], view: Viewport, actions: &mut Vec<Action>) {
    let width = ui.available_width();
    let columns = grid_columns(ui.ctx(), width);
    let cell_w = ((width - CELL_GAP_X * (columns as f32 - 1.0)) / columns as f32).max(1.0);
    let cover_h = (cell_w - 2.0 * CELL_PAD).max(1.0) * COVER_ASPECT;
    let row_h = cover_h + CELL_TEXT + 2.0 * CELL_PAD;
    let rows = order.len().div_ceil(columns);
    let pitch = row_h + CELL_GAP_Y;
    let top = ui.cursor().top();
    let area = Rect::from_min_size(pos2(ui.cursor().left(), top), vec2(width, (rows as f32 * pitch - CELL_GAP_Y).max(0.0)));
    for row in view.rows(top, pitch, rows) {
        let y = top + row as f32 * pitch;
        for column in 0..columns {
            let Some(&index) = order.get(row * columns + column) else { break };
            let cell = Rect::from_min_size(pos2(area.left() + column as f32 * (cell_w + CELL_GAP_X), y), vec2(cell_w, row_h));
            book_cell(app, ui, t, index, cell, cover_h, actions);
        }
    }
    ui.allocate_rect(area, Sense::hover());
}

/// One book in the grid: a card (cover, title, status) with the S shadow, its fill and edge
/// lifting to the hover state under the pointer.
fn book_cell(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize, cell: Rect, cover_h: f32, actions: &mut Vec<Action>) {
    let Some(book) = app.margin_library.books.get(index).cloned() else { return };
    let resp = ui.interact(cell, ui.id().with(("book", book.id)), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &book.title));
    let radius = CornerRadius::same(look::RADIUS_CARD);
    ui.painter().add(t.surfaces.shadow_card.as_shape(cell, radius));
    let lift = super::motion::hover(ui.ctx(), resp.id, resp.hovered());
    let edge = t.border.lerp_to_gamma(t.surfaces.edge_hover, lift);
    ui.painter().rect(cell, radius, t.card.lerp_to_gamma(t.hover, lift), look::edge_stroke(edge), egui::StrokeKind::Inside);
    look::focus_ring(ui, t, cell, &resp, look::RADIUS_CARD);
    let inner = cell.shrink(CELL_PAD);
    let cover = Rect::from_min_size(inner.min, vec2(inner.width(), cover_h));
    paint_cover(app, ui, t, cover, &book);
    let names = tag_names(&app.margin_library, &book);
    let pills = tag_pills(ui, t, &names, cover.width() - 2.0 * PILL_GAP);
    paint_pills(ui, t, &pills, pos2(cover.left() + PILL_GAP, cover.bottom() - PILL_GAP - PILL_HEIGHT));
    let mut job = egui::text::LayoutJob::simple(book.title.clone(), theme::medium(12.5), t.text, inner.width());
    job.wrap.max_rows = 2;
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    ui.painter().galley(pos2(inner.left(), cover.bottom() + 10.0), galley, t.text);
    status_line(ui, t, &book, Rect::from_min_max(pos2(inner.left(), inner.bottom() - 16.0), inner.max));
    let resp = look::tooltip(resp, &book.path, None);
    if resp.clicked() {
        actions.push(Action::Open(book.path.clone()));
    }
    context_menu(app, &resp, &book, actions);
}

/// "▬▬ 43%   14 pp", "new", "done".
fn status_line(ui: &egui::Ui, t: &Tokens, book: &Book, rect: Rect) {
    let font = theme::regular(11.5);
    let painter = ui.painter();
    let y = rect.center().y;
    painter.text(pos2(rect.right(), y), Align2::RIGHT_CENTER, format!("{} pp", book.pages), font.clone(), t.text_muted);
    if book.finished {
        painter.text(pos2(rect.left(), y), Align2::LEFT_CENTER, "done", font, t.text_muted);
    } else if book.is_reading() {
        let bar = Rect::from_min_size(pos2(rect.left(), y - 1.5), vec2(BAR_WIDTH.min(rect.width() / 2.0), 3.0));
        painter.rect_filled(bar, CornerRadius::same(2), t.surfaces.track);
        let fill = Rect::from_min_size(bar.min, vec2(bar.width() * book.progress.clamp(0.0, 1.0) as f32, bar.height()));
        painter.rect_filled(fill, CornerRadius::same(2), t.accent);
        let percent = format!("{}%", (book.progress * 100.0).round() as u32);
        painter.text(pos2(bar.right() + GAP, y), Align2::LEFT_CENTER, percent, font, t.text_muted);
    } else {
        painter.text(pos2(rect.left(), y), Align2::LEFT_CENTER, "new", font, t.text_muted);
    }
}

/// The list view: one row per book in a panel-coloured, edged container, a divider between
/// rows; in the page's scroll area, laying out only the rows on screen like the grid.
fn list(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, order: &[usize], view: Viewport, actions: &mut Vec<Action>) {
    let width = ui.available_width();
    let pitch = LIST_ROW + look::EDGE;
    let height = order.len() as f32 * pitch - look::EDGE + 2.0 * look::EDGE;
    let container = Rect::from_min_size(ui.cursor().min, vec2(width, height.max(0.0)));
    ui.painter().rect(container, CornerRadius::same(look::RADIUS_CARD), t.panel, look::edge_stroke(t.border), egui::StrokeKind::Inside);
    let top = container.top() + look::EDGE;
    let row_width = (width - 2.0 * look::EDGE).max(0.0);
    for row in view.rows(top, pitch, order.len()) {
        let Some(&index) = order.get(row) else { continue };
        let rect = Rect::from_min_size(pos2(container.left() + look::EDGE, top + row as f32 * pitch), vec2(row_width, LIST_ROW));
        list_row(app, ui, t, index, rect, actions);
    }
    ui.allocate_rect(container, Sense::hover());
}

/// One book's row at `rect`: cover, title and author (or path), its tag pills in a slot every
/// row reserves (so every title is cut at the same x), and the status on the right.
fn list_row(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize, rect: Rect, actions: &mut Vec<Action>) {
    let Some(book) = app.margin_library.books.get(index).cloned() else { return };
    let resp = ui.interact(rect, ui.id().with(("book-row", book.id)), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &book.title));
    look::row_fill(ui, t, rect, &resp, false);
    ui.painter().hline(rect.x_range(), rect.bottom() + look::EDGE / 2.0, Stroke::new(look::EDGE, t.divider));
    let cover = Rect::from_min_size(pos2(rect.left() + GAP, rect.center().y - LIST_COVER.y / 2.0), LIST_COVER);
    paint_cover(app, ui, t, cover, &book);
    let text_left = cover.right() + GAP_LARGE;
    let status = Rect::from_min_max(pos2(rect.right() - 200.0, rect.top()), pos2(rect.right() - GAP, rect.bottom()));
    let names = tag_names(&app.margin_library, &book);
    let pills = tag_pills(ui, t, &names, LIST_PILLS_WIDTH);
    let slot_right = status.left() - GAP_LARGE;
    paint_pills(ui, t, &pills, pos2(slot_right - pills.width, rect.center().y - PILL_HEIGHT / 2.0));
    let text_width = (slot_right - LIST_PILLS_WIDTH - GAP_LARGE - text_left).max(GAP);
    crate::widgets::row_text(ui, pos2(text_left, rect.top() + 17.0), crate::bidi::visual(&book.title), theme::medium(13.0), t.text, text_width);
    let detail = if book.author.is_empty() { book.path.clone() } else { book.author.clone() };
    crate::widgets::row_text(ui, pos2(text_left, rect.top() + 35.0), crate::bidi::visual(&detail), theme::regular(11.5), t.text_muted, text_width);
    status_line(ui, t, &book, status);
    if resp.clicked() {
        actions.push(Action::Open(book.path.clone()));
    }
    context_menu(app, &resp, &book, actions);
}

/// The names of the book's tags, A to Z.
fn tag_names<'a>(state: &'a LibraryState, book: &Book) -> Vec<&'a str> {
    let mut names: Vec<&str> = book.tags.iter().filter_map(|tag| state.tag_names.get(tag).map(String::as_str)).collect();
    names.sort_by_key(|name| name.to_lowercase());
    names
}

/// Tag pills laid out to fit a width: as many as fit, then "+n" for the rest.
struct Pills {
    galleys: Vec<Arc<egui::Galley>>,
    width: f32,
}

fn tag_pills(ui: &egui::Ui, t: &Tokens, names: &[&str], max_width: f32) -> Pills {
    let mut pills = Pills { galleys: Vec::new(), width: 0.0 };
    let layout = |text: String| ui.painter().layout_no_wrap(text, theme::regular(PILL_TEXT), t.text_muted);
    for (index, name) in names.iter().enumerate() {
        let gap = if pills.galleys.is_empty() { 0.0 } else { PILL_GAP };
        let galley = layout((*name).to_owned());
        let next = pills.width + gap + galley.size().x + 2.0 * PILL_PAD;
        if next > max_width {
            let more = layout(format!("+{}", names.len() - index));
            let with_more = pills.width + gap + more.size().x + 2.0 * PILL_PAD;
            if with_more <= max_width {
                pills.width = with_more;
                pills.galleys.push(more);
            }
            break;
        }
        pills.width = next;
        pills.galleys.push(galley);
    }
    pills
}

/// Paints `pills` left to right from `left_top`: a rounded floating chip each, muted text.
fn paint_pills(ui: &egui::Ui, t: &Tokens, pills: &Pills, left_top: egui::Pos2) {
    let fill = t.surfaces.floating;
    let mut x = left_top.x;
    for galley in &pills.galleys {
        let rect = Rect::from_min_size(pos2(x, left_top.y), vec2(galley.size().x + 2.0 * PILL_PAD, PILL_HEIGHT));
        ui.painter().rect(rect, CornerRadius::same(PILL_RADIUS), fill, look::edge_stroke(t.border), egui::StrokeKind::Inside);
        ui.painter().galley(pos2(rect.left() + PILL_PAD, rect.center().y - galley.size().y / 2.0), galley.clone(), t.text_muted);
        x = rect.right() + PILL_GAP;
    }
}

/// The book's menu: open, details, show in folder, finished, collections, tags, remove. Margin's
/// menu rows, so every label starts at the same x and the menu is one width for every book.
fn context_menu(app: &mut PdfCraftApp, resp: &egui::Response, book: &Book, actions: &mut Vec<Action>) {
    resp.context_menu(|ui| {
        look::menu_body(ui);
        if look::menu_row(ui, Some("book-open"), "Open", None, false).clicked() {
            actions.push(Action::Open(book.path.clone()));
        }
        if look::menu_row(ui, Some("file-text"), "Details\u{2026}", None, false).clicked() {
            actions.push(Action::Details(book.id));
        }
        if look::menu_row(ui, Some("folder-open"), "Show in folder", None, false).clicked() {
            actions.push(Action::ShowInFolder(book.path.clone()));
        }
        let finished = if book.finished { "Mark as not finished" } else { "Mark as finished" };
        if look::menu_row(ui, Some("circle-check"), finished, None, false).clicked() {
            actions.push(Action::Finished(book.id, !book.finished));
        }
        look::submenu(ui, "layers", "Add to collection", false, |ui| {
            for collection in &app.margin_library.collections {
                let member = book.collections.contains(&collection.id);
                if ui.add_enabled_ui(!member, |ui| look::menu_row(ui, None, &collection.name, None, member)).inner.clicked() {
                    actions.push(Action::AddTo(collection.id, book.id));
                }
            }
            if !app.margin_library.collections.is_empty() {
                ui.separator();
            }
            if look::menu_row(ui, Some("plus"), "New collection", None, false).clicked() {
                actions.push(Action::NewCollectionWith(book.id));
            }
        });
        tags_menu(app, ui, book, actions);
        if let Some(collection) =
            app.margin_library.collection.filter(|c| app.margin_library.shelf == Shelf::Collections && book.collections.contains(c))
            && look::menu_row(ui, Some("minus"), "Remove from this collection", None, false).clicked()
        {
            actions.push(Action::RemoveFrom(collection, book.id));
        }
        ui.separator();
        if look::menu_row(ui, Some("trash-2"), "Remove from library", None, false).clicked() {
            actions.push(Action::Remove(book.id));
        }
    });
}

/// Tags ▸: every tag, checked when the book has it (a click puts it on the book or takes it
/// off, and the submenu stays open for the next one), then a field that tags the book with a new
/// name on Enter.
fn tags_menu(app: &mut PdfCraftApp, ui: &mut egui::Ui, book: &Book, actions: &mut Vec<Action>) {
    look::submenu(ui, "tag", "Tags", true, |ui| {
        for tag in &app.margin_library.tags {
            let on = book.tags.contains(&tag.id);
            if look::menu_row(ui, None, &tag.name, None, on).clicked() {
                actions.push(if on { Action::Untag(book.id, tag.id) } else { Action::TagWith(book.id, tag.name.clone()) });
            }
        }
        if !app.margin_library.tags.is_empty() {
            ui.separator();
        }
        let resp = look::text_field(ui, egui::Id::new(("margin-new-tag", book.id)), &mut app.margin_library.new_tag, "New tag", TAG_FIELD_WIDTH);
        let entered = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if entered && !app.margin_library.new_tag.trim().is_empty() {
            actions.push(Action::TagWith(book.id, std::mem::take(&mut app.margin_library.new_tag)));
            ui.close();
        }
    });
}

/// Draws the book's cover in `rect` (the top of the page, filling the width), or a placeholder
/// while it isn't decoded: radius 4, the S shadow and a faint light edge (§2).
fn paint_cover(app: &mut PdfCraftApp, ui: &egui::Ui, t: &Tokens, rect: Rect, book: &Book) {
    let radius = CornerRadius::same(COVER_RADIUS);
    ui.painter().add(t.surfaces.shadow_card.as_shape(rect, radius));
    match cover_texture(app, ui.ctx(), book) {
        Some(texture) => {
            let size = texture.size_vec2();
            // Fill the width; a page taller than 3:4 is cut at the bottom, a wider one centred.
            let image_aspect = size.y / size.x.max(1.0);
            let rect_aspect = rect.height() / rect.width().max(1.0);
            let uv = if image_aspect >= rect_aspect {
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, (rect_aspect / image_aspect).clamp(0.0, 1.0)))
            } else {
                let share = (image_aspect / rect_aspect).clamp(0.0, 1.0);
                Rect::from_min_max(pos2((1.0 - share) / 2.0, 0.0), pos2((1.0 + share) / 2.0, 1.0))
            };
            ui.painter().add(egui::epaint::RectShape::filled(rect, radius, Color32::WHITE).with_texture(texture.id(), uv));
        }
        None => {
            ui.painter().rect_filled(rect, radius, t.hover);
            icons::paint(ui, Rect::from_center_size(rect.center(), Vec2::splat(28.0)), "file-text", 24.0, t.text_muted);
        }
    }
    ui.painter().rect_stroke(rect, radius, Stroke::new(look::EDGE, COVER_EDGE), egui::StrokeKind::Inside);
}

/// [`paint_cover`] outside the library (the Stats sheet's thumbnails), on the library's cover
/// cache and per-frame decoding budget.
pub(crate) fn paint_book_cover(app: &mut PdfCraftApp, ui: &egui::Ui, t: &Tokens, rect: Rect, book: &Book) {
    begin_cover_pass(&mut app.margin_library, ui.ctx());
    paint_cover(app, ui, t, rect, book);
}

/// Starts a new frame's cover budget once per egui pass, whoever draws covers first; books
/// whose cover couldn't be read are scanned once to make it again.
fn begin_cover_pass(state: &mut LibraryState, ctx: &egui::Context) {
    for file in state.covers.begin_frame(ctx) {
        let file = PathBuf::from(file);
        if !state.pending_files.contains(&file) {
            state.pending_files.push(file);
        }
    }
}

/// The book's cover texture once decoded (on the worker; on this thread in tests).
fn cover_texture(app: &mut PdfCraftApp, ctx: &egui::Context, book: &Book) -> Option<TextureHandle> {
    let inline = app.margin_library.run_inline || app.run_inline;
    let state = &mut app.margin_library;
    let data = state.db.as_ref()?.data_dir().to_path_buf();
    state.covers.texture(ctx, &data, book, inline)
}

/// A page to show once the book at `path` opens.
struct AskedPage {
    path: String,
    /// 0-based.
    page: usize,
    /// The page wins over the place the sidecar restores (a text search hit).
    over_saved_place: bool,
}

/// Opens the book at `path` (or shows its tab). The sidecar restores the page as any open does;
/// a book without a sidecar position opens at the page the library kept.
pub(crate) fn open_book(app: &mut PdfCraftApp, path: &str) {
    let page = app.margin_library.books.iter().find(|book| book.path == path).map_or(0, |book| book.page);
    let was_open = app.views.iter().any(|view| app.session.get(view.id).and_then(|doc| doc.path.as_deref()) == Some(path));
    if !was_open && page > 0 {
        let page = usize::try_from(page).unwrap_or(usize::MAX);
        app.margin_library.asked_page = Some(AskedPage { path: path.to_owned(), page, over_saved_place: false });
    }
    app.open_recent(path);
}

/// Opens the book at `path` at `page` (0-based), from a text search hit: the page asked for wins
/// over the place the reader left.
fn open_book_at(app: &mut PdfCraftApp, path: &str, page: u32) {
    let page = usize::try_from(page).unwrap_or(usize::MAX);
    app.margin_library.asked_page = Some(AskedPage { path: path.to_owned(), page, over_saved_place: true });
    app.open_recent(path);
    // Open already: `open_recent` only brought its tab forward.
    if let Some(index) = app.active {
        show_asked_page(app, index);
    }
}

/// Shows the page the library asked for when view `index` is the book it was asked for.
fn show_asked_page(app: &mut PdfCraftApp, index: usize) {
    let Some(view) = app.views.get_mut(index) else { return };
    let path = app.session.get(view.id).and_then(|doc| doc.path.as_deref());
    let Some(asked) = app.margin_library.asked_page.take_if(|asked| path == Some(asked.path.as_str())) else { return };
    if asked.over_saved_place || !super::reading::has_saved_place(view) {
        view.go_to_page(asked.page);
    }
}

/// Forgets the asked page once its book is neither open nor being opened: the open failed or
/// stopped at a password, so a later open of that file by another route keeps its own place.
/// Runs after `PdfCraftApp::poll_pending_opens` has opened this frame's books.
fn forget_failed_asked_page(app: &mut PdfCraftApp) {
    let Some(asked) = &app.margin_library.asked_page else { return };
    let path = Some(asked.path.as_str());
    let open = app.views.iter().any(|view| app.session.get(view.id).and_then(|doc| doc.path.as_deref()) == path);
    let opening = app.pending_opens.iter().any(|pending| pending.path.as_deref() == path);
    if !open && !opening {
        app.margin_library.asked_page = None;
    }
}

/// A `file://` URL for `path` (spaces and other unsafe bytes escaped).
fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
            url.push(char::from(byte));
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

/// Writes `finished` into the book's sidecar, so it travels with the file: through the open
/// document's reading state when it is open, else straight into the sidecar.
fn finish_in_sidecar(app: &mut PdfCraftApp, path: &str, finished: bool) -> Result<(), String> {
    let open = app.views.iter().position(|view| app.session.get(view.id).and_then(|doc| doc.path.as_deref()) == Some(path));
    if let Some(index) = open {
        return super::reading::set_finished(app, index, finished);
    }
    let document = Path::new(path);
    let fingerprint = margin_config::store::fingerprint_file(document).map_err(|err| err.to_string())?;
    let book_id = app.margin_library.books.iter().find(|book| book.path == path).map(|book| book.book_id.clone());
    let key = book_id.as_deref().unwrap_or(&fingerprint);
    let data = app.margin_keys.data_dir();
    let (mut sidecar, preferred) = match margin_config::store::find_any(document, &[key, &fingerprint], data.as_deref()) {
        margin_config::store::Found::Sidecar { sidecar, path } => (*sidecar, Some(path)),
        margin_config::store::Found::Missing => (margin_config::Sidecar::new(), None),
        margin_config::store::Found::Locked { reason, .. } => return Err(reason),
    };
    let progress = sidecar.progress.get_or_insert_with(|| margin_config::Progress {
        locator: margin_config::Locator::Pdf { page: 0, quads: Vec::new(), at: None },
        fraction: 0.0,
        finished,
        updated: margin_config::store::now_rfc3339(),
        extra: Default::default(),
    });
    progress.finished = finished;
    progress.updated = margin_config::store::now_rfc3339();
    margin_config::store::write(&mut sidecar, document, key, data.as_deref(), preferred.as_deref()).map(|_| ())
}

fn apply(app: &mut PdfCraftApp, ctx: &egui::Context, actions: Vec<Action>) {
    for action in actions {
        let result: Result<(), String> = match action {
            Action::Open(path) => {
                open_book(app, &path);
                Ok(())
            }
            Action::Back => {
                run(app, "library.open");
                Ok(())
            }
            Action::Stats => {
                run(app, "app.stats");
                Ok(())
            }
            Action::Rescan => {
                rescan(app, ctx);
                Ok(())
            }
            Action::ShowInFolder(path) => {
                let folder = Path::new(&path).parent().map(Path::to_path_buf).unwrap_or_default();
                app.open_url(&file_url(&folder));
                Ok(())
            }
            Action::Finished(id, finished) => {
                let path = app.margin_library.books.iter().find(|book| book.id == id).map(|book| book.path.clone());
                let db_result = app.margin_library.db.as_ref().map_or(Ok(()), |db| db.set_finished(id, finished));
                match (db_result, path) {
                    (Ok(()), Some(path)) => {
                        if let Err(err) = finish_in_sidecar(app, &path, finished) {
                            log::warn!("library: the sidecar of {path} keeps its old state: {err}");
                        }
                        app.notify(if finished { "Marked as finished" } else { "Marked as not finished" });
                        Ok(())
                    }
                    (result, _) => result,
                }
            }
            Action::Remove(id) => app.margin_library.db.as_ref().map_or(Ok(()), |db| db.remove(id)).map(|()| {
                app.notify("Removed from the library. The file stays on disk.");
            }),
            Action::AddTo(collection, book) => app.margin_library.db.as_ref().map_or(Ok(()), |db| db.add_to_collection(collection, book)),
            Action::RemoveFrom(collection, book) => app.margin_library.db.as_ref().map_or(Ok(()), |db| db.remove_from_collection(collection, book)),
            Action::DeleteCollection(collection) => {
                if app.margin_library.collection == Some(collection) {
                    app.margin_library.collection = None;
                }
                app.margin_library.db.as_ref().map_or(Ok(()), |db| db.delete_collection(collection))
            }
            Action::NewCollectionWith(book) => {
                let name = format!("Collection {}", app.margin_library.collections.len() + 1);
                app.margin_library
                    .db
                    .as_ref()
                    .map_or(Ok(()), |db| db.create_collection(&name, store::now_unix()).and_then(|id| db.add_to_collection(id, book)))
            }
            Action::CreateCollection => {
                let name = std::mem::take(&mut app.margin_library.new_collection);
                app.margin_library.db.as_ref().map_or(Ok(()), |db| db.create_collection(&name, store::now_unix()).map(|_| ()))
            }
            Action::TagWith(book, name) => app.margin_library.db.as_ref().map_or(Ok(()), |db| db.add_tag(book, &name, store::now_unix()).map(|_| ())),
            Action::Untag(book, tag) => app.margin_library.db.as_ref().map_or(Ok(()), |db| db.remove_tag(book, tag)),
            Action::DeleteTag(tag) => {
                if app.margin_library.tag == Some(tag) {
                    app.margin_library.tag = None;
                }
                app.margin_library.db.as_ref().map_or(Ok(()), |db| db.delete_tag(tag))
            }
            Action::Details(book) => {
                details::open(app, book);
                Ok(())
            }
            Action::OpenAt(path, page) => {
                open_book_at(app, &path, page);
                Ok(())
            }
        };
        app.margin_library.stale = true;
        if let Err(err) = result {
            log::warn!("library: {err}");
            app.notify(format!("That didn't work: {err}"));
        }
    }
}
