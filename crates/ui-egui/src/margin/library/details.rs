//! A book's details sheet: its title, authors, year, DOI and arXiv id to edit, its tags, the
//! venue, ISBN and abstract an import or a lookup filled, and "Look up details", which asks
//! Crossref and arXiv about the book's DOI or arXiv id. Nothing goes over the network until that
//! button is pressed (or `library.metadata_lookup` is on, [`start_automatic`]).
//!
//! What the user saves here is theirs: no scan, import or lookup changes it again
//! (`margin_config::library_meta`).

use egui::{Align, Layout, RichText, vec2};
use margin_config::library::MAX_TAG_CHARS;
use margin_config::library_meta::{Details, DetailsEdit, LookupJob, LookupReport, LookupRequest, parse_year};

use super::super::look::{self, Button, ButtonSize, GAP};
use crate::PdfCraftApp;
use crate::theme::{self, Tokens};

/// The sheet's width, and its fields' width.
const SHEET_WIDTH: f32 = 560.0;
const FIELD_WIDTH: f32 = 400.0;
/// The label column's width.
const LABEL_WIDTH: f32 = 96.0;
/// The region under the fields (tags, venue, ISBN, file, abstract) is this tall whatever it
/// holds, and scrolls past it: a lookup or a new line of tags never resizes the sheet.
const FACTS_HEIGHT: f32 = 220.0;
const TEXT_SIZE: f32 = 13.0;
const NOTE_SIZE: f32 = 12.0;
/// The id of the field that adds a tag.
const NEW_TAG_ID: &str = "margin-details-new-tag";
const LOOKUP_TIP: &str = "Sends this book's DOI or arXiv id to api.crossref.org and export.arxiv.org and fills in what they know. \
                          Fields you edited stay as they are.";

/// What the sheet's fields hold, as typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Form {
    title: String,
    authors: String,
    year: String,
    doi: String,
    arxiv: String,
}

impl Form {
    fn of(details: &Details) -> Self {
        Self {
            title: details.title.clone(),
            authors: details.authors.clone(),
            year: details.year.map(|year| year.to_string()).unwrap_or_default(),
            doi: details.doi.clone().unwrap_or_default(),
            arxiv: details.arxiv.clone().unwrap_or_default(),
        }
    }

    /// The edit to save; a year that isn't one is refused here, DOI and arXiv id by the library.
    fn edit(&self) -> Result<DetailsEdit, String> {
        Ok(DetailsEdit {
            title: self.title.clone(),
            authors: self.authors.clone(),
            year: parse_year(&self.year)?,
            doi: self.doi.clone(),
            arxiv: self.arxiv.clone(),
        })
    }

    /// Takes from `fresh` each field the user hasn't changed since `before` was shown.
    fn refresh(&mut self, before: &Self, fresh: &Self) {
        let fields = [
            (&mut self.title, &before.title, &fresh.title),
            (&mut self.authors, &before.authors, &fresh.authors),
            (&mut self.year, &before.year, &fresh.year),
            (&mut self.doi, &before.doi, &fresh.doi),
            (&mut self.arxiv, &before.arxiv, &fresh.arxiv),
        ];
        for (shown, was, now) in fields {
            if shown == was {
                shown.clone_from(now);
            }
        }
    }
}

/// The open sheet.
pub(crate) struct Sheet {
    book: i64,
    path: String,
    loaded: Details,
    form: Form,
    /// The book's tags as edited (names).
    tags: Vec<String>,
    new_tag: String,
    /// Why the last save or lookup failed, or what the lookup did.
    note: Option<String>,
}

/// A lookup running: of one book (the sheet's button) or of every book never looked up.
pub(crate) struct Lookup {
    job: LookupJob,
    book: Option<i64>,
}

/// What a click in the sheet asks for.
enum Action {
    Save,
    Close,
    LookUp,
}

/// Opens the sheet for the book with row id `book`.
pub(crate) fn open(app: &mut PdfCraftApp, book: i64) {
    let state = &app.margin_library;
    let Some(db) = state.db.as_ref() else { return };
    let Some(listed) = state.books.iter().find(|listed| listed.id == book) else { return };
    let mut tags: Vec<String> = listed.tags.iter().filter_map(|tag| state.tag_names.get(tag).cloned()).collect();
    tags.sort_by_key(|name| name.to_lowercase());
    let path = listed.path.clone();
    match db.details(book) {
        Ok(loaded) => {
            let form = Form::of(&loaded);
            app.margin_library.details = Some(Sheet { book, path, loaded, form, tags, new_tag: String::new(), note: None });
        }
        Err(err) => {
            log::warn!("library: {err}");
            app.notify(format!("Couldn't read the book's details: {err}"));
        }
    }
}

