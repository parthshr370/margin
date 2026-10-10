//! Margin's shared look, the owner's Obsidian "Ember Night": list rows, icon buttons, section
//! labels and tooltips. Hovered and selected rows and icon buttons share one pill (the theme's
//! `hover`, #1b1b1b, opaque, at the theme's 6-point radius), with no accent bar; a selected one
//! has accent text and icon, a hovered one bright text and an accent icon; secondary columns
//! (page numbers) stay muted; a disabled control stays muted and gets no pill. Colours and the
//! corner radius are the theme's [`Tokens`], so the palette lives in one place (`theme.rs`);
//! the two Ember colours that are not tokens, the tooltip card and the accent button's hover,
//! are named here.
//!
//! PdfCraft's panels that Margin's sidebar hosts (Bookmarks, Pages, the Highlights list) paint
//! their rows here when their [`RowLook`] is [`RowLook::Ember`].

use egui::{Color32, CornerRadius, Rect, Response, Sense, Stroke, Vec2, vec2};
use pdfcraft_engine::commands;
use serde_json::Value;

use crate::margin::{keysheet, motion};
use crate::theme::{self, Tokens};
use crate::{Flavor, PdfCraftApp, icons};

/// Spacing scale, used for every gap: the small step (between related things), the large one
/// (between groups, and the sidebar's edge padding) and the section step (a popover's edge and
/// the space either side of a section divider).
pub const GAP: f32 = 8.0;
pub const GAP_LARGE: f32 = 12.0;
pub const GAP_SECTION: f32 = 16.0;
/// Height of one control row (a label and its slider, switch or segmented choice).
pub const CONTROL_ROW: f32 = 32.0;
/// Space between a list row's edge and its text; section labels line up with it.
pub const ROW_PAD: f32 = 8.0;
/// A section label's size and letter spacing (small caps: uppercase, tracked out).
const LABEL_SIZE: f32 = 11.0;
const LABEL_TRACKING: f32 = 0.9;
/// An icon's size within its button.
const ICON_SHARE: f32 = 0.5;
/// The switch (mockup `.sw-t`: 30×18, knob inset 2).
pub(crate) const SWITCH_SIZE: Vec2 = Vec2::new(30.0, 18.0);
const SWITCH_KNOB_INSET: f32 = 2.0;
/// The segmented control (mockup `.seg`): its height inside a [`CONTROL_ROW`], the well's inset
/// around and between the chips, a chip's side padding and its text size.
const SEGMENT_HEIGHT: f32 = 28.0;
const SEGMENT_INSET: f32 = 2.0;
const SEGMENT_PAD_X: f32 = 10.0;
const SEGMENT_TEXT: f32 = 12.0;
/// A text button's height (Save, Cancel, the notes box's buttons) and its text size.
pub const BUTTON_HEIGHT: f32 = 28.0;
const BUTTON_TEXT: f32 = 12.5;
/// An icon's size on a text button.
const BUTTON_ICON: f32 = 14.0;
/// The accent button keeps this share of its colour while pressed.
const ACCENT_PRESSED_SHADE: f32 = 0.85;
/// An accent-filled button's hover colour: Ember's `--interactive-accent-hover` (`--magenta`).
const ACCENT_HOVER: Color32 = Color32::from_rgb(0xEE, 0x79, 0x48);
/// The tooltip card's fill, measured on the owner's Obsidian tooltip (#1a1a1a, opaque).
const TOOLTIP_FILL: Color32 = Color32::from_rgb(0x1A, 0x1A, 0x1A);
/// The tooltip's padding, its distance from its control (room for the arrow) and the arrow's
/// half-width and height.
const TOOLTIP_PAD: egui::Margin = egui::Margin::symmetric(8, 5);
const TOOLTIP_GAP: f32 = 8.0;
const TOOLTIP_ARROW: f32 = 5.0;
/// A colour row's cells (the mockup's `.cdot`: 30 px, 14 px dots), the gap between them, and
/// the ring around the current colour (its width and its gap from the dot).
const COLOR_CELL: f32 = 30.0;
const COLOR_CELL_GAP: f32 = 2.0;
const COLOR_DOT: f32 = 14.0;
const COLOR_RING: f32 = 1.5;
const COLOR_RING_GAP: f32 = 2.5;
/// How dark a colour dot's edge is.
const DOT_EDGE_ALPHA: u8 = 40;
/// The slider's rail thickness and its idle knob's ring.
const SLIDER_RAIL_HEIGHT: f32 = 3.0;
const SLIDER_KNOB_RING: f32 = 1.5;
/// The keyboard focus ring: an accent outline, so a focused control reads apart from a hovered
/// one (the pill) and a selected one (the pill and accent text).
const FOCUS_RING: f32 = 1.5;

