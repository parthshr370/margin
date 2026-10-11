//! Full-text search over the library (`library.sqlite`, schema 5). Two FTS5 tables:
//! `book_fts` holds each book's title, authors, tags and details, kept up to date by triggers;
//! `text_fts` holds each book's page text (PdfCraft's reading-order text) and the text its PDF
//! highlights mark, with their comments.
//!
//! Page text is read in the background by a [`TextJob`], a cancellable worker apart from the
//! folder scan, after scans end: only books whose file changed since they were last read
//! (fingerprint, size, modified time) are read again. A book's text is capped, so one huge file
//! can't fill the database, and committed one book at a time, so cancelling keeps what was done.
//! A `text_fts` row's id is `book << 20 | n`, so a book's rows are replaced by id range without
//! scanning the table.
//!
//! Searching the text runs on its own thread too ([`TextSearch`]): a common word in a large
//! library can match many rows, and the UI must not wait for the ranking.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use pdfcraft_render::{Annotation, PageInfo, PageRenderer, PageText, RenderConfig, RenderRequest, RequestKind};
use rusqlite::{Connection, InterruptHandle, Transaction, params};

use crate::library::{LibraryDb, db_err, now_unix, open_connection};

/// Pages read per book at most.
const MAX_PAGES: usize = 2_000;
/// Highlights read per book at most.
const MAX_HIGHLIGHTS: usize = 10_000;
/// Text kept per book at most, in bytes; the rest of a longer book is not searchable.
const MAX_BOOK_TEXT: usize = 4 << 20;
/// Largest file whose text is read.
const MAX_FILE_BYTES: u64 = 256 << 20;
/// Threads reading text: few, as the pass runs long and in the background.
const THREADS: usize = 2;
/// Bits of a `text_fts` row id that number the rows of one book.
const ROW_BITS: u32 = 20;
/// Most places a text search returns.
pub const MAX_HITS: usize = 200;
/// Most books a title search returns.
const MAX_BOOK_HITS: usize = 10_000;
/// Words a search reads at most.
const MAX_QUERY_WORDS: usize = 16;
/// Words a snippet shows around a match.
const SNIPPET_WORDS: i64 = 12;
/// Characters that mark the matched words in a snippet: start and end.
pub const MATCH_START: char = '\u{2}';
pub const MATCH_END: char = '\u{3}';
/// The markup comments whose marked text is indexed (ISO 32000-2 §12.5.6.10).
const MARKUP: [&str; 4] = ["Highlight", "Underline", "StrikeOut", "Squiggly"];

/// Schema 5's tables and triggers, and `book_fts` filled from the books already listed. The
/// trigger's `1048576` is `1 << ROW_BITS`.
pub(crate) const SCHEMA: &str = "
CREATE VIRTUAL TABLE book_fts USING fts5(title, authors, tags, details, tokenize = 'unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE text_fts USING fts5(text, page UNINDEXED, kind UNINDEXED, tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE text_index (
    book INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    pages INTEGER NOT NULL,
    indexed INTEGER NOT NULL
);
CREATE TRIGGER book_fts_insert AFTER INSERT ON books BEGIN
    INSERT INTO book_fts (rowid, title, authors, tags, details)
    VALUES (new.id, new.title, new.author, '', concat_ws(' ', new.year, new.doi, new.arxiv, new.publisher, new.isbn, new.abstract));
END;
CREATE TRIGGER book_fts_update AFTER UPDATE OF title, author, year, doi, arxiv, publisher, isbn, abstract ON books BEGIN
    UPDATE book_fts SET title = new.title, authors = new.author,
        details = concat_ws(' ', new.year, new.doi, new.arxiv, new.publisher, new.isbn, new.abstract)
    WHERE rowid = new.id;
END;
CREATE TRIGGER book_fts_delete AFTER DELETE ON books BEGIN
    DELETE FROM book_fts WHERE rowid = old.id;
    DELETE FROM text_fts WHERE rowid BETWEEN old.id * 1048576 AND old.id * 1048576 + 1048575;
