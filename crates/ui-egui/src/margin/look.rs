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

use crate::margin::keysheet;
use crate::theme::{self, Tokens};
use crate::{Flavor, PdfCraftApp, icons};

/// Spacing rhythm: the small step (between related things) and the large one (between groups,
/// and the sidebar's edge padding).
pub const GAP: f32 = 8.0;
pub const GAP_LARGE: f32 = 12.0;
/// Space between a list row's edge and its text; section labels line up with it.
pub const ROW_PAD: f32 = 8.0;
/// A section label's size and letter spacing (small caps: uppercase, tracked out).
const LABEL_SIZE: f32 = 11.0;
const LABEL_TRACKING: f32 = 0.9;
/// An icon's size within its button.
const ICON_SHARE: f32 = 0.5;

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

/// A row's background: the hover fill under a hovered or selected row, nothing otherwise.
pub(crate) fn row_fill(ui: &egui::Ui, t: &Tokens, rect: Rect, hovered: bool, selected: bool) {
    if hovered || selected {
        ui.painter().rect_filled(rect, CornerRadius::same(t.radius), t.hover);
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
    let mut job = egui::text::LayoutJob::default();
    let format =
        egui::TextFormat { font_id: theme::semibold(LABEL_SIZE), color: t.text_muted, extra_letter_spacing: LABEL_TRACKING, ..Default::default() };
    job.append(&title.to_uppercase(), 0.0, format.clone());
    if let Some(n) = count {
        job.append(&n.to_string(), GAP, egui::TextFormat { font_id: theme::regular(LABEL_SIZE), ..format });
    }
    ui.label(job)
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
    ui.painter().rect(rect, CornerRadius::same(t.radius), button_fill(&t, &resp, on), stroke, egui::StrokeKind::Inside);
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
    ui.painter().rect_filled(rect, CornerRadius::same(t.radius), button_fill(&t, &resp, on));
    let tint = icon_tint(&t, on, resp.hovered());
    let icon_rect = Rect::from_center_size(egui::pos2(rect.left() + GAP + icon_size / 2.0, rect.center().y), Vec2::splat(icon_size));
    icons::paint(ui, icon_rect, icon, icon_size, tint);
    let text_pos = egui::pos2(icon_rect.right() + GAP / 2.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(text_pos, galley, t.text);
    tooltip(resp, tip, None)
}

/// A button's background: pressed, then the hover fill when on or hovered.
fn button_fill(t: &Tokens, resp: &Response, on: bool) -> Color32 {
    if resp.is_pointer_button_down_on() {
        t.pressed
    } else if on || resp.hovered() {
        t.hover
    } else {
        Color32::TRANSPARENT
    }
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
