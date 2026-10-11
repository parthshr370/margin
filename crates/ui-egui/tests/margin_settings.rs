//! Margin's Settings window: Preferences in the Margin flavour are Margin's modal, its rows write
//! PdfCraft's preference fields (and Margin's `settings.jsonc` for the document defaults), and
//! Esc closes it.

use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::canvas::PageLayout;
use pdfcraft_ui_egui::theme::ThemePreference;
use pdfcraft_ui_egui::{Dialog, Flavor, PdfCraftApp};

/// One 200×300 page.
fn fixture() -> Vec<u8> {
    let objs =
        ["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>", "<< /Type /Page /Parent 2 0 R >>"];
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

/// A temporary config folder for test `name`, empty.
fn config_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("margin-settings-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    dir
}

/// A Margin app on `config`, the fixture open (from bytes: no sidecar), Preferences open.
fn margin_harness(config: std::path::PathBuf) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        app.set_theme_preference(ThemePreference::Dark);
        app.margin_keys.set_config_dir(config);
        app.margin_keys.set_data_dir(std::env::temp_dir().join(format!("margin-settings-data-{}", std::process::id())));
        app.open_bytes("doc.pdf", None, fixture()).unwrap();
        app.set_option("dialog", "preferences").unwrap();
        app
    });
    h.input_mut().system_theme = Some(egui::Theme::Dark);
    h.run_steps(6);
    h
}

#[test]
fn settings_rows_write_the_preferences() {
    let config = config_dir("rows");
    let mut h = margin_harness(config.clone());
    assert!(h.query_by_label("Settings").is_some(), "Margin's window, titled Settings");
    assert!(h.query_by_label("Preferences").is_none(), "not PdfCraft's dialog");
    assert!(h.query_by_label("Open in").is_none(), "no workspace mode row: Margin never shows it");

    // Pages: Two-up goes to settings.jsonc and the next document opens in it; PdfCraft's own
    // default stays as it was.
    let before = h.state().view_defaults;
    h.get_by_label("Two-up").click();
    h.run_steps(2);
    assert_eq!(h.state().margin_keys.settings().view.layout, margin_config::Layout::TwoUp);
    let written = std::fs::read_to_string(config.join(margin_config::SETTINGS_FILE)).unwrap();
    assert!(written.contains("\"two_up\""), "{written}");
    assert_eq!(h.state().view_defaults, before);
    h.state_mut().open_bytes("next.pdf", None, fixture()).unwrap();
    h.run_steps(2);
    assert_eq!(h.state().views.last().unwrap().layout, PageLayout::TwoUp);
    h.state_mut().set_option("dialog", "preferences").unwrap();
    h.run_steps(4);

    // Fit: Page.
    h.get_by_label("Page").click();
    h.run_steps(2);
    assert_eq!(h.state().margin_keys.settings().view.fit, margin_config::Fit::Page);

    // Your name: the comment author PdfCraft's Identity row sets, saved with the app's state.
    h.get_by_role_and_label(Role::TextInput, "Your name").click();
    h.run_steps(1);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.get_by_role_and_label(Role::TextInput, "Your name").type_text("Grace Hopper");
    h.run_steps(2);
    assert_eq!(h.state().comment_prefs.author, "Grace Hopper");
    let mut restored = PdfCraftApp::new();
    restored.restore(&h.state().persist());
    assert_eq!(restored.comment_prefs.author, "Grace Hopper");

    // Acrobat JavaScript: the session's switch.
    let javascript = h.state().session.javascript();
    h.get_by_role_and_label(Role::CheckBox, "Acrobat JavaScript").click();
    h.run_steps(2);
    assert_eq!(h.state().session.javascript(), !javascript);

    // Done closes it.
    h.get_by_label("Done").click();
    h.run_steps(2);
    assert_eq!(h.state().dialog, None);
    std::fs::remove_dir_all(&config).ok();
}

#[test]
fn escape_closes_settings() {
    let config = config_dir("escape");
    let mut h = margin_harness(config.clone());
    assert_eq!(h.state().dialog, Some(Dialog::Preferences));
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().dialog, None);
    std::fs::remove_dir_all(&config).ok();
}

/// A Margin app on `config` restored from the app state `state`, after its first frames (which
/// load the settings and migrate the state's notes block).
fn restored_harness(config: std::path::PathBuf, state: serde_json::Value) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.margin_keys.set_config_dir(config);
        app.margin_keys.set_data_dir(std::env::temp_dir().join(format!("margin-settings-data-{}", std::process::id())));
        app.restore(&state.to_string());
        app
    });
    h.run_steps(4);
    h
}

#[test]
fn legacy_notes_migrate_before_app_state_is_stripped() {
    let config = config_dir("notes-migration");
    std::fs::create_dir_all(&config).unwrap();
    let settings_path = config.join(margin_config::SETTINGS_FILE);
    std::fs::write(&settings_path, r#"{"notes":{"vault":"","note_template":""}}"#).unwrap();
    let legacy = serde_json::json!({
        "notes": { "vault": "/legacy-vault", "note_template": "reading/legacy.md", "colors": [["Purple", "#123456"]] }
    });
    let h = restored_harness(config.clone(), legacy);

    let settings = margin_config::Settings::load(&config).settings;
    assert_eq!(settings.notes.vault, None, "an explicitly empty vault disables Obsidian");
    assert_eq!(settings.notes.note_template, "", "an explicitly empty template wins migration");
    assert_eq!(settings.notes.colors.first().map(|color| color.name.as_str()), Some("Purple"));
    let written = std::fs::read_to_string(settings_path).unwrap();
    assert!(written.contains("\"vault\":\"\""), "{written}");
    let state: serde_json::Value = serde_json::from_str(&h.state().persist()).unwrap();
    assert!(state.get("notes").is_none(), "successful migration removes legacy app-state notes");
    std::fs::remove_dir_all(config).ok();
}

#[test]
fn failed_notes_migration_keeps_its_app_state_source() {
    let config = config_dir("notes-migration-error");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join(margin_config::SETTINGS_FILE), "{").unwrap();
    let legacy = serde_json::json!({ "notes": { "vault": "/legacy-vault", "note_template": "reading/legacy.md" } });
    let h = restored_harness(config.clone(), legacy);

    let state: serde_json::Value = serde_json::from_str(&h.state().persist()).unwrap();
    assert_eq!(state["notes"]["vault"], "/legacy-vault");
    assert_eq!(state["notes"]["note_template"], "reading/legacy.md");
    std::fs::remove_dir_all(config).ok();
}
