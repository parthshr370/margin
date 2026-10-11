//! Margin PDF writes: one worker owns serialization and atomic replacement. The UI holds
//! immutable graph snapshots, never a whole-file copy or a reparsed saved document.

use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

use pdfcraft_engine::{DocId, SaveSnapshot};

use crate::{Flavor, PdfCraftApp};

type AfterSave = Box<dyn FnOnce(&mut PdfCraftApp) + Send>;
type SaveWriter = Box<dyn Fn(&str, &[u8]) -> Result<(), String> + Send>;

struct Job {
    snapshot: SaveSnapshot,
    destination: String,
}

struct Completion {
    job: Job,
    result: Result<Arc<Vec<u8>>, String>,
}

struct Pending {
    after: Vec<AfterSave>,
    destination: String,
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
                        Ok(bytes)
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
        if let Some(pending) = self.margin_saves.pending.get_mut(&id) {
            pending.destination = destination;
            if let Some(after) = after {
                pending.after.push(after);
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
        if self.margin_saves.sender.is_none() {
            let writer: SaveWriter =
                Box::new(|destination, bytes| crate::editing::write_atomically(destination, bytes).map_err(|error| error.to_string()));
            if let Err(error) = self.margin_saves.start(self.ctx.clone(), writer) {
                self.margin_save_failed(id, error);
                return false;
            }
        }
        let pending = Pending { after: after.into_iter().collect(), destination: destination.clone() };
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
        let pending = self.margin_saves.pending.remove(&id);
        match result {
            Err(error) => {
                super::reading::save_failed(self, id, generation);
                self.notify(format!("Couldn't save the PDF: {error}"));
            }
            Ok(bytes) => match self.session.acknowledge_save(job.snapshot, bytes, Some(job.destination.clone())) {
                Err(error) => self.margin_save_failed(id, error.to_string()),
                Ok(current) => {
                    super::reading::saved_pdf(self, id);
                    let needs_destination = pending.as_ref().is_some_and(|pending| pending.destination != job.destination);
                    if current && !needs_destination {
                        self.forget_recovery(id);
                        if let Some(pending) = pending {
                            for after in pending.after {
                                after(self);
                            }
                        }
                    } else if let Some(pending) = pending.filter(|pending| !pending.after.is_empty() || needs_destination) {
                        // Save As and close-after-save must include edits made during the write.
                        self.queue_margin_save(
                            id,
                            pending.destination,
                            Some(Box::new(move |app| {
                                for after in pending.after {
                                    after(app);
                                }
                            })),
                        );
                    }
                }
            },
        }
    }

    pub(crate) fn margin_save_pending(&self, id: DocId) -> bool {
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
        assert_eq!(super::super::reading::save_status(&app, 0), super::super::reading::SaveStatus::ReadOnly { dirty: false });
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
