//! The left sidebar: Library and palette buttons, the document card (cover, title, author,
//! progress), the current tab's pane, and the tab strip at the bottom.
//!
//! Panes reuse PdfCraft's panels: Pages, Bookmarks and Find are the right panel's bodies
//! (`panels::panel_body`), Highlights is the Comments list limited to text markup, Tools is the
//! tool catalogue (`panels::all_tools` / `tool_detail`). Contents is the outline as a reading
//! table of contents, marking the section being read.

use std::collections::HashSet;

use egui::{Align2, CornerRadius, Rect, Sense, vec2};
use pdfcraft_engine::catalog;
use pdfcraft_render::{DocInfo, OutlineItem};

use crate::margin::look::{self, GAP, GAP_LARGE, ROW_PAD, RowLook};
use crate::margin::{MAX_OUTLINE_DEPTH, MAX_OUTLINE_ITEMS, SidebarTab, current_section, open_tab, outline_at, reading};
use crate::panels::Nav;
use crate::theme::{self, Tokens};
use crate::{DocView, LeftPanel, PdfCraftApp, icons};

const DEFAULT_WIDTH: f32 = 296.0;
const MIN_WIDTH: f32 = 240.0;
const MAX_WIDTH: f32 = 440.0;
const TAB_HEIGHT: f32 = 32.0;
const TOP_BUTTON: f32 = 30.0;
const ROW_HEIGHT: f32 = 28.0;
/// Rows sit nearly touching, so neighbouring hover fills read as one list.
const ROW_SPACING: f32 = 2.0;
const INDENT: f32 = 12.0;
/// The expand/collapse chevron's box.
const TWISTY: f32 = 16.0;
const COVER: egui::Vec2 = vec2(52.0, 67.0);
/// Annotation subtypes the Highlights tab lists.
const MARKUP: [&str; 4] = ["Highlight", "Underline", "StrikeOut", "Squiggly"];
/// The page bookmark list scrolls past this height, leaving room for the PDF's bookmarks.
const PAGE_BOOKMARKS_MAX_HEIGHT: f32 = 220.0;
const ROW_BUTTON: f32 = 22.0;

pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    // Each panel draws its own divider line (egui's separator, in the theme's divider colour).
    egui::Panel::left("margin_sidebar")
        .resizable(true)
        .default_size(DEFAULT_WIDTH)
        .size_range(MIN_WIDTH..=MAX_WIDTH)
        .frame(egui::Frame::NONE.fill(t.panel))
        .show(ui, |ui| {
            // The reader sets the sidebar's width by dragging its edge; its contents never do.
            // egui widens a panel to what its contents use, so a pane row a little too wide
            // (Bookmarks' search field was) widened it more every frame, shrinking the page and
            // moving it. The panes draw in a child the panel doesn't measure, clipped to it.
            let rect = ui.max_rect();
            let mut body = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
            body.set_clip_rect(rect.intersect(ui.clip_rect()));
            sections(app, &mut body, &t);
            ui.expand_to_include_rect(rect);
        });
}

/// The sidebar's three parts: buttons and the document card, the pane, the tab strip.
fn sections(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let edge = GAP_LARGE as i8;
    egui::Panel::top("margin_sidebar_top")
        .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: edge, right: edge, top: edge, bottom: 0 }))
        .show(ui, |ui| {
            top_buttons(app, ui);
            document_card(app, ui, t);
        });
    egui::Panel::bottom("margin_sidebar_tabs")
        .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: edge, right: edge, top: GAP as i8, bottom: edge }))
        .show(ui, |ui| tab_strip(app, ui, t));
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: GAP as i8, right: GAP as i8, top: 0, bottom: GAP as i8 }))
        .show(ui, |ui| pane(app, ui, t));
}

fn top_buttons(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        if look::icon_button(ui, "book-open", TOP_BUTTON, false, "Library", None).clicked() {
            app.active = None;
        }
        let palette = look::shortcut(app, "view.palette");
        if look::icon_button(ui, "square-terminal", TOP_BUTTON, app.palette_open, "Command palette", palette.as_deref()).clicked() {
            app.palette_open = true;
        }
    });
}

