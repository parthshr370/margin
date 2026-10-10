//! Margin's shared look, the owner's Obsidian "Ember Night": surfaces, list rows, buttons,
//! section labels and tooltips (plan/R10-SURFACES.md §2 and §6). Permanent regions are separated
//! by a fill step and one edge; floating ones (rail, popovers, menus, tooltips, toasts) add the
//! floating fill and a shadow; a modal adds a scrim. A hovered row or ghost control gets the
//! `hover` fill and a lighter edge; a selected one the `selected` fill, an accent edge and accent
//! icon; secondary columns (page numbers) stay muted; a disabled control stays muted. The theme's
//! [`Tokens`] hold the base ladder (`theme.rs`); [`Surfaces`] adds the levels and states only
//! Margin uses.
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
/// A compact button's height ([`ButtonSize::Compact`]), for rows laid out around one.
pub const BUTTON_HEIGHT: f32 = COMPACT.height;
/// Outside Margin's palette, the accent keeps this share of its colour while pressed.
const ACCENT_PRESSED_SHADE: f32 = 0.85;
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
/// The keyboard focus ring: 2 px of accent, 2 px outside the control, so a focused control reads
/// apart from a hovered one and a selected one.
const FOCUS_RING: f32 = 2.0;
const FOCUS_GAP: f32 = 2.0;

/// Corner radii by surface (§2): a card, a menu, tooltip or toast, the rail and popovers, a
/// modal sheet.
pub(crate) const RADIUS_CARD: u8 = 8;
pub(crate) const RADIUS_MENU: u8 = 8;
pub(crate) const RADIUS_FLOATING: u8 = 12;
pub(crate) const RADIUS_MODAL: u8 = 16;
/// The three shadows: S under a card on the base (library), L under a floating surface, M
/// under a modal sheet.
pub(crate) const SHADOW_S: egui::Shadow = egui::Shadow { offset: [0, 1], blur: 3, spread: 0, color: Color32::from_black_alpha(77) };
pub(crate) const SHADOW_L: egui::Shadow = egui::Shadow { offset: [0, 4], blur: 12, spread: 0, color: Color32::from_black_alpha(102) };
pub(crate) const SHADOW_M: egui::Shadow = egui::Shadow { offset: [0, 8], blur: 32, spread: 0, color: Color32::from_black_alpha(153) };

/// Margin's colours beyond the theme's ladder (§2): the floating level, the hover and control
/// edges, the accent's states and its ink, the modal scrim and the empty track. Margin's palette
/// has them all; in a light theme each falls back to the nearest token, so nothing here invents
/// a light palette.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Surfaces {
    /// Rail, popover, menu, tooltip, toast, modal: #282828.
    pub floating: Color32,
    /// A hovered card, row or ghost control's edge: #606060.
    pub edge_hover: Color32,
    /// The outline that marks a control (input, secondary button, segmented well): #808080.
    pub control_edge: Color32,
    pub accent_hover: Color32,
    pub accent_pressed: Color32,
    /// Text and icons on an accent fill: Ember's `--text-on-accent`, #0a0a0a.
    pub on_accent: Color32,
    /// Behind a modal: black at 60 %.
    pub scrim: Color32,
    /// An empty progress track or activity cell: #404040.
    pub track: Color32,
    /// The activity ramp, no reading to busiest: `heat_0..4`.
    pub heat: [Color32; 5],
}

