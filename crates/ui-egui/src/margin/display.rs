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

use super::look::{self, CONTROL_ROW, GAP, GAP_LARGE, GAP_SECTION, Segment};
use super::motion;
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
const SWATCH_HEIGHT: f32 = 62.0;
/// The swatch's name line under its page preview.
const SWATCH_NAME_HEIGHT: f32 = 34.0;
/// The label column of slider and choice rows (the mockup's `.ctl` grid: 92 px).
const LABEL_WIDTH: f32 = 92.0;
const VALUE_WIDTH: f32 = 36.0;
/// A row holding only a text link ("Reset brightness & contrast"): shorter than a control row.
const LINK_ROW: f32 = 24.0;
/// The section header row (small caps label and "Set as default").
const SECTION_HEADER: f32 = 16.0;
/// The mode description keeps room for this many lines, so the rows under it stay put when the
/// mode (and its description's length) changes.
const DESCRIPTION_LINES: f32 = 2.0;
/// Text sizes: row labels, the line under a switch label, descriptions and links.
const LABEL_TEXT: f32 = 12.5;
const SUB_TEXT: f32 = 11.5;
const BODY_TEXT: f32 = 12.0;
/// Space between a switch label and the line under it.
const SUB_GAP: f32 = 2.0;

/// The trim editor's page previews.
const TRIM_PREVIEW_WIDTH: f32 = 96.0;
/// A drag smaller than this share of the preview on either side is a slip, not a crop.
const MIN_TRIM_DRAG: f32 = 0.1;
/// How dark the trimmed-off part of a preview is drawn.
const TRIMMED_SHADE: u8 = 150;

/// The popover's frame: the theme's popup, padded by the section step on every side.
pub fn popover_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::popup(style).inner_margin(GAP_SECTION)
}

/// The Display popover's body for the active document, laid out like the mockup: page colour
/// swatches, keep figure hues (Ink and Mono), brightness and contrast (double-click a slider to
/// reset it) and their reset; then layout, cover page, fit and trim; then follow system. "Set
/// as default" copies the page colour or layout into `settings.jsonc`. The caller owns the popup
/// ([`popover_frame`]) and its placement. Every gap is a step of `look`'s spacing scale: the
/// frame and both sides of a divider get the section step, a header is followed by the small
/// step, controls sit in rows of [`CONTROL_ROW`].
pub fn popover_contents(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.set_width(POPOVER_WIDTH);
    ui.spacing_mut().item_spacing.y = 0.0;
    let Some(index) = app.active.filter(|i| *i < app.views.len()) else {
        ui.label(egui::RichText::new(i18n::t("Open a document to change how it shows.")).font(theme::regular(LABEL_TEXT)).color(t.text_muted));
        return;
    };
    let mut default_row = None;
    if section_header(ui, &t, "Page color") {
        default_row = Some(DefaultRow::PageColor);
    }
    page_color_rows(&mut app.views[index].margin.display, ui, &t);
    section_divider(ui, &t);
    if section_header(ui, &t, "Layout") {
        default_row = Some(DefaultRow::Layout);
    }
    let reading_width = app.margin_keys.settings().view.reading_width_px;
    if let Some(id) = layout_rows(&mut app.views[index], reading_width, ui, &t) {
        app.execute(id);
    }
    let pages = app.session.get(app.views[index].id).map(|doc| doc.info.pages.iter().map(|p| (p.width, p.height)).collect::<Vec<_>>());
    trim_rows(&mut app.views[index], &pages.unwrap_or_default(), ui, &t);
    section_divider(ui, &t);
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

/// A section header (small caps title, "Set as default" on the right) and the small step under
/// it. True when "Set as default" was clicked.
fn section_header(ui: &mut egui::Ui, t: &Tokens, title: &str) -> bool {
    let clicked = ui
        .allocate_ui_with_layout(vec2(POPOVER_WIDTH, SECTION_HEADER), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.label(look::section_caps(t, i18n::t(title), None));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = egui::RichText::new(i18n::t("Set as default")).font(theme::regular(BODY_TEXT)).color(t.accent_text);
                ui.add(egui::Label::new(label).sense(Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(i18n::t("Use this for documents opened for the first time (settings.jsonc)"))
                    .clicked()
            })
            .inner
        })
        .inner;
    ui.add_space(GAP);
    clicked
}

/// The line between sections, across the whole popover (into the frame's padding), with the
/// section step on both sides.
fn section_divider(ui: &mut egui::Ui, t: &Tokens) {
    ui.add_space(GAP_SECTION);
    let (rect, _) = ui.allocate_exact_size(vec2(POPOVER_WIDTH, 1.0), Sense::hover());
    ui.painter().hline((rect.left() - GAP_SECTION)..=(rect.right() + GAP_SECTION), rect.center().y, Stroke::new(1.0, t.divider));
    ui.add_space(GAP_SECTION);
}

