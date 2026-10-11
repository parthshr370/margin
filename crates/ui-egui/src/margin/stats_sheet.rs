//! The Stats sheet (`app.stats`, Ctrl+Shift+T), plan/R10-SURFACES.md §5: a wide modal with a
//! fixed header (title, Export CSV, Close) over one scrolling body: four metric cards, the last
//! 12 months as a calendar heatmap with a day card, this week's bars and the streaks, and the
//! period's books as a sortable table. One `StatsDb::overview` read when the sheet opens (or on
//! Retry) feeds all of it; painting never queries the database. No history yet, recording
//! turned off and an unreadable database each have their own state.

use std::cmp::Ordering;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Align2, CornerRadius, FontId, Galley, Layout, Rect, RichText, Sense, Stroke, UiBuilder, Vec2, pos2, vec2};
use margin_config::library::Book;
use margin_config::stats::{BookTotal, DayStats, HEAT_LEVELS, Overview, Streaks, civil, day_number, heat_level, weekday};

use super::look::{self, Button, ButtonSize, Surfaces};
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

/// The wide sheet's largest size and its distance from the window's edges; below
/// [`FULL_WINDOW_BELOW`] points wide it fills the window instead.
const SHEET_MAX: Vec2 = Vec2::new(1120.0, 864.0);
const SHEET_INSET: f32 = 48.0;
const FULL_WINDOW_BELOW: f32 = 768.0;
const HEADER_HEIGHT: f32 = 64.0;
/// The header's side padding, the body's padding and the gap between sections.
const PAD: f32 = 24.0;
const SECTION_GAP: f32 = 24.0;
/// Padding inside a card, chart well or the day card, and the gap between related things in it.
const INNER_PAD: f32 = 16.0;
const INNER_GAP: f32 = 8.0;
const TITLE_SIZE: f32 = 24.0;
const SECTION_TITLE_SIZE: f32 = 15.0;
const BODY_TEXT: f32 = 13.0;
const SMALL_TEXT: f32 = 12.0;
const LABEL_TEXT: f32 = 11.0;

const CARD_GAP: f32 = 16.0;
const CARD_HEIGHT: f32 = 120.0;
const CARD_ICON: f32 = 16.0;
const CARD_VALUE: f32 = 32.0;
const TREND_ICON: f32 = 12.0;
const TREND_GAP: f32 = 4.0;
/// Body widths under which the metric cards take two columns, then one.
const TWO_COLUMNS_BELOW: f32 = 800.0;
const ONE_COLUMN_BELOW: f32 = 480.0;

const CELL: f32 = 12.0;
const CELL_GAP: f32 = 4.0;
const CELL_STEP: f32 = CELL + CELL_GAP;
const CELL_RADIUS: u8 = 2;
/// Today's ring: 1 pt of text colour, 1 pt outside the cell.
const TODAY_RING: f32 = 1.0;
const WEEKDAY_GUTTER: f32 = 40.0;
/// Room around the grid for today's ring and the keyboard focus ring.
const RING_ROOM: f32 = 4.0;
const MONTH_ROW: f32 = 24.0;
const MONTH_LABEL_GAP: f32 = 8.0;
const LEGEND_ROW: f32 = 24.0;
const LEGEND_CELLS: usize = HEAT_LEVELS as usize + 1;
const DAY_CARD_WIDTH: f32 = 240.0;
/// The most books a day card lists; more are counted.
const DAY_CARD_BOOKS: usize = 5;
const DAYS_PER_WEEK: i64 = 7;

const BAR_WIDTH: f32 = 32.0;
const PLOT_HEIGHT: f32 = 128.0;
const VALUE_ROW: f32 = 24.0;
const WEEKDAY_ROW: f32 = 24.0;
const BAR_RADIUS: f32 = 4.0;
/// A day with reading never draws a bar shorter than this.
const MIN_BAR: f32 = 2.0;
/// A day without reading: a baseline dash this high, not a grey column.
const ZERO_DASH: f32 = 2.0;
const TODAY_DOT: f32 = 4.0;
const STREAK_ROW: f32 = 40.0;

const TABLE_HEADER: f32 = 40.0;
const TABLE_ROW: f32 = 64.0;
const THUMB: Vec2 = Vec2::new(32.0, 40.0);
const THUMB_RADIUS: u8 = 4;
const THUMB_GLYPH: f32 = 16.0;
const COLUMN_GAP: f32 = 16.0;
const TITLE_MIN: f32 = 240.0;
const TITLE_ROWS: usize = 2;
const PROGRESS_TRACK: Vec2 = Vec2::new(96.0, 4.0);
const SORT_ICON: f32 = 12.0;
/// The hover state of a header button sits this far inside its cell.
const HEADER_BUTTON_INSET: Vec2 = Vec2::new(4.0, 6.0);

const EMPTY_PAD: f32 = 48.0;
const EMPTY_ICON: f32 = 32.0;
const EMPTY_TITLE: f32 = 16.0;

const SECS_PER_MINUTE: i64 = 60;
const PERCENT: f64 = 100.0;
/// What the table and cards print for a number that doesn't exist (no pages, not in the library).
const NONE_MARK: &str = "—";
const NO_STATS: &str = "there are no reading statistics";
const EXPORT_TIP: &str = "Every recorded page visit, all time, as CSV";

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTH_NAMES: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const WEEKDAY_NAMES: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// The open sheet: what it read, the table's order and the calendar's keyboard day.
#[derive(Default)]
pub(crate) struct Sheet {
    /// The database's answer when the sheet opened (or on Retry).
    model: Option<Result<Model, String>>,
    sort: Sort,
    /// The indices of the model's books in table order, for `sort`.
    order: Vec<usize>,
    /// The calendar's keyboard day (today until moved) and whether its card stays open.
    cursor: Option<i64>,
    pinned: bool,
}

/// What the sheet shows.
struct Model {
    overview: Overview,
    /// Each period book's library entry (cover and reading position), parallel to
    /// `overview.books`; `None` for a book no longer in the library.
    library: Vec<Option<Book>>,
}

/// A table column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Cover,
    Title,
    Time,
    Pages,
    Pace,
    LastRead,
    Progress,
}

impl Column {
    /// Left to right, with their widths (0: the title takes what is left) and alignment.
    const ALL: [(Column, f32, Align); 7] = [
        (Column::Cover, 40.0, Align::Min),
        (Column::Title, 0.0, Align::Min),
        (Column::Time, 96.0, Align::Max),
        (Column::Pages, 72.0, Align::Max),
        (Column::Pace, 96.0, Align::Max),
        (Column::LastRead, 144.0, Align::Min),
        (Column::Progress, 160.0, Align::Max),
    ];

    fn heading(self) -> &'static str {
        match self {
            Column::Cover => "",
            Column::Title => "Title",
            Column::Time => "Time",
            Column::Pages => "Pages",
            Column::Pace => "Min/page",
            Column::LastRead => "Last read",
            Column::Progress => "Progress",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Column::Cover | Column::Title => "Sort by title",
            Column::Time => "Recorded time in the last 12 months",
            Column::Pages => "Distinct pages with recorded reading in the last 12 months",
            Column::Pace => "Minutes per distinct page; rereading counts",
            Column::LastRead => "When the last recorded visit ended",
            Column::Progress => "Reading position, from the library",
        }
    }
}

/// The table's order: a column and its direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub column: Column,
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self { column: Column::Time, descending: true }
    }
}

impl Sort {
    /// After a click on `column`'s header: the other direction, or the new column in its
    /// natural direction (titles A to Z, numbers and dates largest first).
    pub fn clicked(self, column: Column) -> Self {
        if self.column == column { Self { descending: !self.descending, ..self } } else { Self { column, descending: column != Column::Title } }
    }
}