/// Draws the sheet while it is open.
pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let Some(mut sheet) = app.margin_library.details.take() else { return };
    let looking_up = app.margin_library.lookup.as_ref().is_some_and(|lookup| lookup.book == Some(sheet.book));
    let action = draw(ctx, &mut sheet, looking_up);
    match action {
        None => app.margin_library.details = Some(sheet),
        Some(Action::Close) => {}
        Some(Action::Save) => match save(app, &sheet) {
            Ok(()) => {
                app.margin_library.stale = true;
                app.notify("Details saved");
            }
            Err(err) => {
                sheet.note = Some(err);
                app.margin_library.details = Some(sheet);
            }
        },
        Some(Action::LookUp) => {
            // The ids typed are the ones looked up: save them first.
            match save(app, &sheet) {
                Ok(()) => {
                    app.margin_library.stale = true;
                    sheet.loaded = reload(app, sheet.book).unwrap_or(sheet.loaded);
                    sheet.form = Form::of(&sheet.loaded);
                    sheet.note = start(app, Some(sheet.book)).err();
                }
                Err(err) => sheet.note = Some(err),
            }
            app.margin_library.details = Some(sheet);
        }
    }
}

/// Saves the form and the tags.
fn save(app: &PdfCraftApp, sheet: &Sheet) -> Result<(), String> {
    let db = app.margin_library.db.as_ref().ok_or("the library is unavailable")?;
    let edit = sheet.form.edit()?;
    db.edit_details(sheet.book, &edit)?;
    db.set_tags(sheet.book, &sheet.tags, margin_config::library::now_unix())
}

fn reload(app: &PdfCraftApp, book: i64) -> Option<Details> {
    let db = app.margin_library.db.as_ref()?;
    db.details(book).map_err(|err| log::warn!("library: {err}")).ok()
}

/// Starts a lookup of `book`, or of every book never looked up (`None`), replacing an automatic
/// one that runs.
pub(crate) fn start(app: &mut PdfCraftApp, book: Option<i64>) -> Result<(), String> {
    let state = &mut app.margin_library;
    let data = state.db.as_ref().map(|db| db.data_dir().to_path_buf()).ok_or("the library is unavailable")?;
    if let Some(running) = &state.lookup {
        if running.book.is_some() {
            return Err("a lookup is already running".to_owned());
        }
        running.job.cancel();
    }
    let wake = app.ctx.clone();
    let job = LookupJob::start(LookupRequest { data, book }, move || {
        if let Some(ctx) = &wake {
            ctx.request_repaint();
        }
    })
    .map_err(|err| format!("can't start the lookup: {err}"))?;
    app.margin_library.lookup = Some(Lookup { job, book });
    Ok(())
}

/// After a scan or an import, with `library.metadata_lookup` on: looks up the books with an id
/// that were never looked up. Turning the setting on later starts the pass then.
pub(crate) fn start_automatic(app: &mut PdfCraftApp) {
    // A pass that runs now keeps the newest books due for the next one.
    if !app.margin_keys.settings().library.metadata_lookup
        || app.margin_library.lookup.is_some()
        || !std::mem::take(&mut app.margin_library.lookup_due)
    {
        return;
    }
    if let Err(err) = start(app, None) {
        log::warn!("library: {err}");
    }
}

/// Every frame: takes a finished lookup's report.
pub(crate) fn poll(app: &mut PdfCraftApp) {
    let Some(lookup) = &app.margin_library.lookup else { return };
    let Some(result) = lookup.job.take_result() else { return };
    let book = lookup.book;
    app.margin_library.lookup = None;
    app.margin_library.stale = true;
    match (book, result) {
        (Some(book), Ok(report)) => looked_up(app, book, &report),
        (Some(book), Err(err)) => {
            if let Some(sheet) = app.margin_library.details.as_mut().filter(|sheet| sheet.book == book) {
                sheet.note = Some(err);
            }
        }
        (None, Ok(report)) => log::info!("library: looked up {} books online, {} updated", report.asked, report.updated),
        (None, Err(err)) => log::warn!("library: the online lookup failed: {err}"),
    }
}

