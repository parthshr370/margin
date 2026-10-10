//! `<data>/library.sqlite`: the books Margin knows (path, sidecar fingerprint, title, author,
//! pages, when added and last opened, progress, finished), the collections they are in, and
//! their covers (`<data>/cache/thumbs/<fingerprint>.jpg`, the first page through PdfCraft's
//! renderer).
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

use crate::store;
use crate::{Locator, Sidecar};

/// The library database inside `<data>`.
pub const LIBRARY_DB: &str = "library.sqlite";
/// Cached covers inside `<data>`.
pub const THUMBS_DIR: &str = "cache/thumbs";
/// Cover width in pixels: a grid cell's width at a device pixel ratio of 2.
pub const COVER_WIDTH: u32 = 240;
/// Covers are small, so a modest JPEG quality is invisible at that size.
const COVER_QUALITY: u8 = 80;
/// The schema version (`PRAGMA user_version`).
const SCHEMA_VERSION: i64 = 1;
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

/// Whether a book's cover is in the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cover {
    /// Not made yet.
    Missing,
    /// In `<data>/cache/thumbs/<fingerprint>.jpg`.
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
    pub path: String,
    /// The sidecar fingerprint (KOReader's partial MD5): the book's identity in `stats.sqlite`.
    pub fingerprint: String,
    pub title: String,
    pub author: String,
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

/// What the reader knows about a document it opened.
#[derive(Clone, Debug)]
pub struct Opened<'a> {
    pub path: &'a str,
    pub fingerprint: &'a str,
    pub title: &'a str,
    pub author: &'a str,
    pub pages: u32,
    pub size: u64,
}

/// A book the reader opened, as the library lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedBook {
    pub id: i64,
    /// The book's identity (stats are kept under it).
    pub fingerprint: String,
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

