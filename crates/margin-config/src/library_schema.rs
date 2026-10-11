//! How `library.sqlite` grew: one [`migrate::Step`] per schema version, oldest first. A step
//! never drops data: rows it reshapes are copied into the new tables before the old ones go, and
//! [`migrate::upgrade`] keeps a copy of the whole file from before the first step it runs.

use std::collections::{HashMap, HashSet};

use rusqlite::{Transaction, params};

use crate::migrate;
use crate::store;

/// Every step, in order; the library is at version `STEPS.len()`.
pub(crate) const STEPS: &[migrate::Step] = &[v1_books_by_path, v2_book_identity, v3_tags];

fn db_err(err: rusqlite::Error) -> String {
    err.to_string()
}

/// Version 1: books by path, folders, collections.
const V1: &str = "
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

fn v1_books_by_path(tx: &Transaction<'_>) -> Result<(), String> {
    tx.execute_batch(V1).map_err(db_err)
}

/// Version 2's tables. A book is its stable `book_id`; its files (one per path: a copy in
/// another folder is another file of the same book) are `book_files`, each with the fingerprint
/// and size a moved file is recognised by. `books.path` is the file the library opens.
/// `sidecar_id` is the `doc.book_id` of the book's sidecar when that differs from `book_id`.
/// `checked = 0`: the fingerprint was carried over from version 1, where a file kept its first
/// fingerprint after it changed; the next scan reads the file's own.
const V2: &str = "
CREATE TABLE books_v2 (
    id INTEGER PRIMARY KEY,
    book_id TEXT NOT NULL UNIQUE,
    sidecar_id TEXT,
    path TEXT NOT NULL,
    title TEXT NOT NULL,
    author TEXT NOT NULL DEFAULT '',
    pages INTEGER NOT NULL DEFAULT 0,
    added INTEGER NOT NULL,
    last_opened INTEGER,
    progress REAL NOT NULL DEFAULT 0,
    page INTEGER NOT NULL DEFAULT 0,
    finished INTEGER NOT NULL DEFAULT 0,
    removed INTEGER NOT NULL DEFAULT 0,
    missing INTEGER NOT NULL DEFAULT 0,
    cover INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE book_files (
    path TEXT PRIMARY KEY,
    book INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL,
    size INTEGER NOT NULL DEFAULT 0,
    mtime INTEGER NOT NULL DEFAULT 0,
    missing INTEGER NOT NULL DEFAULT 0,
    checked INTEGER NOT NULL DEFAULT 1
);
";

const V2_INDEXES: &str = "
CREATE INDEX books_path ON books(path);
CREATE INDEX books_sidecar ON books(sidecar_id);
CREATE INDEX book_files_book ON book_files(book);
CREATE INDEX book_files_content ON book_files(fingerprint, size);
";

/// A version 1 row.
struct V1Row {
    id: i64,
    path: String,
    fingerprint: String,
    title: String,
    author: String,
    pages: i64,
    size: i64,
    mtime: i64,
    added: i64,
    last_opened: Option<i64>,
    progress: f64,
    page: i64,
    finished: bool,
    removed: bool,
    missing: bool,
    cover: i64,
}

/// Version 2: one row per book, keyed by a stable `book_id`, and its files apart. Version 1
/// keyed books by path, and stats and covers by the fingerprint a path was first read with, so
/// that fingerprint becomes the book's `book_id` (stats and covers stay where they are). Rows
/// that shared it were one file in several places: they become one book with several files,
/// keeping the oldest added date, the newest open, the place of the last opened copy, every
/// collection, and finished if any copy was.
fn v2_book_identity(tx: &Transaction<'_>) -> Result<(), String> {
    let rows = v1_rows(tx)?;
    tx.execute_batch(V2).map_err(db_err)?;
    let mut groups: Vec<Vec<&V1Row>> = Vec::new();
    let mut by_key: HashMap<&str, usize> = HashMap::new();
    for row in &rows {
        // A placeholder (`size-<n>`, a file that couldn't be read) names no content: never merged.
        let shared = store::is_fingerprint(&row.fingerprint).then(|| by_key.get(row.fingerprint.as_str()).copied()).flatten();
        match shared.and_then(|index| groups.get_mut(index)) {
            Some(group) => group.push(row),
            None => {
                if store::is_fingerprint(&row.fingerprint) {
                    by_key.insert(&row.fingerprint, groups.len());
                }
                groups.push(vec![row]);
            }
        }
    }
    let mut used: HashSet<String> = HashSet::new();
    for group in &groups {
        write_book(tx, group, &mut used)?;
    }
    tx.execute_batch("DROP TABLE books; ALTER TABLE books_v2 RENAME TO books;").map_err(db_err)?;
    tx.execute_batch(V2_INDEXES).map_err(db_err)
}

fn v1_rows(tx: &Transaction<'_>) -> Result<Vec<V1Row>, String> {
    let mut stmt = tx
        .prepare(
            "SELECT id, path, fingerprint, title, author, pages, size, mtime, added, last_opened, progress, page, finished, removed, missing,
                    cover FROM books ORDER BY id",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(V1Row {
                id: row.get(0)?,
                path: row.get(1)?,
                fingerprint: row.get(2)?,
                title: row.get(3)?,
                author: row.get(4)?,
                pages: row.get(5)?,
                size: row.get(6)?,
                mtime: row.get(7)?,
                added: row.get(8)?,
                last_opened: row.get(9)?,
                progress: row.get(10)?,
                page: row.get(11)?,
                finished: row.get::<_, i64>(12)? != 0,
                removed: row.get::<_, i64>(13)? != 0,
                missing: row.get::<_, i64>(14)? != 0,
                cover: row.get(15)?,
            })
        })
        .map_err(db_err)?;
    rows.collect::<Result<_, _>>().map_err(db_err)
}

