//! The footer under the page: the page box (`x / N`, type a number or label), zoom `− 100% +`
//! with its menu and the fit toggle, a reading-progress scrubber, and the save status.

use egui::{Align, CornerRadius, Layout, Sense, Stroke, vec2};
use serde_json::{Value, json};

use crate::PdfCraftApp;
use crate::canvas::Fit;
use crate::margin::look::{self, GAP, GAP_LARGE};
use crate::margin::motion;
use crate::margin::reading;
use crate::theme::{self, Tokens};

pub const HEIGHT: f32 = 36.0;
/// A zoom stop this close (as a factor) is the zoom shown.
const ZOOM_MATCH: f32 = 0.005;
/// The id of the page box, shared with PdfCraft's `view.focus_page_input` (⇧⌘N).
const PAGE_INPUT: &str = "page-input";
const STATUS_WIDTH: f32 = 200.0;
const SCRUB_HEIGHT: f32 = 18.0;
/// The scrubber's knob, and its size under the pointer or while dragged.
const KNOB_RADIUS: f32 = 5.0;
const KNOB_RADIUS_HOVER: f32 = 6.0;
const ZOOM_BUTTON: f32 = 22.0;
const FIT_BUTTON: f32 = 24.0;
/// The page field's text size.
const PAGE_TEXT: f32 = 11.5;
/// The page field: room around its text, and its narrowest and widest (a longer label scrolls
/// inside it).
const PAGE_FIELD_PAD: f32 = 6.0;
const PAGE_FIELD_MIN: f32 = 34.0;
const PAGE_FIELD_MAX: f32 = 96.0;
/// The most the page field holds: more than any page label, and a long paste stops here.
const PAGE_INPUT_CHARS: usize = 64;
/// How much of a typed page or a label the field's message repeats.
const HINT_TEXT_CHARS: usize = 16;

pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let Some((index, id)) = app.active_ids() else { return };
    let Some(doc) = app.session.get(id) else { return };
    let labels: Vec<String> = doc.info.pages.iter().map(|p| p.label.clone()).collect();
    let sections: Vec<usize> = doc.info.outline.iter().filter_map(|o| o.page).collect();
    let status = crate::margin::reading::save_status(app, index);
    let save_key = look::shortcut(app, "file.save");
    let save_line = super::saving::line(
        status,
        &super::saving::Context {
            autosave_on: app.margin_keys.settings().reader.autosave,
            has_file: doc.path.is_some(),
            read_only_reason: doc.read_only_reason.as_deref(),
            save_key: save_key.as_deref(),
        },
    );
    let keys = ZoomKeys {
        zoom_out: look::shortcut(app, "view.zoom.out"),
        zoom_in: look::shortcut(app, "view.zoom.in"),
        fit: look::shortcut(app, "view.fit.toggle_width"),
    };
    let steps = app.margin_keys.settings().view.zoom_steps.clone();
    let Some(view) = app.views.get_mut(index) else { return };
    let mut command = None;
    let mut save_command = None;
    // egui draws the panel's top divider line.
    egui::Panel::bottom("margin_footer")
        .exact_size(HEIGHT)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(GAP_LARGE as i8, 0)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                page_box(ui, &t, view, &labels);
                command = zoom_group(ui, &t, view, &steps, &keys);
                ui.add_space(GAP_LARGE);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.allocate_ui(vec2(STATUS_WIDTH, HEIGHT), |ui| {
                        save_command = super::saving::show(ui, &t, &save_line);
                    });
                    ui.add_space(GAP_LARGE);
                    scrubber(ui, &t, view, labels.len(), &sections);
                });
            });
        });
    if let Some((id, args)) = command {
        // The keys' own path: the same zoom stops and fits as `Ctrl+=`, `W` and the palette.
        super::keys::run(app, ui.ctx(), id, &args);
    }
    if let Some(id) = save_command {
        super::keys::run(app, ui.ctx(), id, &Value::Null);
    }
}

