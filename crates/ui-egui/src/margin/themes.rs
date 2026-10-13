//! Margin's themes (Margin flavor only): the built-in palettes "Margin Dark" (the owner's Black
//! palette) and "Margin Light", the user's theme families from `<config>` laid over them
//! (`margin_config::theme` reads and resolves the files), `theme_overrides` from settings.jsonc
//! on top, and which theme shows: settings.jsonc's `theme`, and the system's appearance in
//! `system` mode.
//!
//! Every Margin colour comes from here through [`Tokens`] and its [`Surfaces`]; nothing in Margin
//! asks which theme is showing. [`sync`] runs every frame, before Margin draws, and installs the
//! chosen theme when it changed: a save of any config file applies on the next frame.

use std::sync::LazyLock;

use egui::Color32;
use margin_config::theme::{self as format, StyleKey, StyleValue, ThemeDef, ThemeSource};
use margin_config::{Appearance, Problem, Settings, Severity, ThemeMode, ThemeModes, ThemeSetting};
use serde_json::{Value, json};

use super::look::{SHADOW_L, SHADOW_M, SHADOW_S, Surfaces};
use crate::PdfCraftApp;
use crate::theme::{self, ThemeKind, Tokens};

/// A control's corner radius in both built-ins (rows, buttons, inputs).
const RADIUS: u8 = 6;

/// The owner's Black palette (plan/R10-SURFACES.md §2 structure, theme lab "Black"): pure black
/// under the pages, the library and the header/footer strips, #0a0a0a for permanent side panes,
/// #111111 for cards, #161616 for the floating level. Regions part by fill alone: the frame edge
/// is transparent (no stroke is drawn), and #161616 dividers rule rows and sections. Hover,
/// selected and pressed are three neutral steps; a selected item's label and icon carry the
/// orange. Orange is only a signal: `accent` fills, `accent_text` colours text and active icons.
pub(crate) fn margin_dark() -> Tokens {
    const BLACK: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
    const ACCENT: Color32 = Color32::from_rgb(0xFF, 0x5F, 0x1F);
    const ACCENT_HOVER: Color32 = Color32::from_rgb(0xFF, 0x7A, 0x45);
    const TRACK: Color32 = Color32::from_rgb(0x1E, 0x1E, 0x1E);
    const HEADING: Color32 = Color32::from_rgb(0xEC, 0x5B, 0x2B);
    const SELECTED_TEXT: Color32 = Color32::from_rgb(0xFF, 0x6A, 0x33);
    /// The accent at 22 % (text selection, find matches).
    const ACCENT_SOFT: Color32 = Color32::from_rgba_unmultiplied_const(0xFF, 0x5F, 0x1F, 0x38);
    /// Black at 60 % behind a modal.
    const SCRIM_ALPHA: u8 = 153;
    const PAGE_SHADOW_ALPHA: u8 = 0x78;
    let text_faint = Color32::from_rgb(0x5A, 0x5A, 0x5A);
    Tokens {
        kind: ThemeKind::Margin,
        titlebar: BLACK,
        chrome: BLACK,
        panel: Color32::from_rgb(0x0A, 0x0A, 0x0A),
        pasteboard: BLACK,
        card: Color32::from_rgb(0x11, 0x11, 0x11),
        border: Color32::TRANSPARENT,
        divider: Color32::from_rgb(0x16, 0x16, 0x16),
        text: Color32::from_rgb(0xEE, 0xEE, 0xEE),
        text_muted: Color32::from_rgb(0x80, 0x80, 0x80),
        text_faint,
        icon: Color32::from_rgb(0x8A, 0x8A, 0x8A),
        hover: Color32::from_rgb(0x18, 0x18, 0x18),
        pressed: Color32::from_rgb(0x26, 0x26, 0x26),
        selected: Color32::from_rgb(0x1E, 0x1E, 0x1E),
        accent: ACCENT,
        accent_text: SELECTED_TEXT,
        accent_soft: ACCENT_SOFT,
        field: Color32::from_rgb(0x05, 0x05, 0x05),
        badge_new: ACCENT,
        page_shadow: Color32::from_black_alpha(PAGE_SHADOW_ALPHA),
        radius: RADIUS,
        is_dark: true,
        surfaces: Surfaces {
            floating: Color32::from_rgb(0x16, 0x16, 0x16),
            status_bar: BLACK,
            edge_hover: Color32::TRANSPARENT,
            control_edge: Color32::from_rgb(0x24, 0x24, 0x24),
            selected_edge: Color32::TRANSPARENT,
            accent_hover: ACCENT_HOVER,
            accent_pressed: Color32::from_rgb(0xE0, 0x4F, 0x15),
            on_accent: BLACK,
            focus: ACCENT,
            find_match: ACCENT_SOFT,
            scrim: Color32::from_black_alpha(SCRIM_ALPHA),
            track: TRACK,
            // The track, two dark oranges (opaque, so a cell's colour never depends on its
            // parent), the accent, its hover.
            heat: [TRACK, Color32::from_rgb(0x68, 0x30, 0x19), Color32::from_rgb(0xAD, 0x45, 0x1C), ACCENT, ACCENT_HOVER],
            heading: HEADING,
            section_label: HEADING,
            section_count: text_faint,
            selected_text: SELECTED_TEXT,
            shadow_card: SHADOW_S,
            shadow_floating: SHADOW_L,
            shadow_modal: SHADOW_M,
        },
    }
}

