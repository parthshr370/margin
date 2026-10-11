//! The annotate rail: a floating strip on the configured side of the page area.
//! It takes the place of PdfCraft's quick-action bar in the Margin flavor. Each button runs an
//! existing command; the selected comment tool's controls follow underneath: keep it picked,
//! its colour, opacity and (for lines) thickness, each in a popover on the spacing scale.

use std::ops::RangeInclusive;

use egui::{Rect, Sense, Stroke, vec2};
use pdfcraft_engine::commands;

use crate::comments::{CommentPrefs, CommentTool, SWATCHES, color32};
use crate::margin::look::{self, ColorDot};
use crate::margin::{display, motion};
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, QuickTool};

/// One rail entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailItem<'a> {
    /// The Select tool (V). PdfCraft has no registry command for it.
    Select,
    /// A registered command id.
    Command(&'a str),
    Divider,
}

const BUTTON: f32 = 32.0;
/// The rail's padding around its buttons; its corners follow the buttons' plus this.
const PADDING: u8 = 6;
/// Distance from the page area's edge.
const INSET: f32 = 14.0;

impl<'a> RailItem<'a> {
    fn from_id(id: &'a str, app: &PdfCraftApp) -> Option<Self> {
        match id {
            "|" => Some(Self::Divider),
            "reader.tool.select" => Some(Self::Select),
            _ if commands::command(id).is_some() || app.margin_keys.registry().is_some_and(|registry| registry.title(id).is_some()) => {
                Some(Self::Command(id))
            }
            _ => None,
        }
    }

