//! Margin PDF writes: one worker owns serialization and atomic replacement. The UI holds
//! immutable graph snapshots, never a whole-file copy or a reparsed saved document.
//!
//! Each document has at most one write in flight. Writes asked for meanwhile (Save,
//! close-after-save, autosave) wait in order and all happen; none replaces another. Callbacks
//! run only once the document's last requested write holds every edit made before it.
//!
//! Only writes over the document's own file come here (`editing::save`): Save As to another
//! file and a recovered document's first save are PdfCraft's clean full rewrites. What
//! PdfCraft's save does around the write is done here too: Will Save scripts, the Fill & Sign
//! preferences and question, the watcher's record of the file's version, and Apply redactions'
//! rule that only an asked-for save writes redacted (or un-redacted) content over the file.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::thread::JoinHandle;

use pdfcraft_engine::{DocId, SaveAck, SaveSnapshot, SavedFile};

use crate::file_watch::Stamp;
use crate::{Flavor, PdfCraftApp};

type AfterSave = Box<dyn FnOnce(&mut PdfCraftApp) + Send>;
type SaveWriter = Box<dyn Fn(&str, &[u8]) -> Result<(), String> + Send>;

/// Replace `destination` atomically. A file made read-only after Margin opened it is refused:
/// on Unix a rename would still replace it and silently drop the flag.
fn write_pdf(destination: &str, bytes: &[u8]) -> Result<(), String> {
    if std::fs::metadata(destination).is_ok_and(|meta| meta.permissions().readonly()) {
        return Err(format!("{destination} is read-only; Save As writes a copy"));
    }
    crate::editing::write_atomically(destination, bytes).map_err(|error| error.to_string())
}

struct Job {
    snapshot: SaveSnapshot,
    destination: String,
}

struct Completion {
    job: Job,
    result: Result<SavedFile, String>,
    /// The written file's version, read on the worker right after the write, so the watcher's
    /// record (`watch.known_as`) is this write and not a later one by another program.
    written_version: Option<Stamp>,
}

/// A view's Apply redactions bookkeeping (`DocView::redactions_applied` and the rest).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Redactions {
    applied: usize,
    undone_after_save: usize,
    cut_off: bool,
    downloaded: usize,
}

impl Redactions {
    fn of(app: &PdfCraftApp, id: DocId) -> Self {
        app.views.iter().find(|view| view.id == id).map_or_else(Self::default, |view| Self {
            applied: view.redactions_applied,
            undone_after_save: view.redactions_undone_after_save,
            cut_off: view.saved_redactions_cut_off,
            downloaded: view.redactions_downloaded,
        })
    }
}

/// A write asked for while another write of the same document is in flight.
struct Request {
    destination: String,
    after: Vec<AfterSave>,
    /// The redaction state the user chose to save over the file in (an explicit Save, after
    /// Save asked where if it had to); `None` for autosave.
    approved: Option<Redactions>,
}

struct Pending {
    /// Where the in-flight job writes.
    destination: String,
    /// Runs once the in-flight write succeeds and nothing newer is waiting.
    after: Vec<AfterSave>,
    /// The approval of the requests merged into the in-flight write, for its follow-up write.
    approved: Option<Redactions>,
    /// The redaction state when the in-flight write's bytes were taken.
    taken_with: Redactions,
    next: VecDeque<Request>,
}

#[derive(Default)]
pub(crate) struct SaveQueue {
    sender: Option<mpsc::Sender<Job>>,
    completions: Option<mpsc::Receiver<Completion>>,
    worker: Option<JoinHandle<()>>,
    pending: HashMap<DocId, Pending>,
}

impl SaveQueue {
    fn start(&mut self, context: Option<egui::Context>, writer: SaveWriter) -> Result<(), String> {
        if self.sender.is_some() {
            return Ok(());
        }
        let (sender, jobs) = mpsc::channel::<Job>();
        let (finished, completions) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("margin-save".into())
            .spawn(move || {
                for job in jobs {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let bytes = job.snapshot.bytes().map_err(|error| error.to_string())?;
                        // Bytes that don't reopen as the document never replace its file (#1698).
                        job.snapshot.check(&bytes).map_err(|error| error.to_string())?;
                        writer(&job.destination, &bytes)?;
                        Ok(job.snapshot.written(bytes))
                    }))
                    .unwrap_or_else(|_| Err("the save worker failed unexpectedly; the document remains unsaved".into()));
                    let written_version = result.as_ref().ok().and_then(|_| Stamp::at(&job.destination));
                    if finished.send(Completion { job, result, written_version }).is_err() {
                        // The app has shut down; completed writes remain durable on disk.
                        break;
                    }
                    if let Some(context) = &context {
                        context.request_repaint();
                    }
                }
            })
            .map_err(|error| format!("couldn't start the save worker: {error}"))?;
        self.sender = Some(sender);
        self.completions = Some(completions);
        self.worker = Some(worker);
        Ok(())
    }

    pub(crate) fn contains(&self, id: DocId) -> bool {
        self.pending.contains_key(&id)
    }

    pub(crate) fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

impl Drop for SaveQueue {
    fn drop(&mut self) {
        // Closing the job channel drains accepted writes before the worker exits.
        self.sender.take();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            log::error!("Margin save worker stopped unexpectedly during shutdown");
        }
    }
}

