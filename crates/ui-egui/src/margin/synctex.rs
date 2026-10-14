//! SyncTeX for LaTeX writers (`pdfcraft_engine::synctex` reads the files).
//!
//! Inverse search: a click with `reader.synctex_click`'s modifier on a page opens the source line
//! under it with `reader.synctex_editor`, a command template run without a shell.
//!
//! Forward search: `margin --forward-search file.tex:line[:column] paper.pdf` opens (or brings
//! forward) the PDF and scrolls to the line's box, outlined briefly. A Margin already running
//! takes the request: it serves an inbox folder in Margin's data folder (`forward-search/`),
//! holding a lock on `running.lock` while it does, so a second launch writes the request there
//! and exits. Only processes of the same user can write that folder; a request can only open a
//! PDF and scroll it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use pdfcraft_engine::DocId;
use pdfcraft_engine::synctex::{self, Synctex};
use serde::{Deserialize, Serialize};

use super::reading::{self, Spot};
use crate::{Flavor, PdfCraftApp};

/// The inbox folder inside Margin's data folder.
pub const INBOX_DIR: &str = "forward-search";
/// Held (locked) by the Margin that serves the inbox.
const LOCK_FILE: &str = "running.lock";
const REQUEST_EXTENSION: &str = "json";
const CLAIMED_EXTENSION: &str = "claimed";
/// A request file larger than this is not one Margin wrote.
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
/// How long a second launch waits for the running Margin to take its request.
const HAND_OFF_WAIT: std::time::Duration = std::time::Duration::from_secs(3);
const HAND_OFF_POLL: std::time::Duration = std::time::Duration::from_millis(25);
/// The forward-search box sits this share of the window below the window's top.
const FORWARD_LEAD: f32 = 0.3;

/// A forward search: show `line` (1-based) of `source` in `pdf`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardRequest {
    pub pdf: PathBuf,
    pub source: PathBuf,
    pub line: u32,
    #[serde(default)]
    pub column: Option<u32>,
}

impl ForwardRequest {
    /// Parse the command line's `file.tex:line[:column]` and the PDF's path; relative paths are
    /// taken from the current folder.
    pub fn parse(location: &str, pdf: &str) -> Result<Self, String> {
        let bad = || format!("--forward-search wants FILE.tex:LINE[:COLUMN], not `{location}`");
        let (rest, last) = location.rsplit_once(':').ok_or_else(bad)?;
        let last: u32 = last.trim().parse().map_err(|_| bad())?;
        // FILE:LINE:COLUMN, or FILE:LINE (a Windows drive letter's colon is part of FILE).
        let (source, line, column) = match rest.rsplit_once(':').map(|(file, line)| (file, line.trim().parse::<u32>())) {
            Some((file, Ok(line))) if !file.is_empty() => (file, line, Some(last)),
            _ => (rest, last, None),
        };
        if source.is_empty() || line == 0 {
            return Err(bad());
        }
        let absolute = |path: &str| std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
        Ok(Self { pdf: absolute(pdf), source: absolute(source), line, column: column.filter(|c| *c > 0) })
    }
}

/// What a second launch did with its forward search.
#[derive(Debug, PartialEq, Eq)]
pub enum HandOff {
    /// The running Margin took it.
    Sent,
    /// No Margin is serving the inbox (or it didn't answer): open the PDF here.
    NotRunning,
}

/// Give `request` to the Margin running for this user, if one serves the inbox in `data_dir`.
pub fn hand_off(data_dir: &Path, request: &ForwardRequest) -> Result<HandOff, String> {
    let inbox = data_dir.join(INBOX_DIR);
    if !someone_serves(&inbox) {
        return Ok(HandOff::NotRunning);
    }
    let text = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let name = format!("{}-{stamp}", std::process::id());
    let partial = inbox.join(format!("{name}.part"));
    let request_path = inbox.join(format!("{name}.{REQUEST_EXTENSION}"));
    std::fs::write(&partial, text).map_err(|e| format!("{}: {e}", partial.display()))?;
    std::fs::rename(&partial, &request_path).map_err(|e| format!("{}: {e}", request_path.display()))?;
    let deadline = std::time::Instant::now() + HAND_OFF_WAIT;
    while std::time::Instant::now() < deadline {
        if !request_path.exists() {
            return Ok(HandOff::Sent);
        }
        std::thread::sleep(HAND_OFF_POLL);
    }
    // Not taken: withdraw it, unless it was taken just now.
    match std::fs::remove_file(&request_path) {
        Ok(()) => Ok(HandOff::NotRunning),
        Err(_) => Ok(HandOff::Sent),
    }
}