/// Swatches, the mode's description, keep figure hues, brightness, contrast and their reset.
fn page_color_rows(state: &mut DisplayState, ui: &mut egui::Ui, t: &Tokens) {
    let shown = state.effective(ui.ctx().system_theme());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        let w = (POPOVER_WIDTH - GAP * 4.0) / 5.0;
        for mode in PageMode::ALL {
            if swatch(ui, t, mode, mode == shown.mode, w).clicked() {
                state.pick(mode);
            }
        }
    });
    ui.add_space(GAP_LARGE);
    let font = theme::regular(BODY_TEXT);
    let description_height = DESCRIPTION_LINES * ui.fonts_mut(|f| f.row_height(&font));
    ui.allocate_ui_with_layout(vec2(POPOVER_WIDTH, description_height), egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_min_height(description_height);
        ui.add(egui::Label::new(egui::RichText::new(i18n::t(shown.mode.description())).font(font).color(t.text_muted)).wrap());
    });
    ui.add_space(GAP);
    if shown.mode.offers_keep_hues() {
        let sub = i18n::t("Ink and Mono only: figures get Night colors, not two-tone");
        if switch_row(ui, t, i18n::t("Keep figure hues"), Some(sub), state.color.keep_hues) {
            state.color.keep_hues = !state.color.keep_hues;
        }
    }
    slider_row(ui, t, i18n::t("Brightness"), &mut state.color.brightness, BRIGHTNESS_RANGE, |v| v.to_string());
    let mono = shown.mode == PageMode::Mono;
    let (label, low, high) = (if mono { i18n::t("Cut-off") } else { i18n::t("Contrast") }, *CONTRAST_RANGE.start(), *CONTRAST_RANGE.end());
    let show = move |v: u16| if mono { format!("{}%", u32::from(v.saturating_sub(low)) * 100 / u32::from(high - low)) } else { v.to_string() };
    slider_row(ui, t, label, &mut state.color.contrast, CONTRAST_RANGE, show);
    ui.allocate_ui_with_layout(vec2(POPOVER_WIDTH, LINK_ROW), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let reset = egui::RichText::new(i18n::t("Reset brightness & contrast")).font(theme::regular(BODY_TEXT)).color(t.accent_text);
        if ui.add(egui::Label::new(reset).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            state.color.brightness = NEUTRAL_PERCENT;
            state.color.contrast = NEUTRAL_PERCENT;
        }
    });
}

/// Page display (Scroll, Single, Two-up, through PdfCraft's commands, which it returns), the
/// cover page in Two-up, and fit (Width, Page, Read).
fn layout_rows(view: &mut DocView, reading_width: u32, ui: &mut egui::Ui, t: &Tokens) -> Option<&'static str> {
    let layouts = [(PageLayout::Continuous, "Scroll"), (PageLayout::Single, "Single"), (PageLayout::TwoUp, "Two-up")];
    let segments = layouts.map(|(choice, name)| Segment { label: i18n::t(name), selected: view.layout == choice, tip: None });
    let command = choice_row(ui, t, i18n::t("Pages"), &segments).and_then(|i| layouts.get(i)).map(|(choice, _)| choice.command());
    if view.cover_applies() {
        let sub = i18n::t("The first page sits alone on the right, like a book's cover");
        if switch_row(ui, t, i18n::t("Cover page alone"), Some(sub), view.cover) {
            view.set_cover(!view.cover);
        }
    }
    let reading = reading::reads(view);
    let segments = [
        Segment { label: i18n::t("Width"), selected: !reading && view.fit == Fit::Width, tip: None },
        Segment { label: i18n::t("Page"), selected: !reading && view.fit == Fit::Page, tip: None },
        Segment { label: i18n::t("Read"), selected: reading, tip: Some(i18n::t("A comfortable reading width (view.reading_width_px)")) },
    ];
    match choice_row(ui, t, i18n::t("Fit"), &segments) {
        Some(0) => view.set_fit(Fit::Width),
        Some(1) => view.set_fit(Fit::Page),
        Some(_) => reading::fit_read(view, reading_width),
        None => {}
    }
    command
}