    fn icon(self, app: &'a PdfCraftApp) -> &'a str {
        match self {
            Self::Select => "mouse-pointer-2",
            Self::Command("edit.snapshot") => "square-dashed-mouse-pointer",
            Self::Command(id) => commands::command(id)
                .map(|command| command.icon)
                .or_else(|| app.margin_keys.registry()?.margin_entries().find(|entry| entry.id.as_str() == id)?.icon.as_deref())
                .unwrap_or("square"),
            Self::Divider => "minus",
        }
    }

    /// Its tooltip: the command's name, and the keys that run it in Margin.
    fn tooltip(self, app: &PdfCraftApp) -> (String, Option<String>) {
        match self {
            Self::Select => ("Select".to_string(), look::shortcut(app, "reader.tool.select")),
            Self::Command("edit.snapshot") => ("Area snapshot: copy a region of the page".to_string(), look::shortcut(app, "edit.snapshot")),
            Self::Command(id) => match commands::command(id) {
                Some(spec) => (crate::i18n::menu_label(spec.id, spec.label), look::shortcut(app, id)),
                None => (app.margin_keys.registry().and_then(|registry| registry.title(id)).unwrap_or(id).to_string(), look::shortcut(app, id)),
            },
            Self::Divider => (String::new(), None),
        }
    }

    /// Whether this tool is the one in use.
    fn active(self, tool: QuickTool) -> bool {
        match self {
            Self::Select => tool == QuickTool::Select,
            Self::Command("edit.snapshot") => tool == QuickTool::Snapshot,
            Self::Command(id) => CommentTool::from_command(id).is_some_and(|c| tool == QuickTool::Comment(c)),
            Self::Divider => false,
        }
    }

    /// Run it: what clicking the button does.
    pub fn run(self, app: &mut PdfCraftApp, ctx: &egui::Context) {
        match self {
            Self::Select => {
                super::keys::run(app, ctx, "reader.tool.select", &serde_json::Value::Null);
            }
            Self::Command(id) => {
                super::keys::run(app, ctx, id, &serde_json::Value::Null);
            }
            Self::Divider => {}
        }
    }
}

/// Whether Margin's Highlight tool draws an area box on a page with this text layer: only on a
/// page known to have no text (a scan). On a text page a drag highlights the text it covers
/// wherever it starts, between lines, in a word gap or in the margin, as the Select tool's drag
/// selects it; PdfCraft's box off a glyph (Acrobat's area highlight) read as a broken highlight.
/// While the page's text is still loading, nothing is boxed.
pub(crate) fn highlight_boxes(text: Option<&pdfcraft_render::PageText>) -> bool {
    text.is_some_and(|text| text.glyphs.is_empty())
}

/// Draw the rail over the page area `area` (the canvas calls this instead of its quick bar). It
/// fades and slides in from the edge over [`motion::SLIDE`], and takes clicks only while open.
pub(crate) fn show(app: &mut PdfCraftApp, area: Rect, ui: &mut egui::Ui) {
    if app.margin_chrome.focus_mode || app.margin_keys.settings().ui.rail.items.is_empty() {
        return;
    }
    let id = egui::Id::new("margin-rail");
    let open = app.margin_chrome.rail_open;
    let shown = motion::toggle(ui.ctx(), id.with("open"), open, motion::SLIDE);
    if shown <= 0.0 {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let side = app.margin_keys.settings().ui.rail.side;
    let (pivot, anchor) = placement(area, side, shown);
    egui::Area::new(id).order(egui::Order::Middle).pivot(pivot).fixed_pos(anchor).interactable(open).fade_in(false).show(ui.ctx(), |ui| {
        if !open {
            // Closing: no clicks (`interactable`) and no keyboard focus either.
            ui.disable();
        }
        ui.set_opacity(shown);
        look::floating_frame(&t, look::RADIUS_FLOATING).inner_margin(egui::Margin::same(PADDING as i8)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.vertical(|ui| {
                let mut clicked = None;
                for command in &app.margin_keys.settings().ui.rail.items {
                    let Some(item) = RailItem::from_id(command, app) else { continue };
                    if item == RailItem::Divider {
                        divider(ui, &t);
                        continue;
                    }
                    let enabled = match item {
                        RailItem::Command(id) => commands::command(id).is_none_or(|spec| app.command_enabled(spec)),
                        _ => true,
                    };
                    let (label, shortcut) = item.tooltip(app);
                    let on = item.active(app.quick_tool);
                    let r = ui.add_enabled_ui(enabled, |ui| look::rail_button(ui, item.icon(app), BUTTON, on, &label, shortcut.as_deref())).inner;
                    if r.clicked() {
                        clicked = Some(command.clone());
                    }
                }
                if let Some(command) = clicked
                    && let Some(item) = RailItem::from_id(&command, app)
                {
                    item.run(app, ui.ctx());
                }
                if let QuickTool::Comment(tool) = app.quick_tool {
                    divider(ui, &t);
                    tool_controls(ui, &t, tool, &mut app.comment_prefs, side);
                }
            });
        });
    });
}

fn placement(area: Rect, side: margin_config::RailSide, shown: f32) -> (egui::Align2, egui::Pos2) {
    let inset = INSET - (1.0 - shown) * motion::RAIL_SHIFT;
    match side {
        margin_config::RailSide::Left => (egui::Align2::LEFT_CENTER, area.left_center() + vec2(inset, 0.0)),
        margin_config::RailSide::Right => (egui::Align2::RIGHT_CENTER, area.right_center() - vec2(inset, 0.0)),
    }
}

fn divider(ui: &mut egui::Ui, t: &Tokens) {
    let (r, _) = ui.allocate_exact_size(vec2(BUTTON, 9.0), Sense::hover());
    ui.painter().hline(r.x_range().shrink(6.0), r.center().y, Stroke::new(1.0, t.divider));
}

/// The tool popovers' width (inside their 16 px frame) and row height, and their text size.
const POPOVER_WIDTH: f32 = 200.0;
const POPOVER_ROW: f32 = 36.0;
const POPOVER_TEXT: f32 = 12.5;
/// Opacity in percent and line thickness in points: their ranges and steps (PdfCraft's).
const OPACITY_PERCENT: RangeInclusive<f64> = 10.0..=100.0;
const OPACITY_STEP: f64 = 5.0;
const PERCENT: f64 = 100.0;
const THICKNESS_PT: RangeInclusive<f64> = 0.5..=12.0;
const THICKNESS_STEP: f64 = 0.5;
/// Two channels within this much (0–1) are the same swatch colour.
const SAME_COLOR: f64 = 0.02;

/// The picked comment tool's controls, PdfCraft's quick-bar ones in the rail's look: keep the
/// tool picked, its colour (the `notes.colors` palette), its opacity and, for lines, thickness.
fn tool_controls(ui: &mut egui::Ui, t: &Tokens, tool: CommentTool, prefs: &mut CommentPrefs, side: margin_config::RailSide) {
    let style = prefs.style(tool);
    let pin = if prefs.pinned { "Keep tool selected: on" } else { "Keep tool selected" };
    if look::rail_button(ui, "pin", BUTTON, prefs.pinned, pin, None).clicked() {
        prefs.pinned = !prefs.pinned;
    }
    let colour = look::color_button(ui, BUTTON, color32(style.color), true, "Colour", None);
    popover(&colour, "colour", side, |ui| {
        let palette = crate::margin::obsidian::swatches(ui.ctx()).unwrap_or_else(|| SWATCHES.iter().map(|(n, c)| ((*n).to_string(), *c)).collect());
        let dots: Vec<ColorDot<'_>> = palette
            .iter()
            .map(|(name, c)| ColorDot {
                name,
                color: color32(*c),
                selected: c.iter().zip(style.color).all(|(a, b)| (a - b).abs() < SAME_COLOR),
                shortcut: None,
            })
            .collect();
        if let Some((_, c)) = look::color_dots(ui, &dots).and_then(|i| palette.get(i)) {
            prefs.set_color(tool, *c);
        }
    });
    let opacity = look::rail_button(ui, "blend", BUTTON, false, "Opacity", None);
    popover(&opacity, "opacity", side, |ui| {
        let mut percent = (style.opacity * PERCENT).round();
        if value_slider(ui, t, "Opacity", &mut percent, OPACITY_PERCENT, OPACITY_STEP, |v| format!("{v:.0} %")) {
            prefs.set_opacity(tool, percent / PERCENT);
        }
    });
    if tool.has_width() {
        let thickness = look::rail_button(ui, "sliders-horizontal", BUTTON, false, "Line thickness", None);
        popover(&thickness, "thickness", side, |ui| {
            let mut width = style.width;
            if value_slider(ui, t, "Line thickness", &mut width, THICKNESS_PT, THICKNESS_STEP, |v| format!("{v:.1} pt")) {
                prefs.set_width(tool, width);
            }
        });
    }
}

/// A popover toward the page from a rail button, in the Display popover's frame, named `which`
/// (`look::popover`: picking a colour or dragging a slider leaves it open).
fn popover(button: &egui::Response, which: &str, side: margin_config::RailSide, contents: impl FnOnce(&mut egui::Ui)) {
    let align = match side {
        margin_config::RailSide::Left => egui::RectAlign::RIGHT_START,
        margin_config::RailSide::Right => egui::RectAlign::LEFT_START,
    };
    look::popover(button, egui::Id::new(("margin-rail-popover", which)))
        .align(align)
        // Clear of the rail's own padding, then the small step.
        .gap(f32::from(PADDING) + look::GAP)
        .frame(display::popover_frame(&button.ctx.global_style()))
        .show(contents);
}

/// A value with its slider: the label and the value (right-aligned) on one row, the slider across
/// the full width on the next, both [`POPOVER_ROW`] tall. True when the slider moved.
fn value_slider(
    ui: &mut egui::Ui,
    t: &Tokens,
    label: &str,
    value: &mut f64,
    range: RangeInclusive<f64>,
    step: f64,
    show: impl Fn(f64) -> String,
) -> bool {
    ui.set_width(POPOVER_WIDTH);
    ui.spacing_mut().item_spacing.y = 0.0;
    let row = vec2(POPOVER_WIDTH, POPOVER_ROW);
    ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.label(egui::RichText::new(label).font(theme::regular(POPOVER_TEXT)).color(t.text_muted));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(show(*value)).font(theme::regular(POPOVER_TEXT)).color(t.text));
        });
    });
    ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Center), |ui| {
        look::slider_style(ui, t);
        ui.spacing_mut().slider_width = POPOVER_WIDTH;
        ui.add(egui::Slider::new(value, range).step_by(step).show_value(false)).changed()
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_items_replace_defaults_and_unknown_ids_are_skipped() {
        let app = PdfCraftApp::with_flavor(crate::Flavor::Margin);
        let configured = ["edit.redo", "|", "not.a.command", "reader.tool.select", "edit.undo"];
        let items: Vec<RailItem<'_>> = configured.iter().filter_map(|id| RailItem::from_id(id, &app)).collect();
        assert_eq!(items, [RailItem::Command("edit.redo"), RailItem::Divider, RailItem::Select, RailItem::Command("edit.undo")]);
        assert!(std::iter::empty::<&str>().filter_map(|id| RailItem::from_id(id, &app)).next().is_none());
    }

    #[test]
    fn left_and_right_rails_anchor_inside_the_requested_edge() {
        let area = Rect::from_min_size(egui::pos2(100.0, 50.0), vec2(600.0, 800.0));
        let (pivot, anchor) = placement(area, margin_config::RailSide::Left, 1.0);
        assert_eq!(pivot, egui::Align2::LEFT_CENTER);
        assert_eq!(anchor.x, area.left() + INSET);
        let (pivot, anchor) = placement(area, margin_config::RailSide::Right, 1.0);
        assert_eq!(pivot, egui::Align2::RIGHT_CENTER);
        assert_eq!(anchor.x, area.right() - INSET);
    }
}
