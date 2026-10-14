//! Margin's library and reading stats (R6, R10 §5) in the real shell: a folder scan indexes
//! PDFs with covers on a worker thread, Continue opens a book where it was left, reading logs
//! page times and a session, and the Stats sheet shows the seeded history: today on the
//! calendar, the cards' totals, the streaks and a sortable table.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use margin_config::library::Cover;
use margin_config::stats::{Recorder, Showing, StatsDb};
use pdfcraft_ui_egui::margin::{reading, stats_sheet};
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};

const STEP_SECS: f32 = 0.1;
/// The acceptance budget for indexing 200 PDFs with covers, files in the OS cache.
const SCAN_BUDGET: Duration = Duration::from_secs(10);
const BOOKS: usize = 200;

/// A PDF of `pages` 200×300 pages with a title, a drawn rectangle on each page.
fn pdf(title: &str, pages: usize) -> Vec<u8> {
    let content = "0.2 0.3 0.8 rg 20 180 160 100 re f 0 g BT /F1 18 Tf 24 140 Td (Margin) Tj ET\n";
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", i + 6)).collect();
    let mut objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Kids [{}] /Count {pages} /MediaBox [0 0 200 300] >>", kids.join(" ")),
        format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
        format!("<< /Title ({title}) /Author (Test Author) >>"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    objs.extend((0..pages).map(|_| "<< /Type /Page /Parent 2 0 R /Contents 3 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned()));
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{obj}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

/// A temporary root with `books/`, `config/` and `data/`; removed on drop.
struct Place(PathBuf);

impl Place {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("margin-r6-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        for dir in ["books", "config", "data"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        Self(root)
    }
    fn books(&self) -> PathBuf {
        self.0.join("books")
    }
    fn data(&self) -> PathBuf {
        self.0.join("data")
    }
    fn add(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.books().join(name);
        std::fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }
    /// `settings.jsonc` watching `books/`.
    fn watch_books(&self) {
        let settings = serde_json::json!({ "library": { "folders": [self.books()] } });
        std::fs::write(self.0.join("config").join("settings.jsonc"), settings.to_string()).unwrap();
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn app(place: &Place) -> PdfCraftApp {
    let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
    app.set_option("language", "en").unwrap();
    app.margin_keys.set_config_dir(place.0.join("config"));
    app.margin_keys.set_data_dir(place.data());
    app
}

fn harness(app: PdfCraftApp) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1440.0, 900.0)).with_step_dt(STEP_SECS).build_eframe(move |_cc| app);
    h.run_steps(4);
    h
}

/// Steps until no scan runs and `want` books are listed, or `limit` passes since `start`.
fn wait_for_scan(h: &mut Harness<'static, PdfCraftApp>, want: usize, start: Instant, limit: Duration) -> Duration {
    while start.elapsed() < limit {
        h.run_steps(1);
        if !h.state().margin_library.scanning() && h.state().margin_library.books().len() >= want {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    start.elapsed()
}

#[test]
fn scanning_200_pdfs_indexes_them_with_covers_within_budget() {
    let place = Place::new("scan");
    for i in 0..BOOKS {
        place.add(&format!("book-{i:03}.pdf"), &pdf(&format!("Book {i:03}"), 1 + i % 5));
    }
    place.watch_books();
    let start = Instant::now();
    let mut h = harness(app(&place));
    let took = wait_for_scan(&mut h, BOOKS, start, SCAN_BUDGET * 6);
    eprintln!("scan of {BOOKS} PDFs with covers (files in the OS cache): {took:?}");
    let books = h.state().margin_library.books();
    assert_eq!(books.len(), BOOKS);
    assert!(books.iter().all(|book| book.cover == Cover::Cached), "every book has a cover");
    assert!(books.iter().any(|book| book.title == "Book 007" && book.author == "Test Author" && book.pages == 3));
    assert!(took < SCAN_BUDGET, "indexed in {took:?}");
    // The grid shows: covers of the books on screen are decoded (on a worker), only those.
    let start = Instant::now();
    while h.state().margin_library.covers_loaded() == 0 && start.elapsed() < Duration::from_secs(5) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(10);
    let loaded = h.state().margin_library.covers_loaded();
    assert!(loaded > 0 && loaded < BOOKS, "{loaded} covers decoded");
    assert!(h.query_all_by_label_contains("Book ").next().is_some(), "books are listed");
    // Frames that change nothing don't filter and sort the books again.
    let built = h.state().margin_library.orders_built();
    h.run_steps(20);
    assert_eq!(h.state().margin_library.orders_built(), built, "the shelf's order is kept between frames");
    h.state_mut().margin_library.query = "book 01".into();
    h.run_steps(2);
    assert_eq!(h.state().margin_library.orders_built(), built + 1, "and made again when the search changes");
    h.state_mut().margin_library.query.clear();
    drop(h);

    // A rescan of unchanged files reads none of them again.
    let start = Instant::now();
    let mut h = harness(app(&place));
    let rescan = wait_for_scan(&mut h, BOOKS, start, SCAN_BUDGET);
    eprintln!("rescan of {BOOKS} unchanged PDFs: {rescan:?}");
    assert!(rescan < SCAN_BUDGET);
    assert_eq!(h.state().margin_library.books().len(), BOOKS);
}

#[test]
fn continue_opens_the_book_at_its_saved_page() {
    let place = Place::new("continue");
    let path = place.add("novel.pdf", &pdf("A Novel", 10));
    place.add("other.pdf", &pdf("Another", 2));
    let mut first = app(&place);
    first.open_path(&path);
    first.set_option("page", "7").unwrap();
    let mut h = harness(first);
    h.run_steps(20);
    reading::flush(h.state_mut());
    assert_eq!(h.state().views[0].current, 6);
    drop(h);

    let mut h = harness(app(&place));
    h.run_steps(6);
    assert!(h.state().active.is_none(), "starts on the library");
    let hero = h.state().margin_library.books().iter().find(|book| book.title == "A Novel").cloned().expect("listed");
    assert_eq!(hero.page, 6);
    h.get_by_label("Continue").click();
    // The file is read and prepared in the background; its tab opens on a later frame.
    assert!(step_until(&mut h, SCAN_BUDGET, |h| h.state().active.is_some()), "a book opened");
    let app = h.state();
    let index = app.active.expect("a book is shown");
    assert_eq!(app.views[index].current, 6, "opened where it was left");
}

#[test]
fn reading_logs_page_times_a_session_and_the_calendar_shows_today() {
    let place = Place::new("stats");
    let path = place.add("paper.pdf", &pdf("Paper", 6));
    let mut app = app(&place);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    app.margin_stats.set_clock(now - 120.0);
    app.open_path(&path);
    let mut h = harness(app);
    for _ in 0..3 {
        h.state_mut().margin_stats.advance_clock(10.0);
        h.key_press(Key::ArrowRight);
        h.run_steps(3);
    }
    assert_eq!(h.state().views[0].current, 3);
    let app = h.state();
    let doc = app.views[0].id;
    let book = app.margin_stats.book(doc).expect("a stats key").to_owned();
    let db = app.margin_stats.db().expect("stats.sqlite open");
    assert_eq!(db.page_time_count(&book).unwrap(), 3, "three pages read");
    assert_eq!(db.session_count(&book).unwrap(), 1, "one session");
    assert_eq!(db.summary(&book).unwrap().seconds, 30);
    h.run_steps(2);
    assert!(h.query_by_label_contains("min/page").is_some(), "the document card shows the book's stats");
    // reader.time_left: 3 pages in 30 s is 10 s a page, and pages 5 and 6 are still to read.
    h.get_by_label("~20s left");
    std::fs::write(place.0.join("config").join(margin_config::SETTINGS_FILE), r#"{ "reader": { "time_left": false } }"#).unwrap();
    h.state_mut().margin_keys.set_config_dir(place.0.join("config"));
    h.run_steps(3);
    assert!(h.query_by_label("~20s left").is_none(), "time_left false hides the estimate");

    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::T);
    h.run_steps(3);
    assert!(h.state().margin_stats.sheet_open(), "Ctrl+Shift+T opens the Stats sheet");
    // The day's cell and the calendar's keyboard day (today) both say it.
    assert!(h.query_all_by_label_contains("Today, ").next().is_some(), "the calendar marks today");
    assert!(h.query_all_by_label_contains(": 0 min 30 sec · 3 pages · 1 book").next().is_some(), "with today's time");
    let now = h.state().margin_stats.now();
    let data = stats_sheet::load(h.state_mut(), now).unwrap();
    assert_eq!(data.seconds_on(data.today), 30);

    let csv = place.0.join("stats.csv");
    h.state_mut().pick_override = Some(vec![csv.to_string_lossy().into_owned()]);
    h.get_by_label("Export CSV").click();
    h.run_steps(4);
    let text = std::fs::read_to_string(&csv).expect("CSV written");
    assert_eq!(text.lines().count(), 4, "{text}");
    assert!(text.lines().nth(1).unwrap().contains(",Paper,1,10"), "{text}");
}

/// One page visit of `seconds` from `start` (Unix seconds), through the real recorder.
fn seed_visit(db: &StatsDb, book: &str, page: u32, start: i64, seconds: i64) {
    let mut recorder = Recorder::default();
    recorder.step(db, start as f64, Some(Showing { book, page }), true).unwrap();
    recorder.stop(db, (start + seconds) as f64).unwrap();
}

/// Scrolls the Stats sheet's body to its end.
fn scroll_sheet_down(h: &mut Harness<'static, PdfCraftApp>) {
    h.hover_at(h.get_by_label_contains("Today: ").rect().center());
    for _ in 0..10 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -400.0),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        });
        h.run_steps(1);
    }
    h.run_steps(4);
}

#[test]
fn the_stats_sheet_shows_seeded_totals_streaks_and_a_sorted_table() {
    let place = Place::new("stats-seeded");
    let noon = {
        let db = StatsDb::open(&place.data()).unwrap();
        db.note_book("alpha", "Alpha", 10).unwrap();
        db.note_book("beta", "Beta", 10).unwrap();
        let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let today = db.local_day(i64::try_from(wall).unwrap()).unwrap();
        let at = |day: i64, clock: &str| db.local_unix(day, clock).unwrap();
        seed_visit(&db, "alpha", 0, at(today, "10:00:00"), 600);
        seed_visit(&db, "alpha", 1, at(today, "10:30:00"), 300);
        seed_visit(&db, "beta", 0, at(today - 1, "12:00:00"), 1200);
        seed_visit(&db, "alpha", 2, at(today - 2, "09:00:00"), 600);
        // A week ago: one visit before noon, one after (left out of the to-date comparison).
        seed_visit(&db, "beta", 1, at(today - 7, "09:00:00"), 300);
        seed_visit(&db, "beta", 2, at(today - 7, "13:00:00"), 600);
        for day in today - 40..=today - 37 {
            seed_visit(&db, "alpha", 5, at(day, "08:00:00"), 60);
        }
        at(today, "12:00:00")
    };
    let mut app = app(&place);
    app.margin_stats.set_clock(noon as f64);
    let mut h = harness(app);
    assert!(pdfcraft_ui_egui::margin::run(h.state_mut(), "app.stats", &serde_json::Value::Null));
    h.run_steps(4);

    assert!(h.query_by_label_contains("Today: 15m. +10m vs last ").is_some(), "today against the same weekday, up to noon");
    assert!(h.query_by_label_contains("Last 12 months: 1h 4m. 8 active days").is_some());
    assert!(h.query_by_label_contains("Books: 2.").is_some());
    assert!(
        h.query_all_by_label_contains("Today, ").any(|day| day.rect().width() < 20.0 && day.rect().height() < 20.0),
        "the calendar has today's cell"
    );
    assert!(h.query_all_by_label_contains(": 15 min 0 sec · 2 pages · 1 book").next().is_some(), "with its reading");
    assert!(h.query_by_label("Current streak 3 days").is_some(), "today and the two days before");
    assert!(h.query_by_label("Longest streak 4 days, all time").is_some());

    // One tab stop: arrows move the day, Enter pins its card, Esc closes the card, not the sheet.
    h.get_by_label_contains("Reading calendar.").focus();
    h.run_steps(2);
    h.key_press(Key::ArrowUp);
    h.run_steps(2);
    assert!(
        h.get_by_label_contains("Reading calendar.")
            .accesskit_node()
            .label()
            .is_some_and(|label| label.ends_with(": 20 min 0 sec · 1 page · 1 book"))
    );
    h.key_press(Key::Enter);
    h.run_steps(2);
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(h.state().margin_stats.sheet_open(), "Esc closed the pinned card only");

    scroll_sheet_down(&mut h);
    let top = |h: &Harness<'static, PdfCraftApp>, title: &str| h.get_by_label(title).rect().top();
    assert!(top(&h, "Beta") < top(&h, "Alpha"), "most time first: Beta 45m, Alpha 29m");
    h.get_by_label("Sort by Title").click_accesskit();
    h.run_steps(3);
    assert!(top(&h, "Alpha") < top(&h, "Beta"), "sorted by title");
    h.get_by_label("Sort by Title").click_accesskit();
    h.run_steps(3);
    assert!(top(&h, "Beta") < top(&h, "Alpha"), "and back, Z to A");
    assert!(!h.get_by_label("Export CSV").accesskit_node().is_disabled(), "visits to export");
}

#[test]
fn the_stats_sheet_without_history_and_with_recording_off() {
    let place = Place::new("stats-empty");
    let settings = serde_json::json!({ "stats": { "enabled": false } });
    std::fs::write(place.0.join("config").join("settings.jsonc"), settings.to_string()).unwrap();
    let mut h = harness(app(&place));
    assert!(pdfcraft_ui_egui::margin::run(h.state_mut(), "app.stats", &serde_json::Value::Null));
    h.run_steps(4);
    assert!(h.query_by_label("Your reading history starts here").is_some(), "the empty state, not an empty chart");
    assert!(h.query_by_label_contains("Today: 0m.").is_some(), "the cards stay, at zero");
    assert!(h.query_by_label("Current streak 0 days").is_some());
    assert!(h.get_by_label("Export CSV").accesskit_node().is_disabled(), "nothing to export");
    assert!(h.query_by_label("Recording is off").is_some(), "stats.enabled = false says so");
    h.get_by_label("Open PDF");
    h.get_by_label("Close").click();
    h.run_steps(3);
    assert!(!h.state().margin_stats.sheet_open());
}

#[test]
fn library_keys_toggle_the_library_and_the_sheet_lists_them() {
    let place = Place::new("keys");
    let path = place.add("doc.pdf", &pdf("Doc", 2));
    let mut app = app(&place);
    app.open_path(&path);
    let mut h = harness(app);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::O);
    h.run_steps(3);
    assert!(h.state().active.is_none(), "Ctrl+Shift+O shows the library");
    assert!(h.query_all_by_label("Library").next().is_some());
    // `/` in the library focuses its search field.
    h.key_press(Key::Slash);
    h.run_steps(2);
    h.event(egui::Event::Text("zzz".into()));
    h.run_steps(2);
    assert_eq!(h.state().margin_library.query, "zzz");
    assert!(h.query_by_label("No book matches the search.").is_some());
    h.state_mut().margin_library.query.clear();
    h.key_press(Key::Escape);
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::O);
    h.run_steps(3);
    assert_eq!(h.state().active, Some(0), "and again goes back to the book");

    h.key_press(Key::Questionmark);
    h.run_steps(2);
    assert!(h.state().margin_keys.sheet_open());
    assert!(h.query_all_by_label("Reading stats").next().is_some(), "the keyboard sheet lists app.stats");
    assert!(h.query_all_by_label("Library").next().is_some(), "and library.open");
}

