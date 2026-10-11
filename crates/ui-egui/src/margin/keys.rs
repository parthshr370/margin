//! Margin's keyboard (Margin flavor only): `<config>/settings.jsonc` and `<config>/keymap.jsonc`
//! loaded through `margin-config`, kept live by [`SettingsWatch`], and every keystroke resolved
//! against the compiled keymap for the focused context stack.
//!
//! Hooks (listed in the plan's RESKIN-HOOKS.md): `registry_shortcuts` calls [`dispatch`] first and
//! keeps PdfCraft's static shortcut only where [`MarginKeys::falls_back`] says the keymap leaves
//! it alone; `PdfCraftApp::shortcuts` calls [`view_keys`] instead of `tool_keys` and
//! `canvas::shortcuts`, whose keys the built-in keymap binds to the ids [`run`] handles.
//!
//! Context stack, outermost first: `App os=…`, then one of `Dialog` (a modal, the keyboard sheet),
//! `Palette`, or `Reader format=pdf layout=…` with `AnnotateMode tool=…` while the annotate rail
//! shows or a comment tool is on, then `Find > TextInput` in the find field, `TextInput` in any other text field, or `Page`
//! with `Selection` while text is selected and `Annotation` while a comment is selected.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use egui::{Event, Key, Modifiers};
use margin_config::resolve::{self, Outcome};
use margin_config::{CompileOptions, Compiled, CompiledKeymap, ContextNode, KeymapBase, KeymapSource, Os, Problem, Registry, Settings, Severity};
use pdfcraft_engine::commands::{self, CommandSpec, Shortcut};
use serde_json::Value;

use super::settings_watch::SettingsWatch;
use super::{reading, scroll_step};
use crate::canvas::{DocView, Fit, PageLayout};
use crate::{PdfCraftApp, QuickTool};

/// Wait after the last change event before reloading: editors save a file in several steps.
const RELOAD_DEBOUNCE_S: f64 = 0.15;

/// Zoom limits of `view.zoom.set`, as a factor: the page view's own (`DocView::zoom_at`), which
/// `view.zoom` and `view.zoom_steps` share.
const MIN_ZOOM: f64 = 0.08;
const MAX_ZOOM: f64 = 64.0;

/// The settings before the files are read (and on a build without a config folder).
static DEFAULT_SETTINGS: LazyLock<Settings> = LazyLock::new(Settings::default);

/// The keyboard state of the Margin flavor. Empty (and unused) in the PdfCraft flavor.
#[derive(Default)]
pub struct MarginKeys {
    loaded: Option<Loaded>,
    /// Keys of a chord in progress.
    pending: Vec<String>,
    /// When the chord's last key came (`egui::InputState::time`).
    pending_since: f64,
    /// A change was seen; reload at this time.
    reload_at: Option<f64>,
    /// The keyboard sheet, while open.
    pub(crate) sheet: Option<super::keysheet::Sheet>,
    /// `<config>` chosen by the embedder instead of `margin_config::config_dir()`.
    config_override: Option<PathBuf>,
    /// `<data>` (central sidecars) chosen by the embedder instead of `margin_config::store::data_dir()`.
    data_override: Option<PathBuf>,
    /// The `app.key_context` window is open.
    pub(crate) key_context_open: bool,
    /// The last keystroke resolved, for the key context window.
    pub(crate) last: Option<LastKey>,
}

/// One keystroke and what the keymap made of it.
pub(crate) struct LastKey {
    /// The chord typed so far, this key last.
    pub(crate) typed: Vec<String>,
    /// The context stack it was resolved in.
    pub(crate) stack: Vec<ContextNode>,
    /// The result.
    pub(crate) outcome: Outcome,
}

/// Everything read from `<config>`.
struct Loaded {
    dir: Option<PathBuf>,
    registry: Option<Registry>,
    settings: Settings,
    compiled: Option<Compiled>,
    /// Problems of the last load, for the key context window.
    problems: Vec<Problem>,
    /// Ids some binding runs.
    bound_ids: HashSet<String>,
    /// First keystrokes some binding or unbind uses.
    bound_strokes: HashSet<String>,
    watch: Option<SettingsWatch>,
}

impl MarginKeys {
    /// The effective settings: `settings.jsonc` over the defaults.
    pub fn settings(&self) -> &Settings {
        self.loaded.as_ref().map_or(&DEFAULT_SETTINGS, |loaded| &loaded.settings)
    }

    /// The compiled keymap, once loaded.
    pub fn keymap(&self) -> Option<&CompiledKeymap> {
        self.loaded.as_ref().and_then(|loaded| loaded.compiled.as_ref()).map(|compiled| &compiled.keymap)
    }

    /// Every bindable id with its title.
    pub fn registry(&self) -> Option<&Registry> {
        self.loaded.as_ref().and_then(|loaded| loaded.registry.as_ref())
    }

