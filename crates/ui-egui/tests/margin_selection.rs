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
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("margin-selection-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("vault/papers")).unwrap();
        std::fs::write(root.join("vault/papers/text.pdf"), TEXT_FIXTURE).unwrap();
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
        app.open_bytes("text.pdf", Some(pdf.to_string_lossy().into_owned()), TEXT_FIXTURE.to_vec()).expect("opens");
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

fn is_red(color: Option<[f32; 3]>) -> bool {
    color.is_some_and(|c| (c[0] - 0xea as f32 / 255.0).abs() < 0.01 && (c[1] - 0x52 as f32 / 255.0).abs() < 0.01)
}

fn is_yellow(color: Option<[f32; 3]>) -> bool {
    color.is_some_and(|c| (c[0] - 1.0).abs() < 0.01 && (c[1] - 0xd0 as f32 / 255.0).abs() < 0.01)
}

#[test]
fn copy_and_copy_callout_work_without_writing_a_mark() {
    let vault = Vault::new("copy");
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
    let vault = Vault::new("colour");
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
    let vault = Vault::new("keys");
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
    let vault = Vault::new("outside");
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