/// What a click in the sheet asks for, applied after drawing.
enum Action {
    Close,
    Export,
    Retry,
    Settings,
    OpenPdf,
}

impl Sheet {
    fn load(&mut self, app: &mut PdfCraftApp) {
        let model = load_model(app);
        match &model {
            Ok(model) => self.order = table_order(&model.overview.books, &model.library, self.sort),
            Err(err) => log::warn!("stats: {err}"),
        }
        self.model = Some(model);
    }
}

/// Reads the overview for `now` (Unix seconds).
pub fn load(app: &mut PdfCraftApp, now: f64) -> Result<Overview, String> {
    if !super::stats::ensure_open(app) {
        return Err(NO_STATS.to_owned());
    }
    let db = app.margin_stats.db().ok_or_else(|| NO_STATS.to_owned())?;
    db.overview(now.floor() as i64)
}

fn load_model(app: &mut PdfCraftApp) -> Result<Model, String> {
    let now = app.margin_stats.now();
    let overview = load(app, now)?;
    super::library::refresh(app);
    let shelf = app.margin_library.books();
    let library = overview.books.iter().map(|book| library_entry(shelf, &book.book).cloned()).collect();
    Ok(Model { overview, library })
}

/// The library entry of the book with key `book_id` (stats are kept under the book's key).
fn library_entry<'a>(books: &'a [Book], book_id: &str) -> Option<&'a Book> {
    books.iter().find(|book| book.book_id == book_id)
}

pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let Some(mut sheet) = app.margin_stats.sheet.take() else { return };
    if sheet.model.is_none() {
        sheet.load(app);
    }
    match draw(app, ctx, &mut sheet) {
        None => app.margin_stats.sheet = Some(sheet),
        Some(Action::Close) => {}
        Some(Action::Export) => {
            app.margin_stats.sheet = Some(sheet);
            export_dialog(app);
        }
        Some(Action::Retry) => {
            app.margin_stats.retry_open();
            sheet.model = None;
            app.margin_stats.sheet = Some(sheet);
        }
        Some(Action::Settings) => {
            app.execute("app.preferences");
        }
        Some(Action::OpenPdf) => {
            app.execute("file.open");
        }
    }
}

/// Asks where to write the CSV, then writes it.
pub(crate) fn export_dialog(app: &mut PdfCraftApp) {
    let dialog = rfd::AsyncFileDialog::new().set_title("Export reading stats").set_file_name("margin-reading-stats.csv").add_filter("CSV", &["csv"]);
    app.ask_one(crate::pickers::Ask::Save(dialog), None, |app, path| match super::stats::export_csv(app, &path) {
        Ok(()) => app.notify(format!("Exported reading stats to {}", path.display())),
        Err(err) => app.notify(format!("Couldn't export the stats: {err}")),
    });
}

/// The modal: header, then the scrolling body. Returns what was clicked.
fn draw(app: &mut PdfCraftApp, ctx: &egui::Context, sheet: &mut Sheet) -> Option<Action> {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let full_window = screen.width() < FULL_WINDOW_BELOW;
    let outer = if full_window { screen.size() } else { (screen.size() - Vec2::splat(SHEET_INSET)).min(SHEET_MAX) };
    let size = (outer - Vec2::splat(2.0 * look::EDGE)).max(Vec2::ZERO);
    let frame = if full_window { look::modal_frame(&t).corner_radius(0) } else { look::modal_frame(&t) };
    let recording = app.margin_keys.settings().stats.enabled;
    let has_document = app.active.is_some();
    let mut action = None;
    let modal = look::modal(egui::Id::new("margin-stats"), &t).frame(frame).show(ctx, |ui| {
        ui.set_min_size(size);
        ui.set_max_size(size);
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let export_off = match &sheet.model {
            Some(Ok(model)) if model.overview.visits > 0 => None,
            Some(Ok(_)) => Some("No recorded visits to export"),
            _ => Some("The stats database couldn't be read"),
        };
        header(ui, &t, recording, export_off, &mut action);
        egui::ScrollArea::vertical().id_salt("margin-stats-body").auto_shrink(false).show(ui, |ui| {
            egui::Frame::NONE.inner_margin(PAD).show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                body(app, ui, &t, sheet, has_document, &mut action);
            });
        });
    });
    if action.is_none() && modal.should_close() {
        action = Some(Action::Close);
    }
    action
}

/// Title on the left; Close, Export CSV and (with recording off) its status and Settings on the
/// right; one divider under it.
fn header(ui: &mut egui::Ui, t: &Tokens, recording: bool, export_off: Option<&str>, action: &mut Option<Action>) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_HEIGHT), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.bottom() - look::EDGE / 2.0, Stroke::new(look::EDGE, t.divider));
    let mut row = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(PAD, 0.0))).layout(Layout::left_to_right(Align::Center)));
    row.spacing_mut().item_spacing.x = INNER_GAP;
    row.label(RichText::new("Reading stats").font(theme::semibold(TITLE_SIZE)).color(t.text));
    row.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if look::button_icon(ui, ButtonSize::Standard, "x", "Close", None).clicked() {
            *action = Some(Action::Close);
        }
        let export = ui.add_enabled_ui(export_off.is_none(), |ui| {
            Button::secondary(ButtonSize::Standard, "Export CSV").icon("file-down").tip(export_off.unwrap_or(EXPORT_TIP)).show(ui)
        });
        if export.inner.clicked() {
            *action = Some(Action::Export);
        }
        if !recording {
            let settings = Button::secondary(ButtonSize::Standard, "Settings")
                .icon("settings")
                .tip("Recording is turned off by stats.enabled in settings.jsonc");
            if settings.show(ui).clicked() {
                *action = Some(Action::Settings);
            }
            ui.label(RichText::new("Recording is off").font(theme::regular(BODY_TEXT)).color(t.text_muted));
        }
    });
}

fn body(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, sheet: &mut Sheet, has_document: bool, action: &mut Option<Action>) {
    let Sheet { model, sort, order, cursor, pinned } = sheet;
    let model = match model {
        Some(Ok(model)) => model,
        Some(Err(err)) => {
            error_state(ui, t, err, action);
            return;
        }
        None => return,
    };
    let overview = &model.overview;
    metric_cards(ui, t, overview);
    ui.add_space(SECTION_GAP);
    if overview.visits == 0 {
        empty_state(ui, t, has_document, overview.streaks, action);
        return;
    }
    calendar_section(ui, t, overview, cursor, pinned);
    ui.add_space(SECTION_GAP);
    week_section(ui, t, overview);
    ui.add_space(SECTION_GAP);
    books_section(app, ui, t, model, sort, order);
}

fn section_title(ui: &mut egui::Ui, t: &Tokens, title: &str, detail: Option<&str>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = INNER_GAP;
        ui.label(RichText::new(title).font(theme::semibold(SECTION_TITLE_SIZE)).color(look::Surfaces::of(t).heading));
        if let Some(detail) = detail {
            ui.label(RichText::new(detail).font(theme::regular(SMALL_TEXT)).color(t.text_muted));
        }
    });
    ui.add_space(look::GAP_LARGE);
}

/// A chart well: panel fill, the theme's edge, radius 8, padded.
fn well(ui: &mut egui::Ui, t: &Tokens, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE.fill(t.panel).stroke(look::edge_stroke(t.border)).corner_radius(look::RADIUS_CARD).inner_margin(INNER_PAD).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        contents(ui);
    });
}

