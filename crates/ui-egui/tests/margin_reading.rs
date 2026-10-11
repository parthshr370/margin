//! Margin's reading state (R5) in the real shell: the sidecar brings a document back as it was
//! left, auto trim fills the width with the text, page bookmarks persist, and annotations
//! autosave into the PDF.

use std::path::PathBuf;

use egui::{Key, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::canvas::Fit;
use pdfcraft_ui_egui::margin::pagecolor::PageMode;
use pdfcraft_ui_egui::margin::reading::{self, SaveStatus};
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};

const LETTER: (f32, f32) = (612.0, 792.0);
/// One inch: the synthetic paper's margins, like the corpus' `paper2col.pdf`.
const INCH: f32 = 72.0;
/// Each simulated step: big enough that a second passes in a handful of frames.
const STEP_SECS: f32 = 0.1;

/// A two-column paper: `pages` letter pages whose ink (bars standing in for lines of text) sits
/// inside one-inch margins.
fn paper(pages: usize) -> Vec<u8> {
    let mut content = String::new();
    let column_w = (LETTER.0 - 2.0 * INCH - 18.0) / 2.0;
    let mut y = LETTER.1 - INCH;
    while y > INCH + 8.0 {
        for x in [INCH, INCH + column_w + 18.0] {
            content.push_str(&format!("{x} {} {column_w} 7 re f\n", y - 7.0));
        }
        y -= 12.0;
    }
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", i + 4)).collect();
    let mut objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Kids [{}] /Count {pages} /MediaBox [0 0 {} {}] >>", kids.join(" "), LETTER.0, LETTER.1),
        format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
    ];
    objs.extend((0..pages).map(|_| "<< /Type /Page /Parent 2 0 R /Contents 3 0 R >>".to_owned()));
    with_xref(&objs)
}

/// One 300×200 page of Helvetica text (for highlights).
fn text_page() -> Vec<u8> {
    let content = "BT /F1 14 Tf 20 150 Td (The quick brown fox) Tj ET\n";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    with_xref(&objs)
}

fn with_xref(objs: &[String]) -> Vec<u8> {
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

/// A folder with the document, an empty config folder and a data folder; removed on drop.
struct Place(PathBuf);

impl Place {
    fn new(name: &str, file: &str, bytes: &[u8]) -> Self {
        let root = std::env::temp_dir().join(format!("margin-r5-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs").join(file), bytes).unwrap();
        Self(root)
    }
    fn doc(&self) -> PathBuf {
        let entry = std::fs::read_dir(self.0.join("docs")).unwrap().flatten().find(|e| e.path().extension().is_some_and(|x| x == "pdf"));
        entry.unwrap().path()
    }
    fn config(&self) -> PathBuf {
        self.0.join("config")
    }
    fn sidecar(&self) -> PathBuf {
        let doc = self.doc();
        doc.with_file_name(format!("{}.margin.json", doc.file_name().unwrap().to_string_lossy()))
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// The Margin app with `place`'s folders and its document opened (no frame drawn yet).
fn app(place: &Place) -> PdfCraftApp {
    let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
    app.set_option("language", "en").unwrap();
    app.margin_keys.set_config_dir(place.config());
    app.margin_keys.set_data_dir(place.0.join("data"));
    let doc = place.doc();
    let bytes = std::fs::read(&doc).unwrap();
    app.open_bytes(&doc.file_name().unwrap().to_string_lossy(), Some(doc.to_string_lossy().into_owned()), bytes).expect("opens");
    app
}

fn harness(place: &Place) -> Harness<'static, PdfCraftApp> {
    let app = app(place);
    let mut h = Harness::builder().with_size(egui::vec2(1300.0, 900.0)).with_step_dt(STEP_SECS).build_eframe(move |_cc| app);
    settle(&mut h);
    h
}

/// Run until renders stop arriving.
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

/// Run `secs` of app time.
fn wait(h: &mut Harness<'static, PdfCraftApp>, secs: f32) {
    h.run_steps((secs / STEP_SECS).ceil() as usize);
}

fn sidecar_json(place: &Place) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(place.sidecar()).expect("sidecar written")).expect("sidecar is JSON")
}

