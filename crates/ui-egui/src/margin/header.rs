//! The header over the page: sidebar toggle, view history and Tools on the left; the document's
//! title and current section in the middle; find, annotate, display, full screen and the
//! document menu on the right.

use egui::{Align, Layout};
use pdfcraft_engine::commands;

use crate::margin::look::{self, GAP_LARGE};
use crate::margin::{SidebarTab, current_section, motion, open_tab, outline_at};
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

pub const HEIGHT: f32 = 44.0;
const BUTTON: f32 = 30.0;
/// Space kept between the centred title and the button clusters.
const TITLE_GAP: f32 = 16.0;
/// Below this header width the Tools button drops its word, leaving the title more room.
const COMPACT_BELOW: f32 = 760.0;
/// The header title's tooltip wraps at this width, so a long path stays a readable block.
const TOOLTIP_WIDTH: f32 = 360.0;

/// The document menu, top to bottom; `None` is a separator, `Some("export")` the Export submenu,
/// `Some("recent")` the Recent submenu.
const DOCUMENT_MENU: &[Option<&str>] = &[
    Some("file.open"),
    Some("recent"),
    None,
    Some("file.save"),
    Some("file.save_as"),
    Some("export"),
    Some("print.dialog"),
    Some("file.properties"),
    None,
    Some("app.preferences"),
    Some("help.shortcuts"),
    None,
    Some("file.close"),
];

/// Export ▸: every registered export command.
const EXPORT_MENU: &[&str] = &["export.text", "export.docx", "export.html", "export.rtf", "export.image", "export.all_images"];

/// The header. While the active document is being searched, the find field takes the title's
/// place; returns true when the header has no room for it, so it needs its own row.
pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui) -> bool {
    let t = Tokens::get(ui.ctx());
    let mut needs_row = false;
    // egui draws the panel's bottom divider line.
    egui::Panel::top("margin_header")
        .exact_size(HEIGHT)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(GAP_LARGE as i8, 0)))
        .show(ui, |ui| {
            let full = ui.max_rect();
            let mut left_end = full.left();
            let mut right_start = full.right();
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                left_cluster(app, ui);
                left_end = ui.min_rect().right();
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    right_cluster(app, ui);
                    right_start = ui.min_rect().left();
                });
            });
            let finding = app.active.and_then(|i| app.views.get(i)).is_some_and(|v| v.find.is_some());
            let room = 2.0 * ((full.center().x - left_end).min(right_start - full.center().x) - TITLE_GAP);
            if !finding || !crate::margin::find::in_header(app, ui, full.center(), room) {
                needs_row = finding;
                title(app, ui, &t, full, left_end, right_start);
            }
        });
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
    let tools_open = open && app.margin_chrome.tab == SidebarTab::Tools;
    let compact = ui.max_rect().width() < COMPACT_BELOW;
    let tools = if compact {
        look::icon_button(ui, "wrench", BUTTON, tools_open, "Tools", None)
    } else {
        look::text_button(ui, "wrench", "Tools", BUTTON, tools_open, "All tools")
    };
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

/// Title and current section, centred on `full` and clipped to the space the buttons leave
/// (also the full-screen bar's, `margin::focus`). Hovering it shows the whole name, the file's
/// location and the section, which the clipped line may hide.
pub(crate) fn title(app: &PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, full: egui::Rect, left_end: f32, right_start: f32) {
    let center = full.center().x;
    let half = (center - left_end).min(right_start - center) - TITLE_GAP;
    if half <= 0.0 {
        return;
    }
    let mut job = egui::text::LayoutJob::default();
    let format = |size: f32, color| egui::TextFormat { font_id: theme::regular(size), color, ..Default::default() };
    let mut identity = None;
    match app.active_ids().and_then(|(i, id)| Some((app.views.get(i)?, app.session.get(id)?))) {
        Some((view, doc)) => {
            let name = doc.info.title.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| doc.display_name());
            job.append(&name, 0.0, format(12.5, t.text_muted));
            let section = current_section(&doc.info.outline, view.current).and_then(|p| outline_at(&doc.info.outline, &p).map(|i| i.title.trim().to_owned()));
            if let Some(section) = &section {
                job.append("  ·  ", 0.0, format(12.5, t.text_faint));
                job.append(section, 0.0, format(12.5, t.text));
            }
            if doc.dirty {
                job.append("  •", 0.0, format(12.5, t.accent));
            }
            identity = Some(Identity { name, file: doc.path.clone().unwrap_or_else(|| doc.name.clone()), section, unsaved: doc.dirty });
        }
        None => job.append("Margin", 0.0, format(12.5, t.text_muted)),
    }
    job.wrap = egui::text::TextWrapping::truncate_at_width(half * 2.0);
    let galley = ui.painter().layout_job(job);
    let pos = egui::pos2(center - galley.size().x / 2.0, full.center().y - galley.size().y / 2.0);
    let rect = egui::Rect::from_min_size(pos, galley.size());
    ui.painter().galley(pos, galley, t.text);
    if let Some(identity) = identity {
        let resp = ui.interact(rect, ui.id().with("margin-header-title"), egui::Sense::hover());
        look::tooltip_ui(&resp, |ui| identity.show(ui));
    }
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

/// One registered command as a menu row: its live label, shortcut and enabled state.
pub(crate) fn command_item(app: &mut PdfCraftApp, ui: &mut egui::Ui, id: &str) {
    let Some(spec) = commands::command(id) else { return };
    let mac = cfg!(target_os = "macos");
    let label = commands::current_label(spec, &app.session, app.active_ids().map(|(_, id)| id));
    let label = crate::i18n::menu_label(spec.id, &label);
    let shortcut = spec.shortcut.map(|s| s.label(mac)).unwrap_or_default();
    let enabled = app.command_enabled(spec);
    let row = egui::Button::image_and_text(icons::image(spec.icon, 14.0, Tokens::get(ui.ctx()).icon), label).shortcut_text(shortcut);
    if ui.add_enabled(enabled, row).clicked() {
        ui.close();
        app.execute(spec.id);
    }
}

fn document_menu(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.set_min_width(240.0);
    if let Some(doc) = app.active_ids().and_then(|(_, id)| app.session.get(id)) {
        ui.label(egui::RichText::new(doc.display_name()).font(theme::regular(11.5)).color(t.text_faint));
    }
    for entry in DOCUMENT_MENU {
        match entry {
            None => {
                ui.separator();
            }
            Some("export") => {
                let has_doc = app.active.is_some();
                ui.add_enabled_ui(has_doc, |ui| {
                    ui.menu_button("Export", |ui| {
                        for id in EXPORT_MENU {
                            command_item(app, ui, id);
                        }
                    })
                });
            }
            Some("recent") => recent_menu(app, ui),
            Some(id) => command_item(app, ui, id),
        }
    }
}

/// The most recent documents the Recent submenu lists.
const RECENT_ROWS: usize = 10;

/// Recent ▸: PdfCraft's recent files (deduplicated, missing ones already dropped at start), each
/// opened as the library opens a book (its tab if open, else the file, at its reading place). A
/// file gone since then says so and leaves the current document open.
fn recent_menu(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let recent: Vec<(String, String)> = app.recent.iter().take(RECENT_ROWS).map(|file| (file.name.clone(), file.path.clone())).collect();
    ui.add_enabled_ui(!recent.is_empty(), |ui| {
        ui.menu_button("Recent", |ui| {
            for (name, path) in recent {
                if look::tooltip(ui.button(&name), &path, None).clicked() {
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