#[test]
fn context_menu_actions_change_the_library() {
    let place = Place::new("menu");
    let path = place.add("doc.pdf", &pdf("Menu Book", 3));
    let mut app = app(&place);
    app.open_path(&path);
    app.close_tab(0);
    let mut h = harness(app);
    h.run_steps(4);
    let book = h.state().margin_library.books().first().cloned().expect("listed on open");
    assert!(!book.finished);
    h.get_by_label("Menu Book").click_secondary();
    h.run_steps(2);
    h.get_by_label("Mark as finished").click();
    h.run_steps(3);
    let book = h.state().margin_library.books().first().cloned().unwrap();
    assert!(book.finished, "marked finished");
    let sidecar: serde_json::Value = serde_json::from_slice(&std::fs::read(format!("{path}.margin.json")).expect("sidecar")).unwrap();
    assert_eq!(sidecar["progress"]["finished"], true, "kept in the sidecar too");

    h.get_by_label("Menu Book").click_secondary();
    h.run_steps(2);
    h.get_by_label_contains("Add to collection").hover();
    h.run_steps(2);
    h.get_by_label("New collection").click();
    h.run_steps(3);
    assert_eq!(h.state().margin_library.collections().len(), 1);
    assert_eq!(h.state().margin_library.collections()[0].count, 1);

    h.get_by_label("Menu Book").click_secondary();
    h.run_steps(2);
    h.get_by_label("Show in folder").click();
    h.run_steps(2);
    h.get_by_label("Menu Book").click_secondary();
    h.run_steps(2);
    h.get_by_label("Remove from library").click();
    h.run_steps(3);
    assert!(h.state().margin_library.books().is_empty(), "removed");
    assert!(Path::new(&path).exists(), "the file stays on disk");
}