/// Cover, title, author and how far in the reader is: the title in full text, everything else
/// muted, the progress bar's fill the only accent.
fn document_card(app: &PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let Some((view, doc)) = app.active_ids().and_then(|(i, id)| Some((app.views.get(i)?, app.session.get(id)?))) else { return };
    let info = &doc.info;
    let pages = info.pages.len();
    let radius = CornerRadius::same(t.radius);
    ui.add_space(GAP);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = GAP_LARGE;
        let (cover, _) = ui.allocate_exact_size(COVER, Sense::hover());
        match view.thumb(0) {
            Some(tex) => {
                let size = tex.size_vec2();
                let scale = (COVER.x / size.x.max(1.0)).min(COVER.y / size.y.max(1.0));
                let rect = Rect::from_center_size(cover.center(), size * scale);
                ui.painter().add(
                    egui::epaint::RectShape::filled(rect, radius, egui::Color32::WHITE)
                        .with_texture(tex.id(), Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))),
                );
            }
            None => {
                ui.painter().rect_filled(cover, radius, t.hover);
                icons::paint(ui, cover, "file-text", 22.0, t.text_muted);
            }
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let width = ui.available_width();
            let title = info.title.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| doc.display_name());
            let mut job = egui::text::LayoutJob::simple(title, theme::semibold(13.0), t.text, width);
            job.wrap.max_rows = 2;
            ui.label(job);
            if let Some(author) = info.author.as_deref().filter(|a| !a.trim().is_empty()) {
                ui.add(egui::Label::new(egui::RichText::new(author).font(theme::regular(12.0)).color(t.text_muted)).truncate());
            }
            ui.add_space(GAP);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                let percent = (view.current + 1).saturating_mul(100).checked_div(pages).unwrap_or(0);
                let meta = |text: &str| egui::RichText::new(text).font(theme::regular(11.0)).color(t.text_muted);
                ui.label(meta(&format!("{percent}%")));
                let count = format!("{pages} pp");
                let count_width = ui.fonts_mut(|f| f.layout_no_wrap(count.clone(), theme::regular(11.0), t.text_muted).size().x);
                let bar_width = (ui.available_width() - count_width - GAP).max(GAP);
                let (bar, _) = ui.allocate_exact_size(vec2(bar_width, 3.0), Sense::hover());
                ui.painter().rect_filled(bar, CornerRadius::same(2), t.divider);
                let fill = Rect::from_min_size(bar.min, vec2(bar.width() * percent as f32 / 100.0, bar.height()));
                ui.painter().rect_filled(fill, CornerRadius::same(2), t.accent);
                ui.label(meta(&count));
            });
        });
    });
    ui.add_space(GAP_LARGE);
}

/// The tabs: the shown one gets the hover fill and an accent icon, like a selected row.
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
            look::row_fill(ui, t, rect, resp.hovered(), on);
            icons::paint(ui, rect, tab.icon(), 16.0, look::icon_tint(t, on, resp.hovered()));
            // The keymap opens a tab with `view.sidebar.tab` and its key (Ctrl+1…4 by default).
            let keys = tab.key().and_then(|key| look::shortcut_with(app, "view.sidebar.tab", &serde_json::json!({ "tab": key })));
            if look::tooltip(resp, tab.label(), keys.as_deref()).clicked() {
                open_tab(app, tab);
            }
        }
    });
}

/// A pane's title: its small-caps section label, lined up with the rows' text.
fn pane_title(ui: &mut egui::Ui, t: &Tokens, title: &str, count: Option<usize>) {
    ui.add_space(GAP_LARGE);
    ui.horizontal(|ui| look::section_label(ui, t, title, count));
    ui.add_space(GAP);
}