fn db_err(err: rusqlite::Error) -> String {
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

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS books (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    fingerprint TEXT NOT NULL,
    title TEXT NOT NULL,
    author TEXT NOT NULL DEFAULT '',
    pages INTEGER NOT NULL DEFAULT 0,
    size INTEGER NOT NULL DEFAULT 0,
    mtime INTEGER NOT NULL DEFAULT 0,
    added INTEGER NOT NULL,
    last_opened INTEGER,
    progress REAL NOT NULL DEFAULT 0,
    page INTEGER NOT NULL DEFAULT 0,
    finished INTEGER NOT NULL DEFAULT 0,
    removed INTEGER NOT NULL DEFAULT 0,
    missing INTEGER NOT NULL DEFAULT 0,
    cover INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS books_fingerprint ON books(fingerprint);
CREATE TABLE IF NOT EXISTS folders (
    path TEXT PRIMARY KEY,
    last_scan INTEGER
);
CREATE TABLE IF NOT EXISTS collections (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    created INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS collection_books (
    collection INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    book INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    PRIMARY KEY (collection, book)
);
";

fn open_connection(data: &Path) -> Result<Connection, String> {
    std::fs::create_dir_all(data).map_err(|err| format!("can't create {}: {err}", data.display()))?;
    let conn = Connection::open(data.join(LIBRARY_DB)).map_err(db_err)?;
    configure(&conn)?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(db_err)?;
    if version > SCHEMA_VERSION {
        return Err(format!("library database: written by a newer Margin (schema {version}); not changed"));
    }
    conn.execute_batch(SCHEMA).map_err(db_err)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION).map_err(db_err)?;
    Ok(conn)
}

/// `text` without control characters, trimmed and capped (PDF metadata is untrusted).
fn clean_text(text: &str) -> String {
    text.trim().chars().filter(|c| !c.is_control()).take(MAX_TEXT_CHARS).collect()
}

/// The file name without `.pdf`: the title of a PDF without one.
pub fn title_from_path(path: &Path) -> String {
    path.file_stem().map_or_else(|| path.to_string_lossy().into_owned(), |stem| stem.to_string_lossy().into_owned())
}

/// Where the cover of the book with `fingerprint` is cached.
pub fn cover_path(data: &Path, fingerprint: &str) -> PathBuf {
    data.join(THUMBS_DIR).join(format!("{fingerprint}.jpg"))
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

    /// `<data>`.
    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    /// Every book in the library: not removed by the user, and its file still there at the last
    /// scan. Unordered; the view sorts.
    pub fn books(&self) -> Result<Vec<Book>, String> {
        let mut memberships: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut stmt = self.conn.prepare_cached("SELECT book, collection FROM collection_books").map_err(db_err)?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))).map_err(db_err)?;
        for row in rows {
            let (book, collection) = row.map_err(db_err)?;
            memberships.entry(book).or_default().push(collection);
        }
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT id, path, fingerprint, title, author, pages, size, added, last_opened, progress, page, finished, cover
                 FROM books WHERE removed = 0 AND missing = 0",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Book {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    fingerprint: row.get(2)?,
                    title: row.get(3)?,
                    author: row.get(4)?,
                    pages: u32::try_from(row.get::<_, i64>(5)?).unwrap_or(0),
                    size: u64::try_from(row.get::<_, i64>(6)?).unwrap_or(0),
                    added: row.get(7)?,
                    last_opened: row.get(8)?,
                    progress: row.get::<_, f64>(9)?.clamp(0.0, 1.0),
                    page: u32::try_from(row.get::<_, i64>(10)?).unwrap_or(0),
                    finished: row.get::<_, i64>(11)? != 0,
                    cover: Cover::from_db(row.get(12)?),
                    collections: Vec::new(),
                })
            })
            .map_err(db_err)?;
        let mut books = Vec::new();
        for row in rows {
            let mut book = row.map_err(db_err)?;
            book.collections = memberships.remove(&book.id).unwrap_or_default();
            if !book.progress.is_finite() {
                book.progress = 0.0;
            }
            books.push(book);
        }
        Ok(books)
    }

    /// The reader opened a document: add it (back) to the library and stamp it opened `now`. A
    /// book already listed at this path keeps its fingerprint (an annotation saved into the PDF
    /// changes the file's, not the book's), and its cover is made again if the file changed.
    pub fn opened(&self, doc: &Opened<'_>, now: i64) -> Result<OpenedBook, String> {
        let size = i64::try_from(doc.size).unwrap_or(i64::MAX);
        let mtime = file_mtime(Path::new(doc.path));
        self.conn
            .execute(
                "INSERT INTO books (path, fingerprint, title, author, pages, size, mtime, added, last_opened)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
                 ON CONFLICT(path) DO UPDATE SET
                    cover = CASE WHEN size = excluded.size AND mtime = excluded.mtime THEN cover ELSE 0 END,
                    title = excluded.title, author = excluded.author,
                    pages = excluded.pages, size = excluded.size, mtime = excluded.mtime,
                    last_opened = excluded.last_opened, removed = 0, missing = 0",
                params![doc.path, doc.fingerprint, clean_text(doc.title), clean_text(doc.author), doc.pages, size, mtime, now],
            )
            .map_err(db_err)?;
        let (id, fingerprint, cover): (i64, String, i64) = self
            .conn
            .query_row("SELECT id, fingerprint, cover FROM books WHERE path = ?1", [doc.path], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(db_err)?;
        let cached = Cover::from_db(cover) == Cover::Cached && cover_path(&self.data, &fingerprint).is_file();
        Ok(OpenedBook { id, fingerprint, needs_cover: !cached })
    }

    /// Where the reader is in the book at `path` (page 0-based, `progress` 0..=1).
    pub fn set_progress(&self, path: &str, page: u32, progress: f64) -> Result<(), String> {
        let progress = if progress.is_finite() { progress.clamp(0.0, 1.0) } else { 0.0 };
        self.conn.execute("UPDATE books SET page = ?2, progress = ?3 WHERE path = ?1", params![path, page, progress]).map_err(db_err)?;
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

/// The library's row for a path, as a scan compares it.
struct Known {
    size: i64,
    mtime: i64,
    fingerprint: String,
    cover: Cover,
    removed: bool,
}

/// A file read on a worker: everything a row needs.
struct Probed {
    path: String,
    size: u64,
    mtime: i64,
    fingerprint: String,
    title: String,
    author: String,
    pages: u32,
    cover: Cover,
    reading: Option<SidecarReading>,
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

/// The reading progress in `document`'s sidecar, if it has one.
fn read_sidecar(document: &Path, fingerprint: &str, data: &Path) -> Option<SidecarReading> {
    match store::find(document, fingerprint, Some(data)) {
        store::Found::Sidecar { sidecar, .. } => sidecar_reading(&sidecar),
        store::Found::Missing | store::Found::Locked { .. } => None,
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

/// Reads one PDF: title, author, pages, sidecar progress and its cover. A book the library knows
/// keeps its fingerprint (`known`) even when the file changed, so its stats and cover stay its own.
fn probe(file: &Found, data: &Path, known: Option<&str>) -> Probed {
    let path = file.path.to_string_lossy().into_owned();
    let mut probed = Probed {
        path,
        size: file.size,
        mtime: file.mtime,
        fingerprint: known.map(str::to_owned).unwrap_or_default(),
        title: title_from_path(&file.path),
        author: String::new(),
        pages: 0,
        cover: Cover::Failed,
        reading: None,
    };
    let bytes = if file.size <= MAX_READ_BYTES { std::fs::read(&file.path).ok() } else { None };
    let Some(bytes) = bytes else {
        // Unreadable now: identified by name and size, so it is listed and retried when it changes.
        if probed.fingerprint.is_empty() {
            probed.fingerprint = format!("size-{}", file.size);
        }
        return probed;
    };
    let actual = store::fingerprint(&bytes);
    probed.reading = read_sidecar(&file.path, &actual, data);
    if probed.fingerprint.is_empty() {
        probed.fingerprint = actual;
    }
    let bytes = Arc::new(bytes);
    let info = pdfcraft_render::inspect(bytes.clone(), None).ok();
    if let Some(info) = &info {
        if let Some(title) = info.title.as_deref().map(clean_text).filter(|title| !title.is_empty()) {
            probed.title = title;
        }
        probed.author = info.author.as_deref().map(clean_text).unwrap_or_default();
        probed.pages = u32::try_from(info.pages.len()).unwrap_or(u32::MAX);
    }
    probed.cover = match info.as_ref().and_then(|info| info.pages.first()) {
        Some(first) => write_cover(&bytes, first.width, &cover_path(data, &probed.fingerprint)),
        None => Cover::Failed,
    };
    probed
}

/// Renders the first page `COVER_WIDTH` wide and writes it as JPEG to `out`.
fn write_cover(bytes: &Arc<Vec<u8>>, width_pt: f32, out: &Path) -> Cover {
    if !width_pt.is_finite() || width_pt <= 0.0 {
        return Cover::Failed;
    }
    let mut renderer = PageRenderer::new(bytes.clone(), RenderConfig::default());
    let scale = COVER_WIDTH as f32 / width_pt;
    let page = renderer.render(RenderRequest { page: 0, kind: RequestKind::Pixels, tile: None, scale, tag: 0 });
    if page.error.is_some() || page.width == 0 || page.height == 0 {
        return Cover::Failed;
    }
    let Ok(jpeg) = pdfcraft_engine::export::encode_jpeg(page.width, page.height, &page.rgba, COVER_QUALITY) else { return Cover::Failed };
    if let Some(dir) = out.parent()
        && std::fs::create_dir_all(dir).is_err()
    {
        return Cover::Failed;
    }
    match store::write_atomically(out, &jpeg) {
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
    let mut unchanged: Vec<(String, String)> = Vec::new();
    for file in &found {
        let path = file.path.to_string_lossy().into_owned();
        let size = i64::try_from(file.size).unwrap_or(i64::MAX);
        match known.get(&path) {
            Some(row) if row.removed => {}
            Some(row) if row.size == size && row.mtime == file.mtime => {
                let cover_gone = row.cover == Cover::Cached && !cover_path(&request.data, &row.fingerprint).is_file();
                if row.cover == Cover::Missing || cover_gone {
                    to_read.push((file.clone(), Some(row.fingerprint.clone())));
                } else {
                    unchanged.push((path, row.fingerprint.clone()));
                }
            }
            Some(row) => to_read.push((file.clone(), Some(row.fingerprint.clone()))),
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
    for (path, fingerprint) in &unchanged {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if let Some(reading) = read_sidecar(Path::new(path), fingerprint, &request.data) {
            updates.push((path.clone(), reading));
        }
    }
    commit_readings(&conn, &updates)?;
    report.missing = mark_missing(&conn, &request.folders, &found)?;
    read_files(scan, &conn, to_read, &mut report)?;
    let now = now_unix();
    for folder in &request.folders {
        conn.execute(
            "INSERT INTO folders (path, last_scan) VALUES (?1, ?2) ON CONFLICT(path) DO UPDATE SET last_scan = excluded.last_scan",
            params![folder.to_string_lossy(), now],
        )
        .map_err(db_err)?;
    }
    report.cancelled = cancel.load(Ordering::Relaxed);
    Ok(report)
}

fn known_rows(conn: &Connection) -> Result<HashMap<String, Known>, String> {
    let mut stmt = conn.prepare("SELECT path, size, mtime, fingerprint, cover, removed FROM books").map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Known {
                    size: row.get(1)?,
                    mtime: row.get(2)?,
                    fingerprint: row.get(3)?,
                    cover: Cover::from_db(row.get(4)?),
                    removed: row.get::<_, i64>(5)? != 0,
                },
            ))
        })
        .map_err(db_err)?;
    rows.map(|row| row.map_err(db_err)).collect()
}

fn commit_readings(conn: &Connection, updates: &[(String, SidecarReading)]) -> Result<(), String> {
    if updates.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction().map_err(db_err)?;
    {
        let mut stmt = tx.prepare_cached("UPDATE books SET page = ?2, progress = ?3, finished = ?4 WHERE path = ?1").map_err(db_err)?;
        for (path, reading) in updates {
            stmt.execute(params![path, reading.page, reading.progress, reading.finished]).map_err(db_err)?;
        }
    }
    tx.commit().map_err(db_err)
}

/// Books whose file is gone are hidden (kept, with their collections, in case the drive comes
/// back); books found again are shown. Returns how many are missing.
fn mark_missing(conn: &Connection, folders: &[PathBuf], found: &[Found]) -> Result<usize, String> {
    let present: HashSet<String> = found.iter().map(|file| file.path.to_string_lossy().into_owned()).collect();
    let mut stmt = conn.prepare("SELECT id, path, missing FROM books WHERE removed = 0").map_err(db_err)?;
    let rows: Vec<(i64, String, bool)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? != 0)))
        .map_err(db_err)?
        .collect::<Result<_, _>>()
        .map_err(db_err)?;
    let mut missing = 0;
    let tx = conn.unchecked_transaction().map_err(db_err)?;
    for (id, path, was_missing) in rows {
        let in_folder = folders.iter().any(|folder| Path::new(&path).starts_with(folder));
        let gone = if in_folder { !present.contains(&path) } else { !Path::new(&path).is_file() };
        if gone {
            missing += 1;
        }
        if gone != was_missing {
            tx.execute("UPDATE books SET missing = ?2 WHERE id = ?1", params![id, gone]).map_err(db_err)?;
        }
    }
    tx.commit().map_err(db_err)?;
    Ok(missing)
}

/// Reads `files` on a few threads and commits them in batches as they come. A file the library
/// knows keeps its identity (`Some(fingerprint)`).
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
                result = commit_batch(conn, &mut batch, scan.progress, report);
                (scan.wake)();
                if result.is_err() {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        if result.is_ok() {
            result = commit_batch(conn, &mut batch, scan.progress, report);
        }
    });
    result
}