/// The Black palette's structure in light: a pale grey base under the pages, the library and the
/// header/footer strips, lighter side panes, white cards and floating surfaces. Light fills part
/// less than dark ones do, so regions also get a faint edge. The same orange fills; text, headings
/// and selected labels use a darker orange that reads on white; shadows are lighter.
pub(crate) fn margin_light() -> Tokens {
    const BASE: Color32 = Color32::from_rgb(0xF4, 0xF4, 0xF4);
    const WHITE: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
    const ACCENT: Color32 = Color32::from_rgb(0xFF, 0x5F, 0x1F);
    const ACCENT_TEXT: Color32 = Color32::from_rgb(0xB5, 0x3D, 0x0C);
    const ACCENT_PRESSED: Color32 = Color32::from_rgb(0xE0, 0x4F, 0x15);
    const ACCENT_SOFT: Color32 = Color32::from_rgba_unmultiplied_const(0xFF, 0x5F, 0x1F, 0x38);
    const TRACK: Color32 = Color32::from_rgb(0xE4, 0xE4, 0xE4);
    /// Black at 30 % behind a modal: the light base needs less to recede.
    const SCRIM_ALPHA: u8 = 77;
    const PAGE_SHADOW_ALPHA: u8 = 34;
    /// The three shadows' alpha: about a third of the Black palette's.
    const SHADOW_CARD_ALPHA: u8 = 20;
    const SHADOW_FLOATING_ALPHA: u8 = 31;
    const SHADOW_MODAL_ALPHA: u8 = 46;
    let text_faint = Color32::from_rgb(0x8A, 0x8A, 0x8A);
    let shadow = |shape: egui::Shadow, alpha: u8| egui::Shadow { color: Color32::from_black_alpha(alpha), ..shape };
    Tokens {
        kind: ThemeKind::Margin,
        titlebar: BASE,
        chrome: BASE,
        panel: Color32::from_rgb(0xFA, 0xFA, 0xFA),
        pasteboard: BASE,
        card: WHITE,
        border: Color32::from_rgb(0xE2, 0xE2, 0xE2),
        divider: Color32::from_rgb(0xE8, 0xE8, 0xE8),
        text: Color32::from_rgb(0x1A, 0x1A, 0x1A),
        text_muted: Color32::from_rgb(0x5E, 0x5E, 0x5E),
        text_faint,
        icon: Color32::from_rgb(0x6B, 0x6B, 0x6B),
        hover: Color32::from_rgb(0xEB, 0xEB, 0xEB),
        pressed: Color32::from_rgb(0xDA, 0xDA, 0xDA),
        selected: Color32::from_rgb(0xE4, 0xE4, 0xE4),
        accent: ACCENT,
        accent_text: ACCENT_TEXT,
        accent_soft: ACCENT_SOFT,
        field: WHITE,
        badge_new: ACCENT,
        page_shadow: Color32::from_black_alpha(PAGE_SHADOW_ALPHA),
        radius: RADIUS,
        is_dark: false,
        surfaces: Surfaces {
            floating: WHITE,
            status_bar: BASE,
            edge_hover: Color32::TRANSPARENT,
            control_edge: Color32::from_rgb(0xD4, 0xD4, 0xD4),
            selected_edge: Color32::TRANSPARENT,
            accent_hover: Color32::from_rgb(0xFF, 0x7A, 0x45),
            accent_pressed: ACCENT_PRESSED,
            on_accent: Color32::from_rgb(0x00, 0x00, 0x00),
            focus: ACCENT,
            find_match: ACCENT_SOFT,
            scrim: Color32::from_black_alpha(SCRIM_ALPHA),
            track: TRACK,
            // The track, two pale oranges (opaque), the accent, its pressed shade: busier is
            // stronger on a light base.
            heat: [TRACK, Color32::from_rgb(0xFF, 0xD3, 0xBF), Color32::from_rgb(0xFF, 0xA5, 0x7E), ACCENT, ACCENT_PRESSED],
            heading: ACCENT_TEXT,
            section_label: ACCENT_TEXT,
            section_count: text_faint,
            selected_text: ACCENT_TEXT,
            shadow_card: shadow(SHADOW_S, SHADOW_CARD_ALPHA),
            shadow_floating: shadow(SHADOW_L, SHADOW_FLOATING_ALPHA),
            shadow_modal: shadow(SHADOW_M, SHADOW_MODAL_ALPHA),
        },
    }
}

