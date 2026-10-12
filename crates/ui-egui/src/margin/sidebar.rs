//! The left sidebar: Library and palette buttons, the document card (cover, title, author,
//! progress), the current tab's pane, and the tab strip at the bottom.
//!
//! Panes reuse PdfCraft's panels: Bookmarks and PdfCraft's other panels are the right panel
//! (`panels::right_panel`, frameless in Margin); Tools is Margin's list over the tool catalogue (`margin::tools`).
//! Highlights is Margin's card list (`margin::highlights`), Find the search's results
//! (`margin::find`), Contents the outline as a reading table of contents (`margin::contents`).

use egui::{Align2, CornerRadius, Rect, Sense, vec2};

use crate::margin::look::{self, GAP, GAP_LARGE, ROW_PAD, RowLook};
use crate::margin::{SidebarTab, current_section, motion, open_tab, outline_at, reading, window_chrome};
use crate::panels::Nav;
use crate::theme::{self, Tokens};
use crate::{DocView, PdfCraftApp, icons};

const DEFAULT_WIDTH: f32 = 296.0;
const MIN_WIDTH: f32 = 240.0;
const MAX_WIDTH: f32 = 440.0;
const TAB_HEIGHT: f32 = 32.0;
const TOP_BUTTON: f32 = look::CONTROL_ROW;
/// Once floating, the sidebar docks again only this far past `ui.sidebar.overlay_below_px`, so
/// a window edge dragged across the threshold doesn't flip it back and forth.
const DOCK_HYSTERESIS: f32 = 48.0;
pub(super) const ROW_HEIGHT: f32 = 28.0;
/// Rows sit nearly touching, so neighbouring hover fills read as one list.
pub(super) const ROW_SPACING: f32 = 2.0;
const COVER: egui::Vec2 = vec2(52.0, 67.0);
/// The card's stats: at most this many lines, the units joined by [`STATS_SEPARATOR`].
const STATS_LINES: usize = 2;
const STATS_SEPARATOR: &str = " · ";
const META_TEXT: f32 = 11.0;
/// The page bookmark list scrolls past this height, leaving room for the PDF's bookmarks.
const PAGE_BOOKMARKS_MAX_HEIGHT: f32 = 220.0;
const ROW_BUTTON: f32 = look::SMALL_BUTTON;

/// The sidebar, sliding open and closed with `margin_chrome.sidebar_open` over
/// [`motion::SLIDE`]. The page area changes once per open or close, not on every frame of the
/// slide: opening docks the panel at its full width at once and slides the panes in inside it;
/// closing gives the page its room back at once and slides the panes away over it. In a window
/// narrower than `ui.sidebar.overlay_below_px` it floats over the page instead ([`overlay`]),
/// and starts closed ([`super::ReaderChrome::start_narrow`]).
pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let id = egui::Id::new("margin_sidebar");
    let window = ui.ctx().content_rect();
    let threshold = app.margin_keys.settings().ui.sidebar.overlay_below_px as f32;
    let was_narrow = ui.ctx().data(|d| d.get_temp::<bool>(id.with("narrow"))).unwrap_or(false);
    let narrow = window.width() < if was_narrow { threshold + DOCK_HYSTERESIS } else { threshold };
    ui.ctx().data_mut(|d| d.insert_temp(id.with("narrow"), narrow));
    app.margin_chrome.start_narrow(narrow);
    let shown = motion::toggle(ui.ctx(), id.with("open"), app.margin_chrome.sidebar_open, motion::SLIDE);
    if shown <= 0.0 {
        return;
    }
    if narrow {
        overlay(app, ui.ctx(), &t, id, shown, window);
        return;
    }
    if shown < 1.0 {
        let width = egui::PanelState::load(ui.ctx(), id).map_or(DEFAULT_WIDTH, |s| s.size().x).clamp(MIN_WIDTH, MAX_WIDTH);
        if !app.margin_chrome.sidebar_open {
            // Closing: the page already has its room back; the panes slide away over it.
            floating(app, ui.ctx(), &t, id, shown, Rect::from_min_max(window.min, egui::pos2(window.left() + width, window.bottom())), false);
            return;
        }
        // Opening: the panel takes its full width now (one re-fit of the page), and the panes
        // slide in inside it, clipped.
        egui::Panel::left(id.with("sliding"))
            .exact_size(width)
            .resizable(false)
            .show_separator_line(look::has_edges(&t))
            .frame(egui::Frame::NONE.fill(t.panel))
            .show(ui, |ui| {
                let visible = ui.max_rect();
                let rect = Rect::from_min_size(egui::pos2(visible.left() - (1.0 - shown) * width, visible.top()), vec2(width, visible.height()));
                let mut body = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
                body.set_clip_rect(visible.intersect(ui.clip_rect()));
                sections(app, &mut body, &t);
                ui.expand_to_include_rect(visible);
            });
        return;
    }
    // Each panel draws its own divider line (egui's separator, in the theme's divider colour)
    // where the theme has frame edges; the Black palette parts the sidebar by fill alone.
    egui::Panel::left(id)
        .resizable(true)
        .show_separator_line(look::has_edges(&t))
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