END;
CREATE TRIGGER book_fts_tag_added AFTER INSERT ON book_tags BEGIN
    UPDATE book_fts SET tags = (SELECT group_concat(t.name, ' ') FROM book_tags bt JOIN tags t ON t.id = bt.tag WHERE bt.book = new.book)
    WHERE rowid = new.book;
END;
CREATE TRIGGER book_fts_tag_removed AFTER DELETE ON book_tags BEGIN
    UPDATE book_fts SET tags = COALESCE((SELECT group_concat(t.name, ' ') FROM book_tags bt JOIN tags t ON t.id = bt.tag WHERE bt.book = old.book), '')
    WHERE rowid = old.book;
END;
INSERT INTO book_fts (rowid, title, authors, tags, details)
    SELECT b.id, b.title, b.author,
        COALESCE((SELECT group_concat(t.name, ' ') FROM book_tags bt JOIN tags t ON t.id = bt.tag WHERE bt.book = b.id), ''),
        concat_ws(' ', b.year, b.doi, b.arxiv, b.publisher, b.isbn, b.abstract)
    FROM books b;
";

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a text hit is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitKind {
    /// The page's text.
    Page,
    /// A highlight's marked text and its comment.
    Highlight,
}

/// One place in a book's text that matches a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextHit {
    /// The book's row id.
    pub book: i64,
    /// 0-based page.
    pub page: u32,
    pub kind: HitKind,
    /// The words around the match, the matched ones between [`MATCH_START`] and [`MATCH_END`].
    pub snippet: String,
}

/// A search's words as an FTS5 query. Each word is quoted, so nothing typed is FTS syntax
/// (`AND`, `NEAR(`, `title:`, `*`, `^` are words like any other), and every word is required;
/// the last one also matches as a prefix (results as you type). Words without a letter or digit
/// are dropped: the tokenizer would make nothing of them. `None` without words.
pub fn fts_query(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| word.chars().filter(|c| *c != '"' && !c.is_control()).collect::<String>())
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .take(MAX_QUERY_WORDS)
        .collect();
    let (last, rest) = words.split_last()?;
    let mut query: Vec<String> = rest.iter().map(|word| format!("\"{word}\"")).collect();
    query.push(format!("\"{last}\"*"));
    Some(query.join(" "))
}

/// The ids of the books whose title, authors, tags or details match `text`, best first
/// (removed and missing books too: the caller shows the ones it lists).
pub(crate) fn search_books(conn: &Connection, text: &str) -> Result<Vec<i64>, String> {
    let Some(query) = fts_query(text) else { return Ok(Vec::new()) };
    let mut stmt = conn.prepare_cached("SELECT rowid FROM book_fts WHERE book_fts MATCH ?1 ORDER BY rank LIMIT ?2").map_err(db_err)?;
    let limit = i64::try_from(MAX_BOOK_HITS).unwrap_or(i64::MAX);
    let rows = stmt.query_map(params![query, limit], |row| row.get(0)).map_err(db_err)?;
    rows.map(|row| row.map_err(db_err)).collect()
}

/// The places in the books' text and highlights that match `text`, best first.
pub(crate) fn search_text(conn: &Connection, text: &str) -> Result<Vec<TextHit>, String> {
    let Some(query) = fts_query(text) else { return Ok(Vec::new()) };
    let mut stmt = conn
        .prepare_cached(
            "SELECT rowid, page, kind, snippet(text_fts, 0, char(2), char(3), '…', ?3) FROM text_fts WHERE text_fts MATCH ?1
             ORDER BY rank LIMIT ?2",
        )
        .map_err(db_err)?;
    let limit = i64::try_from(MAX_HITS).unwrap_or(i64::MAX);
    let rows = stmt
        .query_map(params![query, limit, SNIPPET_WORDS], |row| {
            let id: i64 = row.get(0)?;
            let kind = if row.get::<_, i64>(2)? == 1 { HitKind::Highlight } else { HitKind::Page };
            Ok(TextHit { book: id >> ROW_BITS, page: u32::try_from(row.get::<_, i64>(1)?).unwrap_or(0), kind, snippet: row.get(3)? })
        })
        .map_err(db_err)?;
    rows.map(|row| row.map_err(db_err)).collect()
}

