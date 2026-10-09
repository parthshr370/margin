//! The keyboard sheet (`?`, `help.shortcuts` in the Margin flavor): every binding of the compiled
//! keymap, grouped by context, generated from the table the dispatcher uses, so it is never out of
//! date. Below them, PdfCraft's shortcuts that still apply because the keymap leaves them alone.
//! Also the `app.key_context` window and the pending-chord hint.

use egui::{Align2, RichText};
use margin_config::resolve::Outcome;
use margin_config::{CompiledBinding, CompiledKeymap, ContextNode, Registry, depth_of};
use serde_json::Value;

use crate::PdfCraftApp;
use crate::theme::{self, Tokens};

/// The sheet's state while open.
#[derive(Default)]
pub(crate) struct Sheet {
    filter: String,
}

/// One row: keys and what they do.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub(crate) keys: String,
    pub(crate) title: String,
}

/// The rows of one context.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Section {
    pub(crate) context: String,
    pub(crate) rows: Vec<Row>,
}

/// How a binding without a context is headed.
const EVERYWHERE: &str = "Everywhere";

/// The heading of PdfCraft's shortcuts the keymap leaves alone.
const PDFCRAFT_SECTION: &str = "PdfCraft shortcuts";

/// The effective bindings, grouped by context in the order the keymap first names it. A later
/// binding of the same keys in the same context replaces the earlier one in place; an unbind
/// removes it.
pub(crate) fn sections(table: &CompiledKeymap, registry: &Registry, mac: bool) -> Vec<Section> {
    let mut effective: Vec<&CompiledBinding> = Vec::new();
    for binding in &table.bindings {
        let same = effective.iter().position(|seen| seen.context_text == binding.context_text && seen.keys == binding.keys);
        match (same, binding.command.is_some()) {
            (Some(at), true) => {
                if let Some(slot) = effective.get_mut(at) {
                    *slot = binding;
                }
            }
            (Some(at), false) => {
                effective.remove(at);
            }
            (None, true) => effective.push(binding),
            (None, false) => {}
        }
    }
    let mut sections: Vec<Section> = Vec::new();
    for binding in effective {
        let Some(command) = binding.command.as_deref() else { continue };
        let heading = binding.context_text.as_deref().unwrap_or(EVERYWHERE);
        let row = Row { keys: pretty_keys(&binding.keys, mac), title: title(registry, command, &binding.args) };
        match sections.iter_mut().find(|section| section.context == heading) {
            Some(section) => section.rows.push(row),
            None => sections.push(Section { context: heading.to_owned(), rows: vec![row] }),
        }
    }
    sections
}

/// A command's title, with its arguments when it has some (`Zoom (100)`).
fn title(registry: &Registry, command: &str, args: &Value) -> String {
    let name = registry.title(command).unwrap_or(command).trim_end_matches('…');
    let shown: Vec<String> = match args {
        Value::Object(map) => map.values().map(|value| value.as_str().map_or_else(|| value.to_string(), str::to_owned)).collect(),
        Value::Null => Vec::new(),
        other => vec![other.to_string()],
    };
    if shown.is_empty() { name.to_owned() } else { format!("{name} ({})", shown.join(", ")) }
}

/// Canonical keystrokes for people: `Ctrl+Shift+S`, or `⇧⌘S` on macOS; chords spaced.
pub(crate) fn pretty_keys(keys: &[String], mac: bool) -> String {
    keys.iter().map(|stroke| pretty_stroke(stroke, mac)).collect::<Vec<_>>().join(" ")
}

fn pretty_stroke(stroke: &str, mac: bool) -> String {
    let mut modifiers = Vec::new();
    let mut key = stroke;
    while let Some((modifier, rest)) = key.split_once('-').filter(|(_, rest)| !rest.is_empty()) {
        let shown = match (modifier, mac) {
            ("ctrl", true) => "⌃",
            ("ctrl", false) => "Ctrl",
            ("alt", true) => "⌥",
            ("alt", false) => "Alt",
            ("shift", true) => "⇧",
            ("shift", false) => "Shift",
            ("super", true) => "⌘",
            ("super", false) => "Super",
            _ => break,
        };
        modifiers.push(shown);
        key = rest;
    }
    let name = match key {
        "space" => "Space".to_owned(),
        "enter" => "Enter".to_owned(),
        "escape" => "Esc".to_owned(),
        "tab" => "Tab".to_owned(),
        "backspace" => "Backspace".to_owned(),
        "delete" => "Delete".to_owned(),
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        "home" => "Home".to_owned(),
        "end" => "End".to_owned(),
        "pageup" => "Page Up".to_owned(),
        "pagedown" => "Page Down".to_owned(),
        "menu" => "Menu".to_owned(),
        other => other.to_uppercase(),
    };
    if mac { format!("{}{name}", modifiers.concat()) } else { modifiers.into_iter().chain([name.as_str()]).collect::<Vec<_>>().join("+") }
}

fn is_mac() -> bool {
    cfg!(target_os = "macos")
}