/// The sidebar over the page, between the header and the footer, so a narrow window keeps its
/// page width: the same sections and the reader's width (the docked panel's), sliding in from
/// the left edge with the M shadow. A press on the page outside it closes it; while it closes it
/// takes no clicks and no keyboard focus. It docks again once the window is wide enough.
fn overlay(app: &mut PdfCraftApp, ctx: &egui::Context, t: &Tokens, id: egui::Id, shown: f32, window: Rect) {
    let open = app.margin_chrome.sidebar_open;
    let width = egui::PanelState::load(ctx, id).map_or(DEFAULT_WIDTH, |s| s.size().x).clamp(MIN_WIDTH, MAX_WIDTH).min(window.width());
    let (top, bottom) = (window.top() + super::header::HEIGHT, (window.bottom() - super::footer::HEIGHT).max(window.top() + super::header::HEIGHT));
    let visible =
        floating(app, ctx, t, id, shown, Rect::from_min_max(egui::pos2(window.left(), top), egui::pos2(window.left() + width, bottom)), open);
    let pressed_outside =
        ctx.input(|i| i.pointer.primary_pressed() && i.pointer.interact_pos().is_some_and(|p| p.y > top && p.y < bottom && !visible.contains(p)));
    if open && pressed_outside && !egui::Popup::is_any_open(ctx) {
        app.margin_chrome.sidebar_open = false;
    }
}

/// The sidebar's sections on a floating card at `full` (its place when fully shown), slid
/// `1 - shown` of its width off the left edge, with the M shadow; it takes input only while
/// `open`. Returns the part on screen.
fn floating(app: &mut PdfCraftApp, ctx: &egui::Context, t: &Tokens, id: egui::Id, shown: f32, full: Rect, open: bool) -> Rect {
    let rect = full.translate(vec2(-(1.0 - shown) * full.width(), 0.0));
    let visible = Rect::from_min_max(egui::pos2(full.left(), full.top()), rect.right_bottom());
    egui::Area::new(id.with("overlay")).order(egui::Order::Foreground).fixed_pos(visible.min).interactable(open).show(ctx, |ui| {
        let painter = ui.painter();
        painter.add(look::SHADOW_M.as_shape(visible, CornerRadius::ZERO));
        // The overlay's own divider line where the theme has frame edges, none in the Black palette.
        let edge = if look::has_edges(t) { egui::Stroke::new(1.0, t.divider) } else { egui::Stroke::NONE };
        painter.rect(visible, CornerRadius::ZERO, t.panel, edge, egui::StrokeKind::Inside);
        let mut body = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
        body.set_clip_rect(visible);
        if !open {
            body.disable();
            body.set_opacity(ui.opacity());
        }
        sections(app, &mut body, t);
        // The area covers all of it, so a press on its background never reaches the page.
        ui.expand_to_include_rect(visible);
    });
    visible
}

