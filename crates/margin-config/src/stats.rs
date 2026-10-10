//! `<data>/stats.sqlite`: reading sessions and time per page, KOReader's statistics plugin in
//! two tables. A book is its sidecar fingerprint (KOReader's partial MD5), so a moved or renamed
//! file keeps its history.
//!
//! [`Recorder`] turns what the reader shows, frame by frame, into rows: a page visit starts when
//! the page shows and ends when another page shows, the document closes or the reader goes idle
//! ([`IDLE_SECS`] without input: the time after the last input is not counted). Visits shorter
//! than [`MIN_PAGE_SECS`] are flipping, not reading, and are dropped (KOReader's
//! `DEFAULT_MIN_READ_SEC`). A session is the run of visits of one book without an idle gap.
//!
//! Days are local days, computed by SQLite's `localtime`.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::library::configure;

/// The stats database inside `<data>`.
pub const STATS_DB: &str = "stats.sqlite";
/// Without input for this long the reader is away: the visit and the session end.
pub const IDLE_SECS: f64 = 5.0 * 60.0;
/// Shorter page visits are not recorded.
pub const MIN_PAGE_SECS: f64 = 5.0;
const SCHEMA_VERSION: i64 = 1;
/// A local day's first moment, for [`StatsDb::local_unix`].
const MIDNIGHT: &str = "00:00:00";
const MONTHS_PER_YEAR: i64 = 12;
/// 1970-01-01 was a Thursday: day 0 is weekday 3 counting from Monday.
const EPOCH_WEEKDAY: i64 = 3;
const DAYS_PER_WEEK: i64 = 7;
/// Longest title kept for a book (from the PDF, untrusted).
const MAX_TITLE_CHARS: usize = 512;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS books (
    book TEXT PRIMARY KEY,
    title TEXT NOT NULL DEFAULT '',
    pages INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS sessions (
    id INTEGER PRIMARY KEY,
    book TEXT NOT NULL,
    start INTEGER NOT NULL,
    end INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_book ON sessions(book);
CREATE TABLE IF NOT EXISTS page_times (
    id INTEGER PRIMARY KEY,
    book TEXT NOT NULL,
    page INTEGER NOT NULL,
    start INTEGER NOT NULL,
    seconds INTEGER NOT NULL,
    session INTEGER REFERENCES sessions(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS page_times_book ON page_times(book);
CREATE INDEX IF NOT EXISTS page_times_start ON page_times(start);
";

fn db_err(err: rusqlite::Error) -> String {
    format!("stats database: {err}")
}

/// One book's reading so far.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BookSummary {
    /// Time read, seconds.
    pub seconds: i64,
    /// Distinct pages read.
    pub pages: u32,
    pub sessions: u32,
}

impl BookSummary {
    /// Average seconds per page read; `None` before any page.
    pub fn seconds_per_page(&self) -> Option<f64> {
        (self.pages > 0).then(|| self.seconds as f64 / f64::from(self.pages))
    }

    /// Estimated seconds to read `pages_left` more pages at this pace.
    pub fn seconds_left(&self, pages_left: u32) -> Option<f64> {
        self.seconds_per_page().map(|pace| pace * f64::from(pages_left))
    }
}

/// One local day's reading: its time, the distinct pages read (a page of a book counts once,
/// however often it was visited) and the time per book.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DayStats {
    /// Days since 1970-01-01 (local calendar).
    pub day: i64,
    pub seconds: i64,
    pub pages: u32,
    /// The books read that day, most time first.
    pub books: Vec<DayBook>,
}

/// One book's time on one day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayBook {
    pub book: String,
    pub title: String,
    pub seconds: i64,
}

/// The reading of an interval of time: page visits starting in it, each counted only up to the
/// interval's end, and the distinct books they belong to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Interval {
    pub seconds: i64,
    pub books: u32,
}

/// Runs of consecutive local days with reading, over all history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Streaks {
    /// The run ending today, or yesterday while today has no reading yet.
    pub current: u32,
    pub longest: u32,
}

/// One book's reading in a period.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookTotal {
    pub book: String,
    pub title: String,
    pub seconds: i64,
    /// Distinct pages read in the period.
    pub pages: u32,
    /// The end of the last page visit (Unix seconds).
    pub last: i64,
    /// [`Self::last`] as local `YYYY-MM-DD HH:MM:SS`.
    pub last_local: String,
}