/// The built-in theme of `appearance`.
fn builtin(appearance: Appearance) -> Tokens {
    match appearance {
        Appearance::Dark => margin_dark(),
        Appearance::Light => margin_light(),
    }
}

/// How far `text.accent` and `margin.accent` lighten the accent for its hover shade, and how much
/// of each channel its pressed shade keeps.
const ACCENT_HOVER_LIGHTEN: f32 = 0.15;
const ACCENT_PRESSED_KEEP: f32 = 0.88;
/// The alpha of the accent's soft wash (text selection, find matches): 22 %.
const ACCENT_SOFT_ALPHA: u8 = 0x38;
/// The activity ramp's two steps between the track and the accent, as the accent's share.
const HEAT_SHARES: [f32; 2] = [0.35, 0.65];

fn color(rgba: [u8; 4]) -> Color32 {
    let [red, green, blue, alpha] = rgba;
    Color32::from_rgba_unmultiplied(red, green, blue, alpha)
}

fn darken(color: Color32, keep: f32) -> Color32 {
    let scale = |channel: u8| (f32::from(channel) * keep).round().clamp(0.0, f32::from(u8::MAX)) as u8;
    let [red, green, blue, alpha] = color.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(scale(red), scale(green), scale(blue), alpha)
}

fn with_alpha(color: Color32, alpha: u8) -> Color32 {
    let [red, green, blue, _] = color.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(red, green, blue, alpha)
}

/// The accent fill and its shades: hover, pressed, the focus ring, the activity ramp.
fn set_accent_fill(t: &mut Tokens, accent: Color32) {
    let hover = accent.lerp_to_gamma(Color32::WHITE, ACCENT_HOVER_LIGHTEN);
    t.accent = accent;
    t.badge_new = accent;
    let surfaces = &mut t.surfaces;
    surfaces.accent_hover = hover;
    surfaces.accent_pressed = darken(accent, ACCENT_PRESSED_KEEP);
    surfaces.focus = accent;
    let track = surfaces.track;
    surfaces.heat = [track, track.lerp_to_gamma(accent, HEAT_SHARES[0]), track.lerp_to_gamma(accent, HEAT_SHARES[1]), accent, hover];
}

/// Every accent role from one colour (`text.accent`): the fill family, accent text, headings,
/// section labels, selected labels, and the soft wash of selections and find matches.
fn set_accent(t: &mut Tokens, accent: Color32) {
    set_accent_fill(t, accent);
    t.accent_text = accent;
    let soft = with_alpha(accent, ACCENT_SOFT_ALPHA);
    t.accent_soft = soft;
    let surfaces = &mut t.surfaces;
    surfaces.heading = accent;
    surfaces.section_label = accent;
    surfaces.selected_text = accent;
    surfaces.find_match = soft;
}