/// The sidebar's three parts: buttons and the document card, the pane, the tab strip.
fn sections(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let edge = GAP_LARGE as i8;
    #[cfg(not(target_arch = "wasm32"))]
    let summary = super::stats::active_summary_parts(app);
    #[cfg(target_arch = "wasm32")]
    let summary = None;
    let body = ui.max_rect();
    window_chrome::drag_area(app, ui, Rect::from_min_size(body.min, vec2(body.width(), super::header::HEIGHT)), ui.id().with("margin-sidebar-drag"));
    // Under the traffic lights the buttons share the header's row: centred on it, after them.
    let traffic_lights = window_chrome::traffic_lights_inset(ui.ctx(), Rect::from_min_max(body.min + vec2(GAP_LARGE, 0.0), body.max));
    let top_margin = if traffic_lights > 0.0 { ((super::header::HEIGHT - TOP_BUTTON) / 2.0) as i8 } else { edge };
    egui::Panel::top("margin_sidebar_top")
        .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: edge, right: edge, top: top_margin, bottom: 0 }))
        .show(ui, |ui| {
            top_buttons(app, ui, traffic_lights);
            // The card's cover is page 1's thumbnail.
            if let Some(view) = app.active.and_then(|i| app.views.get_mut(i)) {
                view.need_thumbnail(0, true);
            }
            document_card(app, ui, t, summary);
        });
    // The tab strip is chrome (its top edge is the panel's separator where the theme has frame
    // edges); the tabs sit the small step in from the panel's edges, as the panes' rows do.
    egui::Panel::bottom("margin_sidebar_tabs")
        .show_separator_line(look::has_edges(t))
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left: GAP as i8, right: GAP as i8, top: GAP as i8, bottom: edge }))
        .show(ui, |ui| tab_strip(app, ui, t));
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: GAP as i8, right: GAP as i8, top: 0, bottom: GAP as i8 }))
        .show(ui, |ui| pane(app, ui, t));
}

fn top_buttons(app: &mut PdfCraftApp, ui: &mut egui::Ui, traffic_lights: f32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.add_space(traffic_lights);
        let library = look::shortcut(app, "library.open");
        if look::icon_button(ui, "book-open", TOP_BUTTON, false, "Library", library.as_deref()).clicked() {
            #[cfg(not(target_arch = "wasm32"))]
            super::library::run(app, "library.open");
            #[cfg(target_arch = "wasm32")]
            {
                app.active = None;
            }
        }
        let palette = look::shortcut(app, "view.palette");
        if look::icon_button(ui, "square-terminal", TOP_BUTTON, app.palette_open, "Command palette", palette.as_deref()).clicked() {
            app.palette_open = true;
        }
    });
}

/// Cover, title, author and how far in the reader is, on a card: the title in full text,
/// everything else muted, the progress bar's fill the only accent. Under that row, across the
/// card, the book's reading stats when it has some (`stats_lines`).
fn document_card(app: &PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, summary: Option<Vec<String>>) {
    let Some((view, doc)) = app.active_ids().and_then(|(i, id)| Some((app.views.get(i)?, app.session.get(id)?))) else { return };
    let info = &doc.info;
    let pages = info.pages.len();
    let radius = CornerRadius::same(t.radius);
    ui.add_space(GAP);
    let meta = |text: &str| egui::RichText::new(text).font(theme::regular(META_TEXT)).color(t.text_muted);
    look::card_frame(t).inner_margin(GAP_LARGE).show(ui, |ui| {
        ui.set_width(ui.available_width());
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
                    ui.label(meta(&format!("{percent}%")));
                    let count = format!("{pages} pp");
                    let count_width = ui.fonts_mut(|f| f.layout_no_wrap(count.clone(), theme::regular(META_TEXT), t.text_muted).size().x);
                    let bar_width = (ui.available_width() - count_width - GAP).max(GAP);
                    let (bar, _) = ui.allocate_exact_size(vec2(bar_width, 3.0), Sense::hover());
                    ui.painter().rect_filled(bar, CornerRadius::same(2), look::Surfaces::of(t).track);
                    let share = motion::follow(ui.ctx(), egui::Id::new(("margin-card-progress", view.id)), percent as f32 / 100.0, motion::SLIDE);
                    let fill = Rect::from_min_size(bar.min, vec2(bar.width() * share, bar.height()));
                    ui.painter().rect_filled(fill, CornerRadius::same(2), t.accent);
                    ui.label(meta(&count));
                });
            });
        });
        if let Some(parts) = summary.filter(|parts| !parts.is_empty()) {
            ui.add_space(GAP);
            let width = ui.available_width();
            let measure = |text: &str| ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), theme::regular(META_TEXT), t.text_muted).size().x);
            let lines = stats_lines(&parts, width, measure);
            let shown = ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for line in &lines {
                    ui.add(egui::Label::new(meta(line)).truncate());
                }
            });
            // The whole summary when a unit had no room.
            if lines.iter().map(|line| line.split(STATS_SEPARATOR).count()).sum::<usize>() < parts.len() {
                look::tooltip(shown.response, &parts.join(STATS_SEPARATOR), None);
            }
        }
    });
    ui.add_space(GAP_LARGE);
}