fn write_book(tx: &Transaction<'_>, group: &[&V1Row], used: &mut HashSet<String>) -> Result<(), String> {
    // The row that names the book: one still listed, then the last opened, then the oldest.
    let Some(main) = group.iter().copied().min_by_key(|row| (row.removed, row.missing, std::cmp::Reverse(row.last_opened), row.id)) else {
        return Ok(());
    };
    let last = group.iter().copied().max_by_key(|row| (row.last_opened, std::cmp::Reverse(row.id))).unwrap_or(main);
    let book_id = if used.contains(&main.fingerprint) { format!("{}-{}", main.fingerprint, main.id) } else { main.fingerprint.clone() };
    used.insert(book_id.clone());
    let added = group.iter().map(|row| row.added).min().unwrap_or(main.added);
    let last_opened = group.iter().filter_map(|row| row.last_opened).max();
    let finished = group.iter().any(|row| row.finished);
    let removed = group.iter().all(|row| row.removed);
    let missing = group.iter().all(|row| row.missing);
    // The cover file is the fingerprint's: cached for one row is cached for the book.
    let cover = group.iter().map(|row| row.cover).max().unwrap_or(main.cover);
    tx.execute(
        "INSERT INTO books_v2 (id, book_id, path, title, author, pages, added, last_opened, progress, page, finished, removed, missing, cover)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            main.id,
            book_id,
            main.path,
            main.title,
            main.author,
            main.pages,
            added,
            last_opened,
            last.progress,
            last.page,
            finished,
            removed,
            missing,
            cover
        ],
    )
    .map_err(db_err)?;
    for row in group {
        tx.execute(
            "INSERT INTO book_files (path, book, fingerprint, size, mtime, missing, checked) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
            params![row.path, main.id, row.fingerprint, row.size, row.mtime, row.missing],
        )
        .map_err(db_err)?;
        if row.id != main.id {
            tx.execute(
                "INSERT OR IGNORE INTO collection_books (collection, book) SELECT collection, ?2 FROM collection_books WHERE book = ?1",
                [row.id, main.id],
            )
            .map_err(db_err)?;
            tx.execute("DELETE FROM collection_books WHERE book = ?1", [row.id]).map_err(db_err)?;
        }
    }
    Ok(())
}

/// Version 3: tags, any number per book. A name is one tag whatever its case.
const V3: &str = "
CREATE TABLE tags (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    created INTEGER NOT NULL
);
CREATE TABLE book_tags (
    book INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    tag INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (book, tag)
);
CREATE INDEX book_tags_tag ON book_tags(tag);
";

fn v3_tags(tx: &Transaction<'_>) -> Result<(), String> {
    tx.execute_batch(V3).map_err(db_err)
}
