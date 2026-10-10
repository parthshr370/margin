//! Margin's reading statistics (R6) in the reader: every frame [`tick`] tells the recorder
//! (`margin_config::stats::Recorder`) which page of which book shows and whether there was
//! input, and it writes sessions and page times into `<data>/stats.sqlite`. `stats.enabled`
//! (default on) turns it off. The document card shows a book's summary; the Stats sheet
//! (`app.stats`, `stats_sheet.rs`) shows the calendar.
//!
//! A book is the library's fingerprint for its path (`library::opened`), so its stats survive
//! annotations saved into the PDF.

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use margin_config::stats::{BookSummary, Recorder, Showing, StatsDb};
use pdfcraft_engine::DocId;

use crate::{Flavor, PdfCraftApp};

/// The reader's statistics state. Opens nothing until the first Margin frame with a document.
#[derive(Default)]
pub struct StatsState {
    db: Option<StatsDb>,
    /// Opening the database failed; told once, not retried this run.
    failed: bool,
    recorder: Recorder,
    /// The stats key of each open document.
    books: HashMap<DocId, String>,
    /// Summaries read since the last row was written.
    summaries: HashMap<String, BookSummary>,
    /// Unix seconds to use instead of the system clock (tests).
    clock: Option<f64>,
    /// The Stats sheet, while open.
    pub(crate) sheet: Option<super::stats_sheet::Sheet>,
}

impl StatsState {
    /// Uses `unix` (seconds) as the time from now on instead of the system clock (tests).
    pub fn set_clock(&mut self, unix: f64) {
        self.clock = Some(unix);
    }

    /// Moves the test clock on by `seconds` (see [`Self::set_clock`]).
    pub fn advance_clock(&mut self, seconds: f64) {
        if let Some(clock) = &mut self.clock {
            *clock += seconds;
        }
    }

    /// Now, in Unix seconds.
    pub fn now(&self) -> f64 {
        self.clock.unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64()))
    }

    /// The stats key of open document `doc`.
    pub fn book(&self, doc: DocId) -> Option<&str> {
        self.books.get(&doc).map(String::as_str)
    }

    /// The database, once opened.
    pub fn db(&self) -> Option<&StatsDb> {
        self.db.as_ref()
    }

    /// Whether the Stats sheet is open.
    pub fn sheet_open(&self) -> bool {
        self.sheet.is_some()
    }

    /// Opens or closes the Stats sheet (`app.stats`).
    pub(crate) fn toggle_sheet(&mut self) {
        self.sheet = match self.sheet {
            Some(_) => None,
            None => Some(super::stats_sheet::Sheet::default()),
        };
    }
}

/// Opens `<data>/stats.sqlite` if it isn't yet. False when there is none (no data folder, or it
/// can't be opened: said once).
pub(crate) fn ensure_open(app: &mut PdfCraftApp) -> bool {
    if app.margin_stats.db.is_some() {
        return true;
    }
    if app.margin_stats.failed {
        return false;
    }
    let opened = match app.margin_keys.data_dir() {
        Some(data) => StatsDb::open(&data),
        None => Err("no data folder on this system".to_owned()),
    };
    match opened {
        Ok(db) => {
            app.margin_stats.db = Some(db);
            true
        }
        Err(err) => {
            app.margin_stats.failed = true;
            log::warn!("stats: {err}");
            app.notify(format!("Reading statistics are off: {err}"));
            false
        }
    }
}

/// Document `doc` opened as book `book` (`title`, `pages` for the sheet and the CSV).
pub(crate) fn opened(app: &mut PdfCraftApp, doc: DocId, book: String, title: &str, pages: u32) {
    if !app.margin_keys.settings().stats.enabled || !ensure_open(app) {
        app.margin_stats.books.insert(doc, book);
        return;
    }
    if let Some(db) = &app.margin_stats.db
        && let Err(err) = db.note_book(&book, title, pages)
    {
        log::warn!("stats: {err}");
    }
    app.margin_stats.books.insert(doc, book);
}