    /// `<config>`, if this platform has one.
    pub fn config_dir(&self) -> Option<&Path> {
        self.loaded.as_ref().and_then(|loaded| loaded.dir.as_deref())
    }

    /// Problems of the last load.
    pub(crate) fn problems(&self) -> &[Problem] {
        self.loaded.as_ref().map_or(&[], |loaded| &loaded.problems)
    }

    /// Keys of a chord in progress.
    pub fn pending(&self) -> &[String] {
        &self.pending
    }

    /// Reads settings and keymap from `dir` instead of `~/.config/margin` (tests, portable
    /// setups). Takes effect on the next frame.
    pub fn set_config_dir(&mut self, dir: PathBuf) {
        self.config_override = Some(dir);
        self.loaded = None;
    }

    /// Keeps central sidecars in `dir` instead of `~/.local/share/margin` (tests, portable
    /// setups).
    pub fn set_data_dir(&mut self, dir: PathBuf) {
        self.data_override = Some(dir);
    }

    /// `<data>`, where sidecars go when a document's folder can't be written.
    pub(crate) fn data_dir(&self) -> Option<PathBuf> {
        self.data_override.clone().or_else(margin_config::store::data_dir)
    }

    /// `<config>` as the next load will read it: the embedder's choice, else the OS's.
    pub(crate) fn settings_dir(&self) -> Option<PathBuf> {
        self.config_override.clone().or_else(margin_config::config_dir)
    }

    /// The settings a document opens with. A file given on the command line opens before the
    /// first frame loads the files: then `settings.jsonc` is read now (the watch starts later).
    pub(crate) fn settings_for_open(&self) -> Settings {
        match &self.loaded {
            Some(loaded) => loaded.settings.clone(),
            None => self.settings_dir().map(|dir| Settings::load(&dir).settings).unwrap_or_default(),
        }
    }

    /// Whether the keyboard sheet is open.
    pub fn sheet_open(&self) -> bool {
        self.sheet.is_some()
    }

    /// Opens or closes the keyboard sheet (`help.shortcuts` in the Margin flavor).
    pub(crate) fn toggle_sheet(&mut self) {
        self.sheet = match self.sheet {
            Some(_) => None,
            None => Some(super::keysheet::Sheet::default()),
        };
    }

    /// Whether PdfCraft's static shortcut of `spec` still applies: the keymap binds neither the id
    /// nor the keystroke (an unbind counts). Before the keymap loads, every static shortcut applies.
    pub(crate) fn falls_back(&self, spec: &CommandSpec) -> bool {
        let Some(loaded) = self.loaded.as_ref().filter(|loaded| loaded.compiled.is_some()) else { return true };
        let Some(shortcut) = &spec.shortcut else { return true };
        if loaded.bound_ids.contains(spec.id) {
            return false;
        }
        margin_config::canonical(&zed_keystroke(shortcut), Os::current()).map_or(true, |stroke| !loaded.bound_strokes.contains(&stroke))
    }
}

/// PdfCraft's `Shortcut` in Zed syntax: `command` is `secondary-`, `mac_ctrl` adds `ctrl-`.
pub(crate) fn zed_keystroke(shortcut: &Shortcut) -> String {
    let mut text = String::new();
    if shortcut.mac_ctrl {
        text.push_str("ctrl-");
    }
    if shortcut.command {
        text.push_str("secondary-");
    }
    if shortcut.shift {
        text.push_str("shift-");
    }
    text.push_str(&shortcut.key.to_lowercase());
    text
}

impl Loaded {
    /// Reads `<config>` (defaults without one) and compiles the keymap. Returns what was wrong.
    fn read(dir: Option<PathBuf>, registry: Option<Registry>, watch: Option<SettingsWatch>) -> (Self, Vec<Problem>) {
        let mut loaded = Self {
            dir,
            registry,
            settings: Settings::default(),
            compiled: None,
            problems: Vec::new(),
            bound_ids: HashSet::new(),
            bound_strokes: HashSet::new(),
            watch,
        };
        let problems = loaded.reload();
        (loaded, problems)
    }

