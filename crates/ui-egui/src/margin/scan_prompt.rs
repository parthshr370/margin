//! An opt-in text layer for image-only pages. Dismissal lasts for this open document;
//! recognition uses the existing undoable OCR worker, never changes the page image.

use std::collections::HashMap;

use super::look::{Button, ButtonSize};
use crate::canvas::DocView;
use crate::ocr_ui::OcrPages;
use crate::{Flavor, PdfCraftApp};

const SCAN_MIN_AREA: f64 = 0.5;
const PROMPT_WIDTH: f32 = 380.0;
const PROMPT_INSET: f32 = 12.0;

pub const MISSING_MODELS: &str = "This build of Margin lacks text recognition: the recognition models are missing. Install a complete Margin release from github.com/parthshr370/margin/releases to make scanned text selectable.";

#[derive(Default)]
pub struct ScanPrompt {
    dismissed: bool,
    pending: Option<usize>,
    generation: Option<u64>,
    scans: HashMap<usize, bool>,
}

impl ScanPrompt {
    pub(crate) fn is_open(&self) -> bool {
        self.pending.is_some()
    }
}

/// An empty text layer alone is not a scan: blank and vector-only pages stay quiet.
pub(crate) fn is_scan(view: &mut DocView, doc: &pdfcraft_engine::Document, page: usize) -> bool {
    let has_no_text = view.page_text(page).map_or_else(|| view.text_failed.contains(&page), |text| text.glyphs.is_empty());
    if !has_no_text {
        return false;
    }
    let generation = doc.edit_generation();
    if view.scan_prompt.generation != Some(generation) {
        view.scan_prompt.generation = Some(generation);
        view.scan_prompt.scans.clear();
    }
    *view.scan_prompt.scans.entry(page).or_insert_with(|| {
        let Some(info) = doc.info.pages.get(page) else { return false };
        let page_area = f64::from(info.width) * f64::from(info.height);
        doc.page_images(page).iter().any(|image| {
            let [a, b, c, d, _, _] = image.matrix;
            let area = (a * d - b * c).abs();
            image.width > 1 && image.height > 1 && scan_area(area, page_area)
        })
    })
}

fn scan_area(image_area: f64, page_area: f64) -> bool {
    image_area.is_finite() && page_area.is_finite() && page_area > 0.0 && image_area / page_area >= SCAN_MIN_AREA
}

/// Intercept the first selection/highlight gesture; after dismissal area highlights still work.
pub(crate) fn request(view: &mut DocView, doc: &pdfcraft_engine::Document, page: usize) -> bool {
    if view.scan_prompt.dismissed || !is_scan(view, doc, page) {
        return false;
    }
    view.scan_prompt.pending = Some(page);
    true
}

/// The Find tab supplies the page that could not be searched, not merely a failed extraction.
pub(crate) fn find(ui: &mut egui::Ui, app: &mut PdfCraftApp, index: usize) {
    let Some(view) = app.views.get_mut(index) else { return };
    if view.scan_prompt.dismissed || view.scan_prompt.is_open() || app.ocr_run.is_some() {
        return;
    }
    let Some(doc) = app.session.get(view.id) else { return };
    let page = (0..doc.info.pages.len()).find(|&page| is_scan(view, doc, page));
    if let Some(page) = page {
        body(ui, app, index, page);
    }
}

pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context, index: usize) {
    let Some(view) = app.views.get(index) else { return };
    let page = view.scan_prompt.pending;
    let running = app.ocr_run.as_ref().is_some_and(|run| run.doc == view.id);
    if page.is_none() && !running {
        return;
    }
    let position = view.viewport_rect().center_top() + egui::vec2(0.0, PROMPT_INSET);
    egui::Area::new(egui::Id::new(("margin-scan-prompt", view.id)))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::CENTER_TOP)
        .fixed_pos(position)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(PROMPT_WIDTH);
                if running {
                    progress(ui, app);
                } else if let Some(page) = page {
                    body(ui, app, index, page);
                }
            });
        });
}

