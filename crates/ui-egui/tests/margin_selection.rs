//! Margin's selection bar in the real shell (egui_kittest): beside a fresh text selection it
//! copies the text or its PDF++ callout without writing a mark, writes one mark per colour
//! choice and restyles it after, and closes on Esc or a press outside it (R8 QoL 8).

use std::path::PathBuf;

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

/// The callout of "quick brown", selected but not marked: the first colour's name.
const CALLOUT: &str = "> [!PDF|yellow] [[papers/text.pdf#page=1|text, p.1]]\n> > quick brown\n";

/// A temporary vault holding `papers/text.pdf`; removed on drop.
struct Vault(PathBuf);

impl Vault {
    fn new(name: &str, pdf: &[u8]) -> Self {
        let root = std::env::temp_dir().join(format!("margin-selection-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("vault/papers")).unwrap();
        std::fs::write(root.join("vault/papers/text.pdf"), pdf).unwrap();
        Self(root)
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A Margin window with the Select tool on the vault's PDF.
fn harness(vault: &Vault) -> Harness<'static, PdfCraftApp> {
    let root = vault.0.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        app.margin_keys.set_config_dir(root.join("config"));
        app.margin_keys.set_data_dir(root.join("data"));
        let pdf = root.join("vault/papers/text.pdf");
        let bytes = std::fs::read(&pdf).expect("the vault's PDF");
        app.open_bytes("text.pdf", Some(pdf.to_string_lossy().into_owned()), bytes).expect("opens");
        app.set_option("left", "closed").unwrap();
        app.set_option("notes.vault", &root.join("vault").to_string_lossy()).unwrap();
        app.notes.launch_obsidian = false;
        app.set_option("quick", "select").unwrap();
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

/// Drag-select "quick brown" (glyphs 4..=14 of page 1).
fn select(h: &mut Harness<'static, PdfCraftApp>) {
    let (from, to) = {
        let v = &h.state().views[0];
        (v.glyph_screen_pos(0, 4).expect("text layer"), v.glyph_screen_pos(0, 14).expect("glyph"))
    };
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
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("quick brown"));
}

fn marks(h: &Harness<'static, PdfCraftApp>) -> Vec<Option<[f32; 3]>> {
    let id = h.state().views[0].id;
    h.state().session.get(id).unwrap().info.annotations.iter().filter(|a| a.in_reply_to.is_none()).map(|a| a.color).collect()
}

fn bar_open(h: &Harness<'static, PdfCraftApp>) -> bool {
    h.query_by_label("Copy callout").is_some()
}

#[test]
fn selection_popover_setting_hides_only_the_bar_and_applies_live() {
    let vault = Vault::new("popover-setting", TEXT_FIXTURE);
    let config = vault.0.join("config");
    std::fs::create_dir_all(&config).unwrap();
    let settings = config.join(margin_config::SETTINGS_FILE);
    // The vault lives in settings.jsonc too; each rewrite keeps it, so notes (and the bar's Copy
    // callout) stay on.
    let vault_dir = vault.0.join("vault");
    let with_popover =
        |shown: bool| serde_json::json!({ "reader": { "selection_popover": shown }, "notes": { "vault": vault_dir.to_string_lossy() } }).to_string();
    std::fs::write(&settings, with_popover(false)).unwrap();
    let mut h = harness(&vault);
    select(&mut h);
    assert!(!bar_open(&h), "disabled selections do not open the bar");
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("quick brown"));
    assert!(marks(&h).is_empty());

    std::fs::write(&settings, with_popover(true)).unwrap();
    h.state_mut().margin_keys.set_config_dir(config.clone());
    h.run_steps(4);
    assert!(bar_open(&h), "reenabling exposes actions for the retained selection");

    std::fs::write(&settings, with_popover(false)).unwrap();
    h.state_mut().margin_keys.set_config_dir(config);
    h.run_steps(4);
    assert!(!bar_open(&h), "disabling hides an already open bar");
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("quick brown"));
    assert!(marks(&h).is_empty());
}

fn is_red(color: Option<[f32; 3]>) -> bool {
    color.is_some_and(|c| (c[0] - 0xea as f32 / 255.0).abs() < 0.01 && (c[1] - 0x52 as f32 / 255.0).abs() < 0.01)
}

fn is_yellow(color: Option<[f32; 3]>) -> bool {
    color.is_some_and(|c| (c[0] - 1.0).abs() < 0.01 && (c[1] - 0xd0 as f32 / 255.0).abs() < 0.01)
}

#[test]
fn copy_and_copy_callout_work_without_writing_a_mark() {
    let vault = Vault::new("copy", TEXT_FIXTURE);
    let mut h = harness(&vault);
    select(&mut h);
    assert!(bar_open(&h), "a finished selection shows the bar");
    h.get_by_label("Copy callout").click();
    let mut copied = Vec::new();
    for _ in 0..2 {
        h.run_steps(1);
        copied.extend(h.output().platform_output.commands.iter().filter_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.clone()),
            _ => None,
        }));
    }
    assert_eq!(copied, [CALLOUT]);
    assert!(marks(&h).is_empty(), "copying writes no mark");
    assert!(!bar_open(&h), "the bar closes after copying");
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("quick brown"), "the selection stays");
}

