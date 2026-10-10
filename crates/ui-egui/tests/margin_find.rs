//! Margin's Find: one search session in the header's field and the Find tab's results, with the
//! keymap's Enter / Shift+Enter, every match reachable, and the earlier tab back on close.

use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::margin::SidebarTab;
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};

/// Pages of the fixture, lines per page and the word repeated on each line.
const PAGES: usize = 45;
const LINES: usize = 10;
const PER_LINE: usize = 5;
const WORD: &str = "needle";

/// `PAGES` letter pages, each with `LINES` lines of `PER_LINE` times [`WORD`]: more than 2,000
/// matches. No xref: the parser rebuilds it.
fn fixture() -> Vec<u8> {
    const FIRST_PAGE_OBJ: usize = 4;
    let page_obj = |i: usize| FIRST_PAGE_OBJ + 2 * i;
    let kids: Vec<String> = (0..PAGES).map(|i| format!("{} 0 R", page_obj(i))).collect();
    let mut pdf = format!(
        "%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
         2 0 obj << /Type /Pages /Kids [{}] /Count {PAGES} >> endobj\n\
         3 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\n",
        kids.join(" "),
    );
    let line = vec![WORD; PER_LINE].join(" ");
    for i in 0..PAGES {
        let (page, content) = (page_obj(i), page_obj(i) + 1);
        let lines: String = (0..LINES).map(|_| format!("({line}) Tj T* ")).collect();
        let text = format!("BT /F1 12 Tf 16 TL 72 700 Td {lines}ET");
        pdf += &format!(
            "{page} 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {content} 0 R /Resources << /Font << /F1 3 0 R >> >> >> endobj\n\
             {content} 0 obj << /Length {} >> stream\n{text}\nendstream endobj\n",
            text.len(),
        );
    }
    pdf += "trailer << /Root 1 0 R >>\n%%EOF";
    pdf.into_bytes()
}

fn harness() -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        // The shipped keymap and settings, and no library or stats rows in the user's data.
        let dir = std::env::temp_dir().join(format!("margin-find-test-{}", std::process::id()));
        app.margin_keys.set_config_dir(dir.join("config"));
        app.margin_keys.set_data_dir(dir.join("data"));
        app.open_bytes("needles.pdf", None, fixture()).expect("opens");
        app
    });
    h.run_steps(4);
    h
}

