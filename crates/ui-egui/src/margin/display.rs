//! Margin's Display popover and page colour state.
//!
//! [`DisplayState`] is one document's choice: it opens from `settings.jsonc` (`view.page_color`
//! and its neighbours) under the document's sidecar layer, and every change goes back to the
//! sidecar (`margin::reading`). [`PageColorCache`] lives in each `DocView`: it recolours rasters
//! on their way to the GPU and keeps the source pixels of the pages around the current one, so a
//! mode switch re-uploads only the pages on screen, without re-rendering them.

use std::collections::HashMap;

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, TextureHandle, TextureOptions, pos2, vec2};
use serde_json::{Map, Value};

use super::pagecolor::{BRIGHTNESS_RANGE, CONTRAST_RANGE, NEUTRAL_PERCENT, PageColor, PageMode};
use super::reading::{self, DefaultRow};
use super::trim::{self, Margins};
use crate::canvas::{Fit, PageLayout};
use crate::theme::{self, Tokens};
use crate::{DocView, Flavor, PdfCraftApp, i18n};

/// A document's page colour choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayState {
    pub color: PageColor,
    /// Night while the system is dark, Paper while it is light. Picking a mode by hand turns it
    /// off.
    pub follow_system: bool,
}

impl Default for DisplayState {
    fn default() -> Self {
        Self::from_settings(&margin_config::View::default())
    }
}

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

    /// The choice the `[doc]` settings `view` describe.
    pub fn from_settings(view: &margin_config::View) -> Self {
        let mode = match view.page_color {
            margin_config::PageColor::Paper => PageMode::Paper,
            margin_config::PageColor::Sepia => PageMode::Sepia,
            margin_config::PageColor::Night => PageMode::Night,
            margin_config::PageColor::Ink => PageMode::Ink,
            margin_config::PageColor::Mono => PageMode::Mono,
        };
        let percent = |value: u32| u16::try_from(value).unwrap_or(u16::MAX);
        let color = PageColor { mode, keep_hues: view.keep_figures, brightness: percent(view.brightness), contrast: percent(view.contrast) };
        Self { color: color.clamped(), follow_system: view.follow_system }
    }

    /// This choice as `view.*` settings values: `page_color`, `follow_system`, `keep_figures`,
    /// `brightness`, `contrast`.
    pub fn settings_values(&self) -> Map<String, Value> {
        let mut values = Map::new();
        values.insert("page_color".into(), self.color.mode.name().to_lowercase().into());
        values.insert("follow_system".into(), self.follow_system.into());
        values.insert("keep_figures".into(), self.color.keep_hues.into());
        values.insert("brightness".into(), self.color.brightness.into());
        values.insert("contrast".into(), self.color.contrast.into());
        values
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

/// The page colour view `index` should show this frame: `None` keeps PdfCraft's pages as
/// rendered.
pub fn page_color_for(app: &PdfCraftApp, index: usize, ctx: &egui::Context) -> Option<PageColor> {
    let view = app.views.get(index).filter(|_| app.flavor == Flavor::Margin)?;
    Some(view.margin.display.effective(ctx.system_theme()))
}

/// Pages within this distance of the current page keep their source pixels for recolouring.
pub(crate) const SOURCE_NEAR: usize = 2;

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

    /// The pixels `page` was rendered with, if they are kept (it is near the current page).
    pub(crate) fn source(&self, page: usize) -> Option<([usize; 2], &[u8])> {
        self.sources.get(&page).map(|source| (source.size, source.rgba.as_slice()))
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

/// The trim editor's page previews.
const TRIM_PREVIEW_WIDTH: f32 = 96.0;
/// A drag smaller than this share of the preview on either side is a slip, not a crop.
const MIN_TRIM_DRAG: f32 = 0.1;
/// How dark the trimmed-off part of a preview is drawn.
const TRIMMED_SHADE: u8 = 150;

/// The Display popover's body for the active document, laid out like the mockup: page colour
/// swatches, keep figure hues (Ink and Mono), brightness and contrast (double-click a slider to
/// reset it) and their reset; then layout, cover page, fit and trim; then follow system. "Set
/// as default" copies the page colour or layout into `settings.jsonc`. The caller owns the popup
/// frame and its placement.
pub fn popover_contents(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.set_width(POPOVER_WIDTH);
    ui.spacing_mut().item_spacing.y = 6.0;
    let Some(index) = app.active.filter(|i| *i < app.views.len()) else {
        ui.label(egui::RichText::new(i18n::t("Open a document to change how it shows.")).font(theme::regular(12.5)).color(t.text_muted));
        return;
    };
    let mut default_row = None;
    if section_with_default(ui, &t, "Page color") {
        default_row = Some(DefaultRow::PageColor);
    }
    page_color_rows(&mut app.views[index].margin.display, ui, &t);
    ui.add_space(6.0);
    ui.separator();
    if section_with_default(ui, &t, "Layout") {
        default_row = Some(DefaultRow::Layout);
    }
    let reading_width = app.margin_keys.settings().view.reading_width_px;
    if let Some(id) = layout_rows(&mut app.views[index], reading_width, ui, &t) {
        app.execute(id);
    }
    let pages = app.session.get(app.views[index].id).map(|doc| doc.info.pages.iter().map(|p| (p.width, p.height)).collect::<Vec<_>>());
    trim_rows(&mut app.views[index], &pages.unwrap_or_default(), ui, &t);
    ui.add_space(6.0);
    ui.separator();
    let state = &mut app.views[index].margin.display;
    let follow_sub = if state.follow_system {
        i18n::t("Night when the system is dark, Paper when light")
    } else {
        i18n::t("Off: the page color stays as you set it")
    };
    if switch_row(ui, &t, i18n::t("Follow system dark mode"), Some(follow_sub), state.follow_system) {
        state.follow_system = !state.follow_system;
    }
    if let Some(row) = default_row {
        let message = reading::set_as_default(app, index, row);
        app.notify(message);
    }
}

/// A section title with "Set as default" on the right. True when that was clicked.
fn section_with_default(ui: &mut egui::Ui, t: &Tokens, title: &str) -> bool {
    ui.horizontal(|ui| {
        crate::widgets::section_title(ui, title);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
            let label = egui::RichText::new(i18n::t("Set as default")).font(theme::regular(12.0)).color(t.accent_text);
            ui.add(egui::Button::new(label).frame(false))
                .on_hover_text(i18n::t("Use this for documents opened for the first time (settings.jsonc)"))
                .clicked()
        })
        .inner
    })
    .inner
}

