//! Margin's reader layout (`Flavor::Margin`) in the real shell: no mode bar or tab strip, the
//! header, sidebar tabs, footer and annotate rail, each running PdfCraft's own commands.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::comments::CommentTool;
use pdfcraft_ui_egui::margin::SidebarTab;
use pdfcraft_ui_egui::{Dialog, Flavor, PdfCraftApp, QuickTool, RightPanel};

/// Two 300×200 pages of Helvetica text, with a two-entry outline.
const FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R /Outlines 8 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 5 0 R /Resources << /Font << /F1 7 0 R >> >> >> endobj
4 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> >> >> endobj
5 0 obj << /Length 55 >> stream
BT /F1 14 Tf 20 150 Td (The quick brown fox) Tj ET
endstream endobj
6 0 obj << /Length 58 >> stream
BT /F1 14 Tf 20 150 Td (A second brown animal) Tj ET
endstream endobj
7 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
8 0 obj << /Type /Outlines /First 9 0 R /Last 10 0 R /Count 2 >> endobj
9 0 obj << /Title (Opening) /Parent 8 0 R /Next 10 0 R /Dest [3 0 R /Fit] >> endobj
10 0 obj << /Title (Second part) /Parent 8 0 R /Prev 9 0 R /Dest [4 0 R /Fit] >> endobj
trailer << /Root 1 0 R >>
%%EOF";

fn harness(flavor: Flavor) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(flavor);
        app.set_option("language", "en").unwrap();
        app.open_bytes("reader.pdf", None, FIXTURE.to_vec()).expect("opens");
        app
    });
    h.run_steps(4);
    settle(&mut h);
    h
}

fn settle(h: &mut Harness<'static, PdfCraftApp>) {
    for _ in 0..200 {
        h.run_steps(2);
        if !h.state().render_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn click(h: &mut Harness<'static, PdfCraftApp>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(3);
}

#[test]
fn margin_shows_the_reader_chrome_instead_of_the_workbench() {
    let h = harness(Flavor::Margin);
    // PdfCraft's mode bar and right rail are gone…
    assert!(h.query_by_label("E-Sign").is_none());
    assert!(h.query_by_label("Comments").is_none());
    // …and Margin's header, sidebar, footer and rail are there.
    for label in ["Sidebar", "Annotate", "Page colour and layout", "Document: save, export, print, settings", "Library", "Saved"] {
        h.get_by_label(label);
    }
    h.get_by_label_contains("Find in document");
    // Contents lists the outline, and the header names the section being read.
    h.get_by_label("Opening");
    h.get_by_label("Second part");
    assert_eq!(h.state().margin_chrome.tab, SidebarTab::Contents);
    assert_eq!(h.state().window_title, "reader.pdf — Margin");
}

#[test]
fn pdfcraft_keeps_its_workbench() {
    let h = harness(Flavor::PdfCraft);
    h.get_by_label("E-Sign");
    assert!(h.query_by_label("Annotate").is_none());
    assert_eq!(h.state().window_title, "reader.pdf — PdfCraft");
}

#[test]
fn every_rail_button_runs_its_command() {
    let mut h = harness(Flavor::Margin);
    let tools = [
        ("Area snapshot: copy a region of the page", QuickTool::Snapshot),
        ("Highlight text", QuickTool::Comment(CommentTool::Highlight)),
        ("Underline text", QuickTool::Comment(CommentTool::Underline)),
        ("Add a sticky note", QuickTool::Comment(CommentTool::Note)),
        ("Draw freehand", QuickTool::Comment(CommentTool::Ink)),
        ("Add a text box", QuickTool::Comment(CommentTool::TextBox)),
        ("Erase drawings", QuickTool::Comment(CommentTool::Eraser)),
        ("Select", QuickTool::Select),
    ];
    for (label, tool) in tools {
        click(&mut h, label);
        assert_eq!(h.state().quick_tool, tool, "{label}");
    }
    // A comment tool doesn't pull the sidebar away from what the reader had open.
    assert_eq!(h.state().margin_chrome.tab, SidebarTab::Contents);
    // Undo and Redo: rotate a page, then take it back and redo it.
    let rotation = |h: &Harness<'static, PdfCraftApp>| {
        let app = h.state();
        app.session.get(app.views[0].id).map(|d| d.info.pages[0].rotation)
    };
    let before = rotation(&h);
    assert!(h.state_mut().execute("page.rotate"));
    h.run_steps(3);
    let rotated = rotation(&h);
    assert_ne!(rotated, before);
    h.get_by_label("Undo").click();
    h.run_steps(3);
    assert_eq!(rotation(&h), before);
    h.get_by_label("Redo").click();
    h.run_steps(3);
    assert_eq!(rotation(&h), rotated);
}

#[test]
fn sidebar_tabs_drive_pdfcrafts_panels() {
    let mut h = harness(Flavor::Margin);
    let tab = |h: &mut Harness<'static, PdfCraftApp>, label: &str| {
        // The header's Tools button shares its name with the Tools tab; the tab strip comes first.
        h.get_all_by_label(label).next().expect("tab").click();
        h.run_steps(3);
    };
    tab(&mut h, "Pages");
    assert_eq!((h.state().margin_chrome.tab, h.state().right), (SidebarTab::Pages, Some(RightPanel::Pages)));
    tab(&mut h, "Find");
    assert_eq!(h.state().right, Some(RightPanel::Search));
    assert!(h.state().views[0].find.is_some());
    tab(&mut h, "Highlights");
    assert_eq!(h.state().right, Some(RightPanel::Comments));
    h.get_by_label("No highlights yet.");
    tab(&mut h, "Tools");
    assert!(h.state().left_open);
    h.get_by_label("All tools");
    tab(&mut h, "Contents");
    assert_eq!((h.state().right, h.state().left_open), (None, false));
    // A command that opens a PdfCraft panel switches the sidebar to it.
    assert!(h.state_mut().execute("sign.panel"));
    h.run_steps(3);
    assert_eq!(h.state().margin_chrome.tab, SidebarTab::Other(RightPanel::Signatures));
    // The header's toggle hides the sidebar, and with it the panel.
    click(&mut h, "Sidebar");
    assert!(!h.state().margin_chrome.sidebar_open);
    assert_eq!(h.state().right, None);
}

