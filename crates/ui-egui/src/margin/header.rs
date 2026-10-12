//! The header over the page: sidebar toggle, view history and Tools on the left; the document's
//! title and current section in the middle; find, annotate, display, full screen and the
//! document menu on the right.

use egui::{Align, Layout};
use pdfcraft_engine::commands;
use serde_json::Value;

use crate::PdfCraftApp;
use crate::margin::contents::current_section_at;
use crate::margin::look::{self, GAP_LARGE};
use crate::margin::{SidebarTab, motion, open_tab, outline_at, reading, window_chrome};
use crate::theme::{self, Tokens};

pub const HEIGHT: f32 = 44.0;
/// Every header control is one control tall (`look::CONTROL_ROW`).
const BUTTON: f32 = look::CONTROL_ROW;
/// Space kept between the centred title and the button clusters.
const TITLE_GAP: f32 = 16.0;
/// The header title's tooltip wraps at this width, so a long path stays a readable block.
const TOOLTIP_WIDTH: f32 = 360.0;
/// The title's text size, and what the unsaved dot after the name reserves, drawn or not.
const TITLE_TEXT: f32 = 12.5;
const DIRTY_SLOT: &str = "  •";

/// A row of an in-window menu. The titles are the macOS menu bar's (`margin::menu_bar`), so
/// Linux, Windows and macOS name an action the same way.
#[derive(Clone, Copy)]
enum Entry {
    /// A title, the id it runs (PdfCraft's or Margin's registry) and its icon.
    Command(&'static str, &'static str, &'static str),
    Separator,
    Recent,
    Export,
    Import,
}

use Entry::{Command, Export, Import, Recent, Separator};

/// The document menu ("…" in the header), top to bottom.
const DOCUMENT_MENU: &[Entry] = &[
    Command("Open…", "file.open", "folder-open"),
    Recent,
    Command("Library", "library.open", "book-open"),
    Command("Add Library Folder…", "library.add_folder", "folder-plus"),
    Import,
    Separator,
    Command("Save", "file.save", "save"),
    Command("Save As…", "file.save_as", "save"),
    Command("Save a Copy with Highlights…", "reader.save_with_highlights", "copy-plus"),
    Export,
    Command("Document Properties…", "file.properties", "file-text"),
    Command("Print…", "print.dialog", "printer"),
    Separator,
    Command("Choose Obsidian Vault…", "app.choose_vault", "folder-open"),
    Command("Reading Stats", "app.stats", "clock"),
    Command("Command Palette…", "view.palette", "search"),
    Command("Focus Mode", "view.focus_mode.toggle", "minimize-2"),
    Separator,
    Command("Settings…", "app.preferences", "settings"),
    Command("Keyboard Shortcuts", "help.shortcuts", "circle-help"),
    Command("Show Key Context", "app.key_context", "square-terminal"),
    Command("About Margin", "help.about", "badge-check"),
    Separator,
    Command("Close", "file.close", "x"),
    Command("Close All", "file.close_all", "x"),
];

/// Export ▸, as the menu bar's.
const EXPORT_MENU: &[Entry] = &[
    Command("Text…", "export.text", "file-output"),
    Command("Word Document…", "export.docx", "file-output"),
    Command("HTML…", "export.html", "file-code"),
    Command("Rich Text…", "export.rtf", "file-output"),
    Separator,
    Command("Page as Image…", "export.image", "camera"),
    Command("All Images…", "export.all_images", "camera"),
    Separator,
    Command("Paper as Markdown…", "reader.export_markdown", "file-down"),
];

/// The Library's app menu: what the document menu offers without a document, and the library's
/// own actions, since the Library shows no reader header.
const APP_MENU: &[Entry] = &[
    Command("Open…", "file.open", "folder-open"),
    Recent,
    Command("Add Library Folder…", "library.add_folder", "folder-plus"),
    Import,
    Command("Choose Obsidian Vault…", "app.choose_vault", "folder-open"),
    Separator,
    Command("Command Palette…", "view.palette", "search"),
    Command("Settings…", "app.preferences", "settings"),
    Command("Keyboard Shortcuts", "help.shortcuts", "circle-help"),
    Command("About Margin", "help.about", "badge-check"),
];

/// The header, and under it the open documents' tab strip. While the active document is being
/// searched, the find field takes the title's place; returns true when the header has no room
/// for it, so it needs its own row.
pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui) -> bool {
    let t = Tokens::get(ui.ctx());
    let mut needs_row = false;
    // egui draws the panel's bottom divider line where the theme has frame edges.
    egui::Panel::top("margin_header")
        .exact_size(HEIGHT)
        .show_separator_line(look::has_edges(&t))
        .frame(egui::Frame::NONE.fill(t.titlebar).inner_margin(egui::Margin::symmetric(GAP_LARGE as i8, 0)))
        .show(ui, |ui| {
            let full = ui.max_rect();
            // The whole bar, its padding too, moves the window where nothing else takes the press.
            window_chrome::drag_area(app, ui, full.expand2(egui::vec2(GAP_LARGE, 0.0)), ui.id().with("margin-header-drag"));
            let traffic_lights = window_chrome::traffic_lights_inset(ui.ctx(), full);
            let mut left_end = full.left();
            let mut right_start = full.right();
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.add_space(traffic_lights);
                left_cluster(app, ui);
                left_end = ui.min_rect().right();
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    right_cluster(app, ui);
                    right_start = ui.min_rect().left();
                });
            });
            let finding = app.active.and_then(|i| app.views.get(i)).is_some_and(|v| v.find.is_some());
            let room = 2.0 * ((full.center().x - left_end).min(right_start - full.center().x) - TITLE_GAP);
            let between = (left_end + TITLE_GAP, right_start - TITLE_GAP);
            if !finding || !crate::margin::find::in_header(app, ui, full.center(), room, between) {
                needs_row = finding;
                title(app, ui, &t, full, left_end, right_start);
            }
        });
    // The open documents, under the header (hidden with it).
    crate::margin::tabs::strip(app, ui);
    needs_row
}