/// A lookup of `book` ended: the open sheet shows what it found, keeping what was typed since.
fn looked_up(app: &mut PdfCraftApp, book: i64, report: &LookupReport) {
    let fresh = reload(app, book);
    let Some(sheet) = app.margin_library.details.as_mut().filter(|sheet| sheet.book == book) else {
        if let Some(reason) = &report.failure {
            app.notify(format!("Look up details: {reason}"));
        }
        return;
    };
    if let Some(fresh) = fresh {
        let before = Form::of(&sheet.loaded);
        sheet.form.refresh(&before, &Form::of(&fresh));
        sheet.loaded = fresh;
    }
    sheet.note = Some(match &report.failure {
        Some(reason) => format!("Nothing found: {reason}"),
        None if report.updated > 0 => "Filled in from Crossref and arXiv. Fields you edited stayed as they were.".to_owned(),
        None => "Nothing new found.".to_owned(),
    });
}

/// A label in the label column, left-aligned and centred on a row `height` tall: one cell for
/// every field, fact, the tags and the abstract, so their labels line up.
fn label_cell(ui: &mut egui::Ui, t: &Tokens, label: &str, height: f32) -> egui::Response {
    ui.allocate_ui_with_layout(vec2(LABEL_WIDTH, height), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_size(vec2(LABEL_WIDTH, height));
        ui.add(egui::Label::new(RichText::new(label).font(theme::regular(TEXT_SIZE)).color(t.text_muted)).truncate())
    })
    .inner
}

/// The height of one line of the sheet's text, for labels beside text rather than a field.
fn text_line(ui: &egui::Ui) -> f32 {
    ui.fonts_mut(|fonts| fonts.row_height(&theme::regular(TEXT_SIZE)))
}

/// One labelled single-line field, on one control row.
fn field(ui: &mut egui::Ui, t: &Tokens, label: &str, value: &mut String, hint: &str) {
    look::control_row(ui, |ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        let label = label_cell(ui, t, label, look::CONTROL_ROW);
        let id = ui.id().with(label.id);
        look::text_field(ui, id, value, hint, FIELD_WIDTH).labelled_by(label.id);
    });
}

/// One labelled line of text that isn't edited here.
fn fact(ui: &mut egui::Ui, t: &Tokens, label: &str, value: &str) {
    let line = text_line(ui);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        label_cell(ui, t, label, line);
        ui.add(egui::Label::new(RichText::new(value).font(theme::regular(TEXT_SIZE)).color(t.text)).wrap());
    });
}

/// The field that adds a tag, then the tags as chips under it (a click removes one).
fn tags_row(ui: &mut egui::Ui, t: &Tokens, sheet: &mut Sheet) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        label_cell(ui, t, "Tags", look::CONTROL_ROW);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = GAP / 2.0;
            let id = egui::Id::new(NEW_TAG_ID);
            let resp = look::text_field(ui, id, &mut sheet.new_tag, "Add a tag", FIELD_WIDTH / 2.0);
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                add_tag(sheet);
                resp.request_focus();
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(GAP / 2.0, GAP / 2.0);
                let mut removed = None;
                for (index, name) in sheet.tags.iter().enumerate() {
                    let tip = format!("Remove the tag {name}");
                    if Button::secondary(ButtonSize::Compact, name).trailing("x").tip(&tip).show(ui).clicked() {
                        removed = Some(index);
                    }
                }
                if let Some(index) = removed {
                    sheet.tags.remove(index);
                }
            });
        });
    });
}

/// Adds the typed tag, once whatever its case, at most [`MAX_TAG_CHARS`] long.
fn add_tag(sheet: &mut Sheet) {
    let name: String = std::mem::take(&mut sheet.new_tag).trim().chars().filter(|c| !c.is_control()).take(MAX_TAG_CHARS).collect();
    let key = name.to_lowercase();
    if !name.is_empty() && !sheet.tags.iter().any(|tag| tag.to_lowercase() == key) {
        sheet.tags.push(name);
    }
}