/// How a list marks the row under the pointer and its selected row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowLook {
    /// The list's own PdfCraft look.
    #[default]
    PdfCraft,
    /// Margin's: the hover fill under hovered and selected rows, accent text on the selected one.
    Ember,
}

impl RowLook {
    pub fn of(flavor: Flavor) -> Self {
        match flavor {
            Flavor::Margin => Self::Ember,
            Flavor::PdfCraft => Self::PdfCraft,
        }
    }
}

/// A row's background: the hover fill under a hovered or selected row, fading in and out over
/// [`motion::HOVER`]; nothing otherwise. A row with keyboard focus gets the focus ring.
pub(crate) fn row_fill(ui: &egui::Ui, t: &Tokens, rect: Rect, resp: &Response, selected: bool) {
    let shown = motion::hover(ui.ctx(), resp.id, resp.hovered() || selected);
    if shown > 0.0 {
        ui.painter().rect_filled(rect, CornerRadius::same(t.radius), t.hover.gamma_multiply(shown));
    }
    focus_ring(ui, t, rect, resp, t.radius);
}

/// The keyboard focus ring round `rect` while `resp` has focus. egui gives a button focus only
/// from the keyboard (Tab), never from a click, so the ring never follows the pointer.
pub(crate) fn focus_ring(ui: &egui::Ui, t: &Tokens, rect: Rect, resp: &Response, radius: u8) {
    if resp.has_focus() {
        ui.painter().rect_stroke(rect, CornerRadius::same(radius), Stroke::new(FOCUS_RING, t.accent), egui::StrokeKind::Inside);
    }
}

/// A row's main text colour: the accent on the selected row, `normal` otherwise.
pub(crate) fn row_text(t: &Tokens, selected: bool, normal: Color32) -> Color32 {
    if selected { t.accent } else { normal }
}

/// A section label in small caps, muted ("CONTENTS", "HIGHLIGHTS 3"), inset like row text. Call
/// it in a horizontal layout.
pub(crate) fn section_label(ui: &mut egui::Ui, t: &Tokens, title: &str, count: Option<usize>) -> Response {
    ui.add_space(ROW_PAD);
    ui.label(section_caps(t, title, count))
}

/// The text of a [`section_label`], for places that set their own inset (popover headers).
pub(crate) fn section_caps(t: &Tokens, title: &str, count: Option<usize>) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let format =
        egui::TextFormat { font_id: theme::semibold(LABEL_SIZE), color: t.text_muted, extra_letter_spacing: LABEL_TRACKING, ..Default::default() };
    job.append(&title.to_uppercase(), 0.0, format.clone());
    if let Some(n) = count {
        job.append(&n.to_string(), GAP, egui::TextFormat { font_id: theme::regular(LABEL_SIZE), ..format });
    }
    job
}

/// The keys that run `command` (with no arguments) in Margin, as the keyboard sheet writes them.
pub(crate) fn shortcut(app: &PdfCraftApp, command: &str) -> Option<String> {
    shortcut_with(app, command, &Value::Null)
}