#[test]
fn colours_write_one_mark_and_restyle_it_one_undo_step_each() {
    let vault = Vault::new("colour", TEXT_FIXTURE);
    let mut h = harness(&vault);
    select(&mut h);
    h.get_by_label("Yellow").click();
    h.run_steps(3);
    assert_eq!(marks(&h).len(), 1);
    assert!(is_yellow(marks(&h)[0]));
    assert!(bar_open(&h), "a colour leaves the bar open");
    h.get_by_label("Red").click();
    h.run_steps(3);
    assert_eq!(marks(&h).len(), 1, "a second colour restyles the mark, not a second one");
    assert!(is_red(marks(&h)[0]));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
    assert_eq!(marks(&h).len(), 1);
    assert!(is_yellow(marks(&h)[0]), "one undo takes back the restyle");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
    assert!(marks(&h).is_empty(), "and the next one the mark");
    assert!(!bar_open(&h), "with its mark and selection gone the bar closes");
}

#[test]
fn number_keys_mark_the_selection_and_esc_closes_the_bar() {
    let vault = Vault::new("keys", TEXT_FIXTURE);
    let mut h = harness(&vault);
    select(&mut h);
    // `2` is Red in the built-in keymap's Selection context.
    h.key_press(egui::Key::Num2);
    h.run_steps(3);
    assert_eq!(marks(&h).len(), 1);
    assert!(is_red(marks(&h)[0]));
    h.key_press(egui::Key::Num1);
    h.run_steps(3);
    assert_eq!(marks(&h).len(), 1);
    assert!(is_yellow(marks(&h)[0]));
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(!bar_open(&h));
    assert_eq!(marks(&h).len(), 1, "Esc keeps the mark");
}

#[test]
fn a_press_outside_closes_the_bar_and_it_stays_closed() {
    let vault = Vault::new("outside", TEXT_FIXTURE);
    let mut h = harness(&vault);
    select(&mut h);
    assert!(bar_open(&h));
    // The header: outside the page, so the selection stays, and the bar doesn't come back for it.
    let header = Pos2::new(1000.0, 20.0);
    h.hover_at(header);
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: header, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.run_steps(1);
    }
    h.run_steps(3);
    assert!(!bar_open(&h));
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("quick brown"));
    // A new selection brings it back.
    select(&mut h);
    assert!(bar_open(&h));
}

