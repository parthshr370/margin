//! Reload on change (`reader.reload_on_change`, `reader.reload_debounce_ms`): a document whose
//! file another program rewrites (a LaTeX build, a sync client) shows the new version at the
//! same place, zoom and page colour, with its view history kept.
//!
//! The folders of open files are watched, not the files: writers often save by renaming a new
//! file over the old one, which a watch on the old file would miss. Each change waits for the
//! debounce, then the file is read off the UI thread and checked for being still (size and
//! modification time unchanged across the read). A file that is missing, still being written or
//! doesn't open yet is tried again a few times before Margin says so and keeps the version shown.
//!
//! Margin's own saves change the file too: a file equal to what the document last saved is not
//! a change. A document with unsaved edits is never reloaded by itself: autosave pauses and the
//! reader picks Reload or Keep mine ([`holds`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use pdfcraft_engine::DocId;

use super::reading::{self, Spot};
use super::toast::{self, Action};
use crate::{Flavor, PdfCraftApp};

/// A read that found the file missing, half written or not opening is tried again after this.
const RETRY_SECS: f64 = 0.5;
/// Reads of one change before Margin gives up on it and says so (about 5 s of retries).
const MAX_ATTEMPTS: u32 = 10;
const MS_PER_SEC: f64 = 1000.0;

/// What the watcher knows about every open document with a file (`app.margin_reload`).
#[derive(Default)]
pub struct Reload {
    #[cfg(not(target_arch = "wasm32"))]
    watch: Option<watch::DirWatch>,
    /// The watcher couldn't start: said once, not retried every frame.
    #[cfg(not(target_arch = "wasm32"))]
    watch_failed: bool,
    docs: HashMap<DocId, Watched>,
}

impl Reload {
    /// Whether autosave must leave `doc` alone: its file changed on disk and Margin hasn't yet
    /// compared it with the document's unsaved edits, or the reader hasn't answered yet.
    pub fn holds(&self, doc: DocId) -> bool {
        self.docs.get(&doc).is_some_and(|watched| !matches!(watched.phase, Phase::Idle | Phase::Gone))
    }
}

/// One document's file.
struct Watched {
    /// The document's path as it was opened (perhaps relative to the working folder).
    opened_as: String,
    /// The same, absolute: what is watched and read.
    path: PathBuf,
    phase: Phase,
}

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    /// A change was seen: read the file at `due`. `attempts` reads of this change failed.
    Pending { due: f64, attempts: u32 },
    /// The file is being read off the UI thread.
    Reading { result: Receiver<Disk>, attempts: u32 },
    /// The file changed while the document had unsaved edits: the reader chooses.
    Conflict { bytes: Arc<Vec<u8>> },
    /// The file is gone (said once); the next change to it reads it again.
    Gone,
}

impl Phase {
    /// A change to the file was seen at `now`: read it once nothing changed for `debounce`.
    /// A question already asked stays until answered; the newest file is read then.
    fn changed(&mut self, now: f64, debounce: f64) {
        if !matches!(self, Self::Conflict { .. }) {
            *self = Self::Pending { due: now + debounce, attempts: 0 };
        }
    }

    /// Read `attempts` failed: try again shortly, or give up (`false`) after [`MAX_ATTEMPTS`].
    fn retry(&mut self, now: f64, attempts: u32) -> bool {
        let attempts = attempts.saturating_add(1);
        if attempts >= MAX_ATTEMPTS {
            *self = Self::Idle;
            return false;
        }
        *self = Self::Pending { due: now + RETRY_SECS, attempts };
        true
    }
}

/// The file as read off the UI thread.
enum Disk {
    Read(Arc<Vec<u8>>),
    Missing,
    /// Its size or modification time changed while it was read: still being written.
    Changing,
    Failed(String),
}

/// What a file read means for its document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// What the document last saved (Margin's own save) or showed: nothing to do.
    Unchanged,
    Reload,
    /// Changed under unsaved edits.
    Ask,
}

fn outcome(disk: &[u8], saved: &[u8], unsaved: bool) -> Outcome {
    if disk == saved {
        Outcome::Unchanged
    } else if unsaved {
        Outcome::Ask
    } else {
        Outcome::Reload
    }
}

