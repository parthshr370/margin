//! Margin's library (R6): the Home view of the Margin flavor. Books come from
//! `<data>/library.sqlite` (`margin_config::library`): every PDF Margin opened, and every PDF in
//! the watched folders (`library.folders`), scanned on start, when the setting changes and on
//! Rescan, on a worker thread. A "Continue reading" card opens the last book being read at its
//! page (the sidecar restores it, as any open does); below it the shelves (Recent, Reading,
//! Finished, Folders, Collections) as a cover grid or a list, with search and sort. Both views are
//! virtualised: only the rows on screen are laid out, and only their covers are loaded.
//!
//! Covers are the first page through PdfCraft's renderer, cached as JPEG in
//! `<data>/cache/thumbs/`; a few are decoded per frame so scrolling never stalls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, TextureHandle, Vec2, pos2, vec2};
use margin_config::library::{self as store, Book, Collection, Cover, LibraryDb, Opened, ScanJob, ScanRequest};
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
/// The Continue reading card's corner radius and padding (§2: radius 12, 16 inside).
const HERO_RADIUS: u8 = 12;
const HERO_PAD: i8 = 16;
/// A cover's own radius and its faint light edge, so a dark first page keeps its outline.
const COVER_RADIUS: u8 = 4;
const COVER_EDGE: Color32 = Color32::from_rgba_premultiplied(26, 26, 26, 26);
const SEARCH_WIDTH: f32 = 300.0;
/// The collection name field's text width: the spec's 192 point well less its side padding.
const COLLECTION_FIELD_WIDTH: f32 = 176.0;
const BUTTON: f32 = 32.0;
const BAR_WIDTH: f32 = 70.0;
/// Covers decoded per frame, and most kept as textures (far ones are dropped first).
const COVERS_PER_FRAME: usize = 6;
const MAX_TEXTURES: usize = 200;
/// The search field's id (`library.search` focuses it).
const SEARCH_ID: &str = "margin-library-search";

/// The shelves, in the order of their tab strip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shelf {
    #[default]
    Recent,
    Reading,
    Finished,
    Folders,
    Collections,
}

impl Shelf {
    pub const ALL: [Self; 5] = [Self::Recent, Self::Reading, Self::Finished, Self::Folders, Self::Collections];

    pub fn label(self) -> &'static str {
        match self {
            Self::Recent => "Recent",
            Self::Reading => "Reading",
            Self::Finished => "Finished",
            Self::Folders => "Folders",
            Self::Collections => "Collections",
        }
    }
}

/// A decoded cover.
struct CoverTex {
    texture: TextureHandle,
    /// The frame it was last drawn in.
    used: u64,
}

/// The library's state. Opens nothing in the PdfCraft flavor.
#[derive(Default)]
pub struct LibraryState {
    db: Option<LibraryDb>,
    /// Opening the database failed (said once; the library is empty this run).
    failed: Option<String>,
    books: Vec<Book>,
    collections: Vec<Collection>,
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
    /// `library.search` asked for the keyboard.
    focus_search: bool,
    /// The name typed for a new collection.
    new_collection: String,
    covers: HashMap<String, CoverTex>,
    frame: u64,
    /// Covers decoded this frame (at most [`COVERS_PER_FRAME`]).
    decoded: usize,
    /// The egui pass `frame` and `decoded` count (the library and the Stats sheet share them).
    cover_pass: u64,
    /// The tab to go back to from the library (`library.open` toggles).
    back_to: Option<usize>,
    /// The library showed last frame.
    shown: bool,
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

    /// Whether a scan is running.
    pub fn scanning(&self) -> bool {
        self.scan.is_some()
    }