/// The page field and what follows it: the current page's label (a page label such as `iv`, or
/// its number), editable, then `/ N`, or the page's position `(4 / 120)` when its label isn't
/// its number. G (`view.focus_page_input`) or a click focuses it with its text selected; Enter
/// goes to the label or number typed (`DocView::go_to_typed`, labels first); Esc puts the
/// current label back. A page it doesn't have leaves the text there to fix, with the document's
/// pages on a card over the field.
fn page_box(ui: &mut egui::Ui, t: &Tokens, view: &mut crate::DocView, labels: &[String]) {
    let id = egui::Id::new(PAGE_INPUT);
    let current = page_label(labels, view.current);
    // PdfCraft's canvas keeps the number in `page_input` while the field is idle; Margin's field
    // shows the label, and an edit starts from it, all selected, however the field got focus
    // (G's command runs before this frame's field, a click inside it).
    let editing_id = id.with("margin-page-editing");
    let was_editing = ui.data(|d| d.get_temp::<bool>(editing_id)).unwrap_or(false);
    let focused = ui.memory(|m| m.has_focus(id));
    if !(focused && was_editing) {
        view.page_input.clone_from(&current);
    }
    if focused && !was_editing {
        select_all(ui.ctx(), id, &view.page_input);
    }
    let font = theme::regular(PAGE_TEXT);
    // Sized by the current label and the page count, never by what is being typed.
    let widest = [current.as_str(), &labels.len().to_string()].map(|text| ui.painter().layout_no_wrap(text.to_owned(), font.clone(), t.text).size().x);
    let width = (widest[0].max(widest[1]) + 2.0 * PAGE_FIELD_PAD).clamp(PAGE_FIELD_MIN, PAGE_FIELD_MAX);
    let edit = egui::TextEdit::singleline(&mut view.page_input)
        .id(id)
        .desired_width(width)
        .char_limit(PAGE_INPUT_CHARS)
        .horizontal_align(Align::Center)
        .font(font.clone())
        .margin(vec2(2.0, 2.0));
    let field = egui::Frame::NONE
        .fill(t.field)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(t.radius))
        .show(ui, |ui| ui.add(edit))
        .inner;
    ui.ctx().accesskit_node_builder(field.id, |node| node.set_label("Go to page"));
    if field.gained_focus() {
        select_all(ui.ctx(), id, &view.page_input);
    }
    let hint_id = id.with("margin-page-hint");
    let mut hint: Option<String> = ui.data(|d| d.get_temp(hint_id));
    if field.changed() {
        hint = None;
    }
    if field.lost_focus() {
        let (enter, escape) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
        hint = None;
        if escape {
            view.page_input.clone_from(&current);
        } else if enter {
            let typed = view.page_input.clone();
            if !view.go_to_typed(&typed, labels) {
                hint = Some(no_such_page(&typed, labels));
                ui.memory_mut(|m| m.request_focus(id));
                select_all(ui.ctx(), id, &typed);
            }
        }
    }
    match &hint {
        Some(message) => look::note(&field, message),
        None => _ = look::tooltip(field, "Go to page: type a label or number and press Enter", None),
    }
    ui.data_mut(|d| match hint {
        Some(message) => _ = d.insert_temp(hint_id, message),
        None => _ = d.remove::<String>(hint_id),
    });
    // Read after a refused Enter asked for the focus back: the edit goes on next frame.
    let editing = ui.memory(|m| m.has_focus(id));
    ui.data_mut(|d| d.insert_temp(editing_id, editing));
    let number = (view.current + 1).to_string();
    let position = if current == number { format!("/ {}", labels.len()) } else { format!("({number} / {})", labels.len()) };
    ui.label(egui::RichText::new(position).font(font).color(t.text_muted));
}

/// Page `index`'s label, or its 1-based number when it has none.
fn page_label(labels: &[String], index: usize) -> String {
    labels.get(index).map(|label| label.trim()).filter(|label| !label.is_empty()).map_or_else(|| (index + 1).to_string(), str::to_owned)
}

/// Selects all of the field `id`'s `text`, so typing replaces it.
fn select_all(ctx: &egui::Context, id: egui::Id, text: &str) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(text.chars().count()));
    state.cursor.set_char_range(Some(all));
    state.store(ctx, id);
}

/// What the page field says about `typed`, a page the document doesn't have: its pages by
/// number, and by label when the labels aren't the numbers.
fn no_such_page(typed: &str, labels: &[String]) -> String {
    let count = labels.len();
    let numbered = (0..count).all(|index| page_label(labels, index) == (index + 1).to_string());
    let range = format!("This PDF has pages 1–{count}");
    if numbered || count == 0 {
        return range;
    }
    let shown = |text: &str| clipped(text.trim(), HINT_TEXT_CHARS);
    let (first, last) = (page_label(labels, 0), page_label(labels, count.saturating_sub(1)));
    format!("No page “{}”. {range}, labelled {} to {}", shown(typed), shown(&first), shown(&last))
}

/// `text`, cut to `max` characters with an ellipsis when longer.
fn clipped(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", text.get(..end).unwrap_or(text)),
        None => text.to_owned(),
    }
}

/// The keys of the zoom group's buttons, from Margin's keymap.
struct ZoomKeys {
    zoom_out: Option<String>,
    zoom_in: Option<String>,
    fit: Option<String>,
}

