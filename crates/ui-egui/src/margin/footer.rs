//! The footer under the page: the page box (`x / N`, type a number or label), zoom `− 100% +`
//! with its menu and the fit toggle, a reading-progress scrubber, and the save status.

use egui::{Align, CornerRadius, Layout, Sense, Stroke, vec2};

use crate::PdfCraftApp;
use crate::canvas::Fit;
use crate::margin::look::{self, GAP, GAP_LARGE};
use crate::margin::motion;
use crate::margin::reading::SaveStatus;
use crate::theme::{self, Tokens};

pub const HEIGHT: f32 = 36.0;
/// The zoom menu's fixed steps, in percent.
const ZOOM_STEPS: [u16; 5] = [75, 100, 125, 150, 200];
/// The id of the page box, shared with PdfCraft's `view.focus_page_input` (⇧⌘N).
const PAGE_INPUT: &str = "page-input";
const STATUS_WIDTH: f32 = 200.0;
const SCRUB_HEIGHT: f32 = 18.0;
/// The scrubber's knob, and its size under the pointer or while dragged.
const KNOB_RADIUS: f32 = 5.0;
const KNOB_RADIUS_HOVER: f32 = 6.0;
const ZOOM_BUTTON: f32 = 22.0;
const FIT_BUTTON: f32 = 24.0;

pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let Some((index, id)) = app.active_ids() else { return };
    let Some(doc) = app.session.get(id) else { return };
    let labels: Vec<String> = doc.info.pages.iter().map(|p| p.label.clone()).collect();
    let sections: Vec<usize> = doc.info.outline.iter().filter_map(|o| o.page).collect();
    let status = crate::margin::reading::save_status(app, index);
    let save_key = look::shortcut(app, "file.save");
    let keys = ZoomKeys {
        zoom_out: look::shortcut(app, "view.zoom.out"),
        zoom_in: look::shortcut(app, "view.zoom.in"),
        fit: look::shortcut(app, "view.fit.toggle_width"),
    };
    let Some(view) = app.views.get_mut(index) else { return };
    let mut command = None;
    // egui draws the panel's top divider line.
    egui::Panel::bottom("margin_footer")
        .exact_size(HEIGHT)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(GAP_LARGE as i8, 0)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                page_box(ui, &t, view, &labels);
                command = zoom_group(ui, &t, view, &keys);
                ui.add_space(GAP_LARGE);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.allocate_ui(vec2(STATUS_WIDTH, HEIGHT), |ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let color = if status == SaveStatus::Unsaved { t.accent_text } else { t.text_muted };
                            let hint = match save_key.as_deref() {
                                Some(key) => format!("Annotations save to the PDF by themselves; {key} saves now"),
                                None => "Annotations save to the PDF by themselves".to_owned(),
                            };
                            ui.label(egui::RichText::new(status.label()).font(theme::regular(11.5)).color(color)).on_hover_text(hint);
                        });
                    });
                    ui.add_space(GAP_LARGE);
                    scrubber(ui, &t, view, labels.len(), &sections);
                });
            });
        });
    if let Some(id) = command {
        app.execute(id);
    }
}

/// `x / N`: the current page, editable (a page label such as `iv`, or a number).
fn page_box(ui: &mut egui::Ui, t: &Tokens, view: &mut crate::DocView, labels: &[String]) {
    let edit = egui::TextEdit::singleline(&mut view.page_input)
        .id(egui::Id::new(PAGE_INPUT))
        .desired_width(34.0)
        .horizontal_align(Align::Center)
        .font(theme::regular(11.5))
        .margin(vec2(2.0, 2.0));
    let field = egui::Frame::NONE
        .fill(t.field)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(t.radius))
        .show(ui, |ui| ui.add(edit))
        .inner;
    let r = look::tooltip(field, "Go to page: type a number or label and press Enter", None);
    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        let typed = view.page_input.clone();
        if !view.go_to_typed(&typed, labels) {
            view.page_input = (view.current + 1).to_string();
        }
    }
    ui.label(egui::RichText::new(format!("/ {}", labels.len())).font(theme::regular(11.5)).color(t.text_muted));
}

/// The keys of the zoom group's buttons, from Margin's keymap.
struct ZoomKeys {
    zoom_out: Option<String>,
    zoom_in: Option<String>,
    fit: Option<String>,
}

/// `− 100% +`, the percent opening the zoom menu, then the fit toggle. Returns a command to run.
fn zoom_group(ui: &mut egui::Ui, t: &Tokens, view: &mut crate::DocView, keys: &ZoomKeys) -> Option<&'static str> {
    let mut command = None;
    egui::Frame::NONE.stroke(Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(t.radius)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if look::icon_button(ui, "minus", ZOOM_BUTTON, false, "Zoom out", keys.zoom_out.as_deref()).clicked() {
            view.zoom_step(false);
        }
        let percent = format!("{:.0}%", view.zoom * 100.0);
        let menu =
            ui.add(egui::Button::new(egui::RichText::new(percent).font(theme::regular(11.5)).color(t.text)).frame(false).min_size(vec2(50.0, 0.0)));
        let menu = look::tooltip(menu, "Zoom and fit", None);
        look::popover(&menu, egui::Id::new("margin-zoom-menu")).show(|ui| {
            ui.set_min_width(170.0);
            for (fit, label) in [(Fit::Width, "Fit width"), (Fit::Page, "Fit page")] {
                if ui.radio(view.fit == fit, label).clicked() {
                    view.set_fit(fit);
                }
            }
            if ui.button("Fit visible content").clicked() {
                command = Some("view.fit_visible");
            }
            ui.separator();
            for step in ZOOM_STEPS {
                let zoom = f32::from(step) / 100.0;
                let on = view.fit == Fit::None && (view.zoom - zoom).abs() < 0.005;
                if ui.radio(on, format!("{step}%")).clicked() {
                    view.set_zoom(zoom);
                }
            }
        });
        if look::icon_button(ui, "plus", ZOOM_BUTTON, false, "Zoom in", keys.zoom_in.as_deref()).clicked() {
            view.zoom_step(true);
        }
    });
    let (icon, tip) = if view.fit == Fit::Width { ("maximize-2", "Fit page") } else { ("maximize", "Fit width") };
    if look::icon_button(ui, icon, FIT_BUTTON, false, tip, keys.fit.as_deref()).clicked() {
        view.set_fit(if view.fit == Fit::Width { Fit::Page } else { Fit::Width });
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
