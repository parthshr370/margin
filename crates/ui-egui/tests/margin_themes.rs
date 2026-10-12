//! Margin's themes: settings.jsonc's `theme` picks a built-in or user theme, theme files in
//! `<config>` lay over the built-ins, the system's appearance drives `system` mode, the Settings
//! window writes `theme`, and every change applies live, without a restart.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::Color32;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::theme::{ThemeKind, ThemePreference, Tokens};
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};

/// How long a change on disk may take to arrive through the file watch.
const WATCH_TIMEOUT: Duration = Duration::from_secs(10);
/// The wait between frames while a change is on its way.
const FRAME_PAUSE: Duration = Duration::from_millis(20);

/// Margin Dark's black and orange, and Margin Light's base.
const BLACK: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
const ORANGE: Color32 = Color32::from_rgb(0xFF, 0x5F, 0x1F);
const LIGHT_BASE: Color32 = Color32::from_rgb(0xF4, 0xF4, 0xF4);
const BLUE: Color32 = Color32::from_rgb(0x4B, 0x8B, 0xF5);

/// An empty config folder for test `name`.
fn config_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("margin-themes-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A Margin app on `config`, the system dark.
fn margin_harness(config: &Path) -> Harness<'static, PdfCraftApp> {
    let config = config.to_path_buf();
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        app.set_theme_preference(ThemePreference::Dark);
        app.margin_keys.set_config_dir(config);
        app.margin_keys.set_data_dir(std::env::temp_dir().join(format!("margin-themes-data-{}", std::process::id())));
        app.margin_keys.disable_editor();
        app
    });
    h.input_mut().system_theme = Some(egui::Theme::Dark);
    h.run_steps(4);
    h
}

fn tokens(h: &Harness<'static, PdfCraftApp>) -> Tokens {
    Tokens::get(&h.ctx)
}

fn toast(h: &Harness<'static, PdfCraftApp>) -> String {
    h.state().toast.as_ref().map(|toast| toast.0.clone()).unwrap_or_default()
}

/// Runs frames until `done`, as the file watch delivers a save.
fn run_until(h: &mut Harness<'static, PdfCraftApp>, what: &str, done: impl Fn(&Harness<'static, PdfCraftApp>) -> bool) {
    let deadline = Instant::now() + WATCH_TIMEOUT;
    while !done(h) {
        assert!(Instant::now() < deadline, "{what}: not applied within {WATCH_TIMEOUT:?}");
        std::thread::sleep(FRAME_PAUSE);
        h.run_steps(1);
    }
}

/// With nothing set, Margin draws Margin Dark: the Black palette, as before themes. A theme in
/// themes.json that extends it and a `theme` naming it, both saved while Margin runs, recolour the
/// accent and the background on the next frames; the rest stays Margin Dark's.
#[test]
fn a_user_theme_extending_margin_dark_applies_live() {
    let config = config_dir("live");
    let mut h = margin_harness(&config);
    let t = tokens(&h);
    assert_eq!((t.kind, t.dark()), (ThemeKind::Margin, true));
    assert_eq!((t.chrome, t.pasteboard, t.accent, t.surfaces.focus), (BLACK, BLACK, ORANGE, ORANGE));
    assert!(h.ctx.global_style().visuals.dark_mode);
    assert!(config.join(margin_config::THEMES_FILE).is_file(), "the first run writes themes.json");
    assert!(config.join("schemas").join(margin_config::THEME_SCHEMA_FILE).is_file(), "and the theme schema");
    assert!(config.join("default-settings.jsonc").is_file(), "and the defaults");

    let family = r##"{ "themes": [ { "name": "Mine", "appearance": "dark", "extends": "Margin Dark",
        "style": { "text.accent": "#4b8bf5", "background": "#101010" } } ] }"##;
    std::fs::write(config.join(margin_config::THEMES_FILE), family).unwrap();
    std::fs::write(config.join(margin_config::SETTINGS_FILE), r#"{ "theme": "Mine" }"#).unwrap();
    run_until(&mut h, "the theme Mine", |h| tokens(h).accent == BLUE);
    let t = tokens(&h);
    assert_eq!((t.chrome, t.titlebar, t.surfaces.status_bar), (Color32::from_gray(0x10), Color32::from_gray(0x10), Color32::from_gray(0x10)));
    assert_eq!((t.accent_text, t.surfaces.heading, t.surfaces.selected_text, t.surfaces.focus), (BLUE, BLUE, BLUE, BLUE));
    assert_eq!(t.panel, Color32::from_gray(0x0A), "what Mine leaves out is Margin Dark's");

    // Editing the theme file alone applies too.
    std::fs::write(config.join(margin_config::THEMES_FILE), family.replace("#4b8bf5", "#22aa55")).unwrap();
    run_until(&mut h, "the edited accent", |h| tokens(h).accent == Color32::from_rgb(0x22, 0xAA, 0x55));
    std::fs::remove_dir_all(&config).ok();
}