#[test]
fn opening_alone_writes_nothing() {
    let place = Place::new("open", "paper.pdf", &paper(3));
    let mut h = harness(&place);
    wait(&mut h, 3.0);
    assert!(!place.sidecar().exists(), "never written on open");
    assert_eq!(reading::save_status(h.state(), 0), SaveStatus::Saved { autosave: true });
}

#[test]
fn reopen_restores_page_zoom_page_color_and_trim() {
    let place = Place::new("reopen", "paper.pdf", &paper(6));
    let mut h = harness(&place);
    {
        let app = h.state_mut();
        app.set_option("zoom", "150").unwrap();
        app.set_option("page", "4").unwrap();
        app.set_option("page_color", "sepia").unwrap();
        app.set_option("trim", "auto").unwrap();
    }
    settle(&mut h);
    assert!(!place.sidecar().exists(), "waits a second after the last change");
    wait(&mut h, 1.5);
    let saved = sidecar_json(&place);
    assert_eq!(saved["format"], "margin-sidecar");
    assert_eq!(saved["version"], 1);
    assert_eq!(saved["progress"]["locator"]["page"], 3);
    assert_eq!(saved["settings"]["view"]["page_color"], "sepia");
    assert_eq!(saved["settings"]["view"]["fit"], "zoom");
    assert_eq!(saved["settings"]["view"]["zoom"], 150);
    assert_eq!(saved["settings"]["view"]["trim"], true);
    assert_eq!(saved["doc"]["sidecar_fingerprint"].as_str().map(str::len), Some(32));
    drop(h);

    // Restored by the open itself, before any frame is drawn.
    let app = app(&place);
    let view = &app.views[0];
    assert_eq!(view.current, 3);
    assert_eq!(view.fit, Fit::None);
    assert!((view.zoom - 1.5).abs() < 1e-3, "zoom {}", view.zoom);
    assert_eq!(view.margin.display.color.mode, PageMode::Sepia);
    assert!(!view.margin.display.follow_system);
    assert!(view.margin.trim.on && !view.margin.trim.manual);
    let mut h = Harness::builder().with_size(egui::vec2(1300.0, 900.0)).with_step_dt(STEP_SECS).build_eframe(move |_cc| app);
    settle(&mut h);
    assert_eq!(h.state().views[0].current, 3, "the first frames keep the page");
    let shown = h.state().views[0].visible_page_rects();
    assert!(shown.iter().any(|(page, _)| *page == 3), "{shown:?}");
}

/// The page rect of `page` on screen, once drawn.
fn page_rect(h: &Harness<'static, PdfCraftApp>, page: usize) -> egui::Rect {
    let shown = h.state().views[0].visible_page_rects();
    shown.iter().find(|(p, _)| *p == page).map(|(_, r)| *r).unwrap_or_else(|| panic!("page {page} not shown: {shown:?}"))
}

/// R8 QoL 4: 400 % (above the old 300 % cap), halfway down a page and panned sideways, comes
/// back with the same document point under the same window point, in each fit and layout.
#[test]
fn reopen_puts_the_same_text_under_the_same_point() {
    // Read fit never shows a page wider than the window: there only the vertical spot can move.
    for (layout, read_fit, trim) in [("continuous", false, "off"), ("two-up", false, "auto"), ("continuous", true, "manual")] {
        let place = Place::new(&format!("spot-{layout}-{trim}"), "paper.pdf", &paper(6));
        let mut h = harness(&place);
        {
            let app = h.state_mut();
            app.set_option("layout", layout).unwrap();
            app.set_option("trim", trim).unwrap();
            app.set_option("zoom", "400").unwrap();
            if read_fit {
                reading::fit_read(&mut app.views[0], 1100);
            }
        }
        settle(&mut h);
        reading::go(&mut h.state_mut().views[0], reading::Spot { page: 2, y: 0.55, x: Some(0.7) });
        settle(&mut h);
        let before = page_rect(&h, 2);
        if !read_fit {
            assert!(before.left() < h.state().views[0].viewport_rect().left(), "{layout} {trim}: panned sideways, {before:?}");
        }
        let zoom = h.state().views[0].zoom;
        wait(&mut h, 1.5);
        let saved = sidecar_json(&place);
        assert_eq!(saved["progress"]["locator"]["page"], 2, "{layout} {trim}");
        if !read_fit {
            assert_eq!(saved["settings"]["view"]["zoom"], 400, "not clamped to 300");
        }
        drop(h);

        let app = app(&place);
        let mut h = Harness::builder().with_size(egui::vec2(1300.0, 900.0)).with_step_dt(STEP_SECS).build_eframe(move |_cc| app);
        settle(&mut h);
        assert!((h.state().views[0].zoom - zoom).abs() < zoom * 0.01, "{layout} {trim}: zoom {} was {zoom}", h.state().views[0].zoom);
        let after = page_rect(&h, 2);
        assert!((after.min - before.min).length() <= 2.0, "{layout} {trim}: page 2 at {after:?}, was {before:?}");
        assert_eq!(std::fs::read(place.doc()).unwrap(), paper(6), "reading never writes the PDF");
    }
}

