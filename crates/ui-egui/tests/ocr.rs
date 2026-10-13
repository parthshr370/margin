//! Scan & OCR ▸ Recognize text in the real shell (egui_kittest): the dialog, the run and its
//! result (skipped without the OCR models: `cargo xtask models`).

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_engine::{AddedText, Edit, Session, export};
use pdfcraft_ui_egui::PdfCraftApp;

/// A one-page PDF that is only a picture of a sentence.
fn scan() -> Vec<u8> {
    scan_pages(1)
}

/// A PDF of `pages` pages, each only a picture of a sentence.
fn scan_pages(pages: usize) -> Vec<u8> {
    let mut s = Session::new();
    let text = s.create_from_text("t", "Recognize these scanned words").unwrap();
    let id = s.open("text.pdf", None, text, None).unwrap();
    let png = export::Exporter::new(s.get(id).unwrap()).png(0, 150.0).unwrap();
    let images: Vec<(String, Vec<u8>)> = (0..pages).map(|page| (format!("scan-{page}.png"), png.clone())).collect();
    s.create_from_images(&images).unwrap().to_vec()
}

#[test]
fn recognize_text_dialog_adds_searchable_text() {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("scan.pdf", None, scan()).unwrap();
        app.run_inline = true;
        app
    });
    h.run_steps(4);
    assert!(h.state_mut().execute("ocr.recognize"));
    h.run_steps(2);
    h.get_by_label("Document language");
    h.get_by_label("Output");
    if !pdfcraft_engine::ocr::available() {
        eprintln!("skipped: OCR models not installed");
        return;
    }
    h.get_by_label("Recognize text").click();
    h.run_steps(3);
    assert!(h.state().ocr_run.is_none(), "finished");
    let toast = h.state().toast.clone().unwrap().0;
    assert!(toast.starts_with("Recognized ") && toast.contains("on 1 page"), "{toast}");
    let app = h.state();
    let id = app.active_ids().unwrap().1;
    assert_eq!(app.session.get(id).unwrap().can_undo(), Some("Recognize text"));
}

#[test]
fn recognize_text_in_multiple_files_writes_searchable_copies() {
    if !pdfcraft_engine::ocr::available() {
        eprintln!("skipped: OCR models not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!("pdfcraft-ocr-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    app.run_inline = true;
    app.export_dir_override = Some(dir.to_string_lossy().into_owned());
    app.use_files(pdfcraft_ui_egui::FilePurpose::Ocr, vec![("one.pdf".into(), scan()), ("two.pdf".into(), scan())]);
    assert!(app.ocr_batch.is_none(), "finished");
    let toast = app.toast.clone().unwrap().0;
    assert!(toast.starts_with("Recognized ") && toast.ends_with("in 2 files"), "{toast}");
    for name in ["one.pdf", "two.pdf"] {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        assert!(bytes.len() > scan().len(), "an incremental update was appended");
    }
}

fn margin_scan() -> Harness<'static, PdfCraftApp> {
    let mut harness = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::with_flavor(pdfcraft_ui_egui::Flavor::Margin);
        app.margin_keys.set_data_dir(std::env::temp_dir().join(format!("margin-ocr-tests-{}", std::process::id())));
        app.open_bytes("scan.pdf", None, scan()).unwrap();
        app.run_inline = true;
        app
    });
    settle_scan(&mut harness);
    harness
}