/// The keys that run `command` with `args`: its first keymap binding still in effect (a later
/// binding or unbind of the same keys in the same context replaces it), else PdfCraft's own
/// shortcut while the keymap leaves that alone.
pub(crate) fn shortcut_with(app: &PdfCraftApp, command: &str, args: &Value) -> Option<String> {
    let mac = cfg!(target_os = "macos");
    if let Some(table) = app.margin_keys.keymap() {
        let bindings = &table.bindings;
        let bound = bindings.iter().enumerate().find(|(i, binding)| {
            binding.command.as_deref() == Some(command)
                && binding.args == *args
                && !bindings[i + 1..].iter().any(|later| later.context_text == binding.context_text && later.keys == binding.keys)
        });
        if let Some((_, binding)) = bound {
            return Some(keysheet::pretty_keys(&binding.keys, mac));
        }
    }
    let spec = commands::command(command).filter(|_| args.is_null())?;
    spec.shortcut.filter(|_| app.margin_keys.falls_back(spec)).map(|keys| keys.label(mac))
}

/// A Margin option popover toggled by `button`, under the stable `id` (so a change made inside it
/// never makes it a different popup). Its options apply at once and leave it open; a click
/// outside it, Esc or `button` again closes it. The caller sets its frame and placement.
///
/// Esc gives the keyboard focus back to `button` (egui closes the popup on that Escape, this
/// frame; the next frame sees it closed).
pub(crate) fn popover<'a>(button: &Response, id: egui::Id) -> egui::Popup<'a> {
    let ctx = &button.ctx;
    let escaping_id = id.with("closing-by-escape");
    let open = egui::Popup::is_id_open(ctx, id);
    if !open && ctx.data(|d| d.get_temp::<bool>(escaping_id)).unwrap_or(false) {
        button.request_focus();
    }
    let escaping = open && ctx.input(|i| i.key_pressed(egui::Key::Escape));
    ctx.data_mut(|d| d.insert_temp(escaping_id, escaping));
    egui::Popup::menu(button).id(id).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
}

/// A tooltip naming the control, with its shortcut after it in muted text. A disabled control
/// shows it too, so its label can say why it is off.
pub(crate) fn tooltip(resp: Response, label: &str, shortcut: Option<&str>) -> Response {
    tooltip_ui(&resp, |ui| {
        let t = Tokens::get(ui.ctx());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.label(egui::RichText::new(label).color(t.text));
            if let Some(keys) = shortcut {
                ui.label(egui::RichText::new(keys).color(t.text_muted));
            }
        });
    });
    resp
}

/// Any tooltip contents on Margin's tooltip card: egui's own tooltip (its delay, placement inside
/// the window and clear of the control), drawn as a dark rounded card with an arrow pointing at
/// the control.
pub(crate) fn tooltip_ui(resp: &Response, contents: impl FnOnce(&mut egui::Ui)) {
    let tip = if resp.enabled() { egui::Tooltip::for_enabled(resp) } else { egui::Tooltip::for_disabled(resp) };
    show_card(tip, resp, contents);
}

/// `message` on the tooltip card at `resp` now, without waiting for a hover: what a field says
/// about its input (the page field's page range after a page it doesn't have).
pub(crate) fn note(resp: &Response, message: &str) {
    let tip = egui::Tooltip::always_open(resp.ctx.clone(), resp.layer_id, resp.id.with("margin-note"), resp.rect);
    show_card(tip, resp, |ui| {
        let t = Tokens::get(ui.ctx());
        ui.label(egui::RichText::new(message).color(t.text));
    });
}

fn show_card(mut tip: egui::Tooltip<'_>, resp: &Response, contents: impl FnOnce(&mut egui::Ui)) {
    let t = Tokens::get(&resp.ctx);
    let card = egui::Frame::popup(&resp.ctx.global_style()).fill(TOOLTIP_FILL).corner_radius(CornerRadius::same(t.radius)).inner_margin(TOOLTIP_PAD);
    tip.popup = tip.popup.gap(TOOLTIP_GAP).frame(card);
    if let Some(shown) = tip.show(contents) {
        tooltip_arrow(&resp.ctx, &t, shown.response.layer_id, resp.rect, shown.response.rect);
    }
}