/// The spot's page and height share, for comparing reading lines.
fn line(h: &Harness<'static, PdfCraftApp>) -> (usize, f32) {
    reading::place(&h.state().views[0])
}

fn same_line(a: (usize, f32), b: (usize, f32)) -> bool {
    a.0 == b.0 && (a.1 - b.1).abs() < 0.01
}

/// The footer's zoom percent opens its menu ("Zoom and fit" is the tooltip; the label is the
/// percent shown).
fn open_zoom_menu(h: &mut Harness<'static, PdfCraftApp>) {
    let percent = format!("{:.0}%", h.state().views[0].zoom * 100.0);
    h.get_by_label(&percent).click();
}

/// R8 QoL 5: footer − / + and the zoom menu use `view.zoom_steps` like the keys; a fit change,
/// `W` and a resized window keep the reading line; fit "read" in Two-up fits the whole spread.
#[test]
fn zoom_and_fit_entry_points_agree_and_keep_the_line() {
    let place = Place::new("fit", "paper.pdf", &paper(6));
    std::fs::create_dir_all(place.config()).unwrap();
    std::fs::write(place.config().join("settings.jsonc"), "{ \"view\": { \"fit\": \"zoom\", \"zoom\": 100, \"zoom_steps\": [80, 130, 400] } }")
        .unwrap();
    let mut h = harness(&place);
    let zoom = |h: &Harness<'static, PdfCraftApp>| (h.state().views[0].zoom * 100.0).round();
    h.get_by_label("Zoom in").click();
    settle(&mut h);
    assert_eq!(zoom(&h), 130.0, "the footer's + takes the next configured stop");
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::Equals);
    settle(&mut h);
    assert_eq!(zoom(&h), 400.0, "and so does Ctrl+=");
    h.get_by_label("Zoom out").click();
    settle(&mut h);
    assert_eq!(zoom(&h), 130.0);
    open_zoom_menu(&mut h);
    h.run_steps(2);
    h.get_by_label("400%").click();
    settle(&mut h);
    assert_eq!(zoom(&h), 400.0, "the menu lists the configured stops");
    // The menu stays open while choosing (item 2): Esc closes it.
    h.key_press(Key::Escape);
    h.run_steps(2);

    // Halfway down p. 3, every fit change keeps the line.
    reading::go(&mut h.state_mut().views[0], reading::Spot { page: 2, y: 0.5, x: None });
    settle(&mut h);
    let start = line(&h);
    for (key, fit) in [(Key::W, "width"), (Key::W, "reading width")] {
        h.key_press(key);
        settle(&mut h);
        assert!(same_line(line(&h), start), "W to {fit}: {:?}, was {start:?}", line(&h));
    }
    assert!(reading::reads(&h.state().views[0]), "W toggles back to the reading width, not to page");
    open_zoom_menu(&mut h);
    h.run_steps(2);
    h.get_by_label("Fit page").click();
    settle(&mut h);
    assert!(same_line(line(&h), start), "Fit page: {:?}, was {start:?}", line(&h));
    h.key_press(Key::Escape);
    h.key_press(Key::W);
    settle(&mut h);
    assert_eq!(h.state().views[0].fit, Fit::Width);
    h.set_size(egui::vec2(1000.0, 900.0));
    settle(&mut h);
    assert!(same_line(line(&h), start), "a narrower window at fit width: {:?}, was {start:?}", line(&h));

    // Two-up at the reading width: both pages of the spread fit the window.
    h.state_mut().set_option("layout", "two-up").unwrap();
    reading::fit_read(&mut h.state_mut().views[0], 4000);
    settle(&mut h);
    let view = &h.state().views[0];
    let right = view.visible_page_rects().iter().map(|(_, r)| r.right()).fold(f32::MIN, f32::max);
    assert!(right <= view.viewport_rect().right() + 0.5, "the spread's right page ends at {right}, window {:?}", view.viewport_rect());
}

