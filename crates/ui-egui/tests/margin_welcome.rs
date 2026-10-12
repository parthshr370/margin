//! The library's setup card in the real shell: on a first run it offers the vault, a library
//! folder and Open; picking writes `notes.vault` and `library.folders` into `settings.jsonc` and
//! moves the card on to the next step; its close button and "Not now" are written to disk at
//! once. The system's folder panel is answered through `PdfCraftApp::pick_override`, the hook
//! automation and tests use instead of a native panel.

use std::path::PathBuf;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};

/// A temporary root with `config/`, `data/`, `vault/.obsidian` and `books/`; removed on drop.
struct Place(PathBuf);

impl Place {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("margin-welcome-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        for dir in ["config", "data", "vault/.obsidian", "books"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        Self(root)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn settings(&self) -> margin_config::Settings {
        margin_config::Settings::load(&self.path("config")).settings
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn harness(place: &Place, state: Option<String>) -> Harness<'static, PdfCraftApp> {
    let (config, data) = (place.path("config"), place.path("data"));
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 860.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.set_option("language", "en").unwrap();
        app.margin_keys.set_config_dir(config);
        app.margin_keys.set_data_dir(data);
        if let Some(state) = state {
            app.restore(&state);
        }
        app
    });
    h.run_steps(6);
    h
}

/// Clicks `label` with `picked` as the folder panel's answer, then lets the pick arrive.
fn pick(h: &mut Harness<'static, PdfCraftApp>, label: &str, picked: &std::path::Path) {
    h.state_mut().pick_override = Some(vec![picked.to_string_lossy().into_owned()]);
    h.get_by_label(label).click();
    h.run_steps(6);
}

fn evidence(h: &mut Harness<'static, PdfCraftApp>, name: &str) {
    if let Some(folder) = std::env::var_os("MARGIN_NOTES_EVIDENCE_DIR").map(PathBuf::from) {
        std::fs::create_dir_all(&folder).unwrap();
        h.render().unwrap().save(folder.join(name)).unwrap();
    }
}

#[test]
fn first_run_card_connects_the_vault_and_a_library_folder() {
    let place = Place::new("connect");
    let mut h = harness(&place, None);
    assert!(h.query_by_label("Set up Margin").is_some(), "a first run shows the setup card");
    assert!(h.query_by_label("Open…").is_some(), "Open a PDF is offered");
    evidence(&mut h, "setup-card.png");

    pick(&mut h, "Choose vault…", &place.path("vault"));
    assert_eq!(place.settings().notes.vault, Some(place.path("vault")));
    assert!(h.query_by_label("Choose vault…").is_none(), "the vault step is done");
    assert!(h.query_by_label("Disconnect").is_none(), "onboarding only moves forward: no Disconnect");
    assert!(h.query_by_label("Set up Margin").is_some(), "the card stays for the folder step");

    pick(&mut h, "Add folder…", &place.path("books"));
    assert_eq!(place.settings().library.folders, [place.path("books").to_string_lossy().into_owned()]);
    let written = std::fs::read_to_string(place.path("config").join(margin_config::SETTINGS_FILE)).unwrap();
    assert!(written.contains("//"), "the commented settings file is kept: {written}");
    if let Some(folder) = std::env::var_os("MARGIN_NOTES_EVIDENCE_DIR").map(PathBuf::from) {
        std::fs::write(folder.join("setup-settings.jsonc"), &written).unwrap();
    }
    // The card folds away over the slide animation.
    h.run_steps(30);
    assert!(h.query_by_label("Set up Margin").is_none(), "everything is set: the card is gone");
}

#[test]
fn not_now_hides_the_card_across_restarts() {
    let place = Place::new("dismiss");
    let mut h = harness(&place, None);
    h.get_by_label("Not now").click();
    h.run_steps(30);
    assert!(h.query_by_label("Set up Margin").is_none());
    let state = h.state().persist();
    drop(h);
    let mut h = harness(&place, Some(state));
    h.run_steps(2);
    assert!(h.query_by_label("Set up Margin").is_none(), "dismissed stays dismissed");
    assert!(h.query_by_label("Add a library folder…").is_some(), "the empty library still offers a folder");
}

#[test]
fn the_close_button_hides_the_card_even_without_the_app_state() {
    let place = Place::new("close");
    let mut h = harness(&place, None);
    h.get_by_label("Close setup").click();
    h.run_steps(30);
    assert!(h.query_by_label("Set up Margin").is_none());
    drop(h);
    // No eframe state is handed over: the dismissal was written to the data folder at once.
    let mut h = harness(&place, None);
    h.run_steps(2);
    assert!(h.query_by_label("Set up Margin").is_none(), "the close is remembered on disk");
}

#[test]
fn a_folder_without_obsidian_settings_is_used_with_a_warning() {
    let place = Place::new("plain-folder");
    let mut h = harness(&place, None);
    pick(&mut h, "Choose vault…", &place.path("books"));
    assert_eq!(place.settings().notes.vault, Some(place.path("books")));
    let said = h.state().toast.as_ref().map(|toast| toast.0.clone()).unwrap_or_default();
    assert!(said.contains("no .obsidian folder"), "{said}");
}
