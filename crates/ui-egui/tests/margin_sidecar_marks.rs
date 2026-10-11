//! Highlights and page notes kept in the sidecar (`margin::marks`) in the real shell: a read-only
//! PDF's highlights go to its sidecar, survive a restart and undo; "Save a copy with highlights"
//! writes them as real annotations; page notes are added, edited, deleted and undone; a sidecar
//! another writer changed is merged, and one from a newer Margin is shown but never written;
//! page bookmarks keep their exact spot, take a name and say so when the cap is reached.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;

use egui::Pos2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::margin::{self, marks, reading};
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};
use serde_json::{Value, json};

/// Each simulated step: big enough that a second passes in a handful of frames.
const STEP_SECS: f32 = 0.1;

/// Three 300×200 pages of Helvetica text (the same two lines on each).
fn text_page() -> Vec<u8> {
    let content = "BT /F1 14 Tf 20 150 Td (The quick brown fox) Tj 0 -20 Td (jumps over the lazy dog) Tj ET\n";
    let page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned();
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R 6 0 R 7 0 R] /Count 3 >>".to_owned(),
        page.clone(),
        format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        page.clone(),
        page,
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

/// A folder with the document, a config folder and a data folder; removed (writable again) on
/// drop.
struct Place(PathBuf);

impl Place {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("margin-marks-{name}-{}", std::process::id()));
        if root.exists() {
            std::fs::set_permissions(root.join("docs"), std::fs::Permissions::from_mode(0o755)).ok();
            std::fs::remove_dir_all(&root).ok();
        }
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs").join("text.pdf"), text_page()).unwrap();
        Self(root)
    }
    fn doc(&self) -> PathBuf {
        self.0.join("docs").join("text.pdf")
    }
    fn sidecar(&self) -> PathBuf {
        self.0.join("docs").join("text.pdf.margin.json")
    }
    fn read_only(&self) -> &Self {
        std::fs::set_permissions(self.doc(), std::fs::Permissions::from_mode(0o444)).unwrap();
        self
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        std::fs::set_permissions(self.0.join("docs"), std::fs::Permissions::from_mode(0o755)).ok();
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn app(place: &Place) -> PdfCraftApp {
    let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
    app.set_option("language", "en").unwrap();
    app.margin_keys.set_config_dir(place.0.join("config"));
    app.margin_keys.set_data_dir(place.0.join("data"));
    let doc = place.doc();
    let bytes = std::fs::read(&doc).unwrap();
    app.open_bytes("text.pdf", Some(doc.to_string_lossy().into_owned()), bytes).expect("opens");
    app
}

fn harness(place: &Place) -> Harness<'static, PdfCraftApp> {
    let app = app(place);
    let mut h = Harness::builder().with_size(egui::vec2(1300.0, 900.0)).with_step_dt(STEP_SECS).build_eframe(move |_cc| app);
    settle(&mut h);
    h
}

