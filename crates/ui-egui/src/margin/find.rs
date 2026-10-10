//! Margin's Find: one search session (PdfCraft's `DocView::find`, its background text layers and
//! its match painting on the page) shown as one field in the header's centre, in place of the
//! title, and its results in the sidebar's Find tab. When the header is too narrow for the field,
//! it gets a row of its own under the header, never a box over the page.
//!
//! `/`, Ctrl+F (`edit.find`), the header's search button and the Find tab all open the same
//! session; the Find tab shows while it is open and the tab shown before comes back when it
//! closes ([`sync`]). The field has the `find-input` id, so the keymap's `Find > TextInput`
//! context gives Enter / Shift+Enter (and F3 / Shift+F3 everywhere) the next and previous match;
//! the field never steps itself, so one press moves once. Results are grouped by page label,
//! with the section and a snippet with the match marked; the list draws only the rows in view,
//! so every match is reachable however many there are.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align, Color32, CornerRadius, Id, Layout, Rect, Sense, Stroke, UiBuilder, pos2, vec2};
use pdfcraft_render::DocInfo;

use crate::canvas::Find;
use crate::margin::look::{self, GAP, GAP_LARGE, ROW_PAD};
use crate::margin::{SidebarTab, current_section, outline_at};
use crate::theme::{self, Tokens};
use crate::{DocView, PdfCraftApp, icons};

/// The field's id (the keymap's `Find` context looks for it).
pub(crate) const INPUT_ID: &str = "find-input";
/// The field's widest and narrowest width in the header; narrower than the minimum, it moves to
/// its own row.
const FIELD_MAX_WIDTH: f32 = 460.0;
const FIELD_MIN_WIDTH: f32 = 320.0;
const FIELD_HEIGHT: f32 = 30.0;
/// The row under the header that holds the field in a narrow window.
const ROW_HEIGHT: f32 = 44.0;
/// The field's small buttons (Aa, whole words, previous, next, close) and text.
const FIELD_BUTTON: f32 = 24.0;
const FIELD_BUTTON_COUNT: f32 = 5.0;
const FIELD_TEXT: f32 = 13.0;
const COUNT_TEXT: f32 = 11.5;
const SEARCH_ICON: f32 = 14.0;
/// Result rows: a page group's header and one match, each including the space under it.
const GROUP_ROW: f32 = 28.0;
const HIT_ROW: f32 = 48.0;
const ROW_SPACING: f32 = 2.0;
const GROUP_TEXT: f32 = 11.0;
const SNIPPET_TEXT: f32 = 12.5;
const SNIPPET_LINES: usize = 2;
/// Characters of context shown before and after a match.
const CONTEXT_BEFORE: usize = 40;
const CONTEXT_AFTER: usize = 80;
const STATUS_TEXT: f32 = 12.0;

/// The keys of the field's buttons, from Margin's keymap.
pub(crate) struct FindKeys {
    next: Option<String>,
    prev: Option<String>,
}

impl FindKeys {
    pub(crate) fn of(app: &PdfCraftApp) -> Self {
        Self { next: look::shortcut(app, "reader.find.next"), prev: look::shortcut(app, "reader.find.prev") }
    }
}

/// Open the active document's search from the Find tab, remembering the tab and sidebar state
/// to go back to.
pub(crate) fn open(app: &mut PdfCraftApp) {
    let chrome = &mut app.margin_chrome;
    if chrome.before_find.is_none() {
        chrome.before_find = Some((shown_before(chrome.tab), chrome.sidebar_open));
    }
    chrome.tab = SidebarTab::Find;
    chrome.sidebar_open = true;
    if let Some(view) = app.active.and_then(|i| app.views.get_mut(i)) {
        view.open_find();
    }
}

/// The tab to come back to: Find itself never (it would show no search).
fn shown_before(tab: SidebarTab) -> SidebarTab {
    if tab == SidebarTab::Find { SidebarTab::Contents } else { tab }
}

