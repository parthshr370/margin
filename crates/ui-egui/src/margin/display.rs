//! Margin's Display popover and page colour state.
//!
//! [`DisplayState`] is one document's choice: it opens from `settings.jsonc` (`view.page_color`
//! and its neighbours) under the document's sidecar layer, and every change goes back to the
//! sidecar (`margin::reading`). [`PageColorCache`] lives in each `DocView`: it recolours rasters
//! on their way to the GPU and keeps the source pixels of the pages around the current one, so a
//! mode switch re-uploads only the pages on screen, without re-rendering them.

use std::collections::HashMap;

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, TextureHandle, TextureOptions, pos2, vec2};
use pdfcraft_render::Pixels;
use serde_json::{Map, Value};

use super::figures::{Figures, Frame, Raster};
use super::look::{self, CONTROL_ROW, GAP, GAP_LARGE, GAP_SECTION, Segment};
use super::motion;
use super::pagecolor::{BRIGHTNESS_RANGE, CONTRAST_RANGE, NEUTRAL_PERCENT, PageColor, PageMode};
use super::reading::{self, DefaultRow};
use super::trim::{self, Margins};
use crate::canvas::{Fit, PageLayout};
use crate::theme::{self, Tokens};
use crate::{DocView, Flavor, PdfCraftApp, i18n};

/// The name of the switch that ties the page colour to the theme (screen readers and tests).
pub const FOLLOW_THEME_LABEL: &str = "Match the theme";

/// A document's page colour choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayState {
    pub color: PageColor,
    /// Night while the theme in use is dark, Paper while it is light. Picking a mode by hand turns
    /// it off.
    pub follow_system: bool,
}

impl Default for DisplayState {
    fn default() -> Self {
        Self::from_settings(&margin_config::View::default())
    }
}