impl LibraryDb {
    /// The ids of the books whose title, authors, tags or details (year, DOI, arXiv id, venue,
    /// ISBN, abstract) match the words of `text` (the last one as a prefix).
    pub fn find_books(&self, text: &str) -> Result<Vec<i64>, String> {
        search_books(self.conn(), text)
    }
}

/// A book whose file's text is not indexed as it is now.
#[derive(Clone, Debug)]
struct Stale {
    book: i64,
    path: String,
    fingerprint: String,
    size: i64,
    mtime: i64,
}

/// Books listed and there whose text was never read, or was read from other bytes.
fn stale_books(conn: &Connection) -> Result<Vec<Stale>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT b.id, f.path, f.fingerprint, f.size, f.mtime FROM books b
             JOIN book_files f ON f.path = b.path AND f.missing = 0
             LEFT JOIN text_index t ON t.book = b.id
             WHERE b.removed = 0 AND b.missing = 0
               AND (t.book IS NULL OR t.fingerprint != f.fingerprint OR t.size != f.size OR t.mtime != f.mtime)
             ORDER BY b.last_opened DESC, b.id",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| Ok(Stale { book: row.get(0)?, path: row.get(1)?, fingerprint: row.get(2)?, size: row.get(3)?, mtime: row.get(4)? }))
        .map_err(db_err)?;
    rows.map(|row| row.map_err(db_err)).collect()
}

/// One book's text, read on a worker.
struct BookText {
    file: Stale,
    /// (page, text, kind): pages and highlights.
    rows: Vec<(u32, String, HitKind)>,
    pages: u32,
}