impl PdfCraftApp {
    /// Write document `id` over `destination` (its own file) without asking: autosave and
    /// Margin's own callers. The document's Will Save scripts run first, as for any save (#6969).
    pub(crate) fn queue_margin_save(&mut self, id: DocId, destination: String, after: Option<AfterSave>) -> bool {
        self.run_document_actions(id, pdfcraft_engine::js::DocumentScriptEvent::WillSave);
        self.request_margin_save(id, Request { destination, after: after.into_iter().collect(), approved: None })
    }

    /// An explicit Save of document `id` over its own file (`editing::save`), the steps
    /// PdfCraft's save takes before and after the write included: what was typed is committed
    /// (#166), Fill & Sign marks are flattened when that is the preference and the Will Save
    /// scripts run; once written, the placed signature is not asked about again (#2149). Save
    /// asked where first if Apply redactions needed it, so the current redaction state is the
    /// one the user chose to write over the file.
    pub(crate) fn queue_explicit_margin_save(&mut self, id: DocId, destination: &str, after: AfterSave) {
        if !self.commit_typing_in(id) || !self.flatten_fill_sign_for_save(id) {
            return;
        }
        self.run_document_actions(id, pdfcraft_engine::js::DocumentScriptEvent::WillSave);
        let approved = Some(Redactions::of(self, id));
        let after: AfterSave = Box::new(move |app: &mut PdfCraftApp| {
            app.placed_signatures.remove(&id);
            after(app);
        });
        self.request_margin_save(id, Request { destination: destination.to_owned(), after: vec![after], approved });
    }

