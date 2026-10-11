//! How `library.sqlite` grew: one [`migrate::Step`] per schema version, oldest first. A step
//! never drops data: rows it reshapes are copied into the new tables before the old ones go, and
//! [`migrate::upgrade`] keeps a copy of the whole file from before the first step it runs.

use rusqlite::Transaction;

use crate::migrate;

/// Every step, in order; the library is at version `STEPS.len()`.
pub(crate) const STEPS: &[migrate::Step] = &[v1_books_by_path];

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