/// Once per frame, after `margin::sync`: run the search again when its query or options changed
/// and keep its first pick on the reading page ([`AutoPick`]); show the Find tab when a search
/// opened (from any entry point) and the earlier tab when it closed while Find still shows. True
/// when the tab changed.
pub(crate) fn sync(app: &mut PdfCraftApp) -> bool {
    let Some(view) = app.active.and_then(|i| app.views.get_mut(i)) else { return false };
    // A new query, or a search with no match picked yet (opened with its query, as
    // `--find` does): start picking its first match afresh. Typing on keeps the page it started
    // from.
    let changed = view.find.as_ref().is_some_and(|f| f.query != f.case_query);
    let unpicked = view.find.as_ref().is_some_and(|f| f.current.is_none()) && view.margin.find_pick.is_none();
    if changed || unpicked {
        let from = view.margin.find_pick.map_or(view.current, |auto| auto.from);
        view.margin.find_pick = Some(AutoPick { from, picked: None });
    }
    if changed {
        view.rerun_find();
    }
    let pages = app.session.get(view.id).map_or(0, |doc| doc.info.pages.len());
    keep_first_pick(view, pages);
    let finding = view.find.is_some();
    let chrome = &mut app.margin_chrome;
    match (finding, chrome.before_find) {
        (true, None) => {
            chrome.before_find = Some((shown_before(chrome.tab), chrome.sidebar_open));
            chrome.tab = SidebarTab::Find;
            chrome.sidebar_open = true;
            true
        }
        (false, Some((tab, open))) => {
            chrome.before_find = None;
            // A tab picked while finding stays.
            let back = chrome.tab == SidebarTab::Find;
            if back {
                chrome.tab = tab;
                chrome.sidebar_open = open;
            }
            back
        }
        _ => false,
    }
}

/// The match a new search goes to by itself: the first one at or after the page being read when
/// the query changed (`from`), found as page texts arrive in any order. Ends once the reader
/// moves to another match.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AutoPick {
    from: usize,
    /// The match it went to last, by page and first glyph (indices shift as pages arrive).
    picked: Option<(usize, usize)>,
}

/// The current match by page and first glyph.
fn current_match(find: &Find) -> Option<(usize, usize)> {
    find.current.and_then(|i| find.matches.get(i)).map(|(page, range)| (*page, range.start))
}

/// PdfCraft's search goes to the first match on whichever page's text arrives first; a page
/// nearer the reading place can arrive later. Until the reader picks a match, go to the first
/// one at or after the reading page (the first match of all once every page is searched and none
/// follows), before the page is drawn.
fn keep_first_pick(view: &mut DocView, pages: usize) {
    let Some(mut auto) = view.margin.find_pick else { return };
    let searched = view.texts.len() + view.text_failed.len() >= pages;
    let Some(find) = view.find.as_ref() else {
        view.margin.find_pick = None;
        return;
    };
    if auto.picked.is_some() && current_match(find) != auto.picked {
        view.margin.find_pick = None;
        return;
    }
    let wanted = find.matches.iter().position(|(page, _)| *page >= auto.from).or_else(|| (searched && !find.matches.is_empty()).then_some(0));
    match wanted {
        Some(i) if find.current != Some(i) => view.go_to_match(i),
        _ => {}
    }
    auto.picked = view.find.as_ref().and_then(current_match);
    view.margin.find_pick = Some(auto);
}

/// What the field asked for this frame.
#[derive(Default)]
struct FieldOutcome {
    close: bool,
    step: Option<bool>,
}

/// The header's centre while finding: the field, `room` wide at most, centred on `center`.
/// False when there is no room for it (the caller shows [`row`] instead).
pub(crate) fn in_header(app: &mut PdfCraftApp, ui: &mut egui::Ui, center: egui::Pos2, room: f32) -> bool {
    if room < FIELD_MIN_WIDTH {
        return false;
    }
    let rect = Rect::from_center_size(center, vec2(room.min(FIELD_MAX_WIDTH), FIELD_HEIGHT));
    show_field(app, ui, rect);
    true
}

/// The field on a row of its own under the header (a narrow window).
pub(crate) fn row(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::top("margin_find_row")
        .exact_size(ROW_HEIGHT)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(GAP_LARGE as i8, 0)))
        .show(ui, |ui| {
            let full = ui.max_rect();
            let rect = Rect::from_center_size(full.center(), vec2(full.width().min(FIELD_MAX_WIDTH), FIELD_HEIGHT));
            show_field(app, ui, rect);
        });
}

fn show_field(app: &mut PdfCraftApp, ui: &mut egui::Ui, rect: Rect) {
    let keys = FindKeys::of(app);
    let Some(view) = app.active.and_then(|i| app.views.get_mut(i)) else { return };
    let Some(pages) = app.session.get(view.id).map(|doc| doc.info.pages.len()) else { return };
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
    let out = field(&mut child, view, pages, &keys);
    if out.close {
        view.find = None;
    } else if let Some(forward) = out.step {
        view.find_step(forward);
    }
}

