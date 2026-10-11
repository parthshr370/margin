//! Margin's macOS menu bar: Margin, File, Edit, View, Go, Window and Help, each row one of the
//! commands the reader, the palette and the keymap already run.
//!
//! The keyboard stays the keymap's. A row shows the ⌘ shortcut the keymap gives its command, but
//! when that shortcut is pressed, the menu hands the key back to the window untouched
//! ([`return_key_to_window`]): the keymap resolves it by context as it would without a menu (the
//! Library's ⌘F searches the Library, a text field keeps ⌘C and ⌘Z). Only a click runs the row's
//! command directly. A disabled row shows its shortcut but doesn't take the key: the window gets
//! it (checked by hand: with Find disabled in the Library, ⌘F still focuses the Library search).
//!
//! Rows follow the app each frame ([`tick`]): enabled state, the Undo and Redo labels, the
//! shortcuts when the keymap is reloaded, and the Open Recent list.

use std::str::FromStr;
use std::sync::mpsc;

use muda::accelerator::Accelerator;
use muda::{IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use pdfcraft_engine::commands;
use serde_json::Value;

use super::look::{self, CommandKeys};
use crate::PdfCraftApp;

/// How many recent files Open Recent lists (the document menu's Recent shows as many).
const RECENT_ROWS: usize = 10;
/// The id prefix of a row of [`MENUS`] (followed by its index in [`MenuBar::rows`]).
const ROW_ID: &str = "row:";
/// The id prefix of an Open Recent row (followed by its index in [`MenuBar::recent_paths`]).
const RECENT_ID: &str = "recent:";

/// What a row does when it is chosen.
#[derive(Clone, Copy)]
enum Action {
    /// A command id the keymap and the palette also run, and its arguments as JSON (`""` for none).
    Command { id: &'static str, args: &'static str },
    /// An editing action of the focused text field or page selection, with its fixed shortcut.
    Edit(EditAction),
}

#[derive(Clone, Copy)]
enum EditAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
}

impl EditAction {
    /// The shortcut, as egui and every macOS app take it.
    fn accelerator(self) -> &'static str {
        match self {
            Self::Cut => "Super+X",
            Self::Copy => "Super+C",
            Self::Paste => "Super+V",
            Self::SelectAll => "Super+A",
        }
    }
}