/// The tooltip card's arrow: on the side of `card` that faces `control`, as close to the
/// control's middle as the card's rounded corners allow.
fn tooltip_arrow(ctx: &egui::Context, t: &Tokens, layer: egui::LayerId, control: Rect, card: Rect) {
    let reach = TOOLTIP_ARROW;
    let inset = f32::from(t.radius) + reach;
    let along =
        |middle: f32, low: f32, high: f32| if low + inset <= high - inset { middle.clamp(low + inset, high - inset) } else { (low + high) / 2.0 };
    // The base sits one point inside the card, over its border.
    let (tip, base) = if card.top() >= control.bottom() {
        let x = along(control.center().x, card.left(), card.right());
        (egui::pos2(x, card.top() - reach), [egui::pos2(x - reach, card.top() + 1.0), egui::pos2(x + reach, card.top() + 1.0)])
    } else if card.bottom() <= control.top() {
        let x = along(control.center().x, card.left(), card.right());
        (egui::pos2(x, card.bottom() + reach), [egui::pos2(x - reach, card.bottom() - 1.0), egui::pos2(x + reach, card.bottom() - 1.0)])
    } else if card.left() >= control.right() {
        let y = along(control.center().y, card.top(), card.bottom());
        (egui::pos2(card.left() - reach, y), [egui::pos2(card.left() + 1.0, y - reach), egui::pos2(card.left() + 1.0, y + reach)])
    } else if card.right() <= control.left() {
        let y = along(control.center().y, card.top(), card.bottom());
        (egui::pos2(card.right() + reach, y), [egui::pos2(card.right() - 1.0, y - reach), egui::pos2(card.right() - 1.0, y + reach)])
    } else {
        // Overlapping (a tooltip at the pointer): no side faces the control.
        return;
    };
    let painter = ctx.layer_painter(layer);
    painter.add(egui::Shape::convex_polygon(vec![base[0], tip, base[1]], TOOLTIP_FILL, Stroke::NONE));
    let edge = Stroke::new(1.0, t.border);
    painter.line_segment([base[0], tip], edge);
    painter.line_segment([tip, base[1]], edge);
}

/// An icon button with Margin's states: the pill when hovered (the icon in the accent), the pill
/// and an accent icon when `on`, muted with no pill when disabled. Screen readers and tests get
/// `label`; the tooltip adds `shortcut`.
pub(crate) fn icon_button(ui: &mut egui::Ui, icon: &str, size: f32, on: bool, label: &str, shortcut: Option<&str>) -> Response {
    paint_button(ui, icon, size, on, label, shortcut, None)
}

/// The annotate rail's icon button: [`icon_button`] inside a hairline border.
pub(crate) fn rail_button(ui: &mut egui::Ui, icon: &str, size: f32, on: bool, label: &str, shortcut: Option<&str>) -> Response {
    let border = Tokens::get(ui.ctx()).border;
    paint_button(ui, icon, size, on, label, shortcut, Some(border))
}

fn paint_button(ui: &mut egui::Ui, icon: &str, size: f32, on: bool, label: &str, shortcut: Option<&str>, border: Option<Color32>) -> Response {
    let t = Tokens::get(ui.ctx());
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, on, label));
    let stroke = border.map_or(Stroke::NONE, |c| Stroke::new(1.0, c));
    ui.painter().rect(rect, CornerRadius::same(t.radius), button_fill(ui.ctx(), &t, &resp, on && enabled), stroke, egui::StrokeKind::Inside);
    focus_ring(ui, &t, rect, &resp, t.radius);
    let tint = if enabled { icon_tint(&t, on, resp.hovered()) } else { t.text_faint };
    icons::paint(ui, rect, icon, (size * ICON_SHARE).round(), tint);
    tooltip(resp, label, shortcut)
}