/// The mockup's `.findbox`: search icon, the query, `n of N`, Aa and whole words, previous, next
/// and close.
fn field(ui: &mut egui::Ui, view: &mut DocView, pages: usize, keys: &FindKeys) -> FieldOutcome {
    let t = Tokens::get(ui.ctx());
    let searched = view.texts.len() + view.text_failed.len();
    let mut out = FieldOutcome::default();
    let Some(find) = view.find.as_mut() else { return out };
    let rect = ui.max_rect();
    ui.painter().rect(rect, CornerRadius::same(t.radius), t.card, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    ui.spacing_mut().item_spacing.x = GAP / 2.0;
    ui.add_space(GAP);
    ui.add(icons::image("search", SEARCH_ICON, t.text_muted));
    let count = count_label(find, searched, pages);
    let count_galley = ui.painter().layout_no_wrap(count.clone(), theme::regular(COUNT_TEXT), t.text_muted);
    // The buttons, each after a gap, and the gap before the count.
    let spacing = ui.spacing().item_spacing.x;
    let buttons = FIELD_BUTTON_COUNT * (FIELD_BUTTON + spacing) + spacing;
    let query_width = (ui.available_width() - count_galley.size().x - buttons - GAP).max(GAP);
    let id = Id::new(INPUT_ID);
    let edit = egui::TextEdit::singleline(&mut find.query)
        .id(id)
        .hint_text("Find in this document")
        .font(theme::regular(FIELD_TEXT))
        .frame(egui::Frame::NONE)
        .desired_width(query_width);
    let resp = ui.add(edit);
    if std::mem::take(&mut find.focus) {
        resp.request_focus();
        select_all(ui.ctx(), id, find.query.chars().count());
    }
    if (resp.has_focus() || resp.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        out.close = true;
    }
    ui.add(egui::Label::new(egui::RichText::new(count).font(theme::regular(COUNT_TEXT)).color(t.text_muted)).selectable(false));
    if option_toggle(ui, &t, "Aa", false, find.case_sensitive, "Match case").clicked() {
        find.case_sensitive = !find.case_sensitive;
        find.case_query.clear();
    }
    if option_toggle(ui, &t, "ab", true, find.whole_words, "Whole words").clicked() {
        find.whole_words = !find.whole_words;
        find.case_query.clear();
    }
    let has_matches = !find.matches.is_empty();
    if ui
        .add_enabled_ui(has_matches, |ui| look::icon_button(ui, "chevron-up", FIELD_BUTTON, false, "Previous match", keys.prev.as_deref()))
        .inner
        .clicked()
    {
        out.step = Some(false);
    }
    if ui
        .add_enabled_ui(has_matches, |ui| look::icon_button(ui, "chevron-down", FIELD_BUTTON, false, "Next match", keys.next.as_deref()))
        .inner
        .clicked()
    {
        out.step = Some(true);
    }
    if look::icon_button(ui, "x", FIELD_BUTTON, false, "Close find", Some("Esc")).clicked() {
        out.close = true;
    }
    out
}

/// Select the field's whole text, so typing replaces the last search.
fn select_all(ctx: &egui::Context, id: Id, chars: usize) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    let range = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(chars));
    state.cursor.set_char_range(Some(range));
    state.store(ctx, id);
}

/// A find option as a small text toggle ("Aa", an underlined "ab"): the pill and the accent when
/// on, bright and the pill when hovered.
fn option_toggle(ui: &mut egui::Ui, t: &Tokens, text: &str, underline: bool, on: bool, label: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(FIELD_BUTTON), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), on, label));
    let shown = crate::margin::motion::hover(ui.ctx(), resp.id, resp.hovered() || on);
    ui.painter().rect_filled(rect, CornerRadius::same(t.radius), t.hover.gamma_multiply(shown));
    let color = if on {
        t.accent
    } else if resp.hovered() {
        t.text
    } else {
        t.text_muted
    };
    let underline = if underline { Stroke::new(1.0, color) } else { Stroke::NONE };
    let job = LayoutJob::single_section(text.to_owned(), TextFormat { font_id: theme::medium(FIELD_TEXT), color, underline, ..Default::default() });
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    look::tooltip(resp, label, None)
}