    /// Covers decoded and ready to draw.
    pub fn covers_loaded(&self) -> usize {
        self.covers.len()
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

/// Reads the books and collections again if the database changed.
fn reload(app: &mut PdfCraftApp) {
    let state = &mut app.margin_library;
    if !state.stale {
        return;
    }
    state.stale = false;
    let Some(db) = &state.db else { return };
    match db.books() {
        Ok(books) => state.books = books,
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

/// A document opened into view `index` (from `reading::opened`): it joins the library, its
/// cover is scanned if missing, and its stats key is set.
pub(crate) fn opened(app: &mut PdfCraftApp, index: usize) {
    if app.flavor != Flavor::Margin {
        return;
    }
    let Some(view) = app.views.get(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let doc_id = view.id;
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
        let opened = Opened { path, fingerprint: &actual, title: &title, author: &author, pages, size };
        match db.opened(&opened, store::now_unix()) {
            Ok(listed) => {
                book = listed.fingerprint;
                if listed.needs_cover && !app.margin_library.pending_files.iter().any(|p| p == Path::new(path)) {
                    app.margin_library.pending_files.push(PathBuf::from(path));
                }
                app.margin_library.stale = true;
            }
            Err(err) => log::warn!("library: {err}"),
        }
    }
    super::stats::opened(app, doc_id, book, &title, pages);
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
/// results, scan pending covers.
pub(crate) fn tick(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if app.flavor != Flavor::Margin || !ensure_db(app) {
        return;
    }
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
    app.margin_library.covers.clear();
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
        app.notify("Add a folder to library.folders in settings.jsonc to scan it");
        return;
    }
    start_scan(app, ctx, folders, Vec::new());
}

/// Margin's library ids: `library.open` (show the library, or go back to the book),
/// `library.search` (focus the search field) and `app.stats` (the Stats sheet). `false` for
/// another id.
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
    let order = ordered(&app.margin_library);
    let hero = hero_book(&app.margin_library);
    let hero_line = hero.map(|index| hero_meta(app, index));
    let mut actions = Vec::new();
    let full = ui.available_rect_before_wrap();
    window_chrome::drag_area(app, ui, Rect::from_min_size(full.min, vec2(full.width(), super::header::HEIGHT)), ui.id().with("margin-library-drag"));
    // With the traffic lights over the window, the bar shares their row (the reader's header row).
    let top_pad = if window_chrome::traffic_lights_shown(&ctx) { (super::header::HEIGHT - BUTTON) / 2.0 } else { TOP_PAD };
    let width = (full.width() - 2.0 * SIDE_PAD).clamp(0.0, MAX_WIDTH);
    let left = full.left() + (full.width() - width) / 2.0;
    let content = Rect::from_min_max(pos2(left, full.top() + top_pad), pos2(left + width, full.bottom()));
    let traffic_lights = window_chrome::traffic_lights_inset(&ctx, content);
    let mut page = ui.new_child(egui::UiBuilder::new().max_rect(content).layout(egui::Layout::top_down(egui::Align::Min)));
    top_bar(app, &mut page, &t, traffic_lights, &mut actions);
    page.add_space(GAP_LARGE * 2.0);
    if app.margin_library.failed.is_some() {
        page.label(egui::RichText::new("The library database couldn't be opened; see the log.").color(t.text_muted));
    }
    let searching = !app.margin_library.query.trim().is_empty();
    if let (Some(index), Some(meta), false) = (hero, &hero_line, searching) {
        hero_card(app, &mut page, &t, index, meta, &mut actions);
        page.add_space(GAP_LARGE * 2.0);
    }
    shelf_header(app, &mut page, &t, order.len());
    page.add_space(GAP_LARGE);
    if matches!(app.margin_library.shelf, Shelf::Folders | Shelf::Collections) {
        shelf_picker(app, &mut page, &t, &mut actions);
        page.add_space(GAP_LARGE);
    }
    if order.is_empty() {
        empty_state(app, &mut page, &t);
    } else {
        match app.margin_library.view {
            LibraryView::Grid => grid(app, &mut page, &t, &order, &mut actions),
            LibraryView::List => list(app, &mut page, &t, &order, &mut actions),
        }
    }
    ui.allocate_rect(full, Sense::hover());
    trim_textures(&mut app.margin_library);
    apply(app, &ctx, actions);
}

/// The library stopped showing (a book opened).
pub(crate) fn hidden(app: &mut PdfCraftApp) {
    app.margin_library.shown = false;
}

/// The indices of the books the shelf, folder or collection and search show, sorted.
fn ordered(state: &LibraryState) -> Vec<usize> {
    let query = state.query.trim().to_lowercase();
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
        })
        .filter(|(_, book)| {
            query.is_empty() || book.title.to_lowercase().contains(&query) || book.author.to_lowercase().contains(&query) || {
                let name = Path::new(&book.path).file_name().map(|name| name.to_string_lossy().to_lowercase());
                name.is_some_and(|name| name.contains(&query))
            }
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

/// "p. 6 of 14 · 43% · ~34 min left".
fn hero_meta(app: &mut PdfCraftApp, index: usize) -> String {
    let Some(book) = app.margin_library.books.get(index) else { return String::new() };
    let (page, pages, progress, fingerprint) = (book.page, book.pages, book.progress, book.fingerprint.clone());
    let mut parts = Vec::new();
    if pages > 0 {
        parts.push(format!("p. {} of {pages}", page.saturating_add(1).min(pages)));
    }
    parts.push(format!("{}%", (progress * 100.0).round() as u32));
    let left = pages.saturating_sub(page.saturating_add(1));
    if super::stats::ensure_open(app)
        && let Some(summary) = super::stats::summary(app, &fingerprint)
        && let Some(seconds) = summary.seconds_left(left)
    {
        parts.push(format!("~{} left", margin_config::stats::format_duration(seconds)));
    }
    parts.join("   ")
}

fn top_bar(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, traffic_lights: f32, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = GAP_LARGE;
        // The gap after the space is the row's own item spacing.
        if traffic_lights > 0.0 {
            ui.add_space((traffic_lights - GAP_LARGE).max(0.0));
        }
        let back = app.margin_library.back_to.filter(|index| *index < app.views.len());
        if back.is_some() {
            let keys = look::shortcut(app, "library.open");
            if look::icon_button(ui, "chevron-left", BUTTON, false, "Back to reading", keys.as_deref()).clicked() {
                actions.push(Action::Back);
            }
        }
        ui.label(egui::RichText::new("Library").font(theme::semibold(18.0)).color(t.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = GAP / 2.0;
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
            if let Some(progress) = &scanning {
                ui.label(
                    egui::RichText::new(format!("Scanning {} / {}", progress.done, progress.to_read)).font(theme::regular(12.0)).color(t.text_muted),
                );
            }
            search_field(app, ui, t);
        });
    });
}

fn search_field(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let id = egui::Id::new(SEARCH_ID);
    let focused = look::input_focused(ui, id);
    look::input_frame(t, focused).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
        // Left to right even inside the top bar's right-to-left cluster: icon, then text.
        ui.allocate_ui_with_layout(vec2(SEARCH_WIDTH - 20.0, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            let (icon, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
            icons::paint(ui, icon, "search", 15.0, t.text_muted);
            let keys = look::shortcut(app, "library.search").map(|keys| format!("  ({keys})")).unwrap_or_default();
            let field = egui::TextEdit::singleline(&mut app.margin_library.query)
                .id(id)
                .frame(egui::Frame::NONE)
                .hint_text(format!("Search titles{keys}"))
                .desired_width(SEARCH_WIDTH - 50.0);
            let resp = ui.add(field);
            if std::mem::take(&mut app.margin_library.focus_search) {
                resp.request_focus();
            }
        });
    });
}

fn hero_card(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize, meta: &str, actions: &mut Vec<Action>) {
    let Some(book) = app.margin_library.books.get(index).cloned() else { return };
    let frame = look::card_frame(t).corner_radius(HERO_RADIUS).shadow(look::SHADOW_S).inner_margin(HERO_PAD);
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
                if !book.author.is_empty() {
                    ui.add(egui::Label::new(egui::RichText::new(&book.author).font(theme::regular(13.0)).color(t.text_muted)).truncate());
                }
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

/// The shelf's name and count, then Sort and the shelves as a segmented group on the right.
fn shelf_header(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, count: usize) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        look::section_label(ui, t, app.margin_library.shelf.label(), Some(count));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let current = app.margin_library.shelf;
            let segments = Shelf::ALL.map(|shelf| look::Segment { label: shelf.label(), selected: shelf == current, tip: None });
            if let Some(shelf) = look::segmented(ui, &segments).and_then(|i| Shelf::ALL.get(i)) {
                app.margin_library.shelf = *shelf;
            }
            ui.add_space(GAP_LARGE);
            sort_menu(app, ui);
        });
    });
}