// ---- Metric cards ----

/// Which way a comparison went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trend {
    Up,
    Down,
}

/// One metric card: icon, label, value and the honest comparison under it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Metric {
    icon: &'static str,
    label: &'static str,
    value: String,
    trend: Option<Trend>,
    note: String,
}

impl Metric {
    fn accessible(&self) -> String {
        format!("{}: {}. {}", self.label, self.value, self.note)
    }
}

/// Today, this week, the last 12 months and the books read in them.
fn metrics(overview: &Overview) -> [Metric; 4] {
    let weekday_name = WEEKDAY_NAMES.get(weekday(overview.today) as usize).copied().unwrap_or("");
    let (today_trend, today_note) =
        compare_time(overview.today_so_far.seconds, overview.today_last_week.seconds, &format!("last {weekday_name} at this time"));
    let (week_trend, week_note) = compare_time(overview.week_so_far.seconds, overview.last_week_to_date.seconds, "last week to date");
    let period = overview.period_seconds();
    let active = i64::try_from(overview.days.len()).unwrap_or(i64::MAX);
    let period_note = if active == 0 {
        "No active days".to_owned()
    } else {
        format!("{} · {} per active day", count(overview.days.len(), "active day"), short_duration(period / active))
    };
    let (this_week, last_week) = (i64::from(overview.week_so_far.books), i64::from(overview.last_week_to_date.books));
    let books_change = this_week - last_week;
    let books_note = match books_change.cmp(&0) {
        Ordering::Equal => format!("{this_week} this week · same as last week to date"),
        Ordering::Greater => format!("{this_week} this week · +{books_change} vs last week to date"),
        Ordering::Less => format!("{this_week} this week · −{} vs last week to date", books_change.unsigned_abs()),
    };
    [
        Metric { icon: "clock", label: "Today", value: short_duration(overview.today_so_far.seconds), trend: today_trend, note: today_note },
        Metric { icon: "columns-3", label: "This week", value: short_duration(overview.week_so_far.seconds), trend: week_trend, note: week_note },
        Metric { icon: "grid-3x3", label: "Last 12 months", value: short_duration(period), trend: None, note: period_note },
        Metric { icon: "book-open", label: "Books", value: overview.books.len().to_string(), trend: trend_of(books_change), note: books_note },
    ]
}

fn trend_of(change: i64) -> Option<Trend> {
    match change.cmp(&0) {
        Ordering::Greater => Some(Trend::Up),
        Ordering::Less => Some(Trend::Down),
        Ordering::Equal => None,
    }
}

/// `now` against `before` over the same elapsed time; no comparison against nothing.
fn compare_time(now: i64, before: i64, baseline: &str) -> (Option<Trend>, String) {
    if before <= 0 {
        return (None, "No prior-week reading".to_owned());
    }
    let change = now - before;
    let note = match change.cmp(&0) {
        Ordering::Equal => format!("Same as {baseline}"),
        Ordering::Greater => format!("+{} vs {baseline}", short_duration(change)),
        Ordering::Less => format!("−{} vs {baseline}", short_duration(change.saturating_abs())),
    };
    (trend_of(change), note)
}

fn metric_cards(ui: &mut egui::Ui, t: &Tokens, overview: &Overview) {
    let width = ui.available_width();
    let columns: usize = if width < ONE_COLUMN_BELOW {
        1
    } else if width < TWO_COLUMNS_BELOW {
        2
    } else {
        4
    };
    let card_width = (width - CARD_GAP * (columns - 1) as f32) / columns as f32;
    for (index, row) in metrics(overview).chunks(columns).enumerate() {
        if index > 0 {
            ui.add_space(CARD_GAP);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = CARD_GAP;
            for metric in row {
                metric_card(ui, t, metric, card_width);
            }
        });
    }
}

/// Card fill, the theme's edge, no shadow: these are numbers, not buttons.
fn metric_card(ui: &mut egui::Ui, t: &Tokens, metric: &Metric, width: f32) {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, CARD_HEIGHT), Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, metric.accessible()));
    ui.painter().rect(rect, CornerRadius::same(look::RADIUS_CARD), t.card, look::edge_stroke(t.border), egui::StrokeKind::Inside);
    let inner = rect.shrink(INNER_PAD);
    let painter = ui.painter_at(inner);
    let icon = Rect::from_min_size(inner.min, Vec2::splat(CARD_ICON));
    icons::paint(ui, icon, metric.icon, CARD_ICON, t.icon);
    painter.text(pos2(icon.right() + INNER_GAP, icon.center().y), Align2::LEFT_CENTER, metric.label, theme::medium(SMALL_TEXT), t.text_muted);
    painter.text(pos2(inner.left(), icon.bottom() + INNER_GAP), Align2::LEFT_TOP, &metric.value, theme::semibold(CARD_VALUE), t.text);
    let mut x = inner.left();
    if let Some(trend) = metric.trend {
        let icon = if trend == Trend::Up { "chevron-up" } else { "chevron-down" };
        icons::paint(ui, Rect::from_min_size(pos2(x, inner.bottom() - TREND_ICON), Vec2::splat(TREND_ICON)), icon, TREND_ICON, t.text_muted);
        x += TREND_ICON + TREND_GAP;
    }
    painter.text(pos2(x, inner.bottom()), Align2::LEFT_BOTTOM, &metric.note, theme::regular(SMALL_TEXT), t.text_muted);
}

// ---- Calendar ----

/// The calendar's grid: a column per week (Monday on top) from the week holding `first` to the
/// week holding `today`; 53 columns, or 54 when the dates fall that way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CalendarGrid {
    first: i64,
    today: i64,
    /// The Monday on or before `first`: the first column's top cell.
    start: i64,
    weeks: i64,
}

/// A month's label over the column holding its first day in the period.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MonthLabel {
    column: i64,
    text: String,
    /// The period starts in this month after its first day.
    partial: bool,
}

impl CalendarGrid {
    fn new(first: i64, today: i64) -> Self {
        let start = first - i64::from(weekday(first));
        Self { first, today, start, weeks: (today - start).div_euclid(DAYS_PER_WEEK) + 1 }
    }

    fn size(&self) -> Vec2 {
        vec2(2.0 * RING_ROOM + self.weeks as f32 * CELL_STEP - CELL_GAP, MONTH_ROW + DAYS_PER_WEEK as f32 * CELL_STEP - CELL_GAP + RING_ROOM)
    }

    fn cell(&self, origin: egui::Pos2, day: i64) -> Rect {
        let offset = day - self.start;
        let (column, row) = (offset.div_euclid(DAYS_PER_WEEK), offset.rem_euclid(DAYS_PER_WEEK));
        Rect::from_min_size(pos2(origin.x + RING_ROOM + column as f32 * CELL_STEP, origin.y + MONTH_ROW + row as f32 * CELL_STEP), Vec2::splat(CELL))
    }

    /// The period's day whose cell is at `pos`, if any (not the gaps between cells).
    fn day_at(&self, origin: egui::Pos2, pos: egui::Pos2) -> Option<i64> {
        let column = ((pos.x - origin.x - RING_ROOM) / CELL_STEP).floor();
        let row = ((pos.y - origin.y - MONTH_ROW) / CELL_STEP).floor();
        if !column.is_finite() || !row.is_finite() || column < 0.0 || !(0.0..DAYS_PER_WEEK as f32).contains(&row) {
            return None;
        }
        let day = self.start + column as i64 * DAYS_PER_WEEK + row as i64;
        ((self.first..=self.today).contains(&day) && self.cell(origin, day).contains(pos)).then_some(day)
    }