/// The Settings window's Theme row writes `theme.mode`: Light switches the whole UI to Margin
/// Light at once, keeping settings.jsonc's comments; a dark theme picked from the dropdown shows
/// in dark mode.
#[test]
fn the_settings_window_switches_the_mode() {
    let config = config_dir("window");
    std::fs::write(config.join(margin_config::SETTINGS_FILE), "{\n  // mine\n  \"view\": { \"zoom\": 120 }\n}\n").unwrap();
    let mut h = margin_harness(&config);
    h.state_mut().set_option("dialog", "preferences").unwrap();
    h.run_steps(3);
    // Appearance is the last section: scroll the body to its end.
    const SCROLL_TO_END: f32 = -4000.0;
    h.hover_at(h.get_by_role_and_label(Role::ComboBox, "Interface language").rect().center());
    h.run_steps(1);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, SCROLL_TO_END),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(3);

    h.get_by_label("Light").click();
    h.run_steps(3);
    let written = std::fs::read_to_string(config.join(margin_config::SETTINGS_FILE)).unwrap();
    assert!(written.contains("// mine") && written.contains("\"mode\": \"light\""), "{written}");
    let t = tokens(&h);
    assert!(!t.dark());
    assert_eq!((t.chrome, t.accent), (LIGHT_BASE, ORANGE));
    assert!(!h.ctx.global_style().visuals.dark_mode);

    h.get_by_label("Dark").click();
    h.run_steps(3);
    assert!(tokens(&h).dark());
    assert_eq!(tokens(&h).chrome, BLACK);

    // The dark theme's dropdown lists the built-ins and the user's themes.
    h.get_by_role_and_label(Role::ComboBox, "Dark theme").click();
    h.run_steps(2);
    h.get_by_label("Example Dark").click();
    h.run_steps(3);
    let written = std::fs::read_to_string(config.join(margin_config::SETTINGS_FILE)).unwrap();
    assert!(written.contains("\"dark\": \"Example Dark\""), "{written}");
    assert_eq!(tokens(&h).accent, BLUE, "the first-run example theme's accent");
    std::fs::remove_dir_all(&config).ok();
}

