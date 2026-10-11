//! Schema upgrades for Margin's SQLite files (`library.sqlite`, `stats.sqlite`), keyed by
//! `PRAGMA user_version`. A file at version `n` runs steps `n`, `n + 1`, … in order, each in its
//! own write transaction that also stamps the new version, so a failed step leaves the file at
//! the last version that worked. A file newer than every step is left alone.
//!
//! Before upgrading a file that already holds data, a copy of it is written beside it
//! (`<file>.v<n>.bak`, once per version), so an upgrade can never cost a user their library.
//! Foreign keys are off while steps run (a table rebuild must not cascade into its children) and
//! are checked before each step commits.

use std::path::Path;

use rusqlite::{Connection, Transaction, TransactionBehavior};

/// One upgrade: from the version at its index to the next.
pub(crate) type Step = fn(&Transaction<'_>) -> Result<(), String>;

/// A database file's upgrade path.
pub(crate) struct Schema<'a> {
    /// How errors name the file ("library database").
    pub name: &'a str,
    /// Step `i` upgrades version `i` to `i + 1`; the last step's result is the current version.
    pub steps: &'a [Step],
}

impl Schema<'_> {
    /// The version the last step leaves a file at.
    pub fn latest(&self) -> i64 {
        i64::try_from(self.steps.len()).unwrap_or(i64::MAX)
    }
}

fn version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
}

/// Brings the file `conn` has open (at `path`) to the schema's latest version.
pub(crate) fn upgrade(conn: &Connection, path: &Path, schema: &Schema<'_>) -> Result<(), String> {
    let name = schema.name;
    let err = |err: rusqlite::Error| format!("{name}: {err}");
    let latest = schema.latest();
    let current = version(conn).map_err(err)?;
    if current > latest {
        return Err(format!("{name}: written by a newer Margin (schema {current}); not changed"));
    }
    if current == latest {
        return Ok(());
    }
    if current > 0 {
        back_up(conn, path, current).map_err(|reason| format!("{name}: can't keep a copy before upgrading it ({reason}); not changed"))?;
    }
    conn.pragma_update(None, "foreign_keys", "OFF").map_err(err)?;
    let result = run_steps(conn, schema);
    let restored = conn.pragma_update(None, "foreign_keys", "ON").map_err(err);
    result.and(restored)
}

fn run_steps(conn: &Connection, schema: &Schema<'_>) -> Result<(), String> {
    let name = schema.name;
    let err = |err: rusqlite::Error| format!("{name}: {err}");
    for (index, step) in schema.steps.iter().enumerate() {
        let from = i64::try_from(index).unwrap_or(i64::MAX);
        // Immediate: another connection upgrading the same file waits here, then sees its work.
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(err)?;
        if version(&tx).map_err(err)? != from {
            continue;
        }
        step(&tx).map_err(|reason| format!("{name}: upgrading from schema {from} failed ({reason}); not changed"))?;
        let broken: Option<String> = tx
            .query_row("SELECT \"table\" FROM pragma_foreign_key_check LIMIT 1", [], |row| row.get(0))
            .map(Some)
            .or_else(|err| if err == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(err) })
            .map_err(err)?;
        if let Some(table) = broken {
            return Err(format!("{name}: upgrading from schema {from} left broken references in {table}; not changed"));
        }
        tx.pragma_update(None, "user_version", from.saturating_add(1)).map_err(err)?;
        tx.commit().map_err(err)?;
    }
    Ok(())
}

/// `<file>.v<version>.bak`: a consistent copy of the file as it is, made once per version.
fn back_up(conn: &Connection, path: &Path, version: i64) -> Result<(), String> {
    let mut name = path.file_name().map(|name| name.to_os_string()).ok_or("the database has no file name")?;
    name.push(format!(".v{version}.bak"));
    let copy = path.with_file_name(name);
    if copy.exists() {
        return Ok(());
    }
    let target = copy.to_str().ok_or("the data folder's path is not valid Unicode")?;
    match conn.execute("VACUUM INTO ?1", [target]) {
        Ok(_) => Ok(()),
        // Another connection made it first.
        Err(_) if copy.exists() => Ok(()),
        Err(err) => Err(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::tests::temp_dir;

    fn create(tx: &Transaction<'_>) -> Result<(), String> {
        tx.execute_batch("CREATE TABLE notes (id INTEGER PRIMARY KEY, text TEXT NOT NULL);").map_err(|err| err.to_string())
    }

    fn add_column(tx: &Transaction<'_>) -> Result<(), String> {
        tx.execute_batch("ALTER TABLE notes ADD COLUMN tag TEXT NOT NULL DEFAULT 'none';").map_err(|err| err.to_string())
    }

    fn fail_halfway(tx: &Transaction<'_>) -> Result<(), String> {
        tx.execute_batch("UPDATE notes SET text = 'lost';").map_err(|err| err.to_string())?;
        Err("step failed".to_owned())
    }

    fn open(dir: &Path) -> (Connection, std::path::PathBuf) {
        let path = dir.join("test.sqlite");
        (Connection::open(&path).unwrap(), path)
    }

    #[test]
    fn steps_run_in_order_once_and_back_up_existing_data() {
        let dir = temp_dir("migrate-order");
        let (conn, path) = open(&dir);
        upgrade(&conn, &path, &Schema { name: "test", steps: &[create] }).unwrap();
        assert!(!dir.join("test.sqlite.v0.bak").exists(), "an empty file needs no copy");
        conn.execute("INSERT INTO notes (text) VALUES ('kept')", []).unwrap();
        let schema = Schema { name: "test", steps: &[create, add_column] };
        upgrade(&conn, &path, &schema).unwrap();
        upgrade(&conn, &path, &schema).unwrap();
        assert_eq!(version(&conn).unwrap(), 2);
        let row: (String, String) = conn.query_row("SELECT text, tag FROM notes", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(row, ("kept".to_owned(), "none".to_owned()));
        let copy = Connection::open(dir.join("test.sqlite.v1.bak")).unwrap();
        assert_eq!(version(&copy).unwrap(), 1);
        let text: String = copy.query_row("SELECT text FROM notes", [], |row| row.get(0)).unwrap();
        assert_eq!(text, "kept");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failed_step_rolls_back_to_the_last_good_version() {
        let dir = temp_dir("migrate-fail");
        let (conn, path) = open(&dir);
        upgrade(&conn, &path, &Schema { name: "test", steps: &[create] }).unwrap();
        conn.execute("INSERT INTO notes (text) VALUES ('kept')", []).unwrap();
        let err = upgrade(&conn, &path, &Schema { name: "test", steps: &[create, add_column, fail_halfway] }).unwrap_err();
        assert!(err.contains("from schema 2"), "{err}");
        assert_eq!(version(&conn).unwrap(), 2, "the steps before the failure stay");
        let text: String = conn.query_row("SELECT text FROM notes", [], |row| row.get(0)).unwrap();
        assert_eq!(text, "kept");
        let foreign_keys: i64 = conn.pragma_query_value(None, "foreign_keys", |row| row.get(0)).unwrap();
        assert_eq!(foreign_keys, 1, "foreign keys are back on");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_newer_file_is_refused() {
        let dir = temp_dir("migrate-newer");
        let (conn, path) = open(&dir);
        conn.pragma_update(None, "user_version", 9).unwrap();
        let err = upgrade(&conn, &path, &Schema { name: "test", steps: &[create] }).unwrap_err();
        assert!(err.contains("newer"), "{err}");
        assert_eq!(version(&conn).unwrap(), 9);
        std::fs::remove_dir_all(&dir).ok();
    }
}