/// One entry of a menu.
enum Row {
    /// A row with a fixed title.
    Item(&'static str, Action),
    /// A row titled by its PdfCraft command, which may change ("Undo Rotate page").
    Live(&'static str),
    Separator,
    Submenu(&'static str, &'static [Row]),
    /// Open Recent: the recent files, filled in by [`tick`].
    Recent,
    /// A row macOS itself provides.
    System(SystemRow),
}

#[derive(Clone, Copy)]
enum SystemRow {
    Services,
    Hide,
    HideOthers,
    ShowAll,
    Quit,
    Minimize,
    Zoom,
    BringAllToFront,
}

/// What macOS does with a top-level menu besides showing it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Plain,
    /// The Window menu: macOS adds the open windows to it.
    Window,
    /// The Help menu: macOS adds its search field to it.
    Help,
}

const fn command(title: &'static str, id: &'static str) -> Row {
    Row::Item(title, Action::Command { id, args: "" })
}

const fn command_with(title: &'static str, id: &'static str, args: &'static str) -> Row {
    Row::Item(title, Action::Command { id, args })
}

const EXPORT: &[Row] = &[
    command("Text…", "export.text"),
    command("Word Document…", "export.docx"),
    command("HTML…", "export.html"),
    command("Rich Text…", "export.rtf"),
    Row::Separator,
    command("Page as Image…", "export.image"),
    command("All Images…", "export.all_images"),
];

/// The menu bar, left to right. The first menu is the application menu, which macOS titles with
/// the app's name.
const MENUS: &[(&str, Role, &[Row])] = &[
    (
        "Margin",
        Role::Plain,
        &[
            command("About Margin", "help.about"),
            Row::Separator,
            command("Settings…", "app.preferences"),
            command("Reading Stats", "app.stats"),
            Row::Separator,
            Row::System(SystemRow::Services),
            Row::Separator,
            Row::System(SystemRow::Hide),
            Row::System(SystemRow::HideOthers),
            Row::System(SystemRow::ShowAll),
            Row::Separator,
            Row::System(SystemRow::Quit),
        ],
    ),
    (
        "File",
        Role::Plain,
        &[
            command("Open…", "file.open"),
            Row::Recent,
            command("Library", "library.open"),
            Row::Separator,
            command("Save", "file.save"),
            command("Save As…", "file.save_as"),
            Row::Submenu("Export", EXPORT),
            Row::Separator,
            command("Document Properties…", "file.properties"),
            command("Print…", "print.dialog"),
            Row::Separator,
            command("Close", "file.close"),
            command("Close All", "file.close_all"),
        ],
    ),
    (
        "Edit",
        Role::Plain,
        &[
            Row::Live("edit.undo"),
            Row::Live("edit.redo"),
            Row::Separator,
            Row::Item("Cut", Action::Edit(EditAction::Cut)),
            Row::Item("Copy", Action::Edit(EditAction::Copy)),
            Row::Item("Paste", Action::Edit(EditAction::Paste)),
            Row::Item("Select All", Action::Edit(EditAction::SelectAll)),
            Row::Separator,
            command("Find…", "edit.find"),
            command("Find Next", "reader.find.next"),
            command("Find Previous", "reader.find.prev"),
            command("Search Library", "library.search"),
        ],
    ),
    (
        "View",
        Role::Plain,
        &[
            command("Sidebar", "view.sidebar.toggle"),
            command_with("Contents", "view.sidebar.tab", r#"{"tab":"contents"}"#),
            command_with("Highlights", "view.sidebar.tab", r#"{"tab":"highlights"}"#),
            command_with("Pages", "view.sidebar.tab", r#"{"tab":"pages"}"#),
            command_with("Bookmarks", "view.sidebar.tab", r#"{"tab":"bookmarks"}"#),
            Row::Separator,
            command("Zoom In", "view.zoom.in"),
            command("Zoom Out", "view.zoom.out"),
            command_with("Actual Size", "view.zoom.set", r#"{"percent":100}"#),
            command("Fit Page", "view.fit.page"),
            command("Fit Width", "view.fit.width"),
            command("Fit Reading Width", "view.fit.read"),
            Row::Separator,
            command("Rotate Clockwise", "view.rotate.cw"),
            command("Rotate Counterclockwise", "view.rotate.ccw"),
            command("Trim Margins", "view.trim.toggle"),
            Row::Separator,
            command("Annotate", "reader.annotate.toggle"),
            command("Command Palette…", "view.palette"),
            Row::Separator,
            // Margin's page-only full screen; macOS adds its own Enter Full Screen (Globe-F) below.
            command("Full Screen Reading", "view.full_screen"),
        ],
    ),
    (
        "Go",
        Role::Plain,
        &[
            command("Next Page", "reader.page.next"),
            command("Previous Page", "reader.page.prev"),
            command("First Page", "reader.page.first"),
            command("Last Page", "reader.page.last"),
            command("Go to Page…", "view.focus_page_input"),
            Row::Separator,
            command("Back", "reader.back"),
            command("Forward", "reader.forward"),
            Row::Separator,
            command("Bookmark This Page", "reader.bookmark.toggle"),
        ],
    ),
    (
        "Window",
        Role::Window,
        &[Row::System(SystemRow::Minimize), Row::System(SystemRow::Zoom), Row::Separator, Row::System(SystemRow::BringAllToFront)],
    ),
    ("Help", Role::Help, &[command("Keyboard Shortcuts", "help.shortcuts"), command("Show Key Context", "app.key_context")]),
];

/// A row the app keeps in step with its state.
struct LiveRow {
    item: MenuItem,
    action: Action,
    /// Titled by its PdfCraft command each frame ([`Row::Live`]).
    live_title: bool,
    /// The shortcut the keymap gives it, as the row shows it.
    keys: Option<Accelerator>,
    enabled: bool,
}

/// The installed menu bar and what [`tick`] compares the app against.
pub struct MenuBar {
    /// Owns the native menus; dropping it would empty the bar.
    _menu: Menu,
    rows: Vec<LiveRow>,
    recent: Submenu,
    recent_paths: Vec<String>,
    /// The keymap the shortcuts were read from.
    keymap_revision: Option<u64>,
    /// Rows clicked since the last frame.
    chosen: mpsc::Receiver<MenuId>,
    /// Edit events a clicked row sends to the focused field, at the start of the next frame.
    pending_input: Vec<egui::Event>,
}

/// Replaces the default application menu with Margin's. Call once, after the event loop has
/// started (from eframe's app creator). Without it, macOS keeps the default menu.
pub fn install(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let (sender, chosen) = mpsc::channel();
    let repaint = ctx.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if return_key_to_window() {
            return;
        }
        if sender.send(event.id).is_err() {
            log::warn!("menu bar: a chosen row arrived after the app stopped listening");
        }
        repaint.request_repaint();
    }));
    let menu = Menu::new();
    let mut rows = Vec::new();
    let mut recent = None;
    for (title, role, entries) in MENUS {
        let submenu = Submenu::new(*title, true);
        append_rows(&submenu, entries, &mut rows, &mut recent);
        append(&menu, &submenu, title);
        match role {
            Role::Plain => {}
            Role::Window => submenu.set_as_windows_menu_for_nsapp(),
            Role::Help => submenu.set_as_help_menu_for_nsapp(),
        }
    }
    let Some(recent) = recent else {
        log::warn!("menu bar: no Open Recent row; keeping the default menu");
        return;
    };
    menu.init_for_nsapp();
    app.menu_bar = Some(MenuBar { _menu: menu, rows, recent, recent_paths: Vec::new(), keymap_revision: None, chosen, pending_input: Vec::new() });
}

fn append_rows(submenu: &Submenu, entries: &[Row], rows: &mut Vec<LiveRow>, recent: &mut Option<Submenu>) {
    for entry in entries {
        match entry {
            Row::Item(title, action) => append(submenu, &new_row(rows, title, *action, false), title),
            Row::Live(id) => {
                let title = commands::command(id).map_or(*id, |spec| spec.label);
                append(submenu, &new_row(rows, title, Action::Command { id, args: "" }, true), title);
            }
            Row::Separator => append(submenu, &PredefinedMenuItem::separator(), "separator"),
            Row::Submenu(title, children) => {
                let child = Submenu::new(*title, true);
                append_rows(&child, children, rows, recent);
                append(submenu, &child, title);
            }
            Row::Recent => {
                let child = Submenu::new("Open Recent", false);
                append(submenu, &child, "Open Recent");
                *recent = Some(child);
            }
            Row::System(system) => append(submenu, &system_item(*system), "system row"),
        }
    }
}

/// A new row, enabled. A command row gets its shortcut on the first [`tick`]; an edit row's is
/// fixed.
fn new_row(rows: &mut Vec<LiveRow>, title: &str, action: Action, live_title: bool) -> MenuItem {
    let keys = match action {
        Action::Edit(edit) => parse_accelerator(edit.accelerator()),
        Action::Command { .. } => None,
    };
    let item = MenuItem::with_id(format!("{ROW_ID}{}", rows.len()), title, true, keys);
    rows.push(LiveRow { item: item.clone(), action, live_title, keys, enabled: true });
    item
}

fn system_item(row: SystemRow) -> PredefinedMenuItem {
    match row {
        SystemRow::Services => PredefinedMenuItem::services(None),
        SystemRow::Hide => PredefinedMenuItem::hide(Some("Hide Margin")),
        SystemRow::HideOthers => PredefinedMenuItem::hide_others(None),
        SystemRow::ShowAll => PredefinedMenuItem::show_all(None),
        SystemRow::Quit => PredefinedMenuItem::quit(Some("Quit Margin")),
        SystemRow::Minimize => PredefinedMenuItem::minimize(None),
        SystemRow::Zoom => PredefinedMenuItem::maximize(Some("Zoom")),
        SystemRow::BringAllToFront => PredefinedMenuItem::bring_all_to_front(None),
    }
}

/// Appends `item` to `parent`, logging (with `what`) a row macOS refused: the bar shows without it.
fn append(parent: &dyn AppendTo, item: &dyn IsMenuItem, what: &str) {
    if let Err(err) = parent.append_item(item) {
        log::warn!("menu bar: couldn't add {what}: {err}");
    }
}

/// A menu or a submenu, which muda gives the same `append` without a shared trait.
trait AppendTo {
    fn append_item(&self, item: &dyn IsMenuItem) -> muda::Result<()>;
}

impl AppendTo for Menu {
    fn append_item(&self, item: &dyn IsMenuItem) -> muda::Result<()> {
        self.append(item)
    }
}

impl AppendTo for Submenu {
    fn append_item(&self, item: &dyn IsMenuItem) -> muda::Result<()> {
        self.append(item)
    }
}

/// A row chosen by its shortcut: hands the key press to the window as if there were no menu, so
/// the keymap decides what it does. `false` for a click (or anything but a key press), which the
/// app then runs as the row's command.
fn return_key_to_window() -> bool {
    use objc2_app_kit::{NSApplication, NSEventType};
    let Some(main_thread) = objc2::MainThreadMarker::new() else { return false };
    let application = NSApplication::sharedApplication(main_thread);
    let Some(event) = application.currentEvent().filter(|event| event.r#type() == NSEventType::KeyDown) else { return false };
    let Some(responder) = application.keyWindow().and_then(|window| window.firstResponder()) else { return false };
    responder.keyDown(&event);
    true
}

/// Runs the rows chosen since the last frame and brings every row in step with the app.
pub(crate) fn tick(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let Some(mut bar) = app.menu_bar.take() else { return };
    while let Ok(id) = bar.chosen.try_recv() {
        run(app, &mut bar, ctx, id.as_ref());
    }
    let revision = app.margin_keys.revision();
    let reread_keys = bar.keymap_revision != Some(revision);
    bar.keymap_revision = Some(revision);
    for row in &mut bar.rows {
        sync_row(app, row, reread_keys);
    }
    sync_recent(app, &mut bar);
    if !bar.pending_input.is_empty() {
        ctx.request_repaint();
    }
    app.menu_bar = Some(bar);
}

/// The edit events clicked rows queued, for this frame's raw input.
pub(crate) fn take_pending_input(app: &mut PdfCraftApp) -> Vec<egui::Event> {
    app.menu_bar.as_mut().map(|bar| std::mem::take(&mut bar.pending_input)).unwrap_or_default()
}

fn run(app: &mut PdfCraftApp, bar: &mut MenuBar, ctx: &egui::Context, id: &str) {
    if let Some(index) = id.strip_prefix(RECENT_ID).and_then(|index| index.parse::<usize>().ok()) {
        if let Some(path) = bar.recent_paths.get(index).cloned() {
            super::library::open_book(app, &path);
        }
        return;
    }
    let Some(row) = id.strip_prefix(ROW_ID).and_then(|index| index.parse::<usize>().ok()).and_then(|index| bar.rows.get(index)) else {
        log::warn!("menu bar: unknown row {id}");
        return;
    };
    match row.action {
        Action::Command { id, args } => {
            let args = parse_args(args);
            if !super::keys::run(app, ctx, id, &args) {
                log::warn!("menu bar: nothing runs {id}");
            }
        }
        Action::Edit(edit) => bar.pending_input.extend(edit_events(edit)),
    }
}

fn parse_args(args: &str) -> Value {
    if args.is_empty() {
        return Value::Null;
    }
    serde_json::from_str(args).unwrap_or_else(|err| {
        log::warn!("menu bar: bad arguments {args}: {err}");
        Value::Null
    })
}

/// What egui would have received from the keyboard for `edit`.
fn edit_events(edit: EditAction) -> Vec<egui::Event> {
    match edit {
        EditAction::Cut => vec![egui::Event::Cut],
        EditAction::Copy => vec![egui::Event::Copy],
        EditAction::Paste => match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
            Ok(text) => vec![egui::Event::Paste(text)],
            Err(err) => {
                log::warn!("menu bar: Paste: couldn't read the clipboard: {err}");
                Vec::new()
            }
        },
        EditAction::SelectAll => [true, false]
            .into_iter()
            .map(|pressed| egui::Event::Key { key: egui::Key::A, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::COMMAND })
            .collect(),
    }
}

fn sync_row(app: &PdfCraftApp, row: &mut LiveRow, reread_keys: bool) {
    let Action::Command { id, args } = row.action else { return };
    if reread_keys {
        let keys = command_accelerator(app, id, &parse_args(args));
        // muda can set a key equivalent but not clear one: a shortcut the keymap drops stays shown
        // until restart. Harmless, since the key still goes to the keymap, which runs nothing.
        if keys != row.keys
            && let Some(shown) = keys
        {
            match row.item.set_accelerator(Some(shown)) {
                Ok(()) => row.keys = keys,
                Err(err) => log::warn!("menu bar: couldn't show the shortcut of {id}: {err}"),
            }
        }
    }
    let spec = commands::command(id);
    let enabled = match spec {
        Some(spec) => app.command_enabled(spec),
        None => !needs_document(id) || app.active.is_some(),
    };
    if enabled != row.enabled {
        row.item.set_enabled(enabled);
        row.enabled = enabled;
    }
    if row.live_title
        && let Some(spec) = spec
    {
        let title = commands::current_label(spec, &app.session, app.active_ids().map(|(_, doc)| doc));
        if row.item.text() != title {
            row.item.set_text(title);
        }
    }
}

/// Margin's own ids that act on the open document.
fn needs_document(id: &str) -> bool {
    ["view.", "reader.", "annotation."].iter().any(|prefix| id.starts_with(prefix))
}

/// The keymap's shortcut for `id` as a menu key equivalent: its first binding still in effect that
/// is a single keystroke with ⌘ (another would take plain typing, or be a chord the menu can't
/// show), so Find Next shows ⌘G though `f3` is bound first.
fn command_accelerator(app: &PdfCraftApp, id: &str, args: &Value) -> Option<Accelerator> {
    let shown = |keys: &[String]| match keys {
        [stroke] => stroke_accelerator(stroke),
        _ => None,
    };
    match look::command_keys_where(app, id, args, |keys| shown(keys).is_some())? {
        CommandKeys::Bound(keys) => shown(keys),
        CommandKeys::Static(shortcut) => margin_config::canonical(&super::keys::zed_keystroke(&shortcut), margin_config::Os::current())
            .ok()
            .and_then(|stroke| stroke_accelerator(&stroke)),
    }
}

/// A canonical keystroke as a menu key equivalent, when it has ⌘ and muda can write it.
fn stroke_accelerator(stroke: &str) -> Option<Accelerator> {
    menu_accelerator(stroke).and_then(|text| parse_accelerator(&text))
}

/// A canonical keystroke (`ctrl-alt-shift-super-key`) in muda's syntax (`Control+Shift+Super+S`),
/// when it has ⌘.
fn menu_accelerator(stroke: &str) -> Option<String> {
    const MODIFIERS: [(&str, &str); 4] = [("ctrl-", "Control"), ("alt-", "Alt"), ("shift-", "Shift"), ("super-", "Super")];
    let mut rest = stroke;
    let mut parts = Vec::with_capacity(MODIFIERS.len() + 1);
    for (prefix, name) in MODIFIERS {
        if let Some(after) = rest.strip_prefix(prefix) {
            parts.push(name);
            rest = after;
        }
    }
    // muda joins with `+`, so that key can't be written (zoom in shows its `=` binding instead).
    if !parts.contains(&"Super") || rest.is_empty() || rest == "+" {
        return None;
    }
    let mut text = parts.join("+");
    text.push('+');
    text.push_str(&rest.to_uppercase());
    Some(text)
}

fn parse_accelerator(text: &str) -> Option<Accelerator> {
    Accelerator::from_str(text).map_err(|err| log::debug!("menu bar: no key equivalent for {text}: {err}")).ok()
}

/// Rebuilds Open Recent when the recent files changed.
fn sync_recent(app: &PdfCraftApp, bar: &mut MenuBar) {
    let recent = app.recent.iter().take(RECENT_ROWS);
    if recent.clone().map(|file| &file.path).eq(bar.recent_paths.iter()) {
        return;
    }
    // Open Recent holds only the plain rows added below.
    for item in bar.recent.items() {
        let Some(row) = item.as_menuitem() else { continue };
        if let Err(err) = bar.recent.remove(row) {
            log::warn!("menu bar: couldn't clear Open Recent: {err}");
        }
    }
    bar.recent_paths.clear();
    for (index, file) in recent.enumerate() {
        append(&bar.recent, &MenuItem::with_id(format!("{RECENT_ID}{index}"), &file.name, true, None), "a recent file");
        bar.recent_paths.push(file.path.clone());
    }
    bar.recent.set_enabled(!bar.recent_paths.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_command_keystrokes_become_key_equivalents() {
        assert_eq!(menu_accelerator("super-o").as_deref(), Some("Super+O"));
        assert_eq!(menu_accelerator("shift-super-s").as_deref(), Some("Shift+Super+S"));
        assert_eq!(menu_accelerator("ctrl-super-h").as_deref(), Some("Control+Super+H"));
        assert_eq!(menu_accelerator("super--").as_deref(), Some("Super+-"));
        assert_eq!(menu_accelerator("super-pagedown").as_deref(), Some("Super+PAGEDOWN"));
        // Plain keys belong to typing and the page; a chord can't be a key equivalent.
        assert_eq!(menu_accelerator("j"), None);
        assert_eq!(menu_accelerator("shift-t"), None);
        assert_eq!(menu_accelerator("super-+"), None);
        for stroke in ["super-o", "shift-super-s", "ctrl-super-h", "super--", "super-=", "super-[", "super-,", "super-pagedown", "super-0"] {
            let text = menu_accelerator(stroke).unwrap();
            assert!(parse_accelerator(&text).is_some(), "{stroke} -> {text} doesn't parse");
        }
    }

    /// Every command row names an id the app runs, so a click never does nothing.
    #[test]
    fn every_row_runs() {
        let registry = margin_config::Registry::load().unwrap();
        fn check(rows: &[Row], registry: &margin_config::Registry) {
            for row in rows {
                match row {
                    Row::Item(_, Action::Command { id, args }) => {
                        let known = commands::command(id).is_some() || registry.margin_entries().any(|entry| entry.id.as_str() == *id);
                        assert!(known, "{id} is neither a PdfCraft nor a Margin command");
                        assert!(args.is_empty() || serde_json::from_str::<Value>(args).is_ok(), "{id}: bad arguments {args}");
                    }
                    Row::Live(id) => assert!(commands::command(id).is_some(), "{id} has no live label"),
                    Row::Submenu(_, children) => check(children, registry),
                    _ => {}
                }
            }
        }
        for (_, _, rows) in MENUS {
            check(rows, &registry);
        }
    }
}
