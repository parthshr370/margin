//! The Stats sheet (`app.stats`, Ctrl+Shift+T): totals, a calendar heatmap of the last 12
//! months, this week's time per day, the books read most recently, and "Export CSV" (every page
//! visit). Read from `stats.sqlite` when the sheet opens.

use egui::{Align2, CornerRadius, Rect, RichText, Sense, Stroke, Vec2, pos2, vec2};
use margin_config::stats::{BookTotal, DayTotal, civil, format_duration, weekday};

use crate::PdfCraftApp;
use crate::theme::{self, Tokens};

/// Weeks the calendar shows (a year and the week today is in).
pub const WEEKS: i64 = 53;
const DAYS_PER_WEEK: i64 = 7;
const CELL: f32 = 10.0;
const CELL_GAP: f32 = 3.0;
const MONTH_LABELS: f32 = 16.0;
const WEEKDAY_LABELS: f32 = 26.0;
const BAR_HEIGHT: f32 = 90.0;
const BAR_WIDTH: f32 = 34.0;
/// The Books list's title column.
const TITLE_WIDTH: f32 = 360.0;
const SHEET_WIDTH: f32 = 760.0;
/// Books listed under the charts.
const BOOKS_SHOWN: usize = 8;
/// Heat levels above "none": a day's share of the busiest day, in quarters.
const LEVELS: f32 = 4.0;
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// What the sheet shows, read once when it opens.
#[derive(Default)]
pub(crate) struct Sheet {
    data: Option<Data>,
}

/// The sheet's numbers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Data {
    /// Today, as a day number (local calendar).
    pub today: i64,
    /// The calendar's first day: a Monday, [`WEEKS`] weeks back.
    pub first: i64,
    /// Seconds read per day from `first` on (days without reading left out).
    pub days: Vec<DayTotal>,
    pub books: Vec<BookTotal>,
}

impl Data {
    /// Seconds read on `day`.
    pub fn seconds_on(&self, day: i64) -> i64 {
        self.days.iter().find(|total| total.day == day).map_or(0, |total| total.seconds)
    }

    /// Seconds read from `from` to `to`, both included.
    pub fn seconds_between(&self, from: i64, to: i64) -> i64 {
        self.days.iter().filter(|total| (from..=to).contains(&total.day)).map(|total| total.seconds).sum()
    }

    /// This week's Monday.
    pub fn monday(&self) -> i64 {
        self.today - i64::from(weekday(self.today))
    }
}

/// Reads the sheet's numbers for `now` (Unix seconds).
pub fn load(app: &mut PdfCraftApp, now: f64) -> Result<Data, String> {
    if !super::stats::ensure_open(app) {
        return Err("there are no reading statistics".to_owned());
    }
    let db = app.margin_stats.db().ok_or_else(|| "there are no reading statistics".to_owned())?;
    let today = db.local_day(now.floor() as i64)?;
    let monday = today - i64::from(weekday(today));
    let first = monday - (WEEKS - 1) * DAYS_PER_WEEK;
    Ok(Data { today, first, days: db.days_since(first)?, books: db.book_totals()? })
}

/// The day number as "Sat 10 Oct 2026".
fn day_label(day: i64) -> String {
    let (year, month, date) = civil(day);
    let weekday = WEEKDAYS.get(weekday(day) as usize).copied().unwrap_or("");
    let month = MONTHS.get(month.saturating_sub(1) as usize).copied().unwrap_or("");
    format!("{weekday} {date} {month} {year}")
}

pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if app.margin_stats.sheet.as_ref().is_some_and(|sheet| sheet.data.is_none()) {
        let now = app.margin_stats.now();
        let data = load(app, now).unwrap_or_else(|err| {
            log::warn!("stats: {err}");
            Data::default()
        });
        if let Some(sheet) = app.margin_stats.sheet.as_mut() {
            sheet.data = Some(data);
        }
    }
    let Some(data) = app.margin_stats.sheet.as_ref().and_then(|sheet| sheet.data.clone()) else { return };
    let t = Tokens::get(ctx);
    let enabled = app.margin_keys.settings().stats.enabled;
    let mut close = false;
    let mut export = false;
    let modal = egui::Modal::new(egui::Id::new("margin-stats")).show(ctx, |ui| {
        ui.set_width(SHEET_WIDTH);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Reading stats").font(theme::semibold(18.0)));
            if !enabled {
                ui.label(RichText::new("recording is off (stats.enabled)").color(t.text_muted));
            }
        });
        ui.add_space(12.0);
        totals(ui, &t, &data);
        ui.add_space(16.0);
        section(ui, &t, "Last 12 months");
        calendar(ui, &t, &data);
        ui.add_space(16.0);
        section(ui, &t, "This week");
        week(ui, &t, &data);
        ui.add_space(16.0);
        section(ui, &t, "Books");
        books(ui, &t, &data);
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::widgets::pill_button(ui, "Close", true).clicked() {
                close = true;
            }
            if crate::widgets::pill_button(ui, "Export CSV…", false).clicked() {
                export = true;
            }
        });
    });
    if close || modal.should_close() {
        app.margin_stats.sheet = None;
    }
    if export {
        export_dialog(app);
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

fn section(ui: &mut egui::Ui, t: &Tokens, title: &str) {
    ui.horizontal(|ui| super::look::section_label(ui, t, title, None));
    ui.add_space(6.0);
}

fn totals(ui: &mut egui::Ui, t: &Tokens, data: &Data) {
    let year = data.seconds_between(data.first, data.today);
    let cells = [
        ("Today", format_duration(data.seconds_on(data.today) as f64)),
        ("This week", format_duration(data.seconds_between(data.monday(), data.today) as f64)),
        ("12 months", format_duration(year as f64)),
        ("Books", data.books.len().to_string()),
    ];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 36.0;
        for (label, value) in cells {
            ui.vertical(|ui| {
                ui.label(RichText::new(value).font(theme::semibold(20.0)).color(t.text));
                ui.label(RichText::new(label).font(theme::regular(12.0)).color(t.text_muted));
            });
        }
    });
}

/// A day's heat colour: the hover fill for none, then the accent in four steps.
fn heat(t: &Tokens, seconds: i64, busiest: i64) -> egui::Color32 {
    if seconds <= 0 || busiest <= 0 {
        return t.hover;
    }
    let level = ((seconds as f32 / busiest as f32) * LEVELS).ceil().clamp(1.0, LEVELS);
    t.accent.gamma_multiply(0.25 + 0.75 * (level - 1.0) / (LEVELS - 1.0))
}

/// GitHub-style: a column per week (Monday on top), months above.
fn calendar(ui: &mut egui::Ui, t: &Tokens, data: &Data) {
    let step = CELL + CELL_GAP;
    let size = vec2(WEEKDAY_LABELS + WEEKS as f32 * step, MONTH_LABELS + DAYS_PER_WEEK as f32 * step);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let busiest = data.days.iter().map(|day| day.seconds).max().unwrap_or(0);
    let painter = ui.painter_at(rect.expand(2.0));
    for (row, name) in WEEKDAYS.iter().enumerate() {
        if row % 2 == 0 {
            let y = rect.top() + MONTH_LABELS + row as f32 * step + CELL / 2.0;
            painter.text(pos2(rect.left(), y), Align2::LEFT_CENTER, *name, theme::regular(10.0), t.text_muted);
        }
    }
    let mut last_month = None;
    for week in 0..WEEKS {
        let x = rect.left() + WEEKDAY_LABELS + week as f32 * step;
        let monday = data.first + week * DAYS_PER_WEEK;
        let (_, month, _) = civil(monday);
        if last_month != Some(month) {
            last_month = Some(month);
            if let Some(name) = MONTHS.get(month.saturating_sub(1) as usize) {
                painter.text(pos2(x, rect.top()), Align2::LEFT_TOP, *name, theme::regular(10.0), t.text_muted);
            }
        }
        for row in 0..DAYS_PER_WEEK {
            let day = monday + row;
            if day > data.today {
                break;
            }
            let cell = Rect::from_min_size(pos2(x, rect.top() + MONTH_LABELS + row as f32 * step), Vec2::splat(CELL));
            let seconds = data.seconds_on(day);
            painter.rect_filled(cell, CornerRadius::same(2), heat(t, seconds, busiest));
            let label = if day == data.today {
                format!("Today, {}: {}", day_label(day), format_duration(seconds as f64))
            } else {
                format!("{}: {}", day_label(day), format_duration(seconds as f64))
            };
            let resp = ui.interact(cell, ui.id().with(("stats-day", day)), Sense::hover());
            if day == data.today {
                painter.rect_stroke(cell.expand(1.0), CornerRadius::same(3), Stroke::new(1.0, t.text), egui::StrokeKind::Outside);
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &label));
            }
            super::look::tooltip(resp, &label, None);
        }
    }
}