/// Lays `style` over `t`, key by key in [`StyleKey`] order (the schema describes each mapping).
fn apply_style(t: &mut Tokens, style: &format::Style) {
    for &(key, value) in style.entries() {
        let rgba = match value {
            StyleValue::Points(points) => {
                if key == StyleKey::MarginRadius {
                    t.radius = points;
                }
                continue;
            }
            StyleValue::Color(rgba) => color(rgba),
        };
        let surfaces = &mut t.surfaces;
        match key {
            StyleKey::Background => {
                t.chrome = rgba;
                t.titlebar = rgba;
                surfaces.status_bar = rgba;
            }
            StyleKey::EditorBackground => t.pasteboard = rgba,
            StyleKey::PanelBackground => t.panel = rgba,
            StyleKey::SurfaceBackground => t.card = rgba,
            StyleKey::ElevatedSurfaceBackground => surfaces.floating = rgba,
            StyleKey::TitleBarBackground => t.titlebar = rgba,
            StyleKey::StatusBarBackground => surfaces.status_bar = rgba,
            StyleKey::Border => t.border = rgba,
            StyleKey::BorderVariant => {
                t.divider = rgba;
                surfaces.control_edge = rgba;
            }
            StyleKey::BorderSelected => surfaces.selected_edge = rgba,
            StyleKey::ElementBackground => t.field = rgba,
            StyleKey::ElementHover | StyleKey::GhostElementHover => t.hover = rgba,
            StyleKey::ElementActive | StyleKey::GhostElementActive => t.pressed = rgba,
            StyleKey::ElementSelected | StyleKey::GhostElementSelected => t.selected = rgba,
            StyleKey::Text => t.text = rgba,
            StyleKey::TextMuted => t.text_muted = rgba,
            StyleKey::TextPlaceholder => {
                t.text_faint = rgba;
                surfaces.section_count = rgba;
            }
            StyleKey::Icon | StyleKey::IconMuted => t.icon = rgba,
            StyleKey::MarginTrack => {
                surfaces.track = rgba;
                if let Some(first) = surfaces.heat.first_mut() {
                    *first = rgba;
                }
            }
            StyleKey::TextAccent => set_accent(t, rgba),
            StyleKey::MarginAccent => set_accent_fill(t, rgba),
            StyleKey::BorderFocused => surfaces.focus = rgba,
            StyleKey::ElementSelectionBackground => t.accent_soft = rgba,
            StyleKey::SearchMatchBackground => surfaces.find_match = rgba,
            StyleKey::MarginAccentHover => surfaces.accent_hover = rgba,
            StyleKey::MarginAccentActive => surfaces.accent_pressed = rgba,
            StyleKey::MarginOnAccent => surfaces.on_accent = rgba,
            StyleKey::MarginHeading => surfaces.heading = rgba,
            StyleKey::MarginSectionLabel => surfaces.section_label = rgba,
            StyleKey::MarginSelectedText => surfaces.selected_text = rgba,
            StyleKey::MarginControlBorder => surfaces.control_edge = rgba,
            StyleKey::MarginBorderHover => surfaces.edge_hover = rgba,
            StyleKey::MarginScrim => surfaces.scrim = rgba,
            StyleKey::MarginHeat1 => set_heat(surfaces, 1, rgba),
            StyleKey::MarginHeat2 => set_heat(surfaces, 2, rgba),
            StyleKey::MarginHeat3 => set_heat(surfaces, 3, rgba),
            StyleKey::MarginHeat4 => set_heat(surfaces, 4, rgba),
            StyleKey::MarginPageShadow => t.page_shadow = rgba,
            // A number key never carries a colour (`check_style_value`).
            StyleKey::MarginRadius => {}
        }
    }
}

fn set_heat(surfaces: &mut Surfaces, step: usize, rgba: Color32) {
    if let Some(cell) = surfaces.heat.get_mut(step) {
        *cell = rgba;
    }
}

/// One theme Margin can show, resolved.
#[derive(Clone, Debug)]
pub struct ThemeEntry {
    /// Its name, as settings.jsonc's `theme` names it.
    pub name: String,
    /// Light or dark.
    pub appearance: Appearance,
    /// Its colours, overrides included.
    pub tokens: Tokens,
}

/// Every theme Margin can show: the built-ins, then the user's, each resolved over its parents
/// with its `theme_overrides`.
#[derive(Clone, Debug)]
pub struct Themes {
    entries: Vec<ThemeEntry>,
}