/// Run frames until every page's text is searched.
fn search_settles(h: &mut Harness<'static, PdfCraftApp>) {
    for _ in 0..1000 {
        h.run_steps(2);
        let view = &h.state().views[0];
        if view.find.is_some() && !h.state().render_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h.run_steps(3);
}

fn current(h: &Harness<'static, PdfCraftApp>) -> Option<usize> {
    h.state().views[0].find.as_ref().and_then(|f| f.current)
}

fn field_focused(h: &Harness<'static, PdfCraftApp>) -> bool {
    h.ctx.memory(|m| m.focused()) == Some(egui::Id::new("find-input"))
}

#[test]
fn one_find_in_the_header_and_the_find_tab() {
    let mut h = harness();
    let total = PAGES * LINES * PER_LINE;
    assert!(h.state_mut().execute("edit.find"));
    h.run_steps(3);
    assert_eq!(h.state().margin_chrome.tab, SidebarTab::Find, "the search shows its results");
    assert!(field_focused(&h), "the header's field takes the typing");
    assert!(h.query_by_label("What word or phrase would you like to search for?").is_none(), "no PdfCraft search panel");

    h.event(Event::Text(WORD.into()));
    search_settles(&mut h);
    assert_eq!(h.state().views[0].find.as_ref().map(|f| f.matches.len()), Some(total));
    assert_eq!(current(&h), Some(0), "the first match from the reading page");
    h.get_by_label_contains(&format!("{total} matches on {PAGES} pages"));

    // Enter in the field: one step each, the field keeps the keyboard.
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert_eq!(current(&h), Some(1));
    assert!(field_focused(&h));
    h.key_press_modifiers(Modifiers::SHIFT, Key::Enter);
    h.key_press_modifiers(Modifiers::SHIFT, Key::Enter);
    h.run_steps(3);
    assert_eq!(current(&h), Some(total - 1), "back past the first wraps to the last");
    assert_eq!(h.state().views[0].current, PAGES - 1, "and the page follows");

    // The list reaches the last matches (past index 2,000) and goes where a row is clicked.
    let last_page = format!("p. {PAGES}: ");
    assert!(h.query_all_by_label_contains(&last_page).count() > 0, "the last page's matches are listed");
    h.get_all_by_label_contains(&last_page).next().expect("a row").click();
    h.run_steps(3);
    let picked = current(&h).expect("a match");
    assert!(picked >= total - LINES * PER_LINE, "a match on the last page: {picked}");
    assert!(picked > 2000);

    // The options search again.
    h.get_by_label("Match case").click();
    h.run_steps(2);
    search_settles(&mut h);
    let find = h.state().views[0].find.as_ref().expect("still open");
    assert!(find.case_sensitive);
    assert_eq!(find.matches.len(), total);
    h.get_by_label("Whole words").click();
    h.run_steps(2);
    search_settles(&mut h);
    assert!(h.state().views[0].find.as_ref().is_some_and(|f| f.whole_words && f.matches.len() == total));

    // Closing brings back the tab shown before, and it stays closed.
    h.get_by_label("Close find").click();
    h.run_steps(6);
    assert!(h.state().views[0].find.is_none());
    assert_eq!(h.state().margin_chrome.tab, SidebarTab::Contents);

    // The Find tab opens the same search; Esc in the field closes it, and Find doesn't reopen.
    h.get_all_by_label("Find").next().expect("the Find tab").click();
    h.run_steps(3);
    assert!(h.state().views[0].find.is_some());
    assert!(field_focused(&h));
    h.key_press(Key::Escape);
    h.run_steps(6);
    assert!(h.state().views[0].find.is_none(), "Esc in the field closes the search");
    assert_eq!(h.state().margin_chrome.tab, SidebarTab::Contents);
}

/// In a narrow window the field gets its own row under the header, never a box over the page.
#[test]
fn a_narrow_window_puts_the_field_on_its_own_row() {
    let mut h = harness();
    h.set_size(egui::vec2(760.0, 640.0));
    assert!(h.state_mut().execute("edit.find"));
    h.run_steps(4);
    let field = h.get_by_label("Close find").rect();
    let page = h.state().views[0].viewport_rect();
    assert!(field.bottom() <= page.top(), "the field ({field:?}) is above the page ({page:?})");
}

/// R8 QoL 6: a search keeps the reader's place once, however many matches it steps through, and
/// one Back after closing it returns there.
#[test]
fn a_search_keeps_the_reading_place_once() {
    use pdfcraft_ui_egui::margin::reading::{self, Spot};
    let mut h = harness();
    reading::go(&mut h.state_mut().views[0], Spot { page: 3, y: 0.4, x: None });
    h.run_steps(4);
    let origin = reading::place(&h.state().views[0]);
    assert!(h.state().views[0].back.is_empty(), "moving the view is not a jump");
    assert!(h.state_mut().execute("edit.find"));
    h.run_steps(3);
    h.event(Event::Text(WORD.into()));
    search_settles(&mut h);
    for _ in 0..30 {
        h.key_press(Key::Enter);
        h.run_steps(1);
    }
    assert_eq!(h.state().views[0].back.len(), 1, "one entry for the whole search");
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(h.state().views[0].find.is_none());
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowLeft);
    h.run_steps(4);
    let back = reading::place(&h.state().views[0]);
    assert!(back.0 == origin.0 && (back.1 - origin.1).abs() < 0.01, "Back to {back:?}, the search started at {origin:?}");
}