/// Trim (Off, Auto, Manual) and, for Manual, the page previews to drag the kept part on.
fn trim_rows(view: &mut DocView, pages: &[(f32, f32)], ui: &mut egui::Ui, t: &Tokens) {
    let (on, manual) = (view.margin.trim.on, view.margin.trim.manual);
    let choices = [((false, manual), "Off"), ((true, false), "Auto"), ((true, true), "Manual")];
    let segments = choices.map(|(choice, name)| {
        let selected = if choice.0 { on && manual == choice.1 } else { !on };
        Segment { label: i18n::t(name), selected, tip: None }
    });
    let pick = choice_row(ui, t, i18n::t("Trim"), &segments).and_then(|i| choices.get(i)).map(|(choice, _)| *choice);
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
    ui.add_space(GAP);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = GAP_LARGE;
        for (page, label) in shown {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = GAP / 2.0;
                changed |= trim_preview(view, page, pages.get(page.min(pages.len().saturating_sub(1))).copied(), label, ui, t);
                ui.label(egui::RichText::new(i18n::t(label)).font(theme::regular(SUB_TEXT)).color(t.text_faint));
            });
        }
    });
    ui.add_space(GAP);
    ui.label(egui::RichText::new(i18n::t("Drag across a page to keep that part")).font(theme::regular(SUB_TEXT)).color(t.text_faint));
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

/// A row label in the label column shared by slider and choice rows.
fn row_label(ui: &mut egui::Ui, t: &Tokens, label: &str) {
    ui.allocate_ui_with_layout(vec2(LABEL_WIDTH, CONTROL_ROW), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_width(LABEL_WIDTH);
        ui.label(egui::RichText::new(label).font(theme::regular(LABEL_TEXT)).color(t.text_muted));
    });
}

/// A control row: the label column, then a segmented control against the right edge. Returns
/// the clicked segment.
fn choice_row(ui: &mut egui::Ui, t: &Tokens, label: &str, segments: &[Segment<'_>]) -> Option<usize> {
    ui.allocate_ui_with_layout(vec2(POPOVER_WIDTH, CONTROL_ROW), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        row_label(ui, t, label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| look::segmented(ui, segments)).inner
    })
    .inner
}

/// A page colour swatch: a small page in the mode's paper with lines of its ink, and the name.
fn swatch(ui: &mut egui::Ui, t: &Tokens, mode: PageMode, selected: bool, width: f32) -> egui::Response {
    let name = i18n::t(mode.name());
    let (rect, resp) = ui.allocate_exact_size(vec2(width, SWATCH_HEIGHT + SWATCH_NAME_HEIGHT), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, name));
    let painter = ui.painter();
    // The pick moves between swatches with a crossfade, hover fills fade in and out.
    let picked = motion::toggle(ui.ctx(), resp.id.with("margin-picked"), selected, motion::FADE);
    let hovered = motion::hover(ui.ctx(), resp.id, resp.hovered() && !selected);
    if hovered > 0.0 {
        painter.rect_filled(rect, CornerRadius::same(9), t.hover.gamma_multiply(hovered));
    }
    if picked > 0.0 {
        let stroke = Stroke::new(1.0, t.accent.gamma_multiply(picked));
        painter.rect(rect, CornerRadius::same(9), t.accent_soft.gamma_multiply(picked), stroke, egui::StrokeKind::Inside);
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

/// A label (with an optional line under it) and a switch on the right: a [`CONTROL_ROW`] at
/// least, the text padded by half the small step. True when clicked.
fn switch_row(ui: &mut egui::Ui, t: &Tokens, label: &str, sub: Option<&str>, on: bool) -> bool {
    ui.allocate_ui_with_layout(vec2(POPOVER_WIDTH, CONTROL_ROW), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_height(CONTROL_ROW);
        ui.vertical(|ui| {
            ui.set_width(POPOVER_WIDTH - look::SWITCH_SIZE.x - GAP_LARGE);
            ui.spacing_mut().item_spacing.y = SUB_GAP;
            ui.add_space(GAP / 2.0);
            ui.label(egui::RichText::new(label).font(theme::regular(LABEL_TEXT)).color(t.text_muted));
            if let Some(sub) = sub {
                ui.add(egui::Label::new(egui::RichText::new(sub).font(theme::regular(SUB_TEXT)).color(t.text_faint)).wrap());
            }
            ui.add_space(GAP / 2.0);
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| look::switch(ui, on, label).clicked()).inner
    })
    .inner
}

/// A control row: the label column, the slider, the value right-aligned in its column;
/// double-clicking the slider resets it to 100 %.
fn slider_row(ui: &mut egui::Ui, t: &Tokens, label: &str, value: &mut u16, range: std::ops::RangeInclusive<u16>, show: impl Fn(u16) -> String) {
    ui.allocate_ui_with_layout(vec2(POPOVER_WIDTH, CONTROL_ROW), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = GAP_LARGE;
        row_label(ui, t, label);
        ui.spacing_mut().slider_width = POPOVER_WIDTH - LABEL_WIDTH - VALUE_WIDTH - 2.0 * GAP_LARGE;
        look::slider_style(ui, t);
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
        ui.allocate_ui_with_layout(vec2(VALUE_WIDTH, CONTROL_ROW), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.set_min_width(VALUE_WIDTH);
            ui.label(egui::RichText::new(show(*value)).font(theme::regular(SUB_TEXT)).color(t.text_faint));
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