/// Swatches, keep figure hues, brightness, contrast and their reset.
fn page_color_rows(state: &mut DisplayState, ui: &mut egui::Ui, t: &Tokens) {
    let shown = state.effective(ui.ctx().system_theme());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SWATCH_GAP;
        let w = (POPOVER_WIDTH - SWATCH_GAP * 4.0) / 5.0;
        for mode in PageMode::ALL {
            if swatch(ui, t, mode, mode == shown.mode, w).clicked() {
                state.pick(mode);
            }
        }
    });
    ui.add_space(4.0);
    ui.add(egui::Label::new(egui::RichText::new(i18n::t(shown.mode.description())).font(theme::regular(12.0)).color(t.text_muted)).wrap());
    if shown.mode.offers_keep_hues() {
        let sub = i18n::t("Ink and Mono only: figures get Night colors, not two-tone");
        if switch_row(ui, t, i18n::t("Keep figure hues"), Some(sub), state.color.keep_hues) {
            state.color.keep_hues = !state.color.keep_hues;
        }
    }
    ui.add_space(4.0);
    slider_row(ui, t, i18n::t("Brightness"), &mut state.color.brightness, BRIGHTNESS_RANGE, |v| v.to_string());
    let mono = shown.mode == PageMode::Mono;
    let (label, low, high) = (if mono { i18n::t("Cut-off") } else { i18n::t("Contrast") }, *CONTRAST_RANGE.start(), *CONTRAST_RANGE.end());
    let show = move |v: u16| if mono { format!("{}%", u32::from(v.saturating_sub(low)) * 100 / u32::from(high - low)) } else { v.to_string() };
    slider_row(ui, t, label, &mut state.color.contrast, CONTRAST_RANGE, show);
    let reset_row = vec2(POPOVER_WIDTH, ROW_HEIGHT);
    ui.allocate_ui_with_layout(reset_row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let reset = egui::Button::new(egui::RichText::new(i18n::t("Reset brightness & contrast")).font(theme::regular(12.0)).color(t.accent_text))
            .frame(false);
        if ui.add(reset).clicked() {
            state.color.brightness = NEUTRAL_PERCENT;
            state.color.contrast = NEUTRAL_PERCENT;
        }
    });
}

