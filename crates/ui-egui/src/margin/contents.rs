//! The Contents tab: the document's outline as a reading table of contents (R8 QoL 16).
//!
//! The section being read is marked; inside a closed entry, that entry is marked instead, so
//! the active path always shows. Entries keep the reader's own open and closed choices, not
//! flips of a default that moves with the section. The current section is revealed (its
//! ancestors opened, its row scrolled to the middle once) when Contents opens, after a
//! deliberate jump from elsewhere (anything else that changes the Back history: links, the page
//! field, Back itself) and on "Reveal current". A click in this list opens nothing and scrolls
//! nothing, so the row clicked stays under the pointer. While the reader reads on, the list
//! follows the section only if its row was in view, so a list scrolled away stays put. A chevron
//! only opens or closes; a row goes to its destination. Sections follow the reading line,
//! including multiple section starts on the same page.

use std::collections::HashMap;

use egui::{Align, Align2, Rect, Sense, vec2};
use pdfcraft_render::{DocInfo, OutlineItem};

use super::look::{self, ROW_PAD};
use super::sidebar::{ROW_HEIGHT, ROW_SPACING, empty};
use super::{MAX_OUTLINE_DEPTH, MAX_OUTLINE_ITEMS, links, reading};
use crate::canvas::ViewPlace;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

const INDENT: f32 = 12.0;
/// The expand/collapse chevron's box.
const TWISTY: f32 = 16.0;
/// Text sizes: entry titles and their page labels.
const TITLE_TEXT: f32 = 12.5;
const LABEL_TEXT: f32 = 11.0;

/// One document's Contents list between frames.
#[derive(Clone, Default)]
struct Tree {
    /// The reader's open (true) and closed (false) choices, by outline index path.
    open: HashMap<Vec<usize>, bool>,
    /// The section the list last followed.
    followed: Option<Vec<usize>>,
    /// The Back history's length and top last frame: a change is a deliberate jump.
    seen_history: Option<(usize, Option<ViewPlace>)>,
    /// The pass Contents was last drawn in; a gap means it was opened again.
    drawn_pass: u64,
    /// Whether the marked row was inside the list's view last frame.
    marked_in_view: bool,
    /// Last frame's jump came from a row of this list: the history change it makes is the
    /// reader's own click, not a jump to reveal.
    own_jump: bool,
}

/// The pane's actions in its header.
#[derive(Default)]
struct Links {
    reveal: bool,
    /// Expand all (true) or Collapse all (false).
    all: Option<bool>,
}

/// The Contents tab for the document `index`.
pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize) {
    let Some((view, doc)) = app.active_ids().and_then(|(i, id)| Some((app.views.get(i)?, app.session.get(id)?))) else { return };
    let info = &doc.info;
    let has_outline = !info.outline.is_empty();
    let current = current_section_at(info, reading::spot(view), view.rotation).unwrap_or_default();
    let expandable = info.outline.iter().any(|item| !item.children.is_empty());
    let mut links = Links::default();
    look::pane_header(ui, t, "Contents", None, |ui| {
        if has_outline {
            links = header_links(ui, !current.is_empty(), expandable);
        }
    });
    if !has_outline {
        empty(ui, t, &["This document has no table of contents.", "Pages and Find still get you around."]);
        return;
    }
    let tree_id = egui::Id::new(("margin-contents", view.id.0));
    let mut tree: Tree = ui.data(|d| d.get_temp(tree_id)).unwrap_or_default();
    let pass = ui.ctx().cumulative_pass_nr();
    let history = (view.back.len(), view.back.last().copied());
    if let Some(open) = links.all {
        set_all(&info.outline, &mut tree.open, open);
    }
    let reopened = tree.seen_history.is_none() || tree.drawn_pass.saturating_add(1) < pass;
    let jumped = tree.seen_history.is_some_and(|seen| seen != history) && !std::mem::take(&mut tree.own_jump);
    let reveal = reopened || jumped || links.reveal;
    let moved = tree.followed.as_deref() != Some(current.as_slice());
    if reveal {
        for prefix in (1..current.len()).filter_map(|depth| current.get(..depth)) {
            tree.open.insert(prefix.to_vec(), true);
        }
    }
    // Opening Contents and "Reveal current" centre the row; a jump from elsewhere and reading on
    // only bring it into view, so a row already in view never moves.
    let scroll = if reopened || links.reveal {
        Some(Some(Align::Center))
    } else if jumped || (moved && tree.marked_in_view) {
        Some(None)
    } else {
        None
    };
    tree.followed = Some(current.clone());
    tree.seen_history = Some(history);
    tree.drawn_pass = pass;
    let mut rows = Rows { info, current: &current, open: &tree.open, scroll, go: None, flip: None, drawn: 0, marked_in_view: false };
    egui::ScrollArea::vertical().id_salt(tree_id.with("scroll")).auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = ROW_SPACING;
        for (i, item) in info.outline.iter().enumerate() {
            rows.row(ui, t, item, &mut vec![i]);
        }
    });
    let (go, flip, marked_in_view) = (rows.go, rows.flip, rows.marked_in_view);
    if let Some((path, open)) = flip {
        tree.open.insert(path, open);
    }
    tree.marked_in_view = marked_in_view;
    tree.own_jump = go.is_some();
    ui.data_mut(|d| d.insert_temp(tree_id, tree));
    if let (Some(destination), Some(view)) = (go, app.views.get_mut(index)) {
        links::jump(view, info, destination);
    }
}