    fn reload(&mut self) -> Vec<Problem> {
        let mut problems = Vec::new();
        let mut user_keymap: Option<(String, String)> = None;
        if let Some(dir) = &self.dir {
            let loaded = Settings::load(dir);
            self.settings = loaded.settings;
            problems = loaded.problems;
            let path = dir.join(margin_config::KEYMAP_FILE);
            let shown = path.display().to_string();
            match margin_config::read_capped(&path, margin_config::MAX_SETTINGS_BYTES) {
                Ok(Some(text)) => user_keymap = Some((shown, text)),
                Ok(None) => {}
                Err(message) => problems.push(problem(&shown, "", format!("{message}; file ignored"), Severity::Error)),
            }
        }
        let keys = &self.settings.keys;
        if matches!(keys.base, KeymapBase::Okular | KeymapBase::Sioyek) {
            let file = self.dir.as_ref().map(|dir| dir.join(margin_config::SETTINGS_FILE).display().to_string()).unwrap_or_default();
            problems.push(problem(&file, "keys.base", "this keymap is not shipped yet; using the built-in one".to_owned(), Severity::Info));
        }
        self.compiled = self.registry.as_ref().map(|registry| {
            let mut sources = Vec::with_capacity(2);
            if keys.base != KeymapBase::None {
                sources.push(KeymapSource { file: margin_config::MARGIN_KEYMAP_FILE, text: margin_config::MARGIN_KEYMAP });
            }
            if let Some((file, text)) = &user_keymap {
                sources.push(KeymapSource { file, text });
            }
            let options = CompileOptions { os: Os::current(), chord_timeout_ms: keys.chord_timeout_ms, annotate_letters: keys.annotate_letters };
            margin_config::compile(registry, &sources, &options)
        });
        if let Some(compiled) = &self.compiled {
            problems.extend(compiled.problems.iter().cloned());
            let bindings = &compiled.keymap.bindings;
            self.bound_ids = bindings.iter().filter_map(|binding| binding.command.clone()).collect();
            self.bound_strokes = bindings.iter().filter_map(|binding| binding.keys.first().cloned()).collect();
        }
        self.problems = problems.clone();
        problems
    }
}

fn problem(file: &str, key: &str, message: String, severity: Severity) -> Problem {
    Problem { file: file.to_owned(), line: 1, col: 1, key: key.to_owned(), message, severity }
}

/// Loads the files on the first Margin frame: writes the first-run files and the schemas into
/// `<config>`, reads them, and starts the watch.
fn ensure_loaded(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if app.margin_keys.loaded.is_some() {
        return;
    }
    let mut problems = Vec::new();
    let registry = match Registry::load() {
        Ok(registry) => Some(registry),
        Err(err) => {
            // Only a broken build can get here (margin-config's tests load the same data).
            problems.push(problem("margin-config", "", format!("{err}; using PdfCraft's shortcuts"), Severity::Error));
            None
        }
    };
    let dir = app.margin_keys.config_override.clone().or_else(margin_config::config_dir);
    if let Some(dir) = &dir {
        problems.extend(write_config_files(dir, registry.as_ref()));
    }
    let watch = dir.as_deref().and_then(|dir| match SettingsWatch::start(dir, ctx) {
        Ok(watch) => Some(watch),
        Err(err) => {
            let message = format!("can't watch the folder ({err}); restart Margin to apply changes");
            problems.push(problem(&dir.display().to_string(), "", message, Severity::Warning));
            None
        }
    });
    let (loaded, load_problems) = Loaded::read(dir, registry, watch);
    problems.extend(load_problems);
    app.margin_keys.loaded = Some(loaded);
    app.notes.apply_config(&app.margin_keys.settings().notes);
    if let Err(err) = app.migrate_legacy_notes() {
        log::warn!("could not migrate legacy notes settings: {err}");
    }
    report(app, &problems, false);
}

/// First run: the commented `settings.jsonc` and `keymap.jsonc`; every run: the schemas the
/// files point editors at (rewritten only when they changed).
fn write_config_files(dir: &Path, registry: Option<&Registry>) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut fail = |path: &Path, err: std::io::Error| {
        problems.push(problem(&path.display().to_string(), "", format!("can't write the file ({err})"), Severity::Warning));
    };
    let schemas = dir.join("schemas");
    if let Err(err) = std::fs::create_dir_all(&schemas) {
        fail(&schemas, err);
        return problems;
    }
    let mut first_run = vec![(dir.join(margin_config::SETTINGS_FILE), margin_config::schema::first_run_settings_jsonc())];
    if let Some(registry) = registry {
        first_run.push((dir.join(margin_config::KEYMAP_FILE), margin_config::schema::first_run_keymap_jsonc(registry)));
    }
    for (path, text) in first_run {
        let created = std::fs::OpenOptions::new().write(true).create_new(true).open(&path);
        match created.and_then(|mut file| std::io::Write::write_all(&mut file, text.as_bytes())) {
            Ok(()) => log::info!("wrote {}", path.display()),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => fail(&path, err),
        }
    }
    let generated =
        [("settings.schema.json", margin_config::schema::settings_schema()), ("keymap.schema.json", margin_config::schema::keymap_schema())];
    for (name, schema) in generated {
        let path = schemas.join(name);
        // A `Value` always serialises; an empty file would only lose editor completion.
        let text = serde_json::to_string_pretty(&schema).unwrap_or_default();
        if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
            continue;
        }
        if let Err(err) = std::fs::write(&path, text) {
            fail(&path, err);
        }
    }
    problems
}