/// Pack the stats' units into at most [`STATS_LINES`] lines `width` wide, in order, a line
/// breaking only between units: never inside one or next to a lone separator. Units that don't
/// fit are left out (the tooltip has them).
fn stats_lines(parts: &[String], width: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for part in parts {
        let joined = lines.last().map(|line| format!("{line}{STATS_SEPARATOR}{part}"));
        match joined {
            Some(joined) if measure(&joined) <= width => {
                if let Some(line) = lines.last_mut() {
                    *line = joined;
                }
            }
            _ if lines.len() < STATS_LINES => lines.push(part.clone()),
            _ => break,
        }
    }
    lines
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
            look::row_fill(ui, t, rect, &resp, on);
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
pub(super) fn pane_title(ui: &mut egui::Ui, t: &Tokens, title: &str, count: Option<usize>) {
    ui.add_space(GAP_LARGE);
    ui.horizontal(|ui| look::section_label(ui, t, title, count));
    ui.add_space(GAP);
}

/// The mockup's empty state: a muted message, centred.
pub(super) fn empty(ui: &mut egui::Ui, t: &Tokens, lines: &[&str]) {
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
        SidebarTab::Contents => super::contents::show(app, ui, t, index),
        SidebarTab::Highlights => highlights(app, ui, t, index),
        SidebarTab::Tools => super::tools::show(app, ui, t),
        // The search's status and results; its field is in the header (`margin::find`).
        SidebarTab::Find => super::find::results(app, ui, t, index),
        SidebarTab::Bookmarks => {
            // Margin's page bookmarks first, then the PDF's bookmarks (its outline, editable).
            page_bookmarks(app, ui, t, index);
            crate::panels::right_panel(app, ui);
        }
        // The thumbnail grid (`margin::pages`); PdfCraft's Pages panel stays PdfCraft's.
        SidebarTab::Pages => super::pages::show(app, ui, t, index),
        SidebarTab::Other(_) => {
            // PdfCraft's panel, its title row a section label here (`look::RowLook::Ember`), the
            // Comments panel's cards too.
            if let Some(view) = app.views.get_mut(index) {
                view.comments.rows = RowLook::Ember;
            }
            ui.add_space(GAP_LARGE);
            crate::panels::right_panel(app, ui);
        }
    }
}

/// Highlights, underlines, strikethroughs and squiggles, and page notes, as cards
/// (`margin::highlights`). With none yet the pane keeps the same layout (its search, the notes
/// box) and says how to make one inside the list, so the first highlight never rearranges it.
fn highlights(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize) {
    let count = super::highlights::count(app, index);
    pane_title(ui, t, "Highlights", Some(count));
    let note_hint = look::shortcut(app, super::marks::NOTE_ADD_ID).map(|keys| format!("{keys} adds a note to the page."));
    let mut empty_lines = vec!["No highlights yet.", "Pick Highlight in the rail, then select text."];
    empty_lines.extend(note_hint.as_deref());
    let nav = super::highlights::show(app, ui, t, index, &empty_lines);
    navigate(app.views.get_mut(index), nav);
}

fn navigate(view: Option<&mut DocView>, nav: Option<Nav>) {
    let Some(view) = view else { return };
    match nav {
        // Highlight cards name pages, not destinations; a destination's page is where to go.
        Some(Nav::Page(p) | Nav::Dest(p, _)) => view.go_to_page(p),
        Some(Nav::Flash(p, r)) => {
            view.go_to_page(p);
            view.flash = Some((p, r, 0.0));
        }
        None => {}
    }
}

/// A page bookmark's row: its id, page, shown title, page label and its own name.
struct BookmarkRow {
    id: String,
    page: usize,
    title: String,
    label: String,
    name: String,
}

/// What the reader did to the page bookmarks this frame.
#[derive(Default)]
struct BookmarkActions {
    toggle: bool,
    go: Option<String>,
    remove: Option<String>,
    /// Rename this bookmark to this name.
    rename: Option<(String, String)>,
}