    /// The months' labels: the period's first month (with its year), then each month's first
    /// day, with the year in January.
    fn month_labels(&self) -> Vec<MonthLabel> {
        (self.first..=self.today)
            .filter_map(|day| {
                let (year, month, date) = civil(day);
                if day != self.first && date != 1 {
                    return None;
                }
                let name = MONTHS.get(month.saturating_sub(1) as usize).copied().unwrap_or("");
                let text = if day == self.first || month == 1 { format!("{name} {year}") } else { name.to_owned() };
                Some(MonthLabel { column: (day - self.start).div_euclid(DAYS_PER_WEEK), text, partial: date != 1 })
            })
            .collect()
    }
}

fn calendar_section(ui: &mut egui::Ui, t: &Tokens, overview: &Overview, cursor: &mut Option<i64>, pinned: &mut bool) {
    let range = format!("{} – {}", short_date(overview.first), short_date(overview.today));
    section_title(ui, t, "Last 12 months", Some(&range));
    well(ui, t, |ui| {
        let grid = CalendarGrid::new(overview.first, overview.today);
        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
            weekday_gutter(ui, t, grid.size().y);
            // Too narrow for the whole year: the weeks scroll, starting at the recent end, while
            // the weekday labels stay.
            egui::ScrollArea::horizontal().id_salt("margin-stats-calendar").auto_shrink([false, true]).stick_to_right(true).show(ui, |ui| {
                calendar(ui, t, overview, &grid, cursor, pinned);
            });
        });
        ui.add_space(INNER_GAP);
        legend(ui, t, overview.busiest());
    });
}

/// All seven weekday names, each level with its row of cells.
fn weekday_gutter(ui: &mut egui::Ui, t: &Tokens, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(WEEKDAY_GUTTER, height), Sense::hover());
    for (row, name) in WEEKDAYS.iter().enumerate() {
        let y = rect.top() + MONTH_ROW + row as f32 * CELL_STEP + CELL / 2.0;
        ui.painter().text(pos2(rect.left(), y), Align2::LEFT_CENTER, *name, theme::regular(LABEL_TEXT), t.text_muted);
    }
}

/// The heatmap. One tab stop: arrows move the keyboard day, Enter pins its card, Esc closes it;
/// pointing at a day shows its card, clicking pins it.
fn calendar(ui: &mut egui::Ui, t: &Tokens, overview: &Overview, grid: &CalendarGrid, cursor: &mut Option<i64>, pinned: &mut bool) {
    let heat = Surfaces::of(t).heat;
    let (rect, resp) = ui.allocate_exact_size(grid.size(), Sense::click());
    let origin = rect.min;
    let painter = ui.painter_at(rect);
    for (x, galley) in placed_month_labels(&painter, t, grid, origin) {
        painter.galley(pos2(x, origin.y), galley, t.text_muted);
    }
    let busiest = overview.busiest();
    for day in grid.first..=grid.today {
        let cell = grid.cell(origin, day);
        let level = usize::from(heat_level(overview.seconds_on(day), busiest));
        painter.rect_filled(cell, CornerRadius::same(CELL_RADIUS), heat.get(level).copied().unwrap_or(heat[0]));
        if day == grid.today {
            let ring = CornerRadius::same(CELL_RADIUS + TODAY_RING as u8);
            painter.rect_stroke(cell.expand(TODAY_RING), ring, Stroke::new(TODAY_RING, t.text), egui::StrokeKind::Outside);
        }
        let cell_resp = ui.interact(cell, resp.id.with(day), Sense::hover());
        cell_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, day_label(overview, day)));
    }
    let hovered = if resp.contains_pointer() { ui.input(|i| i.pointer.hover_pos()).and_then(|pos| grid.day_at(origin, pos)) } else { None };
    let mut day = cursor.unwrap_or(grid.today).clamp(grid.first, grid.today);
    if resp.has_focus() {
        let arrows = egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() };
        ui.memory_mut(|memory| memory.set_focus_lock_filter(resp.id, arrows));
        let steps =
            [(egui::Key::ArrowUp, -1), (egui::Key::ArrowDown, 1), (egui::Key::ArrowLeft, -DAYS_PER_WEEK), (egui::Key::ArrowRight, DAYS_PER_WEEK)];
        let moved: i64 = ui.input_mut(|i| steps.iter().filter(|(key, _)| i.consume_key(egui::Modifiers::NONE, *key)).map(|(_, step)| step).sum());
        day = (day + moved).clamp(grid.first, grid.today);
    }
    if resp.clicked_by(egui::PointerButton::Primary) {
        if let Some(hit) = hovered {
            *pinned = !(*pinned && day == hit);
            day = hit;
        }
    } else if resp.clicked() {
        // Enter or Space on the focused calendar.
        *pinned = !*pinned;
    }
    if *pinned && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        *pinned = false;
    }
    *cursor = Some(day);
    look::focus_ring(ui, t, grid.cell(origin, day), &resp, CELL_RADIUS);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, format!("Reading calendar. {}", day_label(overview, day))));
    if let Some(shown) = hovered.or_else(|| (*pinned || resp.has_focus()).then_some(day)) {
        look::detail_card(&resp, grid.cell(origin, shown), resp.id.with("day-card"), |ui| day_card(ui, t, overview, shown));
    }
}

/// The month labels that fit, at least [`MONTH_LABEL_GAP`] apart: when the period's partial
/// first month crowds the next month, the next month keeps its label.
fn placed_month_labels(painter: &egui::Painter, t: &Tokens, grid: &CalendarGrid, origin: egui::Pos2) -> Vec<(f32, Arc<Galley>)> {
    let mut placed: Vec<(f32, Arc<Galley>, bool)> = Vec::new();
    for label in grid.month_labels() {
        let x = origin.x + RING_ROOM + label.column as f32 * CELL_STEP;
        let galley = painter.layout_no_wrap(label.text, theme::regular(LABEL_TEXT), t.text_muted);
        if let Some((last_x, last, last_partial)) = placed.last()
            && x < last_x + last.size().x + MONTH_LABEL_GAP
        {
            if *last_partial && placed.len() == 1 {
                placed.pop();
            } else {
                continue;
            }
        }
        placed.push((x, galley, label.partial));
    }
    placed.into_iter().map(|(x, galley, _)| (x, galley)).collect()
}

/// "Less", the five levels, "More"; its tooltip says what the colours mean.
fn legend(ui: &mut egui::Ui, t: &Tokens, busiest: i64) {
    let heat = Surfaces::of(t).heat;
    let painter = ui.painter();
    let less = painter.layout_no_wrap("Less".to_owned(), theme::regular(LABEL_TEXT), t.text_muted);
    let more = painter.layout_no_wrap("More".to_owned(), theme::regular(LABEL_TEXT), t.text_muted);
    let cells = LEGEND_CELLS as f32 * CELL_STEP - CELL_GAP;
    let (less_size, more_size) = (less.size(), more.size());
    let width = less_size.x + INNER_GAP + cells + INNER_GAP + more_size.x;
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), LEGEND_ROW), Sense::hover());
    let rect = Rect::from_min_size(pos2(row.right() - width, row.top()), vec2(width, LEGEND_ROW));
    let resp = ui.interact(rect, ui.id().with("margin-stats-legend"), Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, "Less to more reading"));
    let y = rect.center().y;
    let mut x = rect.left();
    ui.painter().galley(pos2(x, y - less_size.y / 2.0), less, t.text_muted);
    x += less_size.x + INNER_GAP;
    for color in heat {
        ui.painter().rect_filled(Rect::from_min_size(pos2(x, y - CELL / 2.0), Vec2::splat(CELL)), CornerRadius::same(CELL_RADIUS), color);
        x += CELL_STEP;
    }
    x += INNER_GAP - CELL_GAP;
    ui.painter().galley(pos2(x, y - more_size.y / 2.0), more, t.text_muted);
    look::tooltip_ui(&resp, |ui| {
        ui.label(RichText::new("Relative to your busiest day in this period").color(t.text));
        for line in legend_levels(busiest) {
            ui.label(RichText::new(line).color(t.text_muted));
        }
    });
}