/// A button showing a colour instead of an icon (a comment tool's or a highlight's colour):
/// [`icon_button`]'s states, a hairline border when `bordered` (the rail's buttons have one).
pub(crate) fn color_button(ui: &mut egui::Ui, size: f32, color: Color32, bordered: bool, label: &str, shortcut: Option<&str>) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label));
    let stroke = if bordered { Stroke::new(1.0, t.border) } else { Stroke::NONE };
    ui.painter().rect(rect, CornerRadius::same(t.radius), button_fill(ui.ctx(), &t, &resp, false), stroke, egui::StrokeKind::Inside);
    focus_ring(ui, &t, rect, &resp, t.radius);
    paint_dot(ui, rect.center(), color);
    tooltip(resp, label, shortcut)
}

/// A colour dot with a faint edge, so light colours show on light fills.
fn paint_dot(ui: &egui::Ui, center: egui::Pos2, color: Color32) {
    ui.painter().circle(center, COLOR_DOT / 2.0, color, Stroke::new(1.0, Color32::from_black_alpha(DOT_EDGE_ALPHA)));
}

/// One choice of a [`color_dots`] row.
pub(crate) struct ColorDot<'a> {
    pub name: &'a str,
    pub color: Color32,
    pub selected: bool,
    pub shortcut: Option<&'a str>,
}

/// The mockup's colour row (`.cdot`): dots in square cells with the hover fill, a ring in the
/// dot's colour around the current one. Screen readers and tests see a button per colour named
/// after it; the tooltip adds its keys. Returns the clicked index.
pub(crate) fn color_dots(ui: &mut egui::Ui, dots: &[ColorDot<'_>]) -> Option<usize> {
    let t = Tokens::get(ui.ctx());
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = COLOR_CELL_GAP;
        for (i, dot) in dots.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(COLOR_CELL), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), dot.selected, dot.name));
            ui.painter().rect_filled(rect, CornerRadius::same(t.radius), button_fill(ui.ctx(), &t, &resp, false));
            focus_ring(ui, &t, rect, &resp, t.radius);
            paint_dot(ui, rect.center(), dot.color);
            if dot.selected {
                ui.painter().circle_stroke(rect.center(), COLOR_DOT / 2.0 + COLOR_RING_GAP, Stroke::new(COLOR_RING, dot.color));
            }
            if tooltip(resp, dot.name, dot.shortcut).clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// Margin's slider (the mockup's range input): a thin rail in the divider colour and a round
/// knob. egui fills the idle knob with the rail's colour, so the idle knob is a light ring;
/// hovered it fills; dragged or focused from the keyboard it fills inside the focus ring. Call
/// before adding the slider.
pub(crate) fn slider_style(ui: &mut egui::Ui, t: &Tokens) {
    ui.spacing_mut().slider_rail_height = SLIDER_RAIL_HEIGHT;
    ui.visuals_mut().handle_shape = egui::style::HandleShape::Circle;
    let widgets = &mut ui.visuals_mut().widgets;
    widgets.inactive.bg_fill = t.divider;
    widgets.inactive.fg_stroke = Stroke::new(SLIDER_KNOB_RING, t.text_muted);
    widgets.hovered.bg_fill = t.text;
    widgets.hovered.fg_stroke = Stroke::NONE;
    widgets.active.bg_fill = t.text;
    widgets.active.fg_stroke = Stroke::new(FOCUS_RING, t.accent);
}

/// An icon and a word ("Tools") with [`icon_button`]'s states (the word in the accent too when
/// `on`); `tip` is its tooltip.
pub(crate) fn text_button(ui: &mut egui::Ui, icon: &str, label: &str, height: f32, on: bool, tip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let icon_size = (height * ICON_SHARE).round();
    let galley = ui.painter().layout_no_wrap(label.to_owned(), theme::medium(13.0), Color32::PLACEHOLDER);
    let width = GAP + icon_size + GAP / 2.0 + galley.size().x + GAP;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), on, label));
    ui.painter().rect_filled(rect, CornerRadius::same(t.radius), button_fill(ui.ctx(), &t, &resp, on));
    focus_ring(ui, &t, rect, &resp, t.radius);
    let tint = icon_tint(&t, on, resp.hovered());
    let icon_rect = Rect::from_center_size(egui::pos2(rect.left() + GAP + icon_size / 2.0, rect.center().y), Vec2::splat(icon_size));
    icons::paint(ui, icon_rect, icon, icon_size, tint);
    let text_pos = egui::pos2(icon_rect.right() + GAP / 2.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(text_pos, galley, row_text(&t, on, t.text));
    tooltip(resp, tip, None)
}