/// Sort: a compact secondary button with a chevron, opening the sort orders as a menu.
fn sort_menu(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let label = |sort: LibrarySort| match sort {
        LibrarySort::Recent => "Recent",
        LibrarySort::Title => "Title",
        LibrarySort::Author => "Author",
        LibrarySort::Progress => "Progress",
        LibrarySort::Added => "Added",
    };
    let text = format!("Sort: {}", label(app.margin_library.sort));
    let button = look::Button::secondary(ButtonSize::Compact, &text).trailing("chevron-down").show(ui);
    egui::Popup::menu(&button).show(|ui| {
        for sort in [LibrarySort::Recent, LibrarySort::Title, LibrarySort::Author, LibrarySort::Progress, LibrarySort::Added] {
            if ui.selectable_label(app.margin_library.sort == sort, label(sort)).clicked() {
                app.margin_library.sort = sort;
                ui.close();
            }
        }
    });
}

/// A filter chip: a standard secondary button, the chosen one selected (check, selected label).
fn chip(ui: &mut egui::Ui, label: &str, on: bool) -> egui::Response {
    look::Button::secondary(ButtonSize::Standard, label).selected(on).show(ui)
}

/// The Folders shelf's folder chips, or the Collections shelf's collection chips and, under
/// them, the row that creates one.
fn shelf_picker(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(GAP, GAP);
        match app.margin_library.shelf {
            Shelf::Folders => {
                let folders: Vec<PathBuf> = folder_paths(&app.margin_keys.settings().library.folders);
                if folders.is_empty() {
                    ui.label(egui::RichText::new("No watched folders. Add paths to \"library.folders\" in settings.jsonc.").color(t.text_muted));
                }
                if chip(ui, "All", app.margin_library.folder.is_none()).clicked() {
                    app.margin_library.folder = None;
                }
                for folder in folders {
                    let folder = folder.to_string_lossy().into_owned();
                    let on = app.margin_library.folder.as_deref() == Some(folder.as_str());
                    let name = crate::folders_ui::folder_name(&folder);
                    if look::Button::secondary(ButtonSize::Standard, &name).selected(on).tip(&folder).show(ui).clicked() {
                        app.margin_library.folder = Some(folder);
                    }
                }
            }
            Shelf::Collections => {
                if chip(ui, "All", app.margin_library.collection.is_none()).clicked() {
                    app.margin_library.collection = None;
                }
                let collections = app.margin_library.collections.clone();
                for collection in collections {
                    let on = app.margin_library.collection == Some(collection.id);
                    let resp = chip(ui, &format!("{}  {}", collection.name, collection.count), on);
                    if resp.clicked() {
                        app.margin_library.collection = Some(collection.id);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Delete collection").clicked() {
                            actions.push(Action::DeleteCollection(collection.id));
                        }
                    });
                }
            }
            _ => {}
        }
    });
    if app.margin_library.shelf == Shelf::Collections {
        ui.add_space(GAP);
        new_collection_row(app, ui, t, actions);
    }
}