impl DisplayState {
    /// The page colour in effect: the saved one, with the mode matching the theme in use (dark
    /// theme: Night, light theme: Paper) when asked to. The theme itself follows the system when
    /// `theme.mode` is "system".
    pub fn effective(&self, theme_dark: bool) -> PageColor {
        let mode = match (self.follow_system, theme_dark) {
            (true, true) => PageMode::Night,
            (true, false) => PageMode::Paper,
            (false, _) => self.color.mode,
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
    Some(view.margin.display.effective(Tokens::get(ctx).dark()))
}

/// Pages within this distance of the current page keep their source pixels for recolouring.
pub(crate) const SOURCE_NEAR: usize = 2;

/// The colour key of pixels kept as rendered (PdfCraft, and every thumbnail).
pub(crate) const AS_RENDERED: u64 = 0;

/// One document view's recolouring: the colour in effect, the source pixels of nearby pages and
/// each page's figures.
#[derive(Default)]
pub struct PageColorCache {
    color: Option<PageColor>,
    sources: HashMap<usize, Source>,
    figures: Figures,
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
        color.map_or(AS_RENDERED, PageColor::key)
    }

    /// Identifies the colour textures are recoloured with now ([`AS_RENDERED`]: none).
    pub fn key(&self) -> u64 {
        self.key_of(self.color)
    }

    /// The paper colour drawn under a page until its raster arrives.
    pub fn paper(&self) -> Color32 {
        let [r, g, b] = self.color.map_or(PageMode::Paper, |c| c.mode).paper();
        Color32::from_rgb(r, g, b)
    }

    /// Recolour a rendered raster in the current colour, in place. A whole page near `current`
    /// keeps its source pixels to recolour later; a whole page also settles the page's figures
    /// when the colour wants them (`margin::figures`). The flag: this page's tiles were coloured
    /// before its figures were known and are out of date.
    pub(crate) fn image(&mut self, doc: &pdfcraft_engine::Document, raster: Raster, current: usize) -> (Pixels, bool) {
        let Some(color) = self.color else { return (raster.rgba, false) };
        let info = doc.info.pages.get(raster.page);
        let frame = Frame::of(&raster, info);
        let Raster { page, tile, size, rgba, .. } = raster;
        // The words are the pixels' RGBA bytes in memory order (`Pixels`), recoloured as bytes.
        let mut words = rgba.into_words();
        let bytes: &mut [u8] = bytemuck::cast_slice_mut(&mut words);
        if tile.is_none() {
            if page.abs_diff(current) <= SOURCE_NEAR {
                self.sources.insert(page, Source { size, rgba: bytes.to_vec() });
            } else {
                self.sources.remove(&page);
            }
        }
        let mut stale_tiles = false;
        let figures = if !color.wants_figures() {
            &[][..]
        } else if tile.is_some() {
            self.figures.sync_generation(doc.edit_generation());
            self.figures.for_tile(page)
        } else {
            self.figures.sync_generation(doc.edit_generation());
            self.figures.learn_page(page, &doc.info, || doc.page_images(page).into_iter().map(|image| image.rect).collect());
            stale_tiles = self.figures.decide(page, bytes, size[0], size[1]);
            self.figures.decided(page)
        };
        color.apply(bytes, size[0], size[1], &frame.all_pixels(figures));
        (Pixels::from(words), stale_tiles)
    }

    /// Recolour `page`'s texture in place from its kept source. False when there is no source,
    /// or its figures are wanted and its images not looked up yet (the page must be rendered
    /// again).
    pub fn recolor(&mut self, page: usize, tex: &mut TextureHandle) -> bool {
        let (Some(color), Some(source)) = (self.color, self.sources.get(&page)) else { return false };
        let [width, height] = source.size;
        if color.wants_figures() && !self.figures.is_decided(page) {
            if !self.figures.knows_images(page) {
                return false;
            }
            // A colour change clears every tile already (`canvas`), so no tile is out of date.
            self.figures.decide(page, &source.rgba, width, height);
        }
        let figures = if color.wants_figures() { Frame::whole(width, height).all_pixels(self.figures.decided(page)) } else { Vec::new() };
        let mut rgba = source.rgba.clone();
        color.apply(&mut rgba, width, height, &figures);
        tex.set(egui::ColorImage::from_rgba_premultiplied(source.size, &rgba), TextureOptions::LINEAR);
        true
    }

    /// Drop the sources of pages that moved away from `current`.
    pub fn retain_near(&mut self, current: usize) {
        self.sources.retain(|page, _| page.abs_diff(current) <= SOURCE_NEAR);
    }

    /// Drop every kept source: the view is hidden and its page textures are released.
    pub(crate) fn forget_sources(&mut self) {
        self.sources.clear();
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
/// A row holding a compact action (Reset under the sliders, a section header with "Set as
/// default"): §6's 40-point control slot.
const ACTION_ROW: f32 = 40.0;
/// The mode description keeps room for this many lines, so the rows under it stay put when the
/// mode (and its description's length) changes.
const DESCRIPTION_LINES: f32 = 2.0;
/// Text sizes: row labels, the line under a switch label, descriptions and links.
const LABEL_TEXT: f32 = 12.5;
const SUB_TEXT: f32 = 11.5;
const BODY_TEXT: f32 = 12.0;
/// Space between a switch label and the line under it.
const SUB_GAP: f32 = 2.0;
/// The body's least height when the window is too short for it: a few control rows still show.
const MIN_BODY: f32 = 4.0 * CONTROL_ROW;
/// The scroll bar's own column right of the body, while the body scrolls.
const SCROLL_GUTTER: f32 = 12.0;

/// The trim editor's page previews.
const TRIM_PREVIEW_WIDTH: f32 = 96.0;
/// A drag smaller than this share of the preview on either side is a slip, not a crop.
const MIN_TRIM_DRAG: f32 = 0.1;
/// How dark the trimmed-off part of a preview is drawn.
const TRIMMED_SHADE: u8 = 150;

/// The popover's frame: the theme's popup (Margin's floating level, edge and L shadow) at the
/// floating radius, padded by the section step on every side.
pub fn popover_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::popup(style).corner_radius(look::RADIUS_FLOATING).inner_margin(GAP_SECTION)
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
    // Never taller than the window under the popover's top: in a short window (Two-up's cover
    // row, Manual trim's previews) the body scrolls, its frame padding and a section step clear
    // of the window's edge, so no lower control is out of reach. The popup's area offers only
    // last frame's size, so the body asks for the room itself (as its least scrolled height) and
    // still shrinks to its content. The scroll bar's gutter is always reserved, so the popover
    // keeps one width whether or not the body scrolls. Wheel and scroll bar only: a drag in the
    // body belongs to its sliders and trim previews.
    let room = (ui.ctx().content_rect().bottom() - ui.cursor().top() - 2.0 * GAP_SECTION).max(MIN_BODY);
    ui.set_width(POPOVER_WIDTH + SCROLL_GUTTER);
    let sources = egui::scroll_area::ScrollSource::SCROLL_BAR | egui::scroll_area::ScrollSource::MOUSE_WHEEL;
    egui::ScrollArea::vertical()
        .id_salt("margin-display-body")
        .scroll_source(sources)
        .auto_shrink([false, true])
        .max_height(room)
        .min_scrolled_height(room)
        .show(ui, |ui| {
            ui.set_width(POPOVER_WIDTH);
            body(app, index, ui, &t);
        });
}

/// [`popover_contents`] for the document `index`.
fn body(app: &mut PdfCraftApp, index: usize, ui: &mut egui::Ui, t: &Tokens) {
    let mut default_row = None;
    if section_header(ui, t, "Page color") {
        default_row = Some(DefaultRow::PageColor);
    }
    page_color_rows(&mut app.views[index].margin.display, ui, t);
    section_divider(ui, t);
    if section_header(ui, t, "Layout") {
        default_row = Some(DefaultRow::Layout);
    }
    let reading_width = app.margin_keys.settings().view.reading_width_px;
    if let Some(id) = layout_rows(&mut app.views[index], reading_width, ui, t) {
        app.execute(id);
    }
    let pages = app.session.get(app.views[index].id).map(|doc| doc.info.pages.iter().map(|p| (p.width, p.height)).collect::<Vec<_>>());
    trim_rows(&mut app.views[index], &pages.unwrap_or_default(), ui, t);
    section_divider(ui, t);
    let state = &mut app.views[index].margin.display;
    let follow_sub = if state.follow_system { "Night with a dark theme, Paper with a light one" } else { "Off: the page color stays as you set it" };
    if switch_row(ui, t, FOLLOW_THEME_LABEL, Some(follow_sub), state.follow_system, true) {
        state.follow_system = !state.follow_system;
    }
    if let Some(row) = default_row {
        let message = reading::set_as_default(app, index, row);
        app.notify(message);
    }
}

/// A section header (small caps title, a compact secondary "Set as default" on the right) and
/// the small step under it. True when "Set as default" was clicked.
fn section_header(ui: &mut egui::Ui, t: &Tokens, title: &str) -> bool {
    let clicked = ui
        .allocate_ui_with_layout(vec2(POPOVER_WIDTH, ACTION_ROW), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.label(look::section_caps(t, i18n::t(title), None));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                look::Button::secondary(look::ButtonSize::Compact, i18n::t("Set as default"))
                    .tip(i18n::t("Use this for documents opened for the first time (settings.jsonc)"))
                    .show(ui)
                    .clicked()
            })
            .inner
        })
        .inner;
    ui.add_space(GAP);
    clicked
}

