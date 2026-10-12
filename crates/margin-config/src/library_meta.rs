//! A library book's bibliographic details (title, authors, year, DOI, arXiv id, venue, ISBN,
//! abstract): what the user types in the details sheet, what an import (BibTeX, Zotero) or an
//! online lookup (Crossref, arXiv) fills in, and the background jobs that do the last two.
//!
//! Who wins: the user, then an import or a lookup, then the file. A field the user edited is
//! marked in `books.user_fields` and nothing changes it again; a field an import or a lookup
//! filled is marked in `books.source_fields` and a scan doesn't put the file's own metadata
//! back over it.
//!
//! Nothing here talks to the network except [`LookupJob`], which runs only when the user asks
//! for one book ("Look up details") or turned on `library.metadata_lookup`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params};

use crate::library::{LibraryDb, ScanProgress, ScanRequest, db_err, now_unix, open_connection, tag_id};
use crate::lookup::LookupError;
use crate::reference::{MAX_FIELD_CHARS, Reference, clean};
use crate::{bibtex, ids, zotero};

/// A book field that can be edited, imported or looked up. Its bit in `user_fields` and
/// `source_fields` is `1 << (self as u32)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Title,
    Authors,
    Year,
    Doi,
    Arxiv,
    Publisher,
    Isbn,
    Abstract,
}

impl Field {
    pub const ALL: [Self; 8] = [Self::Title, Self::Authors, Self::Year, Self::Doi, Self::Arxiv, Self::Publisher, Self::Isbn, Self::Abstract];

    pub fn bit(self) -> i64 {
        1 << (self as u32)
    }

    /// Its column in `books`.
    fn column(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Authors => "author",
            Self::Year => "year",
            Self::Doi => "doi",
            Self::Arxiv => "arxiv",
            Self::Publisher => "publisher",
            Self::Isbn => "isbn",
            Self::Abstract => "abstract",
        }
    }
}

/// How several authors are kept in `books.author`.
pub const AUTHOR_SEPARATOR: &str = ", ";
/// Years a user may type.
const YEARS: std::ops::RangeInclusive<i32> = 0..=9999;
/// Titles shorter than this (letters and digits) are too generic to match an import entry by.
const MIN_TITLE_MATCH_CHARS: usize = 8;
/// An import commits this many entries at a time, so the reader's own writes never wait long.
const IMPORT_BATCH: usize = 200;
/// Most books one automatic lookup pass asks about.
const MAX_AUTO_LOOKUPS: usize = 500;
/// The pause between two automatic lookups: arXiv asks API users to wait 3 seconds.
const AUTO_LOOKUP_PAUSE: Duration = Duration::from_secs(3);
/// How often a pause checks whether the job was cancelled.
const CANCEL_POLL: Duration = Duration::from_millis(100);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A book's details as the details sheet shows them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Details {
    pub title: String,
    /// Every author, [`AUTHOR_SEPARATOR`] between them.
    pub authors: String,
    pub year: Option<i32>,
    pub doi: Option<String>,
    pub arxiv: Option<String>,
    pub publisher: Option<String>,
    pub isbn: Option<String>,
    pub abstract_text: Option<String>,
    /// The fields the user edited ([`Field::bit`]s).
    pub user_fields: i64,
    /// When it was last looked up online (Unix seconds).
    pub looked_up: Option<i64>,
}

/// What the user typed in the details sheet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailsEdit {
    pub title: String,
    pub authors: String,
    pub year: Option<i32>,
    /// As typed: `doi:`, a doi.org link or the bare DOI; blank for none.
    pub doi: String,
    /// As typed: `arXiv:`, an arxiv.org link or the bare id; blank for none.
    pub arxiv: String,
}

/// `authors` as `books.author` keeps them.
pub fn join_authors(authors: &[String]) -> String {
    authors.join(AUTHOR_SEPARATOR)
}

/// A typed DOI, normalised; `Ok(None)` for a blank one.
pub fn parse_doi(typed: &str) -> Result<Option<String>, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Ok(None);
    }
    ids::normalize_doi(typed).map(Some).ok_or_else(|| format!("\u{201c}{typed}\u{201d} is not a DOI (10.<number>/<suffix>)"))
}

/// A typed arXiv id, normalised; `Ok(None)` for a blank one.
pub fn parse_arxiv(typed: &str) -> Result<Option<String>, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Ok(None);
    }
    ids::normalize_arxiv(typed).map(Some).ok_or_else(|| format!("\u{201c}{typed}\u{201d} is not an arXiv id (2101.01234 or hep-th/9901001)"))
}