/// Every frame (from `reading::tick`): record what the reader shows.
pub(crate) fn tick(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if app.flavor != Flavor::Margin {
        return;
    }
    let now = app.margin_stats.now();
    if !app.margin_keys.settings().stats.enabled {
        stop(app, now);
        return;
    }
    if app.active.is_none() && app.margin_stats.recorder.reading().is_none() {
        return;
    }
    if !ensure_open(app) {
        return;
    }
    let (focused, closing, input) = ctx.input(|i| {
        let input = i.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key { .. }
                    | egui::Event::Text(_)
                    | egui::Event::PointerButton { .. }
                    | egui::Event::PointerMoved(_)
                    | egui::Event::MouseWheel { .. }
                    | egui::Event::Zoom(_)
            )
        });
        (i.viewport().focused != Some(false), i.viewport().close_requested(), input)
    });
    let shown = if focused && !closing { app.active_ids().and_then(|(index, doc)| Some((app.views.get(index)?.current, doc))) } else { None };
    let StatsState { db, recorder, books, summaries, .. } = &mut app.margin_stats;
    let Some(db) = db.as_ref() else { return };
    let showing = shown.and_then(|(page, doc)| Some(Showing { book: books.get(&doc)?, page: u32::try_from(page).unwrap_or(u32::MAX) }));
    let result = recorder.step(db, now, showing, input).and_then(|wrote| if closing { Ok(recorder.stop(db, now)? || wrote) } else { Ok(wrote) });
    match result {
        Ok(true) => summaries.clear(),
        Ok(false) => {}
        Err(err) => log::warn!("stats: {err}"),
    }
}

/// Ends the page visit and session in progress (stats turned off, embedders shutting down).
pub fn stop(app: &mut PdfCraftApp, now: f64) {
    let StatsState { db, recorder, summaries, .. } = &mut app.margin_stats;
    let Some(db) = db.as_ref() else { return };
    match recorder.stop(db, now) {
        Ok(true) => summaries.clear(),
        Ok(false) => {}
        Err(err) => log::warn!("stats: {err}"),
    }
}

/// `book`'s summary (cached until the next row is written).
pub fn summary(app: &mut PdfCraftApp, book: &str) -> Option<BookSummary> {
    let state = &mut app.margin_stats;
    if let Some(summary) = state.summaries.get(book) {
        return Some(*summary);
    }
    let summary = state.db.as_ref()?.summary(book).map_err(|err| log::warn!("stats: {err}")).ok()?;
    state.summaries.insert(book.to_owned(), summary);
    Some(summary)
}

/// The document card's stats line for the document shown, if it has reading recorded.
pub(crate) fn active_summary_line(app: &mut PdfCraftApp) -> Option<String> {
    let (index, doc) = app.active_ids()?;
    let book = app.margin_stats.book(doc)?.to_owned();
    let summary = summary(app, &book).filter(|summary| summary.seconds > 0)?;
    let view = app.views.get(index)?;
    let pages = app.session.get(doc)?.info.pages.len();
    let left = u32::try_from(pages.saturating_sub(view.current.saturating_add(1))).ok();
    Some(summary_line(&summary, left))
}

/// "2h 14m read · 37 pages · 1.8 min/page · ~34 min left": a book's summary in a line.
/// `pages_left` is `None` when unknown.
pub fn summary_line(summary: &BookSummary, pages_left: Option<u32>) -> String {
    use margin_config::stats::format_duration;
    let mut parts = vec![format!("{} read", format_duration(summary.seconds as f64))];
    parts.push(if summary.pages == 1 { "1 page".to_owned() } else { format!("{} pages", summary.pages) });
    if let Some(pace) = summary.seconds_per_page() {
        parts.push(format!("{:.1} min/page", pace / 60.0));
    }
    if let Some(left) = pages_left.and_then(|pages| summary.seconds_left(pages)) {
        parts.push(format!("~{} left", format_duration(left)));
    }
    parts.join(" · ")
}

/// Writes every page visit as CSV to `path`.
pub fn export_csv(app: &mut PdfCraftApp, path: &Path) -> Result<(), String> {
    if !ensure_open(app) {
        return Err("there are no reading statistics".to_owned());
    }
    let db = app.margin_stats.db.as_ref().ok_or_else(|| "there are no reading statistics".to_owned())?;
    let csv = db.export_csv()?;
    margin_config::store::write_atomically(path, csv.as_bytes()).map_err(|err| format!("can't write {}: {err}", path.display()))
}