fn progress(ui: &mut egui::Ui, app: &mut PdfCraftApp) {
    let Some(run) = app.ocr_run.as_ref() else { return };
    let status = run.progress.lock().ok().map(|status| (status.done, status.total, status.cancel));
    if let Some((done, total, cancelled)) = status {
        ui.label(format!("Recognizing text… {done} of {total} pages"));
        ui.label("Recognition changes the PDF. You can undo it in one step.");
        if cancelled {
            ui.label("Stopping after this page…");
        } else if Button::secondary(ButtonSize::Compact, "Cancel recognition").show(ui).clicked() {
            app.cancel_ocr();
        }
    } else {
        ui.label("Text recognition could not report progress.");
    }
}

fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, index: usize, page: usize) {
    let Some(view) = app.views.get(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let id = view.id;
    let blocked = doc.read_only_reason.clone().or_else(|| blocked(app, view.id).map(str::to_owned));
    #[cfg(not(target_arch = "wasm32"))]
    let file_read_only = doc.path.as_ref().is_some_and(|path| std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().readonly()));
    #[cfg(target_arch = "wasm32")]
    let file_read_only = false;
    ui.label("This page is scanned. Make text selectable?");
    ui.label("Recognition adds invisible text to the PDF, without changing the image. It is one undoable edit.");
    let available = pdfcraft_engine::ocr::available();
    if !available {
        ui.label(MISSING_MODELS);
    }
    if let Some(reason) = &blocked {
        ui.label(reason);
    } else if file_read_only {
        ui.label("This file is read-only. Save a writable copy before recognizing text.");
    }
    let mut pages = None;
    let mut save_as = false;
    let mut dismiss = false;
    ui.horizontal_wrapped(|ui| {
        ui.add_enabled_ui(available && blocked.is_none() && !file_read_only && app.ocr_run.is_none(), |ui| {
            if Button::secondary(ButtonSize::Compact, "This page").show(ui).clicked() {
                pages = Some(OcrPages::Current);
            }
            if Button::secondary(ButtonSize::Compact, "Whole document").show(ui).clicked() {
                pages = Some(OcrPages::All);
            }
        });
        if file_read_only && Button::secondary(ButtonSize::Compact, "Save as…").show(ui).clicked() {
            save_as = true;
        }
        if Button::secondary(ButtonSize::Compact, "Not now").show(ui).clicked() {
            dismiss = true;
        }
    });
    if dismiss {
        if let Some(view) = app.views.get_mut(index) {
            view.scan_prompt.dismissed = true;
            view.scan_prompt.pending = None;
        }
    } else if save_as {
        app.save_view(index, crate::editing::SaveTarget::As);
    } else if let Some(pages) = pages {
        // Find may offer a scan on another page: recognition must target that page.
        if let Some(view) = app.views.get_mut(index).filter(|view| view.id == id) {
            view.go_to_page(page);
            view.scan_prompt.pending = None;
        }
        app.ocr_draft.pages = pages;
        app.start_ocr();
    }
}

/// Recognition is a content edit, not a sidecar mark; PDF permissions still apply.
pub(crate) fn blocked(app: &PdfCraftApp, id: pdfcraft_engine::DocId) -> Option<&'static str> {
    if app.flavor != Flavor::Margin {
        return None;
    }
    let doc = app.session.get(id)?;
    if doc.is_signed() {
        Some("This PDF is signed. Text recognition changes its contents; use an unsigned copy instead.")
    } else if doc.security_summary().is_some_and(|security| !security.owner && !security.permissions.modify()) {
        Some("This PDF does not allow content changes. Text recognition needs an unrestricted copy or the owner password.")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_requires_a_finite_image_covering_half_a_real_page() {
        assert!(scan_area(50.0, 100.0));
        assert!(!scan_area(49.0, 100.0));
        assert!(!scan_area(1.0, 0.0));
        assert!(!scan_area(f64::INFINITY, 100.0));
        assert!(!scan_area(100.0, f64::NAN));
    }
}