/// R8 QoL 6: Back returns to the exact place and zoom before a jump; page turns and moving the
/// view add no history; a jump within the page is reversible; Alt+←/→, the pill and the mouse's
/// back button share the one history; the pill goes after six seconds.
#[test]
fn back_returns_to_the_exact_place_before_a_jump() {
    let place = Place::new("back", "paper.pdf", &paper(8));
    let mut h = harness(&place);
    h.state_mut().set_option("zoom", "150").unwrap();
    settle(&mut h);
    h.state_mut().views[0].step_page(true);
    settle(&mut h);
    reading::go(&mut h.state_mut().views[0], reading::Spot { page: 2, y: 0.5, x: None });
    settle(&mut h);
    assert!(h.state().views[0].back.is_empty(), "page turns and moving the view are not jumps");
    let origin = line(&h);

    // A link, outline entry, bookmark or the page box: `go_to_page`.
    h.state_mut().views[0].go_to_page(6);
    settle(&mut h);
    assert_eq!(line(&h).0, 6);
    h.get_by_label("Back to p. 3");
    // Zoomed after the jump (175 %: the test renderer's textures stop at 2048 px untiled).
    h.state_mut().set_option("zoom", "175").unwrap();
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::ALT, Key::ArrowLeft);
    settle(&mut h);
    assert!(same_line(line(&h), origin), "Back to {:?}, was {origin:?}", line(&h));
    assert!((h.state().views[0].zoom - 1.5).abs() < 1e-3, "and its zoom: {}", h.state().views[0].zoom);
    assert!(h.query_by_label("Back to p. 3").is_none(), "Back takes the pill away");
    h.key_press_modifiers(egui::Modifiers::ALT, Key::ArrowRight);
    settle(&mut h);
    assert_eq!(line(&h).0, 6, "Forward");

    // A jump to the top of the page being read is reversible too; the pill goes back.
    reading::go(&mut h.state_mut().views[0], reading::Spot { page: 6, y: 0.6, x: None });
    settle(&mut h);
    let middle = line(&h);
    h.state_mut().views[0].go_to_page(6);
    settle(&mut h);
    assert!(line(&h).1 < 0.05, "at the page's top: {:?}", line(&h));
    h.get_by_label("Back to p. 7").click();
    settle(&mut h);
    assert!(same_line(line(&h), middle), "the pill: {:?}, was {middle:?}", line(&h));

    // The mouse's back button.
    h.state_mut().views[0].go_to_page(0);
    settle(&mut h);
    let pointer = h.state().views[0].viewport_rect().center();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: pointer, button: egui::PointerButton::Extra1, pressed, modifiers: egui::Modifiers::NONE });
    }
    settle(&mut h);
    assert!(same_line(line(&h), middle), "the mouse's back button: {:?}, was {middle:?}", line(&h));
    h.state_mut().views[0].go_to_page(4);
    settle(&mut h);
    assert!(h.query_by_label("Back to p. 7").is_some());
    wait(&mut h, 6.5);
    assert!(h.query_by_label("Back to p. 7").is_none(), "the pill goes after six seconds");
}

