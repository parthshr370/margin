//! Margin's Display popover and page colour state.
//!
//! [`DisplayState`] is the user's choice (saved with the app settings for now; per-document
//! reading state moves it to a sidecar later). [`PageColorCache`] lives in each `DocView`: it
//! recolours rasters on their way to the GPU and keeps the source pixels of the pages around the
//! current one, so a mode switch re-uploads only the pages on screen, without re-rendering them.

use std::collections::HashMap;

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, TextureHandle, TextureOptions, pos2, vec2};
use serde::{Deserialize, Serialize};

use super::pagecolor::{BRIGHTNESS_RANGE, CONTRAST_RANGE, NEUTRAL_PERCENT, PageColor, PageMode};
use crate::canvas::{Fit, PageLayout};
use crate::theme::{self, Tokens};
use crate::{Flavor, PdfCraftApp, i18n};

/// The page colour choice saved in the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayState {
    pub color: PageColor,
    /// Night while the system is dark, Paper while it is light. Picking a mode by hand turns it
    /// off.
    pub follow_system: bool,
}

impl Default for DisplayState {
    fn default() -> Self {
        Self { color: PageColor { mode: PageMode::Night, ..PageColor::default() }, follow_system: true }
    }
}

/// The settings key [`DisplayState`] is saved under.
pub const SETTINGS_KEY: &str = "margin_display";

impl DisplayState {
    /// The page colour in effect: the saved one, with the mode following the system when asked
    /// to and the system says.
    pub fn effective(&self, system: Option<egui::Theme>) -> PageColor {
        let mode = match (self.follow_system, system) {
            (true, Some(egui::Theme::Dark)) => PageMode::Night,
            (true, Some(egui::Theme::Light)) => PageMode::Paper,
            _ => self.color.mode,
        };
        PageColor { mode, ..self.color }.clamped()
    }

    /// Pick a mode by hand: it wins over "follow system".
    pub fn pick(&mut self, mode: PageMode) {
        self.color.mode = mode;
        self.follow_system = false;
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    /// Read what [`Self::to_json`] wrote; anything malformed keeps the current choice.
    pub fn restore(&mut self, value: &serde_json::Value) {
        if let Ok(state) = serde_json::from_value::<DisplayState>(value.clone()) {
            *self = Self { color: state.color.clamped(), ..state };
        }
    }

    /// `--page_color paper|sepia|night|ink|mono|system` (and the control channel's `set`).
    pub fn set_option(&mut self, value: &str) -> Result<(), String> {
        if value == "system" {
            self.follow_system = true;
            return Ok(());
        }
        let mode = PageMode::ALL
            .into_iter()
            .find(|m| m.name().eq_ignore_ascii_case(value))
            .ok_or_else(|| "page_color must be paper, sepia, night, ink, mono or system".to_string())?;
        self.pick(mode);
        Ok(())
    }
}

/// The page colour a document view should show this frame: `None` keeps PdfCraft's pages as
/// rendered.
pub fn page_color_for(app: &PdfCraftApp, ctx: &egui::Context) -> Option<PageColor> {
    (app.flavor == Flavor::Margin).then(|| app.margin_display.effective(ctx.system_theme()))
}

/// Pages within this distance of the current page keep their source pixels for recolouring.
const SOURCE_NEAR: usize = 2;

/// One document view's recolouring: the colour in effect and the source pixels of nearby pages.
#[derive(Default)]
pub struct PageColorCache {
    color: Option<PageColor>,
    sources: HashMap<usize, Source>,
}

struct Source {
    size: [usize; 2],
    rgba: Vec<u8>,
}

impl PageColorCache {
    /// Set this frame's colour (`None`: PdfCraft, nothing is recoloured or kept). True when the
    /// output changed, so textures recoloured before are out of date.
    pub fn configure(&mut self, color: Option<PageColor>) -> bool {
        let changed = self.key_of(color) != self.key();
        if color.is_none() {
            self.sources.clear();
        }
        self.color = color;
        changed
    }