/// What each level covers, for the legend's tooltip.
fn legend_levels(busiest: i64) -> Vec<String> {
    if busiest <= 0 {
        return vec!["No reading in this period yet".to_owned()];
    }
    let mut lines = vec!["Empty: no reading".to_owned()];
    for level in 1..=HEAT_LEVELS {
        let upper = busiest.saturating_mul(level) / HEAT_LEVELS;
        lines.push(format!("Level {level}: up to {}", short_duration(upper)));
    }
    lines
}

/// A day's accessible name: its full date and reading ("Today, Saturday, 10 October 2026:
/// 15 min 0 sec · 2 pages · 1 book").
fn day_label(overview: &Overview, day: i64) -> String {
    let prefix = if day == overview.today { "Today, " } else { "" };
    match overview.day(day) {
        Some(stats) => format!("{prefix}{}: {}", full_date(day), day_summary(stats)),
        None => format!("{prefix}{}: no reading recorded", full_date(day)),
    }
}

fn day_summary(stats: &DayStats) -> String {
    format!("{} · {} · {}", minutes_seconds(stats.seconds), count(stats.pages as usize, "page"), count(stats.books.len(), "book"))
}

/// The day card: the date, the day's reading, then its books and their time.
fn day_card(ui: &mut egui::Ui, t: &Tokens, overview: &Overview, day: i64) {
    let width = DAY_CARD_WIDTH - 2.0 * INNER_PAD;
    ui.set_width(width);
    ui.spacing_mut().item_spacing.y = INNER_GAP / 2.0;
    ui.label(RichText::new(full_date(day)).font(theme::semibold(BODY_TEXT)).color(t.text));
    let Some(stats) = overview.day(day) else {
        ui.label(RichText::new("No reading recorded").font(theme::regular(SMALL_TEXT)).color(t.text_muted));
        return;
    };
    ui.label(RichText::new(day_summary(stats)).font(theme::regular(SMALL_TEXT)).color(t.text_muted));
    ui.add_space(INNER_GAP / 2.0);
    for book in stats.books.iter().take(DAY_CARD_BOOKS) {
        let title = if book.title.is_empty() { book.book.as_str() } else { book.title.as_str() };
        let time = ui.painter().layout_no_wrap(short_duration(book.seconds), theme::regular(SMALL_TEXT), t.text_muted);
        let title = truncated(ui, title, theme::regular(SMALL_TEXT), width - time.size().x - INNER_GAP, 1);
        let (rect, _) = ui.allocate_exact_size(vec2(width, title.size().y.max(time.size().y)), Sense::hover());
        ui.painter().galley(rect.min, title, t.text);
        ui.painter().galley(pos2(rect.right() - time.size().x, rect.top()), time, t.text_muted);
    }
    let more = stats.books.len().saturating_sub(DAY_CARD_BOOKS);
    if more > 0 {
        ui.label(RichText::new(format!("and {} more", count(more, "book"))).font(theme::regular(SMALL_TEXT)).color(t.text_muted));
    }
}

// ---- This week and streaks ----

fn week_section(ui: &mut egui::Ui, t: &Tokens, overview: &Overview) {
    section_title(ui, t, "This week", None);
    well(ui, t, |ui| {
        week_chart(ui, t, overview);
        streak_line(ui, t, overview.streaks);
    });
}

/// Monday to Sunday: a bar per day with its time above, a dash for a day without reading, an
/// em dash for a day still to come; today in orange, marked "Today" with a dot.
fn week_chart(ui: &mut egui::Ui, t: &Tokens, overview: &Overview) {
    let heat = Surfaces::of(t).heat;
    let monday = overview.monday();
    let seconds: Vec<i64> = (0..DAYS_PER_WEEK).map(|offset| overview.seconds_on(monday + offset)).collect();
    let busiest = seconds.iter().copied().max().unwrap_or(0);
    let height = VALUE_ROW + INNER_GAP + PLOT_HEIGHT + INNER_GAP + WEEKDAY_ROW;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter_at(rect);
    let base = rect.top() + VALUE_ROW + INNER_GAP + PLOT_HEIGHT;
    let plot = Rect::from_min_max(pos2(rect.left(), base - PLOT_HEIGHT), pos2(rect.right(), base));
    painter.hline(rect.x_range(), base + look::EDGE / 2.0, Stroke::new(look::EDGE, t.divider));
    let slot = rect.width() / DAYS_PER_WEEK as f32;
    for (offset, day_seconds) in (0..DAYS_PER_WEEK).zip(seconds.iter().copied()) {
        let day = monday + offset;
        let center = rect.left() + slot * (offset as f32 + 0.5);
        let today = day == overview.today;
        let (value, value_bottom, value_color) = if day > overview.today {
            (NONE_MARK.to_owned(), base - INNER_GAP, t.text_muted)
        } else if day_seconds > 0 && busiest > 0 {
            let bar_height = (PLOT_HEIGHT * day_seconds as f32 / busiest as f32).max(MIN_BAR);
            let bar = Rect::from_min_max(pos2(center - BAR_WIDTH / 2.0, base - bar_height), pos2(center + BAR_WIDTH / 2.0, base));
            let radius = BAR_RADIUS.min(bar_height / 2.0) as u8;
            let fill = if today { heat[3] } else { heat[2] };
            painter.rect_filled(bar, CornerRadius { nw: radius, ne: radius, sw: 0, se: 0 }, fill);
            (short_duration(day_seconds), bar.top() - INNER_GAP, t.text)
        } else {
            let dash = Rect::from_min_max(pos2(center - BAR_WIDTH / 2.0, base - ZERO_DASH), pos2(center + BAR_WIDTH / 2.0, base));
            painter.rect_filled(dash, CornerRadius::ZERO, heat[0]);
            (short_duration(0), dash.top() - INNER_GAP, t.text_muted)
        };
        painter.text(pos2(center, value_bottom), Align2::CENTER_BOTTOM, &value, theme::regular(SMALL_TEXT), value_color);
        let name = WEEKDAYS.get(offset as usize).copied().unwrap_or("");
        let label_y = base + INNER_GAP + WEEKDAY_ROW / 2.0;
        if today {
            let galley = painter.layout_no_wrap("Today".to_owned(), theme::medium(SMALL_TEXT), t.text);
            let width = TODAY_DOT + TREND_GAP + galley.size().x;
            let left = center - width / 2.0;
            painter.circle_filled(pos2(left + TODAY_DOT / 2.0, label_y), TODAY_DOT / 2.0, t.accent);
            painter.galley(pos2(left + TODAY_DOT + TREND_GAP, label_y - galley.size().y / 2.0), galley, t.text);
        } else {
            painter.text(pos2(center, label_y), Align2::CENTER_CENTER, name, theme::regular(SMALL_TEXT), t.text_muted);
        }
        let slot_rect = Rect::from_min_max(pos2(center - slot / 2.0, rect.top()), pos2(center + slot / 2.0, rect.bottom()));
        let slot_resp = ui.interact(slot_rect, resp.id.with(offset), Sense::hover());
        let full = WEEKDAY_NAMES.get(offset as usize).copied().unwrap_or("");
        let spoken = if day > overview.today { "still to come".to_owned() } else { short_duration(day_seconds) };
        slot_resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("{full}{}: {spoken}", if today { ", today" } else { "" }))
        });
    }
    if busiest == 0 {
        painter.text(plot.center(), Align2::CENTER_CENTER, "No reading recorded this week", theme::regular(BODY_TEXT), t.text_muted);
    }
}

