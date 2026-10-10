//! Margin's everyday marking in the real shell (egui_kittest): the rail's Highlight tool (H)
//! highlights the text a drag covers wherever the drag starts, in the rail's colour, at once and
//! undoably; the selection bar's colours, Underline and the 1-4 keys mark a pointer selection;
//! the rail's popovers, the selection bar and a highlight's note never swallow the next drag or
//! key; a highlight turns into an underline and back from the bar and from its card.

use egui::{Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::comments::CommentTool;
use pdfcraft_ui_egui::{Flavor, PdfCraftApp, QuickTool};

/// Three lines of Helvetica 14 on a 300×200 page, 20 points apart.
const LINES: [&str; 3] = ["The quick brown fox", "jumps over the lazy", "dog by the river"];

fn text_pdf() -> Vec<u8> {
    let content = format!("BT /F1 14 Tf 20 150 Td ({}) Tj 0 -20 Td ({}) Tj 0 -20 Td ({}) Tj ET\n", LINES[0], LINES[1], LINES[2]);
    pdf("/Resources << /Font << /F1 7 0 R >> >>", &content)
}

/// A page with a filled square and no text at all (a scan, as far as the text layer knows).
fn textless_pdf() -> Vec<u8> {
    pdf("", "0.2 0.4 0.8 rg 60 60 120 80 re f\n")
}

fn pdf(resources: &str, content: &str) -> Vec<u8> {
    format!(
        "%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 5 0 R {resources} >> endobj
5 0 obj << /Length {} >> stream
{content}endstream endobj
7 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
trailer << /Root 1 0 R >>
%%EOF",
        content.len()
    )
    .into_bytes()
}

/// A Margin window on `pdf` as it starts: the rail open with the Select tool, the sidebar on
/// Contents; settings, keymap and databases in a temporary folder.
fn harness(name: &str, pdf: Vec<u8>) -> Harness<'static, PdfCraftApp> {
    let dir = std::env::temp_dir().join(format!("margin-marking-{name}-{}", std::process::id()));
    let mut h = Harness::builder().with_size(vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        app.margin_keys.set_config_dir(dir.join("config"));
        app.margin_keys.set_data_dir(dir.join("data"));
        app.open_bytes("text.pdf", None, pdf).expect("opens");
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

/// Press at `from`, move to `to` over a few frames, release.
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

/// The first glyph of `needle` on page 1 and its last.
fn glyphs_of(h: &mut Harness<'static, PdfCraftApp>, needle: &str) -> (usize, usize) {
    h.state_mut().set_option("find", needle).unwrap();
    settle(h);
    let view = &mut h.state_mut().views[0];
    let range = view.find.as_ref().and_then(|f| f.matches.first()).map(|(_, r)| r.clone()).expect("a match");
    view.find = None;
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    (range.start, range.end - 1)
}

fn glyph(h: &Harness<'static, PdfCraftApp>, index: usize) -> Pos2 {
    h.state().views[0].glyph_screen_pos(0, index).expect("text layer")
}

/// The first glyph of `from` and the last of `to`. Looked up before a tool is picked: closing
/// the search with Esc also puts the Select tool back.
fn span(h: &mut Harness<'static, PdfCraftApp>, from: &str, to: &str) -> (usize, usize) {
    (glyphs_of(h, from).0, glyphs_of(h, to).1)
}

/// Drag from glyph `first` to glyph `last`.
fn drag_glyphs(h: &mut Harness<'static, PdfCraftApp>, (first, last): (usize, usize)) {
    let (a, b) = (glyph(h, first), glyph(h, last));
    drag(h, a, b);
}

/// The document's comments: subtype, colour and quadrilateral count.
fn marks(h: &Harness<'static, PdfCraftApp>) -> Vec<(String, Option<[f32; 3]>, usize)> {
    let id = h.state().views[0].id;
    let doc = h.state().session.get(id).unwrap();
    doc.info.annotations.iter().filter(|a| a.in_reply_to.is_none()).map(|a| (a.subtype.clone(), a.color, a.quads.len())).collect()
}

fn is_red(color: Option<[f32; 3]>) -> bool {
    color.is_some_and(|c| (c[0] - 0xea as f32 / 255.0).abs() < 0.01 && (c[1] - 0x52 as f32 / 255.0).abs() < 0.01)
}

fn is_yellow(color: Option<[f32; 3]>) -> bool {
    color.is_some_and(|c| (c[0] - 1.0).abs() < 0.01 && (c[1] - 0xd0 as f32 / 255.0).abs() < 0.01)
}

fn undo(h: &mut Harness<'static, PdfCraftApp>) {
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
}

fn show_highlights_tab(h: &mut Harness<'static, PdfCraftApp>) {
    h.get_all_by_label("Highlights").next().expect("tab").click();
    settle(h);
}

/// The owner's report: a drag with the Highlight tool that starts just off the glyphs (above a
/// line, in a gap) used to draw a red box and leave a rectangle over the lines. Now it
/// highlights the text, one quadrilateral per line, in the colour the rail's popover picked
/// (which stays open and doesn't take the drag), with no note editor in the way; the card shows
/// it and one Undo takes it back.
#[test]
fn the_highlight_tool_marks_the_text_wherever_the_drag_starts() {
    let mut h = harness("tool", text_pdf());
    let (quick, lazy) = span(&mut h, "quick", "lazy");
    let jumps = glyphs_of(&mut h, "jumps").0;
    let (first, second) = (glyph(&h, quick), glyph(&h, lazy));
    let line_pitch = glyph(&h, jumps).y - first.y;
    h.key_press(egui::Key::H);
    h.run_steps(3);
    assert_eq!(h.state().quick_tool, QuickTool::Comment(CommentTool::Highlight), "H picks the rail's Highlight");
    h.get_by_label("Colour").click();
    h.run_steps(3);
    h.get_by_label("Red").click();
    h.run_steps(3);
    assert!(h.query_by_label("Yellow").is_some(), "the colour popover stays open");
    // Above the first line, outside every glyph's box: the press that used to start a box.
    drag(&mut h, first - vec2(0.0, 0.45 * line_pitch), second);
    let made = marks(&h);
    assert_eq!(made.len(), 1, "one mark: {made:?}");
    assert_eq!(made[0].0, "Highlight");
    assert_eq!(made[0].2, 2, "one quadrilateral per line, not a box: {made:?}");
    assert!(is_red(made[0].1), "in the rail's colour: {made:?}");
    assert_eq!(h.state().views[0].comments.editing, None, "no note editor opens");
    show_highlights_tab(&mut h);
    h.get_by_label_contains("Red highlight, p. 1: quick brown fox jumps over the lazy");
    undo(&mut h);
    assert!(marks(&h).is_empty(), "one Undo takes it back");
}

/// A page with no text keeps PdfCraft's area highlight: the box the drag draws.
#[test]
fn a_page_without_text_takes_an_area_highlight() {
    let mut h = harness("textless", textless_pdf());
    h.key_press(egui::Key::H);
    h.run_steps(3);
    let page = h.state().views[0].page_screen_rect(0).expect("page drawn");
    drag(&mut h, page.lerp_inside(vec2(0.2, 0.3)), page.lerp_inside(vec2(0.6, 0.6)));
    let made = marks(&h);
    assert_eq!(made.len(), 1, "{made:?}");
    assert_eq!((made[0].0.as_str(), made[0].2), ("Highlight", 1), "a box: {made:?}");
}

/// After a highlight, with the Highlights tab open, the reader's keys still reach the reader: V
/// picks Select and 2 colours the next pointer selection red.
#[test]
fn keys_still_work_after_a_highlight_with_the_highlights_tab_open() {
    let mut h = harness("keys", text_pdf());
    let (quick_brown, dog_river) = (span(&mut h, "quick", "brown"), span(&mut h, "dog", "river"));
    show_highlights_tab(&mut h);
    h.key_press(egui::Key::H);
    h.run_steps(3);
    drag_glyphs(&mut h, quick_brown);
    assert_eq!(marks(&h).len(), 1);
    h.key_press(egui::Key::V);
    h.run_steps(3);
    assert_eq!(h.state().quick_tool, QuickTool::Select, "V reached the reader");
    drag_glyphs(&mut h, dog_river);
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("dog by the river"));
    h.key_press(egui::Key::Num2);
    h.run_steps(3);
    let made = marks(&h);
    assert_eq!(made.len(), 2, "{made:?}");
    assert!(is_red(made[1].1), "2 marked the selection red: {made:?}");
}

/// The rail's Underline tool underlines the text a drag covers.
#[test]
fn the_underline_tool_marks_the_text() {
    let mut h = harness("underline", text_pdf());
    let jumps_lazy = span(&mut h, "jumps", "lazy");
    h.get_by_label("Underline text").click();
    h.run_steps(3);
    drag_glyphs(&mut h, jumps_lazy);
    let made = marks(&h);
    assert_eq!(made.len(), 1, "{made:?}");
    assert_eq!((made[0].0.as_str(), made[0].2), ("Underline", 1));
}

/// With the selection bar open, a new drag on the page selects the new text (the bar doesn't
/// take it) and the bar follows; its colour then marks that text.
#[test]
fn the_selection_bar_never_takes_a_drag_on_the_page() {
    let mut h = harness("bar-drag", text_pdf());
    let (quick_brown, dog_river) = (span(&mut h, "quick", "brown"), span(&mut h, "dog", "river"));
    drag_glyphs(&mut h, quick_brown);
    assert!(h.query_by_label("Comment").is_some(), "the bar is open");
    drag_glyphs(&mut h, dog_river);
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("dog by the river"));
    assert!(h.query_by_label("Comment").is_some(), "the bar follows the new selection");
    h.get_by_label("Yellow").click();
    h.run_steps(3);
    let made = marks(&h);
    assert_eq!(made.len(), 1, "{made:?}");
    assert!(is_yellow(made[0].1));
}

