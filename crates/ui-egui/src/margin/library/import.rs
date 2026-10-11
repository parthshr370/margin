//! Library ▸ Import BibTeX or Zotero…: a BibTeX file (Zotero, Better BibTeX, JabRef and
//! Mendeley export one) or Zotero's own `zotero.sqlite`, read on a worker thread
//! (`margin_config::library_meta::import`). Each entry fills the details and tags of the book it
//! matches, or adds the PDF it names; a toast says how many entries matched, were added and were
//! skipped. Nothing the user edited is changed.

use std::path::PathBuf;

use margin_config::library_meta::{ImportJob, ImportReport, ImportRequest};

use crate::PdfCraftApp;

/// The menu row's label.
const LABEL: &str = "Import BibTeX or Zotero\u{2026}";

/// The Library app menu's import row (`header::APP_MENU`).
pub(crate) fn item(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let running = app.margin_library.import.is_some();
    let label = if running { "Importing\u{2026}" } else { LABEL };
    if ui.add_enabled(!running, egui::Button::new(label)).clicked() {
        ui.close();
        pick(app);
    }
}

/// Asks for the file to import (`library.import`).
pub(crate) fn pick(app: &mut PdfCraftApp) {
    if app.margin_library.import.is_some() {
        app.notify("An import is already running");
        return;
    }
    let dialog = rfd::AsyncFileDialog::new()
        .set_title("Import a BibTeX file or Zotero's zotero.sqlite")
        .add_filter("BibTeX or Zotero", &["bib", "bibtex", "sqlite"])
        .add_filter("BibTeX", &["bib", "bibtex"])
        .add_filter("Zotero database", &["sqlite"]);
    app.ask_one(crate::pickers::Ask::File(dialog), None, start);
}

/// Imports `source` into the library on a worker thread.
pub(crate) fn start(app: &mut PdfCraftApp, source: PathBuf) {
    if !super::ensure_db(app) {
        return;
    }
    let Some(data) = app.margin_library.db.as_ref().map(|db| db.data_dir().to_path_buf()) else { return };
    let request = ImportRequest { data, source };
    let job = if app.margin_library.run_inline || app.run_inline {
        ImportJob::run_inline(&request)
    } else {
        let wake = app.ctx.clone();
        let started = ImportJob::start(request, move || {
            if let Some(ctx) = &wake {
                ctx.request_repaint();
            }
        });
        match started {
            Ok(job) => job,
            Err(err) => {
                app.notify(format!("Couldn't start the import: {err}"));
                return;
            }
        }
    };
    app.margin_library.import = Some(job);
    poll(app);
}

/// What an import did, in one line.
fn summary(report: &ImportReport) -> String {
    let stopped = if report.cancelled { " (stopped early)" } else { "" };
    format!(
        "Imported {} entries{stopped}: {} matched books in the library, {} added, {} skipped",
        report.entries, report.matched, report.added, report.skipped
    )
}

/// Every frame: takes a finished import's report.
pub(crate) fn poll(app: &mut PdfCraftApp) {
    let Some(result) = app.margin_library.import.as_ref().and_then(ImportJob::take_result) else { return };
    let state = &mut app.margin_library;
    state.import = None;
    state.stale = true;
    // New books have new covers and text; their ids may be looked up.
    state.covers.scan_ended();
    state.text.read_again();
    state.lookup_due = true;
    match result {
        Ok(report) => {
            log::info!("library: {}", summary(&report));
            app.notify(summary(&report));
        }
        Err(err) => {
            log::warn!("library: the import failed: {err}");
            app.notify(format!("The import failed: {err}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_toast_counts_every_entry() {
        let report = ImportReport { entries: 5, matched: 2, added: 1, skipped: 2, cancelled: false };
        assert_eq!(summary(&report), "Imported 5 entries: 2 matched books in the library, 1 added, 2 skipped");
    }
}