    fn key_of(&self, color: Option<PageColor>) -> u64 {
        color.map_or(0, PageColor::key)
    }

    /// Identifies the colour textures are recoloured with now (0: as rendered).
    pub fn key(&self) -> u64 {
        self.key_of(self.color)
    }

    /// The paper colour drawn under a page until its raster arrives.
    pub fn paper(&self) -> Color32 {
        let [r, g, b] = self.color.map_or(PageMode::Paper, |c| c.mode).paper();
        Color32::from_rgb(r, g, b)
    }

    /// Turn a rendered raster into a texture image in the current colour. For a whole page
    /// (`page` is `Some`) near `current`, keep the source pixels to recolour it later.
    pub fn image(&mut self, page: Option<usize>, current: usize, size: [usize; 2], mut rgba: Vec<u8>) -> egui::ColorImage {
        let Some(color) = self.color else { return egui::ColorImage::from_rgba_premultiplied(size, &rgba) };
        if let Some(page) = page {
            if page.abs_diff(current) <= SOURCE_NEAR {
                self.sources.insert(page, Source { size, rgba: rgba.clone() });
            } else {
                self.sources.remove(&page);
            }
        }
        color.apply(&mut rgba, size[0], size[1]);
        egui::ColorImage::from_rgba_premultiplied(size, &rgba)
    }

    /// Recolour `page`'s texture in place from its kept source. False when there is no source
    /// (the page must be rendered again).
    pub fn recolor(&self, page: usize, tex: &mut TextureHandle) -> bool {
        let (Some(color), Some(source)) = (self.color, self.sources.get(&page)) else { return false };
        let mut rgba = source.rgba.clone();
        color.apply(&mut rgba, source.size[0], source.size[1]);
        tex.set(egui::ColorImage::from_rgba_premultiplied(source.size, &rgba), TextureOptions::LINEAR);
        true
    }

