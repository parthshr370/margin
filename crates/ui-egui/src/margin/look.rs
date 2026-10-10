//! Margin's shared look, the owner's Obsidian "Ember Night": list rows, icon buttons, section
//! labels and tooltips. A selected row gets the hover fill and accent text, with no accent bar;
//! hovering only fills; secondary columns (page numbers) stay muted. Colours and the corner radius
//! are the theme's [`Tokens`], so the palette lives in one place (`theme.rs`).
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
/// [`motion::HOVER`]; nothing otherwise.
pub(crate) fn row_fill(ui: &egui::Ui, t: &Tokens, rect: Rect, resp: &Response, selected: bool) {
    let shown = motion::hover(ui.ctx(), resp.id, resp.hovered() || selected);
    if shown > 0.0 {
        ui.painter().rect_filled(rect, CornerRadius::same(t.radius), t.hover.gamma_multiply(shown));
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

/// A tooltip naming the control, with its shortcut after it in muted text.
pub(crate) fn tooltip(resp: Response, label: &str, shortcut: Option<&str>) -> Response {
    resp.on_hover_ui(|ui| {
        let t = Tokens::get(ui.ctx());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.label(egui::RichText::new(label).color(t.text));
            if let Some(keys) = shortcut {
                ui.label(egui::RichText::new(keys).color(t.text_muted));
            }
        });
    })
}

/// An icon button with Margin's states: the hover fill when hovered, the hover fill and an accent
/// icon when `on`. Screen readers and tests get `label`; the tooltip adds `shortcut`.
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
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), on, label));
    let stroke = border.map_or(Stroke::NONE, |c| Stroke::new(1.0, c));
    ui.painter().rect(rect, CornerRadius::same(t.radius), button_fill(ui.ctx(), &t, &resp, on), stroke, egui::StrokeKind::Inside);
    icons::paint(ui, rect, icon, (size * ICON_SHARE).round(), icon_tint(&t, on, resp.hovered()));
    tooltip(resp, label, shortcut)
}

/// An icon and a word ("Tools") with [`icon_button`]'s states; `tip` is its tooltip.
pub(crate) fn text_button(ui: &mut egui::Ui, icon: &str, label: &str, height: f32, on: bool, tip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let icon_size = (height * ICON_SHARE).round();
    let galley = ui.painter().layout_no_wrap(label.to_owned(), theme::medium(13.0), t.text);
    let width = GAP + icon_size + GAP / 2.0 + galley.size().x + GAP;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), on, label));
    ui.painter().rect_filled(rect, CornerRadius::same(t.radius), button_fill(ui.ctx(), &t, &resp, on));
    let tint = icon_tint(&t, on, resp.hovered());
    let icon_rect = Rect::from_center_size(egui::pos2(rect.left() + GAP + icon_size / 2.0, rect.center().y), Vec2::splat(icon_size));
    icons::paint(ui, icon_rect, icon, icon_size, tint);
    let text_pos = egui::pos2(icon_rect.right() + GAP / 2.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(text_pos, galley, t.text);
    tooltip(resp, tip, None)
}

/// A button's background: pressed, then the hover fill (fading over [`motion::HOVER`]) when on
/// or hovered.
fn button_fill(ctx: &egui::Context, t: &Tokens, resp: &Response, on: bool) -> Color32 {
    let shown = motion::hover(ctx, resp.id, on || resp.hovered());
    if resp.is_pointer_button_down_on() { t.pressed } else { t.hover.gamma_multiply(shown) }
}

/// An icon's colour on a button or tab: the accent when on, full text when hovered.
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
    resp
}

/// One choice of a [`segmented`] control.
pub(crate) struct Segment<'a> {
    pub label: &'a str,
    pub selected: bool,
    pub tip: Option<&'a str>,
}

/// The mockup's segmented control (`.seg`): choices in a hairline-bordered well, the chosen one a
/// raised chip in full text, the others muted. Lays out left to right; returns the clicked index.
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
        let color = if segment.selected { t.text } else { t.text_muted.lerp_to_gamma(t.text, shown) };
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
        let resp = match segment.tip {
            Some(tip) => resp.on_hover_text(tip),
            None => resp,
        };
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}
