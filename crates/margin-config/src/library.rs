//! `<data>/library.sqlite`: the books Margin knows (title, author, pages, when added and last
//! opened, progress, finished), their files, the collections they are in, and their covers
//! (`<data>/cache/thumbs/<book_id>.jpg`, the first page through PdfCraft's renderer).
//!
//! A book is its stable key, `book_id`: stats, the cover and the sidecar kept in `<data>` are
//! filed under it, so they survive whatever happens to the file. Each book has one or more files
//! (a copy in another folder is the same book). A file the library hasn't seen joins the book its
//! sidecar names (`doc.book_id`), else the book with a file of the same fingerprint and size
//! (moved, renamed or copied), else starts a new book; a file it has seen stays with its book
//! when its bytes change.
//!
//! A book also has tags and bibliographic details ([`crate::library_meta`]): a scan fills the
//! DOI and arXiv id it finds in the file, and never overwrites a field the user edited or an
//! import or lookup filled. Titles, authors, tags and details are searchable through FTS5, and
//! page text once the background text pass has read it ([`crate::library_text`]).
//!
//! [`scan`] indexes the watched folders (`library.folders`) as a cancellable job: callers run it
//! on a worker thread ([`ScanJob::spawn`]). Files whose size and modified time are unchanged are
//! not read again; their progress is refreshed from their sidecar. New and changed files are read
//! on a few threads (fingerprint, metadata, cover) and committed in small transactions, so the UI
//! thread's own connection is never blocked for long.
//!
//! Untrusted input everywhere: a broken PDF gets a title from its file name and no cover, a
//! broken database is reported, never a crash.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, RequestKind};
use rusqlite::{Connection, OptionalExtension, params};

use crate::library_meta::Field;
use crate::{Locator, Sidecar};
use crate::{ids, library_schema, migrate, store};

/// The library database inside `<data>`.
pub const LIBRARY_DB: &str = "library.sqlite";
/// Cached covers inside `<data>`.
pub const THUMBS_DIR: &str = "cache/thumbs";
/// Cover width in pixels: a grid cell's width at a device pixel ratio of 2.
pub const COVER_WIDTH: u32 = 240;
/// Covers are small, so a modest JPEG quality is invisible at that size.
const COVER_QUALITY: u8 = 80;
/// Folder levels a scan descends.
const MAX_DEPTH: usize = 16;
/// Directory entries a scan reads in total, so a huge tree can't stall it.
const MAX_ENTRIES: usize = 500_000;
/// Files larger than this are listed from their name only (never read into memory by a scan).
const MAX_READ_BYTES: u64 = 1 << 30;
/// New and changed books are committed this many at a time, or once the oldest uncommitted one
/// has waited [`BATCH_WAIT`], so books show up while slow files are still being read.
const BATCH: usize = 32;
const BATCH_WAIT: Duration = Duration::from_millis(250);
/// Most threads a scan reads PDFs on.
const MAX_THREADS: usize = 4;
/// How long a connection waits for another's write to finish.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest title or author kept (metadata is untrusted).
const MAX_TEXT_CHARS: usize = 512;
/// Longest collection name.
pub const MAX_COLLECTION_CHARS: usize = 120;
/// Longest tag name (the same cap as a keyword an import brings).
pub const MAX_TAG_CHARS: usize = crate::reference::MAX_KEYWORD_CHARS;

/// Whether a book's cover is in the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cover {
    /// Not made yet.
    Missing,
    /// In `<data>/cache/thumbs/<book_id>.jpg`.
    Cached,
    /// The first page couldn't be rendered; not tried again until the file changes.
    Failed,
}

impl Cover {
    fn from_db(value: i64) -> Self {
        match value {
            1 => Self::Cached,
            -1 => Self::Failed,
            _ => Self::Missing,
        }
    }

    fn to_db(self) -> i64 {
        match self {
            Self::Missing => 0,
            Self::Cached => 1,
            Self::Failed => -1,
        }
    }
}

/// One book as the library lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Book {
    pub id: i64,
    /// The file the library opens: one that was there at the last scan.
    pub path: String,
    /// The book's stable key: its identity in `stats.sqlite`, its cover's and its central
    /// sidecar's file name. A file moved, renamed, edited or copied keeps it.
    pub book_id: String,
    pub title: String,
    /// Every author, comma separated.
    pub author: String,
    /// The year it was published, from an import, a lookup or the user.
    pub year: Option<i32>,
    pub pages: u32,
    pub size: u64,
    /// Unix seconds.
    pub added: i64,
    pub last_opened: Option<i64>,
    /// Share read, 0..=1, from the sidecar or the reader.
    pub progress: f64,
    /// The page the reader stopped on (0-based).
    pub page: u32,
    pub finished: bool,
    pub cover: Cover,
    /// The collections it is in.
    pub collections: Vec<i64>,
    /// Its tags, by id.
    pub tags: Vec<i64>,
}

impl Book {
    /// Started and not finished.
    pub fn is_reading(&self) -> bool {
        !self.finished && (self.progress > 0.0 || self.page > 0)
    }
}

/// A collection and how many books it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collection {
    pub id: i64,
    pub name: String,
    pub count: usize,
}

/// A tag and how many listed books carry it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub count: usize,
}

/// What the reader knows about a document it opened.
#[derive(Clone, Debug)]
pub struct Opened<'a> {
    pub path: &'a str,
    /// The file's fingerprint now.
    pub fingerprint: &'a str,
    pub title: &'a str,
    pub author: &'a str,
    pub pages: u32,
    pub size: u64,
    /// The `doc.book_id` of the document's sidecar, when it has one.
    pub sidecar_id: Option<&'a str>,
}

/// A book the reader opened, as the library lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedBook {
    pub id: i64,
    /// The book's stable key (stats, cover and central sidecar are kept under it).
    pub book_id: String,
    /// Its cover isn't cached yet: scan the file.
    pub needs_cover: bool,
}

/// `<data>/library.sqlite`, opened. One connection; each thread opens its own.
pub struct LibraryDb {
    conn: Connection,
    data: PathBuf,
}

/// Seconds since the Unix epoch now.
pub fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

pub(crate) fn db_err(err: rusqlite::Error) -> String {
    format!("library database: {err}")
}

/// WAL and `synchronous = NORMAL`: a commit doesn't wait for the disk, readers never wait for a
/// writer, and a crash loses at most the last commits, never the database.
pub(crate) fn configure(conn: &Connection) -> Result<(), String> {
    conn.busy_timeout(BUSY_TIMEOUT).map_err(db_err)?;
    conn.pragma_update(None, "journal_mode", "WAL").map_err(db_err)?;
    conn.pragma_update(None, "synchronous", "NORMAL").map_err(db_err)?;
    conn.pragma_update(None, "foreign_keys", "ON").map_err(db_err)
}

pub(crate) fn open_connection(data: &Path) -> Result<Connection, String> {
    std::fs::create_dir_all(data).map_err(|err| format!("can't create {}: {err}", data.display()))?;
    let path = data.join(LIBRARY_DB);
    let conn = Connection::open(&path).map_err(db_err)?;
    configure(&conn)?;
    migrate::upgrade(&conn, &path, &migrate::Schema { name: "library database", steps: library_schema::STEPS })?;
    Ok(conn)
}

/// `text` without control characters, trimmed and capped (PDF metadata is untrusted).
fn clean_text(text: &str) -> String {
    text.trim().chars().filter(|c| !c.is_control()).take(MAX_TEXT_CHARS).collect()
}

/// The id of the tag called `name` (cleaned and capped), made if no tag has that name in any
/// case.
pub(crate) fn tag_id(conn: &Connection, name: &str, now: i64) -> Result<i64, String> {
    let name = crate::reference::clean(name, MAX_TAG_CHARS);
    if name.is_empty() {
        return Err("a tag needs a name".to_owned());
    }
    conn.execute("INSERT OR IGNORE INTO tags (name, created) VALUES (?1, ?2)", params![name, now]).map_err(db_err)?;
    conn.query_row("SELECT id FROM tags WHERE name = ?1", [name], |row| row.get(0)).map_err(db_err)
}

/// The file name without `.pdf`: the title of a PDF without one.
pub fn title_from_path(path: &Path) -> String {
    path.file_stem().map_or_else(|| path.to_string_lossy().into_owned(), |stem| stem.to_string_lossy().into_owned())
}

/// Where the cover of the book with `book_id` is cached.
pub fn cover_path(data: &Path, book_id: &str) -> PathBuf {
    data.join(THUMBS_DIR).join(format!("{book_id}.jpg"))
}

/// Longest book key taken from a sidecar.
const MAX_KEY_CHARS: usize = 64;

