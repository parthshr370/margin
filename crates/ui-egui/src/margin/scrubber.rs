//! The footer's reading-progress track (R8 QoL 24): ticks at the top-level sections, a dot over
//! each page with highlights, and a card at the pointer naming the page, its section and its
//! highlights. Dragging moves through the document as it goes (at most one move per
//! [`PREVIEW_INTERVAL`], so a fast drag asks for few renders) and ends on the page shown; the
//! whole drag is one entry on the view history, so one Back returns to where it started. A click
//! jumps as before (`DocView::go_to_page`).

use std::collections::HashMap;

use egui::{Color32, Response, Sense, Stroke, pos2, vec2};
use pdfcraft_render::DocInfo;

use super::look;
use super::motion;
use super::reading::{self, Spot};
use super::{current_section, outline_at};
use crate::canvas::DocView;
use crate::theme::{self, Tokens};

const HEIGHT: f32 = 18.0;
/// The knob, and its size under the pointer or while dragged.
const KNOB_RADIUS: f32 = 5.0;
const KNOB_RADIUS_HOVER: f32 = 6.0;
/// Below this the footer has no room for a track at all.
const MIN_WIDTH: f32 = 40.0;
/// Below this the track leaves out its section ticks and highlight dots.
pub(crate) const DETAIL_MIN_WIDTH: f32 = 160.0;
/// The closest two ticks, or two dots, are drawn; nearer ones share the first's mark.
const MARK_SPACING: f32 = 4.0;
/// A section tick's half height, and a highlight dot's radius and height above the line.
const TICK_HALF: f32 = 3.0;
const DOT_RADIUS: f32 = 1.5;
const DOT_LIFT: f32 = 5.0;
/// The shortest time between two moves while dragging.
const PREVIEW_INTERVAL: f64 = 0.05;
const CARD_TEXT: f32 = 12.0;

/// What the track shows about the document, gathered before the view is borrowed.
pub(crate) struct Marks {
    pub pages: usize,
    /// The pages the top-level sections start on.
    pub sections: Vec<usize>,
    /// Each marked page's highlight colours (`pages::highlight_dots`).
    pub dots: HashMap<usize, Vec<Color32>>,
}

/// A drag in progress, kept in egui's memory under the track's id.
#[derive(Clone, Copy)]
struct Drag {
    /// The page the view was last moved to.
    shown: usize,
    /// When it was moved there.
    at: f64,
    /// The place before the drag is on the view history.
    remembered: bool,
}

/// Draw the track. Returns it and the page under the pointer while it is hovered or dragged.
pub(crate) fn show(ui: &mut egui::Ui, t: &Tokens, view: &mut DocView, marks: &Marks) -> Option<(Response, usize)> {
    let width = ui.available_width();
    let pages = marks.pages;
    if width < MIN_WIDTH || pages == 0 {
        return None;
    }
    let (rect, resp) = ui.allocate_exact_size(vec2(width, HEIGHT), Sense::click_and_drag());
    // A one-page document's knob sits at the start; nothing divides by zero.
    let last = pages.saturating_sub(1).max(1) as f32;
    let x_of = |page: usize| rect.left() + rect.width() * (page as f32 / last).clamp(0.0, 1.0);
    let share_at = |x: f32| ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
    let page_at = |x: f32| ((share_at(x) * last).round() as usize).min(pages - 1);
    let y = rect.center().y;
    let painter = ui.painter();
    painter.hline(rect.x_range(), y, Stroke::new(2.0, look::Surfaces::of(t).track));
    if rect.width() >= DETAIL_MIN_WIDTH {
        let mut previous = f32::NEG_INFINITY;
        for &page in &marks.sections {
            let x = x_of(page);
            if x - previous >= MARK_SPACING {
                painter.vline(x, (y - TICK_HALF)..=(y + TICK_HALF), Stroke::new(1.0, t.text_faint));
                previous = x;
            }
        }
        let mut marked: Vec<(usize, Color32)> = marks.dots.iter().filter_map(|(&page, colors)| Some((page, *colors.first()?))).collect();
        marked.sort_unstable_by_key(|&(page, _)| page);
        let mut previous = f32::NEG_INFINITY;
        for (page, color) in marked {
            let x = x_of(page);
            if x - previous >= MARK_SPACING {
                painter.circle_filled(pos2(x, y - DOT_LIFT), DOT_RADIUS, color);
                previous = x;
            }
        }
    }
    let pointer = resp.interact_pointer_pos().or_else(|| resp.hover_pos());
    let dragging = resp.dragged() || resp.drag_stopped();
    let pointer_page = pointer.map(|p| page_at(p.x));
    drive(ui, &resp, view, pointer_page, dragging);
    if resp.clicked()
        && let Some(page) = pointer_page
    {
        view.go_to_page(page);
    }
    // The knob follows the pointer while dragged, and glides to a new page otherwise (a jump,
    // page turns); it grows a little under the pointer.
    let target = match pointer {
        Some(p) if dragging => share_at(p.x),
        _ => view.current as f32 / last,
    };
    let followed = motion::follow(ui.ctx(), resp.id.with("knob"), target, motion::SLIDE).clamp(0.0, 1.0);
    let share = if dragging { target } else { followed };
    let knob = rect.left() + rect.width() * share;
    let grown = motion::hover(ui.ctx(), resp.id, resp.hovered() || resp.dragged());
    let painter = ui.painter();
    painter.hline(rect.left()..=knob, y, Stroke::new(2.0, t.accent));
    painter.circle_filled(pos2(knob, y), egui::lerp(KNOB_RADIUS..=KNOB_RADIUS_HOVER, grown), t.accent);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, format!("Page {} of {pages}", view.current + 1)));
    let previewing = resp.hovered() || resp.dragged();
    pointer_page.filter(|_| previewing).map(|page| (resp, page))
}

