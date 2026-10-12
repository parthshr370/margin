//! The library's full-text side: the background pass that reads the books' page text and PDF
//! highlights into the index (`margin_config::library_text::TextJob`, after each scan), and the
//! "In text" search, which lists the books of the shelf whose pages match, each with its
//! snippets; a click on a snippet opens the book at that page.
//!
//! Both run off the UI thread. A search starts once typing pauses, and a newer one interrupts
//! the one still running, so a large library never stalls a frame.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{RichText, Sense, Stroke, vec2};
use margin_config::library_text::{HitKind, MATCH_END, MATCH_START, TextHit, TextJob, TextSearch};

use super::super::look::{self, GAP, GAP_LARGE};
use super::{Action, LibraryState};
use crate::PdfCraftApp;
use crate::theme::{self, Tokens};

/// How long typing pauses before the text is searched.
const DEBOUNCE_SECS: f64 = 0.3;
/// Snippets shown under a book.
const SNIPPETS_PER_BOOK: usize = 3;
const TITLE_SIZE: f32 = 13.5;
const SNIPPET_SIZE: f32 = 12.5;
const NOTE_SIZE: f32 = 12.5;
/// The snippet rows' left inset under their book's title.
const SNIPPET_INDENT: f32 = 16.0;

/// The text pass and the text search.
#[derive(Default)]
pub(crate) struct TextState {
    job: Option<TextJob>,
    /// The pass should run (again) once no scan runs.
    due: bool,
    search: Option<TextSearch>,
    /// The query as last typed, and when.
    typed: String,
    typed_at: f64,
    /// The query the hits are for (or the running search).
    searched: String,
    hits: Vec<TextHit>,
    error: Option<String>,
    /// The hits by book, for the shelf they were grouped for.
    groups: Option<Groups>,
}

/// Hits grouped by book, in the order of each book's best hit.
struct Groups {
    order: Arc<[usize]>,
    generation: u64,
    /// (index into `books`, indices into `hits`).
    books: Vec<(usize, Vec<usize>)>,
}

impl TextState {
    /// The library opened, or a scan or an import ended: read what changed.
    pub(crate) fn read_again(&mut self) {
        self.due = true;
    }

    /// A scan starts: the pass stops (it runs again after), so the two don't compete.
    pub(crate) fn scan_started(&mut self) {
        if let Some(job) = &self.job {
            job.cancel();
        }
        self.due = true;
    }
}

/// Every frame, no scan running: takes a finished pass's result and starts a due one.
pub(crate) fn tick(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let inline = app.margin_library.run_inline || app.run_inline;
    let state = &mut app.margin_library;
    if let Some(result) = state.text.job.as_ref().and_then(TextJob::take_result) {
        state.text.job = None;
        match result {
            Ok(read) if read > 0 => {
                log::info!("library: read the text of {read} books");
                // The index grew: search again.
                state.text.searched.clear();
            }
            Ok(_) => {}
            Err(err) => log::warn!("library: {err}"),
        }
    }
    if state.scan.is_some() || state.text.job.is_some() || !state.text.due {
        return;
    }
    let Some(data) = state.db.as_ref().map(|db| db.data_dir().to_path_buf()) else { return };
    state.text.due = false;
    state.text.job = if inline {
        Some(TextJob::run_inline(&data))
    } else {
        let wake = ctx.clone();
        TextJob::spawn(data, move || wake.request_repaint()).map_err(|err| log::warn!("library: can't start reading the books' text: {err}")).ok()
    };
}

/// Whether the library shows text results: "In text" is on and something is typed.
pub(crate) fn active(state: &LibraryState) -> bool {
    state.in_text && !state.query.trim().is_empty()
}

/// Starts a search once typing pauses, and takes a finished one's hits.
fn search(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let inline = app.margin_library.run_inline || app.run_inline;
    let now = ctx.input(|input| input.time);
    let state = &mut app.margin_library;
    let query = state.query.trim().to_owned();
    if query != state.text.typed {
        state.text.typed.clone_from(&query);
        state.text.typed_at = now;
    }
    if let Some(result) = state.text.search.as_ref().and_then(TextSearch::take) {
        state.text.search = None;
        state.text.groups = None;
        match result {
            Ok(hits) => (state.text.hits, state.text.error) = (hits, None),
            Err(err) => (state.text.hits, state.text.error) = (Vec::new(), Some(err)),
        }
    }
    if state.text.typed == state.text.searched {
        return;
    }
    let waited = now - state.text.typed_at;
    if !inline && waited < DEBOUNCE_SECS {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(DEBOUNCE_SECS - waited));
        return;
    }
    let Some(data) = state.db.as_ref().map(|db| db.data_dir().to_path_buf()) else { return };
    state.text.searched.clone_from(&state.text.typed);
    // Dropping a running search interrupts it.
    state.text.search = None;
    if inline {
        state.text.search = Some(TextSearch::run_inline(&data, &query));
        return;
    }
    let wake = ctx.clone();
    match TextSearch::spawn(data, query, move || wake.request_repaint()) {
        Ok(search) => state.text.search = Some(search),
        Err(err) => state.text.error = Some(format!("can't start the search: {err}")),
    }
}

