//! Compare files in Margin: the document menu's "Compare with…" runs PdfCraft's comparison,
//! and a side-by-side comparison takes Margin's page area (old file left, new file right).

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_engine::Session;
use pdfcraft_ui_egui::{Dialog, Flavor, PdfCraftApp};

const DOCUMENT_MENU: &str = "Document: save, export, print, settings";
const WINDOW: egui::Vec2 = egui::vec2(1400.0, 900.0);
const MENU_STEPS: usize = 3;

fn harness() -> Harness<'static, PdfCraftApp> {
    let session = Session::new();
    let old = session.create_from_text("t", "Delivery within five days. Returns accepted.").expect("old version").to_vec();
    let new = session.create_from_text("t", "Delivery within three days. Returns accepted for a week.").expect("new version").to_vec();
    let mut h = Harness::builder().with_size(WINDOW).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").expect("language option");
        app.margin_keys.set_data_dir(std::env::temp_dir().join(format!("margin-compare-data-{}", std::process::id())));
        app.open_bytes("v1.pdf", None, old.clone()).expect("v1 opens");
        app.open_bytes("v2.pdf", None, new.clone()).expect("v2 opens");
        app
    });
    h.run_steps(4);
    h
}

fn click(h: &mut Harness<'static, PdfCraftApp>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(MENU_STEPS);
}

#[test]
fn compare_with_from_the_document_menu_shows_both_files_side_by_side() {
    let mut h = harness();
    click(&mut h, DOCUMENT_MENU);
    click(&mut h, "Compare with…");
    assert_eq!(h.state().dialog, Some(Dialog::CompareFiles));
    click(&mut h, "Compare");
    let side_by_side = h.state().compare.as_ref().is_some_and(|compare| compare.side_by_side);
    assert!(side_by_side, "the comparison opens side by side");
    h.run_steps(MENU_STEPS);
    // Both halves are drawn in Margin's page area: v1 on the left, v2 on the right, each in half
    // of it.
    let half = |name: &str| {
        let app = h.state();
        let index = app.views.iter().position(|view| app.session.get(view.id).is_some_and(|doc| doc.name == name));
        app.views[index.unwrap_or_else(|| panic!("{name} is open"))].viewport_rect()
    };
    let (old, new) = (half("v1.pdf"), half("v2.pdf"));
    assert!(old.right() <= new.left() + 1.0, "old {old:?} left of new {new:?}");
    assert!(new.width() < WINDOW.x / 2.0 && old.width() < WINDOW.x / 2.0, "each takes half: old {old:?}, new {new:?}");
}
