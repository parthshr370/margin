//! Margin's Obsidian notes in the real shell (egui_kittest): a highlight becomes a PDF++ callout,
//! the reading note is appended to once per highlight, and note paths can't leave the vault.

use std::path::{Path, PathBuf};

use egui::Pos2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::margin::obsidian::{self, Request};
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

/// The callout the first highlight ("quick brown") gets.
const CALLOUT: &str = "> [!PDF|yellow] [[papers/text.pdf#page=1|text, p.1]]\n> > quick brown\n";

/// A temporary vault holding `papers/text.pdf`; removed on drop.
struct Vault(PathBuf);

impl Vault {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("margin-obsidian-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("vault/papers")).unwrap();
        std::fs::write(root.join("vault/papers/text.pdf"), TEXT_FIXTURE).unwrap();
        Self(root)
    }
    fn dir(&self) -> PathBuf {
        self.0.join("vault")
    }
    fn note(&self) -> PathBuf {
        self.dir().join("reading/text.md")
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn harness(vault: &Path) -> Harness<'static, PdfCraftApp> {
    let vault = vault.to_path_buf();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        app.margin_keys.set_data_dir(std::env::temp_dir().join(format!("margin-obsidian-data-{}", std::process::id())));
        let pdf = vault.join("papers/text.pdf");
        app.open_bytes("text.pdf", Some(pdf.to_string_lossy().into_owned()), TEXT_FIXTURE.to_vec()).expect("opens");
        app.set_option("left", "closed").unwrap();
        app.set_option("notes.vault", &vault.to_string_lossy()).unwrap();
        app.notes.launch_obsidian = false;
        app.set_option("quick", "highlight").unwrap();
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

/// Highlight glyphs `first..=last` of page 1.
fn highlight(h: &mut Harness<'static, PdfCraftApp>, first: usize, last: usize) {
    let (a, b) = {
        let v = &h.state().views[0];
        (v.glyph_screen_pos(0, first).expect("text layer"), v.glyph_screen_pos(0, last).expect("glyph"))
    };
    drag(h, a, b);
}

fn ask(h: &mut Harness<'static, PdfCraftApp>, request: Request) {
    h.state_mut().views[0].comments.notes.request = Some(request);
    h.run_steps(2);
}

fn toast(h: &Harness<'static, PdfCraftApp>) -> String {
    h.state().toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
}

#[test]
fn a_highlight_copies_as_a_pdf_plus_callout() {
    let vault = Vault::new("copy");
    let mut h = harness(&vault.dir());
    highlight(&mut h, 4, 14);
    assert_eq!(obsidian::callout_for(h.state(), 0, 0, 0).unwrap(), CALLOUT);
    h.state_mut().views[0].comments.notes.request = Some(Request::CopyCallout(0, 0));
    let mut copied = Vec::new();
    for _ in 0..2 {
        h.run_steps(1);
        copied.extend(h.output().platform_output.commands.iter().filter_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.clone()),
            _ => None,
        }));
    }
    assert_eq!(copied, [CALLOUT]);
    assert_eq!(toast(&h), "Copied callout");
}

#[test]
fn writing_to_the_note_twice_adds_one_entry() {
    let vault = Vault::new("write");
    let mut h = harness(&vault.dir());
    highlight(&mut h, 4, 14);
    ask(&mut h, Request::Write);
    assert_eq!(toast(&h), "Wrote 1 callout to reading/text.md");
    ask(&mut h, Request::Write);
    assert_eq!(toast(&h), "reading/text.md already has every highlight");
    let note = std::fs::read_to_string(vault.note()).unwrap();
    assert_eq!(note.matches("<!-- margin:").count(), 1, "{note}");
    assert!(note.starts_with("---\nsource: \"[[papers/text.pdf]]\"\n---\n\n## Highlights\n\n<!-- margin:"), "{note}");
    assert!(note.ends_with(&format!(" -->\n{CALLOUT}")), "{note}");
}

/// A note path leaving the vault is refused; the failure stays on screen (R8 QoL 23) with Retry
/// until it is dismissed, and a routine message doesn't replace it.
#[test]
fn a_note_path_leaving_the_vault_is_refused_with_a_toast() {
    let vault = Vault::new("escape");
    let mut h = harness(&vault.dir());
    highlight(&mut h, 4, 14);
    assert!(h.state_mut().set_option("notes.note_template", "../escape.md").is_err());
    // A path that got in some other way (a hand-edited settings file) is refused at write time.
    h.state_mut().notes.note_template = "reading/../../escape.md".into();
    ask(&mut h, Request::Write);
    let held = h.state().margin_chrome.notices.held().unwrap_or_default().to_owned();
    assert!(held.contains("must stay inside the vault"), "{held}");
    assert!(!vault.0.join("escape.md").exists() && !vault.dir().join("escape.md").exists());
    h.state_mut().notify("Copied");
    h.run_steps(20);
    assert_eq!(h.state().margin_chrome.notices.held(), Some(held.as_str()), "kept past routine messages");
    h.get_by_label("Retry");
    h.get_by_label("Dismiss").click();
    h.run_steps(2);
    assert!(h.state().margin_chrome.notices.held().is_none());
}

#[test]
fn open_in_obsidian_writes_the_highlight_and_opens_the_note() {
    let vault = Vault::new("open");
    let mut h = harness(&vault.dir());
    highlight(&mut h, 4, 14);
    ask(&mut h, Request::Open(Some((0, 0))));
    assert_eq!(h.state().last_opened_url.as_deref(), Some("obsidian://open?vault=vault&file=reading%2Ftext.md"));
    assert!(std::fs::read_to_string(vault.note()).unwrap().contains(CALLOUT));
}

#[test]
fn autoadd_writes_each_new_highlight_with_its_note() {
    let vault = Vault::new("autoadd");
    let mut h = harness(&vault.dir());
    h.state_mut().set_option("notes.autoadd", "on").unwrap();
    highlight(&mut h, 4, 14);
    // The new highlight's note is open for typing: nothing is written until it is saved.
    assert_eq!(h.state().views[0].comments.editing, Some((0, 0, String::new())));
    assert!(!vault.note().exists());
    let view = &mut h.state_mut().views[0];
    view.comments.editing = None;
    view.pending_edit = Some(pdfcraft_engine::Edit::SetAnnotationContents { page: 0, index: 0, text: "speed matters".into() });
    h.run_steps(3);
    let note = std::fs::read_to_string(vault.note()).unwrap();
    assert!(note.contains(&format!("{CALLOUT}>\n> speed matters\n")), "{note}");
    assert_eq!(toast(&h), "Added to reading/text.md");
    // The flag lives in the document's sidecar, not in the app's saved state.
    assert!(!h.state().persist().contains("\"autoadd\""));
    pdfcraft_ui_egui::margin::reading::flush(h.state_mut());
    let sidecar = std::fs::read_to_string(vault.dir().join("papers/text.pdf.margin.json")).unwrap();
    let sidecar: serde_json::Value = serde_json::from_str(&sidecar).unwrap();
    assert_eq!(sidecar["obsidian"]["auto_add"], true);
}

#[test]
fn pdfcraft_has_no_notes() {
    let mut app = PdfCraftApp::new();
    assert!(app.set_option("notes.vault", "/tmp").is_err());
    assert!(!app.persist().contains("\"notes\""));
}
