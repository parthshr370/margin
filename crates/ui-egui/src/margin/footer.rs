//! The footer under the page: the page box (`x / N`, type a number or label), zoom `− 100% +`
//! with its menu and the fit toggle, the reading-progress scrubber (`margin::scrubber`), and the
//! save status.

use egui::{Align, CornerRadius, Layout, Stroke, Vec2, vec2};
use serde_json::{Value, json};

use crate::PdfCraftApp;
use crate::canvas::Fit;
use crate::margin::look::{self, GAP, GAP_LARGE};
use crate::margin::reading;
use crate::theme::{self, Tokens};

/// Tall enough for one control row (`look::CONTROL_ROW`) with room above and below it.
pub const HEIGHT: f32 = 40.0;
/// A zoom stop this close (as a factor) is the zoom shown.
const ZOOM_MATCH: f32 = 0.005;
/// The id of the page box, shared with PdfCraft's `view.focus_page_input` (⇧⌘N).
const PAGE_INPUT: &str = "page-input";
const STATUS_WIDTH: f32 = 200.0;
const DETAIL_MIN_WIDTH: f32 = 180.0;
const DETAIL_MAX_WIDTH: f32 = 300.0;
/// The zoom group's and fit's buttons: the one control height.
const ZOOM_BUTTON: f32 = look::CONTROL_ROW;
const FIT_BUTTON: f32 = look::CONTROL_ROW;
/// The widest the zoom percent and the page position get: their slots keep this width, so the
/// controls after them never move on a zoom step, a page turn or a tab switch.
const PERCENT_TEMPLATE: &str = "0000%";
const POSITION_TEMPLATE: &str = "(0000 / 0000)";
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
    let time_left_on = app.margin_keys.settings().reader.time_left;
    let detail = reading_detail(app, index);
    let Some(doc) = app.session.get(id) else { return };
    let labels: Vec<String> = doc.info.pages.iter().map(|p| p.label.clone()).collect();
    let marks = super::scrubber::Marks {
        pages: labels.len(),
        sections: doc.info.outline.iter().filter_map(|o| o.page).collect(),
        dots: app
            .views
            .get(index)
            .map(|view| super::pages::highlight_dots(&doc.info, &view.comments.notes.markup_indices, &view.margin.marks, &app.notes))
            .unwrap_or_default(),
    };
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
    let mut preview = None;
    // egui draws the panel's top divider line where the theme has frame edges.
    egui::Panel::bottom("margin_footer")
        .exact_size(HEIGHT)
        .show_separator_line(look::has_edges(&t))
        .frame(egui::Frame::NONE.fill(t.surfaces.status_bar).inner_margin(egui::Margin::symmetric(GAP_LARGE as i8, 0)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                page_box(ui, &t, view, &labels);
                command = zoom_group(ui, &t, view, &steps, &keys);
                ui.add_space(GAP_LARGE);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Optional detail yields first: the scrubber takes what is left (and hides
                    // under its least width), then the status narrows, clipped; the page field
                    // and zoom never move.
                    let status_width = STATUS_WIDTH.min(ui.available_width()).max(0.0);
                    ui.allocate_ui(vec2(status_width, HEIGHT), |ui| {
                        ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                        save_command = super::saving::show(ui, &t, &save_line);
                    });
                    // With time left on, its slot is kept before a pace is known (empty until
                    // then), so the scrubber doesn't shrink mid-session.
                    if time_left_on && ui.available_width() >= DETAIL_MIN_WIDTH + super::scrubber::DETAIL_MIN_WIDTH + GAP_LARGE {
                        let width = (ui.available_width() - super::scrubber::DETAIL_MIN_WIDTH - GAP_LARGE).min(DETAIL_MAX_WIDTH);
                        ui.allocate_ui(vec2(width, HEIGHT), |ui| {
                            ui.set_min_width(width);
                            let text = detail.clone().unwrap_or_default();
                            let label =
                                ui.add(egui::Label::new(egui::RichText::new(&text).font(theme::regular(PAGE_TEXT)).color(t.text_muted)).truncate());
                            if !text.is_empty() {
                                label.on_hover_text(text);
                            }
                        });
                    }
                    ui.add_space(GAP_LARGE);
                    preview = super::scrubber::show(ui, &t, view, &marks);
                });
            });
        });
    if let Some((resp, page)) = preview
        && let Some(doc) = app.session.get(id)
    {
        super::scrubber::preview(&resp, &doc.info, &marks, page);
    }
    if let Some((id, args)) = command {
        // The keys' own path: the same zoom stops and fits as `Ctrl+=`, `W` and the palette.
        super::keys::run(app, ui.ctx(), id, &args);
    }
    if let Some(id) = save_command {
        super::keys::run(app, ui.ctx(), id, &Value::Null);
    }
}

/// `reader.time_left`: the time the rest of the document takes at the reader's recorded pace.
/// Nothing until reading statistics have a pace (the header already names the section).
#[cfg(not(target_arch = "wasm32"))]
fn reading_detail(app: &mut PdfCraftApp, index: usize) -> Option<String> {
    if !app.margin_keys.settings().reader.time_left {
        return None;
    }
    let view = app.views.get(index)?;
    let (id, current) = (view.id, view.current);
    let pages = app.session.get(id)?.info.pages.len();
    let left = u32::try_from(pages.saturating_sub(current.saturating_add(1))).ok()?;
    let book = app.margin_stats.book(id)?.to_owned();
    time_left_text(super::stats::summary(app, &book)?.seconds_left(left)?)
}