/// "Current streak N days" and "Longest streak N days, all time".
fn streak_line(ui: &mut egui::Ui, t: &Tokens, streaks: Streaks) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), STREAK_ROW), Sense::hover());
    let mut row = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
    row.spacing_mut().item_spacing.x = INNER_PAD;
    row.label(streak_text(t, "Current streak", streaks.current, None));
    row.label(streak_text(t, "Longest streak", streaks.longest, Some("all time")));
}

fn streak_text(t: &Tokens, label: &str, days: u32, scope: Option<&str>) -> LayoutJob {
    let mut job = LayoutJob::default();
    let muted = TextFormat::simple(theme::regular(BODY_TEXT), t.text_muted);
    job.append(&format!("{label} "), 0.0, muted.clone());
    job.append(&count(days as usize, "day"), 0.0, TextFormat::simple(theme::semibold(BODY_TEXT), t.text));
    if let Some(scope) = scope {
        job.append(&format!(", {scope}"), 0.0, muted);
    }
    job
}

// ---- Books table ----

/// The columns' x ranges for a table `width` wide starting at `left`.
fn column_ranges(left: f32, width: f32) -> Vec<(Column, egui::Rangef, Align)> {
    let fixed: f32 = Column::ALL.iter().map(|(_, width, _)| width).sum();
    let gaps = COLUMN_GAP * (Column::ALL.len() - 1) as f32 + 2.0 * INNER_PAD;
    let title = (width - fixed - gaps).max(TITLE_MIN);
    let mut x = left + INNER_PAD;
    Column::ALL
        .iter()
        .map(|&(column, fixed_width, align)| {
            let column_width = if column == Column::Title { title } else { fixed_width };
            let range = egui::Rangef::new(x, x + column_width);
            x += column_width + COLUMN_GAP;
            (column, range, align)
        })
        .collect()
}

/// The narrowest table that keeps every column readable.
fn table_min_width() -> f32 {
    let fixed: f32 = Column::ALL.iter().map(|(_, width, _)| width).sum();
    fixed + TITLE_MIN + COLUMN_GAP * (Column::ALL.len() - 1) as f32 + 2.0 * INNER_PAD
}

fn books_section(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, model: &Model, sort: &mut Sort, order: &mut Vec<usize>) {
    section_title(ui, t, "Books · last 12 months", None);
    if model.overview.books.is_empty() {
        well(ui, t, |ui| {
            ui.vertical_centered(|ui| ui.label(RichText::new("No books read in this period").font(theme::regular(BODY_TEXT)).color(t.text_muted)));
        });
        return;
    }
    let width = ui.available_width().max(table_min_width());
    egui::ScrollArea::horizontal().id_salt("margin-stats-books").auto_shrink([false, true]).show(ui, |ui| {
        if let Some(column) = table(app, ui, t, model, *sort, order, width) {
            *sort = sort.clicked(column);
            *order = table_order(&model.overview.books, &model.library, *sort);
        }
    });
}

/// The bordered table: a header of sort buttons, then a row per book (only the rows on screen
/// are drawn). Returns the header clicked.
fn table(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, model: &Model, sort: Sort, order: &[usize], width: f32) -> Option<Column> {
    let height = TABLE_HEADER + TABLE_ROW * order.len() as f32;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let radius = CornerRadius::same(look::RADIUS_CARD);
    ui.painter().rect_filled(rect, radius, t.panel);
    let columns = column_ranges(rect.left(), width);
    let header = Rect::from_min_size(rect.min, vec2(width, TABLE_HEADER));
    let clicked = table_header(ui, t, header, &columns, sort);
    for (position, &index) in order.iter().enumerate() {
        let row = Rect::from_min_size(pos2(rect.left(), header.bottom() + position as f32 * TABLE_ROW), vec2(width, TABLE_ROW));
        if !ui.is_rect_visible(row) {
            continue;
        }
        let (Some(book), Some(entry)) = (model.overview.books.get(index), model.library.get(index)) else { continue };
        if position > 0 {
            ui.painter().hline(row.x_range(), row.top() + look::EDGE / 2.0, Stroke::new(look::EDGE, t.divider));
        }
        table_row(app, ui, t, row, &columns, book, entry.as_ref());
    }
    let edge = look::edge_stroke(t.border);
    if !edge.is_empty() {
        ui.painter().rect_stroke(rect, radius, edge, egui::StrokeKind::Inside);
    }
    clicked
}

fn table_header(ui: &mut egui::Ui, t: &Tokens, rect: Rect, columns: &[(Column, egui::Rangef, Align)], sort: Sort) -> Option<Column> {
    let top = CornerRadius { nw: look::RADIUS_CARD, ne: look::RADIUS_CARD, sw: 0, se: 0 };
    ui.painter().rect_filled(rect, top, t.card);
    ui.painter().hline(rect.x_range(), rect.bottom() - look::EDGE / 2.0, Stroke::new(look::EDGE, t.divider));
    let mut clicked = None;
    for &(column, range, align) in columns.iter().filter(|(column, _, _)| *column != Column::Cover) {
        let cell = Rect::from_x_y_ranges(range, rect.y_range());
        let active = sort.column == column;
        let resp = ui.interact(cell.expand2(vec2(HEADER_BUTTON_INSET.x, 0.0)), ui.id().with(("margin-stats-sort", column.heading())), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, active, format!("Sort by {}", column.heading())));
        let button = cell.expand2(vec2(HEADER_BUTTON_INSET.x, -HEADER_BUTTON_INSET.y));
        look::state_fill(ui, t, button, &resp, false, t.radius);
        look::focus_ring(ui, t, button, &resp, t.radius);
        let color = if active || resp.hovered() { t.text } else { t.text_muted };
        let galley = ui.painter().layout_no_wrap(column.heading().to_owned(), theme::medium(SMALL_TEXT), color);
        let chevron = if active { SORT_ICON + TREND_GAP } else { 0.0 };
        let content = galley.size().x + chevron;
        let left = if align == Align::Max { cell.right() - content } else { cell.left() };
        ui.painter().galley(pos2(left, cell.center().y - galley.size().y / 2.0), galley, color);
        if active {
            let icon = if sort.descending { "chevron-down" } else { "chevron-up" };
            let at = Rect::from_min_size(pos2(left + content - SORT_ICON, cell.center().y - SORT_ICON / 2.0), Vec2::splat(SORT_ICON));
            icons::paint(ui, at, icon, SORT_ICON, t.accent);
        }
        if look::tooltip(resp, column.tip(), None).clicked() {
            clicked = Some(column);
        }
    }
    clicked
}