/// Margin's page bookmarks (`reader.bookmark.toggle`), listed above the PDF's own bookmarks. A
/// click goes to the exact spot the bookmark was added at; a double-click or the menu's Rename
/// names it in place (Enter keeps the name, Esc puts the old one back).
fn page_bookmarks(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize) {
    let key = look::shortcut(app, "reader.bookmark.toggle");
    let Some(view) = app.views.get(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let scroll_id = ("margin-page-bookmarks", view.id.0);
    let renaming_id = egui::Id::new(("margin-bookmark-rename", view.id.0));
    let info = &doc.info;
    // Unnamed bookmarks show the section they are in.
    let rows: Vec<BookmarkRow> = view
        .margin
        .bookmarks
        .iter()
        .filter_map(|bookmark| {
            let page = reading::bookmark_page(bookmark).filter(|page| *page < info.pages.len())?;
            let section =
                || current_section(&info.outline, page).and_then(|path| outline_at(&info.outline, &path)).map(|item| item.title.trim().to_owned());
            let name = bookmark.name.trim().to_owned();
            let title = Some(name.clone()).filter(|name| !name.is_empty()).or_else(section).unwrap_or_else(|| format!("Page {}", page + 1));
            let label = info.pages.get(page).map(|p| p.label.clone()).unwrap_or_default();
            Some(BookmarkRow { id: bookmark.id.clone(), page, title, label, name })
        })
        .collect();
    let current = view.current;
    let marked = rows.iter().any(|row| row.page == current);
    let mut actions = BookmarkActions::default();
    ui.add_space(GAP_LARGE);
    ui.horizontal(|ui| {
        look::section_label(ui, t, "Page bookmarks", Some(rows.len()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if marked { "Remove this page's bookmark" } else { "Bookmark this page" };
            actions.toggle = look::icon_button(ui, if marked { "bookmark" } else { "bookmark-plus" }, 26.0, marked, label, key.as_deref()).clicked();
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
        let mut renaming: Option<(String, String)> = ui.data(|d| d.get_temp(renaming_id));
        egui::ScrollArea::vertical().id_salt(scroll_id).max_height(PAGE_BOOKMARKS_MAX_HEIGHT).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = ROW_SPACING;
            for row in &rows {
                let typed = match renaming.as_mut() {
                    Some((id, text)) if *id == row.id => {
                        Some(rename_row(ui, t, row, text).map(|keep| if keep { text.clone() } else { row.name.clone() }))
                    }
                    _ => None,
                };
                match typed {
                    Some(Some(name)) => actions.rename = Some((row.id.clone(), name)),
                    Some(None) => {}
                    None => bookmark_row(ui, t, row, current, &mut actions, &mut renaming),
                }
            }
        });
        if actions.rename.is_some() {
            renaming = None;
        }
        ui.data_mut(|d| match renaming {
            Some(state) => {
                d.insert_temp(renaming_id, state);
            }
            None => d.remove::<(String, String)>(renaming_id),
        });
    }
    ui.add_space(GAP);
    let Some(view) = app.views.get_mut(index) else { return };
    let page = view.current + 1;
    if actions.toggle && reading::toggle_bookmark(view) == reading::BookmarkToggle::Full {
        app.notify(reading::BookmarkToggle::Full.message(page));
        return;
    }
    if let Some(id) = actions.remove {
        reading::remove_bookmark(view, &id);
    }
    if let Some((id, name)) = actions.rename {
        reading::rename_bookmark(view, &id, &name);
    }
    if let Some(id) = actions.go {
        reading::go_to_bookmark(view, &id);
    }
}

/// One page bookmark's row: hovered, it trades the page label for a remove button; its menu
/// renames or removes it.
fn bookmark_row(
    ui: &mut egui::Ui,
    t: &Tokens,
    row: &BookmarkRow,
    current: usize,
    actions: &mut BookmarkActions,
    renaming: &mut Option<(String, String)>,
) {
    let here = row.page == current;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, here, &row.title));
    look::row_fill(ui, t, rect, &resp, here);
    let icon = Rect::from_center_size(egui::pos2(rect.left() + ROW_PAD + 7.0, rect.center().y), vec2(14.0, 14.0));
    icons::paint(ui, icon, "bookmark", 13.0, t.text_muted);
    let side = Rect::from_center_size(rect.right_center() - vec2(ROW_PAD + ROW_BUTTON / 2.0, 0.0), vec2(ROW_BUTTON, ROW_BUTTON));
    let mut removing = false;
    let label_width = if resp.hovered() || ui.rect_contains_pointer(side) {
        let x = ui.interact(side, ui.id().with(("remove-page-bookmark", &row.id)), Sense::click());
        icons::paint(ui, side, "x", 12.0, look::icon_tint(t, false, x.hovered()));
        if look::tooltip(x, "Remove bookmark", None).clicked() {
            removing = true;
        }
        ROW_BUTTON
    } else {
        ui.painter().text(rect.right_center() - vec2(ROW_PAD, 0.0), Align2::RIGHT_CENTER, &row.label, theme::regular(11.0), t.text_muted);
        ui.fonts_mut(|f| f.layout_no_wrap(row.label.clone(), theme::regular(11.0), t.text_muted).size().x)
    };
    let text_left = icon.right() + GAP;
    let max_width = (rect.right() - ROW_PAD * 2.0 - label_width - text_left).max(0.0);
    let color = look::row_text(t, here, t.text);
    crate::widgets::row_text(ui, egui::pos2(text_left, rect.center().y), row.title.as_str(), theme::regular(12.5), color, max_width);
    resp.context_menu(|ui| {
        if ui.button("Rename").clicked() {
            *renaming = Some((row.id.clone(), row.name.clone()));
            ui.close();
        }
        if ui.button("Remove bookmark").clicked() {
            removing = true;
            ui.close();
        }
    });
    if resp.double_clicked() {
        *renaming = Some((row.id.clone(), row.name.clone()));
    } else if removing {
        actions.remove = Some(row.id.clone());
    } else if resp.clicked() {
        actions.go = Some(row.id.clone());
    }
}