/// Margin's palette for [`Surfaces`] (§2): Ember's terminal-black step, comment grey and muted
/// grey; the accent's peach hover and its 90 % pressed shade; Ember's text-on-accent; the empty
/// track.
const EMBER_FLOATING: Color32 = Color32::from_rgb(0x28, 0x28, 0x28);
const EMBER_EDGE_HOVER: Color32 = Color32::from_rgb(0x60, 0x60, 0x60);
const EMBER_CONTROL_EDGE: Color32 = Color32::from_rgb(0x80, 0x80, 0x80);
const EMBER_ACCENT_HOVER: Color32 = Color32::from_rgb(0xEE, 0x79, 0x48);
const EMBER_ACCENT_PRESSED: Color32 = Color32::from_rgb(0xD5, 0x53, 0x28);
const EMBER_ON_ACCENT: Color32 = Color32::from_rgb(0x0A, 0x0A, 0x0A);
const EMBER_TRACK: Color32 = Color32::from_rgb(0x40, 0x40, 0x40);
/// Ember's activity ramp (§2): the track, orange at 50 % and 75 % over the floating fill
/// (precomposited, so a cell's colour never depends on its parent), orange, peach.
const EMBER_HEAT: [Color32; 5] =
    [EMBER_TRACK, Color32::from_rgb(0x8A, 0x42, 0x2A), Color32::from_rgb(0xBB, 0x4E, 0x2A), Color32::from_rgb(0xEC, 0x5B, 0x2B), EMBER_ACCENT_HOVER];
/// Outside Margin's palette, the accent's share at each heat level above none.
const LIGHT_HEAT_SHARES: [f32; 4] = [0.4, 0.6, 0.8, 1.0];
/// The modal scrim's alpha: black at 60 % in Margin, egui's lighter default elsewhere.
const SCRIM_ALPHA: u8 = 153;
const LIGHT_SCRIM_ALPHA: u8 = 100;

impl Surfaces {
    pub(crate) fn of(t: &Tokens) -> Self {
        if t.kind == theme::ThemeKind::Margin {
            Self {
                floating: EMBER_FLOATING,
                edge_hover: EMBER_EDGE_HOVER,
                control_edge: EMBER_CONTROL_EDGE,
                accent_hover: EMBER_ACCENT_HOVER,
                accent_pressed: EMBER_ACCENT_PRESSED,
                on_accent: EMBER_ON_ACCENT,
                scrim: Color32::from_black_alpha(SCRIM_ALPHA),
                track: EMBER_TRACK,
                heat: EMBER_HEAT,
            }
        } else {
            Self {
                floating: t.card,
                edge_hover: t.border,
                control_edge: t.border,
                accent_hover: t.accent,
                accent_pressed: t.accent.gamma_multiply(ACCENT_PRESSED_SHADE),
                on_accent: Color32::WHITE,
                scrim: Color32::from_black_alpha(LIGHT_SCRIM_ALPHA),
                track: t.divider,
                heat: [
                    t.divider,
                    t.accent.gamma_multiply(LIGHT_HEAT_SHARES[0]),
                    t.accent.gamma_multiply(LIGHT_HEAT_SHARES[1]),
                    t.accent.gamma_multiply(LIGHT_HEAT_SHARES[2]),
                    t.accent.gamma_multiply(LIGHT_HEAT_SHARES[3]),
                ],
            }
        }
    }
}

/// Margin's egui style for windows, menus and popups (called from `theme::apply` for Margin):
/// the floating fill, one edge, the L shadow on menus and popups, the M shadow on windows, and the
/// 2 px accent outline on a focused text field.
pub(crate) fn style_floating(visuals: &mut egui::Visuals, t: &Tokens) {
    visuals.window_fill = Surfaces::of(t).floating;
    visuals.window_stroke = Stroke::new(EDGE, t.border);
    visuals.window_shadow = SHADOW_M;
    visuals.popup_shadow = SHADOW_L;
    visuals.window_corner_radius = CornerRadius::same(RADIUS_MODAL);
    visuals.menu_corner_radius = CornerRadius::same(RADIUS_MENU);
    visuals.selection.stroke = Stroke::new(FOCUS_RING, t.accent);
}

/// The width of every edge between surfaces and of a control's outline.
pub(crate) const EDGE: f32 = 1.0;

/// A floating surface's frame (rail, popover, toast) at `radius`: floating fill, edge, L shadow.
pub(crate) fn floating_frame(t: &Tokens, radius: u8) -> egui::Frame {
    egui::Frame::NONE.fill(Surfaces::of(t).floating).stroke(Stroke::new(EDGE, t.border)).corner_radius(radius).shadow(SHADOW_L)
}