/// The hits grouped by book, keeping only the books of the shelf (`order`).
fn groups(state: &mut LibraryState, order: &Arc<[usize]>) -> Vec<(usize, Vec<usize>)> {
    if let Some(kept) = state.text.groups.as_ref().filter(|kept| Arc::ptr_eq(&kept.order, order) && kept.generation == state.generation) {
        return kept.books.clone();
    }
    let on_shelf: HashSet<usize> = order.iter().copied().collect();
    let mut by_book: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut slot: HashMap<usize, usize> = HashMap::new();
    for (hit_index, hit) in state.text.hits.iter().enumerate() {
        let Some(&book) = state.index_of.get(&hit.book) else { continue };
        if !on_shelf.contains(&book) {
            continue;
        }
        let at = *slot.entry(book).or_insert_with(|| {
            by_book.push((book, Vec::new()));
            by_book.len().saturating_sub(1)
        });
        if let Some((_, hits)) = by_book.get_mut(at) {
            hits.push(hit_index);
        }
    }
    state.text.groups = Some(Groups { order: order.clone(), generation: state.generation, books: by_book.clone() });
    by_book
}

/// A snippet as text with its matched words marked: muted, the matches in the text colour.
fn snippet_job(snippet: &str, t: &Tokens, page: u32, kind: HitKind) -> LayoutJob {
    let mut job = LayoutJob::default();
    let plain = TextFormat { font_id: theme::regular(SNIPPET_SIZE), color: t.text_muted, ..TextFormat::default() };
    let marked = TextFormat { font_id: theme::semibold(SNIPPET_SIZE), color: t.text, ..TextFormat::default() };
    let place = match kind {
        HitKind::Page => format!("p. {}   ", page.saturating_add(1)),
        HitKind::Highlight => format!("p. {} highlight   ", page.saturating_add(1)),
    };
    job.append(&place, 0.0, TextFormat { color: t.accent, ..plain.clone() });
    for (index, part) in snippet.split([MATCH_START, MATCH_END]).enumerate() {
        // Parts alternate: outside a match, inside one.
        let format = if index % 2 == 1 { marked.clone() } else { plain.clone() };
        job.append(part, 0.0, format);
    }
    job
}

/// The text results in place of the shelf, in the library page's own scroll area. The status line
/// above them is always there (one line, empty once results show), so searching never moves
/// the results up and down.
pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, order: &Arc<[usize]>, actions: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    search(app, &ctx);
    let state = &mut app.margin_library;
    let by_book = groups(state, order);
    let note = if let Some(err) = &state.text.error {
        format!("The search failed: {err}")
    } else if let Some(progress) = state.text.job.as_ref().map(TextJob::progress) {
        format!("Reading the books' text: {} of {}. Results grow as it goes.", progress.done, progress.total)
    } else if state.text.search.is_some() {
        "Searching\u{2026}".to_owned()
    } else if by_book.is_empty() {
        "No page of a book on this shelf has these words.".to_owned()
    } else {
        String::new()
    };
    let line_height = ui.fonts_mut(|fonts| fonts.row_height(&theme::regular(NOTE_SIZE)));
    ui.allocate_ui_with_layout(vec2(ui.available_width(), line_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_size(vec2(ui.available_width(), line_height));
        ui.add(egui::Label::new(RichText::new(note).font(theme::regular(NOTE_SIZE)).color(t.text_muted)).truncate());
    });
    ui.add_space(GAP);
    let state = &app.margin_library;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = GAP / 2.0;
        for (book_index, hits) in &by_book {
            let Some(book) = state.books.get(*book_index) else { continue };
            ui.add_space(GAP);
            ui.label(RichText::new(&book.title).font(theme::medium(TITLE_SIZE)).color(t.text));
            for hit in hits.iter().take(SNIPPETS_PER_BOOK).filter_map(|index| state.text.hits.get(*index)) {
                ui.horizontal(|ui| {
                    ui.add_space(SNIPPET_INDENT);
                    let mut job = snippet_job(&hit.snippet, t, hit.page, hit.kind);
                    job.wrap.max_width = (ui.available_width() - GAP_LARGE).max(GAP);
                    let resp = ui.add(egui::Label::new(job).sense(Sense::click()));
                    let accessible = format!("{}, page {}", book.title, hit.page.saturating_add(1));
                    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &accessible));
                    if resp.hovered() {
                        ui.painter().hline(resp.rect.x_range(), resp.rect.bottom(), Stroke::new(look::EDGE, t.divider));
                    }
                    if resp.on_hover_text("Open at this page").clicked() {
                        actions.push(Action::OpenAt(book.path.clone(), hit.page));
                    }
                });
            }
            if hits.len() > SNIPPETS_PER_BOOK {
                ui.horizontal(|ui| {
                    ui.add_space(SNIPPET_INDENT);
                    let more = hits.len() - SNIPPETS_PER_BOOK;
                    ui.label(RichText::new(format!("and {more} more")).font(theme::regular(NOTE_SIZE)).color(t.text_faint));
                });
            }
        }
        ui.allocate_space(vec2(ui.available_width(), GAP_LARGE));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_mark_the_matched_words() {
        let t = Tokens::for_kind(crate::theme::ThemeKind::Light);
        let job = snippet_job(&format!("before {MATCH_START}word{MATCH_END} after"), &t, 4, HitKind::Page);
        assert_eq!(job.text, "p. 5   before word after");
        let colors: Vec<_> = job.sections.iter().map(|section| section.format.color).collect();
        assert_eq!(colors, [t.accent, t.text_muted, t.text, t.text_muted], "the place, then the match in the text colour");
    }
}