/// A bookmark being renamed: a field in its row, focused as it appears. `Some(true)` keeps the
/// typed name (Enter, or a click elsewhere), `Some(false)` puts the old one back (Esc).
fn rename_row(ui: &mut egui::Ui, t: &Tokens, row: &BookmarkRow, text: &mut String) -> Option<bool> {
    let id = ui.make_persistent_id(("margin-bookmark-name", &row.id));
    let appearing = ui.ctx().read_response(id).is_none();
    let resp = look::input_frame(t, true)
        .inner_margin(egui::Margin::symmetric(ROW_PAD as i8, 4))
        .show(ui, |ui| {
            let field = egui::TextEdit::singleline(text)
                .id(id)
                .frame(egui::Frame::NONE)
                .hint_text(row.title.as_str())
                .font(theme::regular(12.5))
                .desired_width(ui.available_width());
            ui.add(field)
        })
        .inner;
    if appearing {
        resp.request_focus();
    }
    if resp.lost_focus() {
        return Some(!ui.input(|i| i.key_pressed(egui::Key::Escape)));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::stats_lines;

    /// Six points a character, about Margin's 11-point UI font.
    fn measure(text: &str) -> f32 {
        text.chars().count() as f32 * 6.0
    }

    fn parts() -> Vec<String> {
        ["2h 14m read", "37 pages", "1.8 min/page", "~34 min left"].map(str::to_owned).to_vec()
    }

    #[test]
    fn stats_wrap_between_whole_units_on_at_most_two_lines() {
        // The card's text width at the sidebar's widest, default and narrowest settings.
        assert_eq!(stats_lines(&parts(), 416.0, measure), ["2h 14m read · 37 pages · 1.8 min/page · ~34 min left"]);
        assert_eq!(stats_lines(&parts(), 272.0, measure), ["2h 14m read · 37 pages · 1.8 min/page", "~34 min left"]);
        assert_eq!(stats_lines(&parts(), 170.0, measure), ["2h 14m read · 37 pages", "1.8 min/page · ~34 min left"]);
        // Too narrow for all: the units that fit, never a split unit or a lone separator.
        assert_eq!(stats_lines(&parts(), 80.0, measure), ["2h 14m read", "37 pages"]);
        assert!(stats_lines(&[], 272.0, measure).is_empty());
    }
}
