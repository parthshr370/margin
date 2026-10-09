//! Margin's theme and Display popover: `Flavor::Margin` wears the Margin palette, the popover
//! drives the page colour, and PdfCraft is unchanged.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::margin::display::{self, DisplayState};
use pdfcraft_ui_egui::margin::pagecolor::PageMode;
use pdfcraft_ui_egui::theme::{self, ThemeKind, ThemePreference, Tokens};
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};

fn app_harness(flavor: Flavor) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1100.0, 800.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(flavor);
        app.set_option("language", "en").unwrap();
        app.set_theme_preference(ThemePreference::Dark);
        app
    });
    h.input_mut().system_theme = Some(egui::Theme::Dark);
    h.run_steps(4);
    h
}

#[test]
fn margin_wears_its_palette_where_pdfcraft_is_dark() {
    let h = app_harness(Flavor::Margin);
    assert_eq!(h.state().theme, ThemeKind::Margin);
    assert_eq!(Tokens::get(&h.ctx).kind, ThemeKind::Margin);
    assert!(h.ctx.global_style().visuals.dark_mode);
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);

    let h = app_harness(Flavor::PdfCraft);
    assert_eq!(h.state().theme, ThemeKind::Dark);
    assert_eq!(Tokens::get(&h.ctx).kind, ThemeKind::Dark);
}

#[test]
fn page_color_is_saved_only_for_margin() {
    let mut margin = PdfCraftApp::with_flavor(Flavor::Margin);
    margin.set_option("page_color", "sepia").unwrap();
    let saved: serde_json::Value = serde_json::from_str(&margin.persist()).unwrap();
    assert_eq!(saved[display::SETTINGS_KEY]["color"]["mode"], "sepia");
    let mut back = PdfCraftApp::with_flavor(Flavor::Margin);
    back.restore(&margin.persist());
    assert_eq!(back.margin_display, margin.margin_display);

    let mut pdfcraft = PdfCraftApp::new();
    assert!(pdfcraft.set_option("page_color", "night").is_err());
    let saved: serde_json::Value = serde_json::from_str(&pdfcraft.persist()).unwrap();
    assert!(saved.get(display::SETTINGS_KEY).is_none());
}

/// The popover alone, in a popup frame, over an app in the Margin flavour.
fn popover_harness() -> Harness<'static, (PdfCraftApp, bool)> {
    let app = PdfCraftApp::with_flavor(Flavor::Margin);
    // 60 fps: the harness feeds one input event per frame, so a double-click spans a few frames.
    let mut h = Harness::builder().with_size(egui::vec2(440.0, 520.0)).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui, (app, styled): &mut (PdfCraftApp, bool)| {
            // Named font families exist only from the frame after they are installed.
            if !*styled {
                theme::install_fonts(ui.ctx());
                theme::apply(ui.ctx(), ThemeKind::Margin);
                *styled = true;
                return;
            }
            egui::Frame::popup(ui.style()).show(ui, |ui| display::popover_contents(app, ui));
        },
        (app, false),
    );
    h.run_steps(3);
    h
}

#[test]
fn popover_picks_modes_and_resets_sliders() {
    let mut h = popover_harness();
    let state = |h: &Harness<'static, (PdfCraftApp, bool)>| h.state().0.margin_display;
    assert_eq!(state(&h), DisplayState::default());

    h.get_by_label("Sepia").click();
    h.run_steps(2);
    assert_eq!(state(&h).color.mode, PageMode::Sepia);
    assert!(!state(&h).follow_system, "a pick by hand turns follow system off");
    assert!(h.query_by_label("Keep figure hues").is_none(), "only Ink and Mono offer it");

    h.get_by_label("Ink").click();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Keep figure hues").click();
    h.run_steps(2);
    assert!(!state(&h).color.keep_hues);

    h.state_mut().0.margin_display.color.brightness = 80;
    h.state_mut().0.margin_display.color.contrast = 140;
    h.run_steps(2);
    let sliders = h.get_all_by_role(egui::accesskit::Role::Slider).collect::<Vec<_>>();
    assert_eq!(sliders.len(), 2);
    sliders[0].click();
    sliders[0].click();
    drop(sliders);
    h.run_steps(8);
    assert_eq!(state(&h).color.brightness, 100, "double-click resets brightness");
    assert_eq!(state(&h).color.contrast, 140);

    h.get_by_label("Reset brightness & contrast").click();
    h.run_steps(2);
    assert_eq!(state(&h).color.contrast, 100);

    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Follow system dark mode").click();
    h.run_steps(2);
    assert!(state(&h).follow_system);
}