/// [`hand_off`] to the Margin using the default data folder (`$MARGIN_DATA_DIR`, else the OS's).
/// Without a data folder there is nobody to hand off to.
pub fn hand_off_to_running(request: &ForwardRequest) -> Result<HandOff, String> {
    match margin_config::store::data_dir() {
        Some(dir) => hand_off(&dir, request),
        None => Ok(HandOff::NotRunning),
    }
}

/// Serve forward-search requests in `app`'s data folder (the desktop app, once at start).
pub fn serve(app: &mut PdfCraftApp, ctx: &egui::Context) -> Result<(), String> {
    let dir = app.margin_keys.data_dir().ok_or("this system has no data folder")?;
    app.margin_synctex.serve_forward_search(&dir, ctx)
}

/// Whether another process holds the inbox lock.
fn someone_serves(inbox: &Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new().read(true).write(true).open(inbox.join(LOCK_FILE)) else { return false };
    match file.try_lock() {
        Ok(()) => {
            if let Err(err) = file.unlock() {
                log::debug!("forward search: unlocking the probe: {err}");
            }
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(err)) => {
            log::debug!("forward search: can't probe the inbox lock: {err}");
            false
        }
    }
}

/// Margin's SyncTeX state (`app.margin_synctex`).
#[derive(Default)]
pub struct SynctexState {
    /// The SyncTeX file read for each PDF, by the PDF's path.
    maps: HashMap<PathBuf, Map>,
    /// Searches waiting for their PDF to open or its SyncTeX file to be read.
    queued: Vec<Query>,
    inbox: Option<Inbox>,
}

/// The inbox served by this Margin: the held lock, and a watch that wakes the app.
struct Inbox {
    dir: PathBuf,
    _lock: std::fs::File,
    #[cfg(not(target_arch = "wasm32"))]
    _watch: Option<notify::RecommendedWatcher>,
    #[cfg(not(target_arch = "wasm32"))]
    events: Option<Receiver<notify::Result<notify::Event>>>,
    /// Look on the first frame for requests written before the app started.
    scanned: bool,
}

struct Map {
    /// The SyncTeX file's size and time when read: a rebuild reads it again.
    stamp: Option<(u64, Option<std::time::SystemTime>)>,
    state: MapState,
}

enum MapState {
    Loading(Receiver<Result<Option<Synctex>, String>>),
    Ready(Arc<Synctex>),
    Missing,
    Failed(String),
}

#[derive(Clone, Debug)]
enum Query {
    Inverse {
        doc: DocId,
        page: usize,
        x: f32,
        y: f32,
    },
    /// `opened`: the PDF was asked to open for this search (once; a failed open ends it).
    Forward {
        pdf: PathBuf,
        source: PathBuf,
        line: u32,
        column: Option<u32>,
        opened: bool,
    },
}

impl Query {
    fn pdf(&self, app: &PdfCraftApp) -> Option<PathBuf> {
        match self {
            // Absolute, so relative input paths in the SyncTeX file resolve to real files.
            Self::Inverse { doc, .. } => {
                app.session.get(*doc)?.path.as_deref().map(|path| std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path)))
            }
            Self::Forward { pdf, .. } => Some(pdf.clone()),
        }
    }
}

