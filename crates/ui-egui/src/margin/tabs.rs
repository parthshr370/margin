//! Open documents. Every file opens as its own document with its own place, zoom, history and
//! sidebar tab; this module lets the reader see and switch them: a calm tab strip under the
//! header while two or more are open (close ×, middle-click to close, drag to reorder, a dot
//! for unsaved changes), the keymap's `view.tab.next` / `view.tab.prev` / `view.tab.go`, and
//! the Library's "Open now" row. Closing the last document shows the Library.

use egui::{Color32, CornerRadius, Rect, Sense, Stroke, vec2};
use pdfcraft_engine::DocId;
use serde_json::Value;

use super::look::{self, GAP, GAP_LARGE};
use super::reading::{self, SaveStatus};
use super::{ReaderChrome, SidebarTab};
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

pub const STRIP_HEIGHT: f32 = 34.0;
const TAB_HEIGHT: f32 = 26.0;
const TAB_MIN_WIDTH: f32 = 110.0;
const TAB_MAX_WIDTH: f32 = 220.0;
const TAB_RADIUS: u8 = 6;
const NAME_SIZE: f32 = 12.5;
const ICON_SIZE: f32 = 14.0;
const CLOSE_SIZE: f32 = 18.0;
const DOT_RADIUS: f32 = 3.5;
/// The marker drawn where a dragged tab will land.
const DROP_MARK_WIDTH: f32 = 2.0;
/// `view.tab.go`'s last index means the last tab, whatever the count (as in browsers).
const LAST_TAB_SLOT: u64 = 9;

/// Margin's document ids: `view.tab.next`, `view.tab.prev` (wrapping; from the Library they
/// go to the first or last document) and `view.tab.go` with `{"index": 1..9}` (9 is the last
/// document). `false` for another id or bad arguments.
pub(crate) fn run(app: &mut PdfCraftApp, id: &str, args: &Value) -> bool {
    match id {
        "view.tab.next" => step(app, true),
        "view.tab.prev" => step(app, false),
        "view.tab.go" => match args.get("index").and_then(Value::as_u64) {
            Some(slot @ 1..=LAST_TAB_SLOT) => go_to_slot(app, slot),
            _ => return false,
        },
        _ => return false,
    }
    true
}

fn step(app: &mut PdfCraftApp, forward: bool) {
    if let Some(index) = neighbour(app.active, app.views.len(), forward) {
        show(app, index);
    }
}

/// The document `forward` (or back) from `active` among `count`, wrapping round; from the
/// Library (`None`), the first or last one.
fn neighbour(active: Option<usize>, count: usize, forward: bool) -> Option<usize> {
    let last = count.checked_sub(1)?;
    Some(match (active, forward) {
        (None, true) => 0,
        (None, false) => last,
        (Some(index), true) if index >= last => 0,
        (Some(index), true) => index + 1,
        (Some(0), false) => last,
        (Some(index), false) => index.min(last + 1) - 1,
    })
}

fn go_to_slot(app: &mut PdfCraftApp, slot: u64) {
    let count = app.views.len();
    let index = if slot == LAST_TAB_SLOT { count.checked_sub(1) } else { usize::try_from(slot - 1).ok().filter(|index| *index < count) };
    if let Some(index) = index {
        show(app, index);
    }
}

/// Bring document `index` forward, leaving the Library or another document as it is.
pub(crate) fn show(app: &mut PdfCraftApp, index: usize) {
    if index < app.views.len() {
        app.active = Some(index);
        app.combine_tab.focused = false;
    }
}

/// The shown document changes from `chrome.synced_doc` to `shown`: keep the tab the one left
/// had (the tab a search's Find replaced, never Find itself) and give `shown` the tab it had
/// when it was last shown. A document shown for the first time keeps the reader's tab.
pub(crate) fn switch_sidebar_tab(chrome: &mut ReaderChrome, views: &[crate::DocView], shown: Option<DocId>) {
    if let Some(left) = chrome.synced_doc {
        let kept = match chrome.before_find {
            Some((tab, _)) if chrome.tab == SidebarTab::Find => tab,
            _ => chrome.tab,
        };
        if kept != SidebarTab::Find {
            chrome.doc_tabs.insert(left, kept);
        }
    }
    chrome.doc_tabs.retain(|id, _| views.iter().any(|view| view.id == *id));
    if let Some(tab) = shown.and_then(|id| chrome.doc_tabs.get(&id)) {
        chrome.tab = *tab;
        // The search in the document left behind doesn't decide this one's tab.
        chrome.before_find = None;
    }
}