/// Logs every problem and shows the first in a notice (a bad value says which file, line and key,
/// and what Margin used instead). With no problem, a reload says it happened.
fn report(app: &mut PdfCraftApp, problems: &[Problem], reloaded: bool) {
    for problem in problems {
        match problem.severity {
            Severity::Error | Severity::Warning => log::warn!("{problem}"),
            Severity::Info => log::info!("{problem}"),
        }
    }
    if let Some(first) = problems.first() {
        let name = Path::new(&first.file).file_name().and_then(|name| name.to_str()).unwrap_or(&first.file);
        let more = match problems.len().saturating_sub(1) {
            0 => String::new(),
            n => format!(" (and {n} more, in the log)"),
        };
        app.notify(format!("{name}:{}:{} {}{more}", first.line, first.col, first.message));
    } else if reloaded {
        app.notify("Settings and keymap reloaded");
    }
}

/// A watched file changed: reload once the saves settle.
fn poll(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let now = ctx.input(|input| input.time);
    let keys = &mut app.margin_keys;
    let Some(loaded) = keys.loaded.as_mut() else { return };
    if loaded.watch.as_ref().is_some_and(SettingsWatch::changed) {
        keys.reload_at = Some(now + RELOAD_DEBOUNCE_S);
    }
    let Some(at) = keys.reload_at else { return };
    if now < at {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(at - now));
        return;
    }
    keys.reload_at = None;
    keys.pending.clear();
    let problems = loaded.reload();
    app.notes.apply_config(&loaded.settings.notes);
    report(app, &problems, true);
    if let Err(err) = app.migrate_legacy_notes() {
        log::warn!("could not migrate legacy notes settings: {err}");
    }
}

/// Margin itself just wrote `settings.jsonc` (the Settings window): read it again now, so what
/// that window shows and the next document opened follow at once. Problems are reported as on
/// any reload. Returns `true` when settings were already loaded and have now been reapplied.
/// The watch still sees the save and reloads once more after the debounce.
pub(crate) fn reload_now(app: &mut PdfCraftApp) -> bool {
    let Some(loaded) = app.margin_keys.loaded.as_mut() else {
        return false;
    };
    let problems = loaded.reload();
    app.notes.apply_config(&loaded.settings.notes);
    report(app, &problems, false);
    true
}

/// Resolves this frame's key presses against the keymap and runs what they bind, then gives the
/// page this frame's share of a screen step (`margin::scroll_step`). A matched key (and the text
/// it types) is taken out of the input, so nothing else acts on it; Escape stays in for the
/// widgets that close on it, as in PdfCraft.
pub(crate) fn dispatch(app: &mut PdfCraftApp, ctx: &egui::Context) {
    resolve_keys(app, ctx);
    scroll_step::drive(app, ctx);
}

fn resolve_keys(app: &mut PdfCraftApp, ctx: &egui::Context) {
    ensure_loaded(app, ctx);
    poll(app, ctx);
    let stack = context_stack(app, ctx);
    let os = Os::current();
    let now = ctx.input(|input| input.time);
    let events = ctx.input(|input| input.events.clone());
    let mut consumed: Vec<usize> = Vec::new();
    let mut actions: Vec<(String, Value)> = Vec::new();
    let keys = &mut app.margin_keys;
    let Some(table) = keys.loaded.as_ref().and_then(|loaded| loaded.compiled.as_ref()).map(|compiled| &compiled.keymap) else { return };
    if !keys.pending.is_empty() {
        let timeout_s = f64::from(table.chord_timeout_ms) / 1000.0;
        let waited = now - keys.pending_since;
        if waited >= timeout_s {
            let outcome = resolve::timeout(table, &stack, &keys.pending);
            if let Outcome::Command { command, args } = &outcome {
                actions.push((command.clone(), args.clone()));
            }
            keys.last = Some(LastKey { typed: std::mem::take(&mut keys.pending), stack: stack.clone(), outcome });
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(timeout_s - waited));
        }
    }
    // A control focused from the keyboard (Tab) takes plain Space and Enter: egui clicks it.
    let control_focused = control_focused(ctx);
    for (index, event) in events.iter().enumerate() {
        let Event::Key { key, pressed: true, modifiers, .. } = event else { continue };
        if control_focused && modifiers.is_none() && matches!(key, Key::Space | Key::Enter) {
            continue;
        }
        let Some(stroke) = stroke(*key, *modifiers, os) else { continue };
        let step = resolve::step(table, &stack, &keys.pending, &stroke);
        let mut typed = keys.pending.clone();
        typed.push(stroke);
        let consume = match &step.outcome {
            Outcome::Command { command, args } => {
                actions.push((command.clone(), args.clone()));
                command != "app.close_layer"
            }
            Outcome::Pending => {
                keys.pending_since = now;
                true
            }
            Outcome::NoMatch => false,
        };
        if consume {
            consumed.push(index);
            // egui-winit sends the character a key types right after the key.
            if matches!(events.get(index + 1), Some(Event::Text(_))) {
                consumed.push(index + 1);
            }
        }
        keys.pending = step.pending;
        keys.last = Some(LastKey { typed, stack: stack.clone(), outcome: step.outcome });
    }
    if !consumed.is_empty() {
        ctx.input_mut(|input| {
            let mut index = 0;
            input.events.retain(|_| {
                let keep = !consumed.contains(&index);
                index += 1;
                keep
            });
        });
    }
    for (id, args) in actions {
        if !run(app, ctx, &id, &args) {
            log::warn!("keymap: `{id}` is registered but does nothing here");
        }
    }
}