/// The line between sections, across the whole popover (into the frame's padding, past the
/// scrolling body's clip), with the section step on both sides.
fn section_divider(ui: &mut egui::Ui, t: &Tokens) {
    ui.add_space(GAP_SECTION);
    let (rect, _) = ui.allocate_exact_size(vec2(POPOVER_WIDTH, 1.0), Sense::hover());
    let painter = ui.painter().with_clip_rect(ui.clip_rect().expand2(vec2(GAP_SECTION, 0.0)));
    painter.hline((rect.left() - GAP_SECTION)..=(rect.right() + GAP_SECTION), rect.center().y, Stroke::new(1.0, t.divider));
    ui.add_space(GAP_SECTION);
}

/// Swatches, the mode's description, keep figure hues, brightness, contrast and their reset.
fn page_color_rows(state: &mut DisplayState, ui: &mut egui::Ui, t: &Tokens) {
    let shown = state.effective(t.dark());
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
    // Always shown, so picking a mode never moves the rows below; only Ink and Mono enable it.
    let sub = i18n::t("Ink and Mono only: photos the page embeds keep their colours; drawings stay two-tone");
    if switch_row(ui, t, i18n::t("Keep figure hues"), Some(sub), state.color.keep_hues, shown.mode.offers_keep_hues()) {
        state.color.keep_hues = !state.color.keep_hues;
    }
    slider_row(ui, t, i18n::t("Brightness"), &mut state.color.brightness, BRIGHTNESS_RANGE, |v| v.to_string());
    let mono = shown.mode == PageMode::Mono;
    let (label, low, high) = (if mono { i18n::t("Cut-off") } else { i18n::t("Contrast") }, *CONTRAST_RANGE.start(), *CONTRAST_RANGE.end());
    let show = move |v: u16| if mono { format!("{}%", u32::from(v.saturating_sub(low)) * 100 / u32::from(high - low)) } else { v.to_string() };
    slider_row(ui, t, label, &mut state.color.contrast, CONTRAST_RANGE, show);
    ui.allocate_ui_with_layout(vec2(POPOVER_WIDTH, ACTION_ROW), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let reset = look::Button::secondary(look::ButtonSize::Compact, i18n::t("Reset"))
            .icon("rotate-ccw")
            .name(i18n::t("Reset brightness & contrast"))
            .tip(i18n::t("Reset brightness & contrast"))
            .show(ui);
        if reset.clicked() {
            state.color.brightness = NEUTRAL_PERCENT;
            state.color.contrast = NEUTRAL_PERCENT;
        }
    });
}