    /// Drop the sources of pages that moved away from `current`.
    pub fn retain_near(&mut self, current: usize) {
        self.sources.retain(|page, _| page.abs_diff(current) <= SOURCE_NEAR);
    }
}

const POPOVER_WIDTH: f32 = 384.0;
const SWATCH_GAP: f32 = 8.0;
const SWATCH_HEIGHT: f32 = 62.0;
const LABEL_WIDTH: f32 = 92.0;
const VALUE_WIDTH: f32 = 36.0;
const SWITCH_SIZE: egui::Vec2 = egui::Vec2::new(30.0, 18.0);
const ROW_HEIGHT: f32 = 22.0;
const SLIDER_RAIL_HEIGHT: f32 = 3.0;

/// The Display popover's body, laid out like the mockup: page colour swatches, keep figure hues
/// (Ink and Mono), brightness and contrast (double-click a slider to reset it) and their reset;
/// then the page layout and fit (PdfCraft's commands) and follow system. The caller owns the
/// popup frame and its placement.
pub fn popover_contents(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let system = ui.ctx().system_theme();
    let state = &mut app.margin_display;
    let shown = state.effective(system);
    ui.set_width(POPOVER_WIDTH);
    ui.spacing_mut().item_spacing.y = 6.0;
    crate::widgets::section_title(ui, "Page color");
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SWATCH_GAP;
        let w = (POPOVER_WIDTH - SWATCH_GAP * 4.0) / 5.0;
        for mode in PageMode::ALL {
            if swatch(ui, &t, mode, mode == shown.mode, w).clicked() {
                state.pick(mode);
            }
        }
    });
    ui.add_space(4.0);
    ui.add(egui::Label::new(egui::RichText::new(i18n::t(shown.mode.description())).font(theme::regular(12.0)).color(t.text_muted)).wrap());
    if shown.mode.offers_keep_hues() {
        let sub = i18n::t("Ink and Mono only: figures get Night colors, not two-tone");
        if switch_row(ui, &t, i18n::t("Keep figure hues"), Some(sub), state.color.keep_hues) {
            state.color.keep_hues = !state.color.keep_hues;
        }
    }
    ui.add_space(4.0);
    slider_row(ui, &t, i18n::t("Brightness"), &mut state.color.brightness, BRIGHTNESS_RANGE, |v| v.to_string());
    let mono = shown.mode == PageMode::Mono;
    let (label, low, high) = (if mono { i18n::t("Cut-off") } else { i18n::t("Contrast") }, *CONTRAST_RANGE.start(), *CONTRAST_RANGE.end());
    let show = move |v: u16| if mono { format!("{}%", u32::from(v.saturating_sub(low)) * 100 / u32::from(high - low)) } else { v.to_string() };
    slider_row(ui, &t, label, &mut state.color.contrast, CONTRAST_RANGE, show);
    let reset_row = vec2(POPOVER_WIDTH, ROW_HEIGHT);
    ui.allocate_ui_with_layout(reset_row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let reset = egui::Button::new(egui::RichText::new(i18n::t("Reset brightness & contrast")).font(theme::regular(12.0)).color(t.accent_text))
            .frame(false);
        if ui.add(reset).clicked() {
            state.color.brightness = NEUTRAL_PERCENT;
            state.color.contrast = NEUTRAL_PERCENT;
        }
    });
    ui.add_space(6.0);
    ui.separator();
    layout_rows(app, ui, &t);
    let state = &mut app.margin_display;
    let follow_sub = if state.follow_system {
        i18n::t("Night when the system is dark, Paper when light")
    } else {
        i18n::t("Off: the page color stays as you set it")
    };
    if switch_row(ui, &t, i18n::t("Follow system dark mode"), Some(follow_sub), state.follow_system) {
        state.follow_system = !state.follow_system;
    }
}

/// The Layout section: page display (Scroll, Single, Two-up) and fit, through the same commands
/// and view calls as PdfCraft's View menu. Nothing without an open document.
fn layout_rows(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let Some((layout, fit)) = app.active.and_then(|i| app.views.get(i)).map(|v| (v.layout, v.fit)) else { return };
    crate::widgets::section_title(ui, "Layout");
    let mut command = None;
    let mut new_fit = None;
    choice_row(ui, t, i18n::t("Pages"), |ui| {
        for (choice, name) in [(PageLayout::TwoUp, "Two-up"), (PageLayout::Single, "Single"), (PageLayout::Continuous, "Scroll")] {
            if ui.selectable_label(layout == choice, i18n::t(name)).clicked() {
                command = Some(choice.command());
            }
        }
    });
    choice_row(ui, t, i18n::t("Fit"), |ui| {
        for (choice, name) in [(Fit::Page, "Page"), (Fit::Width, "Width")] {
            if ui.selectable_label(fit == choice, i18n::t(name)).clicked() {
                new_fit = Some(choice);
            }
        }
    });
    if let Some(choice) = new_fit
        && let Some(view) = app.active.and_then(|i| app.views.get_mut(i))
    {
        view.set_fit(choice);
    }
    if let Some(id) = command {
        app.execute(id);
    }
}

/// A label on the left and choices packed against the right edge (right to left, so list them
/// last first).
fn choice_row(ui: &mut egui::Ui, t: &Tokens, label: &str, choices: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).font(theme::regular(12.5)).color(t.text_muted));
        // The mockup's segmented control: the chosen item is a raised chip, not the selection colour.
        ui.visuals_mut().selection.bg_fill = t.pressed;
        ui.visuals_mut().selection.stroke = Stroke::new(1.0, t.text);
        let rest = vec2(ui.available_width(), ROW_HEIGHT);
        ui.allocate_ui_with_layout(rest, egui::Layout::right_to_left(egui::Align::Center), choices);
    });
}