/// The outlined "Collection name" field and its primary Add, off while the name is blank.
fn new_collection_row(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        let id = egui::Id::new("margin-new-collection");
        let field = look::input_frame(t, look::input_focused(ui, id)).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            let edit = egui::TextEdit::singleline(&mut app.margin_library.new_collection)
                .id(id)
                .frame(egui::Frame::NONE)
                .hint_text("New collection")
                .desired_width(COLLECTION_FIELD_WIDTH);
            ui.add(edit)
        });
        let resp = field.inner;
        ui.ctx().accesskit_node_builder(resp.id, |node| node.set_label("Collection name"));
        let named = !app.margin_library.new_collection.trim().is_empty();
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let add = ui.add_enabled_ui(named, |ui| look::button_primary(ui, ButtonSize::Standard, "Add")).inner.clicked();
        if named && (enter || add) {
            actions.push(Action::CreateCollection);
        }
    });
}

fn empty_state(app: &PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let text = if !app.margin_library.query.trim().is_empty() {
        "No book matches the search."
    } else if app.margin_library.books.is_empty() {
        "Books you open appear here. Add folders to \"library.folders\" in settings.jsonc to scan them."
    } else {
        "Nothing on this shelf yet."
    };
    ui.add_space(GAP_LARGE * 2.0);
    ui.label(egui::RichText::new(text).font(theme::regular(13.0)).color(t.text_muted));
}