/// Whether `key` (from a sidecar, so untrusted) can be a book's key: it names files in `<data>`,
/// so only letters, digits, `-` and `_`.
fn usable_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_KEY_CHARS && key.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// How a file was matched to its book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Match {
    /// The library lists a file at this path.
    Path,
    /// The file's sidecar names the book: a moved file that kept its sidecar, maybe edited too.
    Sidecar,
    /// A file of the book has the same fingerprint and size: moved, renamed or copied.
    Content,
}

/// The book a file belongs to: the one listed at its path; else the one its sidecar names; else
/// the one with a file of the same fingerprint and size (one that is gone first: a move).
/// `None`: a new book.
fn resolve(conn: &Connection, path: &str, fingerprint: &str, size: i64, sidecar_id: Option<&str>) -> Result<Option<(i64, Match)>, String> {
    let by_path = conn.query_row("SELECT book FROM book_files WHERE path = ?1", [path], |row| row.get(0)).optional().map_err(db_err)?;
    if let Some(book) = by_path {
        return Ok(Some((book, Match::Path)));
    }
    if let Some(id) = sidecar_id.filter(|id| usable_key(id)) {
        let by_sidecar = conn
            .query_row("SELECT id FROM books WHERE book_id = ?1 OR sidecar_id = ?1 ORDER BY removed, id LIMIT 1", [id], |row| row.get(0))
            .optional()
            .map_err(db_err)?;
        if let Some(book) = by_sidecar {
            return Ok(Some((book, Match::Sidecar)));
        }
    }
    if !store::is_fingerprint(fingerprint) {
        return Ok(None);
    }
    conn.query_row(
        "SELECT f.book FROM book_files f JOIN books b ON b.id = f.book WHERE f.fingerprint = ?1 AND f.size = ?2
         ORDER BY f.missing DESC, b.removed, f.book LIMIT 1",
        params![fingerprint, size],
        |row| row.get(0),
    )
    .optional()
    .map(|book| book.map(|book| (book, Match::Content)))
    .map_err(db_err)
}

/// What a new book's row starts with.
struct NewBook<'a> {
    path: &'a str,
    title: &'a str,
    author: &'a str,
    pages: u32,
    added: i64,
    /// The `doc.book_id` of the file's sidecar: the book's key too when no other book has it.
    sidecar_id: Option<&'a str>,
}

/// Adds a book; returns its row id. Its key is its sidecar's id when that is usable, else a new
/// ULID.
fn insert_book(conn: &Connection, book: &NewBook<'_>) -> Result<i64, String> {
    let taken = |id: &str| -> Result<bool, String> {
        conn.query_row("SELECT EXISTS (SELECT 1 FROM books WHERE book_id = ?1 OR sidecar_id = ?1)", [id], |row| row.get(0)).map_err(db_err)
    };
    let from_sidecar = match book.sidecar_id.filter(|id| usable_key(id)) {
        Some(id) if !taken(id)? => Some(id.to_owned()),
        _ => None,
    };
    let book_id = from_sidecar.unwrap_or_else(store::new_ulid);
    conn.execute(
        "INSERT INTO books (book_id, path, title, author, pages, added) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![book_id, book.path, book.title, book.author, book.pages, book.added],
    )
    .map_err(db_err)?;
    Ok(conn.last_insert_rowid())
}

/// Remembers the `doc.book_id` of a book's sidecar when it differs from the book's key, so the
/// file is found by it after a move.
fn note_sidecar(conn: &Connection, book: i64, sidecar_id: Option<&str>) -> Result<(), String> {
    if let Some(id) = sidecar_id.filter(|id| usable_key(id)) {
        conn.execute("UPDATE books SET sidecar_id = ?2 WHERE id = ?1 AND sidecar_id IS NULL AND book_id != ?2", params![book, id]).map_err(db_err)?;
    }
    Ok(())
}

/// Keeps the DOI and arXiv id a scan found in the file, where the book has none yet and nobody
/// (the user, an import, a lookup) set one.
fn fill_detected(conn: &Connection, book: i64, doi: Option<&str>, arxiv: Option<&str>) -> Result<(), String> {
    let mut stmt = conn
        .prepare_cached(
            "UPDATE books SET doi = CASE WHEN doi IS NULL AND (user_fields | source_fields) & ?4 = 0 THEN ?2 ELSE doi END,
                arxiv = CASE WHEN arxiv IS NULL AND (user_fields | source_fields) & ?5 = 0 THEN ?3 ELSE arxiv END
             WHERE id = ?1",
        )
        .map_err(db_err)?;
    stmt.execute(params![book, doi, arxiv, Field::Doi.bit(), Field::Arxiv.bit()]).map_err(db_err)?;
    Ok(())
}

/// A file as `book_files` keeps it.
struct FileRow<'a> {
    path: &'a str,
    fingerprint: &'a str,
    size: i64,
    mtime: i64,
}

/// Lists `file` as one of `book`'s files (there now), then refreshes the book.
fn attach(conn: &Connection, book: i64, file: &FileRow<'_>) -> Result<(), String> {
    conn.execute(
        "INSERT INTO book_files (path, book, fingerprint, size, mtime, missing, checked) VALUES (?1, ?2, ?3, ?4, ?5, 0, 1)
         ON CONFLICT(path) DO UPDATE SET book = excluded.book, fingerprint = excluded.fingerprint, size = excluded.size,
            mtime = excluded.mtime, missing = 0, checked = 1",
        params![file.path, book, file.fingerprint, file.size, file.mtime],
    )
    .map_err(db_err)?;
    refresh_book(conn, Some(book))
}

/// A book's `missing` and the file it opens, from its files: the file it opened stays while it
/// is there, else the newest one that is. `None`: every book.
fn refresh_book(conn: &Connection, book: Option<i64>) -> Result<(), String> {
    conn.execute(
        "UPDATE books SET
            path = CASE WHEN EXISTS (SELECT 1 FROM book_files f WHERE f.path = books.path AND f.book = books.id AND f.missing = 0) THEN path
                   ELSE COALESCE((SELECT f.path FROM book_files f WHERE f.book = books.id AND f.missing = 0 ORDER BY f.rowid DESC LIMIT 1), path)
                   END,
            missing = NOT EXISTS (SELECT 1 FROM book_files f WHERE f.book = books.id AND f.missing = 0)
         WHERE ?1 IS NULL OR id = ?1",
        [book],
    )
    .map_err(db_err)?;
    Ok(())
}

/// A cached cover as straight (unpremultiplied, opaque) RGBA: (width, height, pixels).
pub fn load_cover(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let bytes = std::fs::read(path).ok()?;
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).ok()?.into_rgba8();
    Some((image.width(), image.height(), image.into_raw()))
}