/// A button's background: pressed, then the hover fill (fading over [`motion::HOVER`]) when on
/// or hovered.
fn button_fill(ctx: &egui::Context, t: &Tokens, resp: &Response, on: bool) -> Color32 {
    let shown = motion::hover(ctx, resp.id, on || resp.hovered());
    if resp.is_pointer_button_down_on() { t.pressed } else { t.hover.gamma_multiply(shown) }
}

/// An icon's colour on a button or tab: the accent when on or hovered (Ember's
/// `--icon-color-hover`), the muted icon colour otherwise.
pub(crate) fn icon_tint(t: &Tokens, on: bool, hovered: bool) -> Color32 {
    if on || hovered { t.accent } else { t.icon }
}

/// An on/off switch, the mockup's `.sw-t`: an accent track with a white knob on the right when
/// on, a divider-coloured track with a muted knob on the left when off; the knob slides and the
/// colours blend over [`motion::FADE`]. Screen readers and tests see a checkbox named `label`.
pub(crate) fn switch(ui: &mut egui::Ui, on: bool, label: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(SWITCH_SIZE, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), on, label));
    let how_on = motion::toggle(ui.ctx(), resp.id.with("margin-switch"), on, motion::FADE);
    let radius = rect.height() / 2.0;
    ui.painter().rect_filled(rect, CornerRadius::same(radius as u8), t.divider.lerp_to_gamma(t.accent, how_on));
    let knob_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
    let knob = t.text_muted.lerp_to_gamma(Color32::WHITE, how_on);
    ui.painter().circle_filled(egui::pos2(knob_x, rect.center().y), radius - SWITCH_KNOB_INSET, knob);
    focus_ring(ui, &t, rect, &resp, radius as u8);
    resp
}

/// The primary action of a group ("Save"): the accent fill and white text, [`BUTTON_HEIGHT`]
/// tall, padded by the large step on each side.
pub(crate) fn accent_button(ui: &mut egui::Ui, label: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), theme::medium(BUTTON_TEXT), Color32::WHITE);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 2.0 * GAP_LARGE, BUTTON_HEIGHT), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label));
    let lift = motion::hover(ui.ctx(), resp.id, resp.hovered());
    let fill =
        if resp.is_pointer_button_down_on() { t.accent.gamma_multiply(ACCENT_PRESSED_SHADE) } else { t.accent.lerp_to_gamma(ACCENT_HOVER, lift) };
    ui.painter().rect_filled(rect, CornerRadius::same(t.radius), fill);
    // On the accent fill the ring is the text colour.
    if resp.has_focus() {
        ui.painter().rect_stroke(rect, CornerRadius::same(t.radius), Stroke::new(FOCUS_RING, t.text), egui::StrokeKind::Inside);
    }
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    resp
}

/// A secondary action as plain text ("Cancel"): muted, full text and the hover fill when
/// hovered; the same size as an [`accent_button`].
pub(crate) fn plain_button(ui: &mut egui::Ui, label: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), theme::medium(BUTTON_TEXT), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 2.0 * GAP_LARGE, BUTTON_HEIGHT), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label));
    let shown = motion::hover(ui.ctx(), resp.id, resp.hovered());
    ui.painter().rect_filled(rect, CornerRadius::same(t.radius), button_fill(ui.ctx(), &t, &resp, false));
    focus_ring(ui, &t, rect, &resp, t.radius);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, t.text_muted.lerp_to_gamma(t.text, shown));
    resp
}

