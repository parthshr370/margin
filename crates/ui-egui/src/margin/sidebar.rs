//! The left sidebar: Library and palette buttons, the document card (cover, title, author,
//! progress), the current tab's pane, and the tab strip at the bottom.
//!
//! Panes reuse PdfCraft's panels: Pages, Bookmarks and Find are the right panel's bodies
//! (`panels::panel_body`), Highlights is the Comments list limited to text markup, Tools is the
//! tool catalogue (`panels::all_tools` / `tool_detail`). Contents is the outline as a reading
//! table of contents, marking the section being read.

use std::collections::HashSet;

use egui::{Align2, CornerRadius, Rect, Sense, Stroke, vec2};
use pdfcraft_engine::catalog;
use pdfcraft_render::{DocInfo, OutlineItem};

use crate::margin::{MAX_OUTLINE_DEPTH, MAX_OUTLINE_ITEMS, SidebarTab, current_section, open_tab};
use crate::panels::Nav;
use crate::theme::{self, Tokens};
use crate::{DocView, LeftPanel, PdfCraftApp, icons};

const DEFAULT_WIDTH: f32 = 296.0;
const MIN_WIDTH: f32 = 240.0;
const MAX_WIDTH: f32 = 440.0;
const TAB_HEIGHT: f32 = 32.0;
const ROW_HEIGHT: f32 = 26.0;
const INDENT: f32 = 14.0;
const COVER: egui::Vec2 = vec2(52.0, 67.0);
/// Annotation subtypes the Highlights tab lists.
const MARKUP: [&str; 4] = ["Highlight", "Underline", "StrikeOut", "Squiggly"];

pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::left("margin_sidebar")
        .resizable(true)
        .default_size(DEFAULT_WIDTH)
        .size_range(MIN_WIDTH..=MAX_WIDTH)
        .frame(egui::Frame::NONE.fill(t.panel).stroke(Stroke::new(1.0, t.divider)))
        .show(ui, |ui| {
            egui::Panel::top("margin_sidebar_top")
                .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: 10, right: 10, top: 10, bottom: 0 }).stroke(Stroke::new(1.0, t.divider)))
                .show(ui, |ui| {
                    top_buttons(app, ui);
                    document_card(app, ui, &t);
                });
            egui::Panel::bottom("margin_sidebar_tabs")
                .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: 10, right: 10, top: 8, bottom: 10 }).stroke(Stroke::new(1.0, t.divider)))
                .show(ui, |ui| tab_strip(app, ui, &t));
            egui::CentralPanel::default().frame(egui::Frame::NONE.inner_margin(egui::Margin { left: 10, right: 8, top: 4, bottom: 8 })).show(
                ui,
                |ui| {
                    pane(app, ui, &t);
                },
            );
        });
}

fn top_buttons(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        if icons::button(ui, "book-open", 30.0, false, "Library").clicked() {
            app.active = None;
        }
        let palette = match pdfcraft_engine::commands::command("view.palette").and_then(|c| c.shortcut) {
            Some(s) => format!("Command palette  ({})", s.label(cfg!(target_os = "macos"))),
            None => "Command palette".to_string(),
        };
        if icons::button(ui, "square-terminal", 30.0, app.palette_open, &palette).clicked() {
            app.palette_open = true;
        }
    });
}

/// Cover, title, author and how far in the reader is.
fn document_card(app: &PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let Some((view, doc)) = app.active_ids().and_then(|(i, id)| Some((app.views.get(i)?, app.session.get(id)?))) else { return };
    let info = &doc.info;
    let pages = info.pages.len();
    ui.add_space(6.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let (cover, _) = ui.allocate_exact_size(COVER, Sense::hover());
        match view.thumb(0) {
            Some(tex) => {
                let size = tex.size_vec2();
                let scale = (COVER.x / size.x.max(1.0)).min(COVER.y / size.y.max(1.0));
                let rect = Rect::from_center_size(cover.center(), size * scale);
                ui.painter().image(tex.id(), rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            None => {
                ui.painter().rect_filled(cover, CornerRadius::same(3), t.card);
                icons::paint(ui, cover, "file-text", 22.0, t.text_faint);
            }
        }
        ui.vertical(|ui| {
            let width = ui.available_width();
            let title = info.title.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| doc.display_name());
            let mut job = egui::text::LayoutJob::simple(title, theme::semibold(13.0), t.text, width);
            job.wrap.max_rows = 2;
            ui.label(job);
            if let Some(author) = info.author.as_deref().filter(|a| !a.trim().is_empty()) {
                ui.add(egui::Label::new(egui::RichText::new(author).font(theme::regular(12.0)).color(t.text_muted)).truncate());
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let percent = (view.current + 1).saturating_mul(100).checked_div(pages).unwrap_or(0);
                ui.label(egui::RichText::new(format!("{percent}%")).font(theme::regular(11.0)).color(t.text_faint));
                let bar_width = (ui.available_width() - 44.0).max(10.0);
                let (bar, _) = ui.allocate_exact_size(vec2(bar_width, 3.0), Sense::hover());
                ui.painter().rect_filled(bar, CornerRadius::same(2), t.border);
                let fill = Rect::from_min_size(bar.min, vec2(bar.width() * percent as f32 / 100.0, bar.height()));
                ui.painter().rect_filled(fill, CornerRadius::same(2), t.accent);
                ui.label(egui::RichText::new(format!("{pages} pp")).font(theme::regular(11.0)).color(t.text_faint));
            });
        });
    });
    ui.add_space(12.0);
}

