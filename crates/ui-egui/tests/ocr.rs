//! Scan & OCR ▸ Recognize text in the real shell (egui_kittest): the dialog, the run and its
//! result (skipped without the OCR models: `cargo xtask models`).

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_engine::{Session, export};
use pdfcraft_ui_egui::PdfCraftApp;

/// A one-page PDF that is only a picture of a sentence.
fn scan() -> Vec<u8> {
    let mut s = Session::new();
    let text = s.create_from_text("t", "Recognize these scanned words").unwrap();
    let id = s.open("text.pdf", None, text, None).unwrap();
    let png = export::Exporter::new(s.get(id).unwrap()).png(0, 150.0).unwrap();
    s.create_from_images(&[("scan.png".into(), png)]).unwrap().to_vec()
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
    std::fs::write(&original, scan()).unwrap();
    let mut permissions = std::fs::metadata(&original).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&original, permissions).unwrap();
    let mut harness = margin_scan();
    harness.state_mut().close_tab(0);
    harness.state_mut().open_bytes("original.pdf", Some(original.to_string_lossy().into_owned()), scan()).unwrap();
    harness.state_mut().save_override = Some(writable.to_string_lossy().into_owned());
    harness.state_mut().set_option("find", "scanned").unwrap();
    settle_scan(&mut harness);
    harness.get_by_label("Save as…").click();
    settle_scan(&mut harness);
    let app = harness.state();
    assert_eq!(std::fs::read(&original).unwrap(), scan(), "the source scan was not touched");
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