/// Text as it is stored: control characters (the snippet markers among them) become spaces.
fn plain(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Reads one book's page text and highlights; `None` only when cancelled. A file that can't be
/// read, or is too large, is recorded with no text, so it isn't tried again until it changes.
fn read_text(file: Stale, cancel: &AtomicBool) -> Option<BookText> {
    let size = u64::try_from(file.size).unwrap_or(u64::MAX);
    let bytes = if size <= MAX_FILE_BYTES { std::fs::read(&file.path).ok() } else { None };
    let Some(bytes) = bytes else { return Some(BookText { file, rows: Vec::new(), pages: 0 }) };
    let bytes = Arc::new(bytes);
    let info = pdfcraft_render::inspect(bytes.clone(), None).ok();
    let pages_total = info.as_ref().map_or(0, |info| info.pages.len());
    let mut marks_by_page: HashMap<usize, Vec<&Annotation>> = HashMap::new();
    for mark in info.iter().flat_map(|info| &info.annotations) {
        if mark.in_reply_to.is_none() && MARKUP.contains(&mark.subtype.as_str()) {
            marks_by_page.entry(mark.page).or_default().push(mark);
        }
    }
    let mut renderer = PageRenderer::new(bytes, RenderConfig::default());
    let mut rows = Vec::new();
    let mut kept = 0usize;
    let mut highlights = 0usize;
    for page in 0..pages_total.min(MAX_PAGES) {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let rendered = renderer.render(RenderRequest { page, kind: RequestKind::Text, tile: None, scale: 1.0, tag: 0 });
        let page_number = u32::try_from(page).unwrap_or(u32::MAX);
        if let Some(text) = &rendered.text
            && kept < MAX_BOOK_TEXT
        {
            let words = plain(&text.plain_text());
            kept = kept.saturating_add(words.len());
            if !words.is_empty() {
                rows.push((page_number, words, HitKind::Page));
            }
        }
        for mark in marks_by_page.get(&page).into_iter().flatten() {
            if highlights >= MAX_HIGHLIGHTS {
                break;
            }
            let quote = match (&rendered.text, info.as_ref().and_then(|info| info.pages.get(page))) {
                (Some(text), Some(geometry)) => marked_text(text, geometry, mark),
                _ => String::new(),
            };
            let note = plain(&format!("{quote} {}", mark.contents.as_deref().unwrap_or_default()));
            if !note.is_empty() {
                highlights = highlights.saturating_add(1);
                rows.push((page_number, note, HitKind::Highlight));
            }
        }
    }
    Some(BookText { file, rows, pages: u32::try_from(pages_total).unwrap_or(u32::MAX) })
}

/// A quad's bounding box in the page's view space: [x0, y0, x1, y1].
fn view_box(page: &PageInfo, quad: &[f32; 8]) -> [f32; 4] {
    let corners = [0usize, 2, 4, 6].map(|i| {
        let x = quad.get(i).copied().unwrap_or(0.0);
        let y = quad.get(i.saturating_add(1)).copied().unwrap_or(0.0);
        page.user_to_view(x, y)
    });
    let (xs, ys) = (corners.map(|point| point[0]), corners.map(|point| point[1]));
    [
        xs.into_iter().fold(f32::INFINITY, f32::min),
        ys.into_iter().fold(f32::INFINITY, f32::min),
        xs.into_iter().fold(f32::NEG_INFINITY, f32::max),
        ys.into_iter().fold(f32::NEG_INFINITY, f32::max),
    ]
}

/// The text of `page` under a text-markup comment: the glyphs whose centres fall inside one of
/// its quads (or its box), in reading order, words separated by single spaces.
fn marked_text(text: &PageText, page: &PageInfo, mark: &Annotation) -> String {
    let boxes: Vec<[f32; 4]> = if mark.quads.is_empty() {
        let [x0, y0, x1, y1] = mark.rect;
        vec![view_box(page, &[x0, y1, x1, y1, x0, y0, x1, y0])]
    } else {
        mark.quads.iter().map(|quad| view_box(page, quad)).collect()
    };
    let mut out = String::new();
    let mut previous: Option<usize> = None;
    for (i, glyph) in text.glyphs.iter().enumerate() {
        let (cx, cy) = ((glyph.rect[0] + glyph.rect[2]) / 2.0, (glyph.rect[1] + glyph.rect[3]) / 2.0);
        if !boxes.iter().any(|b| cx >= b[0] && cx <= b[2] && cy >= b[1] && cy <= b[3]) {
            continue;
        }
        if let Some(p) = previous {
            let same_line = text.line_of.get(p) == text.line_of.get(i);
            let joined = same_line && p.checked_add(1) == Some(i) && !text.space_before.get(i).copied().unwrap_or(true);
            if !joined {
                out.push(' ');
            }
        }
        out.push_str(&glyph.text);
        previous = Some(i);
    }
    out
}

/// Replaces a book's rows with what was read, and records which bytes they came from.
fn commit(tx: &Transaction<'_>, book: &BookText, now: i64) -> Result<(), String> {
    let first = book.file.book.checked_shl(ROW_BITS).ok_or("library database: a book id out of range")?;
    let last = first.saturating_add((1 << ROW_BITS) - 1);
    tx.execute("DELETE FROM text_fts WHERE rowid BETWEEN ?1 AND ?2", params![first, last]).map_err(db_err)?;
    {
        let mut insert = tx.prepare_cached("INSERT INTO text_fts (rowid, text, page, kind) VALUES (?1, ?2, ?3, ?4)").map_err(db_err)?;
        for (n, (page, text, kind)) in book.rows.iter().enumerate().take(1 << ROW_BITS) {
            let id = first.saturating_add(i64::try_from(n).unwrap_or(0));
            insert.execute(params![id, text, page, i64::from(*kind == HitKind::Highlight)]).map_err(db_err)?;
        }
    }
    tx.execute(
        "INSERT INTO text_index (book, fingerprint, size, mtime, pages, indexed) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(book) DO UPDATE SET fingerprint = excluded.fingerprint, size = excluded.size, mtime = excluded.mtime,
            pages = excluded.pages, indexed = excluded.indexed",
        params![book.file.book, book.file.fingerprint, book.file.size, book.file.mtime, book.pages, now],
    )
    .map_err(db_err)?;
    Ok(())
}

/// How far a text pass is: books read of those to read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextProgress {
    pub done: usize,
    pub total: usize,
    pub finished: bool,
}