impl SynctexState {
    /// Serve forward-search requests from `<data_dir>/forward-search` (the desktop app calls this
    /// once at start). Does nothing when another Margin serves it already.
    pub fn serve_forward_search(&mut self, data_dir: &Path, ctx: &egui::Context) -> Result<(), String> {
        let dir = data_dir.join(INBOX_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let lock_path = dir.join(LOCK_FILE);
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| format!("{}: {e}", lock_path.display()))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err("another Margin serves forward search".into()),
            Err(std::fs::TryLockError::Error(err)) => return Err(format!("{}: {err}", lock_path.display())),
        }
        #[cfg(not(target_arch = "wasm32"))]
        let (watch, events) = match watch_dir(&dir, ctx) {
            Ok((watch, events)) => (Some(watch), Some(events)),
            Err(err) => {
                log::warn!("forward search: can't watch {}: {err}", dir.display());
                (None, None)
            }
        };
        #[cfg(target_arch = "wasm32")]
        let _ = ctx;
        self.inbox = Some(Inbox {
            dir,
            _lock: lock,
            #[cfg(not(target_arch = "wasm32"))]
            _watch: watch,
            #[cfg(not(target_arch = "wasm32"))]
            events,
            scanned: false,
        });
        Ok(())
    }

    /// Search forward from the command line of this launch.
    pub fn forward(&mut self, request: ForwardRequest) {
        self.queued.push(Query::Forward { pdf: request.pdf, source: request.source, line: request.line, column: request.column, opened: false });
    }
}

#[cfg(not(target_arch = "wasm32"))]
type InboxWatch = (notify::RecommendedWatcher, Receiver<notify::Result<notify::Event>>);

#[cfg(not(target_arch = "wasm32"))]
fn watch_dir(dir: &Path, ctx: &egui::Context) -> Result<InboxWatch, String> {
    use notify::Watcher as _;
    let (sender, events) = std::sync::mpsc::channel();
    let ctx = ctx.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if sender.send(event).is_err() {
            log::debug!("forward search: the app stopped listening");
        }
        ctx.request_repaint();
    })
    .map_err(|e| e.to_string())?;
    watcher.watch(dir, notify::RecursiveMode::NonRecursive).map_err(|e| e.to_string())?;
    Ok((watcher, events))
}

/// Every frame (Margin only): take inbox requests, finish SyncTeX reads, run waiting searches.
pub(crate) fn tick(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if app.flavor != Flavor::Margin {
        return;
    }
    take_requests(app, ctx);
    if app.margin_synctex.queued.is_empty() {
        return;
    }
    let queued = std::mem::take(&mut app.margin_synctex.queued);
    for query in queued {
        run_query(app, ctx, query);
    }
}

/// Requests a second launch left in the inbox, each taken once (renamed before it is read, so
/// two Margins never both take it).
fn take_requests(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let Some(inbox) = app.margin_synctex.inbox.as_mut() else { return };
    #[cfg(not(target_arch = "wasm32"))]
    let changed = inbox.events.as_ref().is_some_and(|events| events.try_iter().count() > 0);
    #[cfg(target_arch = "wasm32")]
    let changed = false;
    if inbox.scanned && !changed {
        return;
    }
    inbox.scanned = true;
    let Ok(entries) = std::fs::read_dir(&inbox.dir) else { return };
    let mut requests = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some(REQUEST_EXTENSION) {
            continue;
        }
        let claimed = path.with_extension(CLAIMED_EXTENSION);
        if std::fs::rename(&path, &claimed).is_err() {
            continue;
        }
        let request = read_request(&claimed);
        if let Err(err) = std::fs::remove_file(&claimed) {
            log::warn!("forward search: {}: {err}", claimed.display());
        }
        match request {
            Ok(request) => requests.push(request),
            Err(err) => log::warn!("forward search: ignoring {}: {err}", path.display()),
        }
    }
    if !requests.is_empty() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }
    for request in requests {
        app.margin_synctex.forward(request);
    }
}

fn read_request(path: &Path) -> Result<ForwardRequest, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_REQUEST_BYTES {
        return Err("too large".into());
    }
    let text = std::fs::read(path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&text).map_err(|e| e.to_string())
}