/// A modal sheet's frame: floating fill, edge, M shadow, radius 16.
pub(crate) fn modal_frame(t: &Tokens) -> egui::Frame {
    egui::Frame::NONE.fill(Surfaces::of(t).floating).stroke(Stroke::new(EDGE, t.border)).corner_radius(RADIUS_MODAL).shadow(SHADOW_M)
}

/// The padding inside a modal sheet.
const MODAL_PAD: i8 = 24;

/// A Margin modal under `id`: the scrim over the whole window, then the sheet (floating fill,
/// edge, M shadow) padded by 24.
pub(crate) fn modal(id: egui::Id, t: &Tokens) -> egui::Modal {
    egui::Modal::new(id).backdrop_color(Surfaces::of(t).scrim).frame(modal_frame(t).inner_margin(MODAL_PAD))
}

/// A card's frame (document card, highlight, metric card): card fill, edge, radius 8.
pub(crate) fn card_frame(t: &Tokens) -> egui::Frame {
    egui::Frame::NONE.fill(t.card).stroke(Stroke::new(EDGE, t.border)).corner_radius(RADIUS_CARD)
}

/// A text input's well (search, page number, collection name, note editor): the base fill, a
/// control outline, or the focus ring's accent while `focused`.
pub(crate) fn input_frame(t: &Tokens, focused: bool) -> egui::Frame {
    egui::Frame::NONE.fill(t.field).stroke(input_stroke(t, focused)).corner_radius(t.radius)
}

/// [`input_frame`]'s well painted at `rect`, for a field that lays out its own contents (the
/// find field's icon, query, count and buttons).
pub(crate) fn paint_input_well(ui: &egui::Ui, t: &Tokens, rect: Rect, focused: bool) {
    ui.painter().rect(rect, CornerRadius::same(t.radius), t.field, input_stroke(t, focused), egui::StrokeKind::Inside);
}

fn input_stroke(t: &Tokens, focused: bool) -> Stroke {
    if focused { Stroke::new(FOCUS_RING, t.accent) } else { Stroke::new(EDGE, Surfaces::of(t).control_edge) }
}

/// Whether the text field `id` has the keyboard focus (for [`input_frame`], drawn before it).
pub(crate) fn input_focused(ui: &egui::Ui, id: egui::Id) -> bool {
    ui.ctx().memory(|memory| memory.has_focus(id))
}

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

/// A row's background (§2): a hovered row gets the hover fill and the hover edge, fading in and
/// out over [`motion::HOVER`]; a selected row the selected fill and an accent edge; nothing
/// otherwise. A row with keyboard focus gets the focus ring.
pub(crate) fn row_fill(ui: &egui::Ui, t: &Tokens, rect: Rect, resp: &Response, selected: bool) {
    state_fill(ui, t, rect, resp, selected, t.radius);
    focus_ring(ui, t, rect, resp, t.radius);
}

/// The hover or selected state of a row, card or ghost control at `rect` (§2), without the focus
/// ring: selected wins over hovered; pressed darkens either.
pub(crate) fn state_fill(ui: &egui::Ui, t: &Tokens, rect: Rect, resp: &Response, selected: bool, radius: u8) {
    let shown = motion::hover(ui.ctx(), resp.id, resp.hovered() || selected);
    if shown <= 0.0 {
        return;
    }
    let (fill, edge) = if selected { (t.selected, t.accent) } else { (t.hover, Surfaces::of(t).edge_hover) };
    let fill = if resp.is_pointer_button_down_on() { t.pressed } else { fill };
    ui.painter().rect(
        rect,
        CornerRadius::same(radius),
        fill.gamma_multiply(shown),
        Stroke::new(EDGE, edge.gamma_multiply(shown)),
        egui::StrokeKind::Inside,
    );
}