/// `system` mode follows the system's appearance as it changes.
#[test]
fn system_mode_follows_the_system() {
    let config = config_dir("system");
    std::fs::write(config.join(margin_config::SETTINGS_FILE), r#"{ "theme": { "mode": "system" } }"#).unwrap();
    let mut h = margin_harness(&config);
    assert!(tokens(&h).dark());
    h.input_mut().system_theme = Some(egui::Theme::Light);
    h.run_steps(2);
    assert!(!tokens(&h).dark());
    assert_eq!(tokens(&h).chrome, LIGHT_BASE);
    h.input_mut().system_theme = Some(egui::Theme::Dark);
    h.run_steps(2);
    assert_eq!(tokens(&h).chrome, BLACK);
    std::fs::remove_dir_all(&config).ok();
}

/// A bad colour in a theme file is reported with the file, line and key; the rest of the theme
/// applies and the bad key keeps the parent's value.
#[test]
fn a_bad_style_value_is_reported_and_falls_back() {
    let config = config_dir("bad");
    let family = "{\n  \"themes\": [\n    { \"name\": \"Mine\", \"appearance\": \"dark\", \"style\": {\n      \"background\": \"#101010\",\n      \"text.accent\": \"blue\"\n    } }\n  ]\n}\n";
    std::fs::write(config.join(margin_config::THEMES_FILE), family).unwrap();
    std::fs::write(config.join(margin_config::SETTINGS_FILE), r#"{ "theme": "Mine" }"#).unwrap();
    let h = margin_harness(&config);
    let notice = toast(&h);
    assert!(notice.starts_with("themes.json:5:7 ") && notice.contains("text.accent \"blue\" is not a colour"), "{notice}");
    let t = tokens(&h);
    assert_eq!((t.chrome, t.accent), (Color32::from_gray(0x10), ORANGE));
    std::fs::remove_dir_all(&config).ok();
}

/// A Zed theme family dropped into themes/ loads with no problem: Zed's keys Margin has no role
/// for are accepted, and the keys it maps colour Margin.
#[test]
fn a_zed_theme_file_loads_as_it_is() {
    let config = config_dir("zed");
    std::fs::create_dir_all(config.join(margin_config::THEMES_DIR)).unwrap();
    let one = r##"{
  "$schema": "https://zed.dev/schema/themes/v0.2.0.json",
  "name": "One",
  "author": "Zed Industries",
  "themes": [
    {
      "name": "One Dark",
      "appearance": "dark",
      "style": {
        "border": "#464b57ff",
        "border.variant": "#363c46ff",
        "background": "#3b414dff",
        "editor.background": "#282c33ff",
        "panel.background": "#2f343eff",
        "text": "#dce0e5ff",
        "text.accent": "#74ade8ff",
        "panel.focused_border": null,
        "terminal.ansi.red": "#e06c75ff",
        "background.appearance": "opaque",
        "accents": [],
        "players": [{ "cursor": "#74ade8ff", "background": "#74ade8ff", "selection": "#74ade83d" }],
        "syntax": { "comment": { "color": "#5d636fff", "font_style": null, "font_weight": null } }
      }
    }
  ]
}"##;
    std::fs::write(config.join(margin_config::THEMES_DIR).join("one.json"), one).unwrap();
    std::fs::write(config.join(margin_config::SETTINGS_FILE), r#"{ "theme": "One Dark" }"#).unwrap();
    let h = margin_harness(&config);
    assert_eq!(toast(&h), "", "no problem reported");
    let t = tokens(&h);
    assert_eq!(
        (t.chrome, t.pasteboard, t.accent),
        (Color32::from_rgb(0x3B, 0x41, 0x4D), Color32::from_rgb(0x28, 0x2C, 0x33), Color32::from_rgb(0x74, 0xAD, 0xE8))
    );
    std::fs::remove_dir_all(&config).ok();
}

/// The palette's "Open themes file" makes themes.json from its template when it is missing and
/// opens it.
#[test]
fn open_themes_file_makes_it_when_missing() {
    let config = config_dir("open");
    let mut h = margin_harness(&config);
    std::fs::remove_file(config.join(margin_config::THEMES_FILE)).unwrap();
    assert!(pdfcraft_ui_egui::margin::run(h.state_mut(), "themes.open", &serde_json::Value::Null));
    h.run_steps(1);
    let path = config.join(margin_config::THEMES_FILE);
    assert_eq!(h.state().margin_keys.last_opened_file(), Some(path.as_path()));
    assert!(std::fs::read_to_string(&path).unwrap().contains("\"Example Dark\""));
    std::fs::remove_dir_all(&config).ok();
}