/// The library's text read in the background: cancel it, read its progress, take its result.
pub struct TextJob {
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
    result: Arc<Mutex<Option<Result<usize, String>>>>,
}

impl TextJob {
    /// Reads the text of every stale book in `<data>/library.sqlite` on a new thread; `wake`
    /// runs after each book (the UI repaints its progress).
    pub fn spawn(data: PathBuf, wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let job = Self::idle();
        let (cancel, done, total, result) = (job.cancel.clone(), job.done.clone(), job.total.clone(), job.result.clone());
        std::thread::Builder::new().name("margin-library-text".into()).spawn(move || {
            let outcome = run(&data, &cancel, &done, &total, &wake);
            *lock(&result) = Some(outcome);
            wake();
        })?;
        Ok(job)
    }

    /// Runs on this thread (tests).
    pub fn run_inline(data: &Path) -> Self {
        let job = Self::idle();
        let outcome = run(data, &job.cancel, &job.done, &job.total, &|| {});
        *lock(&job.result) = Some(outcome);
        job
    }

    fn idle() -> Self {
        Self { cancel: Arc::default(), done: Arc::default(), total: Arc::default(), result: Arc::default() }
    }

    /// Asks the pass to stop; the books it finished stay indexed.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn progress(&self) -> TextProgress {
        TextProgress { done: self.done.load(Ordering::Relaxed), total: self.total.load(Ordering::Relaxed), finished: lock(&self.result).is_some() }
    }

    /// How many books were read, once the pass ended (taken once). A worker that died without
    /// a result reports an error.
    pub fn take_result(&self) -> Option<Result<usize, String>> {
        let taken = lock(&self.result).take();
        if taken.is_none() && Arc::strong_count(&self.result) == 1 {
            return Some(Err("the library's text index stopped unexpectedly".to_owned()));
        }
        taken
    }
}

/// The pass: stale books read on [`THREADS`] threads, each committed as it comes.
fn run(data: &Path, cancel: &AtomicBool, done: &AtomicUsize, total: &AtomicUsize, wake: &dyn Fn()) -> Result<usize, String> {
    let conn = open_connection(data)?;
    let stale = stale_books(&conn)?;
    total.store(stale.len(), Ordering::Relaxed);
    if stale.is_empty() {
        return Ok(0);
    }
    let next = AtomicUsize::new(0);
    let (send, receive) = mpsc::channel::<BookText>();
    let stale = &stale;
    let mut result = Ok(0usize);
    std::thread::scope(|scope| {
        for _ in 0..THREADS.min(stale.len()) {
            let send = send.clone();
            let next = &next;
            scope.spawn(move || {
                while !cancel.load(Ordering::Relaxed) {
                    let Some(file) = stale.get(next.fetch_add(1, Ordering::Relaxed)) else { break };
                    let Some(text) = read_text(file.clone(), cancel) else { break };
                    if send.send(text).is_err() {
                        break;
                    }
                }
            });
        }
        drop(send);
        for book in receive {
            if result.is_err() {
                continue;
            }
            let committed = conn.unchecked_transaction().map_err(db_err).and_then(|tx| {
                commit(&tx, &book, now_unix())?;
                tx.commit().map_err(db_err)
            });
            match committed {
                Ok(()) => {
                    if let Ok(count) = &mut result {
                        *count = count.saturating_add(1);
                    }
                    done.fetch_add(1, Ordering::Relaxed);
                    wake();
                }
                Err(err) => {
                    cancel.store(true, Ordering::Relaxed);
                    result = Err(err);
                }
            }
        }
    });
    result
}

/// One search of the library's text on its own thread. Dropping it interrupts the query.
pub struct TextSearch {
    result: Arc<Mutex<Option<Result<Vec<TextHit>, String>>>>,
    interrupt: Arc<Mutex<Option<InterruptHandle>>>,
    dropped: Arc<AtomicBool>,
}