fn tab_strip(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let gap = 4.0;
    let count = SidebarTab::STRIP.len() as f32;
    let width = ((ui.available_width() - gap * (count - 1.0)) / count).max(TAB_HEIGHT);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for tab in SidebarTab::STRIP {
            let on = app.margin_chrome.tab == tab;
            let (rect, resp) = ui.allocate_exact_size(vec2(width, TAB_HEIGHT), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, tab.label()));
            if on {
                ui.painter().rect_filled(rect, CornerRadius::same(7), t.selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, CornerRadius::same(7), t.hover);
            }
            icons::paint(ui, rect, tab.icon(), 16.0, if on { t.text } else { t.text_muted });
            if resp.on_hover_text(tab.label()).clicked() {
                open_tab(app, tab);
            }
        }
    });
}

/// A pane's small-caps title row.
fn pane_title(ui: &mut egui::Ui, t: &Tokens, title: &str, count: Option<usize>) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title.to_uppercase()).font(theme::semibold(11.0)).color(t.text_faint));
        if let Some(n) = count {
            ui.label(egui::RichText::new(n.to_string()).font(theme::regular(11.0)).color(t.text_faint));
        }
    });
    ui.add_space(4.0);
}

/// The mockup's empty state: a muted message, centred.
fn empty(ui: &mut egui::Ui, t: &Tokens, lines: &[&str]) {
    ui.add_space(30.0);
    ui.vertical_centered(|ui| {
        for line in lines {
            ui.label(egui::RichText::new(*line).font(theme::regular(12.5)).color(t.text_faint));
        }
    });
}

fn pane(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let Some((index, _)) = app.active_ids() else { return };
    match app.margin_chrome.tab {
        SidebarTab::Contents => contents(app, ui, t, index),
        SidebarTab::Highlights => highlights(app, ui, t, index),
        SidebarTab::Tools => tools(app, ui, t),
        SidebarTab::Find => {
            // The Search panel lists the find bar's search; it needs one to show.
            if let Some(view) = app.views.get_mut(index)
                && view.find.is_none()
            {
                view.open_find();
            }
            crate::panels::panel_body(app, ui);
        }
        SidebarTab::Pages | SidebarTab::Bookmarks | SidebarTab::Other(_) => crate::panels::panel_body(app, ui),
    }
}

/// The table of contents: the outline, with the section being read marked; click to go there.
fn contents(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize) {
    let Some((view, doc)) = app.active_ids().and_then(|(i, id)| Some((app.views.get(i)?, app.session.get(id)?))) else { return };
    let info = &doc.info;
    pane_title(ui, t, "Contents", None);
    if info.outline.is_empty() {
        empty(ui, t, &["This document has no table of contents.", "Pages and Find still get you around."]);
        return;
    }
    let current = current_section(&info.outline, view.current).unwrap_or_default();
    let toggled_id = egui::Id::new(("margin-toc-toggled", view.id.0));
    let mut toggled: HashSet<Vec<usize>> = ui.data(|d| d.get_temp(toggled_id)).unwrap_or_default();
    let mut cx = TocCtx { info, current: &current, toggled: &mut toggled, go: None, flip: None, drawn: 0 };
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for (i, item) in info.outline.iter().enumerate() {
            toc_row(ui, t, item, &mut vec![i], &mut cx);
        }
    });
    let (go, flip) = (cx.go, cx.flip);
    if let Some(path) = flip
        && !toggled.remove(&path)
    {
        toggled.insert(path);
    }
    ui.data_mut(|d| d.insert_temp(toggled_id, toggled));
    if let (Some(page), Some(view)) = (go, app.views.get_mut(index)) {
        view.go_to_page(page);
    }
}

struct TocCtx<'a> {
    info: &'a DocInfo,
    /// The path of the section being read.
    current: &'a [usize],
    /// Entries whose open state the user flipped from the default.
    toggled: &'a mut HashSet<Vec<usize>>,
    go: Option<usize>,
    flip: Option<Vec<usize>>,
    drawn: usize,
}