/// The keyboard focus ring round `rect` while `resp` has focus: 2 px of accent, 2 px outside the
/// control, so it never moves the layout (§2). egui gives a button focus only from the keyboard
/// (Tab), never from a click, so the ring never follows the pointer.
pub(crate) fn focus_ring(ui: &egui::Ui, t: &Tokens, rect: Rect, resp: &Response, radius: u8) {
    if resp.has_focus() {
        let ring = rect.expand(FOCUS_GAP);
        ui.painter().rect_stroke(
            ring,
            CornerRadius::same(radius.saturating_add(FOCUS_GAP as u8)),
            Stroke::new(FOCUS_RING, t.accent),
            egui::StrokeKind::Outside,
        );
    }
}

/// A row's main text colour: full text on the selected row (its edge and icon carry the
/// accent), `normal` otherwise.
pub(crate) fn row_text(t: &Tokens, selected: bool, normal: Color32) -> Color32 {
    if selected { t.text } else { normal }
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
/// the window and clear of the control), drawn as a floating card (fill, edge, L shadow) with an
/// arrow of the same fill and edge pointing at the control.
pub(crate) fn tooltip_ui(resp: &Response, contents: impl FnOnce(&mut egui::Ui)) {
    let tip = if resp.enabled() { egui::Tooltip::for_enabled(resp) } else { egui::Tooltip::for_disabled(resp) };
    show_card(tip, resp, resp.rect, contents);
}

/// `message` on the tooltip card at `resp` now, without waiting for a hover: what a field says
/// about its input (the page field's page range after a page it doesn't have).
pub(crate) fn note(resp: &Response, message: &str) {
    let tip = egui::Tooltip::always_open(resp.ctx.clone(), resp.layer_id, resp.id.with("margin-note"), resp.rect);
    show_card(tip, resp, resp.rect, |ui| {
        let t = Tokens::get(ui.ctx());
        ui.label(egui::RichText::new(message).color(t.text));
    });
}

/// Contents on the tooltip card now, its arrow at `x` along `resp`: a preview that follows the
/// pointer while a control is hovered or dragged (the footer's scrubber).
pub(crate) fn pointer_card(resp: &Response, x: f32, contents: impl FnOnce(&mut egui::Ui)) {
    let anchor = Rect::from_x_y_ranges(x..=x, resp.rect.y_range());
    let tip = egui::Tooltip::always_open(resp.ctx.clone(), resp.layer_id, resp.id.with("margin-pointer-card"), anchor);
    show_card(tip, resp, anchor, contents);
}

/// A detail card's padding (the heatmap's day card, §5).
const DETAIL_PAD: i8 = 16;

/// Contents on a padded tooltip card now, its arrow at `anchor` (part of `resp`): details that
/// follow the pointer or the keyboard over a chart (the Stats calendar's day). `id` keeps one
/// card as the anchor moves.
pub(crate) fn detail_card(resp: &Response, anchor: Rect, id: egui::Id, contents: impl FnOnce(&mut egui::Ui)) {
    let tip = egui::Tooltip::always_open(resp.ctx.clone(), resp.layer_id, id, anchor);
    show_padded_card(tip, resp, anchor, egui::Margin::same(DETAIL_PAD), contents);
}

/// `tip` on Margin's card, its arrow pointing at `anchor` (part of `resp`).
fn show_card(tip: egui::Tooltip<'_>, resp: &Response, anchor: Rect, contents: impl FnOnce(&mut egui::Ui)) {
    show_padded_card(tip, resp, anchor, TOOLTIP_PAD, contents);
}

/// [`show_card`] with `pad` inside the card.
fn show_padded_card(mut tip: egui::Tooltip<'_>, resp: &Response, anchor: Rect, pad: egui::Margin, contents: impl FnOnce(&mut egui::Ui)) {
    let t = Tokens::get(&resp.ctx);
    let card = floating_frame(&t, RADIUS_MENU).inner_margin(pad);
    tip.popup = tip.popup.gap(TOOLTIP_GAP).frame(card);
    if let Some(shown) = tip.show(contents) {
        tooltip_arrow(&resp.ctx, &t, shown.response.layer_id, anchor, shown.response.rect);
    }
}

/// The tooltip card's arrow: on the side of `card` that faces `control`, as close to the
/// control's middle as the card's rounded corners allow.
fn tooltip_arrow(ctx: &egui::Context, t: &Tokens, layer: egui::LayerId, control: Rect, card: Rect) {
    let reach = TOOLTIP_ARROW;
    let inset = f32::from(RADIUS_MENU) + reach;
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
    painter.add(egui::Shape::convex_polygon(vec![base[0], tip, base[1]], Surfaces::of(t).floating, Stroke::NONE));
    let edge = Stroke::new(EDGE, t.border);
    painter.line_segment([base[0], tip], edge);
    painter.line_segment([tip, base[1]], edge);
}

/// A ghost icon button (§6): transparent at rest, the hover fill and a lighter edge when
/// hovered (the icon brightens), the selected fill, an accent edge and an accent icon when `on`,
/// faint with no state when disabled. Screen readers and tests get `label`; the tooltip adds
/// `shortcut`.
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
    if let Some(border) = border {
        ui.painter().rect_stroke(rect, CornerRadius::same(t.radius), Stroke::new(EDGE, border), egui::StrokeKind::Inside);
    }
    if enabled {
        state_fill(ui, &t, rect, &resp, on, t.radius);
    }
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

/// A button's background: pressed, then the hover fill (fading over [`motion::HOVER`]) when on
/// or hovered.
fn button_fill(ctx: &egui::Context, t: &Tokens, resp: &Response, on: bool) -> Color32 {
    let shown = motion::hover(ctx, resp.id, on || resp.hovered());
    if resp.is_pointer_button_down_on() { t.pressed } else { t.hover.gamma_multiply(shown) }
}

/// An icon's colour on a button or tab: the accent when on, full text when hovered (the icon
/// brightens), the muted icon colour otherwise.
pub(crate) fn icon_tint(t: &Tokens, on: bool, hovered: bool) -> Color32 {
    if on {
        t.accent
    } else if hovered {
        t.text
    } else {
        t.icon
    }
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

/// What a [`Button`] does in its group (§6): the one leading action, an ordinary named action
/// (outlined), or a quiet toolbar or navigation action (no surface until hovered).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
}

/// A button's size (§6): compact for sidebar strips and small resets, standard for popovers and
/// creation rows, prominent for Continue reading and an empty state's action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ButtonSize {
    Compact,
    Standard,
    Prominent,
}