/// A typed year; `Ok(None)` for a blank one.
pub fn parse_year(typed: &str) -> Result<Option<i32>, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Ok(None);
    }
    typed.parse::<i32>().ok().filter(|year| YEARS.contains(year)).map(Some).ok_or_else(|| format!("\u{201c}{typed}\u{201d} is not a year"))
}

/// A field's value as a reference gives it, as the column stores it; `None` when it has none.
fn reference_value(reference: &Reference, field: Field) -> Option<Value> {
    let text = |value: &Option<String>| value.as_deref().filter(|text| !text.is_empty()).map(|text| Value::Text(text.to_owned()));
    match field {
        Field::Title => text(&reference.title),
        Field::Authors => (!reference.authors.is_empty()).then(|| Value::Text(join_authors(&reference.authors))),
        Field::Year => reference.year.filter(|year| YEARS.contains(year)).map(|year| Value::Integer(i64::from(year))),
        Field::Doi => reference.doi.as_deref().and_then(ids::normalize_doi).map(Value::Text),
        Field::Arxiv => reference.arxiv.as_deref().and_then(ids::normalize_arxiv).map(Value::Text),
        Field::Publisher => text(&reference.publisher),
        Field::Isbn => text(&reference.isbn),
        Field::Abstract => text(&reference.abstract_text),
    }
}

/// Fills `book`'s fields from `reference`, except the ones the user edited, and marks them as
/// filled by an import or a lookup. With `keywords_as_tags`, its keywords become the book's tags
/// (added to the ones it has).
pub(crate) fn apply_reference(conn: &Connection, book: i64, reference: &Reference, keywords_as_tags: bool, now: i64) -> Result<(), String> {
    for field in Field::ALL {
        let Some(value) = reference_value(reference, field) else { continue };
        // The column names come from `Field::column`, never from input.
        let sql = format!(
            "UPDATE books SET {column} = ?2, source_fields = source_fields | ?3 WHERE id = ?1 AND user_fields & ?3 = 0",
            column = field.column()
        );
        conn.prepare_cached(&sql).and_then(|mut stmt| stmt.execute(params![book, value, field.bit()])).map_err(db_err)?;
    }
    if keywords_as_tags {
        for keyword in &reference.keywords {
            // A keyword that cleans to nothing is not a tag; the rest of the entry still counts.
            let Ok(tag) = tag_id(conn, keyword, now) else { continue };
            conn.execute("INSERT OR IGNORE INTO book_tags (book, tag) VALUES (?1, ?2)", [book, tag]).map_err(db_err)?;
        }
    }
    Ok(())
}

impl LibraryDb {
    /// The book's details for the details sheet.
    pub fn details(&self, book: i64) -> Result<Details, String> {
        self.conn()
            .query_row(
                "SELECT title, author, year, doi, arxiv, publisher, isbn, abstract, user_fields, looked_up FROM books WHERE id = ?1",
                [book],
                |row| {
                    Ok(Details {
                        title: row.get(0)?,
                        authors: row.get(1)?,
                        year: row.get::<_, Option<i64>>(2)?.and_then(|year| i32::try_from(year).ok()),
                        doi: row.get(3)?,
                        arxiv: row.get(4)?,
                        publisher: row.get(5)?,
                        isbn: row.get(6)?,
                        abstract_text: row.get(7)?,
                        user_fields: row.get(8)?,
                        looked_up: row.get(9)?,
                    })
                },
            )
            .map_err(db_err)
    }

    /// Saves what the user typed: each field that differs from what is kept is written and
    /// marked as the user's, so no scan, import or lookup changes it again. A DOI or arXiv id
    /// that isn't one, a year out of range or a blank title is refused and nothing is saved.
    pub fn edit_details(&self, book: i64, edit: &DetailsEdit) -> Result<(), String> {
        let title = clean(&edit.title, MAX_FIELD_CHARS);
        if title.is_empty() {
            return Err("a book needs a title".to_owned());
        }
        let authors = clean(&edit.authors, MAX_FIELD_CHARS);
        let doi = parse_doi(&edit.doi)?;
        let arxiv = parse_arxiv(&edit.arxiv)?;
        let year = match edit.year {
            Some(year) if !YEARS.contains(&year) => return Err(format!("{year} is not a year")),
            year => year,
        };
        let kept = self.details(book)?;
        let text = |value: Option<String>| value.map_or(Value::Null, Value::Text);
        let changes = [
            (Field::Title, title != kept.title, Value::Text(title)),
            (Field::Authors, authors != kept.authors, Value::Text(authors)),
            (Field::Year, year != kept.year, year.map_or(Value::Null, |year| Value::Integer(i64::from(year)))),
            (Field::Doi, doi != kept.doi, text(doi)),
            (Field::Arxiv, arxiv != kept.arxiv, text(arxiv)),
        ];
        let tx = self.conn().unchecked_transaction().map_err(db_err)?;
        for (field, changed, value) in changes {
            if !changed {
                continue;
            }
            // The column names come from `Field::column`, never from input.
            let sql = format!("UPDATE books SET {column} = ?2, user_fields = user_fields | ?3 WHERE id = ?1", column = field.column());
            tx.execute(&sql, params![book, value, field.bit()]).map_err(db_err)?;
        }
        tx.commit().map_err(db_err)
    }
}

