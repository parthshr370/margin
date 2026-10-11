//! Margin PDF writes: one worker owns serialization and atomic replacement. The UI holds
//! immutable graph snapshots, never a whole-file copy or a reparsed saved document.
//!
//! Each document has at most one write in flight. Writes asked for meanwhile (Save, Save As,
//! close-after-save) wait in order and all happen; none replaces another. Callbacks run only
//! once the document's last requested write holds every edit made before it.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::thread::JoinHandle;

use pdfcraft_engine::{DocId, SaveAck, SaveSnapshot, SavedFile};

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
}

/// A write asked for while another write of the same document is in flight.
struct Request {
    destination: String,
    after: Vec<AfterSave>,
}

struct Pending {
    /// Where the in-flight job writes.
    destination: String,
    /// Runs once the in-flight write succeeds and nothing newer is waiting.
    after: Vec<AfterSave>,
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
                        writer(&job.destination, &bytes)?;
                        Ok(job.snapshot.written(bytes))
                    }))
                    .unwrap_or_else(|_| Err("the save worker failed unexpectedly; the document remains unsaved".into()));
                    if finished.send(Completion { job, result }).is_err() {
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
    pub(crate) fn queue_margin_save(&mut self, id: DocId, destination: String, after: Option<AfterSave>) -> bool {
        self.request_margin_save(id, Request { destination, after: after.into_iter().collect() })
    }

    fn request_margin_save(&mut self, id: DocId, request: Request) -> bool {
        let Request { destination, mut after } = request;
        if let Some(pending) = self.margin_saves.pending.get_mut(&id) {
            match pending.next.back_mut() {
                Some(last) if last.destination == destination => last.after.append(&mut after),
                // Same file as the write in flight: its completion writes again if edits arrived meanwhile.
                None if pending.destination == destination => pending.after.append(&mut after),
                _ => pending.next.push_back(Request { destination, after }),
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
        let pending = Pending { destination: destination.clone(), after, next: VecDeque::new() };
        let sent = self.margin_saves.sender.as_ref().is_some_and(|sender| sender.send(Job { snapshot, destination }).is_ok());
        if !sent {
            self.margin_save_failed(id, "the save worker is unavailable".into());
            return false;
        }
        self.margin_saves.pending.insert(id, pending);
        true
    }

    fn margin_save_failed(&mut self, id: DocId, error: String) {
        if let Some(generation) = self.session.get(id).map(|doc| doc.edit_generation()) {
            super::reading::save_failed(self, id, generation);
        }
        self.notify(format!("Couldn't save the PDF: {error}"));
    }

    fn finish_margin_save(&mut self, completion: Completion) {
        let Completion { job, result } = completion;
        let id = job.snapshot.id;
        let generation = job.snapshot.generation;
        let (mut after, mut next) = match self.margin_saves.pending.remove(&id) {
            Some(Pending { after, next, .. }) => (after, next),
            None => (Vec::new(), VecDeque::new()),
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
                    if ack == SaveAck::Reopened
                        && let Some(doc) = self.session.get(id)
                        && let Some(view) = self.views.iter_mut().find(|view| view.id == id)
                    {
                        view.document_changed(&doc.info);
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
            None if !after.is_empty() => next.push_back(Request { destination: job.destination, after }),
            None => {}
        }
        // Every requested destination is written; a refused request leaves the rest to try.
        while let Some(request) = next.pop_front() {
            if self.request_margin_save(id, request) {
                if let Some(pending) = self.margin_saves.pending.get_mut(&id) {
                    pending.next = next;
                }
                break;
            }
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
        assert!(!app.save_active(crate::SaveTarget::As), "the write completes asynchronously");
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
