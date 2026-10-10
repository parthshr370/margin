//! Margin's Pages tab (R8 QoL 10): the mockup's two-column thumbnail browser. Thumbnails take
//! their size from the pane's width and keep each page's aspect ratio; one column only when two
//! would be too small. The current page has a thin accent outline round its thumbnail; pages
//! picked with Ctrl/Shift-click (for the page commands and Print) carry a check badge and the
//! title counts them. Small dots under a thumbnail show its highlights' colours.
//!
//! Reuses PdfCraft's page machinery: `DocView::{thumb, click_page, clear_page_selection,
//! go_to_page}` and the thumbnail renders the canvas schedules while Pages shows (`app.right`
//! is `RightPanel::Pages`, set by `margin::sync`). Only the rows in view are laid out. The grid
//! reveals the current page once when it opens and whenever the reader's page changes, never
//! while the reader scrolls the grid. PdfCraft's own Pages panel (`panels::pages`) is unchanged.

use std::collections::HashMap;

use egui::{Color32, CornerRadius, Rect, Sense, Stroke, pos2, vec2};

use super::look::{self, GAP, GAP_LARGE};
use super::obsidian;
use crate::PdfCraftApp;
use crate::theme::{self, Tokens};

/// Space between the two columns, between rows, and between a thumbnail and its label.
const COLUMN_GAP: f32 = 12.0;
const ROW_GAP: f32 = 14.0;
const LABEL_GAP: f32 = 5.0;
/// A cell's padding round its thumbnail (the hover pill shows in it).
const CELL_PAD: f32 = 6.0;
/// Below this thumbnail width the grid has one column.
const MIN_THUMB_WIDTH: f32 = 84.0;
/// A thumbnail is never taller than this many times its width (a very long page).
const MAX_ASPECT: f32 = 2.0;
/// The page label's size, and its line height.
const LABEL_TEXT: f32 = 12.0;
const LABEL_HEIGHT: f32 = 15.0;
/// The current page's outline.
const CURRENT_OUTLINE: f32 = 1.5;
/// The picked badge: its diameter and its inset from the thumbnail's corner.
const BADGE: f32 = 18.0;
const BADGE_INSET: f32 = 4.0;
/// Highlight dots under a thumbnail: their size, spacing and how many at most.
const DOT: f32 = 6.0;
const DOT_GAP: f32 = 3.0;
const MAX_DOTS: usize = 4;

/// A mark with no colour of its own shows as PDF++'s yellow.
const UNCOLORED_MARK: [f32; 3] = [1.0, 0.816, 0.0];

/// One laid-out row of the grid: its top (from the grid's top) and height.
struct Row {
    top: f32,
    height: f32,
}

/// The grid's geometry for a pane `width` wide.
struct Grid {
    columns: usize,
    /// A cell's width; its thumbnail is this less the cell padding.
    cell: f32,
}

impl Grid {
    fn for_width(width: f32) -> Self {
        let two = (width - COLUMN_GAP) / 2.0;
        let columns = if two - 2.0 * CELL_PAD >= MIN_THUMB_WIDTH { 2 } else { 1 };
        let cell = if columns == 2 { two } else { width };
        Self { columns, cell: cell.max(1.0) }
    }

    fn thumb_width(&self) -> f32 {
        (self.cell - 2.0 * CELL_PAD).max(1.0)
    }

    /// The thumbnail size of a `width` × `height` page.
    fn thumb_size(&self, width: f32, height: f32) -> egui::Vec2 {
        let w = self.thumb_width();
        vec2(w, (w * height / width.max(1.0)).min(w * MAX_ASPECT))
    }

    fn cell_height(&self, width: f32, height: f32) -> f32 {
        CELL_PAD + self.thumb_size(width, height).y + LABEL_GAP + LABEL_HEIGHT + CELL_PAD
    }
}

/// When the grid last showed, and the reader's page then: it reveals the current page when it
/// opens or that page changes.
#[derive(Clone, Copy, PartialEq)]
struct Seen {
    pass: u64,
    current: usize,
}

