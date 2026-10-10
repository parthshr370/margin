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
const SECS_PER_DAY: i64 = 86_400;
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

/// Time read on one local day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DayTotal {
    /// Days since 1970-01-01 (local calendar).
    pub day: i64,
    pub seconds: i64,
}

/// Time read per book, most recent first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookTotal {
    pub book: String,
    pub title: String,
    pub seconds: i64,
    pub pages: u32,
    /// Last page read (Unix seconds).
    pub last: i64,
}

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

    /// Time read per local day from `first_day` on (days without reading are left out).
    pub fn days_since(&self, first_day: i64) -> Result<Vec<DayTotal>, String> {
        // A day's local midnight is within a day of its UTC midnight: filter wide, then exactly.
        let from = first_day.saturating_sub(1).saturating_mul(SECS_PER_DAY);
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT date(start, 'unixepoch', 'localtime') AS day, SUM(seconds) FROM page_times
                 WHERE start >= ?1 GROUP BY day ORDER BY day",
            )
            .map_err(db_err)?;
        let rows = stmt.query_map([from], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))).map_err(db_err)?;
        let mut days = Vec::new();
        for row in rows {
            let (date, seconds) = row.map_err(db_err)?;
            if let Some(day) = day_number(&date).filter(|day| *day >= first_day) {
                days.push(DayTotal { day, seconds });
            }
        }
        Ok(days)
    }

    /// Every book read, most recently read first.
    pub fn book_totals(&self) -> Result<Vec<BookTotal>, String> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT p.book, COALESCE(b.title, ''), SUM(p.seconds), COUNT(DISTINCT p.page), MAX(p.start + p.seconds) AS last
                 FROM page_times p LEFT JOIN books b ON b.book = p.book
                 GROUP BY p.book ORDER BY last DESC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(BookTotal {
                    book: row.get(0)?,
                    title: row.get(1)?,
                    seconds: row.get(2)?,
                    pages: u32::try_from(row.get::<_, i64>(3)?).unwrap_or(u32::MAX),
                    last: row.get(4)?,
                })
            })
            .map_err(db_err)?;
        rows.map(|row| row.map_err(db_err)).collect()
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

/// Days since 1970-01-01 of a `YYYY-MM-DD` date (Howard Hinnant's `days_from_civil`).
pub fn day_number(date: &str) -> Option<i64> {
    let mut parts = date.splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
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
        let today = db.local_day(now as i64).unwrap();
        let days = db.days_since(today - 365).unwrap();
        assert_eq!(days, vec![DayTotal { day: today, seconds: 40 }]);
        let totals = db.book_totals().unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!((totals[0].title.as_str(), totals[0].seconds, totals[0].pages), ("A, \"quoted\" title", 40, 1));
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
}