impl LibraryDb {
    /// Opens (and creates) `<data>/library.sqlite`.
    pub fn open(data: &Path) -> Result<Self, String> {
        Ok(Self { conn: open_connection(data)?, data: data.to_path_buf() })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    /// The connection, for the other library modules.
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Every book in the library: not removed by the user, and one of its files there at the
    /// last scan. Unordered; the view sorts.
    pub fn books(&self) -> Result<Vec<Book>, String> {
        let mut memberships = self.pairs("SELECT book, collection FROM collection_books")?;
        let mut tagged = self.pairs("SELECT book, tag FROM book_tags")?;
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT b.id, b.path, b.book_id, b.title, b.author, b.pages, COALESCE(f.size, 0), b.added, b.last_opened, b.progress, b.page,
                        b.finished, b.cover, b.year
                 FROM books b LEFT JOIN book_files f ON f.path = b.path WHERE b.removed = 0 AND b.missing = 0",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Book {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    book_id: row.get(2)?,
                    title: row.get(3)?,
                    author: row.get(4)?,
                    year: row.get::<_, Option<i64>>(13)?.and_then(|year| i32::try_from(year).ok()),
                    pages: u32::try_from(row.get::<_, i64>(5)?).unwrap_or(0),
                    size: u64::try_from(row.get::<_, i64>(6)?).unwrap_or(0),
                    added: row.get(7)?,
                    last_opened: row.get(8)?,
                    progress: row.get::<_, f64>(9)?.clamp(0.0, 1.0),
                    page: u32::try_from(row.get::<_, i64>(10)?).unwrap_or(0),
                    finished: row.get::<_, i64>(11)? != 0,
                    cover: Cover::from_db(row.get(12)?),
                    collections: Vec::new(),
                    tags: Vec::new(),
                })
            })
            .map_err(db_err)?;
        let mut books = Vec::new();
        for row in rows {
            let mut book = row.map_err(db_err)?;
            book.collections = memberships.remove(&book.id).unwrap_or_default();
            book.tags = tagged.remove(&book.id).unwrap_or_default();
            if !book.progress.is_finite() {
                book.progress = 0.0;
            }
            books.push(book);
        }
        Ok(books)
    }

    /// (book, other) pairs from `sql`, grouped by book.
    fn pairs(&self, sql: &str) -> Result<HashMap<i64, Vec<i64>>, String> {
        let mut grouped: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut stmt = self.conn.prepare_cached(sql).map_err(db_err)?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))).map_err(db_err)?;
        for row in rows {
            let (book, other) = row.map_err(db_err)?;
            grouped.entry(book).or_default().push(other);
        }
        Ok(grouped)
    }

    /// The key of the book the file at `path` (with this fingerprint and size) belongs to, by
    /// its path or else its content; `None` when the library doesn't know it. Changes nothing.
    pub fn book_key(&self, path: &str, fingerprint: &str, size: u64) -> Result<Option<String>, String> {
        let size = i64::try_from(size).unwrap_or(i64::MAX);
        let Some((book, _)) = resolve(&self.conn, path, fingerprint, size, None)? else { return Ok(None) };
        self.conn.query_row("SELECT book_id FROM books WHERE id = ?1", [book], |row| row.get(0)).optional().map_err(db_err)
    }

    /// The reader opened a document: add it (back) to the library, as a file of the book it
    /// belongs to ([`resolve`]: its path, its sidecar's id, its content) or as a new book, and
    /// stamp it opened `now`. The book keeps its key whatever happened to the file (an
    /// annotation saved into the PDF changes the file's fingerprint, not the book's key); its
    /// cover is made again if the file changed.
    pub fn opened(&self, doc: &Opened<'_>, now: i64) -> Result<OpenedBook, String> {
        let size = i64::try_from(doc.size).unwrap_or(i64::MAX);
        let mtime = file_mtime(Path::new(doc.path));
        let (title, author) = (clean_text(doc.title), clean_text(doc.author));
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        let book = match resolve(&tx, doc.path, doc.fingerprint, size, doc.sidecar_id)? {
            Some((book, how)) => {
                let changed = match how {
                    Match::Path => !tx
                        .query_row(
                            "SELECT EXISTS (SELECT 1 FROM book_files WHERE path = ?1 AND size = ?2 AND mtime = ?3)",
                            params![doc.path, size, mtime],
                            |row| row.get::<_, bool>(0),
                        )
                        .map_err(db_err)?,
                    Match::Sidecar => true,
                    Match::Content => false,
                };
                tx.execute(
                    "UPDATE books SET title = CASE WHEN (user_fields | source_fields) & ?6 = 0 THEN ?2 ELSE title END,
                        author = CASE WHEN (user_fields | source_fields) & ?7 = 0 THEN ?3 ELSE author END,
                        pages = ?4, cover = CASE WHEN ?5 THEN 0 ELSE cover END WHERE id = ?1",
                    params![book, title, author, doc.pages, changed, Field::Title.bit(), Field::Authors.bit()],
                )
                .map_err(db_err)?;
                book
            }
            None => insert_book(
                &tx,
                &NewBook { path: doc.path, title: &title, author: &author, pages: doc.pages, added: now, sidecar_id: doc.sidecar_id },
            )?,
        };
        note_sidecar(&tx, book, doc.sidecar_id)?;
        attach(&tx, book, &FileRow { path: doc.path, fingerprint: doc.fingerprint, size, mtime })?;
        tx.execute("UPDATE books SET path = ?2, last_opened = ?3, removed = 0, missing = 0 WHERE id = ?1", params![book, doc.path, now])
            .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        let (book_id, cover): (String, i64) =
            self.conn.query_row("SELECT book_id, cover FROM books WHERE id = ?1", [book], |row| Ok((row.get(0)?, row.get(1)?))).map_err(db_err)?;
        let cached = Cover::from_db(cover) == Cover::Cached && cover_path(&self.data, &book_id).is_file();
        Ok(OpenedBook { id: book, book_id, needs_cover: !cached })
    }

    /// Where the reader is in the book with a file at `path` (page 0-based, `progress` 0..=1).
    pub fn set_progress(&self, path: &str, page: u32, progress: f64) -> Result<(), String> {
        let progress = if progress.is_finite() { progress.clamp(0.0, 1.0) } else { 0.0 };
        self.conn
            .execute(
                "UPDATE books SET page = ?2, progress = ?3 WHERE id = (SELECT book FROM book_files WHERE path = ?1)",
                params![path, page, progress],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn set_finished(&self, id: i64, finished: bool) -> Result<(), String> {
        self.conn.execute("UPDATE books SET finished = ?2 WHERE id = ?1", params![id, finished]).map_err(db_err)?;
        Ok(())
    }

    /// Takes the book out of the library; the file is not touched. A scan of its folder doesn't
    /// bring it back; opening it does.
    pub fn remove(&self, id: i64) -> Result<(), String> {
        self.conn.execute("UPDATE books SET removed = 1 WHERE id = ?1", [id]).map_err(db_err)?;
        self.conn.execute("DELETE FROM collection_books WHERE book = ?1", [id]).map_err(db_err)?;
        Ok(())
    }

    /// The collections, by name, with their sizes.
    pub fn collections(&self) -> Result<Vec<Collection>, String> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT c.id, c.name, COUNT(b.id) FROM collections c
                 LEFT JOIN collection_books cb ON cb.collection = c.id
                 LEFT JOIN books b ON b.id = cb.book AND b.removed = 0 AND b.missing = 0
                 GROUP BY c.id ORDER BY c.name COLLATE NOCASE",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| Ok(Collection { id: row.get(0)?, name: row.get(1)?, count: usize::try_from(row.get::<_, i64>(2)?).unwrap_or(0) }))
            .map_err(db_err)?;
        rows.map(|row| row.map_err(db_err)).collect()
    }

    /// Makes a collection called `name` (or finds the one that exists). Returns its id.
    pub fn create_collection(&self, name: &str, now: i64) -> Result<i64, String> {
        let name: String = clean_text(name).chars().take(MAX_COLLECTION_CHARS).collect();
        if name.is_empty() {
            return Err("a collection needs a name".to_owned());
        }
        self.conn.execute("INSERT OR IGNORE INTO collections (name, created) VALUES (?1, ?2)", params![name, now]).map_err(db_err)?;
        self.conn.query_row("SELECT id FROM collections WHERE name = ?1", [name], |row| row.get(0)).map_err(db_err)
    }

    pub fn delete_collection(&self, id: i64) -> Result<(), String> {
        self.conn.execute("DELETE FROM collections WHERE id = ?1", [id]).map_err(db_err)?;
        Ok(())
    }

    pub fn add_to_collection(&self, collection: i64, book: i64) -> Result<(), String> {
        self.conn.execute("INSERT OR IGNORE INTO collection_books (collection, book) VALUES (?1, ?2)", [collection, book]).map_err(db_err)?;
        Ok(())
    }

    pub fn remove_from_collection(&self, collection: i64, book: i64) -> Result<(), String> {
        self.conn.execute("DELETE FROM collection_books WHERE collection = ?1 AND book = ?2", [collection, book]).map_err(db_err)?;
        Ok(())
    }

    /// The tags, by name, with how many listed books carry each.
    pub fn tags(&self) -> Result<Vec<Tag>, String> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT t.id, t.name, COUNT(b.id) FROM tags t
                 LEFT JOIN book_tags bt ON bt.tag = t.id
                 LEFT JOIN books b ON b.id = bt.book AND b.removed = 0 AND b.missing = 0
                 GROUP BY t.id ORDER BY t.name COLLATE NOCASE",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| Ok(Tag { id: row.get(0)?, name: row.get(1)?, count: usize::try_from(row.get::<_, i64>(2)?).unwrap_or(0) }))
            .map_err(db_err)?;
        rows.map(|row| row.map_err(db_err)).collect()
    }

    /// Tags `book` with `name`, making the tag if no tag has that name in any case. Returns the
    /// tag's id.
    pub fn add_tag(&self, book: i64, name: &str, now: i64) -> Result<i64, String> {
        let tag = tag_id(&self.conn, name, now)?;
        self.conn.execute("INSERT OR IGNORE INTO book_tags (book, tag) VALUES (?1, ?2)", [book, tag]).map_err(db_err)?;
        Ok(tag)
    }

    pub fn remove_tag(&self, book: i64, tag: i64) -> Result<(), String> {
        self.conn.execute("DELETE FROM book_tags WHERE book = ?1 AND tag = ?2", [book, tag]).map_err(db_err)?;
        Ok(())
    }

    /// Deletes the tag from every book.
    pub fn delete_tag(&self, tag: i64) -> Result<(), String> {
        self.conn.execute("DELETE FROM tags WHERE id = ?1", [tag]).map_err(db_err)?;
        Ok(())
    }

    /// Gives `book` exactly the tags `names` (the details sheet's list).
    pub fn set_tags(&self, book: i64, names: &[String], now: i64) -> Result<(), String> {
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        tx.execute("DELETE FROM book_tags WHERE book = ?1", [book]).map_err(db_err)?;
        for name in names {
            let tag = tag_id(&tx, name, now)?;
            tx.execute("INSERT OR IGNORE INTO book_tags (book, tag) VALUES (?1, ?2)", [book, tag]).map_err(db_err)?;
        }
        tx.commit().map_err(db_err)
    }

    /// When `folder` was last scanned (Unix seconds).
    pub fn last_scan(&self, folder: &str) -> Result<Option<i64>, String> {
        self.conn
            .query_row("SELECT last_scan FROM folders WHERE path = ?1", [folder], |row| row.get(0))
            .optional()
            .map(Option::flatten)
            .map_err(db_err)
    }
}