/// The header's buttons, right to left: Collapse all, Expand all and Reveal current, icon buttons
/// the size of every pane header's, each off when it has nothing to do (no current entry, no
/// entry with children).
fn header_links(ui: &mut egui::Ui, has_current: bool, expandable: bool) -> Links {
    let mut links = Links::default();
    ui.add_enabled_ui(expandable, |ui| {
        if look::icon_button(ui, "chevron-up", look::BUTTON_HEIGHT, false, "Collapse all", None).clicked() {
            links.all = Some(false);
        }
        if look::icon_button(ui, "chevron-down", look::BUTTON_HEIGHT, false, "Expand all", None).clicked() {
            links.all = Some(true);
        }
    });
    let reveal = ui.add_enabled_ui(has_current, |ui| look::icon_button(ui, "circle-dot", look::BUTTON_HEIGHT, false, "Reveal current", None));
    links.reveal = reveal.inner.clicked();
    links
}

/// Opens or closes every entry that has children (bounded like the list).
fn set_all(outline: &[OutlineItem], open: &mut HashMap<Vec<usize>, bool>, to: bool) {
    let mut stack: Vec<(Vec<usize>, &OutlineItem)> = outline.iter().enumerate().map(|(i, item)| (vec![i], item)).collect();
    let mut visited = 0;
    while let Some((path, item)) = stack.pop() {
        visited += 1;
        if visited > MAX_OUTLINE_ITEMS {
            break;
        }
        if item.children.is_empty() || path.len() > MAX_OUTLINE_DEPTH {
            continue;
        }
        for (i, child) in item.children.iter().enumerate() {
            let mut child_path = path.clone();
            child_path.push(i);
            stack.push((child_path, child));
        }
        open.insert(path, to);
    }
}

/// One frame's drawing of the list.
struct Rows<'a> {
    info: &'a DocInfo,
    /// The path of the section being read.
    current: &'a [usize],
    open: &'a HashMap<Vec<usize>, bool>,
    /// Scroll the marked row into view (and how) this frame.
    scroll: Option<Option<Align>>,
    /// A row's page was clicked.
    go: Option<links::Destination>,
    /// A chevron was clicked: the entry and its new state.
    flip: Option<(Vec<usize>, bool)>,
    drawn: usize,
    marked_in_view: bool,
}