#[test]
fn contents_footer_and_document_menu_work() {
    let mut h = harness(Flavor::Margin);
    click(&mut h, "Second part");
    settle(&mut h);
    assert_eq!(h.state().views[0].current, 1);
    let zoom = h.state().views[0].zoom;
    h.get_by_label_contains("Zoom in").click();
    h.run_steps(3);
    assert!(h.state().views[0].zoom > zoom);
    click(&mut h, "Document: save, export, print, settings");
    // Menu rows name their shortcut too (Ctrl+D, or ⌘D on a Mac).
    h.get_by_label_contains("Document properties…").click();
    h.run_steps(3);
    assert!(matches!(h.state().dialog, Some(Dialog::Properties(_))));
}

#[test]
fn margin_actions_run_by_id() {
    let mut h = harness(Flavor::Margin);
    let app = h.state_mut();
    assert!(pdfcraft_ui_egui::margin::run(app, "view.sidebar.tab", &serde_json::json!({ "tab": "pages" })));
    assert_eq!(app.margin_chrome.tab, SidebarTab::Pages);
    assert!(!pdfcraft_ui_egui::margin::run(app, "view.sidebar.tab", &serde_json::json!({ "tab": "nope" })));
    assert!(pdfcraft_ui_egui::margin::run(app, "reader.annotate.toggle", &serde_json::Value::Null));
    assert!(!pdfcraft_ui_egui::margin::annotate_mode(app));
    assert!(pdfcraft_ui_egui::margin::run(app, "view.sidebar.toggle", &serde_json::Value::Null));
    assert!(!app.margin_chrome.sidebar_open);
    assert!(!pdfcraft_ui_egui::margin::run(app, "view.unknown", &serde_json::Value::Null));
    h.run_steps(3);
    assert!(h.query_by_label("Select").is_none(), "the rail hides when annotating is off");
}