/// Page display (Scroll, Single, Two-up, through PdfCraft's commands, which it returns), the
/// cover page in Two-up, and fit (Width, Page, Reading).
fn layout_rows(view: &mut DocView, reading_width: u32, ui: &mut egui::Ui, t: &Tokens) -> Option<&'static str> {
    let layouts = [(PageLayout::Continuous, "Scroll"), (PageLayout::Single, "Single"), (PageLayout::TwoUp, "Two-up")];
    let segments = layouts.map(|(choice, name)| Segment { label: i18n::t(name), selected: view.layout == choice, tip: None });
    let command = choice_row(ui, t, i18n::t("Pages"), &segments).and_then(|i| layouts.get(i)).map(|(choice, _)| choice.command());
    // Always shown, so picking Two-up never moves the rows below; only Two-up enables it.
    let sub = i18n::t("Two-up only: the first page sits alone on the right, like a book's cover");
    if switch_row(ui, t, i18n::t("Cover page alone"), Some(sub), view.cover, view.cover_applies()) {
        view.set_cover(!view.cover);
    }
    let reading = reading::reads(view);
    let segments = [
        Segment { label: i18n::t("Width"), selected: !reading && view.fit == Fit::Width, tip: None },
        Segment { label: i18n::t("Page"), selected: !reading && view.fit == Fit::Page, tip: None },
        Segment { label: i18n::t("Reading"), selected: reading, tip: Some(i18n::t("A comfortable reading width (view.reading_width_px)")) },
    ];
    match choice_row(ui, t, i18n::t("Fit"), &segments) {
        Some(0) => reading::set_fit(view, Fit::Width),
        Some(1) => reading::set_fit(view, Fit::Page),
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
        let spot = reading::spot(view);
        view.margin.trim.on = on;
        view.margin.trim.manual = manual;
        reading::go(view, spot);
    }
    let shown = view.margin.trim.on && view.margin.trim.manual;
    // The editor slides open and shut instead of pushing the rows below in one frame.
    look::collapsible(ui, egui::Id::new("margin-display-manual-trim"), shown, |ui| manual_trim_editor(view, pages, ui, t));
}