#[test]
fn auto_trim_fills_the_width_with_the_text_at_fit_width() {
    let place = Place::new("trim", "paper2col.pdf", &paper(3));
    let mut h = harness(&place);
    h.state_mut().views[0].set_fit(Fit::Width);
    settle(&mut h);
    let width = |h: &Harness<'static, PdfCraftApp>| h.state().views[0].page_screen_rect(0).expect("page 1 on screen").width();
    let full = width(&h);
    h.key_press_modifiers(egui::Modifiers::SHIFT, Key::T);
    settle(&mut h);
    let view = &h.state().views[0];
    assert!(view.margin.trim.on, "Shift+T trims");
    let [u0, _, u1, _] = view.margin.trim.crop(0);
    assert!(u0 > 0.09 && u1 < 0.91, "the one-inch margins are cut: {:?}", view.margin.trim.crop(0));
    // The page box keeps its width at fit width; the text inside it is what grows.
    let trimmed = width(&h);
    assert!((trimmed - full).abs() < 2.0, "fit width still fills the window: {full} vs {trimmed}");
    let text = (LETTER.0 - 2.0 * INCH) / LETTER.0;
    let text_on_screen = trimmed * text / (u1 - u0);
    assert!(text_on_screen > 0.95 * trimmed, "text spans {text_on_screen} of {trimmed} px");
    assert!(text_on_screen > 1.25 * full * text, "the text is drawn wider than untrimmed");
    h.key_press_modifiers(egui::Modifiers::SHIFT, Key::T);
    settle(&mut h);
    assert!(!h.state().views[0].margin.trim.on);
}

#[test]
fn page_bookmarks_persist_and_list_above_the_pdf_bookmarks() {
    let place = Place::new("bookmarks", "paper.pdf", &paper(5));
    let mut h = harness(&place);
    h.state_mut().set_option("page", "3").unwrap();
    settle(&mut h);
    h.key_press(Key::B);
    h.run_steps(2);
    assert_eq!(h.state().views[0].margin.bookmarks.len(), 1);
    assert_eq!(h.state().views[0].margin.bookmarks.first().and_then(reading::bookmark_page), Some(2));
    h.state_mut().margin_chrome.tab = pdfcraft_ui_egui::margin::SidebarTab::Bookmarks;
    h.state_mut().margin_chrome.sidebar_open = true;
    h.run_steps(3);
    let ours = h.get_by_label_contains("PAGE BOOKMARKS").rect().top();
    let pdf = h.get_by_label("BOOKMARKS").rect().top();
    assert!(ours < pdf, "page bookmarks come first ({ours} vs {pdf})");
    reading::flush(h.state_mut());
    assert_eq!(sidecar_json(&place)["bookmarks"][0]["locator"]["page"], 2);
    drop(h);
    let app = app(&place);
    assert_eq!(app.views[0].margin.bookmarks.first().and_then(reading::bookmark_page), Some(2));
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
    h.run_steps(1);
}

#[test]
fn a_highlight_autosaves_within_two_seconds() {
    let place = Place::new("autosave", "text.pdf", &text_page());
    let mut h = harness(&place);
    h.state_mut().set_option("quick", "highlight").unwrap();
    h.run_steps(2);
    let (a, b) = {
        let view = &h.state().views[0];
        (view.glyph_screen_pos(0, 4).expect("text layer"), view.glyph_screen_pos(0, 14).expect("glyph"))
    };
    drag(&mut h, a, b);
    // The highlight's note opens for typing; close it as a reader would.
    h.state_mut().views[0].comments.editing = None;
    let id = h.state().views[0].id;
    assert!(h.state().session.get(id).unwrap().dirty, "the highlight is an unsaved change");
    assert_eq!(reading::save_status(h.state(), 0), SaveStatus::Saving);
    h.run_steps(1);
    assert_eq!(h.state().window_title, "• text.pdf — Margin", "the window marks unsaved edits");
    let mut waited = 0.0;
    while h.state().session.get(id).unwrap().dirty && waited < 2.0 {
        wait(&mut h, STEP_SECS);
        waited += STEP_SECS;
    }
    assert_eq!(reading::save_status(h.state(), 0), SaveStatus::Saved { autosave: true }, "saved within 2 s");
    h.get_by_label("Saved");
    h.run_steps(1);
    assert_eq!(h.state().window_title, "text.pdf — Margin", "the mark clears after the save");
    let bytes = std::fs::read(place.doc()).unwrap();
    assert!(bytes.windows(10).any(|w| w == b"/Highlight"), "the highlight is in the file");
    assert!(h.state().toast.as_ref().is_none_or(|t| !t.0.starts_with("Saved")), "no toast for an autosave");
}

