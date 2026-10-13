//! Margin's own title bar on Linux (`margin::window_chrome`): with the integrated title bar on,
//! the top row (the reader's header or the Library's bar) ends in Minimize, Maximize and Close,
//! which send the window's viewport commands, and the row's own buttons make room for them. Focus
//! mode hides them with the header, `ui.titlebar: "system"` gives the window back to the desktop
//! (at once, without a restart), and PdfCraft never draws them.
#![cfg(target_os = "linux")]

use egui::ViewportCommand;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};

const WINDOW_BUTTONS: [&str; 3] = ["Minimize", "Maximize", "Close"];
/// The reader's window; the Library's narrower one makes its bar reach under the buttons.
const READER_WINDOW: egui::Vec2 = egui::vec2(1440.0, 900.0);
const NARROW_WINDOW: egui::Vec2 = egui::vec2(1000.0, 800.0);
/// Rounding between egui's layout and AccessKit's rects.
const SAME_EDGE: f32 = 1.0;
/// Frames a click gets to reach the button.
const CLICK_FRAMES: usize = 3;

/// One 200×300 page.
fn fixture() -> Vec<u8> {
    let objects =
        ["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>", "<< /Type /Page /Parent 2 0 R >>"];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    out
}

/// What a test's app starts with.
struct Setup {
    flavor: Flavor,
    /// Names the test's config and data folders.
    name: &'static str,
    /// `settings.jsonc`, if any.
    settings: Option<&'static str>,
    /// `reader.pdf` open, else the Library shows.
    document: bool,
    window: egui::Vec2,
}

impl Setup {
    fn reader(name: &'static str) -> Self {
        Self { flavor: Flavor::Margin, name, settings: None, document: true, window: READER_WINDOW }
    }
}

/// A test's config folder, `settings.jsonc` in it.
fn config_dir(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("margin-window-{name}-{}", std::process::id()))
}

/// The app of `setup` with the integrated title bar on, as `apps/margin` sets it on Linux.
fn harness(setup: Setup) -> Harness<'static, PdfCraftApp> {
    let config = config_dir(setup.name);
    std::fs::remove_dir_all(&config).ok();
    std::fs::create_dir_all(&config).expect("temp config dir");
    if let Some(text) = setup.settings {
        std::fs::write(config.join(margin_config::SETTINGS_FILE), text).expect("settings written");
    }
    let data = config.join("data");
    let mut h = Harness::builder().with_size(setup.window).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(setup.flavor);
        app.set_option("language", "en").unwrap();
        app.margin_keys.set_config_dir(config.clone());
        app.margin_keys.set_data_dir(data.clone());
        app.integrated_titlebar = true;
        if setup.document {
            app.open_bytes("reader.pdf", None, fixture()).expect("opens");
        }
        app
    });
    h.run_steps(6);
    h
}

/// The rect of the window button named `label`: the right-most widget with that name, so a
/// sheet's own Close never stands in for it.
fn window_button(h: &Harness<'static, PdfCraftApp>, label: &str) -> egui::Rect {
    h.get_all_by_label(label).map(|node| node.rect()).max_by(|a, b| a.right().total_cmp(&b.right())).unwrap_or_else(|| panic!("no {label} button"))
}

fn sent(h: &Harness<'static, PdfCraftApp>, wanted: &ViewportCommand) -> bool {
    h.output().viewport_output.values().any(|viewport| viewport.commands.contains(wanted))
}

/// Clicks the window button named `label` and reports whether a frame sent `wanted`.
fn click_sends(h: &mut Harness<'static, PdfCraftApp>, label: &str, wanted: &ViewportCommand) -> bool {
    let target = window_button(h, label).center();
    h.hover_at(target);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: target, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    }
    (0..CLICK_FRAMES).any(|_| {
        h.step();
        sent(h, wanted)
    })
}

#[test]
fn the_header_ends_in_window_buttons_that_drive_the_window() {
    let mut h = harness(Setup::reader("reader"));
    let rects = WINDOW_BUTTONS.map(|label| window_button(&h, label));
    let [minimize, maximize, close] = rects;
    assert!(minimize.right() <= maximize.left() + SAME_EDGE && maximize.right() <= close.left() + SAME_EDGE, "left to right: {rects:?}");
    assert!((close.right() - READER_WINDOW.x).abs() <= SAME_EDGE, "flush with the window's right edge: {close:?}");
    assert!(rects.iter().all(|rect| rect.top().abs() <= SAME_EDGE), "in the header row: {rects:?}");
    let search = h.get_by_label_contains("Find in document").rect();
    assert!(search.right() <= minimize.left(), "the header's buttons make room: {search:?} vs {minimize:?}");

    assert!(click_sends(&mut h, "Maximize", &ViewportCommand::Maximized(true)));
    assert!(click_sends(&mut h, "Minimize", &ViewportCommand::Minimized(true)));
    assert!(click_sends(&mut h, "Close", &ViewportCommand::Close), "Close asks to close, as the desktop's button does");
}

#[test]
fn the_library_bar_makes_room_for_the_window_buttons() {
    let h = harness(Setup { name: "library", document: false, window: NARROW_WINDOW, ..Setup::reader("") });
    let minimize = window_button(&h, "Minimize");
    window_button(&h, "Close");
    let menu = h.get_by_label("Margin: open, recent, settings, shortcuts").rect();
    assert!(menu.right() <= minimize.left(), "the bar's last button stays clear: {menu:?} vs {minimize:?}");
    assert!((menu.center().y - minimize.center().y).abs() <= SAME_EDGE, "the bar shares the buttons' row");
}

#[test]
fn focus_mode_hides_the_window_buttons_with_the_header() {
    let mut h = harness(Setup::reader("focus"));
    window_button(&h, "Close");
    h.state_mut().margin_chrome.focus_mode = true;
    h.run_steps(3);
    for label in WINDOW_BUTTONS {
        assert!(h.query_by_label(label).is_none(), "{label} hides in focus mode");
    }
}

#[test]
fn the_system_title_bar_setting_and_pdfcraft_draw_no_window_buttons() {
    let system = harness(Setup { name: "system", settings: Some(r#"{ "ui": { "titlebar": "system" } }"#), ..Setup::reader("") });
    let pdfcraft = harness(Setup { flavor: Flavor::PdfCraft, name: "pdfcraft", ..Setup::reader("") });
    for h in [&system, &pdfcraft] {
        for label in WINDOW_BUTTONS {
            assert!(h.query_by_label(label).is_none(), "{label} is the desktop's");
        }
    }
}

/// Turning `ui.titlebar` to "system" while Margin runs asks the desktop for its decorations and
/// drops Margin's buttons; back to "margin" drops the decorations again.
#[test]
fn the_titlebar_setting_applies_without_a_restart() {
    let mut h = harness(Setup::reader("switch"));
    window_button(&h, "Close");
    let config = config_dir("switch");
    for (text, decorated) in [(r#"{ "ui": { "titlebar": "system" } }"#, true), (r#"{ "ui": { "titlebar": "margin" } }"#, false)] {
        std::fs::write(config.join(margin_config::SETTINGS_FILE), text).expect("settings written");
        h.state_mut().margin_keys.set_config_dir(config.clone());
        let asked = (0..CLICK_FRAMES).any(|_| {
            h.step();
            sent(&h, &ViewportCommand::Decorations(decorated))
        });
        assert!(asked, "{text}: decorations {decorated}");
        h.run_steps(2);
        assert_eq!(h.query_by_label("Minimize").is_some(), !decorated, "{text}: Margin's buttons");
    }
}