fn toc_row(ui: &mut egui::Ui, t: &Tokens, item: &OutlineItem, path: &mut Vec<usize>, cx: &mut TocCtx<'_>) {
    cx.drawn += 1;
    if cx.drawn > MAX_OUTLINE_ITEMS || path.len() > MAX_OUTLINE_DEPTH {
        return;
    }
    let depth = path.len().saturating_sub(1) as f32;
    let is_current = cx.current == path.as_slice();
    // Open by default: entries the document opens, and the ones holding the current section.
    let default_open = item.open || cx.current.starts_with(path);
    let open = default_open != cx.toggled.contains(path.as_slice());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, is_current, &item.title));
    if is_current || resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(6), t.hover);
    }
    if is_current {
        let bar = Rect::from_min_size(rect.min + vec2(0.0, 6.0), vec2(2.0, rect.height() - 12.0));
        ui.painter().rect_filled(bar, CornerRadius::same(1), t.accent);
    }
    let left = rect.left() + 4.0 + depth * INDENT;
    if !item.children.is_empty() {
        let twisty = Rect::from_min_size(egui::pos2(left, rect.top() + 5.0), vec2(16.0, 16.0));
        let click = ui.interact(twisty, ui.id().with(("toc-twisty", path.as_slice())), Sense::click());
        icons::paint(ui, twisty, if open { "chevron-down" } else { "chevron-right" }, 12.0, t.text_faint);
        if click.on_hover_text(if open { "Collapse" } else { "Expand" }).clicked() {
            cx.flip = Some(path.clone());
        }
    }
    let label = item.page.and_then(|p| cx.info.pages.get(p)).map(|p| p.label.clone()).unwrap_or_default();
    let label_width = ui.fonts_mut(|f| f.layout_no_wrap(label.clone(), theme::regular(11.0), t.text_faint).size().x);
    ui.painter().text(rect.right_center() - vec2(8.0, 0.0), Align2::RIGHT_CENTER, &label, theme::regular(11.0), t.text_faint);
    let text_left = left + 20.0;
    let max_width = (rect.right() - 16.0 - label_width - text_left).max(0.0);
    crate::widgets::row_text(
        ui,
        egui::pos2(text_left, rect.center().y),
        item.title.trim(),
        theme::regular(12.5),
        if is_current { t.text } else { t.text_muted },
        max_width,
    );
    if resp.clicked()
        && let Some(page) = item.page
    {
        cx.go = Some(page);
    }
    if open {
        for (i, child) in item.children.iter().enumerate() {
            path.push(i);
            toc_row(ui, t, child, path, cx);
            path.pop();
        }
    }
}

/// The Comments list showing only highlights, underlines, strikethroughs and squiggles.
fn highlights(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize) {
    let Some((_, id)) = app.active_ids() else { return };
    let Some(doc) = app.session.get(id) else { return };
    let info = &doc.info;
    let is_markup = |a: &pdfcraft_render::Annotation| MARKUP.contains(&a.subtype.as_str());
    let count = info.annotations.iter().filter(|a| a.in_reply_to.is_none() && is_markup(a)).count();
    pane_title(ui, t, "Highlights", Some(count));
    if count == 0 {
        empty(ui, t, &["No highlights yet.", "Pick Highlight in the rail, then select text."]);
        return;
    }
    let Some(view) = app.views.get_mut(index) else { return };
    // Hide every other kind for this draw only; the Comments panel's own filters stay as they were.
    let kept = view.comments.hidden_types.clone();
    for a in info.annotations.iter().filter(|a| !is_markup(a)) {
        let kind = crate::comments_panel::kind_label(a).to_string();
        if !view.comments.hidden_types.contains(&kind) {
            view.comments.hidden_types.push(kind);
        }
    }
    let mut nav = None;
    // Read-only list: no "Add a comment" box (it would add a note, which this list hides).
    let edit = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| crate::comments_panel::show(ui, t, info, view, &app.comment_prefs, false, &mut nav))
        .inner;
    view.comments.hidden_types = kept;
    if let Some(edit) = edit {
        app.apply_edit(edit);
    }
    navigate(app.views.get_mut(index), nav);
}

fn navigate(view: Option<&mut DocView>, nav: Option<Nav>) {
    let Some(view) = view else { return };
    match nav {
        Some(Nav::Page(p)) => view.go_to_page(p),
        Some(Nav::Flash(p, r)) => {
            view.go_to_page(p);
            view.flash = Some((p, r, 0.0));
        }
        None => {}
    }
}

/// The tool catalogue, and a picked tool's options (PdfCraft's left panel).
fn tools(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    // Shown now, even if the tab was picked this frame (after `sync`).
    app.left_open = true;
    match app.left {
        LeftPanel::AllTools => crate::panels::all_tools(app, ui, t),
        LeftPanel::Tool(id) => match catalog::group(id) {
            Some(group) => crate::panels::tool_detail(app, ui, t, group),
            None => app.left = LeftPanel::AllTools,
        },
    }
    // The panel's close button.
    if !app.left_open {
        app.margin_chrome.sidebar_open = false;
    }
}