/// `pages` letter-size pages of text, each with its own outline entry (so Bookmarks has rows and
/// its search field). No xref: the parser rebuilds it, as for [`FIXTURE`].
fn long_fixture(pages: usize) -> Vec<u8> {
    // Objects: 1 catalog, 2 pages, 3 font, 4 outlines, then a page, its content and its outline
    // entry for each page.
    const FIRST_PAGE_OBJ: usize = 5;
    let page_obj = |i: usize| FIRST_PAGE_OBJ + 3 * i;
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", page_obj(i))).collect();
    let mut pdf = format!(
        "%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >> endobj\n\
         2 0 obj << /Type /Pages /Kids [{}] /Count {pages} >> endobj\n\
         3 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\n\
         4 0 obj << /Type /Outlines /First {} 0 R /Last {} 0 R /Count {pages} >> endobj\n",
        kids.join(" "),
        page_obj(0) + 2,
        page_obj(pages - 1) + 2,
    );
    for i in 0..pages {
        let (page, content, entry) = (page_obj(i), page_obj(i) + 1, page_obj(i) + 2);
        let text = format!("BT /F1 18 Tf 72 700 Td (Chapter {} begins here) Tj ET", i + 1);
        let prev = if i > 0 { format!(" /Prev {} 0 R", page_obj(i - 1) + 2) } else { String::new() };
        let next = if i + 1 < pages { format!(" /Next {} 0 R", page_obj(i + 1) + 2) } else { String::new() };
        pdf += &format!(
            "{page} 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {content} 0 R /Resources << /Font << /F1 3 0 R >> >> >> endobj\n\
             {content} 0 obj << /Length {} >> stream\n{text}\nendstream endobj\n\
             {entry} 0 obj << /Title (Chapter {}) /Parent 4 0 R{prev}{next} /Dest [{page} 0 R /XYZ 0 792 0] >> endobj\n",
            text.len(),
            i + 1,
        );
    }
    pdf += "trailer << /Root 1 0 R >>\n%%EOF";
    pdf.into_bytes()
}

#[test]
fn switching_sidebar_tabs_keeps_the_page_still() {
    const PAGES: usize = 12;
    const READING_PAGE: usize = 5;
    let mut h = Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        app.open_bytes("long.pdf", None, long_fixture(PAGES)).expect("opens");
        app
    });
    h.run_steps(4);
    settle(&mut h);
    // Read from the middle, so a scroll jump either way shows.
    h.state_mut().views[0].go_to_page(READING_PAGE);
    h.run_steps(4);
    settle(&mut h);
    let place = |h: &Harness<'static, PdfCraftApp>| {
        let view = &h.state().views[0];
        (view.viewport_rect(), view.page_screen_rect(READING_PAGE), view.zoom)
    };
    let before = place(&h);
    assert!(before.1.is_some(), "page {READING_PAGE} is on screen");
    for tab in SidebarTab::STRIP.into_iter().chain([SidebarTab::Contents]) {
        // The header's Tools button shares its name with the Tools tab; the tab strip comes first.
        h.get_all_by_label(tab.label()).next().expect("tab").click();
        h.run_steps(6);
        assert_eq!(h.state().margin_chrome.tab, tab);
        assert_eq!(place(&h), before, "{} moved the document", tab.label());
    }
}

#[test]
fn tooltips_name_the_keymap_shortcut_apart() {
    let mut h = harness(Flavor::Margin);
    // The shipped keymap only, not the user's `~/.config/margin`.
    let config = std::env::temp_dir().join(format!("margin-tooltip-test-{}", std::process::id()));
    std::fs::create_dir_all(&config).expect("temp dir");
    h.state_mut().margin_keys.set_config_dir(config.clone());
    h.run_steps(2);
    // The rail's Select: "Select", then its annotate-mode key from the keymap as a second label.
    h.get_by_label("Select").hover();
    for _ in 0..60 {
        h.run_steps(1);
        if h.query_by_label("V").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(h.query_by_label("V").is_some(), "the tooltip shows the shortcut");
    std::fs::remove_dir_all(&config).expect("temp dir removed");
}