fn left_cluster(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let has_doc = app.active.is_some();
    let open = app.margin_chrome.sidebar_open;
    let keys = look::shortcut(app, "view.sidebar.toggle");
    if ui.add_enabled_ui(has_doc, |ui| look::icon_button(ui, "panel-left", BUTTON, open && has_doc, "Sidebar", keys.as_deref())).inner.clicked() {
        app.margin_chrome.sidebar_open = !open;
    }
    let (back_keys, forward_keys) = (look::shortcut(app, "reader.back"), look::shortcut(app, "reader.forward"));
    let Some(view) = app.active.and_then(|i| app.views.get_mut(i)) else { return };
    let (back, forward) = (!view.back.is_empty(), !view.forward.is_empty());
    if ui.add_enabled_ui(back, |ui| look::icon_button(ui, "chevron-left", BUTTON, false, "Previous view", back_keys.as_deref())).inner.clicked() {
        view.view_history(false);
    }
    if ui.add_enabled_ui(forward, |ui| look::icon_button(ui, "chevron-right", BUTTON, false, "Next view", forward_keys.as_deref())).inner.clicked() {
        view.view_history(true);
    }
    ui.add_space(4.0);
    // Always labelled, at the header's one control height: a narrow header shortens the title
    // rather than swapping this button for another of a different width.
    let tools_open = open && app.margin_chrome.tab == SidebarTab::Tools;
    let tools = look::Button::ghost(look::ButtonSize::Standard, "Tools").icon("wrench").selected(tools_open).tip("All tools").show(ui);
    if tools.clicked() {
        if tools_open {
            app.margin_chrome.sidebar_open = false;
        } else {
            open_tab(app, SidebarTab::Tools);
        }
    }
}

fn right_cluster(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let menu = look::icon_button(ui, "ellipsis", BUTTON, false, "Document: save, export, print, settings", None);
    egui::Popup::menu(&menu).show(|ui| document_menu(app, ui));
    let Some(index) = app.active else { return };
    let keys = look::shortcut(app, "view.full_screen");
    if look::icon_button(ui, "maximize", BUTTON, app.full_screen, "Full screen", keys.as_deref()).clicked() {
        app.execute("view.full_screen");
    }
    let display_id = egui::Id::new("margin-display-popover");
    let display_open = egui::Popup::is_id_open(ui.ctx(), display_id);
    let display = look::icon_button(ui, "contrast", BUTTON, display_open, "Page colour and layout", None);
    let popup = look::popover(&display, display_id).frame(crate::margin::display::popover_frame(ui.style()));
    let rise_id = popup.get_id().with("margin-rise");
    // egui fades the popup in (`motion::sync` sets its fade time); it also rises a few pixels.
    match popup.show(|ui| display_popover(app, ui)) {
        Some(shown) => {
            let rise = (1.0 - motion::toggle(ui.ctx(), rise_id, true, motion::FADE)) * motion::RISE;
            ui.ctx().transform_layer_shapes(shown.response.layer_id, egui::emath::TSTransform::from_translation(egui::vec2(0.0, rise)));
        }
        // Closed: back to the start at once, so the next opening rises again.
        None => _ = motion::toggle(ui.ctx(), rise_id, false, 0.0),
    }
    let rail = app.margin_chrome.rail_open;
    let keys = look::shortcut(app, "reader.annotate.toggle");
    if look::icon_button(ui, "pen-tool", BUTTON, rail, "Annotate", keys.as_deref()).clicked() {
        app.margin_chrome.rail_open = !rail;
    }
    let finding = app.views.get(index).is_some_and(|v| v.find.is_some());
    let keys = look::shortcut(app, "edit.find");
    if look::icon_button(ui, "search", BUTTON, finding, "Find in document", keys.as_deref()).clicked() {
        match app.views.get_mut(index) {
            Some(view) if finding => view.find = None,
            _ => {
                app.execute("edit.find");
            }
        }
    }
}