/// Page display (Scroll, Single, Two-up, through PdfCraft's commands, which it returns), the
/// cover page in Two-up, and fit (Read, Page, Width).
fn layout_rows(view: &mut DocView, reading_width: u32, ui: &mut egui::Ui, t: &Tokens) -> Option<&'static str> {
    let mut command = None;
    choice_row(ui, t, i18n::t("Pages"), |ui| {
        for (choice, name) in [(PageLayout::TwoUp, "Two-up"), (PageLayout::Single, "Single"), (PageLayout::Continuous, "Scroll")] {
            if ui.selectable_label(view.layout == choice, i18n::t(name)).clicked() {
                command = Some(choice.command());
            }
        }
    });
    if view.cover_applies() {
        let sub = i18n::t("The first page sits alone on the right, like a book's cover");
        if switch_row(ui, t, i18n::t("Cover page alone"), Some(sub), view.cover) {
            view.set_cover(!view.cover);
        }
    }
    let reading = reading::reads(view);
    choice_row(ui, t, i18n::t("Fit"), |ui| {
        if ui.selectable_label(!reading && view.fit == Fit::Width, i18n::t("Width")).clicked() {
            view.set_fit(Fit::Width);
        }
        if ui.selectable_label(!reading && view.fit == Fit::Page, i18n::t("Page")).clicked() {
            view.set_fit(Fit::Page);
        }
        if ui.selectable_label(reading, i18n::t("Read")).on_hover_text(i18n::t("A comfortable reading width (view.reading_width_px)")).clicked() {
            reading::fit_read(view, reading_width);
        }
    });
    command
}

/// Trim (Off, Auto, Manual) and, for Manual, the page previews to drag the kept part on.
fn trim_rows(view: &mut DocView, pages: &[(f32, f32)], ui: &mut egui::Ui, t: &Tokens) {
    let (on, manual) = (view.margin.trim.on, view.margin.trim.manual);
    let mut pick = None;
    choice_row(ui, t, i18n::t("Trim"), |ui| {
        for (choice, name) in [((true, true), "Manual"), ((true, false), "Auto"), ((false, manual), "Off")] {
            let selected = if choice.0 { on && manual == choice.1 } else { !on };
            if ui.selectable_label(selected, i18n::t(name)).clicked() {
                pick = Some(choice);
            }
        }
    });
    if let Some((on, manual)) = pick {
        // Manual starts from what auto trim found on this page.
        if manual && !view.margin.trim.manual && view.margin.trim.odd == Margins::default() {
            let found = view.margin.trim.crop(view.current);
            let current = view.current;
            view.margin.trim.set_margins(current, trim::to_margins(found));
        }
        let place = reading::place(view);
        view.margin.trim.on = on;
        view.margin.trim.manual = manual;
        view.goto = Some(place);
    }
    if !(view.margin.trim.on && view.margin.trim.manual) {
        return;
    }
    let current = view.current;
    let separate = view.margin.trim.separate;
    // Odd pages are 1, 3, … (0-based even indices).
    let shown: Vec<(usize, &str)> = if separate {
        let odd = current - current % 2;
        vec![(odd, "Odd pages"), (odd + 1, "Even pages")]
    } else {
        vec![(current, "All pages")]
    };
    let mut changed = false;
    ui.horizontal(|ui| {
        for (page, label) in shown {
            ui.vertical(|ui| {
                changed |= trim_preview(view, page, pages.get(page.min(pages.len().saturating_sub(1))).copied(), label, ui, t);
                ui.label(egui::RichText::new(i18n::t(label)).font(theme::regular(11.0)).color(t.text_faint));
            });
        }
    });
    ui.label(egui::RichText::new(i18n::t("Drag across a page to keep that part")).font(theme::regular(11.5)).color(t.text_faint));
    let sub = i18n::t("For scanned books, whose margins swap sides");
    if switch_row(ui, t, i18n::t("Odd and even pages separately"), Some(sub), separate) {
        let trim = &mut view.margin.trim;
        trim.separate = !separate;
        if trim.separate {
            trim.even = trim.odd;
        }
        changed = true;
    }
    if changed {
        view.goto = Some(reading::place(view));
    }
}