/// Move the document at `from` to position `to`, keeping the active one active.
fn move_tab(app: &mut PdfCraftApp, from: usize, to: usize) {
    if from >= app.views.len() || from == to {
        return;
    }
    let active = app.active_ids().map(|(_, id)| id);
    let view = app.views.remove(from);
    let to = to.min(app.views.len());
    app.views.insert(to, view);
    if let Some(id) = active {
        app.active = app.views.iter().position(|view| view.id == id);
    }
}

/// The strip under the header, while two or more documents are open.
pub(crate) fn strip(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    if app.views.len() < 2 || app.active.is_none() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    egui::Panel::top("margin_tabs")
        .exact_size(STRIP_HEIGHT)
        .show_separator_line(look::has_edges(&t))
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(GAP_LARGE as i8, 0)))
        .show(ui, |ui| {
            let count = app.views.len() as f32;
            let width = ((ui.available_width() - GAP * (count - 1.0)) / count).clamp(TAB_MIN_WIDTH, TAB_MAX_WIDTH);
            egui::ScrollArea::horizontal().id_salt("margin_tabs_scroll").auto_shrink([false, true]).show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    tabs(app, ui, &t, width);
                });
            });
        });
}

/// What the reader did to the tabs this frame, applied once they are all drawn.
#[derive(Default)]
struct Picked {
    show: Option<usize>,
    close: Option<usize>,
    /// The tab being dragged, and whether the drag ended this frame.
    drag: Option<(usize, bool)>,
}

fn tabs(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, width: f32) {
    let mut picked = Picked::default();
    let mut rects = Vec::with_capacity(app.views.len());
    for index in 0..app.views.len() {
        let (resp, close) = tab(app, ui, t, index, width);
        if resp.clicked() {
            picked.show = Some(index);
        }
        if resp.middle_clicked() || close {
            picked.close = Some(index);
        }
        if resp.dragged() || resp.drag_stopped() {
            picked.drag = Some((index, resp.drag_stopped()));
        }
        rects.push(resp.rect);
    }
    let target = picked.drag.zip(ui.ctx().pointer_interact_pos()).map(|((from, done), pointer)| (from, done, drop_slot(&rects, pointer.x)));
    if let Some((from, false, slot)) = target
        && slot != from
        && slot != from + 1
    {
        let x = rects.get(slot).map_or_else(|| rects.last().map_or(0.0, |r| r.right() + GAP / 2.0), |r| r.left() - GAP / 2.0);
        ui.painter().vline(x, ui.max_rect().y_range().shrink(GAP), Stroke::new(DROP_MARK_WIDTH, t.accent));
    }
    if let Some(index) = picked.close {
        app.request_close_tab(index);
    } else if let Some((from, true, slot)) = target {
        // `slot` counts the gaps with the dragged tab still in place; it leaves its place first.
        move_tab(app, from, if slot > from { slot - 1 } else { slot });
    } else if let Some(index) = picked.show {
        show(app, index);
    }
}

/// The gap (0 = before the first tab) nearest `x` among the tabs drawn at `rects`.
fn drop_slot(rects: &[Rect], x: f32) -> usize {
    rects.iter().position(|rect| x < rect.center().x).unwrap_or(rects.len())
}