/// Run `query` once its PDF's SyncTeX file is read; until then it waits in the queue.
fn run_query(app: &mut PdfCraftApp, ctx: &egui::Context, mut query: Query) {
    if let Query::Forward { pdf, opened, .. } = &mut query
        && find_view(app, pdf).is_none()
    {
        // Asked once: an open that failed or waits for a password ends the search.
        if *opened && !opening(app, pdf) {
            return;
        }
        if !*opened {
            if !opening(app, pdf) {
                open_for_forward(app, pdf);
            }
            *opened = true;
        }
        if find_view(app, pdf).is_none() {
            // Read and prepared in the background: search once its tab has opened.
            if opening(app, pdf) {
                app.margin_synctex.queued.push(query);
            }
            return;
        }
    }
    let Some(pdf) = query.pdf(app) else { return };
    match map_for(app, ctx, &pdf) {
        Lookup::Waiting => app.margin_synctex.queued.push(query),
        Lookup::Ready(map) => answer(app, &map, query),
        Lookup::Missing => app.notify(format!(
            "No SyncTeX file next to “{}”. Build with `-synctex=1` (latexmk and most editors do).",
            pdf.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
        )),
        Lookup::Failed(err) => app.notify(format!("Couldn't read the SyncTeX file: {err}")),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn open_for_forward(app: &mut PdfCraftApp, pdf: &Path) {
    app.open_path(&pdf.to_string_lossy());
}

#[cfg(target_arch = "wasm32")]
fn open_for_forward(app: &mut PdfCraftApp, pdf: &Path) {
    app.notify(format!("Can't open {} here", pdf.display()));
}

/// Whether `pdf` is being read and prepared to open (`PdfCraftApp::open_path` once the app runs).
#[cfg(not(target_arch = "wasm32"))]
fn opening(app: &PdfCraftApp, pdf: &Path) -> bool {
    app.pending_opens.iter().any(|open| open.path.as_deref().is_some_and(|path| Path::new(path) == pdf))
}

#[cfg(target_arch = "wasm32")]
fn opening(_: &PdfCraftApp, _: &Path) -> bool {
    false
}

/// The open view showing the file `pdf` (the same file under any spelling of its path).
fn find_view(app: &PdfCraftApp, pdf: &Path) -> Option<usize> {
    let real = std::fs::canonicalize(pdf).ok();
    app.views.iter().position(|view| {
        app.session.get(view.id).and_then(|doc| doc.path.as_deref()).is_some_and(|path| {
            let path = Path::new(path);
            path == pdf || (real.is_some() && std::fs::canonicalize(path).ok() == real)
        })
    })
}

enum Lookup {
    Waiting,
    Ready(Arc<Synctex>),
    Missing,
    Failed(String),
}

/// `pdf`'s SyncTeX map: read (again, after a rebuild) in the background.
fn map_for(app: &mut PdfCraftApp, ctx: &egui::Context, pdf: &Path) -> Lookup {
    let stamp = synctex::file_for(pdf).and_then(|file| std::fs::metadata(file).ok()).map(|meta| (meta.len(), meta.modified().ok()));
    let maps = &mut app.margin_synctex.maps;
    let fresh = maps.get(pdf).is_some_and(|map| map.stamp == stamp || matches!(map.state, MapState::Loading(_)));
    if !fresh {
        maps.insert(pdf.to_path_buf(), Map { stamp, state: MapState::Loading(load_in_background(ctx, pdf)) });
    }
    let Some(map) = maps.get_mut(pdf) else { return Lookup::Waiting };
    if let MapState::Loading(result) = &map.state {
        map.state = match result.try_recv() {
            Ok(Ok(Some(read))) => MapState::Ready(Arc::new(read)),
            Ok(Ok(None)) => MapState::Missing,
            Ok(Err(err)) => MapState::Failed(err),
            Err(std::sync::mpsc::TryRecvError::Empty) => return Lookup::Waiting,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => MapState::Failed("reading it stopped".into()),
        };
    }
    match &map.state {
        MapState::Loading(_) => Lookup::Waiting,
        MapState::Ready(read) => Lookup::Ready(read.clone()),
        MapState::Missing => Lookup::Missing,
        MapState::Failed(err) => Lookup::Failed(err.clone()),
    }
}

fn load_in_background(ctx: &egui::Context, pdf: &Path) -> Receiver<Result<Option<Synctex>, String>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let (pdf, ctx) = (pdf.to_path_buf(), ctx.clone());
    let load = move || {
        if sender.send(synctex::load_for(&pdf)).is_err() {
            log::debug!("synctex: nobody waits for {}", pdf.display());
        }
        ctx.request_repaint();
    };
    #[cfg(not(target_arch = "wasm32"))]
    if let Err(err) = std::thread::Builder::new().name("margin-synctex".into()).spawn(load) {
        log::warn!("synctex: no reader thread: {err}");
    }
    #[cfg(target_arch = "wasm32")]
    load();
    receiver
}

fn answer(app: &mut PdfCraftApp, map: &Synctex, query: Query) {
    match query {
        Query::Inverse { page, x, y, .. } => match map.inverse(page, x, y) {
            Some(place) => open_in_editor(app, &place),
            None => app.notify("SyncTeX has no source line for that spot"),
        },
        Query::Forward { pdf, source, line, column, .. } => {
            let Some(index) = find_view(app, &pdf) else { return };
            match map.forward(&source, line, column) {
                Some(hit) => show_hit(app, index, &hit),
                None => app.notify(format!("SyncTeX has no box for {}:{line}", source.display())),
            }
        }
    }
}

/// Bring view `index` forward at the forward search's boxes and outline the first one.
fn show_hit(app: &mut PdfCraftApp, index: usize, hit: &synctex::ForwardHit) {
    super::tabs::show(app, index);
    let Some(view) = app.views.get(index) else { return };
    let Some(page) = app.session.get(view.id).and_then(|doc| doc.info.pages.get(hit.page)).cloned() else { return };
    let Some(view) = app.views.get_mut(index) else { return };
    let Some(&[x0, y0, x1, y1]) = hit.boxes.first() else { return };
    let [ux0, uy0] = synctex::page_to_user(&page, x0, y0);
    let [ux1, uy1] = synctex::page_to_user(&page, x1, y1);
    let [_, view_top] = page.user_to_view(ux0, uy0.max(uy1));
    let share = (view_top / page.height.max(f32::EPSILON)
        - FORWARD_LEAD * view.viewport_h / page.height.max(f32::EPSILON) / view.zoom.max(f32::EPSILON))
    .clamp(0.0, 1.0);
    // Through `go_to_page`, so Back returns to where the reader was.
    view.go_to_page(hit.page);
    reading::go(view, Spot { page: hit.page, y: share, x: None });
    view.flash = Some((hit.page, [ux0.min(ux1), uy0.min(uy1), ux0.max(ux1), uy0.max(uy1)], 0.0));
}

/// Inverse search, if `reader.synctex_click` is held while the page is clicked: queue the
/// lookup of the clicked spot. Call after the page view is drawn.
pub(crate) fn inverse_click(app: &mut PdfCraftApp, ctx: &egui::Context, index: usize) {
    let click = app.margin_keys.settings().reader.synctex_click;
    // The modifiers held when the button went up, as the event carries them.
    let clicked = ctx.input(|input| {
        input.pointer.primary_clicked()
            && input.events.iter().any(|event| {
                matches!(event, egui::Event::PointerButton { button: egui::PointerButton::Primary, pressed: false, modifiers, .. } if click_held(click, *modifiers))
            })
    });
    let Some(pointer) = clicked.then(|| ctx.input(|input| input.pointer.interact_pos())).flatten() else { return };
    let Some(view) = app.views.get(index) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let Some(pdf) = doc.path.as_deref() else { return };
    if synctex::file_for(Path::new(pdf)).is_none() {
        return;
    }
    let Some(page) = view.visible_page_rects().into_iter().find(|(_, rect)| rect.contains(pointer)).map(|(page, _)| page) else { return };
    let (Some(xf), Some(info)) = (view.page_xform(page), doc.info.pages.get(page)) else { return };
    let (vx, vy) = xf.screen_to_view(pointer);
    let [ux, uy] = info.view_to_user(vx, vy);
    let [x, y] = synctex::user_to_page(info, ux, uy);
    app.margin_synctex.queued.push(Query::Inverse { doc: view.id, page, x, y });
}

/// Whether a click with `modifiers` on `pdf`'s pages is SyncTeX's: `reader.synctex_click` is held
/// and the PDF has a SyncTeX file. The canvas then doesn't also follow a link under the pointer.
pub(crate) fn claims_click(click: margin_config::SynctexClick, modifiers: egui::Modifiers, pdf: Option<&str>) -> bool {
    click_held(click, modifiers) && pdf.is_some_and(|pdf| synctex::file_for(Path::new(pdf)).is_some())
}

fn click_held(click: margin_config::SynctexClick, modifiers: egui::Modifiers) -> bool {
    use margin_config::SynctexClick as C;
    match click {
        C::SecondaryClick => modifiers.command && !modifiers.shift && !modifiers.alt,
        C::SecondaryShiftClick => modifiers.command && modifiers.shift && !modifiers.alt,
        C::AltClick => modifiers.alt && !modifiers.command && !modifiers.shift,
    }
}

/// Run `reader.synctex_editor` for `place`.
fn open_in_editor(app: &mut PdfCraftApp, place: &synctex::SourcePlace) {
    let template = app.margin_keys.settings().reader.synctex_editor.clone();
    if template.trim().is_empty() {
        app.notify(format!("{}:{} — set reader.synctex_editor in settings.jsonc to open it in your editor", place.file.display(), place.line));
        return;
    }
    let args = match editor_command(&template, place) {
        Ok(args) => args,
        Err(err) => {
            app.notify(format!("reader.synctex_editor: {err}"));
            return;
        }
    };
    if let Err(err) = launch(&args) {
        app.notify(format!(
            "Couldn't run the editor `{}`: {err}. Give its full path in reader.synctex_editor.",
            args.first().map_or("", String::as_str)
        ));
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn launch(args: &[String]) -> Result<(), String> {
    let (program, rest) = args.split_first().ok_or("the command is empty")?;
    let mut child = std::process::Command::new(program)
        .args(rest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    // Reap the editor's launcher when it exits, so it never lingers as a zombie.
    let reaper = std::thread::Builder::new().name("margin-synctex-editor".into()).spawn(move || {
        if let Err(err) = child.wait() {
            log::debug!("synctex: waiting for the editor: {err}");
        }
    });
    if let Err(err) = reaper {
        log::debug!("synctex: no thread to wait for the editor: {err}");
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn launch(_args: &[String]) -> Result<(), String> {
    Err("no programs run on the web".into())
}

/// The editor command for `place`: `template` split into arguments (whitespace separates them;
/// single or double quotes keep spaces), then `{file}`, `{line}` and `{column}` filled in inside
/// each argument. A file name with spaces stays one argument; no shell sees it.
pub fn editor_command(template: &str, place: &synctex::SourcePlace) -> Result<Vec<String>, String> {
    let file = place.file.to_string_lossy();
    let line = place.line.to_string();
    let column = place.column.unwrap_or(1).to_string();
    let args: Vec<String> =
        split_args(template)?.into_iter().map(|arg| arg.replace("{file}", &file).replace("{line}", &line).replace("{column}", &column)).collect();
    if args.is_empty() {
        return Err("the command is empty".into());
    }
    Ok(args)
}

fn split_args(template: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    let mut quote: Option<char> = None;
    for c in template.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_arg = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            (None, c) => {
                current.push(c);
                in_arg = true;
            }
        }
    }
    if quote.is_some() {
        return Err("a quote is not closed".into());
    }
    if in_arg {
        args.push(current);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(file: &str) -> synctex::SourcePlace {
        synctex::SourcePlace { file: PathBuf::from(file), line: 42, column: None }
    }

    #[test]
    fn editor_templates_split_without_a_shell() {
        assert_eq!(editor_command("code -g {file}:{line}", &place("/a b/main.tex")).unwrap(), ["code", "-g", "/a b/main.tex:42"]);
        assert_eq!(editor_command("zed {file}:{line}:{column}", &place("/x.tex")).unwrap(), ["zed", "/x.tex:42:1"]);
        let nvim = editor_command("nvim --server /tmp/nvim.sock --remote-send '<C-\\><C-N>:e {file}<CR>:{line}<CR>'", &place("/x.tex")).unwrap();
        assert_eq!(nvim, ["nvim", "--server", "/tmp/nvim.sock", "--remote-send", "<C-\\><C-N>:e /x.tex<CR>:42<CR>"]);
        assert_eq!(
            editor_command("\"/Applications/My Editor.app/bin/ed\" ''", &place("/x.tex")).unwrap(),
            ["/Applications/My Editor.app/bin/ed", ""]
        );
        // A file name that looks like a shell command stays one inert argument.
        assert_eq!(editor_command("ed {file}", &place("/x; rm -rf ~.tex")).unwrap(), ["ed", "/x; rm -rf ~.tex"]);
        assert!(editor_command("  ", &place("/x.tex")).is_err());
        assert!(editor_command("code 'unclosed", &place("/x.tex")).is_err());
    }

    #[test]
    fn forward_search_arguments_parse() {
        let r = ForwardRequest::parse("/p/main.tex:12", "/p/main.pdf").unwrap();
        assert_eq!((r.source.as_path(), r.line, r.column), (Path::new("/p/main.tex"), 12, None));
        let r = ForwardRequest::parse("/p/main.tex:12:5", "/p/main.pdf").unwrap();
        assert_eq!((r.line, r.column), (12, Some(5)));
        assert!(ForwardRequest::parse("/p/main.tex", "/p/main.pdf").is_err());
        assert!(ForwardRequest::parse("/p/main.tex:0", "/p/main.pdf").is_err());
        assert!(ForwardRequest::parse(":3", "/p/main.pdf").is_err());
        assert!(ForwardRequest::parse("/p/main.tex:x", "/p/main.pdf").is_err());
    }

    /// A second launch hands its request to the Margin serving the inbox, and finds nobody once
    /// that one is gone.
    #[test]
    fn requests_reach_the_serving_margin() {
        let dir = std::env::temp_dir().join(format!("margin-forward-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let request = ForwardRequest { pdf: dir.join("a.pdf"), source: dir.join("a.tex"), line: 3, column: None };
        assert_eq!(hand_off(&dir, &request).unwrap(), HandOff::NotRunning, "nobody serves yet");
        let mut serving = SynctexState::default();
        serving.serve_forward_search(&dir, &egui::Context::default()).unwrap();
        assert!(SynctexState::default().serve_forward_search(&dir, &egui::Context::default()).is_err(), "one server at a time");
        let inbox = dir.join(INBOX_DIR);
        // The serving app takes the request on a frame; here a thread plays that part.
        let taker = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                for entry in std::fs::read_dir(&inbox).unwrap().flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some(REQUEST_EXTENSION) {
                        let claimed = path.with_extension(CLAIMED_EXTENSION);
                        std::fs::rename(&path, &claimed).unwrap();
                        let got = read_request(&claimed).unwrap();
                        std::fs::remove_file(&claimed).unwrap();
                        return Some(got);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            None
        });
        assert_eq!(hand_off(&dir, &request).unwrap(), HandOff::Sent);
        assert_eq!(taker.join().unwrap(), Some(request.clone()));
        drop(serving);
        assert_eq!(hand_off(&dir, &request).unwrap(), HandOff::NotRunning, "the lock goes with the app");
        std::fs::remove_dir_all(&dir).ok();
    }
}