/// `− 100% +`, the percent opening the zoom menu, then the fit toggle. Returns the command to
/// run (Margin's `view.zoom.*` / `view.fit.*`, with its arguments).
fn zoom_group(ui: &mut egui::Ui, t: &Tokens, view: &crate::DocView, steps: &[u32], keys: &ZoomKeys) -> Option<(&'static str, Value)> {
    let mut command = None;
    let reads = reading::reads(view);
    egui::Frame::NONE.stroke(Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(t.radius)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if look::icon_button(ui, "minus", ZOOM_BUTTON, false, "Zoom out", keys.zoom_out.as_deref()).clicked() {
            command = Some(("view.zoom.out", Value::Null));
        }
        let percent = format!("{:.0}%", view.zoom * 100.0);
        let menu =
            ui.add(egui::Button::new(egui::RichText::new(percent).font(theme::regular(11.5)).color(t.text)).frame(false).min_size(vec2(50.0, 0.0)));
        let menu = look::tooltip(menu, "Zoom and fit", None);
        look::popover(&menu, egui::Id::new("margin-zoom-menu")).show(|ui| {
            ui.set_min_width(170.0);
            let fits = [
                ("view.fit.width", !reads && view.fit == Fit::Width, "Fit width"),
                ("view.fit.page", !reads && view.fit == Fit::Page, "Fit page"),
                ("view.fit.read", reads, "Fit reading width"),
            ];
            for (id, on, label) in fits {
                if ui.radio(on, label).clicked() {
                    command = Some((id, Value::Null));
                }
            }
            if ui.button("Fit visible content").clicked() {
                command = Some(("view.fit_visible", Value::Null));
            }
            ui.separator();
            for &step in steps {
                let on = view.fit == Fit::None && !reads && (view.zoom - step as f32 / 100.0).abs() < ZOOM_MATCH;
                if ui.radio(on, format!("{step}%")).clicked() {
                    command = Some(("view.zoom.set", json!({ "percent": step })));
                }
            }
        });
        if look::icon_button(ui, "plus", ZOOM_BUTTON, false, "Zoom in", keys.zoom_in.as_deref()).clicked() {
            command = Some(("view.zoom.in", Value::Null));
        }
    });
    // `W`: fit width, or back to the reading width.
    let (icon, tip) = if view.fit == Fit::Width { ("maximize-2", "Fit reading width") } else { ("maximize", "Fit width") };
    if look::icon_button(ui, icon, FIT_BUTTON, false, tip, keys.fit.as_deref()).clicked() {
        command = Some(("view.fit.toggle_width", Value::Null));
    }
    command
}

/// The reading position as a track: ticks mark the top-level sections; click or drag to jump.
fn scrubber(ui: &mut egui::Ui, t: &Tokens, view: &mut crate::DocView, pages: usize, sections: &[usize]) {
    let width = ui.available_width();
    if width < 40.0 || pages == 0 {
        return;
    }
    let (rect, resp) = ui.allocate_exact_size(vec2(width, SCRUB_HEIGHT), Sense::click_and_drag());
    let last = pages.saturating_sub(1).max(1) as f32;
    let x_of = |page: usize| rect.left() + rect.width() * (page as f32 / last).clamp(0.0, 1.0);
    let y = rect.center().y;
    let painter = ui.painter();
    painter.hline(rect.x_range(), y, Stroke::new(2.0, t.border));
    for &s in sections {
        painter.vline(x_of(s), (y - 3.0)..=(y + 3.0), Stroke::new(1.0, t.text_faint));
    }
    // The knob glides to a new page (a jump, or page turns) and grows a little under the pointer.
    let share = motion::follow(ui.ctx(), resp.id.with("knob"), view.current as f32 / last, motion::SLIDE).clamp(0.0, 1.0);
    let knob = rect.left() + rect.width() * share;
    let grown = motion::hover(ui.ctx(), resp.id, resp.hovered() || resp.dragged());
    painter.hline(rect.left()..=knob, y, Stroke::new(2.0, t.accent));
    painter.circle_filled(egui::pos2(knob, y), egui::lerp(KNOB_RADIUS..=KNOB_RADIUS_HOVER, grown), t.accent);
    let page_at = |x: f32| (((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0) * last).round() as usize;
    if (resp.clicked() || resp.drag_stopped())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        view.go_to_page(page_at(pos.x).min(pages - 1));
    }
    let hovered_page = resp.hover_pos().map(|pos| page_at(pos.x).min(pages - 1));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, format!("Page {} of {pages}", view.current + 1)));
    if let Some(page) = hovered_page {
        resp.on_hover_text_at_pointer(format!("p. {}", page + 1));
    }
}