/// The themes before `<config>` is read (and on a build without a config folder).
static BUILTIN_THEMES: LazyLock<Themes> = LazyLock::new(|| Themes::build(&[], &Settings::default(), "").0);

impl Themes {
    /// The built-ins alone.
    pub(crate) fn builtin() -> &'static Themes {
        &BUILTIN_THEMES
    }

    /// Resolves `themes` (read from the files) and lays `settings.theme_overrides` over them.
    /// Returns what was wrong: a missing parent, a loop, an override or a `theme` naming no
    /// theme. `settings_file` names settings.jsonc in those problems.
    pub(crate) fn load(sources: &[ThemeSource], settings: &Settings, settings_file: &str) -> (Self, Vec<Problem>) {
        let (themes, mut problems) = format::read_themes(sources);
        let (built, more) = Self::build(&themes, settings, settings_file);
        problems.extend(more);
        (built, problems)
    }

    fn build(themes: &[ThemeDef], settings: &Settings, settings_file: &str) -> (Self, Vec<Problem>) {
        let mut problems: Vec<Problem> = Vec::new();
        let mut entries = Vec::new();
        for (name, _) in format::theme_names(themes) {
            let Some(resolution) = format::resolve(themes, &name) else { continue };
            for problem in resolution.problems {
                if !problems.contains(&problem) {
                    problems.push(problem);
                }
            }
            let mut tokens = builtin(resolution.base);
            for layer in &resolution.layers {
                apply_style(&mut tokens, &layer.style);
            }
            tokens.is_dark = resolution.appearance == Appearance::Dark;
            if let Some(overrides) = settings.theme_overrides.get(&name) {
                apply_style(&mut tokens, &format::Style::from_checked(&overrides.0));
            }
            entries.push(ThemeEntry { name, appearance: resolution.appearance, tokens });
        }
        let built = Self { entries };
        let settings_problem = |key: &str, message: String| Problem {
            file: settings_file.to_owned(),
            line: 1,
            col: 1,
            key: key.to_owned(),
            message,
            severity: Severity::Warning,
        };
        for name in settings.theme_overrides.keys().filter(|name| built.get(name).is_none()) {
            problems.push(settings_problem(
                &format!("theme_overrides.{name}"),
                format!("theme_overrides names \"{name}\", which no theme is named; ignored"),
            ));
        }
        for (key, name, fallback) in chosen_names(&settings.theme) {
            if built.get(name).is_none() {
                problems.push(settings_problem(key, format!("{key} names \"{name}\", which no theme is named; using {fallback}")));
            }
        }
        (built, problems)
    }

    /// The theme called `name`.
    pub fn get(&self, name: &str) -> Option<&ThemeEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// Every theme, built-ins first.
    pub fn entries(&self) -> &[ThemeEntry] {
        &self.entries
    }

    /// The theme `setting` shows while the system is dark (`Some(true)`), light, or silent. A name
    /// no theme has falls back to the built-in of the slot it fills (Margin Dark for a lone name).
    pub fn choose(&self, setting: &ThemeSetting, system_dark: Option<bool>) -> &ThemeEntry {
        let (name, slot) = setting.pick(system_dark);
        self.get(name).or_else(|| self.get(slot.builtin())).unwrap_or(&DARK_ENTRY)
    }
}

/// Margin Dark as an entry: what [`Themes::choose`] falls back to if a list lacked the built-ins
/// (`build` always lists them first, so it never does).
static DARK_ENTRY: LazyLock<ThemeEntry> =
    LazyLock::new(|| ThemeEntry { name: Appearance::Dark.builtin().to_owned(), appearance: Appearance::Dark, tokens: margin_dark() });

/// The theme names `setting` holds, with their settings key and the built-in that stands in for
/// a missing one.
fn chosen_names(setting: &ThemeSetting) -> Vec<(&'static str, &str, &'static str)> {
    match setting {
        ThemeSetting::Named(name) => vec![("theme", name.as_str(), Appearance::Dark.builtin())],
        ThemeSetting::ByMode(modes) => {
            vec![("theme.light", modes.light.as_str(), Appearance::Light.builtin()), ("theme.dark", modes.dark.as_str(), Appearance::Dark.builtin())]
        }
    }
}

