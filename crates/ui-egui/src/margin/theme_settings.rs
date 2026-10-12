//! The Settings window's Appearance section: the theme mode (System, Light, Dark), the light and
//! dark themes, and buttons that open settings.jsonc and themes.json. Each control writes
//! settings.jsonc's `theme` (`margin::themes::set`), which applies on the next frame; every
//! other setting is edited in the files.

use egui::RichText;
use margin_config::{Appearance, ThemeMode, ThemeSetting};

use super::{CHEVRON_SIZE, CONTROL_HEIGHT, DROPDOWN_WIDTH, FIELD_TEXT, RowText, field_style, row, section};
use crate::margin::look::{self, Button, ButtonSize, Segment};
use crate::margin::themes::{self, ThemeChange};
use crate::theme::{self as theme_style, Tokens};
use crate::{PdfCraftApp, i18n};

/// What a control in the section asked for this frame.
enum Pick {
    Mode(ThemeMode),
    Theme(Appearance, String),
    Open(&'static str),
}

/// The Appearance section, after the section gap.
pub(super) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    section(ui, t, "Appearance");
    let setting = app.margin_keys.settings().theme.clone();
    let mut pick = None;
    let text = RowText {
        id: "theme-mode",
        name: i18n::t("Theme"),
        description: Some(i18n::t("System follows the light or dark appearance of your computer.")),
        first: true,
    };
    row(ui, t, text, |ui, _| {
        let current = match &setting {
            ThemeSetting::ByMode(modes) => Some(modes.mode),
            ThemeSetting::Named(_) => None,
        };
        let choices = [(ThemeMode::System, "System"), (ThemeMode::Light, "Light"), (ThemeMode::Dark, "Dark")];
        let segments = choices.map(|(mode, label)| Segment { label: i18n::t(label), selected: current == Some(mode), tip: None });
        let clicked = look::segmented_sized(ui, &segments, CONTROL_HEIGHT).and_then(|index| choices.get(index));
        if let Some((mode, _)) = clicked.filter(|(mode, _)| current != Some(*mode)) {
            pick = Some(Pick::Mode(*mode));
        }
    });
    for (appearance, id, name, description) in [
        (Appearance::Light, "theme-light", "Light theme", "Shown in light mode."),
        (Appearance::Dark, "theme-dark", "Dark theme", "Shown in dark mode."),
    ] {
        let text = RowText { id, name: i18n::t(name), description: Some(i18n::t(description)), first: false };
        row(ui, t, text, |ui, label| {
            if let Some(name) = theme_dropdown(app, ui, t, appearance, &setting, label) {
                pick = Some(Pick::Theme(appearance, name));
            }
        });
    }
    let text = RowText {
        id: "theme-files",
        name: i18n::t("Files"),
        description: Some(i18n::t("Every setting is in settings.jsonc and your themes are in themes.json. Margin applies a save at once.")),
        first: false,
    };
    row(ui, t, text, |ui, _| {
        // The row lays out right to left: the themes file sits at its end.
        if Button::secondary(ButtonSize::Standard, i18n::t("Open themes.json")).show(ui).clicked() {
            pick = Some(Pick::Open("themes.open"));
        }
        if Button::secondary(ButtonSize::Standard, i18n::t("Open settings.jsonc")).show(ui).clicked() {
            pick = Some(Pick::Open("settings.open"));
        }
    });
    match pick {
        Some(Pick::Mode(mode)) => themes::set(app, ThemeChange::Mode(mode)),
        Some(Pick::Theme(appearance, name)) => themes::set(app, ThemeChange::Slot(appearance, &name)),
        Some(Pick::Open(id)) => {
            crate::margin::config_files::run(app, id);
        }
        None => {}
    }
}

/// The theme shown in `appearance`'s mode, as a dropdown of every theme by name (`label` names it
/// for screen readers). Returns the name picked, if it differs.
fn theme_dropdown(app: &PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, appearance: Appearance, setting: &ThemeSetting, label: &str) -> Option<String> {
    field_style(ui, t);
    let (current, salt) = match (setting, appearance) {
        (ThemeSetting::Named(theme), Appearance::Light) => (theme.as_str(), "theme-light"),
        (ThemeSetting::Named(theme), Appearance::Dark) => (theme.as_str(), "theme-dark"),
        (ThemeSetting::ByMode(modes), Appearance::Light) => (modes.light.as_str(), "theme-light"),
        (ThemeSetting::ByMode(modes), Appearance::Dark) => (modes.dark.as_str(), "theme-dark"),
    };
    let chevron = t.icon;
    let mut picked = None;
    let dropdown = egui::ComboBox::from_id_salt(salt)
        .width(DROPDOWN_WIDTH)
        .truncate()
        .icon(move |ui, rect, _, _| crate::icons::paint(ui, rect, "chevron-down", CHEVRON_SIZE, chevron))
        .selected_text(RichText::new(current).font(theme_style::regular(FIELD_TEXT)).color(t.text))
        .show_ui(ui, |ui| {
            look::menu_body(ui);
            for entry in app.margin_keys.themes().entries() {
                if look::menu_row(ui, None, &entry.name, None, entry.name == current).clicked() {
                    picked = Some(entry.name.clone());
                    ui.close();
                }
            }
        });
    dropdown.response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, ui.is_enabled(), label));
    picked.filter(|picked| picked != current)
}