/// The window's title in Margin: PdfCraft's `name — Margin` (`fallback`, built from the
/// document's display name or title preference), with a leading `•` while the active document has
/// unsaved edits. Only the dirty flag adds to it, so scrolling never changes it.
pub(crate) fn window_title(app: &PdfCraftApp, fallback: String) -> String {
    let dirty = app.active_ids().and_then(|(_, id)| app.session.get(id)).is_some_and(|doc| doc.dirty);
    if dirty { format!("• {fallback}") } else { fallback }
}

/// Title and current section, clipped to the space the buttons leave (also the full-screen
/// bar's, `margin::focus`). The document's name alone is centred, so a new section or the unsaved
/// dot never slides it: the dot has a slot after the name whether it shows or not, and the
/// section follows, cut to what is left. Hovering it shows the whole name, the file's location
/// and the section, which the clipped line may hide.
pub(crate) fn title(app: &PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, full: egui::Rect, left_end: f32, right_start: f32) {
    let center = full.center().x;
    let half = (center - left_end).min(right_start - center) - TITLE_GAP;
    if half <= 0.0 {
        return;
    }
    let font = theme::regular(TITLE_TEXT);
    let line = |text: &str, color, width: f32| {
        let mut job = egui::text::LayoutJob::single_section(text.to_owned(), egui::TextFormat::simple(font.clone(), color));
        job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(0.0));
        ui.painter().layout_job(job)
    };
    let Some((view, doc)) = app.active_ids().and_then(|(i, id)| Some((app.views.get(i)?, app.session.get(id)?))) else {
        let galley = line("Margin", t.text_muted, half * 2.0);
        ui.painter().galley(egui::pos2(center - galley.size().x / 2.0, full.center().y - galley.size().y / 2.0), galley, t.text_muted);
        return;
    };
    let name = doc.info.title.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| doc.display_name());
    let dot_width = look::template_width(ui, DIRTY_SLOT, font.clone());
    let name_galley = line(&name, t.text_muted, half * 2.0 - dot_width);
    let top = full.center().y - name_galley.size().y / 2.0;
    let name_left = (center - name_galley.size().x / 2.0).max(center - half);
    let mut right = name_left + name_galley.size().x;
    let mut rect = egui::Rect::from_min_size(egui::pos2(name_left, top), name_galley.size());
    ui.painter().galley(egui::pos2(name_left, top), name_galley, t.text_muted);
    if doc.dirty {
        let dot = line(DIRTY_SLOT, t.accent, dot_width);
        ui.painter().galley(egui::pos2(right, top), dot, t.accent);
    }
    right += dot_width;
    let section = current_section_at(&doc.info, reading::spot(view), view.rotation)
        .and_then(|p| outline_at(&doc.info.outline, &p).map(|i| i.title.trim().to_owned()));
    if let Some(section) = &section {
        let room = center + half - right;
        let galley = line(&format!("·  {section}"), t.text, room);
        if galley.size().x > 0.0 && room > 0.0 {
            rect = rect.union(egui::Rect::from_min_size(egui::pos2(right, top), galley.size()));
            ui.painter().galley(egui::pos2(right, top), galley, t.text);
        }
    }
    let identity = Identity { name, file: doc.path.clone().unwrap_or_else(|| doc.name.clone()), section, unsaved: doc.dirty };
    let resp = ui.interact(rect, ui.id().with("margin-header-title"), egui::Sense::hover());
    look::tooltip_ui(&resp, |ui| identity.show(ui));
}

/// What the header title's tooltip says about the active document.
struct Identity {
    name: String,
    /// The file's path, or its name when it has no file yet.
    file: String,
    section: Option<String>,
    unsaved: bool,
}