    /// Send `request` to the worker, or queue it behind the write in flight. A request sent at
    /// once was checked by its caller (autosave waits while Save would ask where; an explicit
    /// Save was approved just now); a queued one is checked when its bytes are taken
    /// (`finish_margin_save`).
    fn request_margin_save(&mut self, id: DocId, request: Request) -> bool {
        let Request { destination, mut after, approved } = request;
        if let Some(pending) = self.margin_saves.pending.get_mut(&id) {
            match pending.next.back_mut() {
                Some(last) if last.destination == destination => {
                    last.after.append(&mut after);
                    last.approved = approved.or(last.approved);
                }
                // Same file as the write in flight: its completion writes again if edits arrived meanwhile.
                None if pending.destination == destination => {
                    pending.after.append(&mut after);
                    pending.approved = approved.or(pending.approved);
                }
                _ => pending.next.push_back(Request { destination, after, approved }),
            }
            return true;
        }
        let snapshot = match self.session.save_snapshot(id) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.margin_save_failed(id, error.to_string());
                return false;
            }
        };
        if self.margin_saves.sender.is_none()
            && let Err(error) = self.margin_saves.start(self.ctx.clone(), Box::new(write_pdf))
        {
            self.margin_save_failed(id, error);
            return false;
        }
        let taken_with = Redactions::of(self, id);
        let pending = Pending { destination: destination.clone(), after, approved, taken_with, next: VecDeque::new() };
        let sent = self.margin_saves.sender.as_ref().is_some_and(|sender| sender.send(Job { snapshot, destination }).is_ok());
        if !sent {
            self.margin_save_failed(id, "the save worker is unavailable".into());
            return false;
        }
        self.margin_saves.pending.insert(id, pending);
        true
    }

    /// Whether a queued write of document `id` to `destination` may go ahead now. Over the
    /// document's own file it may not when Save would ask where (`asks_where_to_save`) and the
    /// user didn't choose this redaction state: an Apply redactions, or an Undo past a saved
    /// one, landed after the write was asked for.
    fn may_write_over(&self, id: DocId, destination: &str, approved: Option<Redactions>) -> bool {
        let own = self.session.get(id).and_then(|doc| doc.path.as_deref()).is_some_and(|path| crate::same_file(path, destination));
        let asks_where = self.views.iter().find(|view| view.id == id).is_some_and(crate::editing::asks_where_to_save);
        !own || !asks_where || approved == Some(Redactions::of(self, id))
    }

    fn margin_save_failed(&mut self, id: DocId, error: String) {
        if let Some(generation) = self.session.get(id).map(|doc| doc.edit_generation()) {
            super::reading::save_failed(self, id, generation);
        }
        self.notify(format!("Couldn't save the PDF: {error}"));
    }

    fn finish_margin_save(&mut self, completion: Completion) {
        let Completion { job, result, written_version } = completion;
        let id = job.snapshot.id;
        let generation = job.snapshot.generation;
        let (mut after, approved, taken_with, mut next) = match self.margin_saves.pending.remove(&id) {
            Some(Pending { after, approved, taken_with, next, .. }) => (after, approved, taken_with, next),
            None => (Vec::new(), None, Redactions::default(), VecDeque::new()),
        };
        let holds_every_edit = match result {
            Err(error) => {
                super::reading::save_failed(self, id, generation);
                self.notify(format!("Couldn't save the PDF: {error}"));
                // A failed write never runs its callbacks (close-after-save keeps the tab open).
                after.clear();
                false
            }
            Ok(saved) => match self.session.acknowledge_save(job.snapshot, saved, Some(job.destination.clone())) {
                Err(error) => {
                    self.margin_save_failed(id, error.to_string());
                    after.clear();
                    false
                }
                Ok(ack) => {
                    super::reading::saved_pdf(self, id);
                    // Margin's own write is not another program's change on disk (#2166).
                    self.watch.known_as(id, written_version);
                    // The file holds the redaction state the write was approved in: as after
                    // PdfCraft's save, Save no longer asks where because of it. When a redaction
                    // step was applied or undone during the write, the document no longer
                    // matches a file that may now be redacted: Save asks where, and autosave
                    // waits, as after an Undo past a saved redaction.
                    let now = Redactions::of(self, id);
                    for view in self.views.iter_mut().filter(|view| view.id == id) {
                        if now == taken_with {
                            view.redactions_applied = 0;
                            view.redactions_undone_after_save = 0;
                            view.saved_redactions_cut_off = false;
                        } else if taken_with != Redactions::default() {
                            view.saved_redactions_cut_off = true;
                        }
                    }
                    // A full rewrite gives the document a new renderer and nothing else: the
                    // reader's selection and place must survive their own save landing.
                    if ack == SaveAck::Reopened
                        && let Some(view) = self.views.iter_mut().find(|view| view.id == id)
                    {
                        view.renderer_replaced();
                    }
                    ack != SaveAck::Superseded
                }
            },
        };
        match next.front_mut() {
            // Later requests carry this write's callbacks: they run once the last write lands.
            Some(first) => {
                after.append(&mut first.after);
                first.after = after;
            }
            None if holds_every_edit => {
                self.forget_recovery(id);
                for callback in after {
                    callback(self);
                }
            }
            // Save and close-after-save include edits made during the write.
            None if !after.is_empty() => next.push_back(Request { destination: job.destination, after, approved }),
            None => {}
        }
        // Every requested destination is written; a refused request leaves the rest to try.
        let mut ask_where: Vec<AfterSave> = Vec::new();
        while let Some(request) = next.pop_front() {
            if !self.may_write_over(id, &request.destination, request.approved) {
                // Autosave waits (`margin::reading`); a Save the user asked for asks where.
                ask_where.extend(request.after);
                continue;
            }
            if self.request_margin_save(id, request) {
                if let Some(pending) = self.margin_saves.pending.get_mut(&id) {
                    pending.next = next;
                }
                break;
            }
        }
        if !ask_where.is_empty()
            && let Some(index) = self.views.iter().position(|view| view.id == id)
        {
            self.save_then(index, crate::SaveTarget::As, move |app| {
                for callback in ask_where {
                    callback(app);
                }
            });
        }
    }

    /// Whether a write of document `id` is in flight or waiting behind one (Margin only).
    pub fn margin_save_pending(&self, id: DocId) -> bool {
        self.margin_saves.contains(id)
    }

    pub(crate) fn poll_margin_saves(&mut self) {
        if self.flavor != Flavor::Margin {
            return;
        }
        let completed: Vec<_> = self.margin_saves.completions.as_ref().map(|receiver| receiver.try_iter().collect()).unwrap_or_default();
        for completion in completed {
            self.finish_margin_save(completion);
        }
    }

    /// Explicit shutdown/close and signed legacy saves wait for accepted writes, never race them.
    pub(crate) fn wait_margin_saves(&mut self) {
        while self.margin_saves.is_pending() {
            let completion = self.margin_saves.completions.as_ref().and_then(|receiver| receiver.recv().ok());
            match completion {
                Some(completion) => self.finish_margin_save(completion),
                None => {
                    let failed: Vec<_> = self.margin_saves.pending.drain().map(|(id, _)| id).collect();
                    for id in failed {
                        self.margin_save_failed(id, "the save worker stopped before completing the write".into());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use pdfcraft_engine::Edit;

    use super::*;

    fn app(name: &str, bytes: Vec<u8>) -> (PdfCraftApp, DocId, PathBuf) {
        let root = std::env::temp_dir().join(format!("margin-save-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let destination = root.join("document.pdf");
        std::fs::write(&destination, &bytes).unwrap();
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.margin_keys.set_config_dir(root.join("config"));
        app.margin_keys.set_data_dir(root.join("data"));
        app.open_bytes("document.pdf", Some(destination.to_string_lossy().into_owned()), bytes).unwrap();
        let id = app.views[0].id;
        (app, id, root)
    }

    fn blank() -> Vec<u8> {
        let bytes = pdfcraft_engine::Session::new().create_blank(300.0, 400.0, 1).unwrap();
        (*bytes).clone()
    }

    fn edit(app: &mut PdfCraftApp, title: &str) {
        assert!(app.apply_edit(Edit::SetInfo { key: "Title".into(), value: title.into() }));
    }

    const WAIT: Duration = Duration::from_secs(5);

    fn text(path: &std::path::Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn title_on_disk(path: &std::path::Path) -> Option<String> {
        pdfcraft_render::inspect(Arc::new(std::fs::read(path).unwrap()), None).unwrap().title
    }

    fn set_read_only(path: &std::path::Path, read_only: bool) {
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(read_only);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    /// Each write reports its destination, then waits for one release: a write is reliably in
    /// flight while the test edits and asks for more.
    fn hold_writes(app: &mut PdfCraftApp) -> (mpsc::Receiver<String>, mpsc::Sender<()>) {
        let (started, started_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        app.margin_saves
            .start(
                None,
                Box::new(move |destination, bytes| {
                    started.send(destination.to_owned()).map_err(|error| error.to_string())?;
                    release_rx.recv().map_err(|error| error.to_string())?;
                    write_pdf(destination, bytes)
                }),
            )
            .unwrap();
        (started_rx, release)
    }

    #[test]
    fn every_destination_asked_for_during_a_write_is_written_in_order() {
        let (mut app, id, root) = app("every-destination", blank());
        let (started, release) = hold_writes(&mut app);
        let original = root.join("document.pdf");
        let copies = [root.join("first.pdf"), root.join("second.pdf")];
        edit(&mut app, "autosaved");
        assert!(app.queue_margin_save(id, text(&original), None));
        assert_eq!(started.recv_timeout(WAIT).unwrap(), text(&original));
        edit(&mut app, "copied");
        let callbacks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for copy in &copies {
            let counter = callbacks.clone();
            let count_callback: AfterSave = Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            });
            assert!(app.queue_margin_save(id, text(copy), Some(count_callback)));
        }
        for _ in 0..3 {
            release.send(()).unwrap();
        }
        app.wait_margin_saves();
        let order: Vec<String> = started.try_iter().collect();
        assert_eq!(order, [text(&copies[0]), text(&copies[1])], "each copy is written once, after the first write");
        assert_eq!(title_on_disk(&original).as_deref(), Some("autosaved"));
        for copy in &copies {
            assert_eq!(title_on_disk(copy).as_deref(), Some("copied"), "{copy:?} holds the edits made before it was asked for");
        }
        assert_eq!(callbacks.load(Ordering::SeqCst), 2, "callbacks run once the last write lands");
        let doc = app.session.get(id).unwrap();
        assert!(!doc.dirty);
        assert_eq!(doc.path.as_deref(), Some(text(&copies[1]).as_str()));
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn save_as_asked_for_during_a_failing_write_still_writes_the_copy() {
        let (mut app, id, root) = app("failed-then-copy", blank());
        let (started, release) = hold_writes(&mut app);
        let original = root.join("document.pdf");
        let before = std::fs::read(&original).unwrap();
        let copy = root.join("copy.pdf");
        edit(&mut app, "kept");
        assert!(app.queue_margin_save(id, text(&original), None));
        started.recv_timeout(WAIT).unwrap();
        set_read_only(&original, true);
        assert!(app.queue_margin_save(id, text(&copy), None));
        release.send(()).unwrap();
        release.send(()).unwrap();
        app.wait_margin_saves();
        assert_eq!(std::fs::read(&original).unwrap(), before, "the read-only original is untouched");
        assert!(std::fs::metadata(&original).unwrap().permissions().readonly());
        assert_eq!(title_on_disk(&copy).as_deref(), Some("kept"));
        assert!(!app.session.get(id).unwrap().dirty);
        assert_eq!(super::super::reading::save_status(&app, 0), super::super::reading::SaveStatus::Saved { autosave: true });
        set_read_only(&original, false);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_file_made_read_only_after_opening_is_never_replaced() {
        let (mut app, id, root) = app("made-read-only", blank());
        let original = root.join("document.pdf");
        let before = std::fs::read(&original).unwrap();
        set_read_only(&original, true);
        edit(&mut app, "unsaved");
        assert!(app.queue_margin_save(id, text(&original), None));
        app.wait_margin_saves();
        assert_eq!(std::fs::read(&original).unwrap(), before);
        assert!(std::fs::metadata(&original).unwrap().permissions().readonly(), "a rename never strips the flag");
        assert!(app.session.get(id).unwrap().dirty);
        assert!(matches!(super::super::reading::save_status(&app, 0), super::super::reading::SaveStatus::ReadOnly { dirty: true, .. }));
        set_read_only(&original, false);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn quitting_during_a_write_finishes_it_before_the_app_goes() {
        const SLOW_WRITE: Duration = Duration::from_millis(300);
        let (mut app, id, root) = app("quit-during", blank());
        let original = root.join("document.pdf");
        app.margin_saves
            .start(
                None,
                Box::new(|destination, bytes| {
                    std::thread::sleep(SLOW_WRITE);
                    write_pdf(destination, bytes)
                }),
            )
            .unwrap();
        edit(&mut app, "written at quit");
        assert!(app.queue_margin_save(id, text(&original), None));
        drop(app);
        assert_eq!(title_on_disk(&original).as_deref(), Some("written at quit"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn edits_during_a_save_stay_dirty_and_save_again() {
        let (mut app, id, root) = app("dirty-during", blank());
        let destination = root.join("document.pdf").to_string_lossy().into_owned();
        let (started, started_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        app.margin_saves
            .start(
                None,
                Box::new(move |destination, bytes| {
                    started.send(()).map_err(|error| error.to_string())?;
                    release_rx.recv().map_err(|error| error.to_string())?;
                    crate::editing::write_atomically(destination, bytes).map_err(|error| error.to_string())
                }),
            )
            .unwrap();
        edit(&mut app, "first");
        assert!(app.queue_margin_save(id, destination.clone(), None));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        edit(&mut app, "second");
        release.send(()).unwrap();
        app.wait_margin_saves();
        assert!(app.session.get(id).unwrap().dirty, "the older save cannot clear newer edits");
        assert!(app.queue_margin_save(id, destination.clone(), None));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        release.send(()).unwrap();
        app.wait_margin_saves();
        assert!(!app.session.get(id).unwrap().dirty);
        let disk = Arc::new(std::fs::read(&destination).unwrap());
        let info = pdfcraft_render::inspect(disk, None).unwrap();
        assert_eq!(info.title.as_deref(), Some("second"));
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A file with no cross-reference table: it opens repaired, so its first save rewrites it
    /// in full and the document continues from the written file (`SaveAck::Reopened`).
    const NEEDS_REPAIR: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj
4 0 obj << /Length 55 >> stream
BT /F1 14 Tf 20 150 Td (The quick brown fox) Tj ET
endstream endobj
5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
trailer << /Root 1 0 R >>
%%EOF";

    /// An autosave landing while the reader selects text must not take the selection away:
    /// the rewritten file reads the same, only the renderer behind the view is new.
    #[test]
    fn a_save_that_rewrites_the_file_keeps_the_text_selection() {
        let (mut app, id, root) = app("rewrite-keeps-selection", NEEDS_REPAIR.to_vec());
        assert!(!app.session.get(id).unwrap().repair_log().is_empty(), "the fixture needs repair");
        edit(&mut app, "rewritten");
        assert!(app.queue_margin_save(id, text(&root.join("document.pdf")), None));
        app.views[0].select_text(0, 4, 14);
        let selected = app.views[0].selection_range();
        app.wait_margin_saves();
        let doc = app.session.get(id).unwrap();
        assert!(!doc.dirty && doc.repair_log().is_empty(), "the write rewrote the file and the document reads from it");
        assert!(selected.is_some());
        assert_eq!(app.views[0].selection_range(), selected);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_write_keeps_edits_and_reports_failed_save_state() {
        let (mut app, id, root) = app("failure", blank());
        let original = std::fs::read(root.join("document.pdf")).unwrap();
        edit(&mut app, "unsaved");
        let destination = root.join("missing/document.pdf").to_string_lossy().into_owned();
        assert!(app.queue_margin_save(id, destination, None));
        app.wait_margin_saves();
        assert!(app.session.get(id).unwrap().dirty);
        assert_eq!(super::super::reading::save_status(&app, 0), super::super::reading::SaveStatus::Failed);
        assert_eq!(std::fs::read(root.join("document.pdf")).unwrap(), original);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writable_save_as_reenables_autosave_from_read_only_source() {
        let (mut app, id, root) = app("readonly-copy", blank());
        let original = root.join("document.pdf");
        let original_permissions = std::fs::metadata(&original).unwrap().permissions();
        let mut readonly = original_permissions.clone();
        readonly.set_readonly(true);
        std::fs::set_permissions(&original, readonly).unwrap();
        super::super::reading::opened(&mut app, 0);
        assert!(matches!(super::super::reading::save_status(&app, 0), super::super::reading::SaveStatus::ReadOnly { dirty: false, .. }));
        edit(&mut app, "copy");
        app.save_override = Some(root.join("copy.pdf").to_string_lossy().into_owned());
        assert!(app.save_active(crate::SaveTarget::As), "Save As to another file is a clean copy, written at once");
        app.wait_margin_saves();
        assert_eq!(super::super::reading::save_status(&app, 0), super::super::reading::SaveStatus::Saved { autosave: true });
        edit(&mut app, "new edit");
        assert!(app.session.get(id).unwrap().dirty);
        assert_eq!(super::super::reading::save_status(&app, 0), super::super::reading::SaveStatus::Saving);
        std::fs::set_permissions(original, original_permissions).unwrap();
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn save_as_after_inflight_write_preserves_destination_and_close_callback() {
        let (mut app, id, root) = app("save-as", blank());
        let first = root.join("document.pdf").to_string_lossy().into_owned();
        let second = root.join("copy.pdf").to_string_lossy().into_owned();
        edit(&mut app, "first");
        assert!(app.queue_margin_save(id, first, None));
        edit(&mut app, "second");
        let callback_ran = Arc::new(AtomicBool::new(false));
        let callback = callback_ran.clone();
        assert!(app.queue_margin_save(id, second.clone(), Some(Box::new(move |_| callback.store(true, Ordering::Relaxed)))));
        app.wait_margin_saves();
        assert!(callback_ran.load(Ordering::Relaxed));
        assert!(!app.session.get(id).unwrap().dirty);
        assert_eq!(app.session.get(id).unwrap().path.as_deref(), Some(second.as_str()));
        assert!(std::fs::read(second).unwrap().windows(6).any(|bytes| bytes == b"second"));
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn closing_while_save_callback_closes_the_same_tab_never_closes_another_tab() {
        let (mut app, id, root) = app("close-callback", blank());
        edit(&mut app, "saved before close");
        let destination = root.join("document.pdf").to_string_lossy().into_owned();
        assert!(app.queue_margin_save(id, destination, Some(Box::new(|app| app.close_tab(0)))));
        let extra = app.session.create_blank(300.0, 400.0, 1).unwrap();
        app.open_bytes("other.pdf", None, (*extra).clone()).unwrap();
        let other = app.views[1].id;
        app.close_tab(0);
        assert!(app.session.get(id).is_none());
        assert!(app.session.get(other).is_some());
        assert_eq!(app.views.len(), 1);
        assert_eq!(app.views[0].id, other);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn revert_never_discards_another_document_when_save_callback_closes_its_target() {
        let (mut app, id, root) = app("revert-callback", blank());
        edit(&mut app, "saved before close");
        let destination = root.join("document.pdf").to_string_lossy().into_owned();
        assert!(app.queue_margin_save(id, destination, Some(Box::new(|app| app.close_tab(0)))));
        let extra = app.session.create_blank(300.0, 400.0, 1).unwrap();
        app.open_bytes("other.pdf", None, (*extra).clone()).unwrap();
        let other = app.views[1].id;
        edit(&mut app, "keep these unsaved edits");
        app.active = Some(0);
        app.revert_active();
        assert!(app.session.get(id).is_none());
        let remaining = app.session.get(other).unwrap();
        assert!(remaining.dirty);
        assert_eq!(remaining.info_value("Title").as_deref(), Some("keep these unsaved edits"));
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A document whose file already holds `title` in its first revision.
    fn titled(title: &str) -> Vec<u8> {
        let mut session = pdfcraft_engine::Session::new();
        let blank = session.create_blank(300.0, 400.0, 1).unwrap();
        let id = session.open_new("titled.pdf", blank).unwrap();
        session.apply(id, Edit::SetInfo { key: "Title".into(), value: title.into() }).unwrap();
        (*session.save_full_bytes(id).unwrap()).clone()
    }

    fn contains(bytes: &[u8], needle: &[u8]) -> bool {
        bytes.windows(needle.len()).any(|window| window == needle)
    }

    /// H3 (#2165): Save As to another file is PdfCraft's clean full rewrite, not the file as
    /// opened plus an increment that still holds what was replaced or deleted.
    #[test]
    fn save_as_to_another_file_is_a_clean_copy() {
        let (mut app, id, root) = app("clean-copy", titled("secret-original"));
        edit(&mut app, "public");
        let copy = root.join("copy.pdf");
        app.save_override = Some(text(&copy));
        assert!(app.save_active(crate::SaveTarget::As));
        app.wait_margin_saves();
        let written = std::fs::read(&copy).unwrap();
        assert!(!contains(&written, b"secret-original"), "the replaced title is not recoverable from the copy");
        assert_eq!(pdfcraft_cos::Document::open(Arc::new(written)).unwrap().revisions().len(), 1);
        assert_eq!(title_on_disk(&copy).as_deref(), Some("public"));
        assert_eq!(app.session.get(id).unwrap().path.as_deref(), Some(text(&copy).as_str()));
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// M3 (#2166): an explicit Save over a file another program changed since it was opened asks
    /// first, as PdfCraft's does; Save anyway then writes it.
    #[test]
    fn an_explicit_save_over_a_file_changed_on_disk_asks_first() {
        let (mut app, id, root) = app("changed-on-disk", blank());
        let original = root.join("document.pdf");
        app.watch.known_now(id, &text(&original));
        edit(&mut app, "mine");
        let theirs = titled("theirs");
        std::fs::write(&original, &theirs).unwrap();
        assert!(!app.save_active(crate::SaveTarget::InPlace));
        app.wait_margin_saves();
        assert_eq!(app.dialog, Some(crate::Dialog::OverwriteChanged), "the changed-on-disk question shows");
        assert_eq!(std::fs::read(&original).unwrap(), theirs, "the other program's version is untouched");
        app.overwrite_changed_file();
        app.wait_margin_saves();
        assert_eq!(title_on_disk(&original).as_deref(), Some("mine"));
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// M3: the watcher's record follows Margin's own writes, so neither an autosave nor an
    /// explicit Save after it reads as another program's change.
    #[test]
    fn margin_writes_are_not_changes_on_disk() {
        let (mut app, id, root) = app("own-writes", blank());
        let original = text(&root.join("document.pdf"));
        app.watch.known_now(id, &original);
        edit(&mut app, "autosaved");
        assert!(app.queue_margin_save(id, original.clone(), None));
        app.wait_margin_saves();
        assert!(!app.watch.disk_differs(id, &original, None), "the autosave is the version the document came from");
        edit(&mut app, "saved");
        assert!(!app.save_active(crate::SaveTarget::InPlace));
        app.wait_margin_saves();
        assert_eq!(app.dialog, None, "nothing asks about Margin's own write");
        assert_eq!(title_on_disk(&root.join("document.pdf")).as_deref(), Some("saved"));
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A one-field form (`log`) whose Will Save action stamps the field.
    fn stamped_on_save() -> Vec<u8> {
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R /AA << /WS << /S /JavaScript /JS (getField\\('log'\\).value = 'stamped';) >> >> /AcroForm << /Fields [4 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 5 0 R >> >> >> >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>",
            "<< /Type /Page /Parent 2 0 R /Annots [4 0 R] >>",
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (log) /V () /Rect [20 250 180 270] /P 3 0 R /F 4 >>",
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (number, object) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", number + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
        out
    }

    /// M3 (#6969): the document's Will Save scripts run before Margin's write, and what they
    /// change is in the file.
    #[test]
    fn an_explicit_save_runs_will_save_scripts() {
        let (mut app, _, root) = app("will-save", stamped_on_save());
        edit(&mut app, "edited");
        assert!(!app.save_active(crate::SaveTarget::InPlace));
        app.wait_margin_saves();
        let mut reader = pdfcraft_engine::Session::new();
        reader.set_javascript(false);
        let saved = Arc::new(std::fs::read(root.join("document.pdf")).unwrap());
        let reopened = reader.open("document.pdf", None, saved, None).unwrap();
        let log = reader.get(reopened).unwrap().form.iter().find(|field| field.name == "log").unwrap().value.join(",");
        assert_eq!(log, "stamped");
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// M3 (#2149): with "flatten on every save" remembered, an explicit Save locks Fill & Sign
    /// marks into the page, and the saved document isn't asked about again.
    #[test]
    fn an_explicit_save_flattens_fill_and_sign_and_forgets_the_placed_signature() {
        let (mut app, id, root) = app("flatten", blank());
        let check = pdfcraft_engine::Shape::Mark { rect: [40.0, 80.0, 56.0, 96.0], mark: pdfcraft_engine::FillMark::Check };
        let style = pdfcraft_engine::Style::default_for(&check);
        let mark =
            Edit::AddAnnotation(pdfcraft_engine::NewAnnotation { page: 0, shape: check, style, contents: String::new(), author: "Ada".into() });
        app.session.apply(id, mark).unwrap();
        app.placed_signatures.insert(id);
        app.flatten_fill_sign_on_save = true;
        assert!(!app.save_active(crate::SaveTarget::InPlace));
        app.wait_margin_saves();
        let saved = Arc::new(std::fs::read(root.join("document.pdf")).unwrap());
        let annotations = pdfcraft_render::inspect(saved, None).unwrap().annotations;
        assert!(annotations.is_empty(), "the mark is page content: {annotations:?}");
        assert!(!app.placed_signatures.contains(&id), "what was saved isn't asked about again");
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// M3 (#1937): Save writes the repair of a file that opened damaged, edited or not.
    #[test]
    fn an_explicit_save_writes_the_repair_of_an_unedited_file() {
        let (mut app, id, root) = app("repair-unedited", NEEDS_REPAIR.to_vec());
        assert!(!app.session.get(id).unwrap().dirty);
        assert!(!app.save_active(crate::SaveTarget::InPlace));
        app.wait_margin_saves();
        let saved = std::fs::read(root.join("document.pdf")).unwrap();
        assert_ne!(saved, NEEDS_REPAIR, "the damaged bytes are not what Save writes");
        assert!(pdfcraft_cos::Document::open(Arc::new(saved)).unwrap().repair_log().is_empty());
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// M1: once Save has written the redacted document over its file (the user chose to, when
    /// Save asked where), later saves don't ask where again.
    #[test]
    fn a_save_over_the_file_clears_the_redaction_counts() {
        let (mut app, id, root) = app("redaction-counts", blank());
        let original = text(&root.join("document.pdf"));
        // As Apply redactions leaves them.
        app.views[0].redactions_applied = 1;
        edit(&mut app, "redacted");
        app.save_override = Some(original.clone());
        assert!(!app.save_active(crate::SaveTarget::InPlace));
        app.wait_margin_saves();
        assert_eq!(title_on_disk(&root.join("document.pdf")).as_deref(), Some("redacted"));
        assert_eq!(app.views[0].redactions_applied, 0);
        assert!(!crate::editing::asks_where_to_save(&app.views[0]), "the next Save writes the file again");
        assert_eq!(app.session.get(id).unwrap().path.as_deref(), Some(original.as_str()));
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// M1: a Save the user chose to write over the file in one redaction state never writes
    /// another one there: an Undo of the redaction during the write asks where instead.
    #[test]
    fn a_redaction_undone_during_a_save_is_not_written_over_the_file() {
        let (mut app, _, root) = app("redaction-during", blank());
        let (started, release) = hold_writes(&mut app);
        let original = root.join("document.pdf");
        let copy = root.join("restored.pdf");
        app.views[0].redactions_applied = 1;
        edit(&mut app, "redacted");
        app.save_override = Some(text(&original));
        assert!(!app.save_active(crate::SaveTarget::InPlace));
        assert_eq!(started.recv_timeout(WAIT).unwrap(), text(&original));
        app.save_override = None;
        // Undo of the unsaved Apply redactions, then the content it brings back.
        app.views[0].redactions_applied = 0;
        edit(&mut app, "restored");
        app.pick_override = Some(vec![text(&copy)]);
        release.send(()).unwrap();
        app.wait_margin_saves();
        app.process_picked();
        assert!(started.try_recv().is_err(), "the original is written once");
        assert_eq!(title_on_disk(&original).as_deref(), Some("redacted"));
        assert_eq!(title_on_disk(&copy).as_deref(), Some("restored"), "the restored content went where the user chose");
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// App time each frame of [`window`] steps.
    const STEP_SECS: f32 = 0.1;
    /// Longer than autosave's debounce (`reading::DEBOUNCE_SECS`), with room for its write.
    const PAST_DEBOUNCE_SECS: f32 = 3.0;

    /// Margin's window over `app`, so `reading::tick` autosaves as it does for a reader.
    fn window(app: PdfCraftApp) -> egui_kittest::Harness<'static, PdfCraftApp> {
        egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 900.0)).with_step_dt(STEP_SECS).build_eframe(move |_cc| app)
    }

    /// Run `secs` of app time, letting each write the worker was handed land.
    fn run_for(window: &mut egui_kittest::Harness<'static, PdfCraftApp>, secs: f32) {
        for _ in 0..(secs / STEP_SECS).ceil() as usize {
            window.run_steps(1);
            window.state_mut().wait_margin_saves();
        }
    }

    /// H2 (#3923): a recovered document's path comes from its recovery sidecar, which anyone who
    /// can write the recovery folder can name; autosave never writes there before a Save has
    /// confirmed it, and the footer says the changes are unsaved.
    #[test]
    fn a_recovered_document_is_not_autosaved_to_the_path_its_sidecar_names() {
        let root = std::env::temp_dir().join(format!("margin-save-recovered-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let named = root.join("named.pdf");
        std::fs::write(&named, blank()).unwrap();
        let store = crate::RecoveryStore::new(root.join("recovery"));
        {
            let mut crashed = PdfCraftApp::new();
            crashed.enable_recovery(store.clone());
            crashed.open_bytes("named.pdf", Some(text(&named)), blank()).unwrap();
            assert!(crashed.apply_edit(Edit::SetInfo { key: "Title".into(), value: "recovered".into() }));
            crashed.autosave_now();
        }
        let before = std::fs::read(&named).unwrap();
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.margin_keys.set_config_dir(root.join("config"));
        app.margin_keys.set_data_dir(root.join("data"));
        app.enable_recovery(store);
        let keys: Vec<String> = app.recoverable.iter().map(|meta| meta.key.clone()).collect();
        assert_eq!(keys.len(), 1);
        app.recover(&keys);
        let mut window = window(app);
        run_for(&mut window, PAST_DEBOUNCE_SECS);
        assert_eq!(std::fs::read(&named).unwrap(), before, "the named file is untouched");
        assert_eq!(super::super::reading::save_status(window.state(), 0), super::super::reading::SaveStatus::Unsaved);
        drop(window);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// M1: after Apply redactions, Save asks where (and proposes a `_Redacted` copy); after an
    /// Undo past a saved one, the file is redacted and the document isn't. Autosave writes
    /// neither over the file; once Save no longer has to ask, it autosaves again.
    #[test]
    fn autosave_waits_while_save_would_ask_where_because_of_redactions() {
        let (app, _, root) = app("redaction-autosave", blank());
        let original = root.join("document.pdf");
        let before = std::fs::read(&original).unwrap();
        let mut window = window(app);
        run_for(&mut window, STEP_SECS);
        // As Apply redactions leaves the counts.
        window.state_mut().views[0].redactions_applied = 1;
        edit(window.state_mut(), "redacted");
        run_for(&mut window, PAST_DEBOUNCE_SECS);
        assert_eq!(std::fs::read(&original).unwrap(), before, "the redacted document is not written over the original");
        assert_eq!(super::super::reading::save_status(window.state(), 0), super::super::reading::SaveStatus::Unsaved);
        // As an Undo past a saved Apply redactions leaves them.
        window.state_mut().views[0].redactions_applied = 0;
        window.state_mut().views[0].redactions_undone_after_save = 1;
        edit(window.state_mut(), "unredacted");
        run_for(&mut window, PAST_DEBOUNCE_SECS);
        assert_eq!(std::fs::read(&original).unwrap(), before, "removed content is not written back");
        window.state_mut().views[0].redactions_undone_after_save = 0;
        edit(window.state_mut(), "plain");
        run_for(&mut window, PAST_DEBOUNCE_SECS);
        assert_eq!(title_on_disk(&original).as_deref(), Some("plain"), "with nothing to ask, autosave writes");
        drop(window);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Reproducible large-file comparison, kept opt-in so normal tests do not allocate 64 MiB.
    #[test]
    #[ignore = "large-PDF autosave timing comparison"]
    fn large_pdf_save_stall_measurement() {
        use pdfcraft_cos::{Dict, Object, SaveOptions, Stream};

        const PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
        const REPETITIONS: usize = 5;
        let mut graph = pdfcraft_cos::Document::open(Arc::new(blank())).unwrap();
        let payload = graph.add(Object::Stream(Stream::from_raw(Dict::new(), vec![b'x'; PAYLOAD_BYTES])));
        graph.update_dict(graph.root().unwrap(), |catalog| catalog.set(b"MarginSyntheticPayload".to_vec(), Object::Ref(payload))).unwrap();
        let bytes = pdfcraft_cos::write_full(&graph, &SaveOptions::default()).unwrap();
        let highlight = || {
            Edit::AddAnnotation(pdfcraft_engine::NewAnnotation {
                page: 0,
                shape: pdfcraft_engine::Shape::TextMarkup {
                    kind: pdfcraft_engine::Markup::Highlight,
                    quads: vec![pdfcraft_engine::rect_quad([20.0, 30.0, 80.0, 45.0])],
                },
                style: Default::default(),
                contents: String::new(),
                author: "Synthetic benchmark".into(),
            })
        };
        let mut baseline = Vec::new();
        let mut handoff = Vec::new();
        let mut acknowledge = Vec::new();
        for repetition in 0..REPETITIONS {
            let (mut old_app, old_id, old_root) = app(&format!("bench-old-{repetition}"), bytes.clone());
            assert!(old_app.apply_edit(highlight()));
            let destination = old_root.join("document.pdf").to_string_lossy().into_owned();
            let started = Instant::now();
            let saved = old_app.session.save_bytes(old_id).unwrap();
            crate::editing::write_atomically(&destination, &saved).unwrap();
            old_app.session.mark_saved(old_id, saved, Some(destination)).unwrap();
            baseline.push(started.elapsed().as_secs_f64() * 1000.0);
            drop(old_app);
            std::fs::remove_dir_all(old_root).unwrap();

            let (mut new_app, new_id, new_root) = app(&format!("bench-new-{repetition}"), bytes.clone());
            assert!(new_app.apply_edit(highlight()));
            let destination = new_root.join("document.pdf").to_string_lossy().into_owned();
            let started = Instant::now();
            assert!(new_app.queue_margin_save(new_id, destination, None));
            handoff.push(started.elapsed().as_secs_f64() * 1000.0);
            let completion = new_app.margin_saves.completions.as_ref().unwrap().recv().unwrap();
            let started = Instant::now();
            new_app.finish_margin_save(completion);
            acknowledge.push(started.elapsed().as_secs_f64() * 1000.0);
            drop(new_app);
            std::fs::remove_dir_all(new_root).unwrap();
        }
        baseline.sort_by(f64::total_cmp);
        handoff.sort_by(f64::total_cmp);
        acknowledge.sort_by(f64::total_cmp);
        println!(
            "64 MiB PDF, 5 runs, median milliseconds: old UI save={:.3}, new UI handoff={:.3}, new UI acknowledge={:.3}",
            baseline[REPETITIONS / 2],
            handoff[REPETITIONS / 2],
            acknowledge[REPETITIONS / 2]
        );
    }
}