impl TextSearch {
    /// Searches the text of `<data>/library.sqlite` for `query`; `wake` runs when it is done.
    pub fn spawn(data: PathBuf, query: String, wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let search = Self::idle();
        let (result, interrupt, dropped) = (search.result.clone(), search.interrupt.clone(), search.dropped.clone());
        std::thread::Builder::new().name("margin-library-search".into()).spawn(move || {
            let outcome = open_connection(&data).and_then(|conn| {
                *lock(&interrupt) = Some(conn.get_interrupt_handle());
                if dropped.load(Ordering::Relaxed) {
                    return Ok(Vec::new());
                }
                search_text(&conn, &query)
            });
            *lock(&result) = Some(outcome);
            wake();
        })?;
        Ok(search)
    }

    /// Searches on this thread (tests).
    pub fn run_inline(data: &Path, query: &str) -> Self {
        let search = Self::idle();
        *lock(&search.result) = Some(open_connection(data).and_then(|conn| search_text(&conn, query)));
        search
    }

    fn idle() -> Self {
        Self { result: Arc::default(), interrupt: Arc::default(), dropped: Arc::default() }
    }

    /// The hits, once the search ended (taken once).
    pub fn take(&self) -> Option<Result<Vec<TextHit>, String>> {
        let taken = lock(&self.result).take();
        if taken.is_none() && Arc::strong_count(&self.result) == 1 {
            return Some(Err("the text search stopped unexpectedly".to_owned()));
        }
        taken
    }
}