fn settle(h: &mut Harness<'static, PdfCraftApp>) {
    h.run_steps(4);
    for _ in 0..300 {
        h.run_steps(2);
        if !h.state().render_pending() {
            h.run_steps(3);
            if !h.state().render_pending() {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn wait(h: &mut Harness<'static, PdfCraftApp>, secs: f32) {
    h.run_steps((secs / STEP_SECS).ceil() as usize);
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

/// Highlight "quick brown" with the rail's Highlight tool.
fn highlight(h: &mut Harness<'static, PdfCraftApp>) {
    h.state_mut().set_option("quick", "highlight").unwrap();
    h.run_steps(2);
    let (a, b) = {
        let view = &h.state().views[0];
        (view.glyph_screen_pos(0, 4).expect("text layer"), view.glyph_screen_pos(0, 14).expect("glyph"))
    };
    drag(h, a, b);
}

fn command(h: &mut Harness<'static, PdfCraftApp>, id: &str) {
    assert!(margin::run(h.state_mut(), id, &Value::Null), "{id} runs");
    h.run_steps(3);
}

fn sidecar_json(place: &Place) -> Value {
    serde_json::from_slice(&std::fs::read(place.sidecar()).expect("sidecar written")).expect("sidecar is JSON")
}

fn sidecar_highlights(h: &Harness<'static, PdfCraftApp>) -> usize {
    marks::sidecar_highlights(&h.state().views[0]).len()
}

/// Type `text` into the focused field and press the button labelled `button`.
fn type_and_press(h: &mut Harness<'static, PdfCraftApp>, text: &str, button: &str) {
    h.event(egui::Event::Text(text.to_owned()));
    h.run_steps(2);
    h.get_by_label(button).click();
    h.run_steps(3);
}

#[test]
fn a_read_only_pdf_keeps_highlights_in_its_sidecar_across_a_restart() {
    let place = Place::new("read-only");
    place.read_only();
    let mut h = harness(&place);
    highlight(&mut h);
    let id = h.state().views[0].id;
    assert_eq!(sidecar_highlights(&h), 1, "kept beside the PDF");
    assert!(h.state().session.get(id).unwrap().info.annotations.is_empty(), "not in the document");
    assert!(!h.state().session.get(id).unwrap().dirty);
    wait(&mut h, 2.0);
    let saved = sidecar_json(&place);
    let kept = &saved["highlights"][0];
    assert_eq!(kept["pdf_annot"], Value::Null);
    assert_eq!(kept["style"], "highlight");
    assert_eq!(kept["text"], "quick brown");
    assert_eq!(kept["locator"]["page"], 0);
    assert_eq!(std::fs::read(place.doc()).unwrap(), text_page(), "the PDF is never written");

    // Undo removes it and leaves a tombstone; redo brings it back.
    assert!(h.state_mut().execute("edit.undo"));
    h.run_steps(2);
    assert_eq!(sidecar_highlights(&h), 0);
    wait(&mut h, 2.0);
    let undone = sidecar_json(&place);
    assert_eq!(undone["highlights"].as_array().map(Vec::len), Some(0));
    assert_eq!(undone["deleted"][0], kept["id"]);
    assert!(h.state_mut().execute("edit.redo"));
    h.run_steps(2);
    assert_eq!(sidecar_highlights(&h), 1);
    wait(&mut h, 2.0);
    drop(h);

    let mut h = harness(&place);
    assert_eq!(sidecar_highlights(&h), 1, "read back after a restart");
    assert!(margin::run(h.state_mut(), "view.sidebar.tab", &json!({ "tab": "highlights" })));
    settle(&mut h);
    assert!(h.query_by_label_contains("highlight, p. 1: quick brown").is_some(), "listed in the Highlights tab");
}

#[test]
fn save_a_copy_writes_real_annotations_and_leaves_the_original() {
    let place = Place::new("copy");
    place.read_only();
    let mut h = harness(&place);
    highlight(&mut h);
    let copy = place.0.join("copy.pdf");
    h.state_mut().save_override = Some(copy.to_string_lossy().into_owned());
    command(&mut h, marks::SAVE_COPY_ID);
    let bytes = std::fs::read(&copy).expect("the copy is written");
    let mut session = pdfcraft_engine::Session::new();
    let id = session.open("copy.pdf", None, Arc::new(bytes), None).expect("the copy opens");
    let doc = session.get(id).unwrap();
    let marked: Vec<&str> = doc.info.annotations.iter().map(|a| a.subtype.as_str()).collect();
    assert_eq!(marked, ["Highlight"]);
    assert_eq!(std::fs::read(place.doc()).unwrap(), text_page());
    assert_eq!(sidecar_highlights(&h), 1, "still kept beside the original");
}

#[test]
fn page_notes_are_added_edited_deleted_and_undone() {
    let place = Place::new("notes");
    let mut h = harness(&place);
    command(&mut h, marks::NOTE_ADD_ID);
    settle(&mut h);
    type_and_press(&mut h, "Check the proof", "Save");
    let notes = marks::page_notes(&h.state().views[0]);
    assert_eq!(notes.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(), ["Check the proof"]);
    assert!(h.query_by_label_contains("Page note, p. 1: Check the proof").is_some(), "a card in the Highlights tab");
    wait(&mut h, 2.0);
    assert_eq!(sidecar_json(&place)["notes"][0]["text"], "Check the proof", "kept for a writable PDF too");

    // The new note is selected: edit it (select all, then type over it).
    command(&mut h, marks::NOTE_EDIT_ID);
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(1);
    type_and_press(&mut h, "Check the proof twice", "Save");
    assert_eq!(marks::page_notes(&h.state().views[0]).first().map(|n| n.text.clone()).as_deref(), Some("Check the proof twice"));

    command(&mut h, marks::NOTE_DELETE_ID);
    assert!(marks::page_notes(&h.state().views[0]).is_empty());
    assert!(h.state_mut().execute("edit.undo"));
    h.run_steps(2);
    assert_eq!(marks::page_notes(&h.state().views[0]).len(), 1, "undo brings it back");
    wait(&mut h, 2.0);
    let saved = sidecar_json(&place);
    assert_eq!(saved["notes"][0]["text"], "Check the proof twice");
    assert_eq!(saved["deleted"].as_array().map(Vec::len), Some(0), "an undone deletion leaves no tombstone");
    assert_eq!(std::fs::read(place.doc()).unwrap(), text_page(), "notes never touch the PDF");
}

/// A highlight record as another writer would put it in the sidecar.
fn record(id: &str, page: u32, text: &str) -> Value {
    json!({
        "id": id, "locator": { "kind": "pdf", "page": page, "quads": [[20.0, 30.0, 120.0, 30.0, 20.0, 46.0, 120.0, 46.0]] },
        "text": text, "style": "highlight", "color": "yellow", "source": "margin", "created": "2026-10-10T10:00:00Z", "pdf_annot": null
    })
}

#[test]
fn a_sidecar_another_writer_changed_is_merged_not_overwritten() {
    let place = Place::new("merge");
    place.read_only();
    let mut h = harness(&place);
    highlight(&mut h);
    wait(&mut h, 2.0);
    let mut theirs = sidecar_json(&place);
    let revision = theirs["revision"].as_u64().unwrap();
    theirs["revision"] = json!(revision + 5);
    theirs["highlights"].as_array_mut().unwrap().push(record("THEIRS", 0, "jumps over"));
    std::fs::write(place.sidecar(), serde_json::to_vec(&theirs).unwrap()).unwrap();
    command(&mut h, marks::NOTE_ADD_ID);
    settle(&mut h);
    type_and_press(&mut h, "mine", "Save");
    wait(&mut h, 2.0);
    let merged = sidecar_json(&place);
    let ids: Vec<&str> = merged["highlights"].as_array().unwrap().iter().filter_map(|h| h["id"].as_str()).collect();
    assert!(ids.contains(&"THEIRS") && ids.len() == 2, "{ids:?}");
    assert_eq!(merged["notes"][0]["text"], "mine");
    assert!(merged["revision"].as_u64().unwrap() > revision + 5, "counts past the other writer");
    assert_eq!(sidecar_highlights(&h), 2, "the other writer's highlight shows here too");
}

#[test]
fn a_sidecar_from_a_newer_margin_shows_its_highlights_and_is_never_written() {
    let place = Place::new("newer");
    let newer =
        json!({ "format": "margin-sidecar", "version": 9, "revision": 3, "highlights": [record("NEW", 0, "quick")], "future": { "kept": true } });
    let text = serde_json::to_string(&newer).unwrap();
    std::fs::write(place.sidecar(), &text).unwrap();
    let mut h = harness(&place);
    assert_eq!(sidecar_highlights(&h), 1, "shown read-only");
    assert!(margin::run(h.state_mut(), marks::NOTE_ADD_ID, &Value::Null));
    h.run_steps(2);
    assert!(marks::page_notes(&h.state().views[0]).is_empty());
    wait(&mut h, 2.0);
    assert_eq!(std::fs::read_to_string(place.sidecar()).unwrap(), text, "left exactly as it was");
}

#[test]
fn bookmarks_keep_their_spot_take_a_name_and_say_when_full() {
    let place = Place::new("bookmarks");
    let mut h = harness(&place);
    let view = &mut h.state_mut().views[0];
    reading::go(view, reading::Spot { page: 1, y: 0.2, x: Some(0.5) });
    settle(&mut h);
    let view = &mut h.state_mut().views[0];
    assert_eq!(view.current, 1);
    assert_eq!(reading::toggle_bookmark(view), reading::BookmarkToggle::Added);
    let id = view.margin.bookmarks[0].id.clone();
    reading::rename_bookmark(view, &id, "  Proof sketch  ");
    wait(&mut h, 2.0);
    let saved = sidecar_json(&place);
    let at = saved["bookmarks"][0]["locator"]["at"].as_array().expect("the exact spot is kept").clone();
    assert!((at[1].as_f64().unwrap() - 0.2).abs() < 0.05, "{at:?}");
    assert_eq!(saved["bookmarks"][0]["name"], "Proof sketch");
    // Jumping back to it restores the spot, not the page's top.
    reading::go(&mut h.state_mut().views[0], reading::Spot { page: 0, y: 0.0, x: None });
    settle(&mut h);
    reading::go_to_bookmark(&mut h.state_mut().views[0], &id);
    settle(&mut h);
    let spot = reading::spot(&h.state().views[0]);
    assert!(spot.page == 1 && (spot.y - 0.2).abs() < 0.05, "{spot:?}");

    let view = &mut h.state_mut().views[0];
    reading::remove_bookmark(view, &id);
    let template = saved["bookmarks"][0].clone();
    view.margin.bookmarks = (0..reading::MAX_BOOKMARKS)
        .map(|i| {
            let mut bookmark: margin_config::Bookmark = serde_json::from_value(template.clone()).unwrap();
            bookmark.id = format!("b{i}");
            // On another page than the current one, so the toggle adds rather than removes.
            bookmark.locator = margin_config::Locator::Pdf { page: 2, quads: Vec::new(), at: None };
            bookmark
        })
        .collect();
    assert_eq!(reading::toggle_bookmark(view), reading::BookmarkToggle::Full);
    let message = reading::BookmarkToggle::Full.message(1);
    assert!(message.starts_with("Can't bookmark page 1") && !message.contains("Removed"), "{message}");
}