/// Installs the theme settings.jsonc picks for the system's appearance, if it isn't the one
/// installed: after a reload, a change of `theme`, or the system turning light or dark. Every
/// frame, before Margin draws.
pub(crate) fn sync(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let follow = matches!(&app.margin_keys.settings().theme, ThemeSetting::ByMode(modes) if modes.mode == ThemeMode::System);
    let system_dark = app.system_dark(ctx, follow);
    let keys = &app.margin_keys;
    let entry = keys.themes().choose(&keys.settings().theme, system_dark);
    let revision = keys.revision();
    if keys.applied_theme.as_ref().is_some_and(|(applied, name)| *applied == revision && *name == entry.name) {
        return;
    }
    theme::apply_tokens(ctx, &entry.tokens);
    let name = entry.name.clone();
    app.margin_keys.applied_theme = Some((revision, name));
}

/// One change the Settings window or a command makes to `theme`.
pub(crate) enum ThemeChange<'a> {
    /// Which theme shows: system, light or dark.
    Mode(ThemeMode),
    /// The theme shown in light (`Appearance::Light`) or dark mode.
    Slot(Appearance, &'a str),
}

/// Writes `change` into settings.jsonc's `theme`, keeping the file's comments, and reads the
/// file again so the theme applies on the next frame. A `theme` written as one name becomes the
/// `{ mode, light, dark }` form, that name in both slots. A failure is told in a notice.
pub(crate) fn set(app: &mut PdfCraftApp, change: ThemeChange<'_>) {
    let Some(dir) = app.margin_keys.settings_dir() else {
        app.notify("This system has no settings folder.");
        return;
    };
    let current = app.margin_keys.settings().theme.clone();
    let (key, value) = match &change {
        ThemeChange::Mode(mode) => ("theme.mode", Value::from(mode.as_str())),
        ThemeChange::Slot(Appearance::Light, name) => ("theme.light", Value::from(*name)),
        ThemeChange::Slot(Appearance::Dark, name) => ("theme.dark", Value::from(*name)),
    };
    let written = match current {
        ThemeSetting::ByMode(_) => margin_config::set_user_values(&dir, &[(key, value)]),
        ThemeSetting::Named(name) => {
            let appearance = app.margin_keys.themes().get(&name).map_or(ThemeMode::Dark, |entry| match entry.appearance {
                Appearance::Dark => ThemeMode::Dark,
                Appearance::Light => ThemeMode::Light,
            });
            let mut modes = ThemeModes { mode: appearance, light: name.clone(), dark: name };
            match change {
                ThemeChange::Mode(mode) => modes.mode = mode,
                ThemeChange::Slot(Appearance::Light, name) => name.clone_into(&mut modes.light),
                ThemeChange::Slot(Appearance::Dark, name) => name.clone_into(&mut modes.dark),
            }
            let whole = json!({ "mode": modes.mode.as_str(), "light": modes.light, "dark": modes.dark });
            margin_config::set_user_values(&dir, &[("theme", whole)])
        }
    };
    match written {
        Ok(()) => super::keys::reload_now(app),
        Err(err) => app.notify(format!("Couldn't set the theme: {err}")),
    }
}

