//! R8 QoL 24: the footer's scrubber in the real shell (egui_kittest). Dragging moves through the
//! document as it goes, the card at the pointer names the page by its label, the drag ends on
//! the page under the pointer, and one Back returns to where it started.

use egui::{Key, Modifiers, Pos2, Rect, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};

/// The first pages are numbered i, ii, iii, iv; the rest 1, 2, 3, ….
const FRONT_MATTER: usize = 4;

/// `pages` small pages of text, labelled as [`FRONT_MATTER`] says, with sections on the first
/// numbered page and halfway.
fn fixture(pages: usize) -> Vec<u8> {
    let page_obj = |i: usize| 7 + 2 * i;
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", page_obj(i))).collect();
    let half = pages / 2;
    let first = FRONT_MATTER.min(pages - 1);
    let mut pdf = format!(
        "%PDF-1.7\n\
         1 0 obj << /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /PageLabels << /Nums [0 << /S /r >> {FRONT_MATTER} << /S /D >>] >> >> endobj\n\
         2 0 obj << /Type /Pages /Kids [{}] /Count {pages} >> endobj\n\
         3 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\n\
         4 0 obj << /Type /Outlines /First 5 0 R /Last 6 0 R /Count 2 >> endobj\n\
         5 0 obj << /Title (Opening) /Parent 4 0 R /Next 6 0 R /Dest [{} 0 R /Fit] >> endobj\n\
         6 0 obj << /Title (Middle) /Parent 4 0 R /Prev 5 0 R /Dest [{} 0 R /Fit] >> endobj\n",
        kids.join(" "),
        page_obj(first),
        page_obj(half),
    );
    for i in 0..pages {
        let text = format!("BT /F1 12 Tf 20 150 Td (Page {}) Tj ET", i + 1);
        pdf += &format!(
            "{} 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >> endobj\n\
             {} 0 obj << /Length {} >> stream\n{text}\nendstream endobj\n",
            page_obj(i),
            page_obj(i) + 1,
            page_obj(i) + 1,
            text.len(),
        );
    }
    pdf += "trailer << /Root 1 0 R >>\n%%EOF";
    pdf.into_bytes()
}

fn harness(pages: usize) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        // The shipped keymap and settings, and no library or stats rows in the user's data.
        let dir = std::env::temp_dir().join(format!("margin-scrubber-test-{}-{pages}", std::process::id()));
        app.margin_keys.set_config_dir(dir.join("config"));
        app.margin_keys.set_data_dir(dir.join("data"));
        app.open_bytes("pages.pdf", None, fixture(pages)).expect("opens");
        app
    });
    h.run_steps(6);
    h
}

fn current(h: &Harness<'static, PdfCraftApp>) -> usize {
    h.state().views[0].current
}

fn track(h: &Harness<'static, PdfCraftApp>, pages: usize) -> Rect {
    h.get_by_label(&format!("Page {} of {pages}", current(h) + 1)).rect()
}

fn along(rect: Rect, share: f32) -> Pos2 {
    // Half a point in from either end: on a 500-page track a whole point is already page 2.
    let inset = 0.5;
    rect.left_center() + vec2(inset + (rect.width() - 2.0 * inset) * share, 0.0)
}

#[test]
fn dragging_the_scrubber_moves_through_the_document_and_one_back_returns() {
    const PAGES: usize = 500;
    const MOVES: usize = 8;
    let mut h = harness(PAGES);
    let rect = track(&h, PAGES);
    h.hover_at(along(rect, 0.0));
    h.run_steps(2);
    // The card at the pointer names the page by its label and says where it is.
    h.get_by_label("p. i");
    h.get_by_label(&format!("1 / {PAGES}"));
    h.drag_at(along(rect, 0.0));
    h.run_steps(1);
    let mut seen = Vec::new();
    for k in 1..=MOVES {
        h.hover_at(along(rect, k as f32 / MOVES as f32 * 0.9));
        h.run_steps(1);
        seen.push(current(&h));
    }
    assert!(seen.windows(2).all(|w| w[1] >= w[0]) && seen[MOVES / 2] > 0, "the view follows the drag: {seen:?}");
    h.run_steps(2);
    // The card, the page field and the view agree on the page the drag reached.
    let shown = current(&h);
    let label = h.state().session.get(h.state().views[0].id).unwrap().info.pages[shown].label.clone();
    let label = if label.is_empty() { (shown + 1).to_string() } else { label };
    let field = format!("{:?}", h.get_by_label("Go to page"));
    assert!(field.contains(&format!("value: \"{label}\"")), "the page field shows the page the drag reached ({label}): {field}");
    h.get_by_label(&format!("p. {label}"));
    assert!(h.query_all_by_label("Middle").count() >= 2, "the card names the section, beside its Contents row");
    h.drop_at(along(rect, 1.0));
    h.run_steps(4);
    assert_eq!(current(&h), PAGES - 1, "the drag ends on the last page");
    assert_eq!(h.state().views[0].back.len(), 1, "the whole drag is one history entry");
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowLeft);
    h.run_steps(4);
    assert_eq!(current(&h), 0, "one Back returns to the start");
}

#[test]
fn a_one_page_document_scrubs_without_errors() {
    let mut h = harness(1);
    let rect = track(&h, 1);
    h.get_by_label("Page 1 of 1").click();
    h.run_steps(2);
    h.hover_at(along(rect, 0.5));
    h.run_steps(2);
    h.get_by_label("p. i");
    h.drag_at(along(rect, 0.0));
    h.run_steps(1);
    h.hover_at(along(rect, 1.0));
    h.run_steps(1);
    h.drop_at(along(rect, 1.0));
    h.run_steps(3);
    assert_eq!(current(&h), 0);
    assert!(h.state().views[0].back.len() <= 1);
}