/// The sheet: its header, the five fields, the fixed-height region of tags and facts, the note
/// line (always there, one line), and the buttons. Its size never changes while it is open.
fn draw(ctx: &egui::Context, sheet: &mut Sheet, looking_up: bool) -> Option<Action> {
    let t = Tokens::get(ctx);
    let mut action = None;
    let modal = look::modal(ctx, egui::Id::new("margin-book-details"), &t).show(ctx, |ui| {
        ui.set_width(SHEET_WIDTH);
        ui.spacing_mut().item_spacing = vec2(GAP, GAP);
        if look::sheet_header(ui, &t, "Details") {
            action = Some(Action::Close);
        }
        ui.add_space(GAP);
        field(ui, &t, "Title", &mut sheet.form.title, "Title");
        field(ui, &t, "Authors", &mut sheet.form.authors, "Ada Lovelace, Grace Hopper");
        field(ui, &t, "Year", &mut sheet.form.year, "2024");
        field(ui, &t, "DOI", &mut sheet.form.doi, "10.1145/3290605.3300857");
        field(ui, &t, "arXiv", &mut sheet.form.arxiv, "2101.01234");
        egui::ScrollArea::vertical()
            .id_salt("margin-details-facts")
            .auto_shrink([false, false])
            .min_scrolled_height(FACTS_HEIGHT)
            .max_height(FACTS_HEIGHT)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(GAP, GAP);
                tags_row(ui, &t, sheet);
                if let Some(venue) = &sheet.loaded.publisher {
                    fact(ui, &t, "Venue", venue);
                }
                if let Some(isbn) = &sheet.loaded.isbn {
                    fact(ui, &t, "ISBN", isbn);
                }
                fact(ui, &t, "File", &sheet.path);
                if let Some(text) = &sheet.loaded.abstract_text {
                    fact(ui, &t, "Abstract", text);
                }
            });
        let line = ui.fonts_mut(|fonts| fonts.row_height(&theme::regular(NOTE_SIZE)));
        ui.allocate_ui_with_layout(vec2(ui.available_width(), line), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_size(vec2(ui.available_width(), line));
            if let Some(note) = &sheet.note {
                ui.add(egui::Label::new(RichText::new(note).font(theme::regular(NOTE_SIZE)).color(t.text_muted)).truncate());
            }
        });
        ui.add_space(GAP);
        look::control_row(ui, |ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            let has_id = !sheet.form.doi.trim().is_empty() || !sheet.form.arxiv.trim().is_empty();
            let lookup_label = if looking_up { "Looking up\u{2026}" } else { "Look up details" };
            let tip = if has_id { LOOKUP_TIP } else { "Add a DOI or an arXiv id to look this book up" };
            let lookup = Button::secondary(ButtonSize::Standard, lookup_label).icon("search").tip(tip);
            let width = lookup.widest(ui, &["Look up details", "Looking up\u{2026}"]);
            if ui.add_enabled_ui(has_id && !looking_up, |ui| lookup.fixed_width(width).show(ui)).inner.clicked() {
                action = Some(Action::LookUp);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if look::button_primary(ui, ButtonSize::Standard, "Save").clicked() {
                    action = Some(Action::Save);
                }
                if look::button_secondary(ui, ButtonSize::Standard, "Cancel").clicked() {
                    action = Some(Action::Close);
                }
            });
        });
    });
    if action.is_none() && modal.should_close() {
        action = Some(Action::Close);
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lookup_fills_only_the_fields_left_alone() {
        let before = Form { title: "Old".into(), authors: String::new(), year: String::new(), doi: "10.1/x".into(), arxiv: String::new() };
        let mut shown = Form { title: "Typed".into(), ..before.clone() };
        let fresh = Form { title: "Found".into(), authors: "Ada Lovelace".into(), year: "1843".into(), ..before.clone() };
        shown.refresh(&before, &fresh);
        assert_eq!((shown.title.as_str(), shown.authors.as_str(), shown.year.as_str()), ("Typed", "Ada Lovelace", "1843"));
        assert!(Form { year: "soon".into(), ..before }.edit().is_err());
    }

    #[test]
    fn tags_are_added_once_and_capped() {
        let mut sheet = Sheet {
            book: 1,
            path: String::new(),
            loaded: Details::default(),
            form: Form::default(),
            tags: vec!["ML".into()],
            new_tag: " ml ".into(),
            note: None,
        };
        add_tag(&mut sheet);
        sheet.new_tag = "x".repeat(MAX_TAG_CHARS * 2);
        add_tag(&mut sheet);
        sheet.new_tag = " \u{7} ".into();
        add_tag(&mut sheet);
        assert_eq!(sheet.tags.len(), 2);
        assert_eq!(sheet.tags.get(1).map(|tag| tag.chars().count()), Some(MAX_TAG_CHARS));
    }
}