/// One size's height, side padding, text size and icon size.
struct SizeSpec {
    height: f32,
    pad_x: f32,
    text: f32,
    icon: f32,
}

const COMPACT: SizeSpec = SizeSpec { height: 28.0, pad_x: 8.0, text: 12.0, icon: 14.0 };
const STANDARD: SizeSpec = SizeSpec { height: 32.0, pad_x: 12.0, text: 13.0, icon: 16.0 };
const PROMINENT: SizeSpec = SizeSpec { height: 36.0, pad_x: 16.0, text: 14.0, icon: 18.0 };

impl ButtonSize {
    fn spec(self) -> &'static SizeSpec {
        match self {
            Self::Compact => &COMPACT,
            Self::Standard => &STANDARD,
            Self::Prominent => &PROMINENT,
        }
    }
}

/// Margin's one button (§6), built then shown: a kind, a size, a visible label (empty for an
/// icon-only square), optional leading and trailing icons, an accessible name when the label
/// is shorter than what the control does, a tooltip, and a selected state for filters.
pub(crate) struct Button<'a> {
    kind: ButtonKind,
    size: ButtonSize,
    label: &'a str,
    icon: Option<&'a str>,
    trailing: Option<&'a str>,
    name: Option<&'a str>,
    tip: Option<&'a str>,
    shortcut: Option<&'a str>,
    selected: bool,
    fixed_width: Option<f32>,
}