impl Drop for TextSearch {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Relaxed);
        if let Some(handle) = lock(&self.interrupt).as_ref() {
            handle.interrupt();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::tests::temp_dir;
    use crate::library::{ScanJob, ScanRequest};

    /// A PDF with `pages` pages of text; page 2 (index 1) says `words`, and carries a highlight
    /// over them with a comment.
    fn text_pdf(words: &str, pages: usize) -> Vec<u8> {
        let plain_page = "BT /F1 12 Tf 24 140 Td (Nothing to see on this page) Tj ET\n";
        let marked_page = format!("BT /F1 12 Tf 24 140 Td ({words}) Tj ET\n");
        let first_page = 7;
        let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", first_page + i)).collect();
        let mut objs = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            format!("<< /Type /Pages /Kids [{}] /Count {pages} /MediaBox [0 0 200 300] >>", kids.join(" ")),
            format!("<< /Length {} >>\nstream\n{plain_page}endstream", plain_page.len()),
            format!("<< /Length {} >>\nstream\n{marked_page}endstream", marked_page.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
            "<< /Type /Annot /Subtype /Highlight /Rect [20 130 200 160] /QuadPoints [20 160 200 160 20 130 200 130] /Contents (worth rereading) >>"
                .to_owned(),
        ];
        for i in 0..pages {
            let (contents, annots) = if i == 1 { (4, " /Annots [6 0 R]") } else { (3, "") };
            objs.push(format!("<< /Type /Page /Parent 2 0 R /Contents {contents} 0 R /Resources << /Font << /F1 5 0 R >> >>{annots} >>"));
        }
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
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
        out
    }

    #[test]
    fn typed_words_are_never_fts_syntax() {
        assert_eq!(fts_query("  "), None);
        assert_eq!(fts_query("- * ^ \" ()"), None, "no word the tokenizer can use");
        assert_eq!(fts_query("deep learning"), Some("\"deep\" \"learning\"*".into()));
        assert_eq!(
            fts_query("say \"hi\" NEAR(a b) title:x AND OR NOT"),
            Some("\"say\" \"hi\" \"NEAR(a\" \"b)\" \"title:x\" \"AND\" \"OR\" \"NOT\"*".into())
        );
        assert_eq!(fts_query(&"word ".repeat(1000)).map(|query| query.matches("word").count()), Some(MAX_QUERY_WORDS));
    }

    #[test]
    fn page_text_and_highlights_are_found_and_read_again_only_when_the_file_changes() {
        let root = temp_dir("text-index");
        let books = root.join("books");
        let data = root.join("data");
        std::fs::create_dir_all(&books).unwrap();
        let file = books.join("quiet.pdf");
        std::fs::write(&file, text_pdf("Margin reads quietly", 3)).unwrap();
        std::fs::write(books.join("broken.pdf"), "%PDF-1.7 not really").unwrap();
        ScanJob::run_inline(&ScanRequest { data: data.clone(), folders: vec![books.clone()], files: Vec::new(), threads: 1 })
            .take_report()
            .unwrap()
            .unwrap();
        let job = TextJob::run_inline(&data);
        assert_eq!(job.take_result(), Some(Ok(2)), "both books are read; the broken one with no text");
        let db = LibraryDb::open(&data).unwrap();
        let book = db.books().unwrap().into_iter().find(|book| book.path.ends_with("quiet.pdf")).unwrap().id;

        let hits = search_text(db.conn(), "quiet").unwrap();
        let page_hit = hits.iter().find(|hit| hit.kind == HitKind::Page).expect("the page's text matches by prefix");
        assert_eq!((page_hit.book, page_hit.page), (book, 1));
        assert!(page_hit.snippet.contains(&format!("{MATCH_START}quietly{MATCH_END}")), "{}", page_hit.snippet);
        let highlight = search_text(db.conn(), "rereading").unwrap();
        assert_eq!(highlight.len(), 1);
        assert_eq!((highlight[0].kind, highlight[0].page), (HitKind::Highlight, 1));
        assert!(highlight[0].snippet.contains("Margin reads quietly"), "the highlight's quote is indexed: {}", highlight[0].snippet);
        let searched = TextSearch::run_inline(&data, "margin reads").take().unwrap().unwrap();
        assert_eq!(searched.len(), 2, "the page and the highlight");

        assert_eq!(TextJob::run_inline(&data).take_result(), Some(Ok(0)), "nothing changed, nothing is read");
        std::fs::write(&file, text_pdf("Margin reads loudly", 3)).unwrap();
        ScanJob::run_inline(&ScanRequest { data: data.clone(), folders: vec![books], files: Vec::new(), threads: 1 }).take_report().unwrap().unwrap();
        assert_eq!(TextJob::run_inline(&data).take_result(), Some(Ok(1)), "only the changed book is read again");
        assert!(search_text(db.conn(), "quietly").unwrap().is_empty(), "the old text is gone");
        assert_eq!(search_text(db.conn(), "loudly").unwrap().first().map(|hit| hit.book), Some(book));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn hostile_searches_are_plain_words() {
        let root = temp_dir("text-hostile");
        let db = LibraryDb::open(&root.join("data")).unwrap();
        for typed in
            ["\"", "a\"b", "NEAR(", "title:x", "*", "x*y", "^start", "a OR", "AND", "(", "{x}", "\u{0}\u{2}\u{3}", "-x", "😀", &"z".repeat(10_000)]
        {
            assert!(search_text(db.conn(), typed).is_ok(), "{typed:?}");
            assert!(search_books(db.conn(), typed).is_ok(), "{typed:?}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_cancelled_pass_commits_nothing_more() {
        let root = temp_dir("text-cancel");
        let books = root.join("books");
        let data = root.join("data");
        std::fs::create_dir_all(&books).unwrap();
        std::fs::write(books.join("a.pdf"), text_pdf("alpha", 2)).unwrap();
        ScanJob::run_inline(&ScanRequest { data: data.clone(), folders: vec![books], files: Vec::new(), threads: 1 }).take_report().unwrap().unwrap();
        let cancel = AtomicBool::new(true);
        let (done, total) = (AtomicUsize::new(0), AtomicUsize::new(0));
        assert_eq!(run(&data, &cancel, &done, &total, &|| {}), Ok(0));
        assert_eq!((done.load(Ordering::Relaxed), total.load(Ordering::Relaxed)), (0, 1));
        std::fs::remove_dir_all(&root).ok();
    }
}