/// The cover grid, laid out a row at a time for the rows on screen only.
fn grid(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, order: &[usize], actions: &mut Vec<Action>) {
    let width = ui.available_width();
    let columns = (((width + CELL_GAP_X) / (CELL_MIN + CELL_GAP_X)).floor() as usize).max(1);
    let cell_w = ((width - CELL_GAP_X * (columns as f32 - 1.0)) / columns as f32).max(1.0);
    let cover_h = (cell_w - 2.0 * CELL_PAD).max(1.0) * COVER_ASPECT;
    let row_h = cover_h + CELL_TEXT + 2.0 * CELL_PAD;
    let rows = order.len().div_ceil(columns);
    ui.spacing_mut().item_spacing.y = CELL_GAP_Y;
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("margin-library-grid").show_rows(ui, row_h, rows, |ui, range| {
        for row in range {
            let (rect, _) = ui.allocate_exact_size(vec2(width, row_h), Sense::hover());
            for column in 0..columns {
                let Some(&index) = order.get(row * columns + column) else { break };
                let cell = Rect::from_min_size(pos2(rect.left() + column as f32 * (cell_w + CELL_GAP_X), rect.top()), vec2(cell_w, row_h));
                book_cell(app, ui, t, index, cell, cover_h, actions);
            }
        }
    });
}

/// One book in the grid: a card (cover, title, status) with the S shadow, its fill and edge
/// lifting to the hover state under the pointer.
fn book_cell(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize, cell: Rect, cover_h: f32, actions: &mut Vec<Action>) {
    let Some(book) = app.margin_library.books.get(index).cloned() else { return };
    let resp = ui.interact(cell, ui.id().with(("book", book.id)), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &book.title));
    let radius = CornerRadius::same(look::RADIUS_CARD);
    ui.painter().add(look::SHADOW_S.as_shape(cell, radius));
    let lift = super::motion::hover(ui.ctx(), resp.id, resp.hovered());
    let edge = t.border.lerp_to_gamma(look::Surfaces::of(t).edge_hover, lift);
    ui.painter().rect(cell, radius, t.card.lerp_to_gamma(t.hover, lift), look::edge_stroke(edge), egui::StrokeKind::Inside);
    look::focus_ring(ui, t, cell, &resp, look::RADIUS_CARD);
    let inner = cell.shrink(CELL_PAD);
    let cover = Rect::from_min_size(inner.min, vec2(inner.width(), cover_h));
    paint_cover(app, ui, t, cover, &book);
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
        painter.rect_filled(bar, CornerRadius::same(2), look::Surfaces::of(t).track);
        let fill = Rect::from_min_size(bar.min, vec2(bar.width() * book.progress.clamp(0.0, 1.0) as f32, bar.height()));
        painter.rect_filled(fill, CornerRadius::same(2), t.accent);
        let percent = format!("{}%", (book.progress * 100.0).round() as u32);
        painter.text(pos2(bar.right() + GAP, y), Align2::LEFT_CENTER, percent, font, t.text_muted);
    } else {
        painter.text(pos2(rect.left(), y), Align2::LEFT_CENTER, "new", font, t.text_muted);
    }
}