/// A page colour swatch: a small page in the mode's paper with lines of its ink, and the name.
fn swatch(ui: &mut egui::Ui, t: &Tokens, mode: PageMode, selected: bool, width: f32) -> egui::Response {
    let name = i18n::t(mode.name());
    let (rect, resp) = ui.allocate_exact_size(vec2(width, SWATCH_HEIGHT + 34.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, name));
    let painter = ui.painter();
    if selected {
        painter.rect(rect, CornerRadius::same(9), t.accent_soft, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        painter.rect_filled(rect, CornerRadius::same(9), t.hover);
    }
    let page = Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(width - 12.0, SWATCH_HEIGHT));
    let rgb = |c: [u8; 3]| Color32::from_rgb(c[0], c[1], c[2]);
    let (paper, ink) = (rgb(mode.paper()), rgb(mode.ink()));
    painter.rect(page, CornerRadius::same(4), paper, Stroke::new(1.0, Color32::from_white_alpha(15)), egui::StrokeKind::Inside);
    let inner = page.shrink2(vec2(7.0, 8.0));
    // A figure block: pale blue on light paper, dark blue at night, a faint ink block otherwise.
    let figure = match mode {
        PageMode::Paper | PageMode::Sepia => Color32::from_rgb(0xCF, 0xE0, 0xF3),
        PageMode::Night => Color32::from_rgb(0x2F, 0x4F, 0x6B),
        PageMode::Ink | PageMode::Mono => ink.gamma_multiply(0.25),
    };
    let block = Rect::from_min_size(inner.min, vec2(inner.width() * 0.7, 10.0));
    painter.rect_filled(block, CornerRadius::same(2), figure);
    for (i, frac) in [0.92f32, 0.80, 0.88].into_iter().enumerate() {
        let y = block.bottom() + 6.0 + i as f32 * 7.0;
        let line = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width() * frac, 3.0));
        painter.rect_filled(line, CornerRadius::same(2), ink);
    }
    painter.text(pos2(rect.center().x, page.bottom() + 14.0), Align2::CENTER_CENTER, name, theme::regular(12.0), t.text);
    resp
}

/// A label (with an optional line under it) and a switch on the right. True when clicked.
fn switch_row(ui: &mut egui::Ui, t: &Tokens, label: &str, sub: Option<&str>, on: bool) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width(POPOVER_WIDTH - SWITCH_SIZE.x - 12.0);
            ui.label(egui::RichText::new(label).font(theme::regular(12.5)).color(t.text_muted));
            if let Some(sub) = sub {
                ui.add(egui::Label::new(egui::RichText::new(sub).font(theme::regular(11.5)).color(t.text_faint)).wrap());
            }
        });
        let (rect, resp) = ui.allocate_exact_size(SWITCH_SIZE, Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, label));
        let radius = rect.height() / 2.0;
        ui.painter().rect_filled(rect, CornerRadius::same(radius as u8), if on { t.accent } else { t.divider });
        let knob_x = if on { rect.right() - radius } else { rect.left() + radius };
        ui.painter().circle_filled(pos2(knob_x, rect.center().y), radius - 2.0, if on { Color32::WHITE } else { t.text_muted });
        clicked = resp.clicked();
    });
    clicked
}