impl<'a> Button<'a> {
    pub(crate) fn new(kind: ButtonKind, size: ButtonSize, label: &'a str) -> Self {
        Self { kind, size, label, icon: None, trailing: None, name: None, tip: None, shortcut: None, selected: false, fixed_width: None }
    }

    pub(crate) fn primary(size: ButtonSize, label: &'a str) -> Self {
        Self::new(ButtonKind::Primary, size, label)
    }

    pub(crate) fn secondary(size: ButtonSize, label: &'a str) -> Self {
        Self::new(ButtonKind::Secondary, size, label)
    }

    pub(crate) fn ghost(size: ButtonSize, label: &'a str) -> Self {
        Self::new(ButtonKind::Ghost, size, label)
    }

    pub(crate) fn icon(self, icon: &'a str) -> Self {
        Self { icon: Some(icon), ..self }
    }

    pub(crate) fn trailing(self, icon: &'a str) -> Self {
        Self { trailing: Some(icon), ..self }
    }

    /// What screen readers and tests call it, when the label says less ("Reset").
    pub(crate) fn name(self, name: &'a str) -> Self {
        Self { name: Some(name), ..self }
    }

    pub(crate) fn tip(self, tip: &'a str) -> Self {
        Self { tip: Some(tip), ..self }
    }

    pub(crate) fn shortcut(self, shortcut: Option<&'a str>) -> Self {
        Self { shortcut, ..self }
    }

    /// A filter or toggle that is on: the selected fill, an accent edge, an accent check (or
    /// icon).
    pub(crate) fn selected(self, selected: bool) -> Self {
        Self { selected, ..self }
    }

    /// Exactly `width` wide, its content centred and clipped (buttons sharing a row equally).
    pub(crate) fn fixed_width(self, width: f32) -> Self {
        Self { fixed_width: Some(width), ..self }
    }