/// Lower-case letters and digits only: how titles are compared when matching an import entry.
fn title_key(title: &str) -> String {
    title.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// A listed book an import entry can match.
struct Candidate {
    book: i64,
    year: Option<i32>,
    removed: bool,
}

/// What an import entry matched.
enum Matched {
    /// A book the library lists.
    Listed(i64),
    /// A book the user removed: left alone.
    Removed,
}

/// Every book by title key, for matching entries by title and year.
fn titles(conn: &Connection) -> Result<HashMap<String, Vec<Candidate>>, String> {
    let mut stmt = conn.prepare("SELECT id, title, year, removed FROM books").map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                Candidate {
                    book: row.get(0)?,
                    year: row.get::<_, Option<i64>>(2)?.and_then(|year| i32::try_from(year).ok()),
                    removed: row.get::<_, i64>(3)? != 0,
                },
            ))
        })
        .map_err(db_err)?;
    let mut by_title: HashMap<String, Vec<Candidate>> = HashMap::new();
    for row in rows {
        let (title, candidate) = row.map_err(db_err)?;
        let key = title_key(&title);
        if key.chars().count() >= MIN_TITLE_MATCH_CHARS {
            by_title.entry(key).or_default().push(candidate);
        }
    }
    Ok(by_title)
}

fn matched(book: Option<(i64, bool)>) -> Option<Matched> {
    book.map(|(book, removed)| if removed { Matched::Removed } else { Matched::Listed(book) })
}

/// The book an import entry describes: by one of its files, else its DOI, else its arXiv id,
/// else its title and year (a book without a year matches by title alone).
fn find_book(conn: &Connection, reference: &Reference, by_title: &HashMap<String, Vec<Candidate>>) -> Result<Option<Matched>, String> {
    let by_path = |path: &str| -> Result<Option<(i64, bool)>, String> {
        conn.query_row("SELECT b.id, b.removed FROM book_files f JOIN books b ON b.id = f.book WHERE f.path = ?1", [path], |row| {
            Ok((row.get(0)?, row.get::<_, i64>(1)? != 0))
        })
        .optional()
        .map_err(db_err)
    };
    for file in &reference.files {
        let canonical = std::fs::canonicalize(file).ok();
        for path in std::iter::once(file).chain(canonical.as_ref()) {
            if let Some(found) = matched(by_path(&path.to_string_lossy())?) {
                return Ok(Some(found));
            }
        }
    }
    for (column, id) in
        [("doi", reference.doi.as_deref().and_then(ids::normalize_doi)), ("arxiv", reference.arxiv.as_deref().and_then(ids::normalize_arxiv))]
    {
        let Some(id) = id else { continue };
        // The column names are the two literals above, never input.
        let sql = format!("SELECT id, removed FROM books WHERE {column} = ?1 ORDER BY removed, id LIMIT 1");
        let found = conn.query_row(&sql, [id], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0))).optional().map_err(db_err)?;
        if let Some(found) = matched(found) {
            return Ok(Some(found));
        }
    }
    let Some(title) = reference.title.as_deref() else { return Ok(None) };
    let found = by_title.get(&title_key(title)).and_then(|candidates| {
        let fits = |candidate: &&Candidate| match (candidate.year, reference.year) {
            (Some(kept), Some(given)) => kept == given,
            _ => true,
        };
        candidates.iter().filter(fits).min_by_key(|candidate| (candidate.removed, candidate.book))
    });
    Ok(matched(found.map(|candidate| (candidate.book, candidate.removed))))
}

/// What an import reads: a BibTeX file, or Zotero's `zotero.sqlite`.
#[derive(Clone, Debug)]
pub struct ImportRequest {
    /// `<data>`.
    pub data: PathBuf,
    pub source: PathBuf,
}