/// Every row the sheet shows: the keymap's sections, then PdfCraft's shortcuts that still apply.
fn all_sections(app: &PdfCraftApp) -> Vec<Section> {
    let mac = is_mac();
    let keys = &app.margin_keys;
    let mut all = match (keys.keymap(), keys.registry()) {
        (Some(table), Some(registry)) => sections(table, registry, mac),
        _ => Vec::new(),
    };
    let fallback: Vec<Row> = pdfcraft_engine::commands::COMMANDS
        .iter()
        .filter(|spec| keys.falls_back(spec))
        .filter_map(|spec| spec.shortcut.map(|shortcut| Row { keys: shortcut.label(mac), title: spec.label.trim_end_matches('…').to_owned() }))
        .collect();
    if !fallback.is_empty() {
        all.push(Section { context: PDFCRAFT_SECTION.to_owned(), rows: fallback });
    }
    all
}

fn matches_filter(filter: &str, section: &Section, row: &Row) -> bool {
    let needle = filter.trim().to_lowercase();
    needle.is_empty() || [&section.context, &row.keys, &row.title].iter().any(|text| text.to_lowercase().contains(&needle))
}

/// The sheet, a modal like PdfCraft's dialogs.
pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    let sections = all_sections(app);
    let keymap_path = app.margin_keys.config_dir().map(|dir| dir.join(margin_config::KEYMAP_FILE).display().to_string());
    let Some(sheet) = app.margin_keys.sheet.as_mut() else { return };
    let mut close = false;
    let mut key_context = false;
    let modal = egui::Modal::new(egui::Id::new("margin-keysheet")).show(ctx, |ui| {
        ui.set_width(620.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Keyboard shortcuts").font(theme::semibold(18.0)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(egui::TextEdit::singleline(&mut sheet.filter).hint_text("Filter").desired_width(180.0));
            });
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical().max_height(480.0).show(ui, |ui| {
            for section in &sections {
                let rows: Vec<&Row> = section.rows.iter().filter(|row| matches_filter(&sheet.filter, section, row)).collect();
                if rows.is_empty() {
                    continue;
                }
                ui.add_space(6.0);
                ui.label(RichText::new(&section.context).font(theme::semibold(13.0)).color(t.text_muted));
                egui::Grid::new(("margin-keysheet", &section.context)).num_columns(2).spacing([24.0, 5.0]).min_col_width(150.0).show(ui, |ui| {
                    for row in rows {
                        ui.label(RichText::new(&row.keys).font(egui::FontId::monospace(12.5)));
                        ui.label(&row.title);
                        ui.end_row();
                    }
                });
            }
        });
        ui.add_space(8.0);
        if let Some(path) = &keymap_path {
            ui.label(RichText::new(format!("Change them in {path}")).color(t.text_muted).small());
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::widgets::pill_button(ui, "Close", true).clicked() {
                close = true;
            }
            if crate::widgets::pill_button(ui, "Key context…", false).clicked() {
                key_context = true;
            }
        });
    });
    if close || modal.should_close() {
        app.margin_keys.sheet = None;
    }
    if key_context {
        app.margin_keys.sheet = None;
        app.margin_keys.key_context_open = true;
    }
}

/// One stack node as `Name attr=value flag`.
fn node_text(node: &ContextNode) -> String {
    let mut text = node.name.clone();
    for (attr, value) in &node.attrs {
        text.push_str(&format!(" {attr}={value}"));
    }
    for flag in &node.flags {
        text.push(' ');
        text.push_str(flag);
    }
    text
}

fn stack_text(stack: &[ContextNode]) -> String {
    stack.iter().map(node_text).collect::<Vec<_>>().join(" > ")
}

/// `app.key_context`: the live context stack, the last key and every binding of it with its
/// depth in the stack it was pressed in (the deepest that holds wins, then the later file), and
/// the problems of the last load. A window, not a modal, so keys keep their normal context.
pub(crate) fn key_context(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    let mac = is_mac();
    let live = stack_text(&super::keys::context_stack(app, ctx));
    let keys = &app.margin_keys;
    let pending = pretty_keys(keys.pending(), mac);
    let mut open = keys.key_context_open;
    egui::Window::new("Key context").open(&mut open).default_width(560.0).resizable(true).show(ctx, |ui| {
        ui.label(RichText::new("Context").font(theme::semibold(13.0)).color(t.text_muted));
        ui.label(RichText::new(&live).font(egui::FontId::monospace(12.5)));
        if !pending.is_empty() {
            ui.label(format!("Waiting for the rest of the chord: {pending}"));
        }
        ui.add_space(6.0);
        ui.label(RichText::new("Last key").font(theme::semibold(13.0)).color(t.text_muted));
        match &keys.last {
            None => {
                ui.label("Press a key.");
            }
            Some(last) => {
                let outcome = match &last.outcome {
                    Outcome::Command { command, args } if args.is_null() => format!("runs {command}"),
                    Outcome::Command { command, args } => format!("runs {command} {args}"),
                    Outcome::Pending => "waits for the next key of a chord".to_owned(),
                    Outcome::NoMatch => "no binding; the key goes on to the widgets".to_owned(),
                };
                ui.label(RichText::new(format!("{}  {outcome}", pretty_keys(&last.typed, mac))).font(egui::FontId::monospace(12.5)));
                ui.label(RichText::new(format!("in {}", stack_text(&last.stack))).color(t.text_muted).small());
                if let Some(table) = keys.keymap() {
                    egui::Grid::new("margin-key-context").num_columns(4).spacing([14.0, 4.0]).striped(true).show(ui, |ui| {
                        for header in ["Context", "Runs", "Depth", "Where"] {
                            ui.label(RichText::new(header).strong());
                        }
                        ui.end_row();
                        for binding in table.bindings.iter().filter(|binding| binding.keys.starts_with(&last.typed)) {
                            ui.label(binding.context_text.as_deref().unwrap_or(EVERYWHERE));
                            ui.label(binding.command.as_deref().unwrap_or("(unbound)"));
                            let depth = depth_of(binding.context.as_ref(), &last.stack);
                            ui.label(depth.map_or_else(|| "doesn't hold".to_owned(), |depth| depth.to_string()));
                            ui.label(format!("{}:{}", binding.source.file, binding.source.line));
                            ui.end_row();
                        }
                    });
                }
            }
        }
        let problems = keys.problems();
        if !problems.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Problems in the config files").font(theme::semibold(13.0)).color(t.text_muted));
            for problem in problems {
                ui.label(RichText::new(problem.to_string()).small());
            }
        }
    });
    app.margin_keys.key_context_open = open;
}