/// Everything the Stats sheet shows, read at one captured `now` (plan/R10-SURFACES.md §5).
/// Visits belong to the local date they start on, everywhere: the calendar, the cards, the
/// table and the CSV.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overview {
    /// Unix seconds.
    pub now: i64,
    /// Today's day number (local calendar).
    pub today: i64,
    /// The period's first day: today minus [`PERIOD_MONTHS`] calendar months.
    pub first: i64,
    /// The days from `first` to `today` with reading, oldest first.
    pub days: Vec<DayStats>,
    /// Today so far, and the same weekday last week up to the same local clock time.
    pub today_so_far: Interval,
    pub today_last_week: Interval,
    /// This week (from Monday) so far, and last week over the same elapsed local time.
    pub week_so_far: Interval,
    pub last_week_to_date: Interval,
    /// The books read in the period, most time first.
    pub books: Vec<BookTotal>,
    /// Over all history, not only the period.
    pub streaks: Streaks,
    /// Page visits recorded, all time: zero means there is no history at all.
    pub visits: usize,
}

impl Overview {
    /// The reading on `day`, if any.
    pub fn day(&self, day: i64) -> Option<&DayStats> {
        self.days.binary_search_by_key(&day, |stats| stats.day).ok().and_then(|index| self.days.get(index))
    }

    /// Seconds read on `day`.
    pub fn seconds_on(&self, day: i64) -> i64 {
        self.day(day).map_or(0, |stats| stats.seconds)
    }

    /// Seconds read in the period.
    pub fn period_seconds(&self) -> i64 {
        self.days.iter().map(|stats| stats.seconds).sum()
    }

    /// The most seconds read on one day of the period (0 without reading).
    pub fn busiest(&self) -> i64 {
        self.days.iter().map(|stats| stats.seconds).max().unwrap_or(0)
    }

    /// This week's Monday.
    pub fn monday(&self) -> i64 {
        self.today - i64::from(weekday(self.today))
    }
}

/// The Stats sheet's period: the last 12 calendar months.
pub const PERIOD_MONTHS: u32 = 12;
/// Heat levels above "no reading": a day's share of the period's busiest day, in quarters.
pub const HEAT_LEVELS: i64 = 4;

/// `<data>/stats.sqlite`, opened.
pub struct StatsDb {
    conn: Connection,
}