/// The list view: one row per book in a panel-coloured, edged container, a divider between
/// rows; virtualised like the grid.
fn list(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, order: &[usize], actions: &mut Vec<Action>) {
    let container = egui::Frame::NONE.fill(t.panel).stroke(look::edge_stroke(t.border)).corner_radius(look::RADIUS_CARD).inner_margin(look::EDGE);
    container.show(ui, |ui| list_rows(app, ui, t, order, actions));
}

fn list_rows(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, order: &[usize], actions: &mut Vec<Action>) {
    let width = ui.available_width();
    ui.spacing_mut().item_spacing.y = look::EDGE;
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("margin-library-list").show_rows(ui, LIST_ROW, order.len(), |ui, range| {
        for &index in order.get(range).unwrap_or_default() {
            let Some(book) = app.margin_library.books.get(index).cloned() else { continue };
            let (rect, resp) = ui.allocate_exact_size(vec2(width, LIST_ROW), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &book.title));
            look::row_fill(ui, t, rect, &resp, false);
            ui.painter().hline(rect.x_range(), rect.bottom() + look::EDGE / 2.0, Stroke::new(look::EDGE, t.divider));
            let cover = Rect::from_min_size(pos2(rect.left() + GAP, rect.center().y - LIST_COVER.y / 2.0), LIST_COVER);
            paint_cover(app, ui, t, cover, &book);
            let text_left = cover.right() + GAP_LARGE;
            let status = Rect::from_min_max(pos2(rect.right() - 200.0, rect.top()), pos2(rect.right() - GAP, rect.bottom()));
            let text_width = (status.left() - GAP_LARGE - text_left).max(GAP);
            crate::widgets::row_text(
                ui,
                pos2(text_left, rect.top() + 17.0),
                crate::bidi::visual(&book.title),
                theme::medium(13.0),
                t.text,
                text_width,
            );
            let detail = if book.author.is_empty() { book.path.clone() } else { book.author.clone() };
            crate::widgets::row_text(
                ui,
                pos2(text_left, rect.top() + 35.0),
                crate::bidi::visual(&detail),
                theme::regular(11.5),
                t.text_muted,
                text_width,
            );
            status_line(ui, t, &book, status);
            if resp.clicked() {
                actions.push(Action::Open(book.path.clone()));
            }
            context_menu(app, &resp, &book, actions);
        }
    });
}

/// The book's menu: open, show in folder, finished, collections, remove.
fn context_menu(app: &PdfCraftApp, resp: &egui::Response, book: &Book, actions: &mut Vec<Action>) {
    resp.context_menu(|ui| {
        if ui.button("Open").clicked() {
            actions.push(Action::Open(book.path.clone()));
        }
        if ui.button("Show in folder").clicked() {
            actions.push(Action::ShowInFolder(book.path.clone()));
        }
        let finished = if book.finished { "Mark as not finished" } else { "Mark as finished" };
        if ui.button(finished).clicked() {
            actions.push(Action::Finished(book.id, !book.finished));
        }
        ui.menu_button("Add to collection", |ui| {
            for collection in &app.margin_library.collections {
                let member = book.collections.contains(&collection.id);
                if ui.add_enabled(!member, egui::Button::new(&collection.name)).clicked() {
                    actions.push(Action::AddTo(collection.id, book.id));
                }
            }
            if !app.margin_library.collections.is_empty() {
                ui.separator();
            }
            if ui.button("New collection").clicked() {
                actions.push(Action::NewCollectionWith(book.id));
            }
        });
        if let Some(collection) =
            app.margin_library.collection.filter(|c| app.margin_library.shelf == Shelf::Collections && book.collections.contains(c))
            && ui.button("Remove from this collection").clicked()
        {
            actions.push(Action::RemoveFrom(collection, book.id));
        }
        ui.separator();
        if ui.button("Remove from library").clicked() {
            actions.push(Action::Remove(book.id));
        }
    });
}

