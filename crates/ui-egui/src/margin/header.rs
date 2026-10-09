//! The header over the page: sidebar toggle, view history and Tools on the left; the document's
//! title and current section in the middle; find, annotate, display, full screen and the
//! document menu on the right.

use egui::{Align, Layout, Stroke};
use pdfcraft_engine::commands;

use crate::canvas::{Fit, PageLayout};
use crate::margin::{SidebarTab, current_section, open_tab, outline_at};
use crate::theme::{self, ThemePreference, Tokens};
use crate::{PdfCraftApp, icons};

pub const HEIGHT: f32 = 44.0;
const BUTTON: f32 = 30.0;
/// Space kept between the centred title and the button clusters.
const TITLE_GAP: f32 = 16.0;

/// The document menu, top to bottom; `None` is a separator, `Some("export")` the Export submenu.
const DOCUMENT_MENU: &[Option<&str>] = &[
    Some("file.open"),
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

pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::top("margin_header")
        .exact_size(HEIGHT)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(10, 0)).stroke(Stroke::new(1.0, t.divider)))
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
            title(app, ui, &t, full, left_end, right_start);
        });
}

fn left_cluster(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let has_doc = app.active.is_some();
    let open = app.margin_chrome.sidebar_open;
    if ui.add_enabled_ui(has_doc, |ui| icons::button(ui, "panel-left", BUTTON, open && has_doc, "Sidebar")).inner.clicked() {
        app.margin_chrome.sidebar_open = !open;
    }
    let Some(view) = app.active.and_then(|i| app.views.get_mut(i)) else { return };
    let (back, forward) = (!view.back.is_empty(), !view.forward.is_empty());
    if ui.add_enabled_ui(back, |ui| icons::button(ui, "chevron-left", BUTTON, false, "Previous view")).inner.clicked() {
        view.view_history(false);
    }
    if ui.add_enabled_ui(forward, |ui| icons::button(ui, "chevron-right", BUTTON, false, "Next view")).inner.clicked() {
        view.view_history(true);
    }
    ui.add_space(4.0);
    let tools_open = open && app.margin_chrome.tab == SidebarTab::Tools;
    if crate::widgets::ghost_button(ui, "wrench", "Tools").on_hover_text("All tools").clicked() {
        if tools_open {
            app.margin_chrome.sidebar_open = false;
        } else {
            open_tab(app, SidebarTab::Tools);
        }
    }
}

fn right_cluster(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let menu = icons::button(ui, "ellipsis", BUTTON, false, "Document: save, export, print, settings");
    egui::Popup::menu(&menu).show(|ui| document_menu(app, ui));
    let Some(index) = app.active else { return };
    if icons::button(ui, "maximize", BUTTON, app.full_screen, &with_shortcut("Full screen", "view.full_screen")).clicked() {
        app.execute("view.full_screen");
    }
    let display = icons::button(ui, "contrast", BUTTON, false, "Page colour and layout");
    egui::Popup::menu(&display).show(|ui| display_popover(app, ui));
    let rail = app.margin_chrome.rail_open;
    if icons::button(ui, "pen-tool", BUTTON, rail, "Annotate").clicked() {
        app.margin_chrome.rail_open = !rail;
    }
    let finding = app.views.get(index).is_some_and(|v| v.find.is_some());
    if icons::button(ui, "search", BUTTON, finding, &with_shortcut("Find in document", "edit.find")).clicked() {
        match app.views.get_mut(index) {
            Some(view) if finding => view.find = None,
            _ => {
                app.execute("edit.find");
            }
        }
    }
}

/// Title and current section, centred on the header and clipped to the space the buttons leave.
fn title(app: &PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, full: egui::Rect, left_end: f32, right_start: f32) {
    let center = full.center().x;
    let half = (center - left_end).min(right_start - center) - TITLE_GAP;
    if half <= 0.0 {
        return;
    }
    let mut job = egui::text::LayoutJob::default();
    let format = |size: f32, color| egui::TextFormat { font_id: theme::regular(size), color, ..Default::default() };
    match app.active_ids().and_then(|(i, id)| Some((app.views.get(i)?, app.session.get(id)?))) {
        Some((view, doc)) => {
            let name = doc.info.title.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| doc.display_name());
            job.append(&name, 0.0, format(12.5, t.text_muted));
            let section = current_section(&doc.info.outline, view.current).and_then(|p| outline_at(&doc.info.outline, &p).map(|i| i.title.clone()));
            if let Some(section) = section {
                job.append("  ·  ", 0.0, format(12.5, t.text_faint));
                job.append(section.trim(), 0.0, format(12.5, t.text));
            }
            if doc.dirty {
                job.append("  •", 0.0, format(12.5, t.accent));
            }
        }
        None => job.append("Margin", 0.0, format(12.5, t.text_muted)),
    }
    job.wrap = egui::text::TextWrapping::truncate_at_width(half * 2.0);
    let galley = ui.painter().layout_job(job);
    let pos = egui::pos2(center - galley.size().x / 2.0, full.center().y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, t.text);
}

/// A command's label with its shortcut, for tooltips.
fn with_shortcut(label: &str, id: &str) -> String {
    let mac = cfg!(target_os = "macos");
    match commands::command(id).and_then(|c| c.shortcut) {
        Some(s) => format!("{label}  ({})", s.label(mac)),
        None => label.to_string(),
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
            Some(id) => command_item(app, ui, id),
        }
    }
}

/// The Display popover's body: page layout, fit and theme from PdfCraft's commands. The page
/// colour controls (R1) take this popover over.
fn display_popover(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.set_min_width(220.0);
    let mut command = None;
    if let Some(view) = app.active.and_then(|i| app.views.get_mut(i)) {
        ui.label(egui::RichText::new("Pages").font(theme::medium(11.5)).color(t.text_faint));
        for layout in PageLayout::ORDER {
            if ui.radio(view.layout == layout, crate::i18n::t(layout.label())).clicked() {
                command = Some(layout.command());
            }
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Fit").font(theme::medium(11.5)).color(t.text_faint));
        for (fit, label) in [(Fit::Width, "Width"), (Fit::Page, "Page")] {
            if ui.radio(view.fit == fit, label).clicked() {
                view.set_fit(fit);
            }
        }
        ui.add_space(4.0);
    }
    ui.label(egui::RichText::new("Theme").font(theme::medium(11.5)).color(t.text_faint));
    for (preference, id, label) in [
        (ThemePreference::Dark, "view.theme.dark", "Dark"),
        (ThemePreference::Light, "view.theme.light", "Light"),
        (ThemePreference::System, "view.theme.system", "Follow the system"),
    ] {
        if ui.radio(app.theme_preference == preference, label).clicked() {
            command = Some(id);
        }
    }
    if let Some(id) = command {
        app.execute(id);
    }
}
