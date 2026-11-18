//! Saves written on an embedder's worker thread (Margin's autosave): the session hands out an
//! immutable [`SaveSnapshot`], the worker serializes, checks and writes it, and
//! [`Session::acknowledge_save`] records the result. Successive background saves coalesce: each
//! appends one incremental revision with every change since the document was opened to the file
//! as opened, so a reading session that saves every second still adds one revision, not hundreds.

use super::{Arc, DocId, EditError, SaveOptions, Session, guard, writable, write_incremental};
use crate::history::Keys;

/// An immutable save job. Original file data and parsed objects are shared; only the edited
/// object overlay is copied. Serialization and file I/O belong on the caller's worker.
pub struct SaveSnapshot {
    pub id: DocId,
    pub generation: u64,
    cos: Option<pdfcraft_cos::Document>,
    bytes: Arc<Vec<u8>>,
    options: SaveOptions,
    keys: Keys,
    /// What [`Self::check`] expects the written file to open with and hold.
    password: Option<String>,
    pages: usize,
}

/// A file a save worker wrote, and whether the save rewrote the whole file (a repaired file, an
/// encryption change, a redaction), which renumbers objects so editing continues from it.
pub struct SavedFile {
    pub bytes: Arc<Vec<u8>>,
    rewrote: bool,
}

/// What [`Session::acknowledge_save`] did with a written file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveAck {
    /// Edits made during the write are still unsaved.
    Superseded,
    /// The document is clean.
    Saved,
    /// The document is clean and now edits the rewritten file: views redraw their pages.
    Reopened,
}

impl SaveSnapshot {
    /// The file to write: the document as opened plus one incremental revision with every
    /// change, without typed Password-field values (#3926), as [`Session::save_bytes`] writes them.
    pub fn bytes(&self) -> Result<Arc<Vec<u8>>, EditError> {
        let Some(cos) = self.cos.as_ref().filter(|cos| cos.is_modified()) else { return Ok(self.bytes.clone()) };
        let cos = writable(cos)?;
        guard(|| write_incremental(&cos, &self.options)).map_err(EditError::Write)?.map(Arc::new).map_err(|err| EditError::Write(err.to_string()))
    }

    /// Check that `bytes`, from [`Self::bytes`], reopen as this document before the worker writes
    /// them, as [`Session::check_save`] does for a save on the app's thread (#1698).
    pub fn check(&self, bytes: &Arc<Vec<u8>>) -> Result<(), EditError> {
        if self.cos.is_none() {
            return Ok(());
        }
        pdfcraft_cos::Document::open_with_password(bytes.clone(), self.keys.reopen.as_deref()).map_err(|e| EditError::Reopen(e.to_string()))?;
        pdfcraft_render::check_pages(bytes.clone(), self.password.as_deref(), self.pages).map_err(|e| EditError::Reopen(e.to_string()))
    }

    /// `bytes` once written.
    pub fn written(&self, bytes: Arc<Vec<u8>>) -> SavedFile {
        let rewrote = self
            .cos
            .as_ref()
            .is_some_and(|cos| cos.is_modified() && (cos.revisions().is_empty() || cos.encryption_changed() || cos.full_save_required()));
        SavedFile { bytes, rewrote }
    }
}

impl Session {
    /// Capture a save without copying file bytes, reparsing the PDF or rebuilding its renderer.
    /// A signed file must use the normal rebasing save path to preserve every signed revision.
    pub fn save_snapshot(&self, id: DocId) -> Result<SaveSnapshot, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        if doc.is_signed() {
            return Err(EditError::Signed);
        }
        Ok(SaveSnapshot {
            id,
            generation: doc.generation,
            cos: doc.editor.as_ref().map(|editor| editor.cos.clone()),
            bytes: doc.bytes.clone(),
            keys: doc.editor.as_ref().map(|editor| editor.keys.clone()).unwrap_or_default(),
            password: doc.password.clone(),
            pages: doc.info.pages.len(),
            // What the user removed again before saving (a cleared redaction mark) stays out of the file.
            options: SaveOptions { mod_date: self.now().map(pdfcraft_cos::pdf_date), drop_unlinked: true, ..SaveOptions::default() },
        })
    }

    /// Acknowledge a background write. Newer edits remain dirty. An incremental save keeps the
    /// opened graph as the base, which coalesces this session into one incremental revision
    /// instead of appending one per small edit; the written file becomes what Revert opens and
    /// what a save on the app's thread appends to. A full rewrite renumbered objects, so a
    /// current document continues from the written file, as [`Self::mark_saved`] does.
    pub fn acknowledge_save(&mut self, snapshot: SaveSnapshot, saved: SavedFile, path: Option<String>) -> Result<SaveAck, EditError> {
        let doc = self.doc_mut(snapshot.id)?;
        if doc.generation == snapshot.generation && saved.rewrote {
            self.mark_saved(snapshot.id, saved.bytes, path)?;
            return Ok(SaveAck::Reopened);
        }
        if let Some(editor) = doc.editor.as_mut() {
            editor.saved = saved.bytes;
            editor.saved_keys = snapshot.keys;
            editor.unsaved_file = false;
        }
        if let Some(path) = path {
            doc.name = std::path::Path::new(&path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
            doc.path = Some(path);
        }
        if doc.generation != snapshot.generation {
            return Ok(SaveAck::Superseded);
        }
        doc.dirty = false;
        Ok(SaveAck::Saved)
    }

    /// Bound history to a reader's configured undo depth, including snapshots already held.
    pub fn set_undo_limit(&mut self, id: DocId, limit: usize) -> Result<(), EditError> {
        if let Some(editor) = self.doc_mut(id)?.editor.as_mut() {
            editor.undo_limit = limit;
            editor.trim_history();
        }
        Ok(())
    }
}