/// How wide a [`plain_button`] showing `label` is, for laying out what sits beside it.
pub(crate) fn plain_button_width(ui: &egui::Ui, label: &str) -> f32 {
    ui.painter().layout_no_wrap(label.to_owned(), theme::medium(BUTTON_TEXT), Color32::PLACEHOLDER).size().x + 2.0 * GAP_LARGE
}

/// A bordered button `width` wide with an icon and a word, both centred ("Copy all"): the
/// hover fill when hovered, muted when disabled. Rows of them share the width equally.
pub(crate) fn chip_button(ui: &mut egui::Ui, icon: &str, label: &str, width: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, BUTTON_HEIGHT), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    ui.painter().rect(
        rect,
        CornerRadius::same(t.radius),
        button_fill(ui.ctx(), &t, &resp, false),
        Stroke::new(1.0, t.border),
        egui::StrokeKind::Inside,
    );
    focus_ring(ui, &t, rect, &resp, t.radius);
    let color = if enabled { t.text } else { t.text_faint };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), theme::regular(BUTTON_TEXT), color);
    let content = BUTTON_ICON + GAP / 2.0 + galley.size().x;
    let left = rect.center().x - content.min(rect.width()) / 2.0;
    let icon_rect = Rect::from_center_size(egui::pos2(left + BUTTON_ICON / 2.0, rect.center().y), Vec2::splat(BUTTON_ICON));
    icons::paint(ui, icon_rect, icon, BUTTON_ICON, if enabled { icon_tint(&t, false, resp.hovered()) } else { t.text_faint });
    let text_pos = egui::pos2(icon_rect.right() + GAP / 2.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())).galley(text_pos, galley, color);
    resp
}

/// One choice of a [`segmented`] control.
pub(crate) struct Segment<'a> {
    pub label: &'a str,
    pub selected: bool,
    pub tip: Option<&'a str>,
}

/// The mockup's segmented control (`.seg`): choices in a hairline-bordered well, the chosen one
/// on the pill in the accent, the others muted (bright and on the pill when hovered). Lays out
/// left to right; returns the clicked index.
pub(crate) fn segmented(ui: &mut egui::Ui, segments: &[Segment<'_>]) -> Option<usize> {
    let t = Tokens::get(ui.ctx());
    let font = theme::regular(SEGMENT_TEXT);
    // Laid out in the placeholder colour, so each chip paints it in its own state's colour.
    let galleys: Vec<_> = segments.iter().map(|s| ui.painter().layout_no_wrap(s.label.to_owned(), font.clone(), Color32::PLACEHOLDER)).collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 2.0 * SEGMENT_PAD_X).collect();
    let inner_w: f32 = widths.iter().sum::<f32>() + SEGMENT_INSET * (segments.len().saturating_sub(1)) as f32;
    let size = vec2(inner_w + 2.0 * SEGMENT_INSET, SEGMENT_HEIGHT);
    let (well, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect(well, CornerRadius::same(t.radius), t.field, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let mut clicked = None;
    let mut x = well.left() + SEGMENT_INSET;
    for (i, ((segment, galley), width)) in segments.iter().zip(galleys).zip(widths).enumerate() {
        let rect = Rect::from_min_size(egui::pos2(x, well.top() + SEGMENT_INSET), vec2(width, SEGMENT_HEIGHT - 2.0 * SEGMENT_INSET));
        x += width + SEGMENT_INSET;
        let resp = ui.interact(rect, ui.id().with(("segment", segment.label)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, ui.is_enabled(), segment.selected, segment.label));
        let shown = motion::hover(ui.ctx(), resp.id, resp.hovered());
        let fill = if segment.selected { t.selected } else { t.hover.gamma_multiply(shown) };
        ui.painter().rect_filled(rect, CornerRadius::same(t.radius.saturating_sub(1)), fill);
        focus_ring(ui, &t, rect, &resp, t.radius.saturating_sub(1));
        let color = if segment.selected { t.accent } else { t.text_muted.lerp_to_gamma(t.text, shown) };
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
        let resp = match segment.tip {
            Some(tip) => tooltip(resp, tip, None),
            None => resp,
        };
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}