/// Monday to Sunday of this week, minutes per day as bars.
fn week(ui: &mut egui::Ui, t: &Tokens, data: &Data) {
    let monday = data.monday();
    let seconds: Vec<i64> = (0..DAYS_PER_WEEK).map(|offset| data.seconds_on(monday + offset)).collect();
    let busiest = seconds.iter().copied().max().unwrap_or(0).max(1);
    let step = BAR_WIDTH + 14.0;
    let (rect, _) = ui.allocate_exact_size(vec2(step * DAYS_PER_WEEK as f32, BAR_HEIGHT + 34.0), Sense::hover());
    let painter = ui.painter_at(rect);
    for (offset, day_seconds) in seconds.iter().enumerate() {
        let x = rect.left() + offset as f32 * step;
        let base = rect.top() + 14.0 + BAR_HEIGHT;
        let height = (BAR_HEIGHT * *day_seconds as f32 / busiest as f32).max(if *day_seconds > 0 { 2.0 } else { 0.0 });
        let track = Rect::from_min_max(pos2(x, rect.top() + 14.0), pos2(x + BAR_WIDTH, base));
        painter.rect_filled(track, CornerRadius::same(3), t.hover);
        let bar = Rect::from_min_max(pos2(x, base - height), pos2(x + BAR_WIDTH, base));
        let today = monday + offset as i64 == data.today;
        painter.rect_filled(bar, CornerRadius::same(3), if today { t.accent } else { t.accent.gamma_multiply(0.6) });
        if *day_seconds > 0 {
            painter.text(
                pos2(x + BAR_WIDTH / 2.0, base - height - 2.0),
                Align2::CENTER_BOTTOM,
                format_duration(*day_seconds as f64),
                theme::regular(10.0),
                t.text_muted,
            );
        }
        let name = WEEKDAYS.get(offset).copied().unwrap_or("");
        painter.text(
            pos2(x + BAR_WIDTH / 2.0, base + 6.0),
            Align2::CENTER_TOP,
            name,
            theme::regular(11.0),
            if today { t.text } else { t.text_muted },
        );
    }
}

fn books(ui: &mut egui::Ui, t: &Tokens, data: &Data) {
    if data.books.is_empty() {
        ui.label(RichText::new("Nothing read yet. Time on each page is recorded as you read.").color(t.text_muted));
        return;
    }
    egui::Grid::new("margin-stats-books").num_columns(3).spacing([24.0, 6.0]).show(ui, |ui| {
        for book in data.books.iter().take(BOOKS_SHOWN) {
            let title = if book.title.is_empty() { book.book.as_str() } else { book.title.as_str() };
            ui.add_sized([TITLE_WIDTH, 18.0], egui::Label::new(RichText::new(title).color(t.text)).truncate());
            ui.label(RichText::new(format_duration(book.seconds as f64)).color(t.text_muted));
            ui.label(RichText::new(format!("{} pages", book.pages)).color(t.text_muted));
            ui.end_row();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_sums_days_and_weeks() {
        let today = 20_736; // Saturday 10 Oct 2026
        let data = Data {
            today,
            first: today - 5 - (WEEKS - 1) * DAYS_PER_WEEK,
            days: vec![DayTotal { day: today - 7, seconds: 100 }, DayTotal { day: today - 1, seconds: 60 }, DayTotal { day: today, seconds: 30 }],
            books: Vec::new(),
        };
        assert_eq!(data.monday(), today - 5);
        assert_eq!(data.seconds_on(today), 30);
        assert_eq!(data.seconds_between(data.monday(), today), 90);
        assert_eq!(data.seconds_between(data.first, today), 190);
        assert_eq!(day_label(today), "Sat 10 Oct 2026");
    }
}