/// The file's modified time in Unix seconds (0 when unknown).
fn file_mtime(path: &Path) -> i64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .map_or(0, |time| time.duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX)))
}

/// What a scan does.
#[derive(Clone, Debug, Default)]
pub struct ScanRequest {
    /// `<data>`.
    pub data: PathBuf,
    /// Folders indexed with their subfolders.
    pub folders: Vec<PathBuf>,
    /// Single files to index too (a document just opened that needs its cover).
    pub files: Vec<PathBuf>,
    /// Threads reading PDFs; 0 = a few, by the machine's cores.
    pub threads: usize,
}

/// How far a scan is. `done` counts files read so far of `to_read`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanProgress {
    /// PDFs found in the folders (and files given).
    pub found: usize,
    /// New or changed files that have to be read.
    pub to_read: usize,
    pub done: usize,
    /// Books committed since the last time the view reloaded (it resets this).
    pub committed: usize,
    pub finished: bool,
    pub cancelled: bool,
    pub error: Option<String>,
}

/// What a finished scan did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub found: usize,
    /// Books read (new or changed) and committed.
    pub indexed: usize,
    /// Covers written.
    pub covers: usize,
    /// Books whose file is gone.
    pub missing: usize,
    pub cancelled: bool,
}

/// A PDF found on disk.
#[derive(Clone, Debug)]
struct Found {
    path: PathBuf,
    size: u64,
    mtime: i64,
}

/// A file the library lists, as a scan compares it.
struct Known {
    /// Its book's row id and key.
    book: i64,
    book_id: String,
    size: i64,
    mtime: i64,
    fingerprint: String,
    /// The fingerprint came from schema version 1 and may be stale: read the file's own.
    unchecked: bool,
    /// Its book's cover.
    cover: Cover,
    removed: bool,
}

/// What a cover render gave.
enum CoverOut {
    /// The first page as JPEG, to write under the book's key once the book is known.
    Jpeg(Vec<u8>),
    Failed,
}

/// A file read on a worker: everything a row needs.
struct Probed {
    path: String,
    size: u64,
    mtime: i64,
    /// The file's fingerprint, or a placeholder when it couldn't be read.
    fingerprint: String,
    title: String,
    author: String,
    pages: u32,
    cover: CoverOut,
    reading: Option<SidecarReading>,
    /// The `doc.book_id` of the file's sidecar.
    sidecar_id: Option<String>,
    /// The DOI and arXiv id the file names ([`detect_ids`]).
    doi: Option<String>,
    arxiv: Option<String>,
}

/// Progress kept in a document's sidecar.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SidecarReading {
    page: u32,
    progress: f64,
    finished: bool,
}

fn sidecar_reading(sidecar: &Sidecar) -> Option<SidecarReading> {
    let progress = sidecar.progress.as_ref()?;
    let page = match &progress.locator {
        Locator::Pdf { page, .. } => *page,
        _ => 0,
    };
    let fraction = if progress.fraction.is_finite() { progress.fraction.clamp(0.0, 1.0) } else { 0.0 };
    Some(SidecarReading { page, progress: fraction, finished: progress.finished })
}

/// What a scan takes from a document's sidecar.
#[derive(Default)]
struct SidecarFacts {
    reading: Option<SidecarReading>,
    /// Its `doc.book_id`.
    book_id: Option<String>,
}

/// `document`'s sidecar, found beside it or in `<data>` under one of `keys`.
fn read_sidecar(document: &Path, keys: &[&str], data: &Path) -> SidecarFacts {
    match store::find_any(document, keys, Some(data)) {
        store::Found::Sidecar { sidecar, .. } => {
            SidecarFacts { reading: sidecar_reading(&sidecar), book_id: sidecar.doc.as_ref().map(|doc| doc.book_id.clone()) }
        }
        store::Found::Missing | store::Found::Locked { .. } => SidecarFacts::default(),
    }
}

/// Every PDF under `folders` (hidden files and folders skipped, links to folders not followed so
/// a loop can't trap the walk), plus `files`.
fn walk(folders: &[PathBuf], files: &[PathBuf], cancel: &AtomicBool) -> Vec<Found> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let mut entries = 0usize;
    let mut stack: Vec<(PathBuf, usize)> = folders.iter().map(|folder| (folder.clone(), 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) || entries >= MAX_ENTRIES {
            break;
        }
        let Ok(listing) = std::fs::read_dir(&dir) else { continue };
        for entry in listing.flatten() {
            entries = entries.saturating_add(1);
            if entries >= MAX_ENTRIES {
                break;
            }
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")) {
                continue;
            }
            push_file(&mut found, &mut seen, path);
        }
    }
    for file in files {
        push_file(&mut found, &mut seen, file.clone());
    }
    found
}

fn push_file(found: &mut Vec<Found>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    // Follows links, as opening the file does.
    let Ok(meta) = std::fs::metadata(&path) else { return };
    if !meta.is_file() || !seen.insert(path.clone()) {
        return;
    }
    let mtime =
        meta.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    found.push(Found { path, size: meta.len(), mtime });
}

/// Reads one PDF: fingerprint, title, author, pages, its sidecar's progress and id, the DOI and
/// arXiv id it names, and its cover. `book_id`: the key of the book the library lists this path
/// under, where its central sidecar is kept.
fn probe(file: &Found, data: &Path, book_id: Option<&str>) -> Probed {
    let path = file.path.to_string_lossy().into_owned();
    let mut probed = Probed {
        path,
        size: file.size,
        mtime: file.mtime,
        // Unreadable: identified by name and size, so it is listed and retried when it changes.
        fingerprint: format!("size-{}", file.size),
        title: title_from_path(&file.path),
        author: String::new(),
        pages: 0,
        cover: CoverOut::Failed,
        reading: None,
        sidecar_id: None,
        doi: None,
        arxiv: None,
    };
    let bytes = if file.size <= MAX_READ_BYTES { std::fs::read(&file.path).ok() } else { None };
    let Some(bytes) = bytes else { return probed };
    probed.fingerprint = store::fingerprint(&bytes);
    let keys: Vec<&str> = book_id.into_iter().chain([probed.fingerprint.as_str()]).collect();
    let sidecar = read_sidecar(&file.path, &keys, data);
    (probed.reading, probed.sidecar_id) = (sidecar.reading, sidecar.book_id);
    let bytes = Arc::new(bytes);
    let info = pdfcraft_render::inspect(bytes.clone(), None).ok();
    if let Some(info) = &info {
        if let Some(title) = info.title.as_deref().map(clean_text).filter(|title| !title.is_empty()) {
            probed.title = title;
        }
        probed.author = info.author.as_deref().map(clean_text).unwrap_or_default();
        probed.pages = u32::try_from(info.pages.len()).unwrap_or(u32::MAX);
    }
    if let Some(first) = info.as_ref().and_then(|info| info.pages.first()) {
        probed.cover = render_cover(&bytes, first.width);
    }
    (probed.doi, probed.arxiv) = detect_ids(&bytes, info.as_ref());
    probed
}

/// The DOI and arXiv id a PDF names, found without asking anyone: in its Info dictionary
/// (publishers often put `doi:…` in the subject), its XMP packet, then its first page's text
/// (arXiv stamps its id in the margin there). Each search is bounded ([`ids`]).
fn detect_ids(bytes: &Arc<Vec<u8>>, info: Option<&pdfcraft_render::DocInfo>) -> (Option<String>, Option<String>) {
    let (mut doi, mut arxiv) = (None, None);
    if let Some(info) = info {
        for text in [info.subject.as_deref(), info.keywords.as_deref(), info.title.as_deref()].into_iter().flatten() {
            doi = doi.or_else(|| ids::find_doi(text));
            arxiv = arxiv.or_else(|| ids::find_arxiv(text));
        }
    }
    if (doi.is_none() || arxiv.is_none())
        && let Some(packet) = ids::xmp_packet(bytes)
    {
        let (xmp_doi, xmp_arxiv) = ids::xmp_ids(packet);
        doi = doi.or(xmp_doi);
        arxiv = arxiv.or(xmp_arxiv);
    }
    if doi.is_none() && arxiv.is_none() && info.is_some_and(|info| !info.pages.is_empty()) {
        let mut renderer = PageRenderer::new(bytes.clone(), RenderConfig::default());
        let page = renderer.render(RenderRequest { page: 0, kind: RequestKind::Text, tile: None, scale: 1.0, tag: 0, preview: false });
        if let Some(text) = page.text {
            let text = text.plain_text();
            (doi, arxiv) = (ids::find_doi(&text), ids::find_arxiv(&text));
        }
    }
    (doi, arxiv)
}