#[test]
fn autosave_off_leaves_the_pdf_to_ctrl_s() {
    let place = Place::new("autosave-off", "text.pdf", &text_page());
    std::fs::create_dir_all(place.config()).unwrap();
    std::fs::write(place.config().join("settings.jsonc"), "{ \"reader\": { \"autosave\": false } }").unwrap();
    let mut h = harness(&place);
    h.state_mut().set_option("quick", "highlight").unwrap();
    h.run_steps(2);
    let (a, b) = {
        let view = &h.state().views[0];
        (view.glyph_screen_pos(0, 4).expect("text layer"), view.glyph_screen_pos(0, 14).expect("glyph"))
    };
    drag(&mut h, a, b);
    h.state_mut().views[0].comments.editing = None;
    wait(&mut h, 2.0);
    assert_eq!(reading::save_status(h.state(), 0), SaveStatus::Unsaved);
    assert!(h.query_by_label_contains("Unsaved · ").is_some(), "the footer names the key that saves");
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::S);
    h.run_steps(3);
    assert_eq!(reading::save_status(h.state(), 0), SaveStatus::Saved { autosave: false }, "Ctrl+S still saves");
}

/// Highlight the text page's first words (the note that opens is closed, as a reader would).
fn highlight(h: &mut Harness<'static, PdfCraftApp>) {
    h.state_mut().set_option("quick", "highlight").unwrap();
    h.run_steps(2);
    let (a, b) = {
        let view = &h.state().views[0];
        (view.glyph_screen_pos(0, 4).expect("text layer"), view.glyph_screen_pos(0, 14).expect("glyph"))
    };
    drag(h, a, b);
    h.state_mut().views[0].comments.editing = None;
}