/// Every frame (Margin only): follow the open files, read changed ones and reload or ask.
pub(crate) fn tick(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if app.flavor != Flavor::Margin {
        return;
    }
    let reader = &app.margin_keys.settings().reader;
    if !reader.reload_on_change {
        // Dropping the watcher stops its thread; a question asked before is withdrawn.
        for doc in std::mem::take(&mut app.margin_reload).docs.into_keys() {
            toast::withdraw(app, Action::Reload(doc));
        }
        return;
    }
    let debounce = f64::from(reader.reload_debounce_ms) / MS_PER_SEC;
    let now = ctx.input(|input| input.time);
    follow_open_files(app, ctx);
    #[cfg(not(target_arch = "wasm32"))]
    take_events(app, now, debounce);
    let ids: Vec<DocId> = app.margin_reload.docs.keys().copied().collect();
    let mut wake: Option<f64> = None;
    for id in ids {
        if let Some(at) = step(app, ctx, id, now) {
            wake = Some(wake.map_or(at, |w| w.min(at)));
        }
    }
    if let Some(at) = wake {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((at - now).max(0.0)));
    }
    ask_again(app);
}

/// Start following documents opened since last frame, stop following closed ones.
fn follow_open_files(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let open: HashMap<DocId, &str> = app.views.iter().filter_map(|view| Some((view.id, app.session.get(view.id)?.path.as_deref()?))).collect();
    let reload = &mut app.margin_reload;
    let closed: Vec<DocId> =
        reload.docs.iter().filter(|(id, watched)| open.get(id) != Some(&watched.opened_as.as_str())).map(|(id, _)| *id).collect();
    for id in closed {
        if let Some(watched) = reload.docs.remove(&id) {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(watch) = reload.watch.as_mut() {
                watch.release(&watched.path);
            }
            #[cfg(target_arch = "wasm32")]
            let _ = watched;
        }
    }
    for (id, opened_as) in open {
        if reload.docs.contains_key(&id) {
            continue;
        }
        // A file given on the command line may be relative: its folder is the working folder.
        let path = std::path::absolute(opened_as).unwrap_or_else(|_| PathBuf::from(opened_as));
        #[cfg(not(target_arch = "wasm32"))]
        {
            if reload.watch.is_none() && !reload.watch_failed {
                match watch::DirWatch::start(ctx) {
                    Ok(watch) => reload.watch = Some(watch),
                    Err(err) => {
                        reload.watch_failed = true;
                        log::warn!("reload on change: can't watch files: {err}");
                    }
                }
            }
            if let Some(watch) = reload.watch.as_mut()
                && let Err(err) = watch.follow(&path)
            {
                log::warn!("reload on change: can't watch {}: {err}", path.display());
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = ctx;
        reload.docs.insert(id, Watched { opened_as: opened_as.to_owned(), path, phase: Phase::Idle });
    }
}

/// Mark the documents whose files the watcher saw change.
#[cfg(not(target_arch = "wasm32"))]
fn take_events(app: &mut PdfCraftApp, now: f64, debounce: f64) {
    let reload = &mut app.margin_reload;
    let Some(watch) = reload.watch.as_ref() else { return };
    let changes = watch.changes();
    for watched in reload.docs.values_mut() {
        if changes.touches(&watched.path) {
            watched.phase.changed(now, debounce);
        }
    }
}

/// One document's part of a frame. Returns when to look again, if it waits for a time.
fn step(app: &mut PdfCraftApp, ctx: &egui::Context, id: DocId, now: f64) -> Option<f64> {
    let own_write_landing = saving(app, id);
    let watched = app.margin_reload.docs.get_mut(&id)?;
    match std::mem::take(&mut watched.phase) {
        // Margin's own write is still landing: what it wrote is known once it has.
        Phase::Pending { due, attempts } if now >= due && own_write_landing => {
            let due = now + RETRY_SECS;
            watched.phase = Phase::Pending { due, attempts };
            Some(due)
        }
        Phase::Pending { due, attempts } if now >= due => {
            watched.phase = Phase::Reading { result: read_in_background(ctx, &watched.path), attempts };
            None
        }
        Phase::Pending { due, attempts } => {
            watched.phase = Phase::Pending { due, attempts };
            Some(due)
        }
        Phase::Reading { result, attempts } => match result.try_recv() {
            Ok(disk) => {
                arrived(app, id, disk, now, attempts);
                None
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                watched.phase = Phase::Reading { result, attempts };
                None
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                log::warn!("reload on change: the read of {} stopped", watched.path.display());
                None
            }
        },
        Phase::Conflict { bytes } => {
            // Saved or reverted meanwhile: no edits to protect any more, look at the file again.
            let unsaved = index_of(app, id).is_some_and(|index| app.has_unsaved_work(index));
            let watched = app.margin_reload.docs.get_mut(&id)?;
            watched.phase = if unsaved { Phase::Conflict { bytes } } else { Phase::Pending { due: now, attempts: 0 } };
            if !unsaved {
                toast::withdraw(app, Action::Reload(id));
            }
            None
        }
        other => {
            watched.phase = other;
            None
        }
    }
}

/// A read of `id`'s file finished.
fn arrived(app: &mut PdfCraftApp, id: DocId, disk: Disk, now: f64, attempts: u32) {
    let Some(index) = index_of(app, id) else { return };
    let name = app.session.get(id).map(pdfcraft_engine::Document::display_name).unwrap_or_default();
    let bytes = match disk {
        Disk::Read(bytes) => bytes,
        Disk::Missing => {
            if !retry(app, id, now, attempts) {
                set_phase(app, id, Phase::Gone);
                app.notify(format!("“{name}” was moved or deleted. Margin keeps showing it; it reloads if the file comes back."));
            }
            return;
        }
        Disk::Changing => {
            if !retry(app, id, now, attempts) {
                app.notify(format!("“{name}” keeps changing on disk; it reloads after the next change."));
            }
            return;
        }
        Disk::Failed(err) => {
            app.notify(format!("Couldn't read “{name}” to reload it: {err}"));
            return;
        }
    };
    // Read while Margin's own write was landing: look again once it has.
    if saving(app, id) {
        set_phase(app, id, Phase::Pending { due: now + RETRY_SECS, attempts });
        return;
    }
    let Some(saved) = app.session.get(id).map(pdfcraft_engine::Document::saved_bytes) else { return };
    match outcome(&bytes, &saved, app.has_unsaved_work(index)) {
        Outcome::Unchanged => {}
        Outcome::Ask => {
            set_phase(app, id, Phase::Conflict { bytes });
            ask(app, id);
        }
        Outcome::Reload => {
            if let Err(err) = apply(app, index, bytes)
                && !retry(app, id, now, attempts)
            {
                // A build that failed half way leaves a broken file: keep showing the last good one.
                app.notify(format!("“{name}” changed on disk but doesn't open: {err}. Showing the version opened before."));
            }
        }
    }
}

fn retry(app: &mut PdfCraftApp, id: DocId, now: f64, attempts: u32) -> bool {
    app.margin_reload.docs.get_mut(&id).is_some_and(|watched| watched.phase.retry(now, attempts))
}

fn set_phase(app: &mut PdfCraftApp, id: DocId, phase: Phase) {
    if let Some(watched) = app.margin_reload.docs.get_mut(&id) {
        watched.phase = phase;
    }
}

/// Whether Margin's own write of `id` is in flight or queued (`margin::saves`). Until it lands
/// `saved_bytes` can't tell that write from another program's.
#[cfg(not(target_arch = "wasm32"))]
fn saving(app: &PdfCraftApp, id: DocId) -> bool {
    app.margin_save_pending(id)
}

#[cfg(target_arch = "wasm32")]
fn saving(_: &PdfCraftApp, _: DocId) -> bool {
    false
}

fn index_of(app: &PdfCraftApp, id: DocId) -> Option<usize> {
    app.views.iter().position(|view| view.id == id)
}

fn ask(app: &mut PdfCraftApp, id: DocId) {
    let name = app.session.get(id).map(pdfcraft_engine::Document::display_name).unwrap_or_default();
    let text = format!("“{name}” changed on disk, and it has edits Margin hasn't saved. Reload drops them; Keep mine saves them over the file.");
    toast::ask(app, text, Action::Reload(id), Action::KeepMine(id));
}

/// A question that another held message replaced is asked again once that one goes.
fn ask_again(app: &mut PdfCraftApp) {
    if app.margin_chrome.notices.held().is_some() {
        return;
    }
    let waiting = app.margin_reload.docs.iter().find(|(_, watched)| matches!(watched.phase, Phase::Conflict { .. })).map(|(id, _)| *id);
    if let Some(id) = waiting {
        ask(app, id);
    }
}

/// The reader's answer to the question about `doc`: `reload` shows the file's version (the
/// unsaved edits go), otherwise the edits stay and autosave or Save writes them.
pub(crate) fn resolve(app: &mut PdfCraftApp, doc: DocId, reload: bool) {
    let Some(watched) = app.margin_reload.docs.get_mut(&doc) else { return };
    let Phase::Conflict { bytes } = std::mem::take(&mut watched.phase) else { return };
    if !reload {
        return;
    }
    let Some(index) = index_of(app, doc) else { return };
    if let Err(err) = apply(app, index, bytes) {
        app.notify(format!("Couldn't reload: {err}. Your edits are still here."));
    }
}

/// Read `path` on a worker thread; the result arrives on the receiver and wakes `ctx`.
fn read_in_background(ctx: &egui::Context, path: &Path) -> Receiver<Disk> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let (path, ctx) = (path.to_path_buf(), ctx.clone());
    let read = move || {
        if sender.send(read_still(&path)).is_err() {
            log::debug!("reload on change: the document closed before its file was read");
        }
        ctx.request_repaint();
    };
    #[cfg(not(target_arch = "wasm32"))]
    if let Err(err) = std::thread::Builder::new().name("margin-reload".into()).spawn(read) {
        log::warn!("reload on change: no reader thread: {err}");
    }
    #[cfg(target_arch = "wasm32")]
    read();
    receiver
}

/// The file's bytes, if it held still while they were read.
fn read_still(path: &Path) -> Disk {
    let stamp = |meta: &std::fs::Metadata| (meta.len(), meta.modified().ok());
    let before = match std::fs::metadata(path) {
        Ok(meta) => stamp(&meta),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Disk::Missing,
        Err(err) => return Disk::Failed(err.to_string()),
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Disk::Missing,
        Err(err) => return Disk::Failed(err.to_string()),
    };
    match std::fs::metadata(path) {
        Ok(meta) if stamp(&meta) == before && u64::try_from(bytes.len()).ok() == Some(meta.len()) => Disk::Read(Arc::new(bytes)),
        Ok(_) => Disk::Changing,
        Err(_) => Disk::Missing,
    }
}

/// Show `bytes` as view `index`'s document, at the reader's place: same spot on the page (the
/// last page when the document got shorter), zoom, fit, page colour, trim and history.
pub(crate) fn apply(app: &mut PdfCraftApp, index: usize, bytes: Arc<Vec<u8>>) -> Result<(), String> {
    let Some(view) = app.views.get(index) else { return Err("the document is no longer open".into()) };
    let (id, place) = (view.id, reading::spot(view));
    app.session.reload(id, bytes).map_err(|err| err.to_string())?;
    // Nothing unsaved is left to recover.
    app.forget_recovery(id);
    let (Some(doc), Some(view)) = (app.session.get(id), app.views.get_mut(index)) else { return Ok(()) };
    view.document_changed(&doc.info);
    // Edits typed or queued against the old version would land on the wrong content.
    view.forms.focus = None;
    view.forms.committed = None;
    view.pending_edit = None;
    // Recolouring keeps the old pixels per page: drop them with the pages they came from.
    view.page_color = Default::default();
    let place = carried_place(place, doc.info.pages.len());
    view.current = place.page;
    view.page_input = (place.page + 1).to_string();
    reading::go(view, place);
    Ok(())
}

/// `place` in a document now `pages` long.
fn carried_place(place: Spot, pages: usize) -> Spot {
    if place.page < pages { place } else { Spot { page: pages.saturating_sub(1), y: 0.0, x: place.x } }
}

#[cfg(not(target_arch = "wasm32"))]
mod watch {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::Receiver;

    use notify::Watcher as _;

    /// One watch on the folders of the open files, shared by the files in each folder.
    pub(super) struct DirWatch {
        watcher: notify::RecommendedWatcher,
        events: Receiver<notify::Result<notify::Event>>,
        /// Watched folders and how many open files are in each.
        folders: HashMap<PathBuf, usize>,
    }

    /// The paths changed since the last look.
    #[derive(Default)]
    pub(super) struct Changes {
        paths: Vec<PathBuf>,
        /// The watcher lost events: every file counts as changed.
        everything: bool,
    }

    impl Changes {
        pub(super) fn touches(&self, file: &Path) -> bool {
            self.everything || self.paths.iter().any(|path| same_file(path, file))
        }
    }

    /// Whether the event path `seen` names `file`: the same path, or the same name in the same
    /// folder once links are resolved (`/var` is `/private/var` to macOS's file events).
    fn same_file(seen: &Path, file: &Path) -> bool {
        if seen == file {
            return true;
        }
        if seen.file_name() != file.file_name() {
            return false;
        }
        let real = |path: &Path| path.parent().and_then(|dir| std::fs::canonicalize(dir).ok());
        real(seen).is_some_and(|dir| Some(dir) == real(file))
    }

    impl DirWatch {
        pub(super) fn start(ctx: &egui::Context) -> Result<Self, String> {
            let (sender, events) = std::sync::mpsc::channel();
            let ctx = ctx.clone();
            let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if sender.send(event).is_err() {
                    log::debug!("reload on change: the app stopped listening; dropping a change event");
                }
                ctx.request_repaint();
            })
            .map_err(|err| err.to_string())?;
            Ok(Self { watcher, events, folders: HashMap::new() })
        }

        /// Watch the folder of `file`.
        pub(super) fn follow(&mut self, file: &Path) -> Result<(), String> {
            let Some(folder) = folder_of(file) else { return Err("it has no folder".into()) };
            if let Some(count) = self.folders.get_mut(&folder) {
                *count = count.saturating_add(1);
                return Ok(());
            }
            self.watcher.watch(&folder, notify::RecursiveMode::NonRecursive).map_err(|err| err.to_string())?;
            self.folders.insert(folder, 1);
            Ok(())
        }

        /// `file` closed: stop watching its folder once no open file is in it.
        pub(super) fn release(&mut self, file: &Path) {
            let Some(folder) = folder_of(file) else { return };
            let Some(count) = self.folders.get_mut(&folder) else { return };
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.folders.remove(&folder);
                if let Err(err) = self.watcher.unwatch(&folder) {
                    log::debug!("reload on change: unwatching {}: {err}", folder.display());
                }
            }
        }

        /// Drain the queued events. Reads (access events) don't count.
        pub(super) fn changes(&self) -> Changes {
            let mut changes = Changes::default();
            for event in self.events.try_iter() {
                match event {
                    Ok(event) if event.need_rescan() => changes.everything = true,
                    Ok(event) if !matches!(event.kind, notify::EventKind::Access(_)) => changes.paths.extend(event.paths),
                    Ok(_) => {}
                    Err(err) => log::warn!("reload on change: {err}"),
                }
            }
            changes
        }
    }

    fn folder_of(file: &Path) -> Option<PathBuf> {
        file.parent().filter(|dir| !dir.as_os_str().is_empty()).map(Path::to_path_buf)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A file renamed over a watched one is seen as a change to it (an atomic save).
        #[test]
        fn an_atomic_replace_is_seen() {
            let dir = std::env::temp_dir().join(format!("margin-reload-watch-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("paper.pdf");
            std::fs::write(&file, b"one").unwrap();
            let mut watch = DirWatch::start(&egui::Context::default()).unwrap();
            watch.follow(&file).unwrap();
            let temp = dir.join(".paper.pdf.tmp");
            std::fs::write(&temp, b"two").unwrap();
            std::fs::rename(&temp, &file).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut seen = false;
            while !seen && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
                seen = watch.changes().touches(&file);
            }
            std::fs::remove_dir_all(&dir).ok();
            assert!(seen, "no change event within 5 s");
        }

        #[test]
        fn only_the_watched_file_counts() {
            let changes = Changes { paths: vec![PathBuf::from("/a/other.pdf")], everything: false };
            assert!(!changes.touches(Path::new("/a/paper.pdf")));
            let changes = Changes { paths: vec![PathBuf::from("/a/paper.pdf")], everything: false };
            assert!(changes.touches(Path::new("/a/paper.pdf")));
            assert!(Changes { paths: Vec::new(), everything: true }.touches(Path::new("/a/paper.pdf")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBOUNCE: f64 = 0.75;

    #[test]
    fn changes_wait_for_the_debounce_after_the_last_one() {
        let mut phase = Phase::Idle;
        phase.changed(10.0, DEBOUNCE);
        phase.changed(10.5, DEBOUNCE);
        assert!(matches!(phase, Phase::Pending { due, attempts: 0 } if (due - 11.25).abs() < 1e-9), "a later change moves the read");
    }

    #[test]
    fn retries_are_bounded() {
        let mut phase = Phase::Idle;
        let mut tries = 0;
        let mut attempts = 0;
        while phase.retry(0.0, attempts) {
            tries += 1;
            let Phase::Pending { attempts: next, due } = phase else { panic!("a retry waits") };
            assert!((due - RETRY_SECS).abs() < 1e-9);
            attempts = next;
        }
        assert_eq!(tries, MAX_ATTEMPTS - 1);
        assert!(matches!(phase, Phase::Idle));
    }

    #[test]
    fn a_question_waits_for_its_answer() {
        let mut phase = Phase::Conflict { bytes: Arc::new(b"new".to_vec()) };
        phase.changed(1.0, DEBOUNCE);
        assert!(matches!(phase, Phase::Conflict { .. }), "a newer change doesn't drop the question");
        let watched = |path: &str, phase| Watched { opened_as: path.to_owned(), path: PathBuf::from(path), phase };
        let mut reload = Reload::default();
        reload.docs.insert(DocId(1), watched("/a.pdf", phase));
        reload.docs.insert(DocId(2), watched("/b.pdf", Phase::Pending { due: 0.0, attempts: 0 }));
        reload.docs.insert(DocId(3), watched("/c.pdf", Phase::Gone));
        assert!(reload.holds(DocId(1)) && reload.holds(DocId(2)), "autosave waits while a change is open");
        assert!(!reload.holds(DocId(3)) && !reload.holds(DocId(4)));
    }

    #[test]
    fn own_saves_are_not_changes_and_edits_are_never_dropped() {
        assert_eq!(outcome(b"saved", b"saved", true), Outcome::Unchanged);
        assert_eq!(outcome(b"theirs", b"saved", false), Outcome::Reload);
        assert_eq!(outcome(b"theirs", b"saved", true), Outcome::Ask);
    }

    /// A Margin app reading `pages` blank pages, with its settings from an empty folder.
    fn reading(pages: usize) -> PdfCraftApp {
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.margin_keys.set_config_dir(std::env::temp_dir().join(format!("margin-reload-config-{}", std::process::id())));
        let bytes = app.session.create_blank(300.0, 400.0, pages).unwrap();
        app.open_bytes("paper.pdf", None, bytes.to_vec()).unwrap();
        app
    }

    #[test]
    fn a_reload_keeps_the_place_zoom_colour_and_history() {
        let mut app = reading(3);
        let id = app.views[0].id;
        let view = &mut app.views[0];
        view.zoom = 1.7;
        view.go_to_page(2);
        view.margin.display.set_option("night").unwrap();
        let (history, colour) = (view.back.clone(), view.margin.display);
        assert!(!history.is_empty());
        let longer = app.session.create_blank(300.0, 400.0, 5).unwrap();
        apply(&mut app, 0, longer).unwrap();
        let view = &app.views[0];
        assert_eq!(view.id, id, "the same document, not a new one");
        assert_eq!(app.session.get(id).unwrap().info.pages.len(), 5);
        assert!((view.zoom - 1.7).abs() < f32::EPSILON);
        assert_eq!((view.current, view.goto.map(|(page, _)| page)), (2, Some(2)));
        assert_eq!((&view.back, view.margin.display), (&history, colour));
        // A shorter version: the last page. A broken one: an error, and the document as it was.
        let shorter = app.session.create_blank(300.0, 400.0, 1).unwrap();
        apply(&mut app, 0, shorter).unwrap();
        assert_eq!(app.views[0].current, 0);
        assert!(apply(&mut app, 0, Arc::new(b"%PDF-1.7 truncated".to_vec())).is_err());
        assert_eq!(app.session.get(id).unwrap().info.pages.len(), 1);
    }

    /// The file a document saved last is no change; a different one under unsaved edits asks.
    #[test]
    fn saved_bytes_tell_own_saves_from_other_writers() {
        let mut app = reading(2);
        let id = app.views[0].id;
        let saved = app.session.get(id).unwrap().saved_bytes();
        assert_eq!(outcome(&saved, &saved, false), Outcome::Unchanged);
        app.session.apply(id, pdfcraft_engine::Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
        let doc = app.session.get(id).unwrap();
        assert!(doc.dirty);
        assert_eq!(*doc.saved_bytes(), *saved, "unsaved edits are not in the saved file");
        assert!(app.has_unsaved_work(0));
    }

    #[test]
    fn the_place_survives_a_shorter_document() {
        let place = Spot { page: 7, y: 0.4, x: Some(0.5) };
        assert_eq!(carried_place(place, 10), place);
        assert_eq!(carried_place(place, 3), Spot { page: 2, y: 0.0, x: Some(0.5) });
        assert_eq!(carried_place(place, 0).page, 0);
    }
}