/// What a finished import did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Entries read from the source.
    pub entries: usize,
    /// Entries that matched a book in the library, whose details they filled.
    pub matched: usize,
    /// Entries whose PDF the library didn't list: the file was added as a new book.
    pub added: usize,
    /// Entries that matched no book and named no PDF that is there, or matched a removed book.
    pub skipped: usize,
    pub cancelled: bool,
}

/// Whether `path` is an SQLite database (Zotero's), from its first bytes.
fn is_sqlite(path: &Path) -> Result<bool, String> {
    use std::io::Read;
    const MAGIC: &[u8] = b"SQLite format 3\0";
    let mut head = [0u8; MAGIC.len()];
    let mut file = std::fs::File::open(path).map_err(|err| format!("can't open {}: {err}", path.display()))?;
    let read = file.read(&mut head).map_err(|err| format!("can't read {}: {err}", path.display()))?;
    Ok(head.get(..read) == Some(MAGIC))
}

/// The entries of a BibTeX file, their relative file paths resolved against its folder.
fn read_bibtex(path: &Path) -> Result<Vec<Reference>, String> {
    let size = std::fs::metadata(path).map_err(|err| format!("can't read {}: {err}", path.display()))?.len();
    if !usize::try_from(size).is_ok_and(|size| size <= bibtex::MAX_BIBTEX_BYTES) {
        return Err(format!("{} is larger than {} MiB", path.display(), bibtex::MAX_BIBTEX_BYTES >> 20));
    }
    let bytes = std::fs::read(path).map_err(|err| format!("can't read {}: {err}", path.display()))?;
    let folder = path.parent().unwrap_or_else(|| Path::new(""));
    let entries = bibtex::parse(&String::from_utf8_lossy(&bytes))?;
    Ok(entries
        .iter()
        .map(|entry| {
            let mut reference = bibtex::reference(entry);
            for file in &mut reference.files {
                if file.is_relative() {
                    *file = folder.join(&*file);
                }
            }
            reference
        })
        .collect())
}

/// The entries of `source`: Zotero's database when it is SQLite, else BibTeX.
pub fn read_references(source: &Path) -> Result<Vec<Reference>, String> {
    if is_sqlite(source)? { zotero::read_zotero(source) } else { read_bibtex(source) }
}

/// The first PDF an entry names that is there.
fn existing_pdf(reference: &Reference) -> Option<PathBuf> {
    reference.files.iter().find(|file| file.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")) && file.is_file()).cloned()
}

/// Imports `request.source` into the library: each entry fills the details of the book it
/// matches ([`find_book`]) and adds its keywords as tags; an entry that matches none but names a
/// PDF that is there adds that file as a book (scanned for its cover), then fills it. Stops soon
/// after `cancel` is set, keeping what was committed.
pub fn import(request: &ImportRequest, cancel: &AtomicBool) -> Result<ImportReport, String> {
    let references = read_references(&request.source)?;
    let conn = open_connection(&request.data)?;
    let mut report = ImportReport { entries: references.len(), ..ImportReport::default() };
    let by_title = titles(&conn)?;
    let mut to_add: Vec<(&Reference, PathBuf)> = Vec::new();
    let now = now_unix();
    for batch in references.chunks(IMPORT_BATCH) {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            return Ok(report);
        }
        let tx = conn.unchecked_transaction().map_err(db_err)?;
        for reference in batch {
            match find_book(&tx, reference, &by_title)? {
                Some(Matched::Listed(book)) => {
                    apply_reference(&tx, book, reference, true, now)?;
                    report.matched += 1;
                }
                Some(Matched::Removed) => report.skipped += 1,
                None => match existing_pdf(reference) {
                    Some(file) => to_add.push((reference, file)),
                    None => report.skipped += 1,
                },
            }
        }
        tx.commit().map_err(db_err)?;
    }
    if to_add.is_empty() {
        return Ok(report);
    }
    let scan =
        ScanRequest { data: request.data.clone(), folders: Vec::new(), files: to_add.iter().map(|(_, file)| file.clone()).collect(), threads: 0 };
    let scanned = crate::library::scan(&scan, cancel, &Mutex::new(ScanProgress::default()))?;
    report.cancelled = scanned.cancelled;
    let tx = conn.unchecked_transaction().map_err(db_err)?;
    for (reference, file) in &to_add {
        let path = file.to_string_lossy();
        let book: Option<(i64, bool)> = tx
            .query_row("SELECT b.id, b.removed FROM book_files f JOIN books b ON b.id = f.book WHERE f.path = ?1", [path.as_ref()], |row| {
                Ok((row.get(0)?, row.get::<_, i64>(1)? != 0))
            })
            .optional()
            .map_err(db_err)?;
        match book {
            Some((book, false)) => {
                apply_reference(&tx, book, reference, true, now)?;
                report.added += 1;
            }
            _ => report.skipped += 1,
        }
    }
    tx.commit().map_err(db_err)?;
    Ok(report)
}