/// R8 QoL 7: a failed autosave stays visible and offers Save and Save as, the change stays
/// unsaved until a save works; a read-only file says so and is never written by itself.
#[cfg(unix)]
#[test]
fn the_footer_tells_the_truth_about_saving() {
    use std::os::unix::fs::PermissionsExt as _;
    let place = Place::new("save-fails", "text.pdf", &text_page());
    let docs = place.doc().parent().unwrap().to_path_buf();
    std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores permissions: nothing to show there.
    if std::fs::write(docs.join(".probe"), b"").is_ok() {
        std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let mut h = harness(&place);
    highlight(&mut h);
    wait(&mut h, 2.0);
    let id = h.state().views[0].id;
    assert_eq!(reading::save_status(h.state(), 0), SaveStatus::Failed);
    assert!(h.state().session.get(id).unwrap().dirty, "the change is kept, unsaved");
    h.get_by_label("Couldn't save");
    h.get_by_label("Save as…");
    wait(&mut h, 3.0);
    assert_eq!(reading::save_status(h.state(), 0), SaveStatus::Failed, "the failure stays until a save works");
    std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o755)).unwrap();
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert_eq!(reading::save_status(h.state(), 0), SaveStatus::Saved { autosave: true }, "Save retried");
    assert!(std::fs::read(place.doc()).unwrap().windows(10).any(|w| w == b"/Highlight"));
    drop(h);

    let place = Place::new("read-only-file", "text.pdf", &text_page());
    std::fs::set_permissions(place.doc(), std::fs::Permissions::from_mode(0o444)).unwrap();
    let mut h = harness(&place);
    assert_eq!(reading::save_status(h.state(), 0), SaveStatus::ReadOnly { dirty: false });
    h.get_by_label("Read-only");
    highlight(&mut h);
    wait(&mut h, 2.0);
    assert_eq!(reading::save_status(h.state(), 0), SaveStatus::ReadOnly { dirty: true });
    h.get_by_label("Save as…");
    assert_eq!(std::fs::read(place.doc()).unwrap(), text_page(), "never written by itself");
    std::fs::set_permissions(place.doc(), std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn set_as_default_writes_settings_jsonc_and_keeps_its_comments() {
    let place = Place::new("default", "paper.pdf", &paper(2));
    let mut h = harness(&place);
    let before = std::fs::read_to_string(place.config().join("settings.jsonc")).expect("first run writes settings.jsonc");
    h.state_mut().set_option("page_color", "ink").unwrap();
    h.state_mut().set_option("layout", "two-up").unwrap();
    let message = reading::set_as_default(h.state_mut(), 0, reading::DefaultRow::PageColor);
    assert!(message.starts_with("Page color saved"), "{message}");
    reading::set_as_default(h.state_mut(), 0, reading::DefaultRow::Layout);
    let after = std::fs::read_to_string(place.config().join("settings.jsonc")).unwrap();
    let header = before.lines().next().unwrap();
    assert!(after.starts_with(header), "comments stay:\n{after}");
    let loaded = margin_config::Settings::from_user_text(&after, "settings.jsonc");
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    assert_eq!(loaded.settings.view.page_color, margin_config::PageColor::Ink);
    assert!(!loaded.settings.view.follow_system);
    assert_eq!(loaded.settings.view.layout, margin_config::Layout::TwoUp);
    // A document Margin hasn't seen opens with the new defaults.
    let other = Place::new("default-other", "fresh.pdf", &paper(2));
    std::fs::create_dir_all(other.config()).unwrap();
    std::fs::write(other.config().join("settings.jsonc"), &after).unwrap();
    let app = app(&other);
    assert_eq!(app.views[0].margin.display.color.mode, PageMode::Ink);
    assert_eq!(app.views[0].layout, pdfcraft_ui_egui::canvas::PageLayout::TwoUp);
}

/// A folder Margin can't write keeps the sidecar under `<data>/sidecars/<fingerprint>…`.
#[cfg(unix)]
#[test]
fn read_only_folders_keep_the_sidecar_in_the_data_folder() {
    use std::os::unix::fs::PermissionsExt as _;
    let place = Place::new("readonly", "paper.pdf", &paper(4));
    let docs = place.doc().parent().unwrap().to_path_buf();
    std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores permissions: nothing to show there.
    if std::fs::write(docs.join(".probe"), b"").is_ok() {
        std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let mut h = harness(&place);
    h.state_mut().set_option("page", "2").unwrap();
    settle(&mut h);
    wait(&mut h, 1.5);
    let path = reading::sidecar_path(h.state(), 0).expect("written");
    std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(path.starts_with(place.0.join("data").join("sidecars")), "{}", path.display());
    assert!(!place.sidecar().exists());
    let told = h.state().toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(told.contains("data folder"), "the fallback is explained: {told:?}");
    drop(h);
    let written = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(app(&place).views[0].current, 1, "found again by fingerprint: {written}");
}

#[test]
fn disabling_per_document_memory_keeps_bookmarks_but_ignores_and_preserves_view_state() {
    let place = Place::new("remember-off", "paper.pdf", &paper(5));
    let mut original = harness(&place);
    original.state_mut().set_option("page", "3").unwrap();
    original.state_mut().set_option("zoom", "150").unwrap();
    reading::toggle_bookmark(&mut original.state_mut().views[0]);
    settle(&mut original);
    reading::flush(original.state_mut());
    let saved = sidecar_json(&place);
    drop(original);
    std::fs::write(place.config().join("settings.jsonc"), r#"{ "reader": { "remember_per_document": false } }"#).unwrap();
    let mut current = harness(&place);
    assert_eq!(current.state().views[0].current, 0, "the saved page is ignored");
    assert!(reading::reads(&current.state().views[0]), "global read-fit wins over the saved zoom");
    assert_eq!(current.state().views[0].margin.bookmarks.len(), 1);
    current.state_mut().set_option("page", "5").unwrap();
    current.state_mut().set_option("zoom", "175").unwrap();
    settle(&mut current);
    reading::toggle_bookmark(&mut current.state_mut().views[0]);
    reading::flush(current.state_mut());
    let rewritten = sidecar_json(&place);
    assert_eq!(rewritten["progress"], saved["progress"]);
    assert_eq!(rewritten["settings"], saved["settings"]);
    assert_eq!(rewritten["bookmarks"].as_array().unwrap().len(), 2);
}
