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
    pdf_with(pages, "")
}

/// `pages` blank 200×300 pages, with `catalog` (more catalog entries, such as `/PageLabels`).
fn pdf_with(pages: usize, catalog: &str) -> Vec<u8> {
    pdf_sized(pages, 300, catalog)
}

/// `pages` blank pages 200 points wide and `height` tall, with `catalog` entries.
fn pdf_sized(pages: usize, height: u32, catalog: &str) -> Vec<u8> {
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", i + 3)).collect();
    let mut objs = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>"),
        format!("<< /Type /Pages /Kids [{}] /Count {pages} /MediaBox [0 0 200 {height}] >>", kids.join(" ")),
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
    // Cmd+P on macOS, Ctrl+P elsewhere: the key egui's COMMAND modifier presses.
    std::fs::write(dir.join("keymap.jsonc"), "[ { \"context\": \"App\", \"bindings\": { \"secondary-p\": \"view.palette\" } } ]").unwrap();
    let stroke = margin_config::canonical("secondary-p", margin_config::Os::current()).unwrap();
    step_until(&mut h, "the keymap reload", |app| {
        app.margin_keys
            .keymap()
            .is_some_and(|k| k.bindings.iter().any(|b| b.keys == [stroke.as_str()] && b.command.as_deref() == Some("view.palette")))
    });
    h.run_steps(2);
    assert!(!h.state().palette_open);
    h.key_press_modifiers(Modifiers::COMMAND, Key::P);
    h.run_steps(2);
    assert!(h.state().palette_open, "Cmd/Ctrl+P opens the palette");
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
        keys.iter()
            .map(|key| {
                press(h, *key);
                current(h)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(turns(&mut h, &[Key::ArrowRight, Key::ArrowRight, Key::ArrowLeft]), [2, 4, 2], "Two-up: spreads [0 1] [2 3] [4 5]");
    h.state_mut().views[0].set_cover(true);
    press(&mut h, Key::Home);
    assert_eq!(turns(&mut h, &[Key::ArrowRight, Key::ArrowRight, Key::ArrowLeft, Key::ArrowLeft]), [1, 3, 1, 0], "with a cover: [0] [1 2] [3 4] [5]");
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

/// What the page field shows (between frames PdfCraft's canvas leaves the page number in
/// `DocView::page_input`; Margin's field shows the label).
fn page_field_text(h: &Harness<'static, PdfCraftApp>) -> String {
    h.get_by_label("Go to page").value().unwrap_or_default()
}

fn page_field_focused(h: &Harness<'static, PdfCraftApp>) -> bool {
    h.ctx.memory(|m| m.has_focus(egui::Id::new("page-input")))
}

/// G, then `typed` into the page field (G selects what it holds, so the typing replaces it),
/// then Enter.
fn go_to(h: &mut Harness<'static, PdfCraftApp>, typed: &str) {
    press(h, Key::G);
    assert!(page_field_focused(h), "G focuses the page field");
    h.event(egui::Event::Text(typed.to_owned()));
    h.run_steps(1);
    press(h, Key::Enter);
}

/// R8 QoL 14: the footer's page field, named "Go to page", takes a label first, then a page
/// number; a page the document doesn't have stays in the field with the document's pages on a
/// card; Esc puts the current label back.
#[test]
fn the_page_field_goes_by_label_then_number_and_keeps_what_it_refuses() {
    // Labels i, ii, iii, iv, 1, 2.
    let labels = "/PageLabels << /Nums [0 << /S /r >> 4 << /S /D >>] >>";
    let dir = config_dir("page-field");
    let mut h = harness_with(&dir, pdf_with(6, labels));
    assert_eq!(page_field_text(&h), "i", "the field shows the label");
    h.get_by_label("(1 / 6)");

    for (typed, page) in [("iv", 3), ("1", 4), ("6", 5), ("  iii  ", 2)] {
        go_to(&mut h, typed);
        assert_eq!(current(&h), page, "{typed:?}: a label first, else a page number");
        assert!(!page_field_focused(&h), "Enter hands the keys back to the page");
    }
    assert_eq!(page_field_text(&h), "iii");
    h.get_by_label("(3 / 6)");

    let long = "9".repeat(5000);
    for typed in ["0", "7", "xyz", long.as_str()] {
        go_to(&mut h, typed);
        assert_eq!(current(&h), 2, "{typed:.12}: no jump");
        assert!(page_field_focused(&h), "the field stays open to fix it");
        let kept = page_field_text(&h);
        assert!(kept.chars().count() <= 64, "a long paste is cut");
        assert!(kept.starts_with(&typed[..1]), "and the typing stays: {kept:?}");
        h.get_by_label_contains("This PDF has pages 1–6, labelled i to 2");
        press(&mut h, Key::Escape);
        assert!(!page_field_focused(&h));
        assert_eq!(page_field_text(&h), "iii", "Esc puts the current label back");
        assert!(h.query_by_label_contains("This PDF has pages").is_none());
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// A 60-fps harness over `bytes`, with `settings` written to a fresh config folder first.
fn realtime_harness(dir: &Path, bytes: Vec<u8>, settings: Option<&str>) -> Harness<'static, PdfCraftApp> {
    std::fs::create_dir_all(dir).unwrap();
    if let Some(text) = settings {
        std::fs::write(dir.join("settings.jsonc"), text).unwrap();
    }
    let dir = dir.to_path_buf();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_step_dt(1.0 / 60.0).with_max_steps(120).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        app.margin_keys.set_config_dir(dir.clone());
        app.margin_keys.set_data_dir(dir.join("data"));
        app.open_bytes("doc.pdf", None, bytes.clone()).unwrap();
        app
    });
    h.run_steps(4);
    h.try_run().expect("the window goes idle");
    h
}

/// The first page's top on screen (it moves up as the document scrolls down).
fn first_page_top(h: &Harness<'static, PdfCraftApp>) -> f32 {
    h.state().views[0].page_screen_rect(0).expect("the first page is drawn").top()
}

/// A screen step: `view.scroll_screen` (0.85 by default) of the document area.
fn screen(h: &Harness<'static, PdfCraftApp>) -> f32 {
    h.state().views[0].viewport_rect().height() * 0.85
}

/// Points within this are the same scroll position.
const SAME_SPOT: f32 = 1.0;

/// R8 QoL 15: Page Down glides one screen and stops (the idle window asks for no more frames);
/// held presses add up to exactly one screen each; the reader's own wheel drops what is left of
/// a step; under `ui.reduce_motion` the step lands at once.
#[test]
fn screen_steps_glide_add_up_and_yield_to_the_wheel() {
    // One page 6000 points tall: every step stays inside it.
    let tall = || pdf_sized(1, 6000, "");
    let dir = config_dir("screen-steps");
    let mut h = realtime_harness(&dir, tall(), None);
    let (top, step) = (first_page_top(&h), screen(&h));
    press(&mut h, Key::PageDown);
    let partway = top - first_page_top(&h);
    assert!(partway > 0.0 && partway < step - SAME_SPOT, "mid-glide: {partway} of {step}");
    let frames = h.try_run().expect("the step ends");
    assert!(frames <= 20, "{frames} frames");
    assert!((top - first_page_top(&h) - step).abs() < SAME_SPOT, "one screen: {}", top - first_page_top(&h));
    // The pages the step uncovered render in the background; once they are in, nothing moves.
    while h.state().render_pending() {
        std::thread::sleep(std::time::Duration::from_millis(5));
        h.run_steps(1);
    }
    h.try_run().expect("idle");
    assert_eq!(h.try_run().expect("idle"), 1, "an idle reader stops repainting");

    let top = first_page_top(&h);
    for _ in 0..4 {
        h.key_press(Key::PageDown);
        h.run_steps(1);
    }
    h.try_run().expect("the steps end");
    assert!((top - first_page_top(&h) - 4.0 * step).abs() < SAME_SPOT, "four presses, four screens: {}", top - first_page_top(&h));

    let top = first_page_top(&h);
    press(&mut h, Key::PageDown);
    h.hover_at(h.state().views[0].viewport_rect().center());
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::Vec2::ZERO,
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    h.try_run().expect("idle");
    let moved = top - first_page_top(&h);
    assert!(moved > 0.0 && moved < step - SAME_SPOT, "the wheel stopped the step: {moved} of {step}");
    std::fs::remove_dir_all(&dir).ok();

    let dir = config_dir("screen-steps-reduced");
    let mut h = realtime_harness(&dir, tall(), Some("{ \"ui\": { \"reduce_motion\": true } }"));
    let top = first_page_top(&h);
    press(&mut h, Key::PageDown);
    assert!((top - first_page_top(&h) - screen(&h)).abs() < SAME_SPOT, "reduced motion: the step lands at once");
    std::fs::remove_dir_all(&dir).ok();
}

/// R8 QoL 15: in Single, Page Down turns a page that fits once per press, and pans an enlarged
/// page to its bottom before it turns.
#[test]
fn single_turns_a_fitting_page_and_pans_an_enlarged_one_first() {
    let dir = config_dir("single-steps");
    let mut h = realtime_harness(&dir, pdf(3), None);
    assert!(h.state_mut().execute("view.layout.single"));
    h.state_mut().views[0].set_fit(pdfcraft_ui_egui::canvas::Fit::Page);
    h.try_run().expect("idle");
    press(&mut h, Key::PageDown);
    h.try_run().expect("idle");
    assert_eq!(current(&h), 1, "a fitting page turns once");
    press(&mut h, Key::PageUp);
    h.try_run().expect("idle");
    assert_eq!(current(&h), 0);

    h.state_mut().views[0].set_zoom(5.0);
    h.try_run().expect("idle");
    let mut pans = 0;
    while current(&h) == 0 {
        let page = h.state().views[0].page_screen_rect(0).unwrap();
        let window = h.state().views[0].viewport_rect();
        press(&mut h, Key::PageDown);
        h.try_run().expect("idle");
        if current(&h) == 0 {
            pans += 1;
        } else {
            assert!(page.bottom() <= window.bottom() + SAME_SPOT, "it turned only with the page's bottom in view: {page:?} {window:?}");
        }
        assert!(pans < 10, "the pans reach the bottom");
    }
    assert!(pans >= 1, "the enlarged page panned before it turned");
    assert_eq!(current(&h), 1, "one turn");
    std::fs::remove_dir_all(&dir).ok();
}