/// Renders the first page `COVER_WIDTH` wide, as JPEG.
fn render_cover(bytes: &Arc<Vec<u8>>, width_pt: f32) -> CoverOut {
    if !width_pt.is_finite() || width_pt <= 0.0 {
        return CoverOut::Failed;
    }
    let mut renderer = PageRenderer::new(bytes.clone(), RenderConfig::default());
    let scale = COVER_WIDTH as f32 / width_pt;
    let page = renderer.render(RenderRequest { page: 0, kind: RequestKind::Pixels, tile: None, scale, tag: 0, preview: false });
    if page.error.is_some() || page.width == 0 || page.height == 0 {
        return CoverOut::Failed;
    }
    match pdfcraft_engine::export::encode_jpeg(page.width, page.height, &page.rgba, COVER_QUALITY) {
        Ok(jpeg) => CoverOut::Jpeg(jpeg),
        Err(_) => CoverOut::Failed,
    }
}

/// Writes a rendered cover as `<data>/cache/thumbs/<book_id>.jpg`.
fn write_cover(cover: &CoverOut, data: &Path, book_id: &str) -> Cover {
    let CoverOut::Jpeg(jpeg) = cover else { return Cover::Failed };
    let out = cover_path(data, book_id);
    if let Some(dir) = out.parent()
        && std::fs::create_dir_all(dir).is_err()
    {
        return Cover::Failed;
    }
    match store::write_atomically(&out, jpeg) {
        Ok(()) => Cover::Cached,
        Err(err) => {
            log::warn!("library: can't write the cover {}: {err}", out.display());
            Cover::Failed
        }
    }
}

fn threads_for(requested: usize) -> usize {
    if requested > 0 {
        return requested.min(MAX_THREADS * 2);
    }
    std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(2).clamp(1, MAX_THREADS))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Indexes `request`'s folders and files into `<data>/library.sqlite`. Synchronous: run it on a
/// worker thread. Stops soon after `cancel` is set, keeping what was committed.
pub fn scan(request: &ScanRequest, cancel: &AtomicBool, progress: &Mutex<ScanProgress>) -> Result<ScanReport, String> {
    run_scan(&Scan { request, cancel, progress, wake: &|| {} })
}

fn run_scan(scan: &Scan<'_>) -> Result<ScanReport, String> {
    let (request, cancel, progress) = (scan.request, scan.cancel, scan.progress);
    let conn = open_connection(&request.data)?;
    let found = walk(&request.folders, &request.files, cancel);
    let mut report = ScanReport { found: found.len(), ..ScanReport::default() };
    let known = known_rows(&conn)?;
    let mut to_read: Vec<(Found, Option<String>)> = Vec::new();
    let mut unchanged: Vec<&Known> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    for file in &found {
        let path = file.path.to_string_lossy().into_owned();
        let size = i64::try_from(file.size).unwrap_or(i64::MAX);
        match known.get(&path) {
            Some(row) if row.removed => {}
            Some(row) if row.size == size && row.mtime == file.mtime => {
                let cover_gone = row.cover == Cover::Cached && !cover_path(&request.data, &row.book_id).is_file();
                if row.cover == Cover::Missing || cover_gone {
                    to_read.push((file.clone(), Some(row.book_id.clone())));
                } else {
                    unchanged.push(row);
                    paths.push(path);
                }
            }
            Some(row) => to_read.push((file.clone(), Some(row.book_id.clone()))),
            None => to_read.push((file.clone(), None)),
        }
    }
    {
        let mut shown = lock(progress);
        shown.found = found.len();
        shown.to_read = to_read.len();
    }
    // Unchanged files: only their sidecar can have moved on (another device, another reader).
    let mut updates = Vec::new();
    for (row, path) in unchanged.iter().zip(&paths) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let fingerprint = if row.unchecked { store::fingerprint_file(Path::new(path)).ok() } else { None };
        let keys = [row.book_id.as_str(), fingerprint.as_deref().unwrap_or(&row.fingerprint)];
        let sidecar = read_sidecar(Path::new(path), &keys, &request.data);
        updates.push(Refresh { book: row.book, path, fingerprint, sidecar });
    }
    commit_refreshes(&conn, &updates)?;
    mark_missing(&conn, &request.folders, &found)?;
    read_files(scan, &conn, to_read, &mut report)?;
    let now = now_unix();
    for folder in &request.folders {
        conn.execute(
            "INSERT INTO folders (path, last_scan) VALUES (?1, ?2) ON CONFLICT(path) DO UPDATE SET last_scan = excluded.last_scan",
            params![folder.to_string_lossy(), now],
        )
        .map_err(db_err)?;
    }
    // Counted last: a file moved within the folders was gone before its new path was read.
    let missing: i64 = conn.query_row("SELECT COUNT(*) FROM books WHERE removed = 0 AND missing = 1", [], |row| row.get(0)).map_err(db_err)?;
    report.missing = usize::try_from(missing).unwrap_or(0);
    report.cancelled = cancel.load(Ordering::Relaxed);
    Ok(report)
}

fn known_rows(conn: &Connection) -> Result<HashMap<String, Known>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT f.path, f.book, b.book_id, f.size, f.mtime, f.fingerprint, f.checked, b.cover, b.removed
             FROM book_files f JOIN books b ON b.id = f.book",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Known {
                    book: row.get(1)?,
                    book_id: row.get(2)?,
                    size: row.get(3)?,
                    mtime: row.get(4)?,
                    fingerprint: row.get(5)?,
                    unchecked: row.get::<_, i64>(6)? == 0,
                    cover: Cover::from_db(row.get(7)?),
                    removed: row.get::<_, i64>(8)? != 0,
                },
            ))
        })
        .map_err(db_err)?;
    rows.map(|row| row.map_err(db_err)).collect()
}

/// What a scan learned about an unchanged file: its sidecar, and its own fingerprint when the
/// one kept came from schema version 1.
struct Refresh<'a> {
    book: i64,
    path: &'a str,
    fingerprint: Option<String>,
    sidecar: SidecarFacts,
}

fn commit_refreshes(conn: &Connection, updates: &[Refresh<'_>]) -> Result<(), String> {
    if updates.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction().map_err(db_err)?;
    {
        let mut reading = tx.prepare_cached("UPDATE books SET page = ?2, progress = ?3, finished = ?4 WHERE id = ?1").map_err(db_err)?;
        let mut checked = tx.prepare_cached("UPDATE book_files SET fingerprint = ?2, checked = 1 WHERE path = ?1").map_err(db_err)?;
        for update in updates {
            if let Some(state) = update.sidecar.reading {
                reading.execute(params![update.book, state.page, state.progress, state.finished]).map_err(db_err)?;
            }
            if let Some(fingerprint) = &update.fingerprint {
                checked.execute(params![update.path, fingerprint]).map_err(db_err)?;
            }
            note_sidecar(&tx, update.book, update.sidecar.book_id.as_deref())?;
        }
    }
    tx.commit().map_err(db_err)
}

/// Files that are gone are marked so, and a book with none left is hidden (kept, with its
/// collections, in case the drive comes back); found again, it shows. A book whose file is gone
/// opens another of its files.
fn mark_missing(conn: &Connection, folders: &[PathBuf], found: &[Found]) -> Result<(), String> {
    let present: HashSet<String> = found.iter().map(|file| file.path.to_string_lossy().into_owned()).collect();
    let mut stmt = conn.prepare("SELECT f.path, f.missing FROM book_files f JOIN books b ON b.id = f.book WHERE b.removed = 0").map_err(db_err)?;
    let rows: Vec<(String, bool)> =
        stmt.query_map([], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0))).map_err(db_err)?.collect::<Result<_, _>>().map_err(db_err)?;
    let tx = conn.unchecked_transaction().map_err(db_err)?;
    for (path, was_missing) in rows {
        let in_folder = folders.iter().any(|folder| Path::new(&path).starts_with(folder));
        let gone = if in_folder { !present.contains(&path) } else { !Path::new(&path).is_file() };
        if gone != was_missing {
            tx.execute("UPDATE book_files SET missing = ?2 WHERE path = ?1", params![path, gone]).map_err(db_err)?;
        }
    }
    refresh_book(&tx, None)?;
    tx.commit().map_err(db_err)
}