fn table_row(
    app: &mut PdfCraftApp,
    ui: &mut egui::Ui,
    t: &Tokens,
    row: Rect,
    columns: &[(Column, egui::Rangef, Align)],
    book: &BookTotal,
    entry: Option<&Book>,
) {
    let title = display_title(book, entry);
    let resp = ui.interact(row, ui.id().with(("margin-stats-book", &book.book)), Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, title));
    look::state_fill(ui, t, row.shrink(look::EDGE), &resp, false, 0);
    let text = theme::regular(BODY_TEXT);
    for &(column, range, align) in columns {
        let cell = Rect::from_x_y_ranges(range, row.y_range());
        match column {
            Column::Cover => {
                let thumb = Rect::from_min_size(pos2(cell.left(), cell.center().y - THUMB.y / 2.0), THUMB);
                match entry {
                    Some(entry) => super::library::paint_book_cover(app, ui, t, thumb, entry),
                    None => missing_cover(ui, t, thumb),
                }
            }
            Column::Title => {
                let galley = truncated(ui, title, text.clone(), cell.width(), TITLE_ROWS);
                ui.painter().galley(pos2(cell.left(), cell.center().y - galley.size().y / 2.0), galley, t.text);
            }
            Column::Time => cell_text(ui, cell, align, short_duration(book.seconds), t.text),
            Column::Pages => cell_text(ui, cell, align, book.pages.to_string(), t.text),
            Column::Pace => cell_text(ui, cell, align, pace(book).map_or_else(|| NONE_MARK.to_owned(), |pace| format!("{pace:.1}")), t.text),
            Column::LastRead => cell_text(ui, cell, align, last_read(&book.last_local), t.text_muted),
            Column::Progress => progress_cell(ui, t, cell, entry),
        }
    }
    let tip = format!("{title}\nLast read {}", book.last_local);
    look::tooltip(resp, &tip, None);
}

/// A book no longer in the library: a neutral document glyph, never a made-up cover.
fn missing_cover(ui: &egui::Ui, t: &Tokens, rect: Rect) {
    let radius = CornerRadius::same(THUMB_RADIUS);
    ui.painter().rect(rect, radius, t.card, look::edge_stroke(t.border), egui::StrokeKind::Inside);
    icons::paint(ui, rect, "file-text", THUMB_GLYPH, t.text_faint);
}

fn cell_text(ui: &egui::Ui, cell: Rect, align: Align, text: String, color: egui::Color32) {
    let anchor = if align == Align::Max { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER };
    let x = if align == Align::Max { cell.right() } else { cell.left() };
    ui.painter().text(pos2(x, cell.center().y), anchor, text, theme::regular(BODY_TEXT), color);
}

/// The library's reading position: a 96 × 4 track with the share filled, and the percentage.
fn progress_cell(ui: &egui::Ui, t: &Tokens, cell: Rect, entry: Option<&Book>) {
    let Some(entry) = entry else {
        cell_text(ui, cell, Align::Max, NONE_MARK.to_owned(), t.text_muted);
        return;
    };
    let share = if entry.progress.is_finite() { entry.progress.clamp(0.0, 1.0) } else { 0.0 };
    let label = ui.painter().layout_no_wrap(format!("{:.0}%", share * PERCENT), theme::regular(BODY_TEXT), t.text);
    let label_left = cell.right() - label.size().x;
    ui.painter().galley(pos2(label_left, cell.center().y - label.size().y / 2.0), label, t.text);
    let track = Rect::from_min_size(pos2(label_left - INNER_GAP - PROGRESS_TRACK.x, cell.center().y - PROGRESS_TRACK.y / 2.0), PROGRESS_TRACK);
    let radius = CornerRadius::same(CELL_RADIUS);
    ui.painter().rect_filled(track, radius, Surfaces::of(t).track);
    let fill = Rect::from_min_size(track.min, vec2(track.width() * share as f32, track.height()));
    ui.painter().rect_filled(fill, radius, t.accent);
}

/// The title the stats kept, else the library's, else the book's key.
fn display_title<'a>(book: &'a BookTotal, entry: Option<&'a Book>) -> &'a str {
    if !book.title.is_empty() {
        return &book.title;
    }
    entry.map(|entry| entry.title.as_str()).filter(|title| !title.is_empty()).unwrap_or(&book.book)
}

/// Minutes per distinct page; `None` before any page.
fn pace(book: &BookTotal) -> Option<f64> {
    (book.pages > 0).then(|| book.seconds as f64 / SECS_PER_MINUTE as f64 / f64::from(book.pages))
}

/// The table's rows for `sort`: by the column (books without a pace or library entry last
/// either way), then most recently read, then title and key.
fn table_order(books: &[BookTotal], library: &[Option<Book>], sort: Sort) -> Vec<usize> {
    let entry = |index: usize| library.get(index).and_then(Option::as_ref);
    let directed = |ordering: Ordering| if sort.descending { ordering.reverse() } else { ordering };
    let missing_last = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (Some(a), Some(b)) => directed(a.total_cmp(&b)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    let mut order: Vec<usize> = (0..books.len()).collect();
    order.sort_by(|&a, &b| {
        let (Some(x), Some(y)) = (books.get(a), books.get(b)) else { return Ordering::Equal };
        let title = |book, index| display_title(book, entry(index)).to_lowercase();
        let primary = match sort.column {
            Column::Cover | Column::Title => directed(title(x, a).cmp(&title(y, b))),
            Column::Time => directed(x.seconds.cmp(&y.seconds)),
            Column::Pages => directed(x.pages.cmp(&y.pages)),
            Column::Pace => missing_last(pace(x), pace(y)),
            Column::LastRead => directed(x.last.cmp(&y.last)),
            Column::Progress => missing_last(entry(a).map(|book| book.progress), entry(b).map(|book| book.progress)),
        };
        primary.then_with(|| y.last.cmp(&x.last)).then_with(|| title(x, a).cmp(&title(y, b))).then_with(|| x.book.cmp(&y.book))
    });
    order
}

// ---- Empty and error states ----

/// No history at all: what will appear here, and the way back to a book.
fn empty_state(ui: &mut egui::Ui, t: &Tokens, has_document: bool, streaks: Streaks, action: &mut Option<Action>) {
    centred_panel(ui, t, |ui| {
        ui.add(icons::image("book-open", EMPTY_ICON, t.icon));
        ui.add_space(INNER_PAD);
        ui.label(RichText::new("Your reading history starts here").font(theme::semibold(EMPTY_TITLE)).color(t.text));
        ui.add_space(INNER_GAP);
        ui.label(
            RichText::new("Time is recorded while you read; quick page flips are not counted.").font(theme::regular(BODY_TEXT)).color(t.text_muted),
        );
        ui.add_space(SECTION_GAP);
        let (label, next) = if has_document { ("Back to reading", Action::Close) } else { ("Open PDF", Action::OpenPdf) };
        if look::button_primary(ui, ButtonSize::Prominent, label).clicked() {
            *action = Some(next);
        }
        ui.add_space(INNER_PAD);
        ui.label(streak_text(t, "Current streak", streaks.current, None));
        ui.add_space(INNER_GAP / 2.0);
        ui.label(streak_text(t, "Longest streak", streaks.longest, Some("all time")));
    });
}

/// The database couldn't be read: say so and offer Retry (not an empty history).
fn error_state(ui: &mut egui::Ui, t: &Tokens, err: &str, action: &mut Option<Action>) {
    centred_panel(ui, t, |ui| {
        ui.add(icons::image("triangle-alert", EMPTY_ICON, t.icon));
        ui.add_space(INNER_PAD);
        ui.label(RichText::new("Couldn't read your reading history").font(theme::semibold(EMPTY_TITLE)).color(t.text));
        ui.add_space(INNER_GAP);
        ui.label(RichText::new(err).font(theme::regular(BODY_TEXT)).color(t.text_muted));
        ui.add_space(SECTION_GAP);
        if Button::secondary(ButtonSize::Standard, "Retry").icon("rotate-cw").show(ui).clicked() {
            *action = Some(Action::Retry);
        }
    });
}

fn centred_panel(ui: &mut egui::Ui, t: &Tokens, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE.fill(t.panel).stroke(look::edge_stroke(t.border)).corner_radius(look::RADIUS_CARD).inner_margin(EMPTY_PAD).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            contents(ui);
        });
    });
}