impl Rows<'_> {
    fn row(&mut self, ui: &mut egui::Ui, t: &Tokens, item: &OutlineItem, path: &mut Vec<usize>) {
        self.drawn += 1;
        if self.drawn > MAX_OUTLINE_ITEMS || path.len() > MAX_OUTLINE_DEPTH {
            return;
        }
        let depth = path.len().saturating_sub(1) as f32;
        let open = !item.children.is_empty() && self.open.get(path.as_slice()).copied().unwrap_or(item.open);
        // The current section, or the closed entry that holds it.
        let marked = self.current == path.as_slice() || (!open && self.current.starts_with(path));
        let title = item.title.trim();
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, marked, title));
        look::row_fill(ui, t, rect, &resp, marked);
        if marked {
            self.marked_in_view = ui.clip_rect().contains_rect(rect);
            if let Some(align) = self.scroll {
                resp.scroll_to_me(align);
            }
        }
        let left = rect.left() + ROW_PAD / 2.0 + depth * INDENT;
        if !item.children.is_empty() {
            let twisty = Rect::from_center_size(egui::pos2(left + TWISTY / 2.0, rect.center().y), vec2(TWISTY, TWISTY));
            let click = ui.interact(twisty, ui.id().with(("toc-twisty", path.as_slice())), Sense::click());
            let action = if open { "Collapse" } else { "Expand" };
            click.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{action} {title}")));
            icons::paint(ui, twisty, if open { "chevron-down" } else { "chevron-right" }, 12.0, look::icon_tint(t, false, click.hovered()));
            if look::tooltip(click, action, None).clicked() {
                self.flip = Some((path.clone(), !open));
            }
        }
        // The page column stays muted, selected or not, against the right edge.
        let label = item.page.and_then(|p| self.info.pages.get(p)).map(|p| p.label.clone()).unwrap_or_default();
        let label_width = ui.fonts_mut(|f| f.layout_no_wrap(label.clone(), theme::regular(LABEL_TEXT), t.text_muted).size().x);
        ui.painter().text(rect.right_center() - vec2(ROW_PAD, 0.0), Align2::RIGHT_CENTER, &label, theme::regular(LABEL_TEXT), t.text_muted);
        let text_left = left + TWISTY + 4.0;
        let max_width = (rect.right() - ROW_PAD * 2.0 - label_width - text_left).max(0.0);
        let font = theme::regular(TITLE_TEXT);
        let title_width = ui.fonts_mut(|f| f.layout_no_wrap(title.to_owned(), font.clone(), t.text).size().x);
        crate::widgets::row_text(ui, egui::pos2(text_left, rect.center().y), title, font, look::row_text(t, marked, t.text), max_width);
        // A cut title shows in full on hover.
        let resp = if title_width > max_width { look::tooltip(resp, title, None) } else { resp };
        if resp.clicked()
            && let Some(page) = item.page
        {
            self.go = Some((page, item.view));
        }
        if open {
            for (i, child) in item.children.iter().enumerate() {
                path.push(i);
                self.row(ui, t, child, path);
                path.pop();
            }
        }
    }
}

/// The last section start at or before the reading line, including starts on the
/// same page. Missing positions mean the top of the page.
pub fn current_section_at(info: &pdfcraft_render::DocInfo, spot: reading::Spot, rotation: u16) -> Option<Vec<usize>> {
    let mut best = None;
    let mut best_start = None;
    let mut stack: Vec<(&[pdfcraft_render::OutlineItem], usize)> = vec![(&info.outline, 0)];
    let mut path = Vec::new();
    let mut visited = 0usize;
    while let Some((entries, next)) = stack.pop() {
        path.truncate(stack.len());
        let Some(entry) = entries.get(next) else { continue };
        visited += 1;
        if visited > MAX_OUTLINE_ITEMS {
            break;
        }
        stack.push((entries, next + 1));
        path.push(next);
        if let Some(page) = entry.page {
            let y = links::destination_spot(info, (page, entry.view), rotation).map_or(0.0, |spot| spot.y);
            let start = (page, y);
            // The canvas rounds scroll offsets to device pixels; a line reached by
            // a jump can be fractionally above its requested PDF coordinate.
            let tolerance = info.pages.get(page).map_or(0.0, |geometry| {
                let height = if rotation % 180 == 90 { geometry.width } else { geometry.height };
                1.0 / height.max(1.0)
            });
            let reached = page < spot.page || (page == spot.page && y <= spot.y + tolerance);
            if reached && best_start.is_none_or(|previous| start >= previous) {
                best_start = Some(start);
                best = Some(path.clone());
            }
        }
        if !entry.children.is_empty() && stack.len() < MAX_OUTLINE_DEPTH {
            stack.push((&entry.children, 0));
        }
    }
    best
}
