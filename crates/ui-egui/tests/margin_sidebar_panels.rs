//! The sidebar's Attachments and Layers tabs: shown only for a PDF with embedded files or
//! optional-content layers, opened from the tab strip, the document menu or `view.sidebar.tab`.

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use pdfcraft_ui_egui::margin::{self, SidebarTab};
use pdfcraft_ui_egui::{Flavor, PdfCraftApp, RightPanel};

const DOCUMENT_MENU: &str = "Document: save, export, print, settings";
const WINDOW: egui::Vec2 = egui::vec2(1440.0, 900.0);
const CLICK_STEPS: usize = 3;

/// One page with two layers (Red on, Green off) and one embedded file, `notes.txt`.
fn rich_pdf() -> Vec<u8> {
    let content = "/OC /R BDC 1 0 0 rg 20 300 260 40 re f EMC /OC /G BDC 0 1 0 rg 20 240 260 40 re f EMC";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Names [(notes.txt) 8 0 R] >> >> /OCProperties << /OCGs [5 0 R 6 0 R] /D << /OFF [6 0 R] /Order [5 0 R 6 0 R] >> >> >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 4 0 R /Resources << /Properties << /R 5 0 R /G 6 0 R >> >> >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /OCG /Name (Red) >>".to_owned(),
        "<< /Type /OCG /Name (Green) >>".to_owned(),
        "<< /Type /EmbeddedFile /Length 11 >>\nstream\nhello notes\nendstream".to_owned(),
        "<< /Type /Filespec /F (notes.txt) /UF (notes.txt) /EF << /F 7 0 R >> >>".to_owned(),
    ];
    with_xref(&objs)
}

/// One page with neither layers nor embedded files.
fn plain_pdf() -> Vec<u8> {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] >>".to_owned(),
    ];
    with_xref(&objs)
}

fn with_xref(objs: &[String]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{obj}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

/// Margin with `docs` open (the last one shown) and the sidebar open on Contents.
fn harness(docs: Vec<(&'static str, Vec<u8>)>) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(WINDOW).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").expect("language option");
        app.margin_keys.set_data_dir(std::env::temp_dir().join(format!("margin-panels-data-{}", std::process::id())));
        for (name, bytes) in &docs {
            app.open_bytes(name, None, bytes.clone()).expect("opens");
        }
        app
    });
    h.run_steps(4);
    assert!(margin::run(h.state_mut(), "view.sidebar.tab", &serde_json::json!({ "tab": "contents" })));
    h.run_steps(CLICK_STEPS);
    h
}

fn click(h: &mut Harness<'static, PdfCraftApp>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(CLICK_STEPS);
}

/// How many widgets carry `label` (a tab and a menu row may share it).
fn count(h: &Harness<'static, PdfCraftApp>, label: &str) -> usize {
    h.query_all_by_label(label).count()
}

#[test]
fn a_pdf_with_attachments_and_layers_gets_both_tabs() {
    let mut h = harness(vec![("rich.pdf", rich_pdf())]);
    assert_eq!(count(&h, "Attachments"), 1, "the Attachments tab shows");
    assert_eq!(count(&h, "Layers"), 1, "the Layers tab shows");

    click(&mut h, "Attachments");
    assert_eq!(h.state().margin_chrome.tab, SidebarTab::Attachments);
    assert_eq!(h.state().right, Some(RightPanel::Attachments));
    assert!(h.query_by_label_contains("notes.txt").is_some(), "the attachment is listed");

    click(&mut h, "Layers");
    assert_eq!(h.state().margin_chrome.tab, SidebarTab::Layers);
    assert_eq!(h.state().right, Some(RightPanel::Layers));
    assert!(h.query_by_label_contains("Green").is_some(), "the layers are listed");
}

#[test]
fn the_document_menu_and_the_keymap_open_them() {
    let mut h = harness(vec![("rich.pdf", rich_pdf())]);
    click(&mut h, DOCUMENT_MENU);
    assert_eq!(count(&h, "Layers"), 2, "the menu adds a Layers row to the tab");
    // The tab sits in the sidebar at the left; the menu row hangs from the header's right.
    let right_edge = |node: &egui_kittest::Node<'_>| node.accesskit_node().bounding_box().map_or(f64::MIN, |rect| rect.x0);
    let menu_row = h.query_all_by_label("Layers").max_by(|a, b| right_edge(a).total_cmp(&right_edge(b)));
    menu_row.expect("a Layers menu row").click();
    h.run_steps(CLICK_STEPS);
    assert_eq!(h.state().margin_chrome.tab, SidebarTab::Layers);

    assert!(margin::run(h.state_mut(), "view.sidebar.tab", &serde_json::json!({ "tab": "attachments" })));
    h.run_steps(CLICK_STEPS);
    assert_eq!(h.state().margin_chrome.tab, SidebarTab::Attachments);
}

#[test]
fn a_plain_pdf_has_neither_and_falls_back_to_contents() {
    let mut h = harness(vec![("rich.pdf", rich_pdf()), ("plain.pdf", plain_pdf())]);
    assert_eq!(count(&h, "Attachments"), 0, "no Attachments tab");
    assert_eq!(count(&h, "Layers"), 0, "no Layers tab");
    click(&mut h, DOCUMENT_MENU);
    assert_eq!(count(&h, "Attachments") + count(&h, "Layers"), 0, "no menu rows either");
    h.key_press(egui::Key::Escape);
    h.run_steps(CLICK_STEPS);
    // The key is spent, saying why; the tab stays.
    assert!(margin::run(h.state_mut(), "view.sidebar.tab", &serde_json::json!({ "tab": "layers" })));
    h.run_steps(CLICK_STEPS);
    assert_eq!(h.state().margin_chrome.tab, SidebarTab::Contents);

    // The rich document's Attachments tab gives way to Contents in the plain one.
    assert!(margin::run(h.state_mut(), "view.tab.go", &serde_json::json!({ "index": 1 })));
    h.run_steps(CLICK_STEPS);
    click(&mut h, "Attachments");
    assert_eq!(h.state().margin_chrome.tab, SidebarTab::Attachments);
    assert!(margin::run(h.state_mut(), "view.tab.go", &serde_json::json!({ "index": 2 })));
    h.run_steps(CLICK_STEPS);
    assert_eq!(h.state().margin_chrome.tab, SidebarTab::Contents);
    assert_eq!(h.state().right, None);
}