/// Statistics are desktop-only, so a browser never has a pace.
#[cfg(target_arch = "wasm32")]
fn reading_detail(_app: &mut PdfCraftApp, _index: usize) -> Option<String> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn time_left_text(seconds: f64) -> Option<String> {
    (seconds.is_finite() && seconds >= 0.0).then(|| format!("~{} left", margin_config::stats::format_duration(seconds)))
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
    // Sized by the document's widest label (worked out once per document) and its page count,
    // never by the page shown or what is being typed.
    let widest_id = id.with(("margin-page-widest", view.id.0));
    let widest = match ui.data(|d| d.get_temp::<f32>(widest_id)) {
        Some(widest) => widest,
        None => {
            let count = labels.len().to_string();
            let widest = labels
                .iter()
                .map(String::as_str)
                .chain([count.as_str()])
                .map(|text| ui.painter().layout_no_wrap(text.trim().to_owned(), font.clone(), t.text).size().x)
                .fold(0.0, f32::max);
            ui.data_mut(|d| d.insert_temp(widest_id, widest));
            widest
        }
    };
    let width = (widest + 2.0 * PAGE_FIELD_PAD).clamp(PAGE_FIELD_MIN, PAGE_FIELD_MAX);
    let field = look::field_well(ui, id, vec2(width, look::CONTROL_ROW), |ui| {
        let edit = egui::TextEdit::singleline(&mut view.page_input)
            .id(id)
            .frame(egui::Frame::NONE)
            .desired_width(ui.available_width())
            .char_limit(PAGE_INPUT_CHARS)
            .horizontal_align(Align::Center)
            .font(font.clone())
            .margin(Vec2::ZERO);
        ui.add(edit)
    });
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
        None => d.remove::<String>(hint_id),
    });
    // Read after a refused Enter asked for the focus back: the edit goes on next frame.
    let editing = ui.memory(|m| m.has_focus(id));
    ui.data_mut(|d| d.insert_temp(editing_id, editing));
    let number = (view.current + 1).to_string();
    let position = if current == number { format!("/ {}", labels.len()) } else { format!("({number} / {})", labels.len()) };
    look::reserved_label(ui, &position, POSITION_TEMPLATE, font, t.text_muted, Align::Min);
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

/// `− 100% +` in a control outline, the percent (in a slot as wide as "0000%") opening the zoom
/// menu, then the fit toggle, every button one control tall. Returns the command to run
/// (Margin's `view.zoom.*` / `view.fit.*`, with its arguments).
fn zoom_group(ui: &mut egui::Ui, t: &Tokens, view: &crate::DocView, steps: &[u32], keys: &ZoomKeys) -> Option<(&'static str, Value)> {
    let mut command = None;
    let reads = reading::reads(view);
    // Its full height from the start: `ui.horizontal` starts a row at the default control height
    // and grows it downward, which put the group below the footer's centre line and past the
    // window's bottom edge.
    let group = ui.allocate_ui_with_layout(vec2(ui.available_width(), ZOOM_BUTTON), Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if look::icon_button(ui, "minus", ZOOM_BUTTON, false, "Zoom out", keys.zoom_out.as_deref()).clicked() {
            command = Some(("view.zoom.out", Value::Null));
        }
        let percent = format!("{:.0}%", view.zoom * 100.0);
        let button = look::Button::ghost(look::ButtonSize::Standard, &percent).tip("Zoom and fit");
        let width = button.widest(ui, &[PERCENT_TEMPLATE]);
        let menu = button.fixed_width(width).show(ui);
        look::popover(&menu, egui::Id::new("margin-zoom-menu")).show(|ui| {
            look::menu_body(ui);
            let fits = [
                ("view.fit.width", !reads && view.fit == Fit::Width, "Fit width"),
                ("view.fit.page", !reads && view.fit == Fit::Page, "Fit page"),
                ("view.fit.read", reads, "Fit reading width"),
            ];
            for (id, on, label) in fits {
                if look::menu_row(ui, None, label, None, on).clicked() {
                    command = Some((id, Value::Null));
                }
            }
            if look::menu_row(ui, Some("crop"), "Fit visible content", None, false).clicked() {
                command = Some(("view.fit_visible", Value::Null));
            }
            ui.separator();
            for &step in steps {
                let on = view.fit == Fit::None && !reads && (view.zoom - step as f32 / 100.0).abs() < ZOOM_MATCH;
                if look::menu_row(ui, None, &format!("{step}%"), None, on).clicked() {
                    command = Some(("view.zoom.set", json!({ "percent": step })));
                }
            }
        });
        if look::icon_button(ui, "plus", ZOOM_BUTTON, false, "Zoom in", keys.zoom_in.as_deref()).clicked() {
            command = Some(("view.zoom.in", Value::Null));
        }
    });
    // Painted over the group rather than a frame around it, so the outline adds no size.
    let outline = Stroke::new(look::EDGE, t.surfaces.control_edge);
    ui.painter().rect_stroke(group.response.rect, CornerRadius::same(t.radius), outline, egui::StrokeKind::Inside);
    // `W`: fit width, or back to the reading width.
    let (icon, tip) = if view.fit == Fit::Width { ("maximize-2", "Fit reading width") } else { ("maximize", "Fit width") };
    if look::icon_button(ui, icon, FIT_BUTTON, false, tip, keys.fit.as_deref()).clicked() {
        command = Some(("view.fit.toggle_width", Value::Null));
    }
    command
}
