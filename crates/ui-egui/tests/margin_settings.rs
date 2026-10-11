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
    fixture_pages(1)
}

fn fixture_pages(pages: usize) -> Vec<u8> {
    let kids = (0..pages).map(|page| format!("{} 0 R", page + 3)).collect::<Vec<_>>().join(" ");
    let mut objects =
        vec!["<< /Type /Catalog /Pages 2 0 R >>".to_owned(), format!("<< /Type /Pages /Kids [{kids}] /Count {pages} /MediaBox [0 0 200 300] >>")];
    objects.extend((0..pages).map(|_| "<< /Type /Page /Parent 2 0 R >>".to_owned()));
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

#[test]
fn reader_settings_apply_to_real_views_and_replace_the_rail() {
    let config = config_dir("runtime-consumers");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join(margin_config::SETTINGS_FILE),
        r#"{
        "perf": { "tile_px": 1024 },
        "view": { "layout": "single", "page_gap_px": 31 },
        "reader": { "back_stack": 1, "back_pill": false, "time_left": false,
                    "find": { "case": true, "whole_word": true } },
        "ui": { "rail": { "side": "left", "items": ["reader.tool.select", "comment.highlight"] } },
        "stats": { "enabled": false }
    }"#,
    )
    .unwrap();
    let mut h = margin_harness(config.clone());
    h.state_mut().dialog = None;
    h.state_mut().open_bytes("multiple.pdf", None, fixture_pages(3)).unwrap();
    h.run_steps(4);
    let view = h.state_mut().views.last_mut().unwrap();
    assert_eq!(view.page_gap_px, Some(31), "the effective document setting reaches canvas geometry");
    view.open_find();
    assert!(view.find.as_ref().is_some_and(|find| find.case_sensitive && find.whole_words));
    view.go_to_page(1);
    view.go_to_page(2);
    assert_eq!(view.back.iter().map(|place| place.page).collect::<Vec<_>>(), [1]);
    h.run_steps(4);
    h.get_by_label("Select");
    h.get_by_label("Highlight text");
    assert!(h.query_by_label("Area snapshot: copy a region of the page").is_none(), "configured items replace, not append");
    assert!(h.query_by_label("Underline text").is_none());
    assert!(h.query_by_label("Back to p. 2").is_none(), "back_pill false suppresses the offer but not history");
    assert_eq!(h.state().views.last().unwrap().back.len(), 1);
    std::fs::write(config.join(margin_config::SETTINGS_FILE), r#"{ "ui": { "rail": { "items": [] } } }"#).unwrap();
    h.state_mut().margin_keys.set_config_dir(config.clone());
    h.run_steps(4);
    assert!(h.query_by_label("Select").is_none(), "an empty rail has no floating toolbar");
    assert!(h.query_by_label("Highlight text").is_none());
    std::fs::remove_dir_all(config).unwrap();
}