/// A 400×300 two-column page drawn row by row across the columns, with a heading over both
/// columns and a figure caption over both halfway down.
fn columns_pdf() -> Vec<u8> {
    let mut content = String::from("BT /F1 12 Tf 20 270 Td (A heading that spans both columns of text) Tj ET\n");
    let rows = [
        (255, "Left one alpha", "Right one delta"),
        (242, "Left two beta", "Right two epsilon"),
        (229, "Figure 1: a caption that runs across the full width of both text columns", ""),
        (216, "Left three gamma", "Right three zeta"),
        (203, "Left four", "Right four"),
    ];
    for (y, left, right) in rows {
        content.push_str(&format!("BT /F1 10 Tf 20 {y} Td ({left}) Tj ET\n"));
        if !right.is_empty() {
            content.push_str(&format!("BT /F1 10 Tf 210 {y} Td ({right}) Tj ET\n"));
        }
    }
    format!(
        "%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Contents 5 0 R /Resources << /Font << /F1 7 0 R >> >> >> endobj
5 0 obj << /Length {} >> stream
{content}endstream endobj
7 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
trailer << /Root 1 0 R >>
%%EOF",
        content.len()
    )
    .into_bytes()
}

/// The glyph range of `needle` on page 1 (through the document's own search).
fn glyphs_of(h: &mut Harness<'static, PdfCraftApp>, needle: &str) -> std::ops::Range<usize> {
    h.state_mut().set_option("find", needle).unwrap();
    settle(h);
    let view = &mut h.state_mut().views[0];
    let range = view.find.as_ref().and_then(|f| f.matches.first()).map(|(_, r)| r.clone()).expect("a match");
    view.find = None;
    range
}

/// Drag from glyph `from` to glyph `to` of page 1 and return the selected text.
fn drag_glyphs(h: &mut Harness<'static, PdfCraftApp>, from: usize, to: usize) -> String {
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    let (a, b) = {
        let v = &h.state().views[0];
        (v.glyph_screen_pos(0, from).expect("glyph"), v.glyph_screen_pos(0, to).expect("glyph"))
    };
    h.hover_at(a);
    h.run_steps(1);
    h.drag_at(a);
    h.run_steps(1);
    for k in 1..=4 {
        h.hover_at(a + (b - a) * (k as f32 / 4.0));
        h.run_steps(1);
    }
    h.drop_at(b);
    h.run_steps(3);
    h.state().views[0].selected_text().unwrap_or_default()
}

/// Across the gutter, forwards and backwards, then rotated and trimmed: the selection reads the
/// left column's tail then the right column's head; copy, callout and the mark quote the same.
#[test]
fn two_column_selection_follows_reading_order_and_copy_callout_and_mark_agree() {
    let vault = Vault::new("columns", &columns_pdf());
    let mut h = harness(&vault);
    let from = glyphs_of(&mut h, "Left two beta").start;
    let to = glyphs_of(&mut h, "Right one delta").end - 1;
    let expected = "Left two beta\nRight one delta";
    assert_eq!(drag_glyphs(&mut h, from, to), expected, "forward drag");
    assert_eq!(drag_glyphs(&mut h, to, from), expected, "reverse drag");
    // The caption over both columns is read after the right column's top half.
    let caption = glyphs_of(&mut h, "Figure 1").start;
    assert!(caption > glyphs_of(&mut h, "Right two epsilon").start && caption < glyphs_of(&mut h, "Left three").start);
    // Rotated a quarter turn, then auto-trimmed: the same glyphs give the same text.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::R);
    settle(&mut h);
    assert_eq!(drag_glyphs(&mut h, from, to), expected, "rotated");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::R);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::R);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::R);
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::T);
    settle(&mut h);
    assert_eq!(drag_glyphs(&mut h, to, from), expected, "trimmed");
    // Copy callout quotes it; the highlight the bar writes marks exactly it.
    let quote = "> > Left two beta Right one delta\n";
    let callout = pdfcraft_ui_egui::margin::obsidian::selection_callout(h.state(), 0, 0, expected, None).unwrap();
    assert!(callout.ends_with(quote), "{callout}");
    h.get_by_label("Yellow").click();
    h.run_steps(3);
    settle(&mut h);
    assert_eq!(pdfcraft_ui_egui::margin::obsidian::callout_for(h.state(), 0, 0, 0).unwrap(), callout);
}