/// One page preview of the manual trim: the page (its raster when one has arrived) with the
/// trimmed-off part shaded. A drag across it sets the margins of `page`'s parity. True when
/// they changed.
fn trim_preview(view: &mut DocView, page: usize, size: Option<(f32, f32)>, label: &str, ui: &mut egui::Ui, t: &Tokens) -> bool {
    let (w, h) = size.filter(|(w, h)| *w > 0.0 && *h > 0.0).unwrap_or((1.0, 1.41));
    let preview = vec2(TRIM_PREVIEW_WIDTH, TRIM_PREVIEW_WIDTH * h / w);
    let (rect, resp) = ui.allocate_exact_size(preview, Sense::drag());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, format!("Trim preview: {label}")));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, view.page_color.paper());
    if let Some(tex) = view.page_texture(page) {
        painter.image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    let start_id = resp.id.with("trim-drag-start");
    let mut changed = false;
    if resp.drag_started()
        && let Some(at) = resp.interact_pointer_pos()
    {
        ui.data_mut(|d| d.insert_temp(start_id, at));
    }
    if resp.dragged()
        && let (Some(start), Some(at)) = (ui.data(|d| d.get_temp::<egui::Pos2>(start_id)), resp.interact_pointer_pos())
    {
        let kept = Rect::from_two_pos(start, at).intersect(rect);
        if kept.width() >= rect.width() * MIN_TRIM_DRAG && kept.height() >= rect.height() * MIN_TRIM_DRAG {
            let margins = [
                (kept.left() - rect.left()) / rect.width(),
                (kept.top() - rect.top()) / rect.height(),
                (rect.right() - kept.right()) / rect.width(),
                (rect.bottom() - kept.bottom()) / rect.height(),
            ];
            view.margin.trim.set_margins(page, margins);
            changed = true;
        }
    }
    let [u0, v0, u1, v1] = trim::from_margins(view.margin.trim.margins(page));
    let kept = Rect::from_min_max(rect.lerp_inside(vec2(u0, v0)), rect.lerp_inside(vec2(u1, v1)));
    let shade = Color32::from_black_alpha(TRIMMED_SHADE);
    for part in [
        Rect::from_min_max(rect.min, pos2(rect.right(), kept.top())),
        Rect::from_min_max(pos2(rect.left(), kept.bottom()), rect.max),
        Rect::from_min_max(pos2(rect.left(), kept.top()), pos2(kept.left(), kept.bottom())),
        Rect::from_min_max(pos2(kept.right(), kept.top()), pos2(rect.right(), kept.bottom())),
    ] {
        painter.rect_filled(part, CornerRadius::ZERO, shade);
    }
    painter.rect_stroke(kept, CornerRadius::ZERO, Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
    painter.rect_stroke(rect, CornerRadius::ZERO, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    resp.on_hover_cursor(egui::CursorIcon::Crosshair);
    changed
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
    fn settings_round_trip_through_the_doc_layer() {
        let mut s = DisplayState::default();
        s.pick(PageMode::Ink);
        s.color.brightness = 90;
        s.color.keep_hues = false;
        let layer = serde_json::json!({ "view": s.settings_values() });
        let loaded = margin_config::Settings::default().with_layer(&layer, "test");
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(DisplayState::from_settings(&loaded.settings.view), s);
        let view = margin_config::View { brightness: 4000, ..margin_config::View::default() };
        assert_eq!(DisplayState::from_settings(&view).color.brightness, *BRIGHTNESS_RANGE.end());
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