/// `n of N`: the current match of all found. `N+` while pages are still being searched (not a
/// final count); "Searching…" before the first match; "No matches" once every page is searched.
fn count_label(find: &Find, searched: usize, pages: usize) -> String {
    if find.case_query.trim().is_empty() {
        return String::new();
    }
    let done = searched >= pages;
    match (find.matches.len(), done) {
        (0, false) => "Searching…".to_owned(),
        (0, true) => "No matches".to_owned(),
        (n, done) => {
            let current = find.current.map_or(0, |c| c.saturating_add(1));
            format!("{current} of {n}{}", if done { "" } else { "+" })
        }
    }
}

/// One row of the results list.
#[derive(Clone, Copy)]
enum ResultRow {
    /// The first match on this page follows.
    Page(usize),
    /// The match at this index of `Find::matches`.
    Hit(usize),
}

impl ResultRow {
    fn height(self) -> f32 {
        match self {
            Self::Page(_) => GROUP_ROW,
            Self::Hit(_) => HIT_ROW,
        }
    }
}

/// The Find tab: the search's status, then its matches grouped by page. Clicking one goes to it.
pub(crate) fn results(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, index: usize) {
    let Some(view) = app.views.get_mut(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let info = &doc.info;
    ui.add_space(GAP_LARGE);
    ui.horizontal(|ui| look::section_label(ui, t, "Find", None));
    ui.add_space(GAP);
    let Some(find) = view.find.as_ref() else {
        status(ui, t, &["Press / or Ctrl+F to find text in this document."]);
        return;
    };
    let pages = info.pages.len();
    let failed = view.text_failed.len();
    let searched = view.texts.len() + failed;
    let n = find.matches.len();
    let query = find.case_query.trim();
    let mut lines = Vec::new();
    if query.is_empty() {
        lines.push("Type in the find field to search this document.".to_owned());
        lines.push("Enter goes to the next match, Shift+Enter back.".to_owned());
    } else if searched < pages {
        lines.push(format!("Searching… {searched} of {pages} pages, {n} {} so far", plural(n, "match", "matches")));
    } else if n == 0 {
        lines.push(format!("No matches for “{query}”."));
    } else {
        let on_pages = page_count(&find.matches);
        lines.push(format!("{n} {} on {on_pages} {}", plural(n, "match", "matches"), plural(on_pages, "page", "pages")));
    }
    if failed > 0 && !query.is_empty() {
        lines.push(format!("{failed} {} no text to search (scanned pages need text recognition).", plural(failed, "page has", "pages have")));
    }
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    status(ui, t, &lines);
    if n == 0 {
        return;
    }
    let go = list(ui, t, view, info);
    if let Some(i) = go {
        view.go_to_match(i);
    }
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// How many pages the matches are on (they are in page order).
fn page_count(matches: &[(usize, std::ops::Range<usize>)]) -> usize {
    let mut pages = 0;
    let mut last = None;
    for (page, _) in matches {
        if last != Some(*page) {
            pages += 1;
            last = Some(*page);
        }
    }
    pages
}

/// Muted status lines, inset like row text.
fn status(ui: &mut egui::Ui, t: &Tokens, lines: &[&str]) {
    for line in lines {
        ui.horizontal(|ui| {
            ui.add_space(ROW_PAD);
            ui.add(egui::Label::new(egui::RichText::new(*line).font(theme::regular(STATUS_TEXT)).color(t.text_muted)).wrap());
        });
    }
    ui.add_space(GAP);
}

/// The matches, drawn only where the list is in view. Scrolls the current match into view when it
/// changes (a step or a click). Returns the clicked match.
fn list(ui: &mut egui::Ui, t: &Tokens, view: &DocView, info: &DocInfo) -> Option<usize> {
    let find = view.find.as_ref()?;
    let mut rows = Vec::with_capacity(find.matches.len().saturating_add(info.pages.len().min(find.matches.len())));
    let mut last_page = None;
    for (i, (page, _)) in find.matches.iter().enumerate() {
        if last_page != Some(*page) {
            rows.push(ResultRow::Page(*page));
            last_page = Some(*page);
        }
        rows.push(ResultRow::Hit(i));
    }
    let total: f32 = rows.iter().map(|r| r.height()).sum();
    // Scroll to the current match once each time it changes.
    let seen_id = Id::new(("margin-find-seen", view.id.0));
    let seen: Option<Option<usize>> = ui.data(|d| d.get_temp(seen_id));
    let reveal = seen != Some(find.current);
    ui.data_mut(|d| d.insert_temp(seen_id, find.current));
    let mut go = None;
    egui::ScrollArea::vertical().id_salt(("margin-find-results", view.id.0)).auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
        ui.set_min_height(total);
        let origin = ui.max_rect().min;
        let width = ui.available_width();
        let mut y = 0.0;
        for row in &rows {
            let height = row.height();
            let rect = Rect::from_min_size(pos2(origin.x, origin.y + y), vec2(width, height - ROW_SPACING));
            let shown = y + height >= viewport.min.y && y <= viewport.max.y;
            match *row {
                ResultRow::Page(page) if shown => group_row(ui, t, info, page, rect),
                ResultRow::Hit(i) => {
                    let current = find.current == Some(i);
                    if current && reveal {
                        ui.scroll_to_rect(rect, Some(Align::Center));
                    }
                    if shown && hit_row(ui, t, view, info, i, current, rect) {
                        go = Some(i);
                    }
                }
                ResultRow::Page(_) => {}
            }
            y += height;
        }
    });
    go
}

