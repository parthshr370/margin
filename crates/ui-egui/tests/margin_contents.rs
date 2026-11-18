//! A PDF's bookmark title styling reaches the galleys painted in Margin's Contents sidebar.

use egui::{Color32, TextFormat};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::theme::{self, Tokens};
use pdfcraft_ui_egui::{Flavor, PdfCraftApp, margin};

fn fixture() -> Vec<u8> {
    // ISO 32000-2 §12.3.3: /F bits 1 and 2 are italic and bold; /C contains RGB components.
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 500 700] >>",
        "<< /Type /Page /Parent 2 0 R >>",
        "<< /Type /Outlines /First 5 0 R /Last 8 0 R /Count 4 >>",
        "<< /Title (Plain bookmark) /Parent 4 0 R /Next 6 0 R /Dest [3 0 R /Fit] >>",
        "<< /Title (Bold bookmark) /Parent 4 0 R /Prev 5 0 R /Next 7 0 R /Dest [3 0 R /Fit] /F 2 >>",
        "<< /Title (Italic bookmark) /Parent 4 0 R /Prev 6 0 R /Next 8 0 R /Dest [3 0 R /Fit] /F 1 >>",
        "<< /Title (Red bold italic bookmark) /Parent 4 0 R /Prev 7 0 R /Dest [3 0 R /Fit] /F 3 /C [1 0 0] >>",
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    bytes
}

fn row_format<'a>(h: &'a Harness<'_, PdfCraftApp>, title: &str) -> &'a TextFormat {
    h.get_by_label(title);
    let sidebar_right = h.state().views[0].viewport_rect().left();
    h.output()
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) if text.pos.x < sidebar_right && text.galley.text() == title => {
                text.galley.job.sections.first().map(|section| &section.format)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no painted title for {title}"))
}

#[test]
fn contents_paints_bookmark_weight_slant_and_color() {
    let dir = std::env::temp_dir().join(format!("margin-test-contents-{}", std::process::id()));
    let home = dir.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        app.margin_keys.set_config_dir(home.join("config"));
        app.margin_keys.set_data_dir(home.join("data"));
        app.open_bytes("styled.pdf", None, fixture()).unwrap();
        app
    });
    h.run_steps(4);
    assert!(margin::run(h.state_mut(), "view.sidebar.tab", &serde_json::json!({ "tab": "contents" })));
    h.run_steps(6);

    let tokens = Tokens::get(&h.ctx);
    let plain = row_format(&h, "Plain bookmark");
    let bold = row_format(&h, "Bold bookmark");
    let italic = row_format(&h, "Italic bookmark");
    let styled = row_format(&h, "Red bold italic bookmark");
    assert_eq!(plain.font_id, theme::regular(plain.font_id.size));
    assert_eq!(plain.color, tokens.text);
    assert!(!plain.italics);
    assert_eq!(bold.font_id, theme::semibold(plain.font_id.size));
    assert!(!bold.italics);
    assert_eq!(italic.font_id, plain.font_id);
    assert!(italic.italics);
    assert_eq!(styled.font_id, bold.font_id);
    assert!(styled.italics);
    assert!(styled.color.r() > styled.color.g() && styled.color.g() == styled.color.b(), "expected a red title: {:?}", styled.color);
    // All destinations start at the same place, so the last bookmark is the current section.
    let luminance = |color: Color32| {
        let linear = egui::Rgba::from(color);
        0.2126 * linear.r() + 0.7152 * linear.g() + 0.0722 * linear.b()
    };
    let selected = tokens.panel.blend(tokens.selected);
    assert!((luminance(styled.color) + 0.05) / (luminance(selected) + 0.05) >= 4.5);
    drop(h);
    std::fs::remove_dir_all(dir).unwrap();
}