/// The Pages tab of view `index`.
pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize) {
    let Some(view) = app.views.get_mut(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let info = &doc.info;
    let modal = app.dialog.is_some() || app.palette_open;
    // Escape drops the picked pages while the pointer is over the grid and nothing else wants it
    // (PdfCraft's Pages panel does the same).
    if !modal
        && !view.selected.is_empty()
        && !ui.ctx().egui_wants_keyboard_input()
        && ui.rect_contains_pointer(ui.clip_rect())
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        view.clear_page_selection(Some(view.current));
    }
    ui.add_space(GAP_LARGE);
    ui.horizontal(|ui| {
        look::section_label(ui, t, "Pages", Some(info.pages.len()));
        if view.selected.len() > 1 {
            ui.add_space(GAP);
            ui.label(egui::RichText::new(format!("{} picked", view.selected.len())).font(theme::medium(LABEL_TEXT)).color(t.accent_text));
        }
    });
    ui.add_space(GAP);

    let grid = Grid::for_width(ui.available_width());
    let mut rows = Vec::with_capacity(info.pages.len().div_ceil(grid.columns));
    let mut top = 0.0;
    for pair in info.pages.chunks(grid.columns) {
        let height = pair.iter().map(|p| grid.cell_height(p.width, p.height)).fold(0.0, f32::max);
        rows.push(Row { top, height });
        top += height + ROW_GAP;
    }
    let total = (top - ROW_GAP).max(0.0);
    let dots = highlight_dots(info);

    let seen_id = egui::Id::new(("margin-pages-seen", view.id.0));
    let pass = ui.ctx().cumulative_pass_nr();
    let previous: Option<Seen> = ui.data(|d| d.get_temp(seen_id));
    let reveal = previous.is_none_or(|seen| seen.pass + 1 < pass || seen.current != view.current);
    ui.data_mut(|d| d.insert_temp(seen_id, Seen { pass, current: view.current }));

    let mut go_to = None;
    egui::ScrollArea::vertical().id_salt(("margin-pages", view.id.0)).auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
        let origin = ui.max_rect().min;
        let width = ui.available_width();
        ui.allocate_exact_size(vec2(width, total), Sense::hover());
        if reveal && let Some(row) = rows.get(view.current / grid.columns) {
            let rect = Rect::from_min_size(origin + vec2(0.0, row.top), vec2(width, row.height));
            let shown = viewport.translate(origin.to_vec2());
            if !shown.contains_rect(rect) {
                ui.scroll_to_rect(rect, Some(egui::Align::Center));
            }
        }
        for (r, row) in rows.iter().enumerate() {
            if row.top + row.height < viewport.top() || row.top > viewport.bottom() {
                continue;
            }
            for column in 0..grid.columns {
                let i = r * grid.columns + column;
                let Some(page) = info.pages.get(i) else { break };
                let x = column as f32 * (grid.cell + COLUMN_GAP);
                let cell = Rect::from_min_size(origin + vec2(x, row.top), vec2(grid.cell, grid.cell_height(page.width, page.height)));
                let cell_input = Cell {
                    rect: cell,
                    thumb: grid.thumb_size(page.width, page.height),
                    label: &page.label,
                    current: i == view.current,
                    picked: view.selected.contains(&i),
                    texture: view.thumb(i).map(|tex| tex.id()),
                    dots: dots.get(&i).map_or(&[][..], Vec::as_slice),
                };
                let resp = ui.interact(cell, ui.id().with(("page", i)), Sense::click());
                paint_cell(ui, t, &resp, &cell_input);
                if resp.clicked() {
                    let modifiers = ui.input(|input| input.modifiers);
                    if modifiers.shift || modifiers.command {
                        view.click_page(i, modifiers, true);
                    } else {
                        view.clear_page_selection(Some(i));
                        go_to = Some(i);
                    }
                }
            }
        }
    });
    if let Some(page) = go_to {
        view.go_to_page(page);
    }
}

/// What one cell shows.
struct Cell<'a> {
    rect: Rect,
    thumb: egui::Vec2,
    label: &'a str,
    current: bool,
    picked: bool,
    texture: Option<egui::TextureId>,
    dots: &'a [Color32],
}

fn paint_cell(ui: &egui::Ui, t: &Tokens, resp: &egui::Response, cell: &Cell<'_>) {
    let name = format!("Page {}", cell.label);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, cell.picked, &name));
    look::row_fill(ui, t, cell.rect, resp, cell.picked);
    let painter = ui.painter();
    let thumb = Rect::from_min_size(pos2(cell.rect.center().x - cell.thumb.x / 2.0, cell.rect.top() + CELL_PAD), cell.thumb);
    painter.rect_filled(thumb, CornerRadius::ZERO, Color32::WHITE);
    if let Some(texture) = cell.texture {
        painter.image(texture, thumb, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    let (outline, outline_width) = if cell.current { (t.accent, CURRENT_OUTLINE) } else { (t.border, 1.0) };
    painter.rect_stroke(thumb, CornerRadius::ZERO, Stroke::new(outline_width, outline), egui::StrokeKind::Outside);
    if cell.picked {
        let center = pos2(thumb.right() - BADGE_INSET - BADGE / 2.0, thumb.top() + BADGE_INSET + BADGE / 2.0);
        painter.circle(center, BADGE / 2.0, t.accent, Stroke::new(1.0, t.panel));
        crate::icons::paint(ui, Rect::from_center_size(center, vec2(BADGE, BADGE)), "check", BADGE * 0.6, Color32::WHITE);
    }
    // The label under the thumbnail, centred, clipped to the cell; highlight dots after it.
    let label_y = thumb.bottom() + LABEL_GAP + LABEL_HEIGHT / 2.0;
    let color = if cell.current { t.accent_text } else { t.text_muted };
    let dots_width = cell.dots.len().min(MAX_DOTS) as f32 * (DOT + DOT_GAP);
    let galley = painter.layout_no_wrap(cell.label.to_string(), theme::medium(LABEL_TEXT), color);
    let room = (cell.rect.width() - 2.0 * CELL_PAD - dots_width).max(1.0);
    let label_width = galley.size().x.min(room);
    let start = cell.rect.center().x - (label_width + dots_width) / 2.0;
    let label_rect = Rect::from_min_size(pos2(start, label_y - galley.size().y / 2.0), vec2(label_width, galley.size().y));
    painter.with_clip_rect(label_rect.intersect(cell.rect)).galley(label_rect.min, galley, color);
    for (k, dot) in cell.dots.iter().take(MAX_DOTS).enumerate() {
        let x = label_rect.right() + DOT_GAP + DOT / 2.0 + k as f32 * (DOT + DOT_GAP);
        painter.circle_filled(pos2(x, label_y), DOT / 2.0, *dot);
    }
}

/// Each page's highlight colours, in order, one per highlight (also the footer scrubber's dots).
pub(crate) fn highlight_dots(info: &pdfcraft_render::DocInfo) -> HashMap<usize, Vec<Color32>> {
    let mut dots: HashMap<usize, Vec<Color32>> = HashMap::new();
    for a in obsidian::markup_comments(info) {
        let [r, g, b] = a.color.unwrap_or(UNCOLORED_MARK).map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
        dots.entry(a.page).or_default().push(Color32::from_rgb(r, g, b));
    }
    dots
}