/// What a lookup asks about.
#[derive(Clone, Debug)]
pub struct LookupRequest {
    /// `<data>`.
    pub data: PathBuf,
    /// One book, whatever was looked up before (the details sheet's button); `None`: every
    /// book with a DOI or arXiv id that was never looked up (`library.metadata_lookup`), a
    /// pause between two.
    pub book: Option<i64>,
}

/// What a finished lookup did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LookupReport {
    /// Books asked about.
    pub asked: usize,
    /// Books whose details were filled.
    pub updated: usize,
    /// Why the last book that failed did (for one book: why it failed).
    pub failure: Option<String>,
    pub cancelled: bool,
}

/// Answers a lookup by DOI and arXiv id (the network one is [`crate::lookup::lookup`]).
pub type Fetch<'a> = &'a (dyn Fn(Option<&str>, Option<&str>) -> Result<Reference, LookupError> + Sync);

/// The books an automatic pass asks about: listed, with an id, never looked up.
fn lookup_candidates(conn: &Connection) -> Result<Vec<i64>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id FROM books WHERE removed = 0 AND missing = 0 AND looked_up IS NULL AND (doi IS NOT NULL OR arxiv IS NOT NULL)
             ORDER BY last_opened DESC, id LIMIT ?1",
        )
        .map_err(db_err)?;
    let limit = i64::try_from(MAX_AUTO_LOOKUPS).unwrap_or(i64::MAX);
    let rows = stmt.query_map([limit], |row| row.get(0)).map_err(db_err)?;
    rows.map(|row| row.map_err(db_err)).collect()
}

/// Waits `pause` unless cancelled first; false when cancelled.
fn wait(pause: Duration, cancel: &AtomicBool) -> bool {
    let Some(until) = Instant::now().checked_add(pause) else { return !cancel.load(Ordering::Relaxed) };
    while Instant::now() < until {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        std::thread::sleep(CANCEL_POLL.min(until.saturating_duration_since(Instant::now())));
    }
    !cancel.load(Ordering::Relaxed)
}

/// Looks books up through `fetch` and fills what it answers ([`apply_reference`]; keywords are
/// not made tags). A book that has no record is stamped looked up so a pass doesn't ask again;
/// being offline ends a pass and stamps nothing.
pub fn run_lookup(request: &LookupRequest, fetch: Fetch<'_>, pause: Duration, cancel: &AtomicBool) -> Result<LookupReport, String> {
    let conn = open_connection(&request.data)?;
    let books = match request.book {
        Some(book) => vec![book],
        None => lookup_candidates(&conn)?,
    };
    let mut report = LookupReport::default();
    for (index, book) in books.into_iter().enumerate() {
        if index > 0 && !wait(pause, cancel) {
            report.cancelled = true;
            break;
        }
        let ids: Option<(Option<String>, Option<String>)> =
            conn.query_row("SELECT doi, arxiv FROM books WHERE id = ?1", [book], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(db_err)?;
        let Some((doi, arxiv)) = ids.filter(|(doi, arxiv)| doi.is_some() || arxiv.is_some()) else {
            report.failure = Some("add a DOI or an arXiv id to look this book up".to_owned());
            continue;
        };
        report.asked += 1;
        let answer = fetch(doi.as_deref(), arxiv.as_deref());
        let now = now_unix();
        match answer {
            Ok(reference) => {
                let tx = conn.unchecked_transaction().map_err(db_err)?;
                apply_reference(&tx, book, &reference, false, now)?;
                tx.execute("UPDATE books SET looked_up = ?2 WHERE id = ?1", params![book, now]).map_err(db_err)?;
                tx.commit().map_err(db_err)?;
                report.updated += 1;
            }
            Err(LookupError::Offline(reason)) => {
                report.failure = Some(reason);
                break;
            }
            Err(LookupError::NotFound(reason) | LookupError::Invalid(reason)) => {
                conn.execute("UPDATE books SET looked_up = ?2 WHERE id = ?1", params![book, now]).map_err(db_err)?;
                report.failure = Some(reason);
            }
        }
    }
    Ok(report)
}

/// Background work with a result: cancel it, take its result once it ends.
pub struct Job<R> {
    cancel: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<R, String>>>>,
}

/// An import on its own thread ([`import`]).
pub type ImportJob = Job<ImportReport>;
/// A lookup on its own thread ([`run_lookup`] over the network).
pub type LookupJob = Job<LookupReport>;

impl<R: Send + 'static> Job<R> {
    fn idle() -> Self {
        Self { cancel: Arc::default(), result: Arc::default() }
    }

    /// Runs `work` on a thread called `name`; `wake` runs when it ends (the UI repaints).
    fn spawn(
        name: &str,
        work: impl FnOnce(&AtomicBool) -> Result<R, String> + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        let job = Self::idle();
        let (cancel, result) = (job.cancel.clone(), job.result.clone());
        std::thread::Builder::new().name(name.to_owned()).spawn(move || {
            let outcome = work(&cancel);
            *lock(&result) = Some(outcome);
            wake();
        })?;
        Ok(job)
    }

    /// Runs `work` on this thread (tests, embedders without threads).
    fn inline(work: impl FnOnce(&AtomicBool) -> Result<R, String>) -> Self {
        let job = Self::idle();
        let outcome = work(&job.cancel);
        *lock(&job.result) = Some(outcome);
        job
    }

    /// Asks the work to stop; what it committed stays.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The result, once the work ended (taken once). A worker that died without one reports an
    /// error rather than leaving the job running forever.
    pub fn take_result(&self) -> Option<Result<R, String>> {
        let taken = lock(&self.result).take();
        if taken.is_none() && Arc::strong_count(&self.result) == 1 {
            return Some(Err("the background job stopped unexpectedly".to_owned()));
        }
        taken
    }
}