/// PdfCraft's theme commands in Margin (`view.theme`, `view.theme.system|light|dark`): they set
/// settings.jsonc's `theme.mode`, as the Settings window does. `view.theme` switches to the
/// appearance not showing. `false` for any other id.
pub(crate) fn run_command(app: &mut PdfCraftApp, id: &str) -> bool {
    let mode = match id {
        "view.theme.system" => ThemeMode::System,
        "view.theme.light" => ThemeMode::Light,
        "view.theme.dark" => ThemeMode::Dark,
        "view.theme" => {
            let showing_dark = app.ctx.as_ref().is_none_or(|ctx| Tokens::get(ctx).dark());
            if showing_dark { ThemeMode::Light } else { ThemeMode::Dark }
        }
        _ => return false,
    };
    set(app, ThemeChange::Mode(mode));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(text: serde_json::Value) -> ThemeSource {
        ThemeSource { path: "themes.json".into(), text: Ok(Some(text.to_string())) }
    }

    /// Margin Dark is the Black palette exactly as it was drawn before themes.
    #[test]
    fn margin_dark_keeps_the_black_palette() {
        let t = margin_dark();
        assert_eq!(
            (t.chrome, t.panel, t.card, t.surfaces.floating),
            (Color32::BLACK, Color32::from_gray(0x0A), Color32::from_gray(0x11), Color32::from_gray(0x16))
        );
        assert_eq!(
            (t.accent, t.accent_text, t.surfaces.heading),
            (Color32::from_rgb(0xFF, 0x5F, 0x1F), Color32::from_rgb(0xFF, 0x6A, 0x33), Color32::from_rgb(0xEC, 0x5B, 0x2B))
        );
        assert_eq!(t.border, Color32::TRANSPARENT);
        assert!(t.dark());
        assert_eq!(Tokens::for_kind(ThemeKind::Margin).surfaces.control_edge, Color32::from_gray(0x24));
    }

    /// `text.accent` recolours every accent role; a refining key after it wins for its role.
    #[test]
    fn text_accent_recolours_every_accent_role() {
        let blue = Color32::from_rgb(0x4B, 0x8B, 0xF5);
        let family = json!({ "themes": [{ "name": "Blue", "appearance": "dark", "extends": "Margin Dark",
            "style": { "text.accent": "#4b8bf5", "margin.heading": "#ffffff" } }] });
        let (themes, problems) = Themes::load(&[source(family)], &Settings::default(), "settings.jsonc");
        assert_eq!(problems, []);
        let t = themes.get("Blue").unwrap().tokens;
        for (role, value) in [
            ("accent", t.accent),
            ("accent_text", t.accent_text),
            ("badge_new", t.badge_new),
            ("focus", t.surfaces.focus),
            ("section_label", t.surfaces.section_label),
            ("selected_text", t.surfaces.selected_text),
            ("heat busiest-but-one", t.surfaces.heat[3]),
        ] {
            assert_eq!(value, blue, "{role}");
        }
        assert_eq!(t.surfaces.heading, Color32::WHITE);
        assert_eq!(t.accent_soft, with_alpha(blue, ACCENT_SOFT_ALPHA));
        assert_eq!(t.chrome, Color32::BLACK, "the rest is Margin Dark's");
    }

    /// `theme_overrides` lay over the named theme only; one naming no theme is reported.
    #[test]
    fn overrides_apply_to_their_theme() {
        let settings = Settings::from_user_text(
            r##"{ "theme_overrides": { "Margin Light": { "background": "#fafaf0" }, "Nope": { "text": "#000000" } } }"##,
            "settings.jsonc",
        )
        .settings;
        let (themes, problems) = Themes::load(&[], &settings, "settings.jsonc");
        assert_eq!(themes.get("Margin Light").unwrap().tokens.chrome, Color32::from_rgb(0xFA, 0xFA, 0xF0));
        assert_eq!(themes.get("Margin Dark").unwrap().tokens.chrome, Color32::BLACK);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].key, "theme_overrides.Nope");
    }

    /// The mode picks the light or the dark theme; `system` follows the system; a name no theme
    /// has falls back to the built-in of its slot and is reported.
    #[test]
    fn the_mode_picks_the_theme() {
        let themes = Themes::builtin();
        let modes = |mode| ThemeSetting::ByMode(ThemeModes { mode, ..ThemeModes::default() });
        assert_eq!(themes.choose(&modes(ThemeMode::Light), Some(true)).name, "Margin Light");
        assert_eq!(themes.choose(&modes(ThemeMode::Dark), Some(false)).name, "Margin Dark");
        assert_eq!(themes.choose(&modes(ThemeMode::System), Some(false)).name, "Margin Light");
        assert_eq!(themes.choose(&modes(ThemeMode::System), Some(true)).name, "Margin Dark");
        assert!(!themes.choose(&modes(ThemeMode::Light), None).tokens.dark());
        let missing = ThemeSetting::ByMode(ThemeModes { mode: ThemeMode::Light, light: "Gone".into(), dark: "Margin Dark".into() });
        assert_eq!(themes.choose(&missing, None).name, "Margin Light");
        let settings = Settings { theme: missing, ..Settings::default() };
        let (_, problems) = Themes::load(&[], &settings, "settings.jsonc");
        assert_eq!(problems.iter().map(|problem| problem.key.as_str()).collect::<Vec<_>>(), ["theme.light"]);
    }
}