// ---- Text ----

/// `text` wrapped to `width`, at most `rows` rows, the rest cut with an ellipsis.
fn truncated(ui: &egui::Ui, text: &str, font: FontId, width: f32, rows: usize) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat::simple(font, egui::Color32::PLACEHOLDER));
    job.wrap = egui::text::TextWrapping { max_width: width.max(0.0), max_rows: rows, break_anywhere: rows == 1, overflow_character: Some('…') };
    ui.painter().layout_job(job)
}

/// Time for a chart or card: "2h 14m", "25m", "<1m" for a little reading, "0m" for none.
fn short_duration(seconds: i64) -> String {
    match seconds {
        ..=0 => "0m".to_owned(),
        1..SECS_PER_MINUTE => "<1m".to_owned(),
        _ => margin_config::stats::format_duration((seconds - seconds % SECS_PER_MINUTE) as f64),
    }
}

/// "15 min 0 sec", for the day card.
fn minutes_seconds(seconds: i64) -> String {
    let seconds = seconds.max(0);
    format!("{} min {} sec", seconds / SECS_PER_MINUTE, seconds % SECS_PER_MINUTE)
}

/// "1 page", "3 pages".
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

/// "Saturday, 10 October 2026".
fn full_date(day: i64) -> String {
    let (year, month, date) = civil(day);
    let weekday = WEEKDAY_NAMES.get(weekday(day) as usize).copied().unwrap_or("");
    let month = MONTH_NAMES.get(month.saturating_sub(1) as usize).copied().unwrap_or("");
    format!("{weekday}, {date} {month} {year}")
}

/// "10 Oct 2026".
fn short_date(day: i64) -> String {
    let (year, month, date) = civil(day);
    let month = MONTHS.get(month.saturating_sub(1) as usize).copied().unwrap_or("");
    format!("{date} {month} {year}")
}

/// A local `YYYY-MM-DD HH:MM:SS` as "10 Oct 2026, 14:30".
fn last_read(local: &str) -> String {
    match (local.get(..10).and_then(day_number), local.get(11..16)) {
        (Some(day), Some(clock)) => format!("{}, {clock}", short_date(day)),
        _ => local.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use margin_config::stats::{DayBook, Interval};

    fn day(date: &str) -> i64 {
        day_number(date).unwrap()
    }

    fn book(key: &str, title: &str, seconds: i64, pages: u32, last: i64) -> BookTotal {
        BookTotal { book: key.into(), title: title.into(), seconds, pages, last, last_local: "2026-10-10 14:30:05".into() }
    }

    #[test]
    fn the_grid_covers_twelve_months_in_53_or_54_weeks() {
        let grid = CalendarGrid::new(day("2025-10-10"), day("2026-10-10"));
        assert_eq!((grid.start, grid.weeks), (day("2025-10-06"), 53));
        // A Sunday start and a Monday today need a 54th column.
        assert_eq!(CalendarGrid::new(day("2025-01-05"), day("2026-01-05")).weeks, 54);
        let origin = pos2(0.0, 0.0);
        let today = grid.cell(origin, grid.today);
        assert_eq!(grid.day_at(origin, today.center()), Some(grid.today));
        assert_eq!(grid.day_at(origin, today.right_top() + vec2(CELL_GAP / 2.0, 0.0)), None, "the gap is no day");
        assert_eq!(grid.day_at(origin, grid.cell(origin, grid.start).center()), None, "days before the period are blank");
        let labels = grid.month_labels();
        assert_eq!(labels.len(), 13);
        assert_eq!(labels[0], MonthLabel { column: 0, text: "Oct 2025".into(), partial: true });
        assert_eq!(labels[3].text, "Jan 2026", "the year shows where it changes");
        assert_eq!(labels[12].text, "Oct");
    }

    #[test]
    fn durations_and_dates_read_plainly() {
        assert_eq!(short_duration(0), "0m");
        assert_eq!(short_duration(30), "<1m");
        assert_eq!(short_duration(899), "14m");
        assert_eq!(short_duration(3840), "1h 4m");
        assert_eq!(minutes_seconds(930), "15 min 30 sec");
        assert_eq!(full_date(day("2026-10-10")), "Saturday, 10 October 2026");
        assert_eq!(last_read("2026-10-10 14:30:05"), "10 Oct 2026, 14:30");
        assert_eq!(last_read("bad"), "bad");
        assert_eq!(legend_levels(0), vec!["No reading in this period yet".to_owned()]);
        assert_eq!(legend_levels(3600).last().map(String::as_str), Some("Level 4: up to 1h"));
    }

    #[test]
    fn metric_cards_compare_like_with_like() {
        let today = day("2026-10-10");
        let overview = Overview {
            today,
            days: vec![DayStats { day: today, seconds: 900, pages: 2, books: vec![DayBook { book: "a".into(), title: "A".into(), seconds: 900 }] }],
            today_so_far: Interval { seconds: 900, books: 1 },
            today_last_week: Interval { seconds: 300, books: 1 },
            week_so_far: Interval { seconds: 900, books: 1 },
            last_week_to_date: Interval::default(),
            books: vec![book("a", "A", 900, 2, 0)],
            ..Overview::default()
        };
        let [today_card, week, period, books] = metrics(&overview);
        assert_eq!((today_card.value.as_str(), today_card.trend), ("15m", Some(Trend::Up)));
        assert_eq!(today_card.note, "+10m vs last Saturday at this time");
        assert_eq!(week.note, "No prior-week reading", "never a growth rate against nothing");
        assert_eq!(period.note, "1 active day · 15m per active day");
        assert_eq!(books.note, "1 this week · +1 vs last week to date");
        assert_eq!(books.accessible(), "Books: 1. 1 this week · +1 vs last week to date");
    }

    #[test]
    fn the_table_sorts_by_time_then_by_any_column() {
        let books = vec![book("a", "alpha", 600, 3, 30), book("b", "Beta", 1200, 0, 10), book("c", "", 600, 6, 40)];
        let entry = Book {
            id: 1,
            path: String::new(),
            book_id: "a".into(),
            title: "Gamma".into(),
            author: String::new(),
            pages: 10,
            size: 0,
            added: 0,
            last_opened: None,
            progress: 0.5,
            page: 0,
            finished: false,
            cover: margin_config::library::Cover::Missing,
            collections: Vec::new(),
            tags: Vec::new(),
        };
        let entry_c = Book { book_id: "c".into(), ..entry.clone() };
        let library = vec![Some(entry), None, Some(entry_c.clone())];
        let sort = Sort::default();
        assert_eq!(table_order(&books, &library, sort), vec![1, 2, 0], "most time first; a tie goes to the most recently read");
        let by_title = sort.clicked(Column::Title);
        assert!(!by_title.descending);
        assert_eq!(table_order(&books, &library, by_title), vec![0, 1, 2], "alpha, Beta, then c's library title Gamma");
        assert_eq!(table_order(&books, &library, by_title.clicked(Column::Title)), vec![2, 1, 0]);
        assert_eq!(table_order(&books, &library, sort.clicked(Column::Pace)), vec![0, 2, 1], "no pages: last either way");
        assert_eq!(table_order(&books, &library, sort.clicked(Column::Progress)), vec![2, 0, 1], "not in the library: last");
        assert_eq!(library_entry(&[entry_c], "c").map(|book| book.title.as_str()), Some("Gamma"));
    }
}