/// A control (not a text field) has the keyboard focus: it was reached with Tab.
fn control_focused(ctx: &egui::Context) -> bool {
    !ctx.text_edit_focused() && ctx.memory(|memory| memory.focused()).is_some()
}

/// What is left of `canvas::shortcuts` once its keys are bound in the keymap: ⌘C, which egui
/// delivers as a Copy event, not a key.
pub(crate) fn view_keys(view: &mut DocView, ctx: &egui::Context) {
    if ctx.text_edit_focused() {
        return;
    }
    let copy = ctx.input(|input| input.events.iter().any(|event| matches!(event, Event::Copy)));
    if copy && let Some(text) = view.selected_text() {
        ctx.copy_text(text);
    }
}

fn node(name: &str, attrs: &[(&str, &str)]) -> ContextNode {
    ContextNode {
        name: name.to_owned(),
        attrs: attrs.iter().map(|(attr, value)| ((*attr).to_owned(), (*value).to_owned())).collect(),
        flags: Vec::new(),
    }
}

/// The focused context stack, outermost first.
pub(crate) fn context_stack(app: &PdfCraftApp, ctx: &egui::Context) -> Vec<ContextNode> {
    // A text field typing; a button focused from the keyboard leaves the reader's keys working.
    let typing = ctx.text_edit_focused();
    let mut stack = vec![node("App", &[("os", Os::current().name())])];
    let text_input = |stack: &mut Vec<ContextNode>| {
        if typing {
            stack.push(node("TextInput", &[]));
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    let stats_sheet = app.margin_stats.sheet_open();
    #[cfg(target_arch = "wasm32")]
    let stats_sheet = false;
    if app.margin_keys.sheet.is_some() || stats_sheet || app.dialog.is_some() || app.password_prompt.is_some() {
        stack.push(node("Dialog", &[]));
        text_input(&mut stack);
        return stack;
    }
    if app.palette_open {
        stack.push(node("Palette", &[]));
        text_input(&mut stack);
        return stack;
    }
    let Some(view) = app.active.and_then(|index| app.views.get(index)) else {
        // Home: Margin's library.
        stack.push(node("Library", &[]));
        text_input(&mut stack);
        return stack;
    };
    let layout = match view.layout {
        PageLayout::Continuous => "scroll",
        PageLayout::Single => "single",
        PageLayout::TwoUp => "two_up",
    };
    stack.push(node("Reader", &[("format", "pdf"), ("layout", layout)]));
    match app.quick_tool {
        QuickTool::Comment(tool) => stack.push(node("AnnotateMode", &[("tool", tool.command())])),
        _ if super::annotate_mode(app) => stack.push(node("AnnotateMode", &[("tool", "reader.tool.select")])),
        _ => {}
    }
    let find_focused = ctx.memory(|memory| memory.focused()) == Some(egui::Id::new(super::find::INPUT_ID));
    if find_focused {
        stack.push(node("Find", &[]));
        stack.push(node("TextInput", &[]));
    } else if typing || view.forms.focus.is_some() {
        stack.push(node("TextInput", &[]));
    } else {
        stack.push(node("Page", &[]));
        if view.selected_text().is_some() {
            stack.push(node("Selection", &[("selection", "text")]));
        }
        // A comment picked on the page or in the Highlights tab: its delete and colour keys.
        if view.comments.selected.is_some() {
            stack.push(node("Annotation", &[]));
        }
    }
    stack
}

/// A key press in the canonical keystroke form `margin-config` compiles keymaps to: modifiers
/// `ctrl- alt- shift- super-` (Cmd is `super-` on macOS), then the key. Shift goes only with
/// letters and named keys; for a symbol the character already says it (`?`, not `shift-/`).
pub(crate) fn stroke(key: Key, modifiers: Modifiers, os: Os) -> Option<String> {
    let name = match key {
        Key::ArrowDown => "down".to_owned(),
        Key::ArrowUp => "up".to_owned(),
        Key::ArrowLeft => "left".to_owned(),
        Key::ArrowRight => "right".to_owned(),
        Key::PageUp => "pageup".to_owned(),
        Key::PageDown => "pagedown".to_owned(),
        Key::Minus => "-".to_owned(),
        Key::Quote => "'".to_owned(),
        Key::Copy | Key::Cut | Key::Paste => return None,
        other => other.symbol_or_name().to_lowercase(),
    };
    let symbol = name.chars().count() == 1 && !name.chars().all(|c| c.is_ascii_alphabetic());
    let mac = os == Os::Macos;
    let flags = [
        (if mac { modifiers.ctrl } else { modifiers.ctrl || modifiers.command }, "ctrl-"),
        (modifiers.alt, "alt-"),
        (modifiers.shift && !symbol, "shift-"),
        (mac && (modifiers.mac_cmd || modifiers.command), "super-"),
    ];
    Some(flags.iter().filter(|(on, _)| *on).map(|(_, prefix)| *prefix).chain([name.as_str()]).collect())
}

/// Runs a bound id (also the footer's zoom controls, so they share the keys' commands). `false`
/// when nothing here handles it.
pub(crate) fn run(app: &mut PdfCraftApp, ctx: &egui::Context, id: &str, args: &Value) -> bool {
    match id {
        "app.close_layer" => close_layer(app, ctx),
        "app.key_context" => app.margin_keys.key_context_open = !app.margin_keys.key_context_open,
        "help.shortcuts" => app.margin_keys.toggle_sheet(),
        "reader.tool.select" => select_tool(app),
        _ if commands::command(id).is_some() || is_catalog(id) => run_pdfcraft(app, ctx, id),
        _ if is_view_command(id) => {
            let settings = app.margin_keys.settings().view.clone();
            // Without a document there is nothing to move: the key is spent, as in PdfCraft.
            let Some(view) = app.active.and_then(|index| app.views.get_mut(index)) else { return true };
            return view_command(view, id, args, &settings);
        }
        // The reader chrome's ids (sidebar, annotate rail).
        _ => return super::run(app, id, args),
    }
    true
}

/// The Select tool: ends any comment tool (the rail's Select, `V` in annotate mode).
pub(crate) fn select_tool(app: &mut PdfCraftApp) {
    app.quick_tool = QuickTool::Select;
}

fn is_catalog(id: &str) -> bool {
    pdfcraft_engine::catalog::TOOL_GROUPS.iter().flat_map(|group| group.sections).flat_map(|section| section.items).any(|item| item.command == id)
}

/// A PdfCraft id, as `registry_shortcuts` runs it: while a form field's editor has the keyboard,
/// it runs next frame, after the field took this frame's typing (#166).
fn run_pdfcraft(app: &mut PdfCraftApp, ctx: &egui::Context, id: &str) {
    let form_typing = app.active.and_then(|index| app.views.get(index)).is_some_and(|view| view.forms.focus.is_some());
    match commands::command(id) {
        Some(spec) if form_typing => {
            app.deferred_commands.push((spec.id, app.active_ids().map(|(_, doc)| doc)));
            ctx.request_repaint();
        }
        _ => app.run_command(id),
    }
}

/// Escape: closes Margin's own layer, else does what PdfCraft's view did (clear the selection,
/// else close the search). Text fields, dialogs, the palette and an open popover (Display,
/// zoom, a colour) close themselves on the Escape the dispatcher leaves in the input, and that
/// Escape does nothing else.
fn close_layer(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if app.margin_keys.sheet.take().is_some() {
        return;
    }
    if ctx.text_edit_focused() || app.dialog.is_some() || app.palette_open || egui::Popup::is_any_open(ctx) {
        return;
    }
    if control_focused(ctx) {
        // A control focused from the keyboard is the topmost thing: Esc lets go of it.
        ctx.memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                memory.surrender_focus(id);
            }
        });
        return;
    }
    let Some(view) = app.active.and_then(|index| app.views.get_mut(index)) else { return };
    // The selection bar and its selection close together.
    if view.margin.selection.dismiss() {
        view.clear_selection();
        return;
    }
    if view.selected_text().is_some() {
        view.clear_selection();
    } else {
        view.find = None;
    }
}

