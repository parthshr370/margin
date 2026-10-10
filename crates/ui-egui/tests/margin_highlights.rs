//! Margin's Highlights tab in the real shell (egui_kittest): cards quote the page text they mark,
//! and their actions (delete, colour) go through the undo stack and the keymap.

use egui::Pos2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};

/// One 300×200 page of Helvetica text.
const TEXT_FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 5 0 R /Resources << /Font << /F1 7 0 R >> >> >> endobj
5 0 obj << /Length 55 >> stream
BT /F1 14 Tf 20 150 Td (The quick brown fox) Tj ET
endstream endobj
7 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
trailer << /Root 1 0 R >>
%%EOF";

/// A Margin window on the fixture with `tool` picked, its settings, keymap and databases in a
/// temporary folder.
fn harness(tool: &str) -> Harness<'static, PdfCraftApp> {
    let tool = tool.to_owned();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        let dir = std::env::temp_dir().join(format!("margin-highlights-{}-{tool}", std::process::id()));
        app.margin_keys.set_config_dir(dir.clone());
        app.margin_keys.set_data_dir(dir.join("data"));
        app.open_bytes("text.pdf", None, TEXT_FIXTURE.to_vec()).expect("opens");
        app.set_option("quick", &tool).unwrap();
        app
    });
    settle(&mut h);
    h
}

fn settle(h: &mut Harness<'static, PdfCraftApp>) {
    h.run_steps(4);
    for _ in 0..200 {
        h.run_steps(2);
        if !h.state().render_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn drag(h: &mut Harness<'static, PdfCraftApp>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.run_steps(1);
    h.drag_at(from);
    h.run_steps(1);
    for k in 1..=4 {
        h.hover_at(from + (to - from) * (k as f32 / 4.0));
        h.run_steps(1);
    }
    h.drop_at(to);
    h.run_steps(3);
}

/// Mark glyphs `first..=last` of page 1 with the picked tool, close the note that opens for
/// typing and show the Highlights tab.
fn mark(h: &mut Harness<'static, PdfCraftApp>, first: usize, last: usize) {
    let (a, b) = {
        let v = &h.state().views[0];
        (v.glyph_screen_pos(0, first).expect("text layer"), v.glyph_screen_pos(0, last).expect("glyph"))
    };
    drag(h, a, b);
    h.state_mut().views[0].comments.editing = None;
    h.get_all_by_label("Highlights").next().expect("tab").click();
    settle(h);
}

fn markup_count(h: &Harness<'static, PdfCraftApp>) -> usize {
    let id = h.state().views[0].id;
    h.state().session.get(id).unwrap().info.annotations.iter().filter(|a| a.in_reply_to.is_none()).count()
}

#[test]
fn an_underline_card_quotes_the_text_it_marks() {
    let mut h = harness("underline");
    mark(&mut h, 4, 14);
    assert_eq!(markup_count(&h), 1);
    // The card reads "<colour> <kind>, p. <page>: <quote>", the quote painted under its header,
    // not the word "Underline".
    h.get_by_label_contains("underline, p. 1: quick brown");
}

/// Point at the only card, so its hover actions show.
fn hover_card(h: &mut Harness<'static, PdfCraftApp>) {
    h.get_by_label_contains("highlight, p. 1: quick brown").hover();
    h.run_steps(3);
}

#[test]
fn the_delete_button_removes_the_highlight_and_undo_restores_it() {
    let mut h = harness("highlight");
    mark(&mut h, 4, 14);
    assert_eq!(markup_count(&h), 1);
    hover_card(&mut h);
    h.get_by_label("Delete highlight").click();
    h.run_steps(3);
    assert_eq!(markup_count(&h), 0);
    h.get_by_label("No highlights yet.");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
    assert_eq!(markup_count(&h), 1);
    h.get_by_label_contains("highlight, p. 1: quick brown");
}

#[test]
fn changing_the_colour_updates_the_swatch() {
    let mut h = harness("highlight");
    mark(&mut h, 4, 14);
    h.get_by_label("Colour: Yellow");
    hover_card(&mut h);
    h.get_by_label("Change colour").click();
    h.run_steps(3);
    h.get_by_label("Red").click();
    h.run_steps(3);
    let id = h.state().views[0].id;
    let color = h.state().session.get(id).unwrap().info.annotations[0].color.expect("a colour");
    assert!((color[0] - 0xea as f32 / 255.0).abs() < 0.01 && (color[1] - 0x52 as f32 / 255.0).abs() < 0.01, "{color:?}");
    h.get_by_label("Colour: Red");
    assert!(h.query_by_label("Colour: Yellow").is_none());
}

#[test]
fn delete_and_number_keys_act_on_the_selected_highlight() {
    let mut h = harness("highlight");
    mark(&mut h, 4, 14);
    h.get_by_label_contains("highlight, p. 1: quick brown").click();
    h.run_steps(3);
    assert_eq!(h.state().views[0].comments.selected, Some((0, 0)));
    // `4` is Important in the built-in keymap's Annotation context.
    h.key_press(egui::Key::Num4);
    h.run_steps(3);
    h.get_by_label("Colour: Important");
    h.key_press(egui::Key::Backspace);
    h.run_steps(3);
    assert_eq!(markup_count(&h), 0);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
    assert_eq!(markup_count(&h), 1);
}
