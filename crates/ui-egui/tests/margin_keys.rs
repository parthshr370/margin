//! Margin flavor: settings.jsonc and keymap.jsonc in the config folder, live reload, the keyboard
//! sheet, and the keymap driving the keys (R3).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};

fn fixture() -> Vec<u8> {
    pdf(2)
}

/// `pages` blank 200×300 pages.
fn pdf(pages: usize) -> Vec<u8> {
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", i + 3)).collect();
    let mut objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Kids [{}] /Count {pages} /MediaBox [0 0 200 300] >>", kids.join(" ")),
    ];
    objs.extend((0..pages).map(|_| "<< /Type /Page /Parent 2 0 R >>".to_owned()));
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

/// A fresh config folder for one test.
fn config_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("margin-r3-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    dir
}

fn harness(dir: &Path) -> Harness<'static, PdfCraftApp> {
    harness_with(dir, fixture())
}

fn harness_with(dir: &Path, bytes: Vec<u8>) -> Harness<'static, PdfCraftApp> {
    let dir = dir.to_path_buf();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        app.margin_keys.set_config_dir(dir.clone());
        app.margin_keys.set_data_dir(dir.join("data"));
        app.open_bytes("doc.pdf", None, bytes.clone()).unwrap();
        app
    });
    h.run_steps(4);
    h
}

/// Steps the app until `done` holds; the watcher thread delivers saves asynchronously.
fn step_until(h: &mut Harness<'static, PdfCraftApp>, what: &str, done: impl Fn(&PdfCraftApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(h.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
        h.run_steps(2);
    }
}