/// The mockup's empty state: a muted message, centred.
fn empty(ui: &mut egui::Ui, t: &Tokens, lines: &[&str]) {
    ui.add_space(GAP_LARGE * 2.0);
    ui.vertical_centered(|ui| {
        for line in lines {
            ui.label(egui::RichText::new(*line).font(theme::regular(12.5)).color(t.text_muted));
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
            ui.add_space(GAP_LARGE);
            crate::panels::panel_body(app, ui);
        }
        SidebarTab::Bookmarks => {
            // Margin's page bookmarks first, then the PDF's bookmarks (its outline, editable).
            page_bookmarks(app, ui, t, index);
            crate::panels::panel_body(app, ui);
        }
        SidebarTab::Pages | SidebarTab::Other(_) => {
            // PdfCraft's panel, its title row a section label here (`look::RowLook::Ember`).
            ui.add_space(GAP_LARGE);
            crate::panels::panel_body(app, ui);
        }
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
        ui.spacing_mut().item_spacing.y = ROW_SPACING;
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
    look::row_fill(ui, t, rect, resp.hovered(), is_current);
    let left = rect.left() + ROW_PAD / 2.0 + depth * INDENT;
    if !item.children.is_empty() {
        let twisty = Rect::from_center_size(egui::pos2(left + TWISTY / 2.0, rect.center().y), vec2(TWISTY, TWISTY));
        let click = ui.interact(twisty, ui.id().with(("toc-twisty", path.as_slice())), Sense::click());
        icons::paint(ui, twisty, if open { "chevron-down" } else { "chevron-right" }, 12.0, t.text_muted);
        if click.on_hover_text(if open { "Collapse" } else { "Expand" }).clicked() {
            cx.flip = Some(path.clone());
        }
    }
    // The page column stays muted, selected or not.
    let label = item.page.and_then(|p| cx.info.pages.get(p)).map(|p| p.label.clone()).unwrap_or_default();
    let label_width = ui.fonts_mut(|f| f.layout_no_wrap(label.clone(), theme::regular(11.0), t.text_muted).size().x);
    ui.painter().text(rect.right_center() - vec2(ROW_PAD, 0.0), Align2::RIGHT_CENTER, &label, theme::regular(11.0), t.text_muted);
    let text_left = left + TWISTY + 4.0;
    let max_width = (rect.right() - ROW_PAD * 2.0 - label_width - text_left).max(0.0);
    crate::widgets::row_text(
        ui,
        egui::pos2(text_left, rect.center().y),
        item.title.trim(),
        theme::regular(12.5),
        look::row_text(t, is_current, t.text),
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
    view.comments.rows = RowLook::Ember;
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

/// Margin's page bookmarks (`reader.bookmark.toggle`), listed above the PDF's own bookmarks.
fn page_bookmarks(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize) {
    let key = look::shortcut(app, "reader.bookmark.toggle");
    let Some(view) = app.views.get(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let info = &doc.info;
    // Unnamed bookmarks show the section they are in.
    let rows: Vec<(usize, String, String)> = view
        .margin
        .bookmarks
        .iter()
        .filter_map(|bookmark| {
            let page = reading::bookmark_page(bookmark).filter(|page| *page < info.pages.len())?;
            let section =
                || current_section(&info.outline, page).and_then(|path| outline_at(&info.outline, &path)).map(|item| item.title.trim().to_owned());
            let title = Some(bookmark.name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .or_else(section)
                .unwrap_or_else(|| format!("Page {}", page + 1));
            Some((page, title, info.pages.get(page).map(|p| p.label.clone()).unwrap_or_default()))
        })
        .collect();
    let current = view.current;
    let marked = rows.iter().any(|(page, ..)| *page == current);
    let (mut toggle, mut go, mut remove) = (false, None, None);
    ui.add_space(GAP_LARGE);
    ui.horizontal(|ui| {
        look::section_label(ui, t, "Page bookmarks", Some(rows.len()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if marked { "Remove this page's bookmark" } else { "Bookmark this page" };
            toggle = look::icon_button(ui, if marked { "bookmark" } else { "bookmark-plus" }, 26.0, marked, label, key.as_deref()).clicked();
        });
    });
    ui.add_space(GAP);
    if rows.is_empty() {
        let hint = match &key {
            Some(key) => format!("Press {key} to bookmark the page you're on."),
            None => "Bookmark the page you're on with the button above.".to_owned(),
        };
        ui.horizontal(|ui| {
            ui.add_space(ROW_PAD);
            ui.label(egui::RichText::new(hint).font(theme::regular(12.0)).color(t.text_muted));
        });
    } else {
        egui::ScrollArea::vertical().id_salt("margin-page-bookmarks").max_height(PAGE_BOOKMARKS_MAX_HEIGHT).auto_shrink([false, true]).show(
            ui,
            |ui| {
                ui.spacing_mut().item_spacing.y = ROW_SPACING;
                for (page, title, label) in &rows {
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, *page == current, title));
                    look::row_fill(ui, t, rect, resp.hovered(), *page == current);
                    let icon = Rect::from_center_size(egui::pos2(rect.left() + ROW_PAD + 7.0, rect.center().y), vec2(14.0, 14.0));
                    icons::paint(ui, icon, "bookmark", 13.0, t.text_muted);
                    // Hovered rows trade the page label for a remove button.
                    let side = Rect::from_center_size(rect.right_center() - vec2(ROW_PAD + ROW_BUTTON / 2.0, 0.0), vec2(ROW_BUTTON, ROW_BUTTON));
                    let label_width = if resp.hovered() || ui.rect_contains_pointer(side) {
                        let x = ui.interact(side, ui.id().with(("remove-page-bookmark", *page)), Sense::click());
                        icons::paint(ui, side, "x", 12.0, if x.hovered() { t.text } else { t.text_muted });
                        if x.on_hover_text("Remove bookmark").clicked() {
                            remove = Some(*page);
                        }
                        ROW_BUTTON
                    } else {
                        ui.painter().text(rect.right_center() - vec2(ROW_PAD, 0.0), Align2::RIGHT_CENTER, label, theme::regular(11.0), t.text_muted);
                        ui.fonts_mut(|f| f.layout_no_wrap(label.clone(), theme::regular(11.0), t.text_muted).size().x)
                    };
                    let text_left = icon.right() + GAP;
                    let max_width = (rect.right() - ROW_PAD * 2.0 - label_width - text_left).max(0.0);
                    let color = look::row_text(t, *page == current, t.text);
                    crate::widgets::row_text(ui, egui::pos2(text_left, rect.center().y), title.as_str(), theme::regular(12.5), color, max_width);
                    if resp.clicked() && remove.is_none() {
                        go = Some(*page);
                    }
                }
            },
        );
    }
    ui.add_space(GAP);
    let Some(view) = app.views.get_mut(index) else { return };
    if toggle {
        reading::toggle_bookmark(view);
    }
    if let Some(page) = remove {
        view.margin.bookmarks.retain(|bookmark| reading::bookmark_page(bookmark) != Some(page));
    }
    if let Some(page) = go {
        view.go_to_page(page);
    }
}