/// The keys of a chord in progress, at the bottom of the window (`keys.show_pending`).
pub(crate) fn pending_chord(app: &PdfCraftApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    let text = format!("{} …", pretty_keys(app.margin_keys.pending(), is_mac()));
    egui::Area::new(egui::Id::new("margin-pending-chord")).order(egui::Order::Foreground).anchor(Align2::CENTER_BOTTOM, [0.0, -48.0]).show(
        ctx,
        |ui| {
            egui::Frame::new()
                .fill(t.card)
                .stroke(egui::Stroke::new(1.0, t.border))
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(10, 5))
                .show(ui, |ui| {
                    ui.label(RichText::new(text).font(egui::FontId::monospace(13.0)).color(t.text));
                });
        },
    );
}

#[cfg(test)]
mod tests {
    use margin_config::{CompileOptions, KeymapSource, Os, compile};

    use super::*;

    fn table(user: &str) -> (CompiledKeymap, Registry) {
        let registry = Registry::load().unwrap();
        let options = CompileOptions { os: Os::Linux, chord_timeout_ms: 1000, annotate_letters: true };
        let sources = [
            KeymapSource { file: margin_config::MARGIN_KEYMAP_FILE, text: margin_config::MARGIN_KEYMAP },
            KeymapSource { file: "keymap.jsonc", text: user },
        ];
        (compile(&registry, &sources, &options).keymap, registry)
    }

    #[test]
    fn keys_read_like_menus() {
        assert_eq!(pretty_keys(&["ctrl-shift-s".to_owned()], false), "Ctrl+Shift+S");
        assert_eq!(pretty_keys(&["shift-super-s".to_owned()], true), "⇧⌘S");
        assert_eq!(pretty_keys(&["ctrl--".to_owned()], false), "Ctrl+-");
        assert_eq!(pretty_keys(&["g".to_owned(), "g".to_owned()], false), "G G");
        assert_eq!(pretty_keys(&["shift-space".to_owned()], false), "Shift+Space");
    }

    /// Every built-in binding is listed once, under its context, in file order.
    #[test]
    fn the_sheet_lists_every_binding_by_context() {
        let (keymap, registry) = table("[]");
        let sections = sections(&keymap, &registry, false);
        let contexts: Vec<&str> = sections.iter().map(|s| s.context.as_str()).collect();
        assert_eq!(contexts, [EVERYWHERE, "Reader", "Find > TextInput", "Selection", "AnnotateMode"]);
        let rows: usize = sections.iter().map(|s| s.rows.len()).sum();
        assert_eq!(rows, keymap.bindings.len());
        let everywhere = &sections[0].rows;
        assert_eq!(everywhere[0], Row { keys: "Ctrl+K".to_owned(), title: "Find tools and commands".to_owned() });
        let zoom = sections[1].rows.iter().find(|row| row.keys == "Ctrl+0").unwrap();
        assert_eq!(zoom.title, "Zoom (100)");
    }

    /// A user's rebinding replaces the row; an unbind removes it.
    #[test]
    fn user_bindings_replace_and_remove_rows() {
        let (keymap, registry) = table("[{ \"bindings\": { \"ctrl-p\": \"view.palette\", \"ctrl-s\": null } }]");
        let sections = sections(&keymap, &registry, false);
        let everywhere = &sections[0].rows;
        let ctrl_p: Vec<&Row> = everywhere.iter().filter(|row| row.keys == "Ctrl+P").collect();
        assert_eq!(ctrl_p.len(), 1);
        assert_eq!(ctrl_p[0].title, "Find tools and commands");
        assert!(!everywhere.iter().any(|row| row.keys == "Ctrl+S"));
    }
}