/// A page group's header: `p. <label> · <section>`, muted.
fn group_row(ui: &egui::Ui, t: &Tokens, info: &DocInfo, page: usize, rect: Rect) {
    let label = info.pages.get(page).map_or_else(|| page.saturating_add(1).to_string(), |p| p.label.clone());
    let section = current_section(&info.outline, page).and_then(|path| outline_at(&info.outline, &path)).map(|item| item.title.trim().to_owned());
    let mut job = LayoutJob::default();
    let format = |color| TextFormat { font_id: theme::medium(GROUP_TEXT), color, ..Default::default() };
    job.append(&format!("p. {label}"), 0.0, format(t.text_muted));
    if let Some(section) = section.filter(|s| !s.is_empty()) {
        job.append(&format!("  ·  {section}"), 0.0, format(t.text_faint));
    }
    job.wrap = TextWrapping::truncate_at_width((rect.width() - 2.0 * ROW_PAD).max(0.0));
    let galley = ui.painter().layout_job(job);
    let pos = pos2(rect.left() + ROW_PAD, rect.bottom() - galley.size().y - GAP / 2.0);
    ui.painter().galley(pos, galley, t.text_muted);
}

/// One match: a snippet of its line with the match marked (Ember's text-highlight colour). True
/// when clicked.
fn hit_row(ui: &mut egui::Ui, t: &Tokens, view: &DocView, info: &DocInfo, i: usize, current: bool, rect: Rect) -> bool {
    let Some((page, range)) = view.find.as_ref().and_then(|f| f.matches.get(i)) else { return false };
    let resp = ui.interact(rect, ui.id().with(("margin-find-hit", i)), Sense::click());
    look::row_fill(ui, t, rect, &resp, current);
    let snippet = view.texts.get(page).map(|text| {
        let n = text.glyphs.len();
        let (start, end) = (range.start.min(n), range.end.min(n));
        let flat = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let before = flat(text.text_of(start.saturating_sub(CONTEXT_BEFORE)..start));
        let after = flat(text.text_of(end..end.saturating_add(CONTEXT_AFTER).min(n)));
        (before, flat(text.text_of(start..end)), after)
    });
    let Some((before, hit, after)) = snippet else { return resp.clicked() };
    let label = info.pages.get(*page).map_or_else(|| page.saturating_add(1).to_string(), |p| p.label.clone());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, current, format!("p. {label}: {before} {hit} {after}")));
    let context = if current { t.text } else { t.text_muted };
    let plain = TextFormat { font_id: theme::regular(SNIPPET_TEXT), color: context, ..Default::default() };
    let marked = TextFormat {
        font_id: theme::medium(SNIPPET_TEXT),
        color: if current { t.accent } else { t.text },
        background: t.accent_soft,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    if !before.is_empty() {
        job.append(&format!("…{before} "), 0.0, plain.clone());
    }
    job.append(&hit, 0.0, marked);
    if !after.is_empty() {
        job.append(&format!(" {after}…"), 0.0, plain);
    }
    let inner = rect.shrink2(vec2(ROW_PAD, GAP / 2.0));
    job.wrap = TextWrapping { max_width: inner.width(), max_rows: SNIPPET_LINES, break_anywhere: false, overflow_character: Some('…') };
    let galley = ui.painter().layout_job(job);
    let pos = pos2(inner.left(), inner.center().y - galley.size().y / 2.0);
    ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())).galley(pos, galley, Color32::PLACEHOLDER);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}