    /// The leading icon: the given one, or a check on a selected secondary filter.
    fn leading(&self) -> Option<&'a str> {
        self.icon.or((self.selected && self.kind == ButtonKind::Secondary).then_some("check"))
    }

    fn label_galley(&self, ui: &egui::Ui) -> Option<std::sync::Arc<egui::Galley>> {
        let font = theme::medium(self.size.spec().text);
        (!self.label.is_empty()).then(|| ui.painter().layout_no_wrap(self.label.to_owned(), font, Color32::PLACEHOLDER))
    }

    /// The width of the icons, label and the gaps between them.
    fn content_width(&self, galley: Option<&egui::Galley>) -> f32 {
        let spec = self.size.spec();
        let parts = [self.leading().map(|_| spec.icon), galley.map(|g| g.size().x), self.trailing.map(|_| spec.icon)];
        let present: Vec<f32> = parts.into_iter().flatten().collect();
        present.iter().sum::<f32>() + GAP * present.len().saturating_sub(1) as f32
    }

    /// How wide the button will be, for laying out what sits beside it.
    pub(crate) fn width(&self, ui: &egui::Ui) -> f32 {
        let galley = self.label_galley(ui);
        self.width_for(galley.as_deref())
    }

    fn width_for(&self, galley: Option<&egui::Galley>) -> f32 {
        let spec = self.size.spec();
        let natural = || if galley.is_none() && self.trailing.is_none() { spec.height } else { self.content_width(galley) + 2.0 * spec.pad_x };
        self.fixed_width.unwrap_or_else(natural)
    }

    pub(crate) fn show(self, ui: &mut egui::Ui) -> Response {
        let t = Tokens::get(ui.ctx());
        let surfaces = Surfaces::of(&t);
        let spec = self.size.spec();
        let enabled = ui.is_enabled();
        let galley = self.label_galley(ui);
        let (rect, resp) = ui.allocate_exact_size(vec2(self.width_for(galley.as_deref()), spec.height), Sense::click());
        let name = self.name.unwrap_or(self.label);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, self.selected, name));
        let lift = motion::hover(ui.ctx(), resp.id, enabled && resp.hovered());
        let pressed = enabled && resp.is_pointer_button_down_on();
        let radius = CornerRadius::same(t.radius);
        let visual = self.visual(&t, &surfaces, enabled, lift, pressed);
        ui.painter().rect(rect, radius, visual.fill, visual.edge, egui::StrokeKind::Inside);
        if self.kind == ButtonKind::Primary && resp.has_focus() {
            // The dark inner ring, then the parent-coloured gap and the accent ring outside.
            ui.painter().rect_stroke(rect, radius, Stroke::new(FOCUS_RING, surfaces.on_accent), egui::StrokeKind::Inside);
        }
        focus_ring(ui, &t, rect, &resp, t.radius);
        let mut x = rect.center().x - self.content_width(galley.as_deref()).min(rect.width() - 2.0 * spec.pad_x) / 2.0;
        let icon_at = |ui: &egui::Ui, icon: &str, x: &mut f32| {
            let icon_rect = Rect::from_min_size(egui::pos2(*x, rect.center().y - spec.icon / 2.0), Vec2::splat(spec.icon));
            icons::paint(ui, icon_rect, icon, spec.icon, visual.icon);
            *x += spec.icon + GAP;
        };
        if let Some(icon) = self.leading() {
            icon_at(ui, icon, &mut x);
        }
        if let Some(galley) = galley {
            let width = galley.size().x;
            let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
            ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())).galley(pos, galley, visual.ink);
            x += width + GAP;
        }
        if let Some(icon) = self.trailing {
            icon_at(ui, icon, &mut x);
        }
        // An icon-only button always names itself; a labelled one only when it has more to say.
        match (self.tip, self.label.is_empty() || self.shortcut.is_some()) {
            (Some(tip), _) => tooltip(resp, tip, self.shortcut),
            (None, true) => tooltip(resp, name, self.shortcut),
            (None, false) => resp,
        }
    }

    /// Fill, edge, label ink and icon colour for this frame's state (§6's table).
    fn visual(&self, t: &Tokens, surfaces: &Surfaces, enabled: bool, lift: f32, pressed: bool) -> ButtonVisual {
        let edge = |color: Color32| Stroke::new(EDGE, color);
        match self.kind {
            ButtonKind::Primary | ButtonKind::Secondary if !enabled => {
                ButtonVisual { fill: t.panel, edge: edge(t.border), ink: t.text_faint, icon: t.text_faint }
            }
            ButtonKind::Ghost if !enabled => ButtonVisual { fill: Color32::TRANSPARENT, edge: Stroke::NONE, ink: t.text_faint, icon: t.text_faint },
            ButtonKind::Primary => {
                let fill = if pressed { surfaces.accent_pressed } else { t.accent.lerp_to_gamma(surfaces.accent_hover, lift) };
                ButtonVisual { fill, edge: Stroke::NONE, ink: surfaces.on_accent, icon: surfaces.on_accent }
            }
            ButtonKind::Secondary => {
                let rest = if self.selected { t.selected } else { t.card };
                let fill = if pressed { t.pressed } else { rest.lerp_to_gamma(t.hover, lift) };
                let outline = if self.selected { t.accent } else { surfaces.control_edge };
                let icon = if self.selected { t.accent } else { t.icon.lerp_to_gamma(t.text, lift) };
                ButtonVisual { fill, edge: edge(outline), ink: t.text, icon }
            }
            ButtonKind::Ghost => {
                let (fill, outline) =
                    if self.selected { (t.selected, t.accent) } else { (t.hover.gamma_multiply(lift), surfaces.edge_hover.gamma_multiply(lift)) };
                let fill = if pressed { t.pressed } else { fill };
                let icon = if self.selected { t.accent } else { t.icon.lerp_to_gamma(t.text, lift) };
                ButtonVisual { fill, edge: edge(outline), ink: t.text, icon }
            }
        }
    }
}

