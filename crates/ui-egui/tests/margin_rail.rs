//! Margin's default annotate rail and shortcuts select the same tools.

use egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::comments::CommentTool;
use pdfcraft_ui_egui::{Flavor, PdfCraftApp, QuickTool};

#[test]
fn freehand_highlight_is_picked_from_the_rail_or_with_m() {
    let dir = std::env::temp_dir().join(format!("margin-test-rail-{}", std::process::id()));
    let home = dir.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        app.margin_keys.set_config_dir(home.join("config"));
        app.margin_keys.set_data_dir(home.join("data"));
        let bytes = app.session.create_blank(300.0, 400.0, 1).unwrap();
        app.open_bytes("rail.pdf", None, bytes.to_vec()).unwrap();
        app
    });
    h.run_steps(4);

    let freehand = QuickTool::Comment(CommentTool::FreehandHighlight);
    h.get_by_label("Freehand-Highlight").click();
    h.run_steps(3);
    assert_eq!(h.state().quick_tool, freehand);

    h.key_press(Key::V);
    h.run_steps(3);
    assert_eq!(h.state().quick_tool, QuickTool::Select);
    h.key_press(Key::M);
    h.run_steps(3);
    assert_eq!(h.state().quick_tool, freehand);
    drop(h);
    std::fs::remove_dir_all(dir).unwrap();
}