/// Label, slider and value; double-clicking the slider resets it to 100 %.
fn slider_row(ui: &mut egui::Ui, t: &Tokens, label: &str, value: &mut u16, range: std::ops::RangeInclusive<u16>, show: impl Fn(u16) -> String) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(LABEL_WIDTH, ROW_HEIGHT), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(LABEL_WIDTH);
            ui.label(egui::RichText::new(label).font(theme::regular(12.5)).color(t.text_muted));
        });
        ui.spacing_mut().slider_width = POPOVER_WIDTH - LABEL_WIDTH - VALUE_WIDTH - 2.0 * ui.spacing().item_spacing.x;
        ui.spacing_mut().slider_rail_height = SLIDER_RAIL_HEIGHT;
        // The mockup's range input: a thin rail in the divider colour, a light knob.
        let widgets = &mut ui.visuals_mut().widgets;
        widgets.inactive.bg_fill = t.divider;
        widgets.hovered.bg_fill = t.text;
        widgets.active.bg_fill = t.text;
        // A slider senses drags, not clicks, so its response never reports a double-click.
        let resp = ui.add(egui::Slider::new(value, range).show_value(false)).on_hover_text(i18n::t("Double-click to reset"));
        let on_slider = ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| resp.rect.contains(p));
        // A third quick click counts as a triple, not a double: either resets.
        let repeated = ui.input(|i| {
            i.pointer.button_double_clicked(egui::PointerButton::Primary) || i.pointer.button_triple_clicked(egui::PointerButton::Primary)
        });
        if on_slider && repeated {
            *value = NEUTRAL_PERCENT;
        }
        ui.allocate_ui_with_layout(vec2(VALUE_WIDTH, ROW_HEIGHT), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.set_min_width(VALUE_WIDTH);
            ui.label(egui::RichText::new(show(*value)).font(theme::regular(11.0)).color(t.text_faint));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_system_drives_the_mode_until_a_pick() {
        let mut s = DisplayState::default();
        assert_eq!(s.effective(Some(egui::Theme::Light)).mode, PageMode::Paper);
        assert_eq!(s.effective(Some(egui::Theme::Dark)).mode, PageMode::Night);
        s.pick(PageMode::Sepia);
        assert!(!s.follow_system);
        assert_eq!(s.effective(Some(egui::Theme::Dark)).mode, PageMode::Sepia);
    }

    #[test]
    fn restore_round_trips_and_ignores_garbage() {
        let mut s = DisplayState::default();
        s.pick(PageMode::Ink);
        s.color.brightness = 90;
        let mut back = DisplayState::default();
        back.restore(&s.to_json());
        assert_eq!(back, s);
        back.restore(&serde_json::json!({"color": {"mode": "nope"}}));
        assert_eq!(back, s);
        back.restore(&serde_json::json!({"color": {"mode": "mono", "brightness": 4000}}));
        assert_eq!(back.color.mode, PageMode::Mono);
        assert_eq!(back.color.brightness, *BRIGHTNESS_RANGE.end());
    }

    #[test]
    fn set_option_accepts_modes_and_system() {
        let mut s = DisplayState::default();
        assert!(s.set_option("Night").is_ok());
        assert_eq!((s.color.mode, s.follow_system), (PageMode::Night, false));
        assert!(s.set_option("system").is_ok());
        assert!(s.follow_system);
        assert!(s.set_option("purple").is_err());
    }

    #[test]
    fn cache_keeps_sources_near_the_current_page_only() {
        let mut cache = PageColorCache::default();
        assert!(!cache.configure(None));
        let night = PageColor { mode: PageMode::Night, ..PageColor::default() };
        assert!(cache.configure(Some(night)));
        assert!(!cache.configure(Some(night)));
        let img = cache.image(Some(3), 3, [1, 1], vec![255, 255, 255, 255]);
        assert_eq!(img.pixels[0], Color32::from_rgb(0x14, 0x13, 0x12));
        let _ = cache.image(Some(9), 3, [1, 1], vec![255; 4]);
        assert!(cache.sources.contains_key(&3) && !cache.sources.contains_key(&9));
        cache.retain_near(8);
        assert!(cache.sources.is_empty());
        // PdfCraft: pixels pass through and nothing is kept.
        assert!(cache.configure(None));
        let img = cache.image(Some(0), 0, [1, 1], vec![255; 4]);
        assert_eq!(img.pixels[0], Color32::WHITE);
        assert!(cache.sources.is_empty());
    }
}