/// One tab: the document's name, its save state and a close button. Returns its response and
/// whether its × was pressed.
fn tab(app: &PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize, width: f32) -> (egui::Response, bool) {
    let Some(view) = app.views.get(index) else { return (ui.label(""), false) };
    let Some(doc) = app.session.get(view.id) else { return (ui.label(""), false) };
    let active = app.active == Some(index);
    let name = doc.display_name();
    let status = reading::save_status(app, index);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, TAB_HEIGHT), Sense::click_and_drag());
    let a11y = match status_word(status) {
        Some(word) => format!("{name} ({word})"),
        None => name.clone(),
    };
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, active, &a11y));
    look::state_fill(ui, t, rect, &resp, active, TAB_RADIUS);
    look::focus_ring(ui, t, rect, &resp, TAB_RADIUS);
    let painter = ui.painter_at(rect);
    let icon = Rect::from_center_size(rect.left_center() + vec2(GAP + ICON_SIZE / 2.0, 0.0), vec2(ICON_SIZE, ICON_SIZE));
    icons::paint(ui, icon, "file-text", ICON_SIZE, if active { t.accent } else { t.text_muted });
    let close_rect = Rect::from_center_size(rect.right_center() - vec2(GAP / 2.0 + CLOSE_SIZE / 2.0, 0.0), vec2(CLOSE_SIZE, CLOSE_SIZE));
    let text_left = icon.right() + GAP / 1.5;
    let text_width = (close_rect.left() - GAP / 2.0 - text_left).max(0.0);
    let color = if active { look::row_text(t, true, t.text) } else { t.text_muted };
    let mut job = egui::text::LayoutJob::single_section(name.clone(), egui::TextFormat::simple(theme::regular(NAME_SIZE), color));
    job.wrap = egui::text::TextWrapping::truncate_at_width(text_width);
    let galley = ui.painter().layout_job(job);
    painter.galley(egui::pos2(text_left, rect.center().y - galley.size().y / 2.0), galley, color);
    let hovered = resp.hovered() || ui.rect_contains_pointer(close_rect);
    let mut closed = false;
    if hovered || active {
        let close = ui.interact(close_rect, resp.id.with("close"), Sense::click());
        let tint = look::icon_tint(t, false, close.hovered());
        if close.hovered() {
            ui.painter().rect_filled(close_rect, CornerRadius::same(TAB_RADIUS / 2), t.hover);
        }
        icons::paint(ui, close_rect.shrink(3.0), "x", ICON_SIZE - 2.0, tint);
        closed = close.on_hover_text("Close").clicked();
    } else {
        status_dot(ui, t, close_rect.center(), status);
    }
    if let Some(dot_color) = (hovered || active).then(|| dot_color(t, status)).flatten() {
        // The × covers the dot: keep the save state visible as a small mark before it.
        painter.circle_filled(close_rect.left_center() - vec2(DOT_RADIUS, 0.0), DOT_RADIUS / 1.5, dot_color);
    }
    let tip = doc.path.clone().unwrap_or_else(|| name.clone());
    let tip = match status_word(status) {
        Some(word) => format!("{tip}\n{word}"),
        None => tip,
    };
    let resp = if resp.dragged() { resp } else { look::tooltip(resp, &tip, None) };
    (resp, closed)
}

fn status_word(status: SaveStatus) -> Option<&'static str> {
    match status {
        SaveStatus::Saving => Some("saving"),
        SaveStatus::Unsaved | SaveStatus::ReadOnly { dirty: true, .. } | SaveStatus::Signed { dirty: true, .. } => Some("unsaved changes"),
        SaveStatus::Failed => Some("couldn't save"),
        _ => None,
    }
}

/// The save state's dot: none when saved, a ring while autosave writes, filled when only Save
/// will keep the changes.
fn dot_color(t: &Tokens, status: SaveStatus) -> Option<Color32> {
    match status {
        SaveStatus::Saving => Some(t.text_faint),
        SaveStatus::Unsaved | SaveStatus::Failed | SaveStatus::ReadOnly { dirty: true, .. } | SaveStatus::Signed { dirty: true, .. } => {
            Some(t.accent)
        }
        _ => None,
    }
}

fn status_dot(ui: &egui::Ui, t: &Tokens, center: egui::Pos2, status: SaveStatus) {
    match (status, dot_color(t, status)) {
        (SaveStatus::Saving, Some(color)) => _ = ui.painter().circle_stroke(center, DOT_RADIUS, Stroke::new(1.5, color)),
        (_, Some(color)) => _ = ui.painter().circle_filled(center, DOT_RADIUS, color),
        (_, None) => {}
    }
}