impl StatsDb {
    /// Opens (and creates) `<data>/stats.sqlite`.
    pub fn open(data: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(data).map_err(|err| format!("can't create {}: {err}", data.display()))?;
        let conn = Connection::open(data.join(STATS_DB)).map_err(db_err)?;
        configure(&conn)?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(db_err)?;
        if version > SCHEMA_VERSION {
            return Err(format!("stats database: written by a newer Margin (schema {version}); not changed"));
        }
        conn.execute_batch(SCHEMA).map_err(db_err)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION).map_err(db_err)?;
        Ok(Self { conn })
    }

    /// Remembers the title and page count of `book`, for the stats sheet and the CSV.
    pub fn note_book(&self, book: &str, title: &str, pages: u32) -> Result<(), String> {
        let title: String = title.trim().chars().filter(|c| !c.is_control()).take(MAX_TITLE_CHARS).collect();
        self.conn
            .execute(
                "INSERT INTO books (book, title, pages) VALUES (?1, ?2, ?3)
                 ON CONFLICT(book) DO UPDATE SET title = excluded.title, pages = excluded.pages",
                params![book, title, pages],
            )
            .map_err(db_err)?;
        Ok(())
    }

    fn start_session(&self, book: &str, start: i64, end: i64) -> Result<i64, String> {
        self.conn.execute("INSERT INTO sessions (book, start, end) VALUES (?1, ?2, ?3)", params![book, start, end]).map_err(db_err)?;
        Ok(self.conn.last_insert_rowid())
    }

    fn extend_session(&self, id: i64, end: i64) -> Result<(), String> {
        self.conn.execute("UPDATE sessions SET end = MAX(end, ?2) WHERE id = ?1", params![id, end]).map_err(db_err)?;
        Ok(())
    }

    fn add_page_time(&self, visit: &FinishedVisit<'_>, session: i64) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO page_times (book, page, start, seconds, session) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![visit.book, visit.page, visit.start, visit.seconds, session],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// `book`'s time, pages and sessions.
    pub fn summary(&self, book: &str) -> Result<BookSummary, String> {
        let (seconds, pages): (i64, i64) = self
            .conn
            .query_row("SELECT COALESCE(SUM(seconds), 0), COUNT(DISTINCT page) FROM page_times WHERE book = ?1", [book], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(db_err)?;
        let sessions: i64 = self.conn.query_row("SELECT COUNT(*) FROM sessions WHERE book = ?1", [book], |row| row.get(0)).map_err(db_err)?;
        Ok(BookSummary { seconds, pages: u32::try_from(pages).unwrap_or(u32::MAX), sessions: u32::try_from(sessions).unwrap_or(u32::MAX) })
    }

    /// Page visits recorded for `book`.
    pub fn page_time_count(&self, book: &str) -> Result<usize, String> {
        let count: i64 = self.conn.query_row("SELECT COUNT(*) FROM page_times WHERE book = ?1", [book], |row| row.get(0)).map_err(db_err)?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// Sessions recorded for `book`.
    pub fn session_count(&self, book: &str) -> Result<usize, String> {
        let count: i64 = self.conn.query_row("SELECT COUNT(*) FROM sessions WHERE book = ?1", [book], |row| row.get(0)).map_err(db_err)?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// The local day `unix` falls on.
    pub fn local_day(&self, unix: i64) -> Result<i64, String> {
        let date: String = self.conn.query_row("SELECT date(?1, 'unixepoch', 'localtime')", [unix], |row| row.get(0)).map_err(db_err)?;
        day_number(&date).ok_or_else(|| format!("stats database: unexpected date {date}"))
    }

    /// The Unix time of the local clock time `clock` (`HH:MM:SS`) on `day`. Uses the local
    /// calendar, so a day across a daylight-saving change is 23 or 25 hours long.
    pub fn local_unix(&self, day: i64, clock: &str) -> Result<i64, String> {
        let (year, month, date) = civil(day);
        let local = format!("{year:04}-{month:02}-{date:02} {clock}");
        let unix: Option<i64> =
            self.conn.query_row("SELECT CAST(strftime('%s', ?1, 'utc') AS INTEGER)", [&local], |row| row.get(0)).map_err(db_err)?;
        unix.ok_or_else(|| format!("stats database: unexpected local time {local}"))
    }

    /// The local clock time of `unix`, as `HH:MM:SS`.
    fn local_clock(&self, unix: i64) -> Result<String, String> {
        self.conn.query_row("SELECT time(?1, 'unixepoch', 'localtime')", [unix], |row| row.get(0)).map_err(db_err)
    }

    /// The reading of the page visits starting in `from..to` (Unix seconds), each counted only
    /// up to `to`, so a to-date comparison never includes time after its end.
    pub fn interval(&self, from: i64, to: i64) -> Result<Interval, String> {
        let (seconds, books): (i64, i64) = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(MIN(seconds, ?2 - start)), 0), COUNT(DISTINCT book) FROM page_times
                 WHERE start >= ?1 AND start < ?2",
                params![from, to],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(db_err)?;
        Ok(Interval { seconds, books: u32::try_from(books).unwrap_or(u32::MAX) })
    }

    /// Each local day's reading from `from` (Unix seconds, a local midnight) to `last_day`. A
    /// day's pages are its distinct (book, page) pairs: each book's distinct pages, summed.
    fn days_from(&self, from: i64, last_day: i64) -> Result<Vec<DayStats>, String> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT date(p.start, 'unixepoch', 'localtime') AS day, p.book, COALESCE(b.title, ''), SUM(p.seconds),
                        COUNT(DISTINCT p.page)
                 FROM page_times p LEFT JOIN books b ON b.book = p.book
                 WHERE p.start >= ?1 GROUP BY day, p.book ORDER BY day",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([from], |row| {
                let book = DayBook { book: row.get(1)?, title: row.get(2)?, seconds: row.get(3)? };
                Ok((row.get::<_, String>(0)?, book, row.get::<_, i64>(4)?))
            })
            .map_err(db_err)?;
        let mut days: Vec<DayStats> = Vec::new();
        for row in rows {
            let (date, book, pages) = row.map_err(db_err)?;
            let Some(day) = day_number(&date).filter(|day| *day <= last_day) else { continue };
            if days.last().is_none_or(|stats| stats.day != day) {
                days.push(DayStats { day, ..DayStats::default() });
            }
            if let Some(stats) = days.last_mut() {
                stats.seconds = stats.seconds.saturating_add(book.seconds);
                stats.pages = stats.pages.saturating_add(u32::try_from(pages).unwrap_or(u32::MAX));
                stats.books.push(book);
            }
        }
        for stats in &mut days {
            stats.books.sort_by(|a, b| b.seconds.cmp(&a.seconds).then_with(|| a.title.cmp(&b.title)).then_with(|| a.book.cmp(&b.book)));
        }
        Ok(days)
    }

    /// The local days with reading, all history, oldest first.
    fn active_days(&self) -> Result<Vec<i64>, String> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT DISTINCT date(start, 'unixepoch', 'localtime') AS day FROM page_times WHERE seconds > 0 ORDER BY day")
            .map_err(db_err)?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).map_err(db_err)?;
        let mut days = Vec::new();
        for row in rows {
            if let Some(day) = day_number(&row.map_err(db_err)?) {
                days.push(day);
            }
        }
        Ok(days)
    }

    /// The books read from `from` (Unix seconds) on: most time first, then most recently read,
    /// then by title and key.
    pub fn book_totals_since(&self, from: i64) -> Result<Vec<BookTotal>, String> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT p.book, COALESCE(b.title, '') AS title, SUM(p.seconds) AS total, COUNT(DISTINCT p.page),
                        MAX(p.start + p.seconds) AS last, datetime(MAX(p.start + p.seconds), 'unixepoch', 'localtime')
                 FROM page_times p LEFT JOIN books b ON b.book = p.book
                 WHERE p.start >= ?1
                 GROUP BY p.book ORDER BY total DESC, last DESC, title, p.book",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([from], |row| {
                Ok(BookTotal {
                    book: row.get(0)?,
                    title: row.get(1)?,
                    seconds: row.get(2)?,
                    pages: u32::try_from(row.get::<_, i64>(3)?).unwrap_or(u32::MAX),
                    last: row.get(4)?,
                    last_local: row.get(5)?,
                })
            })
            .map_err(db_err)?;
        rows.map(|row| row.map_err(db_err)).collect()
    }

    /// Page visits recorded, all books, all time.
    pub fn visit_count(&self) -> Result<usize, String> {
        let count: i64 = self.conn.query_row("SELECT COUNT(*) FROM page_times", [], |row| row.get(0)).map_err(db_err)?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// The Stats sheet's numbers at `now` (Unix seconds), in one read.
    pub fn overview(&self, now: i64) -> Result<Overview, String> {
        let today = self.local_day(now)?;
        let first = months_back(today, PERIOD_MONTHS);
        let monday = today - i64::from(weekday(today));
        let last_week = today - DAYS_PER_WEEK;
        // Last week's moment matching now: the same local clock time, seven calendar days back.
        let last_week_now = self.local_unix(last_week, &self.local_clock(now)?)?;
        let period_start = self.local_unix(first, MIDNIGHT)?;
        Ok(Overview {
            now,
            today,
            first,
            days: self.days_from(period_start, today)?,
            today_so_far: self.interval(self.local_unix(today, MIDNIGHT)?, now)?,
            today_last_week: self.interval(self.local_unix(last_week, MIDNIGHT)?, last_week_now)?,
            week_so_far: self.interval(self.local_unix(monday, MIDNIGHT)?, now)?,
            last_week_to_date: self.interval(self.local_unix(monday - DAYS_PER_WEEK, MIDNIGHT)?, last_week_now)?,
            books: self.book_totals_since(period_start)?,
            streaks: streaks(&self.active_days()?, today),
            visits: self.visit_count()?,
        })
    }

    /// Every page visit as CSV: `date,time,book,title,page,seconds` (local date and time, page
    /// 1-based), oldest first.
    pub fn export_csv(&self) -> Result<String, String> {
        let mut out = String::from("date,time,book,title,page,seconds\n");
        let mut stmt = self
            .conn
            .prepare(
                "SELECT date(p.start, 'unixepoch', 'localtime'), time(p.start, 'unixepoch', 'localtime'), p.book,
                        COALESCE(b.title, ''), p.page, p.seconds
                 FROM page_times p LEFT JOIN books b ON b.book = p.book ORDER BY p.start, p.id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(db_err)?;
        for row in rows {
            let (date, time, book, title, page, seconds) = row.map_err(db_err)?;
            let page = page.saturating_add(1);
            out.push_str(&format!("{date},{time},{},{},{page},{seconds}\n", csv_field(&book), csv_field(&title)));
        }
        Ok(out)
    }

    /// The last session's end for `book` (Unix seconds).
    pub fn last_read(&self, book: &str) -> Result<Option<i64>, String> {
        self.conn
            .query_row("SELECT MAX(end) FROM sessions WHERE book = ?1", [book], |row| row.get::<_, Option<i64>>(0))
            .optional()
            .map(Option::flatten)
            .map_err(db_err)
    }
}

/// A CSV field, quoted when it has a comma, quote or line break (RFC 4180).
fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) { format!("\"{}\"", text.replace('"', "\"\"")) } else { text.to_owned() }
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` date.
pub fn day_number(date: &str) -> Option<i64> {
    let mut parts = date.splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if !(1..=MONTHS_PER_YEAR).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// Days since 1970-01-01 of a valid date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `day` minus `months` calendar months, the date clamped to the earlier month's length (31
/// March minus one month is the last day of February).
pub fn months_back(day: i64, months: u32) -> i64 {
    let (year, month, date) = civil(day);
    let index = year * MONTHS_PER_YEAR + i64::from(month) - 1 - i64::from(months);
    let (year, month) = (index.div_euclid(MONTHS_PER_YEAR), index.rem_euclid(MONTHS_PER_YEAR) + 1);
    let first = days_from_civil(year, month, 1);
    let next = if month == MONTHS_PER_YEAR { days_from_civil(year + 1, 1, 1) } else { days_from_civil(year, month + 1, 1) };
    first + (i64::from(date) - 1).min(next - first - 1)
}

/// A day's heat level, 0 (no reading) to [`HEAT_LEVELS`]: its share of `busiest` in quarters,
/// rounded up, so (0, 25 %] is level 1 and (75 %, 100 %] level 4.
pub fn heat_level(seconds: i64, busiest: i64) -> u8 {
    if seconds <= 0 || busiest <= 0 {
        return 0;
    }
    let (seconds, busiest, levels) = (i128::from(seconds), i128::from(busiest), i128::from(HEAT_LEVELS));
    let level = (seconds * levels + busiest - 1) / busiest;
    u8::try_from(level.clamp(1, levels)).unwrap_or(0)
}

/// The streaks in `active` (days with reading, ascending): the longest run of consecutive
/// days, and the run ending today, or yesterday while today has nothing yet. A missing day
/// breaks a run; days after `today` are ignored.
pub fn streaks(active: &[i64], today: i64) -> Streaks {
    let (mut run, mut longest, mut previous) = (0u32, 0u32, None::<i64>);
    for &day in active.iter().filter(|day| **day <= today) {
        run = match previous {
            Some(last) if last == day => run,
            Some(last) if last + 1 == day => run.saturating_add(1),
            _ => 1,
        };
        longest = longest.max(run);
        previous = Some(day);
    }
    let current = match previous {
        Some(last) if last == today || last + 1 == today => run,
        _ => 0,
    };
    Streaks { current, longest }
}

/// (year, month, day) of a day number (Howard Hinnant's `civil_from_days`).
pub fn civil(day: i64) -> (i64, u32, u32) {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(m <= 2);
    (year, u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

/// 0 = Monday … 6 = Sunday.
pub fn weekday(day: i64) -> u32 {
    u32::try_from((day + EPOCH_WEEKDAY).rem_euclid(DAYS_PER_WEEK)).unwrap_or(0)
}

/// "2h 14m", "14m", "45s".
pub fn format_duration(seconds: f64) -> String {
    let total = if seconds.is_finite() { seconds.max(0.0).round() as i64 } else { 0 };
    let (hours, minutes) = (total / 3600, total % 3600 / 60);
    match (hours, minutes) {
        (0, 0) => format!("{total}s"),
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// What the reader shows this frame: a page of a book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Showing<'a> {
    pub book: &'a str,
    pub page: u32,
}

#[derive(Clone, Debug)]
struct Visit {
    book: String,
    page: u32,
    since: f64,
    last_input: f64,
}

#[derive(Clone, Debug)]
struct OpenSession {
    book: String,
    id: i64,
}

struct FinishedVisit<'a> {
    book: &'a str,
    page: u32,
    start: i64,
    seconds: i64,
}

/// Turns frames into page visits and sessions (see the module docs).
#[derive(Debug, Default)]
pub struct Recorder {
    visit: Option<Visit>,
    session: Option<OpenSession>,
    /// Idle since the last cut: no visit starts until the next input.
    idle: bool,
}

impl Recorder {
    /// One frame at `now` (Unix seconds): the reader shows `showing` (`None`: no document, the
    /// library, or the window lost focus) and there was input this frame when `input`. Returns
    /// whether a row was written.
    pub fn step(&mut self, db: &StatsDb, now: f64, showing: Option<Showing<'_>>, input: bool) -> Result<bool, String> {
        let mut wrote = false;
        if let Some(visit) = &self.visit
            && now - visit.last_input > IDLE_SECS
        {
            let end = visit.last_input;
            wrote |= self.end_visit(db, end)?;
            self.session = None;
            self.idle = true;
        }
        if input {
            self.idle = false;
            if let Some(visit) = &mut self.visit {
                visit.last_input = now;
            }
        }
        let same = match (&self.visit, showing) {
            (Some(visit), Some(showing)) => visit.book == showing.book && visit.page == showing.page,
            (None, None) => true,
            _ => false,
        };
        if same {
            return Ok(wrote);
        }
        wrote |= self.end_visit(db, now)?;
        match showing {
            Some(showing) => {
                if self.session.as_ref().is_some_and(|session| session.book != showing.book) {
                    self.session = None;
                }
                if !self.idle {
                    self.visit = Some(Visit { book: showing.book.to_owned(), page: showing.page, since: now, last_input: now });
                }
            }
            None => self.session = None,
        }
        Ok(wrote)
    }

    /// Ends the visit and the session now (the window is closing).
    pub fn stop(&mut self, db: &StatsDb, now: f64) -> Result<bool, String> {
        let wrote = self.end_visit(db, now)?;
        self.session = None;
        Ok(wrote)
    }

    /// The book of the visit in progress.
    pub fn reading(&self) -> Option<&str> {
        self.visit.as_ref().map(|visit| visit.book.as_str())
    }

    fn end_visit(&mut self, db: &StatsDb, end: f64) -> Result<bool, String> {
        let Some(visit) = self.visit.take() else { return Ok(false) };
        let seconds = end - visit.since;
        if !seconds.is_finite() || seconds < MIN_PAGE_SECS || !visit.since.is_finite() {
            return Ok(false);
        }
        let start = visit.since.floor() as i64;
        let finished = FinishedVisit { book: &visit.book, page: visit.page, start, seconds: seconds.round() as i64 };
        let end = start.saturating_add(finished.seconds);
        let session = match &self.session {
            Some(session) if session.book == visit.book => {
                db.extend_session(session.id, end)?;
                session.id
            }
            _ => {
                let id = db.start_session(&visit.book, start, end)?;
                self.session = Some(OpenSession { book: visit.book.clone(), id });
                id
            }
        };
        db.add_page_time(&finished, session)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::tests::temp_dir;

    fn showing(book: &str, page: u32) -> Option<Showing<'_>> {
        Some(Showing { book, page })
    }

    #[test]
    fn reading_three_pages_logs_three_rows_and_one_session() {
        let dir = temp_dir("stats-three");
        let db = StatsDb::open(&dir).unwrap();
        let mut rec = Recorder::default();
        let t0 = 1_760_000_000.0;
        for (i, page) in [0, 1, 2, 3].into_iter().enumerate() {
            rec.step(&db, t0 + 30.0 * i as f64, showing("b", page), true).unwrap();
        }
        assert_eq!(db.page_time_count("b").unwrap(), 3);
        assert_eq!(db.session_count("b").unwrap(), 1);
        let summary = db.summary("b").unwrap();
        assert_eq!((summary.seconds, summary.pages, summary.sessions), (90, 3, 1));
        assert_eq!(summary.seconds_per_page(), Some(30.0));
        assert_eq!(summary.seconds_left(10), Some(300.0));
        rec.stop(&db, t0 + 100.0).unwrap();
        assert_eq!(db.page_time_count("b").unwrap(), 4);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn short_visits_are_dropped_and_idle_ends_the_session() {
        let dir = temp_dir("stats-idle");
        let db = StatsDb::open(&dir).unwrap();
        let mut rec = Recorder::default();
        let t0 = 1_760_000_000.0;
        rec.step(&db, t0, showing("b", 0), true).unwrap();
        rec.step(&db, t0 + 2.0, showing("b", 1), true).unwrap();
        assert_eq!(db.page_time_count("b").unwrap(), 0, "a 2 s glance is not reading");
        rec.step(&db, t0 + 62.0, None, false).unwrap();
        assert_eq!(db.page_time_count("b").unwrap(), 1);
        // Away from the keyboard: only time up to the last input counts.
        rec.step(&db, t0 + 100.0, showing("b", 2), true).unwrap();
        rec.step(&db, t0 + 160.0, showing("b", 2), true).unwrap();
        rec.step(&db, t0 + 160.0 + IDLE_SECS + 1.0, showing("b", 2), false).unwrap();
        assert_eq!(db.summary("b").unwrap().seconds, 60 + 60);
        // Still idle an hour later: nothing more is counted until there is input again.
        rec.step(&db, t0 + 3600.0, showing("b", 2), false).unwrap();
        rec.step(&db, t0 + 3700.0, showing("b", 2), true).unwrap();
        rec.step(&db, t0 + 3760.0, showing("b", 3), true).unwrap();
        assert_eq!(db.summary("b").unwrap().seconds, 60 + 60 + 60);
        assert_eq!(db.session_count("b").unwrap(), 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn days_totals_and_csv() {
        let dir = temp_dir("stats-days");
        let db = StatsDb::open(&dir).unwrap();
        db.note_book("b", "A, \"quoted\" title", 10).unwrap();
        let mut rec = Recorder::default();
        let now = crate::library::now_unix() as f64;
        rec.step(&db, now - 40.0, showing("b", 0), true).unwrap();
        rec.stop(&db, now).unwrap();
        let overview = db.overview(now as i64).unwrap();
        let today = overview.today;
        assert_eq!(overview.seconds_on(today), 40);
        assert_eq!(overview.visits, 1);
        let totals = &overview.books;
        assert_eq!(totals.len(), 1);
        assert_eq!((totals[0].title.as_str(), totals[0].seconds, totals[0].pages), ("A, \"quoted\" title", 40, 1));
        assert_eq!(totals[0].last_local.len(), "2026-10-10 12:00:00".len());
        let csv = db.export_csv().unwrap();
        let mut lines = csv.lines();
        assert_eq!(lines.next(), Some("date,time,book,title,page,seconds"));
        assert!(lines.next().unwrap().ends_with(",b,\"A, \"\"quoted\"\" title\",1,40"));
        assert!(db.last_read("b").unwrap().is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn calendar_arithmetic() {
        assert_eq!(day_number("1970-01-01"), Some(0));
        assert_eq!(day_number("2026-10-10"), Some(20_736));
        assert_eq!(civil(20_736), (2026, 10, 10));
        assert_eq!(weekday(20_736), 5, "2026-10-10 is a Saturday");
        assert_eq!(weekday(0), 3);
        assert_eq!(day_number("2026-13-01"), None);
        assert_eq!(day_number("garbage"), None);
        for day in [-1, 0, 59, 11_016, 20_736, 2_932_896] {
            let (y, m, d) = civil(day);
            assert_eq!(day_number(&format!("{y:04}-{m:02}-{d:02}")), Some(day));
        }
        assert_eq!(format_duration(8040.0), "2h 14m");
        assert_eq!(format_duration(45.0), "45s");
        assert_eq!(format_duration(3600.0), "1h");
        assert_eq!(format_duration(f64::NAN), "0s");
    }

    /// One page visit of `seconds` from `start`, through the real recorder.
    fn visit(db: &StatsDb, book: &str, page: u32, start: i64, seconds: i64) {
        let mut recorder = Recorder::default();
        recorder.step(db, start as f64, showing(book, page), true).unwrap();
        recorder.stop(db, (start + seconds) as f64).unwrap();
    }

    #[test]
    fn overview_reads_days_comparisons_books_and_streaks() {
        let dir = temp_dir("stats-overview");
        let db = StatsDb::open(&dir).unwrap();
        db.note_book("a", "Alpha", 10).unwrap();
        db.note_book("b", "Beta", 10).unwrap();
        let today = db.local_day(crate::library::now_unix()).unwrap();
        let at = |day: i64, clock: &str| db.local_unix(day, clock).unwrap();
        // Today: pages 0 and 1, page 0 again (time counts, the page once).
        visit(&db, "a", 0, at(today, "10:00:00"), 600);
        visit(&db, "a", 1, at(today, "10:30:00"), 300);
        visit(&db, "a", 0, at(today, "11:00:00"), 60);
        visit(&db, "b", 0, at(today - 1, "12:00:00"), 1200);
        visit(&db, "a", 2, at(today - 2, "09:00:00"), 600);
        // A week ago: before noon, across noon (counted up to noon only) and after noon.
        visit(&db, "b", 1, at(today - 7, "09:00:00"), 300);
        visit(&db, "b", 2, at(today - 7, "11:58:00"), 600);
        visit(&db, "b", 3, at(today - 7, "13:00:00"), 600);
        for day in today - 40..=today - 37 {
            visit(&db, "a", 5, at(day, "08:00:00"), 60);
        }
        // Before the period: in the streaks and the visit count only.
        let first = months_back(today, PERIOD_MONTHS);
        visit(&db, "c", 0, at(first - 1, "08:00:00"), 600);

        let overview = db.overview(at(today, "12:00:00")).unwrap();
        assert_eq!((overview.today, overview.first), (today, first));
        assert_eq!(overview.today_so_far, Interval { seconds: 960, books: 1 });
        assert_eq!(overview.today_last_week, Interval { seconds: 300 + 120, books: 1 });
        assert_eq!(overview.last_week_to_date, Interval { seconds: 300 + 120, books: 1 });
        let weekday = weekday(today);
        let week = 960 + if weekday >= 1 { 1200 } else { 0 } + if weekday >= 2 { 600 } else { 0 };
        assert_eq!(overview.week_so_far.seconds, week);
        let day = overview.day(today).unwrap();
        assert_eq!((day.seconds, day.pages), (960, 2));
        assert_eq!(day.books, vec![DayBook { book: "a".into(), title: "Alpha".into(), seconds: 960 }]);
        assert_eq!(overview.seconds_on(today - 7), 1500, "the calendar counts whole visits");
        assert_eq!(overview.days.len(), 8);
        assert_eq!(overview.period_seconds(), 960 + 1200 + 600 + 1500 + 240);
        assert_eq!(overview.busiest(), 1500);
        let books: Vec<(&str, i64, u32)> = overview.books.iter().map(|book| (book.title.as_str(), book.seconds, book.pages)).collect();
        assert_eq!(books, vec![("Beta", 2700, 4), ("Alpha", 1800, 4)], "most time first; c is before the period");
        assert_eq!(overview.streaks, Streaks { current: 3, longest: 4 });
        assert_eq!(overview.visits, 13);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn heat_levels_months_back_and_streaks() {
        let levels: Vec<u8> = [0, 1, 25, 26, 50, 51, 75, 76, 100, 200].into_iter().map(|seconds| heat_level(seconds, 100)).collect();
        assert_eq!(levels, vec![0, 1, 1, 2, 2, 3, 3, 4, 4, 4]);
        assert_eq!(heat_level(5, 0), 0, "no busiest day: everything is level 0");
        assert_eq!(heat_level(i64::MAX, i64::MAX), 4);
        let day = |date: &str| day_number(date).unwrap();
        assert_eq!(months_back(day("2026-10-10"), 12), day("2025-10-10"));
        assert_eq!(months_back(day("2024-02-29"), 12), day("2023-02-28"));
        assert_eq!(months_back(day("2026-03-31"), 1), day("2026-02-28"));
        assert_eq!(months_back(day("2026-01-15"), 1), day("2025-12-15"));
        assert_eq!(months_back(day("2026-12-31"), 12), day("2025-12-31"));
        assert_eq!(streaks(&[], 10), Streaks::default());
        assert_eq!(streaks(&[8, 9, 10], 10), Streaks { current: 3, longest: 3 });
        assert_eq!(streaks(&[8, 9], 10), Streaks { current: 2, longest: 2 }, "today not read yet keeps yesterday's run");
        assert_eq!(streaks(&[7, 8], 10), Streaks { current: 0, longest: 2 });
        assert_eq!(streaks(&[1, 2, 3, 4, 8, 9, 10], 10), Streaks { current: 3, longest: 4 });
        assert_eq!(streaks(&[10, 11, 12], 10), Streaks { current: 1, longest: 1 }, "future days are ignored");
        assert_eq!(streaks(&[5, 5, 6], 6), Streaks { current: 2, longest: 2 });
    }
}