/// Draws the book's cover in `rect` (the top of the page, filling the width), or a placeholder
/// while it isn't decoded: radius 4, the S shadow and a faint light edge (§2).
fn paint_cover(app: &mut PdfCraftApp, ui: &egui::Ui, t: &Tokens, rect: Rect, book: &Book) {
    let radius = CornerRadius::same(COVER_RADIUS);
    ui.painter().add(look::SHADOW_S.as_shape(rect, radius));
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

/// Starts a new frame's cover budget once per egui pass, whoever draws covers first.
fn begin_cover_pass(state: &mut LibraryState, ctx: &egui::Context) {
    let pass = ctx.cumulative_pass_nr();
    if state.cover_pass != pass {
        state.cover_pass = pass;
        state.frame = state.frame.wrapping_add(1);
        state.decoded = 0;
    }
}

/// The book's cover texture, decoding it now if the frame's budget allows.
fn cover_texture(app: &mut PdfCraftApp, ctx: &egui::Context, book: &Book) -> Option<TextureHandle> {
    let state = &mut app.margin_library;
    let frame = state.frame;
    if let Some(cover) = state.covers.get_mut(&book.fingerprint) {
        cover.used = frame;
        return Some(cover.texture.clone());
    }
    if book.cover != Cover::Cached {
        return None;
    }
    if state.decoded >= COVERS_PER_FRAME {
        ctx.request_repaint();
        return None;
    }
    let data = state.db.as_ref()?.data_dir().to_path_buf();
    let (width, height, rgba) = store::load_cover(&store::cover_path(&data, &book.fingerprint))?;
    let size = [usize::try_from(width).ok()?, usize::try_from(height).ok()?];
    let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
    state.decoded += 1;
    let texture = ctx.load_texture(format!("cover-{}", book.fingerprint), image, egui::TextureOptions::LINEAR);
    state.covers.insert(book.fingerprint.clone(), CoverTex { texture: texture.clone(), used: frame });
    Some(texture)
}

/// Opens the book at `path` (or shows its tab). The sidecar restores the page as any open does;
/// a book without a sidecar position opens at the page the library kept.
pub(crate) fn open_book(app: &mut PdfCraftApp, path: &str) {
    let page = app.margin_library.books.iter().find(|book| book.path == path).map_or(0, |book| book.page);
    let was_open = app.views.iter().any(|view| app.session.get(view.id).and_then(|doc| doc.path.as_deref()) == Some(path));
    app.open_recent(path);
    if was_open || page == 0 {
        return;
    }
    let Some(view) = app.active.and_then(|index| app.views.get_mut(index)) else { return };
    if !super::reading::has_saved_place(view) {
        view.go_to_page(usize::try_from(page).unwrap_or(usize::MAX));
    }
}

/// Drops the covers drawn longest ago once there are more than [`MAX_TEXTURES`].
fn trim_textures(state: &mut LibraryState) {
    if state.covers.len() <= MAX_TEXTURES {
        return;
    }
    let mut ages: Vec<(u64, String)> = state.covers.iter().map(|(key, cover)| (cover.used, key.clone())).collect();
    ages.sort_unstable();
    let excess = state.covers.len() - MAX_TEXTURES;
    for (_, key) in ages.into_iter().take(excess) {
        state.covers.remove(&key);
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
    let data = app.margin_keys.data_dir();
    let (mut sidecar, preferred) = match margin_config::store::find(document, &fingerprint, data.as_deref()) {
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
    margin_config::store::write(&mut sidecar, document, &fingerprint, data.as_deref(), preferred.as_deref()).map(|_| ())
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
        };
        app.margin_library.stale = true;
        if let Err(err) = result {
            log::warn!("library: {err}");
            app.notify(format!("That didn't work: {err}"));
        }
    }
}
