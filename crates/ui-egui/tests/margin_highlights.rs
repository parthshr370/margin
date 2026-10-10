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

/// R8 QoL 23: deleting a mark says so with Undo, on a card inside the reading area above the
/// footer; Undo restores the mark, and a routine message expires and lets the window go idle.
#[test]
fn a_deletion_offers_undo_in_a_toast_above_the_footer() {
    let mut h = harness("highlight");
    mark(&mut h, 4, 14);
    hover_card(&mut h);
    h.get_by_label("Delete highlight").click();
    h.run_steps(3);
    assert_eq!(markup_count(&h), 0);
    assert_eq!(h.state().toast.as_ref().map(|t| t.0.as_str()), Some("Highlight deleted"));
    assert!(h.state().margin_chrome.notices.offers_undo());
    // The rail's Undo sits halfway down the page; the toast's is the lowest.
    let page = h.state().views[0].viewport_rect();
    let undo = h.get_all_by_label("Undo").max_by(|a, b| a.rect().bottom().total_cmp(&b.rect().bottom())).expect("the toast's Undo");
    assert!(page.contains_rect(undo.rect()), "the toast ({:?}) is inside the reading area ({page:?}), above the footer", undo.rect());
    undo.click();
    h.run_steps(3);
    assert_eq!(markup_count(&h), 1, "Undo restores the mark");
    let after = h.state().toast.as_ref().map(|t| t.0.clone());
    assert!(after.as_deref() != Some("Highlight deleted") && !h.state().margin_chrome.notices.offers_undo(), "{after:?}");
    h.state_mut().notify("Copied");
    h.run_steps(16);
    assert!(h.state().toast.is_none(), "a routine message expires");
    h.try_run().expect("idle");
    assert_eq!(h.try_run().expect("idle"), 1, "nothing repaints once it has gone");
}

/// R8 QoL 25: a card's date is short ("10 Oct" this year), 8 points after the page label, never
/// a cut timestamp, at the sidebar's narrowest, default and widest; hovering the card moves
/// neither.
#[test]
fn highlight_dates_are_short_and_never_squeezed() {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mut h = harness("highlight");
    mark(&mut h, 4, 14);
    let (year, month, day) = h.state().session.today();
    let today = format!("{day} {}", MONTHS[month as usize - 1]);
    for width in [240.0_f32, 296.0, 440.0] {
        // The width the reader dragged the sidebar to, as egui keeps it.
        let outer_rect = egui::Rect::from_min_size(egui::pos2(0.0, 44.0), egui::vec2(width, 800.0));
        h.ctx.data_mut(|d| d.insert_persisted(egui::Id::new("margin_sidebar"), egui::PanelState { outer_rect }));
        h.run_steps(3);
        let page = h.get_by_label("p. 1").rect();
        let actions_end = h.get_by_label("Delete highlight").rect().right();
        assert!(actions_end <= width && actions_end > width - 40.0, "the sidebar is {width} wide: actions end at {actions_end}");
        assert_eq!(h.query_all_by_label_contains(&format!("{year}-")).count(), 0, "no raw timestamp at {width}");
        // At the narrowest the date gives way to the reserved actions (the page label's tooltip
        // still has it); wider, it sits 8 points after the page label.
        let date = h.query_by_label(&today).map(|node| node.rect());
        assert!(date.is_some() || width <= 240.0, "the date shows at {width}");
        if let Some(date) = date {
            assert!(date.left() >= page.right() + 7.5, "the date sits 8 points after the page label at {width}: {page:?} {date:?}");
            assert!(date.right() <= actions_end - 90.0, "and clear of the actions at {width}: {date:?}");
        }
        hover_card(&mut h);
        let hovered = (h.get_by_label("p. 1").rect(), h.query_by_label(&today).map(|node| node.rect()));
        assert_eq!(hovered, (page, date), "hover moves nothing at {width}");
        h.hover_at(egui::pos2(width + 400.0, 450.0));
        h.run_steps(3);
    }
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