impl ImportJob {
    /// Starts an import on a new thread.
    pub fn start(request: ImportRequest, wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        Self::spawn("margin-library-import", move |cancel| import(&request, cancel), wake)
    }

    /// Runs an import on this thread.
    pub fn run_inline(request: &ImportRequest) -> Self {
        Self::inline(|cancel| import(request, cancel))
    }
}

impl LookupJob {
    /// Starts a lookup over the network on a new thread.
    pub fn start(request: LookupRequest, wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        Self::spawn("margin-library-lookup", move |cancel| run_lookup(&request, &crate::lookup::lookup, AUTO_LOOKUP_PAUSE, cancel), wake)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Opened;
    use crate::library::tests::{pdf, temp_dir};

    fn opened<'a>(path: &'a str, fingerprint: &'a str, title: &'a str) -> Opened<'a> {
        Opened { path, fingerprint, title, author: "From the file", pages: 1, size: 1, sidecar_id: None }
    }

    #[test]
    fn user_edits_win_over_imports_lookups_and_scans() {
        let root = temp_dir("meta-edit");
        let books = root.join("books");
        std::fs::create_dir_all(&books).unwrap();
        let file = books.join("paper.pdf");
        std::fs::write(&file, pdf("File Title", 2)).unwrap();
        let request = ScanRequest { data: root.join("data"), folders: vec![books.clone()], files: Vec::new(), threads: 1 };
        crate::library::ScanJob::run_inline(&request).take_report().unwrap().unwrap();
        let db = LibraryDb::open(&root.join("data")).unwrap();
        let book = db.books().unwrap().pop().unwrap().id;

        let edit = DetailsEdit {
            title: "My Title".into(),
            authors: "Ada Lovelace, Grace Hopper".into(),
            year: Some(2020),
            doi: "https://doi.org/10.1234/ABC.".into(),
            arxiv: String::new(),
        };
        db.edit_details(book, &edit).unwrap();
        let details = db.details(book).unwrap();
        assert_eq!((details.title.as_str(), details.year, details.doi.as_deref()), ("My Title", Some(2020), Some("10.1234/abc")));
        assert_eq!(details.user_fields, Field::Title.bit() | Field::Authors.bit() | Field::Year.bit() | Field::Doi.bit());

        let reference = Reference {
            title: Some("Imported Title".into()),
            authors: vec!["Someone Else".into()],
            year: Some(1999),
            arxiv: Some("2101.01234".into()),
            publisher: Some("Journal of Tests".into()),
            keywords: vec!["ML".into(), " ".into()],
            ..Reference::default()
        };
        apply_reference(db.conn(), book, &reference, true, 5).unwrap();
        let details = db.details(book).unwrap();
        assert_eq!(details.title, "My Title", "an import never changes what the user typed");
        assert_eq!(details.year, Some(2020));
        assert_eq!((details.arxiv.as_deref(), details.publisher.as_deref()), (Some("2101.01234"), Some("Journal of Tests")), "it fills the rest");
        assert_eq!(db.tags().unwrap().iter().map(|tag| tag.name.as_str()).collect::<Vec<_>>(), ["ML"], "keywords become tags");

        // The file changes: a scan reads it again but keeps every edited and imported field.
        let mut bytes = std::fs::read(&file).unwrap();
        bytes.extend_from_slice(b"\n% edited\n");
        std::fs::write(&file, bytes).unwrap();
        crate::library::ScanJob::run_inline(&request).take_report().unwrap().unwrap();
        let details = db.details(book).unwrap();
        assert_eq!((details.title.as_str(), details.authors.as_str()), ("My Title", "Ada Lovelace, Grace Hopper"));
        assert_eq!(details.publisher.as_deref(), Some("Journal of Tests"));

        // Bad input is refused, and nothing changes.
        for bad in [
            DetailsEdit { doi: "not a doi".into(), ..edit.clone() },
            DetailsEdit { arxiv: "soon".into(), ..edit.clone() },
            DetailsEdit { title: " \u{7} ".into(), ..edit.clone() },
            DetailsEdit { year: Some(123_456), ..edit.clone() },
        ] {
            assert!(db.edit_details(book, &bad).is_err(), "{bad:?}");
        }
        assert_eq!(db.details(book).unwrap().title, "My Title");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_scan_fills_ids_it_finds_in_the_file() {
        let root = temp_dir("meta-ids");
        let books = root.join("books");
        std::fs::create_dir_all(&books).unwrap();
        // The DOI written in the Info dictionary's subject, as many publishers do (the title
        // closes early so the subject is its own key; the helper writes the offsets after).
        let bytes = pdf("Paper) /Subject (doi:10.1145/3290605.3300857", 1);
        std::fs::write(books.join("paper.pdf"), bytes).unwrap();
        let request = ScanRequest { data: root.join("data"), folders: vec![books], files: Vec::new(), threads: 1 };
        crate::library::ScanJob::run_inline(&request).take_report().unwrap().unwrap();
        let db = LibraryDb::open(&root.join("data")).unwrap();
        let book = db.books().unwrap().pop().unwrap().id;
        assert_eq!(db.details(book).unwrap().doi.as_deref(), Some("10.1145/3290605.3300857"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_import_matches_by_file_doi_and_title_adds_files_and_skips_the_rest() {
        let root = temp_dir("meta-import");
        let data = root.join("data");
        let papers = root.join("papers");
        std::fs::create_dir_all(&papers).unwrap();
        let db = LibraryDb::open(&data).unwrap();
        let by_file = db.opened(&opened("/lib/by-file.pdf", "11111111111111111111111111111111", "Something"), 1).unwrap().id;
        let by_doi = db.opened(&opened("/lib/by-doi.pdf", "22222222222222222222222222222222", "Other"), 1).unwrap().id;
        let by_title = db.opened(&opened("/lib/by-title.pdf", "33333333333333333333333333333333", "Attention Is All You Need"), 1).unwrap().id;
        db.edit_details(by_doi, &DetailsEdit { title: "Mine".into(), doi: "10.1000/xyz".into(), ..DetailsEdit::default() }).unwrap();
        let removed = db.opened(&opened("/lib/removed.pdf", "44444444444444444444444444444444", "A Removed Long Title"), 1).unwrap().id;
        db.remove(removed).unwrap();
        std::fs::write(papers.join("new.pdf"), pdf("New Paper", 2)).unwrap();
        let bib = r#"
@article{a, title={Matched by file}, file={/lib/by-file.pdf}, keywords={alpha, beta}}
@article{b, title={Matched by DOI}, doi={https://doi.org/10.1000/XYZ}}
@inproceedings{c, title={Attention is all you need}, year={2017}, author={Vaswani, Ashish and Shazeer, Noam}}
@article{d, title={A Removed Long Title}}
@article{e, title={New}, file={papers/new.pdf}, year=2024}
@article{f, title={Nowhere to be found at all}}
@misc{broken, title={unterminated
"#;
        let source = root.join("library.bib");
        std::fs::write(&source, bib).unwrap();
        let report = ImportJob::run_inline(&ImportRequest { data: data.clone(), source }).take_result().unwrap().unwrap();
        assert_eq!(
            (report.entries, report.matched, report.added, report.skipped),
            (6, 3, 1, 2),
            "the broken entry is dropped by the parser: {report:?}"
        );
        assert_eq!(db.details(by_file).unwrap().title, "Matched by file");
        assert_eq!(db.details(by_doi).unwrap().title, "Mine", "the title the user typed stays");
        let attention = db.details(by_title).unwrap();
        assert_eq!((attention.authors.as_str(), attention.year), ("Ashish Vaswani, Noam Shazeer", Some(2017)));
        let listed = db.books().unwrap();
        let added = listed.iter().find(|book| book.path.ends_with("new.pdf")).expect("the named PDF joined the library");
        assert_eq!((added.title.as_str(), added.year), ("New", Some(2024)));
        let tags: Vec<String> = db.tags().unwrap().into_iter().map(|tag| tag.name).collect();
        assert_eq!(tags, ["alpha", "beta"]);
        assert!(listed.iter().all(|book| book.id != removed), "a removed book stays removed");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_lookup_fills_details_and_an_automatic_pass_asks_each_book_once() {
        let root = temp_dir("meta-lookup");
        let data = root.join("data");
        let db = LibraryDb::open(&data).unwrap();
        let with_doi = db.opened(&opened("/lib/a.pdf", "11111111111111111111111111111111", "a"), 1).unwrap().id;
        let without = db.opened(&opened("/lib/b.pdf", "22222222222222222222222222222222", "b"), 1).unwrap().id;
        let unknown = db.opened(&opened("/lib/c.pdf", "33333333333333333333333333333333", "c"), 1).unwrap().id;
        db.edit_details(with_doi, &DetailsEdit { title: "Mine".into(), doi: "10.1000/found".into(), ..DetailsEdit::default() }).unwrap();
        db.edit_details(unknown, &DetailsEdit { title: "c".into(), arxiv: "2101.01234".into(), ..DetailsEdit::default() }).unwrap();
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let fetch = |doi: Option<&str>, _arxiv: Option<&str>| {
            asked.fetch_add(1, Ordering::Relaxed);
            match doi {
                Some("10.1000/found") => {
                    Ok(Reference { title: Some("Found".into()), publisher: Some("Venue".into()), year: Some(2001), ..Reference::default() })
                }
                _ => Err(LookupError::NotFound("no record".into())),
            }
        };
        let cancel = AtomicBool::new(false);
        let report = run_lookup(&LookupRequest { data: data.clone(), book: None }, &fetch, Duration::ZERO, &cancel).unwrap();
        assert_eq!((report.asked, report.updated), (2, 1));
        let found = db.details(with_doi).unwrap();
        assert_eq!(found.title, "Mine", "the title the user typed stays");
        assert_eq!((found.publisher.as_deref(), found.year), (Some("Venue"), Some(2001)));
        assert!(found.looked_up.is_some() && db.details(unknown).unwrap().looked_up.is_some());
        let again = run_lookup(&LookupRequest { data: data.clone(), book: None }, &fetch, Duration::ZERO, &cancel).unwrap();
        assert_eq!(again.asked, 0, "nothing is asked twice");
        assert_eq!(asked.load(Ordering::Relaxed), 2);
        let one = run_lookup(&LookupRequest { data: data.clone(), book: Some(without) }, &fetch, Duration::ZERO, &cancel).unwrap();
        assert_eq!((one.asked, one.failure.is_some()), (0, true), "a book without an id says so");
        let offline = |_: Option<&str>, _: Option<&str>| Err::<Reference, _>(LookupError::Offline("couldn't reach api.crossref.org".into()));
        let report = run_lookup(&LookupRequest { data, book: Some(with_doi) }, &offline, Duration::ZERO, &cancel).unwrap();
        assert_eq!(report.failure.as_deref(), Some("couldn't reach api.crossref.org"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn typed_ids_and_years_are_checked() {
        assert_eq!(parse_doi(" doi:10.1145/3290605.3300857 "), Ok(Some("10.1145/3290605.3300857".into())));
        assert_eq!(parse_doi("  "), Ok(None));
        assert!(parse_doi("10.12/x").is_err());
        assert_eq!(parse_arxiv("arXiv:2101.01234v2"), Ok(Some("2101.01234".into())));
        assert!(parse_arxiv("2101").is_err());
        assert_eq!(parse_year("2019"), Ok(Some(2019)));
        assert!(parse_year("twenty").is_err() && parse_year("-4").is_err() && parse_year("99999").is_err());
        assert_eq!(title_key("Attention Is All You Need!"), title_key("attention is all  you need"));
    }
}