/// Reads `files` on a few threads and commits them in batches as they come. A file the library
/// lists comes with its book's key (`Some(book_id)`).
fn read_files(scan: &Scan<'_>, conn: &Connection, files: Vec<(Found, Option<String>)>, report: &mut ScanReport) -> Result<(), String> {
    if files.is_empty() {
        return Ok(());
    }
    let next = AtomicUsize::new(0);
    let threads = threads_for(scan.request.threads).min(files.len());
    let (send, receive) = mpsc::channel::<Probed>();
    let files = &files;
    let cancel = scan.cancel;
    let data = &scan.request.data;
    let mut result = Ok(());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let send = send.clone();
            let next = &next;
            scope.spawn(move || {
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((file, known)) = files.get(index) else { break };
                    if send.send(probe(file, data, known.as_deref())).is_err() {
                        break;
                    }
                }
            });
        }
        drop(send);
        let mut batch = Vec::with_capacity(BATCH);
        let mut since = std::time::Instant::now();
        loop {
            match receive.recv_timeout(BATCH_WAIT) {
                Ok(probed) => {
                    if batch.is_empty() {
                        since = std::time::Instant::now();
                    }
                    batch.push(probed);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if result.is_ok() && !batch.is_empty() && (batch.len() >= BATCH || since.elapsed() >= BATCH_WAIT) {
                result = commit_batch(conn, &mut batch, scan, report);
                (scan.wake)();
                if result.is_err() {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        if result.is_ok() {
            result = commit_batch(conn, &mut batch, scan, report);
        }
    });
    result
}

/// Commits read files: each joins the book it belongs to ([`resolve`]) or starts a new one, and
/// its cover is written under the book's key.
fn commit_batch(conn: &Connection, batch: &mut Vec<Probed>, scan: &Scan<'_>, report: &mut ScanReport) -> Result<(), String> {
    if batch.is_empty() {
        return Ok(());
    }
    let now = now_unix();
    let tx = conn.unchecked_transaction().map_err(db_err)?;
    for probed in batch.iter() {
        let size = i64::try_from(probed.size).unwrap_or(i64::MAX);
        let sidecar_id = probed.sidecar_id.as_deref();
        let book = match resolve(&tx, &probed.path, &probed.fingerprint, size, sidecar_id)? {
            Some((book, _)) => {
                // A title or author the user typed, or an import or lookup filled, stays.
                tx.execute(
                    "UPDATE books SET title = CASE WHEN (user_fields | source_fields) & ?5 = 0 THEN ?2 ELSE title END,
                        author = CASE WHEN (user_fields | source_fields) & ?6 = 0 THEN ?3 ELSE author END, pages = ?4 WHERE id = ?1",
                    params![book, probed.title, probed.author, probed.pages, Field::Title.bit(), Field::Authors.bit()],
                )
                .map_err(db_err)?;
                book
            }
            None => insert_book(
                &tx,
                &NewBook { path: &probed.path, title: &probed.title, author: &probed.author, pages: probed.pages, added: now, sidecar_id },
            )?,
        };
        note_sidecar(&tx, book, sidecar_id)?;
        fill_detected(&tx, book, probed.doi.as_deref(), probed.arxiv.as_deref())?;
        let book_id: String = tx.query_row("SELECT book_id FROM books WHERE id = ?1", [book], |row| row.get(0)).map_err(db_err)?;
        let cover = write_cover(&probed.cover, &scan.request.data, &book_id);
        tx.execute("UPDATE books SET cover = ?2 WHERE id = ?1", params![book, cover.to_db()]).map_err(db_err)?;
        if let Some(state) = probed.reading {
            tx.execute(
                "UPDATE books SET page = ?2, progress = ?3, finished = ?4 WHERE id = ?1",
                params![book, state.page, state.progress, state.finished],
            )
            .map_err(db_err)?;
        }
        attach(&tx, book, &FileRow { path: &probed.path, fingerprint: &probed.fingerprint, size, mtime: probed.mtime })?;
        if cover == Cover::Cached {
            report.covers += 1;
        }
    }
    tx.commit().map_err(db_err)?;
    report.indexed += batch.len();
    let mut shown = lock(scan.progress);
    shown.done = shown.done.saturating_add(batch.len());
    shown.committed = shown.committed.saturating_add(batch.len());
    batch.clear();
    Ok(())
}

/// A scan on its own thread: cancel it, read its progress, take its report when done.
pub struct ScanJob {
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<ScanProgress>>,
    report: Arc<Mutex<Option<Result<ScanReport, String>>>>,
}

impl ScanJob {
    /// Starts `request` on a new thread; `wake` runs after each commit and at the end (the UI
    /// asks for a repaint there).
    pub fn spawn(request: ScanRequest, wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let job = Self::idle();
        let (cancel, progress, report) = (job.cancel.clone(), job.progress.clone(), job.report.clone());
        std::thread::Builder::new().name("margin-library-scan".into()).spawn(move || {
            let result = run_scan(&Scan { request: &request, cancel: &cancel, progress: &progress, wake: &wake });
            finish(&progress, &report, result);
            wake();
        })?;
        Ok(job)
    }

    /// Runs `request` on this thread (tests, embedders without threads).
    pub fn run_inline(request: &ScanRequest) -> Self {
        let job = Self::idle();
        let result = scan(request, &job.cancel, &job.progress);
        finish(&job.progress, &job.report, result);
        job
    }

    fn idle() -> Self {
        Self { cancel: Arc::default(), progress: Arc::default(), report: Arc::default() }
    }

    /// Asks the scan to stop; what it committed stays.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn progress(&self) -> ScanProgress {
        lock(&self.progress).clone()
    }

    /// Takes the count of books committed since the last call (the view reloads when non-zero).
    pub fn take_committed(&self) -> usize {
        std::mem::take(&mut lock(&self.progress).committed)
    }

    /// The report, once the scan ended (taken once). A worker that died without one reports an
    /// error rather than leaving the job running forever.
    pub fn take_report(&self) -> Option<Result<ScanReport, String>> {
        let taken = lock(&self.report).take();
        if taken.is_none() && Arc::strong_count(&self.report) == 1 && !lock(&self.progress).finished {
            lock(&self.progress).finished = true;
            return Some(Err("the library scan stopped unexpectedly".to_owned()));
        }
        taken
    }
}

/// One scan's shared state: what to do, the stop flag, the progress it reports and how it wakes
/// the UI after each commit.
struct Scan<'a> {
    request: &'a ScanRequest,
    cancel: &'a AtomicBool,
    progress: &'a Mutex<ScanProgress>,
    wake: &'a dyn Fn(),
}