/// The Library's "Open now" row: every open document, with its page; a click reads it, the ×
/// closes it.
pub(crate) fn open_now(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    if app.views.is_empty() {
        return;
    }
    let mut show_index = None;
    let mut close_index = None;
    ui.horizontal(|ui| {
        look::section_label(ui, t, "Open now", Some(app.views.len()));
    });
    ui.add_space(GAP);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(GAP, GAP);
        for index in 0..app.views.len() {
            let Some(view) = app.views.get(index) else { continue };
            let Some(doc) = app.session.get(view.id) else { continue };
            let pages = doc.info.pages.len();
            let label = format!("{}   p. {} of {pages}", doc.display_name(), view.current.saturating_add(1).min(pages.max(1)));
            let tip = doc.path.clone().unwrap_or_else(|| doc.display_name());
            let resp = look::Button::secondary(look::ButtonSize::Standard, &label).icon("file-text").tip(&tip).show(ui);
            if resp.clicked() {
                show_index = Some(index);
            }
            if resp.middle_clicked() || look::icon_button(ui, "x", look::BUTTON_HEIGHT, false, "Close", None).clicked() {
                close_index = Some(index);
            }
            ui.add_space(GAP);
        }
    });
    ui.add_space(GAP_LARGE * 2.0);
    if let Some(index) = close_index {
        app.request_close_tab(index);
    } else if let Some(index) = show_index {
        show(app, index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_and_previous_wrap_round() {
        assert_eq!(neighbour(Some(0), 3, true), Some(1));
        assert_eq!(neighbour(Some(2), 3, true), Some(0));
        assert_eq!(neighbour(Some(0), 3, false), Some(2));
        assert_eq!(neighbour(Some(2), 3, false), Some(1));
        assert_eq!(neighbour(None, 3, true), Some(0), "from the Library: the first document");
        assert_eq!(neighbour(None, 3, false), Some(2));
        assert_eq!(neighbour(None, 0, true), None);
        assert_eq!(neighbour(Some(5), 3, false), Some(2), "a stale index stays in range");
    }

    #[test]
    fn drop_slots_follow_tab_centres() {
        let rects: Vec<Rect> = (0..3).map(|i| Rect::from_min_size(egui::pos2(i as f32 * 100.0, 0.0), vec2(90.0, 20.0))).collect();
        assert_eq!(drop_slot(&rects, 10.0), 0);
        assert_eq!(drop_slot(&rects, 60.0), 1);
        assert_eq!(drop_slot(&rects, 250.0), 3);
        assert_eq!(drop_slot(&[], 5.0), 0);
    }

    /// Switching, reordering and closing keep each document's own view and the right one active;
    /// closing the last shows the Library.
    #[test]
    fn documents_switch_reorder_and_close() {
        let mut app = PdfCraftApp::with_flavor(crate::Flavor::Margin);
        app.margin_keys.set_config_dir(std::env::temp_dir().join(format!("margin-tabs-config-{}", std::process::id())));
        for pages in [2, 3, 4] {
            let bytes = app.session.create_blank(300.0, 400.0, pages).unwrap();
            app.open_bytes("doc.pdf", None, bytes.to_vec()).unwrap();
        }
        let ids: Vec<_> = app.views.iter().map(|view| view.id).collect();
        app.views[0].go_to_page(1);
        assert_eq!(app.active, Some(2), "the last opened is active");
        assert!(run(&mut app, "view.tab.next", &Value::Null));
        assert_eq!(app.active, Some(0));
        assert!(run(&mut app, "view.tab.go", &serde_json::json!({ "index": 9 })));
        assert_eq!(app.active, Some(2));
        assert!(!run(&mut app, "view.tab.go", &serde_json::json!({ "index": 0 })));
        move_tab(&mut app, 2, 0);
        assert_eq!(app.views.iter().map(|view| view.id).collect::<Vec<_>>(), [ids[2], ids[0], ids[1]]);
        assert_eq!(app.active, Some(0), "the active document moved with its tab");
        assert_eq!(app.views[1].current, 1, "each document keeps its own place");
        for _ in 0..3 {
            app.request_close_tab(0);
        }
        assert!(app.views.is_empty());
        assert_eq!(app.active, None, "the Library shows");
    }
}