/// The document view's ids (PdfCraft hard-codes their keys in `canvas::shortcuts`).
const VIEW_COMMANDS: [&str; 21] = [
    "view.zoom.in",
    "view.zoom.out",
    "view.zoom.set",
    "view.fit.page",
    "view.fit.width",
    "view.fit.read",
    "view.fit.toggle_width",
    "view.rotate.cw",
    "view.rotate.ccw",
    "reader.find.next",
    "reader.find.prev",
    "reader.back",
    "reader.forward",
    "reader.scroll.down",
    "reader.scroll.up",
    "reader.screen.down",
    "reader.screen.up",
    "reader.page.next",
    "reader.page.prev",
    "reader.page.first",
    "reader.page.last",
];

/// Whether `id` acts on the document view (and so does nothing without one).
fn is_view_command(id: &str) -> bool {
    VIEW_COMMANDS.contains(&id)
}

fn view_command(view: &mut DocView, id: &str, args: &Value, settings: &margin_config::View) -> bool {
    // Scroll steps in view pixels; settings bound them (1..2000 px, 0.1..1 screens).
    let line = settings.scroll_line_px as f32;
    let screen = (view.viewport_h * settings.scroll_screen as f32).max(line);
    // Anything but scrolling moves the page elsewhere: what is left of a screen step must not
    // carry on from there.
    if !id.starts_with("reader.scroll.") && !id.starts_with("reader.screen.") {
        view.margin.screen_step.stop();
    }
    match id {
        "view.zoom.in" => zoom_stop(view, &settings.zoom_steps, true),
        "view.zoom.out" => zoom_stop(view, &settings.zoom_steps, false),
        "view.zoom.set" => {
            let Some(percent) = args.get("percent").and_then(Value::as_f64).filter(|p| p.is_finite()) else { return false };
            view.set_zoom((percent / 100.0).clamp(MIN_ZOOM, MAX_ZOOM) as f32);
        }
        "view.fit.page" => reading::set_fit(view, Fit::Page),
        "view.fit.width" => reading::set_fit(view, Fit::Width),
        "view.fit.read" => reading::fit_read(view, settings.reading_width_px),
        // `W`: the width of the window, or back to the reading width.
        "view.fit.toggle_width" if view.fit == Fit::Width => reading::fit_read(view, settings.reading_width_px),
        "view.fit.toggle_width" => reading::set_fit(view, Fit::Width),
        "view.rotate.cw" => view.rotate_view(true),
        "view.rotate.ccw" => view.rotate_view(false),
        "reader.find.next" => view.find_step(true),
        "reader.find.prev" => view.find_step(false),
        // No history: nothing to go back to, as in PdfCraft.
        "reader.back" => _ = view.view_history(false),
        "reader.forward" => _ = view.view_history(true),
        "reader.scroll.down" => view.key_scroll += line,
        "reader.scroll.up" => view.key_scroll -= line,
        "reader.screen.down" | "reader.screen.up" => {
            let down = id == "reader.screen.down";
            let by = if down { screen } else { -screen };
            // Single pans an enlarged page to its edge first, then turns the page.
            let pan = if view.layout == PageLayout::Single { scroll_step::single_pan(view, by) } else { Some(by) };
            match pan {
                Some(pan) => view.margin.screen_step.request(pan),
                None => {
                    view.margin.screen_step.stop();
                    view.step_page(down);
                }
            }
        }
        "reader.page.next" => view.step_page(true),
        "reader.page.prev" => view.step_page(false),
        "reader.page.first" => view.go_to_page(0),
        "reader.page.last" => view.go_to_page(usize::MAX),
        _ => return false,
    }
    true
}

