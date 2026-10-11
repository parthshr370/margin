//! Zotero's own library (`zotero.sqlite`), read for the library import. Zotero keeps the file
//! open and locked while it runs, so the user's database is never opened at all: it is copied
//! (with its write-ahead log, which holds Zotero's latest changes), the copy is read through a
//! query-only connection, and the copy is deleted afterwards.

use crate::reference::{MAX_FIELD_CHARS, MAX_KEYWORD_CHARS, Reference, clean};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
/// Most items read.
const MAX_ITEMS: usize = 200_000;
/// Most authors kept per item.
const MAX_AUTHORS: usize = 200;

fn field_map(connection: &Connection, item_id: i64) -> rusqlite::Result<HashMap<String, String>> {
    let mut statement = connection.prepare_cached(
        "SELECT fields.fieldName, CAST(itemDataValues.value AS TEXT) FROM itemData JOIN fields ON fields.fieldID = itemData.fieldID
         JOIN itemDataValues ON itemDataValues.valueID = itemData.valueID WHERE itemData.itemID = ?1 AND itemDataValues.value IS NOT NULL",
    )?;
    let rows = statement.query_map([item_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    let mut result = HashMap::new();
    for row in rows {
        let (name, value) = row?;
        result.insert(name, value);
    }
    Ok(result)
}

fn optional(fields: &HashMap<String, String>, key: &str) -> Option<String> {
    fields.get(key).map(|value| clean(value, MAX_FIELD_CHARS)).filter(|value| !value.is_empty())
}

fn arxiv_from_extra(extra: &str) -> Option<String> {
    let start = extra.to_ascii_lowercase().find("arxiv:")?.saturating_add("arxiv:".len());
    extra.get(start..)?.split_whitespace().next().map(|id| id.trim_matches([',', ';', '.']).to_owned()).filter(|id| !id.is_empty())
}

fn build_reference(connection: &Connection, item_id: i64, database_dir: &Path) -> rusqlite::Result<Reference> {
    let fields = field_map(connection, item_id)?;
    let mut reference = Reference {
        title: optional(&fields, "title"),
        year: fields.get("date").and_then(|date| date.get(..4)?.parse::<i32>().ok()).filter(|year| (1000..=2999).contains(year)),
        doi: optional(&fields, "DOI").or_else(|| optional(&fields, "doi")).and_then(|doi| crate::ids::normalize_doi(&doi)),
        publisher: ["publicationTitle", "proceedingsTitle", "bookTitle", "publisher"].iter().find_map(|field| optional(&fields, field)),
        isbn: optional(&fields, "ISBN").or_else(|| optional(&fields, "isbn")),
        abstract_text: optional(&fields, "abstractNote"),
        arxiv: optional(&fields, "extra").and_then(|extra| arxiv_from_extra(&extra)).and_then(|id| crate::ids::normalize_arxiv(&id)),
        ..Reference::default()
    };
    let mut creators = connection.prepare_cached(
        "SELECT COALESCE(creators.firstName, ''), COALESCE(creators.lastName, ''), COALESCE(creators.fieldMode, 0) FROM itemCreators
         JOIN creators ON creators.creatorID = itemCreators.creatorID WHERE itemCreators.itemID = ?1 ORDER BY itemCreators.orderIndex",
    )?;
    let rows = creators.query_map([item_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))?;
    for row in rows {
        let (first, last, field_mode) = row?;
        let display = if field_mode == 1 { last } else { format!("{} {}", first.trim(), last.trim()) };
        let display = clean(&display, MAX_FIELD_CHARS);
        if !display.is_empty() && reference.authors.len() < MAX_AUTHORS {
            reference.authors.push(display);
        }
    }
    let mut tags = connection.prepare_cached(
        "SELECT tags.name FROM itemTags JOIN tags ON tags.tagID = itemTags.tagID WHERE itemTags.itemID = ?1 AND tags.name IS NOT NULL",
    )?;
    let rows = tags.query_map([item_id], |row| row.get::<_, String>(0))?;
    for row in rows {
        let tag = clean(&row?, MAX_KEYWORD_CHARS);
        if !tag.is_empty() && !reference.keywords.iter().any(|existing| existing.eq_ignore_ascii_case(&tag)) {
            reference.keywords.push(tag);
        }
    }
    let mut attachments = connection.prepare_cached(
        "SELECT items.key, itemAttachments.path FROM itemAttachments JOIN items ON items.itemID = itemAttachments.itemID
         WHERE itemAttachments.parentItemID = ?1 AND itemAttachments.contentType = 'application/pdf' AND itemAttachments.path IS NOT NULL",
    )?;
    let rows = attachments.query_map([item_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    for row in rows {
        let (attachment_key, path) = row?;
        // A stored file is `storage:<name>` in `storage/<key>/`; a name that walks elsewhere is not Zotero's.
        let plain = |part: &str| Path::new(part).file_name().is_some_and(|name| name == part);
        if let Some(name) = path.strip_prefix("storage:") {
            if plain(name) && plain(&attachment_key) {
                reference.files.push(database_dir.join("storage").join(attachment_key).join(name));
            }
        } else if Path::new(&path).is_absolute() {
            reference.files.push(PathBuf::from(path));
        }
    }
    Ok(reference)
}

/// The files SQLite keeps beside a database: its write-ahead log and shared-memory index.
const SIDE_FILES: [&str; 2] = ["-wal", "-shm"];

/// `path` with `suffix` appended to its file name (`zotero.sqlite-wal`).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The regular items (not notes, attachments or deleted items) of the Zotero database `db`.
pub fn read_zotero(db: &Path) -> Result<Vec<Reference>, String> {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp_path = std::env::temp_dir().join(format!("margin-zotero-{}-{sequence}.sqlite", std::process::id()));
    let copied = fs::copy(db, &temp_path).map_err(|error| format!("couldn't copy the Zotero database: {error}")).and_then(|_| {
        let wal = with_suffix(db, "-wal");
        if wal.is_file() {
            fs::copy(&wal, with_suffix(&temp_path, "-wal")).map_err(|error| format!("couldn't copy the Zotero database's log: {error}"))?;
        }
        Ok(())
    });
    let result = copied.and_then(|()| read_copy(&temp_path, db.parent().unwrap_or_else(|| Path::new("."))));
    let leftovers = std::iter::once(temp_path.clone()).chain(SIDE_FILES.map(|suffix| with_suffix(&temp_path, suffix)));
    for file in leftovers.filter(|file| file.exists()) {
        if let Err(error) = fs::remove_file(&file) {
            // The import goes on; the copy is the user's data, so say where it was left.
            log::warn!("library import: couldn't remove the Zotero copy {}: {error}", file.display());
        }
    }
    result
}

/// Reads the items of the copy at `copy` of a Zotero database kept in `database_dir`. The
/// connection may write (a copied log is folded into the copy) but runs no query that does.
fn read_copy(copy: &Path, database_dir: &Path) -> Result<Vec<Reference>, String> {
    let not_zotero = |error: rusqlite::Error| format!("not a Zotero database ({error})");
    let connection = Connection::open_with_flags(copy, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(not_zotero)?;
    connection.pragma_update(None, "query_only", "ON").map_err(not_zotero)?;
    let mut statement = connection
        .prepare(
            "SELECT items.itemID FROM items JOIN itemTypes ON itemTypes.itemTypeID = items.itemTypeID
             WHERE itemTypes.typeName NOT IN ('attachment', 'note', 'annotation') AND items.itemID NOT IN (SELECT itemID FROM deletedItems)
             LIMIT ?1",
        )
        .map_err(not_zotero)?;
    let limit = i64::try_from(MAX_ITEMS.saturating_add(1)).unwrap_or(i64::MAX);
    let rows = statement.query_map([limit], |row| row.get::<_, i64>(0)).map_err(not_zotero)?;
    let mut references = Vec::new();
    for row in rows {
        if references.len() >= MAX_ITEMS {
            return Err(format!("the Zotero database has more than {MAX_ITEMS} items"));
        }
        let item_id = row.map_err(not_zotero)?;
        references.push(build_reference(&connection, item_id, database_dir).map_err(not_zotero)?);
    }
    Ok(references)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "zotero-test-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos())
        ))
    }

    /// The tables of `zotero.sqlite` the import reads, as Zotero names them.
    const TABLES: &str = "CREATE TABLE items(itemID INTEGER,itemTypeID INTEGER,key TEXT); CREATE TABLE itemTypes(itemTypeID INTEGER,typeName TEXT);
        CREATE TABLE fields(fieldID INTEGER,fieldName TEXT); CREATE TABLE itemData(itemID INTEGER,fieldID INTEGER,valueID INTEGER);
        CREATE TABLE itemDataValues(valueID INTEGER,value); CREATE TABLE creators(creatorID INTEGER,firstName TEXT,lastName TEXT,fieldMode INTEGER);
        CREATE TABLE itemCreators(itemID INTEGER,creatorID INTEGER,orderIndex INTEGER); CREATE TABLE tags(tagID INTEGER,name TEXT);
        CREATE TABLE itemTags(itemID INTEGER,tagID INTEGER); CREATE TABLE itemAttachments(itemID INTEGER,parentItemID INTEGER,contentType TEXT,path TEXT);
        CREATE TABLE deletedItems(itemID INTEGER);";

    /// A `zotero.sqlite` in a new folder: [`TABLES`], then `rows`.
    fn zotero_db(rows: &str) -> (PathBuf, PathBuf) {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("zotero.sqlite");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(TABLES).unwrap();
        conn.execute_batch(rows).unwrap();
        (dir, db)
    }

    #[test]
    fn imports_items_and_pdf_attachment() {
        let (dir, db) = zotero_db(
            "INSERT INTO itemTypes VALUES(1,'journalArticle'),(2,'note'),(3,'attachment'); INSERT INTO items VALUES(1,1,'ABCD'),(2,2,'NOTE'),(3,1,'DEAD'),(4,3,'PDFKEY');
             INSERT INTO deletedItems VALUES(3); INSERT INTO fields VALUES(1,'title'),(2,'date'),(3,'DOI'),(4,'abstractNote');
             INSERT INTO itemData VALUES(1,1,1),(1,2,2),(1,3,3),(1,4,4);
             INSERT INTO itemDataValues VALUES(1,'A paper'),(2,'2024-01-01'),(3,'10.1234/example'),(4,'Abstract');
             INSERT INTO creators VALUES(1,'Ada','Lovelace',0); INSERT INTO itemCreators VALUES(1,1,0); INSERT INTO tags VALUES(1,'Research');
             INSERT INTO itemTags VALUES(1,1); INSERT INTO itemAttachments VALUES(4,1,'application/pdf','storage:paper.pdf');",
        );
        let output = read_zotero(&db).unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output.first().and_then(|r| r.title.as_deref()), Some("A paper"));
        assert_eq!(output.first().and_then(|r| r.year), Some(2024));
        assert_eq!(output.first().and_then(|r| r.doi.as_deref()), Some("10.1234/example"));
        assert_eq!(output.first().map(|r| r.authors.as_slice()), Some(&["Ada Lovelace".to_owned()][..]));
        assert_eq!(output.first().map(|r| r.keywords.as_slice()), Some(&["Research".to_owned()][..]));
        assert_eq!(output.first().and_then(|r| r.abstract_text.as_deref()), Some("Abstract"));
        assert_eq!(output.first().and_then(|r| r.files.first()), Some(&dir.join("storage").join("PDFKEY").join("paper.pdf")));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn odd_values_and_attachments_that_walk_out_are_ignored() {
        let (dir, db) = zotero_db(
            "INSERT INTO itemTypes VALUES(1,'book'),(3,'attachment'); INSERT INTO items VALUES(1,1,'BOOK'),(2,3,'../..'),(3,3,'GOOD'),(4,3,'ABS'),(5,3,'REL');
             INSERT INTO fields VALUES(1,'title'),(2,'date'); INSERT INTO itemData VALUES(1,1,1),(1,2,2);
             INSERT INTO itemDataValues VALUES(1,NULL),(2,20240101); INSERT INTO creators VALUES(1,NULL,'Collective',1),(2,NULL,NULL,NULL);
             INSERT INTO itemCreators VALUES(1,1,0),(1,2,1); INSERT INTO tags VALUES(1,NULL);
             INSERT INTO itemTags VALUES(1,1); INSERT INTO itemAttachments VALUES(2,1,'application/pdf','storage:x.pdf'),
                (3,1,'application/pdf','storage:../../escape.pdf'),(4,1,'application/pdf','/abs/linked.pdf'),(5,1,'application/pdf','attachments:rel.pdf');",
        );
        let output = read_zotero(&db).unwrap();
        let item = output.first().unwrap();
        assert_eq!((item.title.as_deref(), item.year), (None, Some(2024)), "a NULL title is none, a number date still a year");
        assert_eq!(item.authors, ["Collective"], "a creator with no name is dropped");
        assert!(item.keywords.is_empty());
        assert_eq!(item.files, [PathBuf::from("/abs/linked.pdf")], "only a stored file or an absolute link");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn rejects_non_zotero_files() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let text = dir.join("text");
        fs::write(&text, "not sqlite").unwrap();
        assert!(read_zotero(&text).is_err());
        let db = dir.join("other.sqlite");
        Connection::open(&db).unwrap().execute_batch("CREATE TABLE unrelated(value TEXT);").unwrap();
        assert!(read_zotero(&db).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