/// The text field showing `placeholder`.
fn field<'a>(h: &'a Harness<'static, PdfCraftApp>, placeholder: &'a str) -> egui_kittest::Node<'a> {
    h.query_all_by(|node| node.placeholder() == Some(placeholder)).next().unwrap_or_else(|| panic!("no field {placeholder:?}"))
}

/// Steps until `done` holds or `limit` passes.
fn step_until(h: &mut Harness<'static, PdfCraftApp>, limit: Duration, done: impl Fn(&Harness<'static, PdfCraftApp>) -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        h.run_steps(1);
        if done(h) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn the_details_sheet_tags_and_edits_a_book_and_in_text_search_opens_a_page() {
    let place = Place::new("details-text");
    let path = place.add("tagged.pdf", &pdf("Tagged Book", 3));
    place.add("other.pdf", &pdf("Other Book", 2));
    place.watch_books();
    let mut h = harness(app(&place));
    wait_for_scan(&mut h, 2, Instant::now(), SCAN_BUDGET);

    // Details: a tag and a year, saved as the user's.
    h.get_by_label("Tagged Book").click_secondary();
    h.run_steps(2);
    h.get_by_label("Details\u{2026}").click();
    h.run_steps(3);
    assert!(h.state().margin_library.sheet_open());
    field(&h, "Add a tag").focus();
    h.run_steps(1);
    field(&h, "Add a tag").type_text("Physics");
    h.key_press(Key::Enter);
    h.run_steps(2);
    field(&h, "2024").focus();
    h.run_steps(1);
    field(&h, "2024").type_text("1999");
    h.run_steps(1);
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert!(!h.state().margin_library.sheet_open(), "saving closes the sheet");
    let book = h.state().margin_library.books().iter().find(|book| book.title == "Tagged Book").cloned().expect("listed");
    assert_eq!(book.year, Some(1999));
    let tags = h.state().margin_library.tags().to_vec();
    assert_eq!(tags.iter().map(|tag| tag.name.as_str()).collect::<Vec<_>>(), ["Physics"]);
    assert_eq!(book.tags, [tags[0].id]);
    let details = h.state().margin_library.db().unwrap().details(book.id).unwrap();
    assert_ne!(details.user_fields, 0, "the year is the user's now");

    // The search finds a book by its tag.
    h.state_mut().margin_library.query = "physics".into();
    h.run_steps(2);
    assert!(h.query_all_by_label("Tagged Book").next().is_some());
    assert!(h.query_all_by_label("Other Book").next().is_none(), "only the tagged book matches");

    // In text: the pages' words, once the background pass read them; a hit opens its page.
    h.state_mut().margin_library.in_text = true;
    h.state_mut().margin_library.query = "margin".into();
    let found = step_until(&mut h, SCAN_BUDGET, |h| h.query_all_by_label("Tagged Book, page 2").next().is_some());
    assert!(found, "the text of every page is searchable");
    h.get_by_label("Tagged Book, page 2").click();
    assert!(step_until(&mut h, SCAN_BUDGET, |h| h.state().active.is_some()), "the book opened");
    let app = h.state();
    let index = app.active.expect("the book is shown");
    assert_eq!(app.session.get(app.views[index].id).and_then(|doc| doc.path.as_deref()), Some(path.as_str()));
    assert_eq!(app.views[index].current, 1, "at the page of the hit");
}