/// The next stop of `view.zoom_steps` (percent) above or below the zoom; past the last stop,
/// PdfCraft's own zoom steps carry on.
fn zoom_stop(view: &mut DocView, steps: &[u32], up: bool) {
    let zoom = view.zoom;
    let stops = steps.iter().map(|&percent| percent as f32 / 100.0);
    let next = if up {
        stops.filter(|stop| *stop > zoom * 1.01).min_by(f32::total_cmp)
    } else {
        stops.filter(|stop| *stop < zoom * 0.99).max_by(f32::total_cmp)
    };
    match next {
        Some(stop) => view.set_zoom(stop),
        None => view.zoom_step(up),
    }
}

/// Draws the keyboard sheet, the key context window and the pending chord.
pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if app.margin_keys.sheet.is_some() {
        super::keysheet::show(app, ctx);
    }
    if app.margin_keys.key_context_open {
        super::keysheet::key_context(app, ctx);
    }
    if !app.margin_keys.pending.is_empty() && app.margin_keys.settings().keys.show_pending {
        super::keysheet::pending_chord(app, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strokes_match_the_keymap_syntax() {
        let ctrl = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
        let shift = Modifiers { shift: true, ..Modifiers::NONE };
        assert_eq!(stroke(Key::P, ctrl, Os::Linux).as_deref(), Some("ctrl-p"));
        assert_eq!(stroke(Key::S, Modifiers { shift: true, ..ctrl }, Os::Linux).as_deref(), Some("ctrl-shift-s"));
        assert_eq!(stroke(Key::Questionmark, shift, Os::Linux).as_deref(), Some("?"));
        assert_eq!(stroke(Key::Plus, Modifiers { shift: true, ..ctrl }, Os::Linux).as_deref(), Some("ctrl-+"));
        assert_eq!(stroke(Key::Minus, ctrl, Os::Linux).as_deref(), Some("ctrl--"));
        assert_eq!(stroke(Key::Space, shift, Os::Linux).as_deref(), Some("shift-space"));
        assert_eq!(stroke(Key::PageDown, Modifiers::NONE, Os::Linux).as_deref(), Some("pagedown"));
        assert_eq!(stroke(Key::F3, shift, Os::Linux).as_deref(), Some("shift-f3"));
        let cmd = Modifiers { mac_cmd: true, command: true, ..Modifiers::NONE };
        assert_eq!(stroke(Key::K, cmd, Os::Macos).as_deref(), Some("super-k"));
        assert_eq!(stroke(Key::Copy, Modifiers::NONE, Os::Linux), None);
        for key in Key::ALL {
            if let Some(text) = stroke(*key, Modifiers::NONE, Os::Linux) {
                assert!(!text.is_empty() && text == text.to_lowercase(), "{key:?} -> {text}");
            }
        }
    }

    /// The ids `margin::run` (the reader chrome, reading state and library) handles.
    const CHROME_COMMANDS: [&str; 10] = [
        "view.sidebar.toggle",
        "view.sidebar.tab",
        "reader.annotate.toggle",
        "reader.bookmark.toggle",
        "view.trim.toggle",
        "library.open",
        "library.search",
        "app.stats",
        "annotation.delete",
        "annotation.color",
    ];

    /// Every id the built-in keymap binds is one the app runs.
    #[test]
    fn every_built_in_binding_runs() {
        let registry = Registry::load().unwrap();
        let options = CompileOptions { os: Os::Linux, chord_timeout_ms: 1000, annotate_letters: true };
        let compiled = margin_config::compile_shipped(&registry, &options);
        assert_eq!(compiled.problems, []);
        for binding in &compiled.keymap.bindings {
            let Some(id) = binding.command.as_deref() else { continue };
            let handled = matches!(id, "app.close_layer" | "app.key_context" | "help.shortcuts" | "reader.tool.select")
                || commands::command(id).is_some()
                || is_catalog(id)
                || is_view_command(id)
                || CHROME_COMMANDS.contains(&id);
            assert!(handled, "{id} is bound but run() has no action for it");
        }
        for entry in registry.margin_entries() {
            let id = entry.id.as_str();
            assert!(
                matches!(id, "app.close_layer" | "app.key_context" | "reader.tool.select") || is_view_command(id) || CHROME_COMMANDS.contains(&id),
                "{id} is registered but run() ignores it"
            );
        }
    }

    /// PdfCraft's static shortcut applies only where the keymap leaves both id and keys alone.
    #[test]
    fn static_shortcuts_fall_back_only_where_unbound() {
        let registry = Registry::load().unwrap();
        let user = "[{ \"bindings\": { \"ctrl-p\": \"view.palette\", \"ctrl-s\": null } }]";
        let compiled = {
            let options = CompileOptions { os: Os::current(), chord_timeout_ms: 1000, annotate_letters: true };
            margin_config::compile(&registry, &[KeymapSource { file: "keymap.jsonc", text: user }], &options)
        };
        let bindings = &compiled.keymap.bindings;
        let keys = MarginKeys {
            loaded: Some(Loaded {
                dir: None,
                registry: Some(registry),
                settings: Settings::default(),
                problems: Vec::new(),
                bound_ids: bindings.iter().filter_map(|b| b.command.clone()).collect(),
                bound_strokes: bindings.iter().filter_map(|b| b.keys.first().cloned()).collect(),
                compiled: Some(compiled),
                watch: None,
            }),
            ..MarginKeys::default()
        };
        let spec = |id: &str| commands::command(id).unwrap();
        assert!(!keys.falls_back(spec("print.dialog")), "Ctrl+P belongs to the keymap now");
        assert!(!keys.falls_back(spec("file.save")), "an unbind hides the static shortcut too");
        assert!(keys.falls_back(spec("file.open")), "untouched shortcuts keep working");
        assert!(MarginKeys::default().falls_back(spec("file.open")), "before loading, everything falls back");
    }

    #[test]
    fn zoom_steps_follow_the_settings_then_pdfcraft() {
        let info = pdfcraft_render::DocInfo::default();
        let mut view = DocView::new(pdfcraft_engine::DocId(1), &info, crate::canvas::ViewDefaults::default());
        view.zoom = 1.0;
        zoom_stop(&mut view, &[50, 100, 150], true);
        assert!((view.zoom - 1.5).abs() < 1e-3, "{}", view.zoom);
        zoom_stop(&mut view, &[50, 100, 150], true);
        assert!(view.zoom > 1.5, "past the last stop PdfCraft's steps continue: {}", view.zoom);
    }
}