/// R8 QoL 8's gap: the bar's Underline turns its highlight into an underline and back, one
/// undo step each, keeping the colour; the card's menu does the same.
#[test]
fn a_highlight_becomes_an_underline_and_back() {
    let mut h = harness("kind", text_pdf());
    let quick_brown = span(&mut h, "quick", "brown");
    drag_glyphs(&mut h, quick_brown);
    h.get_by_label("Red").click();
    h.run_steps(3);
    h.get_by_label("Underline").click();
    h.run_steps(3);
    let made = marks(&h);
    assert_eq!(made.len(), 1, "changed, not added: {made:?}");
    assert_eq!(made[0].0, "Underline");
    assert!(is_red(made[0].1), "the colour stays: {made:?}");
    h.get_by_label("Underline: on, click for a highlight").click();
    h.run_steps(3);
    assert_eq!(marks(&h).iter().map(|m| m.0.as_str()).collect::<Vec<_>>(), ["Highlight"]);
    undo(&mut h);
    assert_eq!(marks(&h).iter().map(|m| m.0.as_str()).collect::<Vec<_>>(), ["Underline"], "one undo step per change");
    undo(&mut h);
    assert_eq!(marks(&h).iter().map(|m| m.0.as_str()).collect::<Vec<_>>(), ["Highlight"]);
    // From the card: right-click, Change to underline.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    show_highlights_tab(&mut h);
    h.get_by_label_contains("Red highlight, p. 1: quick brown").click_secondary();
    h.run_steps(3);
    h.get_by_label("Change to underline").click();
    h.run_steps(3);
    let made = marks(&h);
    assert_eq!((made.len(), made[0].0.as_str()), (1, "Underline"), "{made:?}");
    h.get_by_label_contains("Red underline, p. 1: quick brown");
    assert_eq!(h.state().views[0].comments.selected.map(|(page, _)| page), Some(0), "the new mark is selected");
}