#[test]
fn first_run_writes_commented_files_and_schemas() {
    let dir = config_dir("first-run");
    let h = harness(&dir);
    let settings = std::fs::read_to_string(dir.join("settings.jsonc")).unwrap();
    assert!(settings.contains("    // \"layout\": \"scroll\","), "every default is listed, commented out:\n{settings}");
    let keymap = std::fs::read_to_string(dir.join("keymap.jsonc")).unwrap();
    assert!(keymap.contains("view.palette") && keymap.trim_end().ends_with("[\n]"), "{keymap}");
    assert!(dir.join("schemas/settings.schema.json").is_file());
    assert!(dir.join("schemas/keymap.schema.json").is_file());
    assert!(h.state().toast.is_none(), "a clean first run says nothing: {:?}", h.state().toast);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn question_mark_opens_the_keyboard_sheet() {
    let dir = config_dir("sheet");
    let mut h = harness(&dir);
    h.key_press(Key::Questionmark);
    h.run_steps(2);
    assert!(h.state().margin_keys.sheet_open());
    assert!(h.state().dialog.is_none(), "PdfCraft's shortcuts dialog stays for the PdfCraft flavor");
    h.get_by_label("Key context…");
    h.get_by_label("Zoom (100)");
    h.get_by_label("Reader");
    h.get_by_label("AnnotateMode");
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(!h.state().margin_keys.sheet_open());
    // The menu and palette entry opens the same sheet.
    h.state_mut().execute("help.shortcuts");
    h.run_steps(2);
    assert!(h.state().margin_keys.sheet_open());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_saved_keymap_applies_without_a_restart() {
    let dir = config_dir("reload");
    let mut h = harness(&dir);
    std::fs::write(dir.join("keymap.jsonc"), "[ { \"context\": \"App\", \"bindings\": { \"ctrl-p\": \"view.palette\" } } ]").unwrap();
    step_until(&mut h, "the keymap reload", |app| {
        app.margin_keys.keymap().is_some_and(|k| k.bindings.iter().any(|b| b.keys == ["ctrl-p"] && b.command.as_deref() == Some("view.palette")))
    });
    h.run_steps(2);
    assert!(!h.state().palette_open);
    h.key_press_modifiers(Modifiers::COMMAND, Key::P);
    h.run_steps(2);
    assert!(h.state().palette_open, "Ctrl+P opens the palette");
    assert!(h.state().dialog.is_none(), "and no longer prints");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_bad_setting_falls_back_with_a_notice() {
    let dir = config_dir("bad-setting");
    let mut h = harness(&dir);
    std::fs::write(dir.join("settings.jsonc"), "{\n  \"keys\": { \"chord_timeout_ms\": \"fast\" },\n  \"view\": { \"scroll_line_px\": 120 }\n}\n")
        .unwrap();
    step_until(&mut h, "the settings reload", |app| app.margin_keys.settings().view.scroll_line_px == 120);
    assert_eq!(h.state().margin_keys.settings().keys.chord_timeout_ms, 1000, "the default replaces the bad value");
    let toast = h.state().toast.as_ref().map(|(text, _)| text.clone()).unwrap_or_default();
    assert!(toast.starts_with("settings.jsonc:2:") && toast.contains("chord_timeout_ms"), "{toast}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn view_keys_run_through_the_keymap() {
    let dir = config_dir("view-keys");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("keymap.jsonc"), "[ { \"context\": \"Reader\", \"bindings\": { \"end\": null, \"x\": \"reader.page.last\" } } ]")
        .unwrap();
    let mut h = harness(&dir);
    h.key_press(Key::End);
    h.run_steps(3);
    assert_eq!(h.state().views[0].current, 0, "End is unbound: PdfCraft's hard-coded End no longer runs");
    h.key_press(Key::X);
    h.run_steps(3);
    assert_eq!(h.state().views[0].current, 1, "x goes to the last page");
    h.key_press(Key::Home);
    h.run_steps(3);
    assert_eq!(h.state().views[0].current, 0);
    std::fs::remove_dir_all(&dir).ok();
}

fn press(h: &mut Harness<'static, PdfCraftApp>, key: Key) {
    h.key_press(key);
    h.run_steps(3);
}

fn current(h: &Harness<'static, PdfCraftApp>) -> usize {
    h.state().views[0].current
}

/// Types `text` the way a keyboard does: each key, then the character it types.
fn type_keys(h: &mut Harness<'static, PdfCraftApp>, text: &str) {
    for ch in text.chars() {
        let key = Key::from_name(&ch.to_string()).expect("a letter key");
        h.key_press(key);
        h.event(egui::Event::Text(ch.to_string()));
        h.run_steps(2);
    }
}

/// R8 QoL 13: the same keys in each layout. Scroll: j moves a line and never a page, Right
/// turns one; Single: j/k turn the page; Two-up: Right/Left turn a spread, with and without a
/// cover page; Home/End everywhere. Typing j, k, g in Find or the page field moves nothing.
#[test]
fn page_keys_follow_the_layout() {
    const PAGES: usize = 6;
    let dir = config_dir("layout-keys");
    let mut h = harness_with(&dir, pdf(PAGES));
    press(&mut h, Key::J);
    assert_eq!(current(&h), 0, "Scroll: j scrolls a line");
    press(&mut h, Key::ArrowRight);
    assert_eq!(current(&h), 1);
    press(&mut h, Key::End);
    assert_eq!(current(&h), PAGES - 1);
    press(&mut h, Key::Home);
    assert_eq!(current(&h), 0);

    assert!(h.state_mut().execute("view.layout.single"));
    h.run_steps(3);
    press(&mut h, Key::J);
    assert_eq!(current(&h), 1, "Single: j turns the page");
    press(&mut h, Key::J);
    press(&mut h, Key::K);
    assert_eq!(current(&h), 1, "and k turns it back");

    assert!(h.state_mut().execute("view.layout.two_up"));
    h.state_mut().views[0].set_cover(false);
    press(&mut h, Key::Home);
    let turns = |h: &mut Harness<'static, PdfCraftApp>, keys: &[Key]| {
        keys.iter().map(|key| {
            press(h, *key);
            current(h)
        })
        .collect::<Vec<_>>()
    };
    assert_eq!(turns(&mut h, &[Key::ArrowRight, Key::ArrowRight, Key::ArrowLeft]), [2, 4, 2], "Two-up: spreads [0 1] [2 3] [4 5]");
    h.state_mut().views[0].set_cover(true);
    press(&mut h, Key::Home);
    assert_eq!(
        turns(&mut h, &[Key::ArrowRight, Key::ArrowRight, Key::ArrowLeft, Key::ArrowLeft]),
        [1, 3, 1, 0],
        "with a cover: [0] [1 2] [3 4] [5]"
    );
    let place = current(&h);

    assert!(h.state_mut().execute("edit.find"));
    h.run_steps(3);
    type_keys(&mut h, "jkg");
    assert_eq!(h.state().views[0].find.as_ref().map(|f| f.query.as_str()), Some("jkg"), "Find took the letters");
    assert_eq!(current(&h), place, "typing in Find moves nothing");
    press(&mut h, Key::Escape);
    assert!(h.state_mut().execute("view.focus_page_input"));
    h.run_steps(2);
    type_keys(&mut h, "jk");
    assert_eq!(current(&h), place, "typing in the page field moves nothing");
    std::fs::remove_dir_all(&dir).ok();
}