/// A button's colours in one state.
struct ButtonVisual {
    fill: Color32,
    edge: Stroke,
    ink: Color32,
    icon: Color32,
}

/// The group's leading action ("Save", "Add", "Continue").
pub(crate) fn button_primary(ui: &mut egui::Ui, size: ButtonSize, label: &str) -> Response {
    Button::primary(size, label).show(ui)
}

/// An ordinary named action ("Cancel", "Export", "Set as default").
pub(crate) fn button_secondary(ui: &mut egui::Ui, size: ButtonSize, label: &str) -> Response {
    Button::secondary(size, label).show(ui)
}

/// A quiet text action in a toolbar or menu-like strip.
pub(crate) fn button_ghost(ui: &mut egui::Ui, size: ButtonSize, label: &str) -> Response {
    Button::ghost(size, label).show(ui)
}

/// A square ghost button showing `icon`, named `name` for screen readers and its tooltip.
pub(crate) fn button_icon(ui: &mut egui::Ui, size: ButtonSize, icon: &str, name: &str, shortcut: Option<&str>) -> Response {
    Button::ghost(size, "").icon(icon).name(name).shortcut(shortcut).show(ui)
}

/// One choice of a [`segmented`] control.
pub(crate) struct Segment<'a> {
    pub label: &'a str,
    pub selected: bool,
    pub tip: Option<&'a str>,
}

/// The mockup's segmented control (`.seg`): choices in an outlined base well (radius 8, inset 2),
/// the chosen one on the selected fill with an accent edge and full text, the others muted
/// (bright and on the hover fill when hovered). Lays out left to right; returns the clicked
/// index.
pub(crate) fn segmented(ui: &mut egui::Ui, segments: &[Segment<'_>]) -> Option<usize> {
    let t = Tokens::get(ui.ctx());
    let font = theme::regular(SEGMENT_TEXT);
    // Laid out in the placeholder colour, so each chip paints it in its own state's colour.
    let galleys: Vec<_> = segments.iter().map(|s| ui.painter().layout_no_wrap(s.label.to_owned(), font.clone(), Color32::PLACEHOLDER)).collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 2.0 * SEGMENT_PAD_X).collect();
    let inner_w: f32 = widths.iter().sum::<f32>() + SEGMENT_INSET * (segments.len().saturating_sub(1)) as f32;
    let size = vec2(inner_w + 2.0 * SEGMENT_INSET, SEGMENT_HEIGHT);
    let (well, _) = ui.allocate_exact_size(size, Sense::hover());
    let control_edge = Surfaces::of(&t).control_edge;
    ui.painter().rect(well, CornerRadius::same(RADIUS_CARD), t.field, Stroke::new(EDGE, control_edge), egui::StrokeKind::Inside);
    let mut clicked = None;
    let mut x = well.left() + SEGMENT_INSET;
    for (i, ((segment, galley), width)) in segments.iter().zip(galleys).zip(widths).enumerate() {
        let rect = Rect::from_min_size(egui::pos2(x, well.top() + SEGMENT_INSET), vec2(width, SEGMENT_HEIGHT - 2.0 * SEGMENT_INSET));
        x += width + SEGMENT_INSET;
        let resp = ui.interact(rect, ui.id().with(("segment", segment.label)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, ui.is_enabled(), segment.selected, segment.label));
        let shown = motion::hover(ui.ctx(), resp.id, resp.hovered());
        let (fill, edge) = if segment.selected { (t.selected, Stroke::new(EDGE, t.accent)) } else { (t.hover.gamma_multiply(shown), Stroke::NONE) };
        ui.painter().rect(rect, CornerRadius::same(t.radius), fill, edge, egui::StrokeKind::Inside);
        focus_ring(ui, &t, rect, &resp, t.radius);
        let color = if segment.selected { t.text } else { t.text_muted.lerp_to_gamma(t.text, shown) };
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