/// Manual trim's editor: the page previews to drag the kept part on, the hint and the odd and
/// even switch. Both preview slots are always laid out, so the switch never resizes the block.
fn manual_trim_editor(view: &mut DocView, pages: &[(f32, f32)], ui: &mut egui::Ui, t: &Tokens) {
    let current = view.current;
    let separate = view.margin.trim.separate;
    // Odd pages are 1, 3, … (0-based even indices).
    let shown: [Option<(usize, &str)>; 2] = if separate {
        let odd = current - current % 2;
        [Some((odd, "Odd pages")), Some((odd + 1, "Even pages"))]
    } else {
        [Some((current, "All pages")), None]
    };
    let mut changed = false;
    ui.add_space(GAP);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = GAP_LARGE;
        for slot in shown {
            ui.vertical(|ui| {
                ui.set_width(TRIM_PREVIEW_WIDTH);
                let Some((page, label)) = slot else { return };
                ui.spacing_mut().item_spacing.y = GAP / 2.0;
                changed |= trim_preview(view, page, pages.get(page.min(pages.len().saturating_sub(1))).copied(), label, ui, t);
                ui.label(egui::RichText::new(i18n::t(label)).font(theme::regular(SUB_TEXT)).color(t.text_faint));
            });
        }
    });
    ui.add_space(GAP);
    ui.label(egui::RichText::new(i18n::t("Drag across a page to keep that part")).font(theme::regular(SUB_TEXT)).color(t.text_faint));
    let sub = i18n::t("For scanned books, whose margins swap sides");
    if switch_row(ui, t, i18n::t("Odd and even pages separately"), Some(sub), separate, true) {
        let trim = &mut view.margin.trim;
        trim.separate = !separate;
        if trim.separate {
            trim.even = trim.odd;
        }
        changed = true;
    }
    if changed {
        let spot = reading::spot(view);
        reading::go(view, spot);
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
    let edge = look::edge_stroke(t.border);
    if !edge.is_empty() {
        painter.rect_stroke(rect, CornerRadius::ZERO, edge, egui::StrokeKind::Inside);
    }
    resp.on_hover_cursor(egui::CursorIcon::Crosshair);
    changed
}

/// A row label in the label column shared by slider and choice rows. Returns the label's id,
/// which names the row's control for screen readers.
fn row_label(ui: &mut egui::Ui, t: &Tokens, label: &str) -> egui::Id {
    ui.allocate_ui_with_layout(vec2(LABEL_WIDTH, CONTROL_ROW), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_width(LABEL_WIDTH);
        ui.label(egui::RichText::new(label).font(theme::regular(LABEL_TEXT)).color(t.text_muted)).id
    })
    .inner
}

/// A control row: the label column, then a segmented control from the controls' shared start
/// (where [`slider_row`]'s slider starts), so Pages, Fit and Trim line up whatever their
/// choices' widths. Returns the clicked segment.
fn choice_row(ui: &mut egui::Ui, t: &Tokens, label: &str, segments: &[Segment<'_>]) -> Option<usize> {
    ui.allocate_ui_with_layout(vec2(POPOVER_WIDTH, CONTROL_ROW), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = GAP_LARGE;
        row_label(ui, t, label);
        look::segmented(ui, segments)
    })
    .inner
}

/// A swatch's corner radius.
const SWATCH_RADIUS: u8 = 9;

/// A page colour swatch: a small page in the mode's paper with lines of its ink, and the name. The
/// picked one sits on the selected fill and edge, its name in the selected label colour.
fn swatch(ui: &mut egui::Ui, t: &Tokens, mode: PageMode, selected: bool, width: f32) -> egui::Response {
    let name = i18n::t(mode.name());
    let (rect, resp) = ui.allocate_exact_size(vec2(width, SWATCH_HEIGHT + SWATCH_NAME_HEIGHT), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, name));
    let painter = ui.painter();
    // The pick moves between swatches with a crossfade, hover fills fade in and out.
    let picked = motion::toggle(ui.ctx(), resp.id.with("margin-picked"), selected, motion::FADE);
    let hovered = motion::hover(ui.ctx(), resp.id, resp.hovered() && !selected);
    if hovered > 0.0 {
        painter.rect_filled(rect, CornerRadius::same(SWATCH_RADIUS), t.hover.gamma_multiply(hovered));
    }
    let surfaces = t.surfaces;
    if picked > 0.0 {
        let stroke = look::edge_stroke(surfaces.selected_edge.gamma_multiply(picked));
        painter.rect(rect, CornerRadius::same(SWATCH_RADIUS), t.selected.gamma_multiply(picked), stroke, egui::StrokeKind::Inside);
    }
    look::focus_ring(ui, t, rect, &resp, SWATCH_RADIUS);
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
    let name_color = t.text.lerp_to_gamma(surfaces.selected_text, picked);
    painter.text(pos2(rect.center().x, page.bottom() + 14.0), Align2::CENTER_CENTER, name, theme::regular(12.0), name_color);
    resp
}