fn commit_batch(conn: &Connection, batch: &mut Vec<Probed>, progress: &Mutex<ScanProgress>, report: &mut ScanReport) -> Result<(), String> {
    if batch.is_empty() {
        return Ok(());
    }
    let now = now_unix();
    let tx = conn.unchecked_transaction().map_err(db_err)?;
    {
        let mut upsert = tx
            .prepare_cached(
                "INSERT INTO books (path, fingerprint, title, author, pages, size, mtime, added, cover)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(path) DO UPDATE SET
                    fingerprint = excluded.fingerprint, title = excluded.title, author = excluded.author,
                    pages = excluded.pages, size = excluded.size, mtime = excluded.mtime, cover = excluded.cover,
                    missing = 0",
            )
            .map_err(db_err)?;
        let mut reading = tx.prepare_cached("UPDATE books SET page = ?2, progress = ?3, finished = ?4 WHERE path = ?1").map_err(db_err)?;
        for probed in batch.iter() {
            let size = i64::try_from(probed.size).unwrap_or(i64::MAX);
            upsert
                .execute(params![
                    probed.path,
                    probed.fingerprint,
                    probed.title,
                    probed.author,
                    probed.pages,
                    size,
                    probed.mtime,
                    now,
                    probed.cover.to_db()
                ])
                .map_err(db_err)?;
            if let Some(state) = probed.reading {
                reading.execute(params![probed.path, state.page, state.progress, state.finished]).map_err(db_err)?;
            }
            if probed.cover == Cover::Cached {
                report.covers += 1;
            }
        }
    }
    tx.commit().map_err(db_err)?;
    report.indexed += batch.len();
    let mut shown = lock(progress);
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
        let (w, h, rgba) = load_cover(&cover_path(&root.join("data"), &list[0].fingerprint)).unwrap();
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
        let opened = Opened { path: &path, fingerprint: "abc", title: " X\u{0} ", author: "", pages: 2, size: 10 };
        let book = db.opened(&opened, 100).unwrap();
        assert!(book.needs_cover);
        assert_eq!(book.fingerprint, "abc");
        let id = book.id;
        db.set_progress(&path, 1, 0.5).unwrap();
        db.set_finished(id, true).unwrap();
        let changed = Opened { fingerprint: "def", ..opened.clone() };
        assert_eq!(db.opened(&changed, 150).unwrap().fingerprint, "abc", "a book keeps its identity");
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
}