/// A drag's moves: the first one that changes page puts the place before the drag on the view
/// history; each later one moves the view without adding to it, no sooner than
/// [`PREVIEW_INTERVAL`] after the last; the release lands on the page under the pointer.
fn drive(ui: &egui::Ui, resp: &Response, view: &mut DocView, pointer_page: Option<usize>, dragging: bool) {
    let id = resp.id.with("drag");
    let now = ui.input(|i| i.time);
    if resp.drag_started() {
        ui.data_mut(|d| d.insert_temp(id, Drag { shown: view.current, at: f64::NEG_INFINITY, remembered: false }));
    }
    let Some(mut drag) = ui.data(|d| d.get_temp::<Drag>(id)) else { return };
    if let Some(page) = pointer_page.filter(|_| dragging) {
        let wait = PREVIEW_INTERVAL - (now - drag.at);
        if page != drag.shown && (wait <= 0.0 || resp.drag_stopped()) {
            if !drag.remembered {
                let here = view.here();
                view.remember(here);
                drag.remembered = true;
            }
            reading::go(view, Spot { page, y: 0.0, x: None });
            (drag.shown, drag.at) = (page, now);
        } else if page != drag.shown {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(wait));
        }
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<Drag>(id));
    } else {
        ui.data_mut(|d| d.insert_temp(id, drag));
    }
}

/// The card over the track at the pointer: the page's label (and its position when the label
/// isn't its number), the section it is in, and how many highlights it has.
pub(crate) fn preview(resp: &Response, info: &DocInfo, marks: &Marks, page: usize) {
    let label = info.pages.get(page).map(|p| p.label.trim()).filter(|l| !l.is_empty()).map_or_else(|| (page + 1).to_string(), str::to_owned);
    let position = (label != (page + 1).to_string()).then(|| format!("{} / {}", page + 1, marks.pages));
    let section = current_section(&info.outline, page).and_then(|path| outline_at(&info.outline, &path).map(|item| item.title.trim().to_owned()));
    let highlights = marks.dots.get(&page).map_or(0, Vec::len);
    let x = resp.interact_pointer_pos().or_else(|| resp.hover_pos()).map_or(resp.rect.center().x, |p| p.x);
    look::pointer_card(resp, x, |ui| {
        let t = Tokens::get(ui.ctx());
        let text = |s: String, color| egui::RichText::new(s).font(theme::regular(CARD_TEXT)).color(color);
        ui.horizontal(|ui| {
            ui.label(text(format!("p. {label}"), t.text));
            if let Some(position) = position {
                ui.label(text(position, t.text_faint));
            }
        });
        if let Some(section) = section.filter(|s| !s.is_empty()) {
            ui.label(text(section, t.text_muted));
        }
        match highlights {
            0 => {}
            1 => _ = ui.label(text("1 highlight".to_owned(), t.text_muted)),
            n => _ = ui.label(text(format!("{n} highlights"), t.text_muted)),
        }
    });
}