/// Scan and grid timings on the perf corpus (`cargo xtask perf --gen-library` in the margin
/// repo): `MARGIN_LIBRARY_CORPUS=<dir with PDFs> cargo test --test margin_library -- --ignored`.
#[test]
#[ignore = "needs MARGIN_LIBRARY_CORPUS"]
fn perf_corpus_scan_and_grid() {
    let Some(corpus) = std::env::var_os("MARGIN_LIBRARY_CORPUS") else { return };
    let place = Place::new("corpus");
    let settings = serde_json::json!({ "library": { "folders": [PathBuf::from(&corpus)] } });
    std::fs::write(place.0.join("config").join("settings.jsonc"), settings.to_string()).unwrap();
    let start = Instant::now();
    let mut h = harness(app(&place));
    let cold = wait_for_scan(&mut h, 1, start, Duration::from_secs(600));
    let count = h.state().margin_library.books().len();
    eprintln!("cold scan with covers: {count} PDFs in {cold:?}");
    let frames = 30;
    let start = Instant::now();
    for _ in 0..frames {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -400.0),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        });
        h.run_steps(1);
    }
    eprintln!("grid of {count}: {:?} per scrolled frame", start.elapsed() / frames);
    drop(h);
    let start = Instant::now();
    let mut h = harness(app(&place));
    let warm = wait_for_scan(&mut h, count, start, Duration::from_secs(600));
    eprintln!("rescan of {count} unchanged PDFs: {warm:?}");
}