fn settle_scan(harness: &mut Harness<'_, PdfCraftApp>) {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(20) {
        harness.run_steps(3);
        if !harness.state().render_pending() {
            harness.run_steps(3);
            if !harness.state().render_pending() {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("scan rendering did not settle");
}

fn scan_shot(harness: &mut Harness<'_, PdfCraftApp>, name: &str) {
    if let Some(directory) = std::env::var_os("MARGIN_OCR_SHOTS") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        harness.render().unwrap().save(directory.join(name)).unwrap();
        std::fs::write(directory.join("synthetic-scan.pdf"), scan()).unwrap();
    }
}

#[test]
fn margin_find_offer_recognizes_then_refreshes_search_and_selection() {
    let mut harness = margin_scan();
    harness.state_mut().set_option("find", "scanned").unwrap();
    settle_scan(&mut harness);
    harness.get_by_label("This page is scanned. Make text selectable?");
    scan_shot(&mut harness, "find-before-recognition.png");
    if !pdfcraft_engine::ocr::available() {
        return;
    }
    harness.get_by_label("Whole document").click();
    settle_scan(&mut harness);
    let app = harness.state_mut();
    let id = app.active_ids().unwrap().1;
    assert_eq!(app.session.get(id).unwrap().can_undo(), Some("Recognize text"));
    let (page, range) = app.views[0].find.as_ref().unwrap().matches.first().cloned().expect("recognized word is findable");
    app.views[0].select_text(page, range.start, range.end.saturating_sub(1));
    assert_eq!(app.views[0].selected_text().as_deref(), Some("scanned"));
    harness.run_steps(3);
    scan_shot(&mut harness, "find-after-recognition-selectable.png");
    assert!(harness.state_mut().execute("comment.highlight"));
    settle_scan(&mut harness);
    let app = harness.state();
    assert!(app.session.get(id).unwrap().info.annotations.iter().any(|annotation| annotation.subtype == "Highlight"));
    scan_shot(&mut harness, "recognized-text-highlight.png");
}

fn drag_scan(harness: &mut Harness<'_, PdfCraftApp>) {
    let rect = harness.state().views[0].page_screen_rect(0).unwrap();
    let origin = rect.center();
    let target = origin + egui::vec2(40.0, 0.0);
    harness.event(egui::Event::PointerMoved(origin));
    harness.event(egui::Event::PointerButton { pos: origin, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    harness.run_steps(2);
    harness.event(egui::Event::PointerMoved(target));
    harness.run_steps(2);
    harness.event(egui::Event::PointerButton { pos: target, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    harness.run_steps(3);
}

#[test]
fn margin_selection_offer_dismissal_stays_with_the_document() {
    let mut harness = margin_scan();
    drag_scan(&mut harness);
    harness.get_by_label("This page is scanned. Make text selectable?");
    scan_shot(&mut harness, "selection-scanned-page-offer.png");
    harness.get_by_label("Not now").click();
    harness.run_steps(3);
    drag_scan(&mut harness);
    assert!(harness.query_by_label("This page is scanned. Make text selectable?").is_none());
    harness.state_mut().set_option("find", "scanned").unwrap();
    settle_scan(&mut harness);
    assert!(harness.query_by_label("This page is scanned. Make text selectable?").is_none(), "Find respects the same dismissal");
}

#[test]
fn margin_read_only_scan_offers_save_as_before_ocr() {
    let directory = std::env::temp_dir().join(format!("margin-read-only-scan-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let original = directory.join("original.pdf");
    let writable = directory.join("copy.pdf");
    // One copy: two `scan()` calls are not byte-for-byte identical.
    let scan_bytes = scan();
    std::fs::write(&original, &scan_bytes).unwrap();
    let mut permissions = std::fs::metadata(&original).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&original, permissions).unwrap();
    let mut harness = margin_scan();
    harness.state_mut().close_tab(0);
    harness.state_mut().open_bytes("original.pdf", Some(original.to_string_lossy().into_owned()), scan_bytes.clone()).unwrap();
    harness.state_mut().save_override = Some(writable.to_string_lossy().into_owned());
    harness.state_mut().set_option("find", "scanned").unwrap();
    settle_scan(&mut harness);
    harness.get_by_label("Save as…").click();
    settle_scan(&mut harness);
    // Margin saves on a background worker; the path moves to the copy once it is written.
    const SAVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
    const POLL: std::time::Duration = std::time::Duration::from_millis(10);
    let id = harness.state().active_ids().unwrap().1;
    let deadline = std::time::Instant::now() + SAVE_TIMEOUT;
    while harness.state().margin_save_pending(id) && std::time::Instant::now() < deadline {
        harness.run_steps(1);
        std::thread::sleep(POLL);
    }
    harness.run_steps(2);
    let app = harness.state();
    assert_eq!(std::fs::read(&original).unwrap(), scan_bytes, "the source scan was not touched");
    assert!(writable.is_file());
    assert_eq!(app.session.get(app.active_ids().unwrap().1).unwrap().path.as_deref(), writable.to_str());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn margin_highlight_offer_does_not_create_an_area_mark() {
    let mut harness = margin_scan();
    harness.state_mut().quick_tool = pdfcraft_ui_egui::QuickTool::Comment(pdfcraft_ui_egui::comments::CommentTool::Highlight);
    drag_scan(&mut harness);
    harness.get_by_label("This page is scanned. Make text selectable?");
    let app = harness.state();
    assert!(app.session.get(app.active_ids().unwrap().1).unwrap().info.annotations.is_empty());
    if pdfcraft_engine::ocr::available() {
        harness.get_by_label("This page").click();
        settle_scan(&mut harness);
        harness.state_mut().set_option("find", "scanned").unwrap();
        settle_scan(&mut harness);
        assert!(!harness.state().views[0].find.as_ref().unwrap().matches.is_empty());
    }
}

#[test]
fn margin_blank_pages_do_not_offer_recognition() {
    let mut harness = margin_scan();
    harness.state_mut().close_tab(0);
    let bytes = harness.state_mut().session.create_from_text("blank", "").unwrap();
    harness.state_mut().open_bytes("blank.pdf", None, bytes.to_vec()).unwrap();
    harness.state_mut().set_option("find", "scanned").unwrap();
    settle_scan(&mut harness);
    assert!(harness.query_by_label("This page is scanned. Make text selectable?").is_none());
    drag_scan(&mut harness);
    assert!(harness.query_by_label("This page is scanned. Make text selectable?").is_none());
}

#[test]
fn margin_signed_scan_is_not_changed_by_recognition() {
    use pdfcraft_engine::sign;
    let mut harness = margin_scan();
    let key = sign::keys::PrivateKey::generate_p256().unwrap();
    let certificate =
        sign::Certificate::self_signed(&sign::Name::build("Synthetic signer", "", "", "", ""), &key, sign::Time::from_unix(1_700_000_000), 5, &[1])
            .unwrap();
    let identity = sign::DigitalId { key, certificate, chain: Vec::new(), friendly_name: None };
    let id = harness.state().active_ids().unwrap().1;
    let signed = harness.state().session.sign(id, &identity, sign::SignOptions::default()).unwrap();
    harness.state_mut().close_tab(0);
    harness.state_mut().open_bytes("signed.pdf", None, signed.to_vec()).unwrap();
    harness.state_mut().set_option("find", "scanned").unwrap();
    settle_scan(&mut harness);
    harness.get_by_label("This page is scanned. Make text selectable?");
    let id = harness.state().active_ids().unwrap().1;
    harness.state_mut().start_ocr();
    assert!(harness.state().ocr_run.is_none());
    assert!(!harness.state().session.get(id).unwrap().dirty);
    assert_eq!(*harness.state().session.get(id).unwrap().bytes, *signed);
}

/// Six scanned pages take seconds to read, so Cancel lands while recognition is running.
const LONG_SCAN_PAGES: usize = 6;

#[test]
fn margin_recognition_shows_progress_and_cancels_between_pages() {
    if !pdfcraft_engine::ocr::available() {
        return;
    }
    let mut harness = margin_scan();
    harness.state_mut().close_tab(0);
    harness.state_mut().open_bytes("long-scan.pdf", None, scan_pages(LONG_SCAN_PAGES)).unwrap();
    harness.state_mut().run_inline = false;
    settle_scan(&mut harness);
    harness.state_mut().set_option("find", "scanned").unwrap();
    settle_scan(&mut harness);
    harness.get_by_label("Whole document").click();
    harness.run_steps(2);
    // The prompt counts finished pages ("… 0 of 6 pages"); the toast names the current one.
    harness.get_by_label_contains(&format!("of {LONG_SCAN_PAGES} pages"));
    scan_shot(&mut harness, "recognition-progress.png");
    harness.get_by_label("Cancel recognition").click();
    harness.run_steps(2);
    let stopping_shown = harness.query_by_label("Stopping after this page…").is_some();
    assert!(stopping_shown || harness.state().ocr_run.is_none(), "Cancel is acknowledged while the page finishes");
    let start = std::time::Instant::now();
    while harness.state().ocr_run.is_some() {
        assert!(start.elapsed() < std::time::Duration::from_secs(120), "recognition stopped after Cancel");
        harness.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    settle_scan(&mut harness);
    let app = harness.state();
    let find = app.views[0].find.as_ref().unwrap();
    let recognized: std::collections::BTreeSet<usize> = find.matches.iter().map(|(page, _)| *page).collect();
    assert!(recognized.len() < LONG_SCAN_PAGES, "Cancel stopped before the last page: {recognized:?}");
    assert!(harness.query_by_label("Cancel recognition").is_none(), "the progress prompt closes with the run");
}

/// #1028: "Also recognize pages that already have text" reads a picture on a page that has its own
/// text, without adding that text a second time.
#[test]
fn recognize_text_can_read_pictures_on_pages_with_text() {
    let hybrid = {
        let mut s = Session::new();
        let id = s.open("scan.pdf", None, std::sync::Arc::new(scan()), None).unwrap();
        let (w, h) = {
            let p = &s.get(id).unwrap().info.pages[0];
            (p.width as f64, p.height as f64)
        };
        let note =
            AddedText { rect: [w * 0.1, h * 0.75, w * 0.9, h * 0.85], text: "Invoice reference 4711".into(), size: 18.0, ..Default::default() };
        s.apply(id, Edit::AddText { page: 0, text: note }).unwrap();
        s.save_bytes(id).unwrap().to_vec()
    };
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("hybrid.pdf", None, hybrid).unwrap();
        app.run_inline = true;
        app
    });
    h.run_steps(4);
    assert!(h.state_mut().execute("ocr.recognize"));
    h.run_steps(2);
    assert!(h.state().ocr_draft.settings().skip_text_pages, "pages with text are skipped by default");
    h.get_by_label("Also recognize pages that already have text").click();
    h.run_steps(2);
    assert!(!h.state().ocr_draft.settings().skip_text_pages);
    if !pdfcraft_engine::ocr::available() {
        eprintln!("skipped: OCR models not installed");
        return;
    }
    h.get_by_label("Recognize text").click();
    h.run_steps(3);
    assert!(h.state().ocr_run.is_none(), "finished");
    let toast = h.state().toast.clone().unwrap().0;
    assert!(toast.starts_with("Recognized ") && toast.ends_with("on 1 page"), "{toast}");
    let app = h.state();
    let id = app.active_ids().unwrap().1;
    let doc = app.session.get(id).unwrap();
    assert_eq!(doc.can_undo(), Some("Recognize text"));
    let mut r = pdfcraft_render::PageRenderer::new(doc.bytes.clone(), Default::default());
    let text = r.render(pdfcraft_render::RenderRequest { page: 0, kind: pdfcraft_render::RequestKind::Text, scale: 1.0, ..Default::default() });
    let text = text.text.unwrap().plain_text().to_lowercase();
    assert_eq!((text.matches("invoice").count(), text.matches("scanned").count()), (1, 1), "{text}");
}

/// Edit text & images on a scanned page: an OCR word (invisible text over the scan) opens for
/// editing on a click, stays open when the pointer leaves it, and its editor hides the scanned
/// word instead of showing it through.
#[test]
fn an_ocr_word_opens_for_editing_and_stays_open() {
    use pdfcraft_engine::ocr::PlacedWord;
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("scan.pdf", None, scan()).unwrap();
        let id = app.views[0].id;
        let ph = app.session.get(id).unwrap().info.pages[0].height as f64;
        let words = vec![PlacedWord { text: "Recognize".into(), origin: [72.0, ph - 100.0], across: [80.0, 0.0], up: [0.0, 14.0] }];
        app.session.apply(id, Edit::AddOcrText { page: 0, words }).unwrap();
        app.set_option("zoom", "100").unwrap();
        assert!(app.execute("edit.edit_text"));
        app
    });
    for _ in 0..60 {
        h.run_steps(2);
        if !h.state().render_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (r, k) = {
        let app = h.state();
        let d = app.session.get(app.views[0].id).unwrap();
        assert!(d.text_blocks(0).first().is_some_and(|b| b.invisible));
        let r = app.views[0].page_screen_rect(0).unwrap();
        (r, r.width() / d.info.pages[0].width)
    };
    // The word, 100 pt from the top of the page.
    let p = r.min + egui::vec2(100.0, 93.0) * k;
    h.hover_at(p);
    h.run_steps(2);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
    }
    h.run_steps(4);
    let opened = |h: &Harness<'_, PdfCraftApp>| h.state().views[0].line_editor.as_ref().map(|e| e.text.clone());
    assert_eq!(opened(&h).as_deref(), Some("Recognize"));
    assert!(h.state().views[0].image_selection.is_none(), "the scan under the word isn't selected");
    h.hover_at(r.min + egui::vec2(300.0, 400.0) * k);
    h.run_steps(4);
    assert_eq!(opened(&h).as_deref(), Some("Recognize"), "moving the pointer away keeps the word open");
}