/// A label (with an optional line under it) and a switch on the right: a [`CONTROL_ROW`] at
/// least, the text padded by half the small step. A row that doesn't apply stays in place,
/// disabled, so the rows below never move. True when clicked.
fn switch_row(ui: &mut egui::Ui, t: &Tokens, label: &str, sub: Option<&str>, on: bool, enabled: bool) -> bool {
    ui.allocate_ui_with_layout(vec2(POPOVER_WIDTH, CONTROL_ROW), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_height(CONTROL_ROW);
        ui.add_enabled_ui(enabled, |ui| {
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
    })
    .inner
}

/// A control row: the label column, the slider, the value right-aligned in its column;
/// double-clicking the slider resets it to 100 %.
fn slider_row(ui: &mut egui::Ui, t: &Tokens, label: &str, value: &mut u16, range: std::ops::RangeInclusive<u16>, show: impl Fn(u16) -> String) {
    ui.allocate_ui_with_layout(vec2(POPOVER_WIDTH, CONTROL_ROW), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = GAP_LARGE;
        let name = row_label(ui, t, label);
        ui.spacing_mut().slider_width = POPOVER_WIDTH - LABEL_WIDTH - VALUE_WIDTH - 2.0 * GAP_LARGE;
        look::slider_style(ui, t);
        // A slider senses drags, not clicks, so its response never reports a double-click.
        let slider = ui.add(egui::Slider::new(value, range).show_value(false)).labelled_by(name);
        let resp = look::tooltip(slider, i18n::t("Double-click to reset"), None);
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
        assert_eq!(s.effective(false).mode, PageMode::Paper);
        assert_eq!(s.effective(true).mode, PageMode::Night);
        s.pick(PageMode::Sepia);
        assert!(!s.follow_system);
        assert_eq!(s.effective(true).mode, PageMode::Sepia);
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

    /// A blank one-page PDF.
    const BLANK: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >> endobj
trailer << /Root 1 0 R >>
%%EOF";

    fn whole_page(page: usize) -> Raster {
        Raster { page, tile: None, scale: 1.0, size: [1, 1], rgba: Pixels::from(vec![u32::MAX]) }
    }

    /// The first pixel of `rgba`.
    fn first_pixel(rgba: &Pixels) -> Color32 {
        Color32::from_rgba_premultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
    }

    #[test]
    fn cache_keeps_sources_near_the_current_page_only() {
        let mut session = pdfcraft_engine::Session::new();
        let id = session.open("blank.pdf", None, std::sync::Arc::new(BLANK.to_vec()), None).expect("opens");
        let doc = session.get(id).expect("open");
        let mut cache = PageColorCache::default();
        assert!(!cache.configure(None));
        let night = PageColor { mode: PageMode::Night, ..PageColor::default() };
        assert!(cache.configure(Some(night)));
        assert!(!cache.configure(Some(night)));
        let (img, _) = cache.image(doc, whole_page(3), 3);
        assert_eq!(first_pixel(&img), Color32::from_rgb(0x14, 0x13, 0x12));
        let _ = cache.image(doc, whole_page(9), 3);
        assert!(cache.sources.contains_key(&3) && !cache.sources.contains_key(&9));
        cache.retain_near(8);
        assert!(cache.sources.is_empty());
        // PdfCraft: pixels pass through and nothing is kept.
        assert!(cache.configure(None));
        let (img, _) = cache.image(doc, whole_page(0), 0);
        assert_eq!(first_pixel(&img), Color32::WHITE);
        assert!(cache.sources.is_empty());
    }
}