fn finish(progress: &Mutex<ScanProgress>, report: &Mutex<Option<Result<ScanReport, String>>>, result: Result<ScanReport, String>) {
    {
        let mut shown = lock(progress);
        shown.finished = true;
        shown.cancelled = result.as_ref().is_ok_and(|report| report.cancelled);
        shown.error = result.as_ref().err().cloned();
    }
    *lock(report) = Some(result);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("margin-library-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A one-page PDF with a title and author, `pages` pages long.
    pub(crate) fn pdf(title: &str, pages: usize) -> Vec<u8> {
        let content = "0 0 1 rg 50 50 100 100 re f\n";
        let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", i + 5)).collect();
        let mut objs = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            format!("<< /Type /Pages /Kids [{}] /Count {pages} /MediaBox [0 0 200 300] >>", kids.join(" ")),
            format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
            format!("<< /Title ({title}) /Author (Ada Lovelace) >>"),
        ];
        objs.extend((0..pages).map(|_| "<< /Type /Page /Parent 2 0 R /Contents 3 0 R >>".to_owned()));
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, obj) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{obj}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
        out
    }

    fn request(root: &Path) -> ScanRequest {
        ScanRequest { data: root.join("data"), folders: vec![root.join("books")], files: Vec::new(), threads: 2 }
    }

    #[test]
    fn scan_indexes_metadata_and_covers_then_skips_unchanged_files() {
        let root = temp_dir("scan");
        let books = root.join("books");
        std::fs::create_dir_all(books.join("sub")).unwrap();
        std::fs::create_dir_all(books.join(".hidden")).unwrap();
        std::fs::write(books.join("a.pdf"), pdf("Alpha", 3)).unwrap();
        std::fs::write(books.join("sub").join("b.PDF"), pdf("Beta", 1)).unwrap();
        std::fs::write(books.join(".hidden").join("c.pdf"), pdf("Hidden", 1)).unwrap();
        std::fs::write(books.join("notes.txt"), "not a pdf").unwrap();
        std::fs::write(books.join("broken.pdf"), "%PDF-1.7 nothing here").unwrap();

        let job = ScanJob::run_inline(&request(&root));
        let report = job.take_report().unwrap().unwrap();
        assert_eq!((report.found, report.indexed, report.covers), (3, 3, 2));
        let db = LibraryDb::open(&root.join("data")).unwrap();
        let mut list = db.books().unwrap();
        list.sort_by(|a, b| a.title.cmp(&b.title));
        let titles: Vec<&str> = list.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(titles, ["Alpha", "Beta", "broken"]);
        assert_eq!((list[0].author.as_str(), list[0].pages), ("Ada Lovelace", 3));
        assert_eq!(list[0].cover, Cover::Cached);
        assert_eq!(list[2].cover, Cover::Failed);
        let (w, h, rgba) = load_cover(&cover_path(&root.join("data"), &list[0].book_id)).unwrap();
        assert_eq!(w, COVER_WIDTH);
        assert_eq!(h, COVER_WIDTH * 3 / 2);
        assert_eq!(rgba.len(), (w * h * 4) as usize);

        let again = ScanJob::run_inline(&request(&root)).take_report().unwrap().unwrap();
        assert_eq!((again.found, again.indexed), (3, 0), "unchanged files are not read again");

        std::fs::remove_file(books.join("sub").join("b.PDF")).unwrap();
        let third = ScanJob::run_inline(&request(&root)).take_report().unwrap().unwrap();
        assert_eq!(third.missing, 1);
        assert_eq!(db.books().unwrap().len(), 2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn progress_comes_from_the_sidecar() {
        let root = temp_dir("sidecar");
        let books = root.join("books");
        std::fs::create_dir_all(&books).unwrap();
        let doc = books.join("read.pdf");
        let bytes = pdf("Read", 10);
        std::fs::write(&doc, &bytes).unwrap();
        let mut sidecar = Sidecar::new();
        sidecar.progress = Some(crate::Progress {
            locator: Locator::Pdf { page: 4, quads: Vec::new(), at: None },
            fraction: 0.45,
            finished: false,
            updated: store::now_rfc3339(),
            extra: Default::default(),
        });
        store::write(&mut sidecar, &doc, &store::fingerprint(&bytes), Some(&root.join("data")), None).unwrap();
        ScanJob::run_inline(&request(&root)).take_report().unwrap().unwrap();
        let db = LibraryDb::open(&root.join("data")).unwrap();
        let book = db.books().unwrap().pop().unwrap();
        assert_eq!((book.page, book.progress), (4, 0.45));
        assert!(book.is_reading());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn opened_finished_removed_and_collections() {
        let root = temp_dir("edit");
        let data = root.join("data");
        let doc = root.join("x.pdf");
        std::fs::write(&doc, pdf("X", 2)).unwrap();
        let db = LibraryDb::open(&data).unwrap();
        let path = doc.to_string_lossy().into_owned();
        let opened = Opened { path: &path, fingerprint: "abc", title: " X\u{0} ", author: "", pages: 2, size: 10, sidecar_id: None };
        let book = db.opened(&opened, 100).unwrap();
        assert!(book.needs_cover);
        assert_eq!(book.book_id.len(), 26, "a new book gets a ULID: {}", book.book_id);
        let id = book.id;
        db.set_progress(&path, 1, 0.5).unwrap();
        db.set_finished(id, true).unwrap();
        let changed = Opened { fingerprint: "def", ..opened.clone() };
        assert_eq!(db.opened(&changed, 150).unwrap().book_id, book.book_id, "a book keeps its identity");
        let book = db.books().unwrap().pop().unwrap();
        assert_eq!((book.title.as_str(), book.last_opened, book.page, book.finished), ("X", Some(150), 1, true));
        assert!(!book.is_reading());

        let shelf = db.create_collection("To read", 1).unwrap();
        assert_eq!(db.create_collection("To read", 2).unwrap(), shelf);
        assert!(db.create_collection("  ", 2).is_err());
        db.add_to_collection(shelf, id).unwrap();
        db.add_to_collection(shelf, id).unwrap();
        assert_eq!(db.collections().unwrap(), vec![Collection { id: shelf, name: "To read".into(), count: 1 }]);
        assert_eq!(db.books().unwrap()[0].collections, vec![shelf]);

        db.remove(id).unwrap();
        assert!(db.books().unwrap().is_empty());
        assert_eq!(db.collections().unwrap()[0].count, 0);
        // A removed book stays out of a folder scan, and opening it brings it back.
        let report = ScanJob::run_inline(&ScanRequest { data: data.clone(), folders: vec![root.clone()], ..Default::default() })
            .take_report()
            .unwrap()
            .unwrap();
        assert_eq!(report.indexed, 0);
        db.opened(&opened, 200).unwrap();
        assert_eq!(db.books().unwrap().len(), 1);
        db.delete_collection(shelf).unwrap();
        assert!(db.collections().unwrap().is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_newer_database_is_left_alone() {
        let root = temp_dir("newer");
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let conn = Connection::open(data.join(LIBRARY_DB)).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        assert!(LibraryDb::open(&data).err().unwrap().contains("newer"));
        std::fs::write(data.join(LIBRARY_DB), "not a database at all, just text").unwrap();
        assert!(LibraryDb::open(&data).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cancelling_keeps_what_was_committed() {
        let root = temp_dir("cancel");
        let books = root.join("books");
        std::fs::create_dir_all(&books).unwrap();
        for i in 0..5 {
            std::fs::write(books.join(format!("{i}.pdf")), pdf(&format!("B{i}"), 1)).unwrap();
        }
        let cancel = AtomicBool::new(true);
        let progress = Mutex::new(ScanProgress::default());
        let report = scan(&request(&root), &cancel, &progress).unwrap();
        assert!(report.cancelled);
        assert_eq!(report.indexed, 0);
        std::fs::remove_dir_all(&root).ok();
    }

    fn rescan(root: &Path) -> ScanReport {
        ScanJob::run_inline(&request(root)).take_report().unwrap().unwrap()
    }

    fn only_book(db: &LibraryDb) -> Book {
        let mut books = db.books().unwrap();
        assert_eq!(books.len(), 1, "{books:#?}");
        books.pop().unwrap()
    }

    /// Appends an incremental-update-like tail: new bytes, new size, the same document.
    fn edit(path: &Path) {
        let mut bytes = std::fs::read(path).unwrap();
        bytes.extend_from_slice(b"\n% edited by another app\n");
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn moving_renaming_and_editing_keep_the_book() {
        let root = temp_dir("identity-move");
        let books = root.join("books");
        std::fs::create_dir_all(books.join("sub")).unwrap();
        let first = books.join("a.pdf");
        std::fs::write(&first, pdf("Alpha", 4)).unwrap();
        rescan(&root);
        let db = LibraryDb::open(&root.join("data")).unwrap();
        let book = only_book(&db);
        let shelf = db.create_collection("Papers", 1).unwrap();
        db.add_to_collection(shelf, book.id).unwrap();
        db.set_finished(book.id, true).unwrap();
        let first_path = first.to_string_lossy().into_owned();
        let opened = Opened {
            path: &first_path,
            fingerprint: &store::fingerprint_file(&first).unwrap(),
            title: "Alpha",
            author: "",
            pages: 4,
            size: std::fs::metadata(&first).unwrap().len(),
            sidecar_id: None,
        };
        db.opened(&opened, 500).unwrap();
        let same = |db: &LibraryDb, path: &Path, why: &str| {
            let now = only_book(db);
            assert_eq!(now.book_id, book.book_id, "{why}");
            assert_eq!(now.id, book.id, "{why}");
            assert_eq!(now.path, path.to_string_lossy(), "{why}");
            assert_eq!((now.collections.as_slice(), now.finished, now.added, now.last_opened), (&[shelf][..], true, book.added, Some(500)), "{why}");
            assert_eq!(now.cover, Cover::Cached, "{why}");
            assert!(cover_path(&root.join("data"), &now.book_id).is_file(), "{why}");
        };

        let renamed = books.join("Alpha (2019).pdf");
        std::fs::rename(&first, &renamed).unwrap();
        rescan(&root);
        same(&db, &renamed, "renamed");

        let moved = books.join("sub").join("alpha.pdf");
        std::fs::rename(&renamed, &moved).unwrap();
        rescan(&root);
        same(&db, &moved, "moved to a subfolder");

        edit(&moved);
        rescan(&root);
        same(&db, &moved, "edited in place");

        // Moved and edited at once: the content no longer matches, the sidecar that moved along
        // still names the book.
        let mut sidecar = Sidecar::new();
        sidecar.doc = Some(crate::SidecarDoc {
            book_id: book.book_id.clone(),
            sidecar_fingerprint: String::new(),
            size: 0,
            file_revision_hash: None,
            format: "pdf".into(),
            pages: None,
            title: None,
            path_hint: None,
            extra: Default::default(),
        });
        let last = books.join("alpha-annotated.pdf");
        std::fs::rename(&moved, &last).unwrap();
        edit(&last);
        store::write(&mut sidecar, &last, &book.book_id, None, None).unwrap();
        rescan(&root);
        same(&db, &last, "moved and edited, with its sidecar");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_copy_is_the_same_book_and_a_missing_file_comes_back() {
        let root = temp_dir("identity-copy");
        let books = root.join("books");
        std::fs::create_dir_all(books.join("other")).unwrap();
        let original = books.join("paper.pdf");
        let copy = books.join("other").join("paper copy.pdf");
        std::fs::write(&original, pdf("Paper", 2)).unwrap();
        rescan(&root);
        std::fs::copy(&original, &copy).unwrap();
        let report = rescan(&root);
        assert_eq!(report.found, 2);
        let db = LibraryDb::open(&root.join("data")).unwrap();
        let book = only_book(&db);
        assert_eq!(book.path, original.to_string_lossy(), "the first file stays the one opened");

        std::fs::remove_file(&original).unwrap();
        rescan(&root);
        let still = only_book(&db);
        assert_eq!((still.id, still.path.as_str()), (book.id, &*copy.to_string_lossy()), "the copy stands in");

        let parked = root.join("parked.pdf");
        std::fs::rename(&copy, &parked).unwrap();
        assert_eq!(rescan(&root).missing, 1);
        assert!(db.books().unwrap().is_empty(), "a book with no file is hidden");
        std::fs::rename(&parked, &original).unwrap();
        assert_eq!(rescan(&root).missing, 0);
        assert_eq!(only_book(&db).book_id, book.book_id, "back where it was, the same book");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sidecar_ids_name_books_only_when_safe() {
        let root = temp_dir("identity-key");
        let data = root.join("data");
        let db = LibraryDb::open(&data).unwrap();
        fn opened<'a>(path: &'a str, sidecar_id: Option<&'a str>) -> Opened<'a> {
            Opened { path, fingerprint: "0123456789abcdef0123456789abcdef", title: "T", author: "", pages: 1, size: 1, sidecar_id }
        }
        let kept = db.opened(&opened("/a/one.pdf", Some("01HXYZSIDECARID0000000000A")), 1).unwrap();
        assert_eq!(kept.book_id, "01HXYZSIDECARID0000000000A", "a new book takes its sidecar's id");
        let hostile = db.opened(&opened("/b/two.pdf", Some("../../escape")), 1).unwrap();
        assert_eq!(hostile.book_id, kept.book_id, "an unusable id is ignored; the content still matches");
        let other = Opened { fingerprint: "ffffffffffffffffffffffffffffffff", ..opened("/c/three.pdf", Some("../../escape")) };
        let fresh = db.opened(&other, 1).unwrap();
        assert!(usable_key(&fresh.book_id) && fresh.book_id != kept.book_id);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_version_1_library_is_upgraded_without_losing_anything() {
        let root = temp_dir("upgrade-v1");
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let path = data.join(LIBRARY_DB);
        {
            // The file as Margin wrote it before book keys: schema 1, rows by path.
            let conn = Connection::open(&path).unwrap();
            configure(&conn).unwrap();
            migrate::upgrade(&conn, &path, &migrate::Schema { name: "test", steps: &library_schema::STEPS[..1] }).unwrap();
            let fp_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
            let fp_b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
            let insert = "INSERT INTO books (id, path, fingerprint, title, author, pages, size, mtime, added, last_opened, progress, page, finished,
                          removed, missing, cover) VALUES (?1, ?2, ?3, ?4, 'Ada', 10, 100, 7, ?5, ?6, ?7, ?8, ?9, ?10, 0, 1)";
            conn.execute(insert, params![1, "/lib/a.pdf", fp_a, "Alpha", 50, 300, 0.25, 2, false, false]).unwrap();
            conn.execute(insert, params![2, "/usb/a-copy.pdf", fp_a, "Alpha", 20, 400, 0.75, 7, true, false]).unwrap();
            conn.execute(insert, params![3, "/lib/b.pdf", fp_b, "Beta", 60, None::<i64>, 0.0, 0, false, true]).unwrap();
            conn.execute(insert, params![4, "/lib/broken1.pdf", "size-100", "Broken one", 70, None::<i64>, 0.0, 0, false, false]).unwrap();
            conn.execute(insert, params![5, "/lib/broken2.pdf", "size-100", "Broken two", 80, None::<i64>, 0.0, 0, false, false]).unwrap();
            conn.execute("INSERT INTO collections (id, name, created) VALUES (1, 'Home', 1), (2, 'Travel', 1)", []).unwrap();
            conn.execute("INSERT INTO collection_books (collection, book) VALUES (1, 1), (2, 2), (1, 4)", []).unwrap();
            conn.execute("INSERT INTO folders (path, last_scan) VALUES ('/lib', 99)", []).unwrap();
        }
        let db = LibraryDb::open(&data).unwrap();
        assert!(data.join("library.sqlite.v1.bak").is_file(), "a copy of the old file is kept");
        let mut books = db.books().unwrap();
        books.sort_by_key(|book| book.id);
        let summary: Vec<(i64, &str, &str, &str)> = books.iter().map(|b| (b.id, b.book_id.as_str(), b.path.as_str(), b.title.as_str())).collect();
        assert_eq!(
            summary,
            [
                (2, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "/usb/a-copy.pdf", "Alpha"),
                (4, "size-100", "/lib/broken1.pdf", "Broken one"),
                (5, "size-100-5", "/lib/broken2.pdf", "Broken two"),
            ]
        );
        let alpha = &books[0];
        assert_eq!((alpha.added, alpha.last_opened, alpha.page, alpha.finished), (20, Some(400), 7, true), "the copies are merged");
        assert_eq!((alpha.progress, alpha.size, alpha.cover), (0.75, 100, Cover::Cached));
        let mut collections = alpha.collections.clone();
        collections.sort_unstable();
        assert_eq!(collections, [1, 2]);
        assert_eq!(books[1].collections, [1]);
        assert_eq!(db.collections().unwrap().iter().map(|c| c.count).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(db.last_scan("/lib").unwrap(), Some(99));
        let files: i64 = db.conn.query_row("SELECT COUNT(*) FROM book_files", [], |row| row.get(0)).unwrap();
        assert_eq!(files, 5, "every path is still known");
        let missing: (String, bool) =
            db.conn.query_row("SELECT book_id, removed FROM books WHERE id = 3", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(missing, ("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(), true), "a removed book is kept");
        let key = db.book_key("/usb/a-copy.pdf", "", 100).unwrap();
        assert_eq!(key.as_deref(), Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), "stats and covers keep their key");
        // Through versions 3 to 5: tags, details and the full-text index, over the same rows.
        let version: i64 = db.conn.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
        assert_eq!(version, 5);
        let details = db.details(2).unwrap();
        assert_eq!((details.title.as_str(), details.authors.as_str(), details.year, details.doi), ("Alpha", "Ada", None, None));
        let mut found = crate::library_text::search_books(&db.conn, "broken").unwrap();
        found.sort_unstable();
        assert_eq!(found, [4, 5], "the books already listed are in the title index");
        db.add_tag(4, "Revisit later", 1).unwrap();
        assert_eq!(crate::library_text::search_books(&db.conn, "revisit").unwrap(), [4], "tags are found too");
        drop(db);
        assert!(LibraryDb::open(&data).is_ok(), "opening again changes nothing");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tags_are_shared_by_name_and_follow_their_books() {
        let root = temp_dir("tags");
        let db = LibraryDb::open(&root.join("data")).unwrap();
        let book = |path: &'static str, fingerprint: &'static str| {
            let opened = Opened { path, fingerprint, title: path, author: "", pages: 1, size: 1, sidecar_id: None };
            db.opened(&opened, 1).unwrap().id
        };
        let (a, b) = (book("/x/a.pdf", "11111111111111111111111111111111"), book("/x/b.pdf", "22222222222222222222222222222222"));
        let ml = db.add_tag(a, " ML ", 1).unwrap();
        assert_eq!(db.add_tag(b, "ml", 2).unwrap(), ml, "one tag whatever the case");
        assert!(db.add_tag(a, " \u{7} ", 2).is_err(), "a blank name is refused");
        db.add_tag(a, &"x".repeat(500), 2).unwrap();
        let tags = db.tags().unwrap();
        assert_eq!(tags.iter().map(|t| (t.name.chars().count(), t.count)).collect::<Vec<_>>(), [(2, 2), (MAX_TAG_CHARS, 1)]);
        assert_eq!(tags[0].name, "ML", "the first spelling stays");
        db.set_tags(b, &["Reading".into(), "ml".into()], 3).unwrap();
        let listed: HashMap<i64, Vec<i64>> = db.books().unwrap().into_iter().map(|book| (book.id, book.tags)).collect();
        assert_eq!(listed[&b].len(), 2);
        db.remove_tag(a, ml).unwrap();
        assert_eq!(db.tags().unwrap().iter().find(|t| t.id == ml).map(|t| t.count), Some(1));
        db.delete_tag(ml).unwrap();
        let listed: HashMap<i64, Vec<i64>> = db.books().unwrap().into_iter().map(|book| (book.id, book.tags)).collect();
        assert_eq!(listed[&b].len(), 1, "a deleted tag leaves every book");
        std::fs::remove_dir_all(&root).ok();
    }
}
