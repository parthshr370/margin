//! An opt-in text layer for image-only pages. Dismissal lasts for this open document;
//! recognition uses the existing undoable OCR worker, never changes the page image.

use std::collections::HashMap;

use super::look::{self, Button, ButtonSize, GAP, ROW_PAD};
use crate::canvas::DocView;
use crate::ocr_ui::OcrPages;
use crate::theme::{self, Tokens};
use crate::{Flavor, PdfCraftApp};

const SCAN_MIN_AREA: f64 = 0.5;
const PROMPT_WIDTH: f32 = 380.0;
const PROMPT_INSET: f32 = 12.0;
const TITLE_TEXT: f32 = 13.0;
/// The size of Find's status lines, so the offer reads as part of that panel.
const NOTE_TEXT: f32 = 12.0;

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

/// The canvas hook: in Margin, the first select or highlight gesture on a scan opens the
/// prompt, and area marks wait while the prompt is open. After dismissal area highlights work.
pub(crate) fn intercepts(
    flavor: Flavor,
    tool: crate::QuickTool,
    response: &egui::Response,
    view: &mut DocView,
    doc: &pdfcraft_engine::Document,
    page: usize,
) -> bool {
    if flavor != Flavor::Margin {
        return false;
    }
    let is_gesture = matches!(tool, crate::QuickTool::Select | crate::QuickTool::Comment(crate::comments::CommentTool::Highlight))
        && (response.drag_started() || response.double_clicked());
    if is_gesture && !view.scan_prompt.dismissed && is_scan(view, doc, page) {
        view.scan_prompt.pending = Some(page);
    }
    view.scan_prompt.is_open()
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
        // Indented like the Find status lines above it.
        ui.horizontal(|ui| {
            ui.add_space(ROW_PAD);
            ui.vertical(|ui| body(ui, app, index, page));
        });
    }
}

/// The prompt over the page's top: a floating banner of one fixed width (its question, or the
/// recognition's progress, wraps inside it, so a changing page count never resizes or recentres
/// it), with a × that dismisses the question like "Not now".
pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context, index: usize) {
    let Some(view) = app.views.get(index) else { return };
    let page = view.scan_prompt.pending;
    let running = app.ocr_run.as_ref().is_some_and(|run| run.doc == view.id);
    if page.is_none() && !running {
        return;
    }
    let area = view.viewport_rect();
    let width = PROMPT_WIDTH.min(area.width() - 2.0 * PROMPT_INSET).max(0.0);
    let position = area.center_top() + egui::vec2(0.0, PROMPT_INSET);
    let closed = egui::Area::new(egui::Id::new(("margin-scan-prompt", view.id)))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::CENTER_TOP)
        .fixed_pos(position)
        .show(ctx, |ui| {
            let close = (!running).then_some("Dismiss the scan prompt");
            look::floating_banner(ui, width, close, |ui| {
                if running {
                    progress(ui, app);
                } else if let Some(page) = page {
                    body(ui, app, index, page);
                }
            })
            .1
        })
        .inner;
    if closed && let Some(view) = app.views.get_mut(index) {
        view.scan_prompt.dismissed = true;
        view.scan_prompt.pending = None;
    }
}

fn progress(ui: &mut egui::Ui, app: &mut PdfCraftApp) {
    let Some(run) = app.ocr_run.as_ref() else { return };
    let status = run.progress.lock().ok().map(|status| (status.done, status.total, status.cancel));
    if let Some((done, total, cancelled)) = status {
        title(ui, &format!("Recognizing text… {done} of {total} pages"));
        note(ui, "Recognition changes the PDF. You can undo it in one step.");
        if cancelled {
            note(ui, "Stopping after this page…");
        } else if Button::secondary(ButtonSize::Compact, "Cancel recognition").show(ui).clicked() {
            app.cancel_ocr();
        }
    } else {
        note(ui, "Text recognition could not report progress.");
    }
}

/// The prompt's question or progress line.
fn title(ui: &mut egui::Ui, text: &str) {
    let tokens = Tokens::get(ui.ctx());
    ui.add(egui::Label::new(egui::RichText::new(text).font(theme::medium(TITLE_TEXT)).color(tokens.text)).wrap());
}

/// The prompt's explanations, in the muted style of Find's status lines.
fn note(ui: &mut egui::Ui, text: &str) {
    let tokens = Tokens::get(ui.ctx());
    ui.add(egui::Label::new(egui::RichText::new(text).font(theme::regular(NOTE_TEXT)).color(tokens.text_muted)).wrap());
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
    title(ui, "This page is scanned. Make text selectable?");
    note(ui, "Recognition adds invisible text to the PDF, without changing the image. It is one undoable edit.");
    let available = pdfcraft_engine::ocr::available();
    if !available {
        note(ui, MISSING_MODELS);
    }
    if let Some(reason) = &blocked {
        note(ui, reason);
    } else if file_read_only {
        note(ui, "This file is read-only. Save a writable copy before recognizing text.");
    }
    ui.add_space(GAP);
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