impl Identity {
    fn show(self, ui: &mut egui::Ui) {
        let t = Tokens::get(ui.ctx());
        ui.set_max_width(TOOLTIP_WIDTH);
        ui.label(egui::RichText::new(self.name).color(t.text));
        ui.label(egui::RichText::new(self.file).color(t.text_muted).size(11.5));
        if let Some(section) = self.section {
            ui.label(egui::RichText::new(section).color(t.text_muted));
        }
        if self.unsaved {
            ui.label(egui::RichText::new("Unsaved changes").color(t.accent));
        }
    }
}

/// Whether `id` can run now: a PdfCraft command when the app says so, Margin's reader ids only
/// with a document open, the rest always.
fn entry_enabled(app: &PdfCraftApp, id: &str) -> bool {
    match commands::command(id) {
        Some(spec) => app.command_enabled(spec),
        None => app.active.is_some() || !["view.", "reader.", "annotation.", "note."].iter().any(|prefix| id.starts_with(prefix)),
    }
}

/// One command row: its icon, its title, the keys the keymap gives it now (so a rebinding shows
/// at once), off when it can't run. A click closes the menu and runs it as its keys would.
fn command_row(app: &mut PdfCraftApp, ui: &mut egui::Ui, title: &str, id: &str, icon: &str) {
    let keys = look::shortcut(app, id);
    let enabled = entry_enabled(app, id);
    let row = ui.add_enabled_ui(enabled, |ui| look::menu_row(ui, Some(icon), crate::i18n::t(title), keys.as_deref(), false)).inner;
    if row.clicked() {
        ui.close();
        let ctx = ui.ctx().clone();
        if !crate::margin::keys::run(app, &ctx, id, &Value::Null) {
            log::warn!("menu: nothing runs {id}");
        }
    }
}

fn document_menu(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    look::menu_body(ui);
    let t = Tokens::get(ui.ctx());
    if let Some(doc) = app.active_ids().and_then(|(_, id)| app.session.get(id)) {
        let name = doc.display_name();
        let label = ui.add(egui::Label::new(egui::RichText::new(&name).font(theme::regular(11.5)).color(t.text_faint)).truncate());
        label.on_hover_text(name);
    }
    menu_rows(app, ui, DOCUMENT_MENU);
}

/// The Library toolbar's menu button and its [`APP_MENU`].
pub(crate) fn app_menu_button(app: &mut PdfCraftApp, ui: &mut egui::Ui, size: f32) {
    let menu = look::icon_button(ui, "ellipsis", size, false, "Margin: open, recent, settings, shortcuts", None);
    egui::Popup::menu(&menu).show(|ui| {
        look::menu_body(ui);
        menu_rows(app, ui, APP_MENU);
    });
}

fn menu_rows(app: &mut PdfCraftApp, ui: &mut egui::Ui, rows: &[Entry]) {
    for entry in rows {
        match *entry {
            Separator => {
                ui.separator();
            }
            Export => {
                let has_doc = app.active.is_some();
                ui.add_enabled_ui(has_doc, |ui| {
                    look::submenu(ui, "file-output", crate::i18n::t("Export"), false, |ui| menu_rows(app, ui, EXPORT_MENU))
                });
            }
            Recent => recent_menu(app, ui),
            #[cfg(not(target_arch = "wasm32"))]
            Import => crate::margin::library::import_item(app, ui),
            #[cfg(target_arch = "wasm32")]
            Import => {}
            Command(title, id, icon) => command_row(app, ui, title, id, icon),
        }
    }
}

/// The most recent documents the Recent submenu lists.
const RECENT_ROWS: usize = 10;

/// Recent ▸: PdfCraft's recent files (deduplicated, missing ones already dropped at start), each
/// opened as the library opens a book (its tab if open, else the file, at its reading place). A
/// file gone since then says so and leaves the current document open. Rows are cut to the
/// menu's width; the path is in their tooltip.
fn recent_menu(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let recent: Vec<(String, String)> = app.recent.iter().take(RECENT_ROWS).map(|file| (file.name.clone(), file.path.clone())).collect();
    ui.add_enabled_ui(!recent.is_empty(), |ui| {
        look::submenu(ui, "clock", crate::i18n::t("Recent"), false, |ui| {
            for (name, path) in recent {
                if look::menu_row(ui, Some("file-text"), &name, None, false).on_hover_text(&path).clicked() {
                    ui.close();
                    #[cfg(not(target_arch = "wasm32"))]
                    crate::margin::library::open_book(app, &path);
                    #[cfg(target_arch = "wasm32")]
                    app.open_recent(&path);
                }
            }
        })
    });
}

/// The Display popover's body: page colour, layout and fit (`margin::display`).
fn display_popover(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    crate::margin::display::popover_contents(app, ui);
}
