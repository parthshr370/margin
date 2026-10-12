//! The Settings window's Library and Notes sections: library folders, the Obsidian vault, the
//! highlight export format, how existing notes change, and the note templates. Every control
//! writes `settings.jsonc` (`library.folders`, `notes.*`) through the comment-keeping edit, and
//! the setting applies at once. Uses the window's row layout from its parent module.

use margin_config::settings::{ExportTarget, NoteUpdateMode, Notes as NotesConfig};

use super::*;
use crate::margin::{obsidian, welcome};

/// The Save button beside a template field.
const TEMPLATE_SAVE_WIDTH: f32 = 56.0;
/// A template field never gets narrower than this.
const TEMPLATE_MIN_WIDTH: f32 = 100.0;

/// Draws both sections (in the Settings window's body).
pub(super) fn sections(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    section(ui, t, "Library");
    let text = RowText {
        id: "library-folders",
        name: "Library folders",
        description: Some("Margin scans them, with their subfolders, for PDFs."),
        first: true,
    };
    row(ui, t, text, |ui, _| folders_control(app, ui));

    section(ui, t, "Notes");
    let text =
        RowText { id: "notes-vault", name: "Obsidian vault", description: Some("Reading notes, highlights and snapshots go here."), first: true };
    row(ui, t, text, |ui, _| vault_control(app, ui));
    let text =
        RowText { id: "notes-export-target", name: "Export format", description: Some("What Copy all and Export highlights write."), first: false };
    row(ui, t, text, |ui, name| export_target_dropdown(app, ui, t, name));
    let text = RowText {
        id: "notes-update-mode",
        name: "Existing notes",
        description: Some("Update rewrites Margin's own highlight blocks; Append only adds new ones."),
        first: false,
    };
    row(ui, t, text, |ui, name| update_mode_dropdown(app, ui, t, name));
    for setting in NotesTextSetting::ALL {
        text_setting(app, ui, t, setting);
    }
}

/// How many folders there are, with Add and a Remove menu (right to left: the row's layout).
/// Remove is always there, off with no folders, so the row never reflows when one is added.
fn folders_control(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let folders = app.margin_keys.settings().library.folders.clone();
    if look::Button::secondary(ButtonSize::Standard, "Add folder…").icon("folder-plus").show(ui).clicked() {
        welcome::add_library_folder(app);
    }
    let remove =
        ui.add_enabled_ui(!folders.is_empty(), |ui| look::Button::secondary(ButtonSize::Standard, "Remove").trailing("chevron-down").show(ui)).inner;
    egui::Popup::menu(&remove).show(|ui| {
        look::menu_body(ui);
        for folder in &folders {
            if look::menu_row(ui, Some("folder-open"), &crate::folders_ui::folder_name(folder), None, false).on_hover_text(folder.as_str()).clicked()
            {
                welcome::remove_folder(app, folder);
            }
        }
    });
    let summary = match folders.len() {
        0 => "None yet".to_owned(),
        1 => "1 folder".to_owned(),
        n => format!("{n} folders"),
    };
    ui.add(egui::Label::new(summary).truncate()).on_hover_text(folders.join("\n"));
}

/// The vault's folder name (cut to what the slot leaves), Choose or Change (one width), and
/// Disconnect, always there and off without a vault, so choosing a vault never reflows the row.
fn vault_control(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let vault = app.notes.config.vault.clone();
    if ui.add_enabled_ui(vault.is_some(), |ui| look::Button::secondary(ButtonSize::Standard, "Disconnect").show(ui)).inner.clicked() {
        welcome::disconnect_vault(app);
    }
    let choose = look::Button::secondary(ButtonSize::Standard, if vault.is_some() { "Change vault…" } else { "Choose vault…" }).icon("folder-open");
    let width = choose.widest(ui, &["Change vault…", "Choose vault…"]);
    if choose.fixed_width(width).show(ui).clicked() {
        welcome::pick_vault(app);
    }
    let shown =
        vault.as_ref().map_or_else(|| "Not set".to_owned(), |path| path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned());
    let tip = vault.as_ref().map_or_else(|| "No vault selected".to_owned(), |path| path.display().to_string());
    ui.add(egui::Label::new(shown).truncate()).on_hover_text(tip);
}

fn export_target_name(target: ExportTarget) -> &'static str {
    match target {
        ExportTarget::Obsidian => "Obsidian",
        ExportTarget::Markdown => "Markdown",
        ExportTarget::Logseq => "Logseq",
        ExportTarget::ReadwiseCsv => "Readwise CSV",
        ExportTarget::Json => "JSON",
    }
}

fn export_target_dropdown(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, name: &str) {
    field_style(ui, t);
    let current = app.notes.config.export_target;
    let mut selected = current;
    let chevron = t.icon;
    let dropdown = egui::ComboBox::from_id_salt("notes-export-target")
        .width(DROPDOWN_WIDTH)
        .icon(move |ui, rect, _, _| crate::icons::paint(ui, rect, "chevron-down", CHEVRON_SIZE, chevron))
        .selected_text(RichText::new(export_target_name(current)).font(theme::regular(FIELD_TEXT)).color(t.text))
        .show_ui(ui, |ui| {
            for target in [ExportTarget::Obsidian, ExportTarget::Markdown, ExportTarget::Logseq, ExportTarget::ReadwiseCsv, ExportTarget::Json] {
                if ui.selectable_value(&mut selected, target, export_target_name(target)).clicked() {
                    ui.close();
                }
            }
        });
    dropdown.response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, ui.is_enabled(), name));
    if selected != current {
        let mut config = app.notes.config.clone();
        config.export_target = selected;
        save_notes(app, config);
    }
}

fn update_mode_name(mode: NoteUpdateMode) -> &'static str {
    match mode {
        NoteUpdateMode::Update => "Update",
        NoteUpdateMode::Append => "Append",
    }
}

fn update_mode_dropdown(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, name: &str) {
    field_style(ui, t);
    let current = app.notes.config.update_mode;
    let mut selected = current;
    let chevron = t.icon;
    let dropdown = egui::ComboBox::from_id_salt("notes-update-mode")
        .width(DROPDOWN_WIDTH)
        .icon(move |ui, rect, _, _| crate::icons::paint(ui, rect, "chevron-down", CHEVRON_SIZE, chevron))
        .selected_text(RichText::new(update_mode_name(current)).font(theme::regular(FIELD_TEXT)).color(t.text))
        .show_ui(ui, |ui| {
            for mode in [NoteUpdateMode::Update, NoteUpdateMode::Append] {
                if ui.selectable_value(&mut selected, mode, update_mode_name(mode)).clicked() {
                    ui.close();
                }
            }
        });
    dropdown.response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, ui.is_enabled(), name));
    if selected != current {
        let mut config = app.notes.config.clone();
        config.update_mode = selected;
        save_notes(app, config);
    }
}

/// The notes settings edited as text, each with a field and a Save button.
#[derive(Clone, Copy)]
enum NotesTextSetting {
    NoteTemplate,
    FrontMatterTemplate,
    HeaderTemplate,
    HighlightTemplate,
    SnapshotFolder,
}

impl NotesTextSetting {
    const ALL: [Self; 5] = [Self::NoteTemplate, Self::FrontMatterTemplate, Self::HeaderTemplate, Self::HighlightTemplate, Self::SnapshotFolder];

    fn id(self) -> &'static str {
        match self {
            Self::NoteTemplate => "note-template",
            Self::FrontMatterTemplate => "front-matter-template",
            Self::HeaderTemplate => "header-template",
            Self::HighlightTemplate => "highlight-template",
            Self::SnapshotFolder => "snapshot-folder",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::NoteTemplate => "Reading note path",
            Self::FrontMatterTemplate => "Front matter template",
            Self::HeaderTemplate => "Note header template",
            Self::HighlightTemplate => "Highlight template",
            Self::SnapshotFolder => "Snapshot folder",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::NoteTemplate => "Inside the vault; {{title-slug}}, {{authors}}, {{year}}, {{citekey}} and the like are filled in.",
            Self::FrontMatterTemplate => "YAML at the top of a new note; {{source}} is the PDF.",
            Self::HeaderTemplate => "Text under the front matter of a new note.",
            Self::HighlightTemplate => "One highlight; {{callout}} is the PDF++ callout, {{text}} and {{note}} its parts.",
            Self::SnapshotFolder => "Folder inside the vault for area snapshots.",
        }
    }

    /// Lines the field shows.
    fn rows(self) -> usize {
        match self {
            Self::NoteTemplate | Self::SnapshotFolder => 1,
            Self::HeaderTemplate | Self::HighlightTemplate => 2,
            Self::FrontMatterTemplate => 4,
        }
    }

    fn value(self, config: &NotesConfig) -> &str {
        match self {
            Self::NoteTemplate => &config.note_template,
            Self::FrontMatterTemplate => &config.front_matter_template,
            Self::HeaderTemplate => &config.header_template,
            Self::HighlightTemplate => &config.highlight_template,
            Self::SnapshotFolder => &config.snapshot_folder,
        }
    }

    /// `config` with `value` set, or why `value` can't be used (a path leaving the vault).
    fn with(self, mut config: NotesConfig, value: String) -> Result<NotesConfig, String> {
        match self {
            Self::NoteTemplate => {
                obsidian::note_path(&value, "title")?;
                config.note_template = value;
            }
            Self::FrontMatterTemplate => config.front_matter_template = value,
            Self::HeaderTemplate => config.header_template = value,
            Self::HighlightTemplate => config.highlight_template = value,
            Self::SnapshotFolder => {
                obsidian::note_path(&format!("{}/snapshot.md", value.trim_matches('/')), "")?;
                config.snapshot_folder = value;
            }
        }
        Ok(config)
    }
}

/// What a text field holds while it is being edited, and the saved value it started from (a
/// change on disk replaces the draft).
#[derive(Clone)]
struct TemplateDraft {
    source: String,
    text: String,
}

/// Where `setting`'s draft lives in egui's memory: one id for the field and for Done's save.
fn template_draft_id(setting: NotesTextSetting) -> egui::Id {
    egui::Id::new(("margin-notes-template-draft", setting.id()))
}

/// Done in the Settings window: saves every template field with unsaved text, as its own Save
/// would, in one write. A draft that can't be used (a path leaving the vault) is reported and
/// left out; the others are still saved.
pub(super) fn save_drafts(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let mut config = app.notes.config.clone();
    let mut changed = false;
    for setting in NotesTextSetting::ALL {
        let current = setting.value(&app.notes.config).to_owned();
        let draft = ctx.data_mut(|memory| {
            let draft = memory.get_temp::<TemplateDraft>(template_draft_id(setting));
            memory.remove::<TemplateDraft>(template_draft_id(setting));
            draft
        });
        let Some(draft) = draft.filter(|draft| draft.source == current && draft.text != current) else { continue };
        match setting.with(config.clone(), draft.text) {
            Ok(next) => {
                config = next;
                changed = true;
            }
            Err(error) => app.notify(format!("{}: {error}", setting.name())),
        }
    }
    if changed {
        save_notes(app, config);
    }
}

fn text_setting(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, setting: NotesTextSetting) {
    let current = setting.value(&app.notes.config).to_owned();
    let draft_id = template_draft_id(setting);
    let mut draft = ui
        .ctx()
        .data(|memory| memory.get_temp::<TemplateDraft>(draft_id))
        .filter(|draft| draft.source == current)
        .map_or_else(|| current.clone(), |draft| draft.text);

    // A divider and padding like every other setting row.
    let top = ui.cursor().top();
    ui.painter().hline(ui.max_rect().x_range(), top + look::EDGE / 2.0, egui::Stroke::new(look::EDGE, t.divider));
    ui.add_space(ROW_PAD_Y);
    ui.label(RichText::new(setting.name()).font(theme::medium(NAME_SIZE)).color(t.text));
    ui.add_space(NAME_GAP);
    ui.label(RichText::new(setting.description()).font(theme::regular(DESCRIPTION_SIZE)).color(t.text_muted));
    ui.add_space(NAME_GAP);
    let unsaved = draft != current;
    // Save on the right, at the field's top, then the field in the rest: a field that scrolls
    // past its rows instead of growing, so typing never moves the rows below or the button.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        ui.spacing_mut().item_spacing.x = look::GAP;
        let save_label = if unsaved { "Save •" } else { "Save" };
        let save = look::Button::secondary(ButtonSize::Standard, save_label).name("Save").fixed_width(TEMPLATE_SAVE_WIDTH);
        let saved = ui.add_enabled_ui(unsaved, |ui| save.show(ui)).inner.clicked();
        let input_id = ui.make_persistent_id(("margin-notes-template-field", setting.id()));
        let editor_width = ui.available_width().max(TEMPLATE_MIN_WIDTH);
        let rows = setting.rows();
        let line = ui.fonts_mut(|fonts| fonts.row_height(&theme::regular(FIELD_TEXT)));
        let height = if rows == 1 { CONTROL_HEIGHT } else { rows as f32 * line + 2.0 * look::GAP };
        look::field_well(ui, input_id, vec2(editor_width, height), |ui| {
            egui::ScrollArea::vertical().id_salt(input_id.with("scroll")).max_height(height).auto_shrink([false, false]).show(ui, |ui| {
                let editor =
                    if rows == 1 { egui::TextEdit::singleline(&mut draft) } else { egui::TextEdit::multiline(&mut draft).desired_rows(rows) };
                let editor = editor
                    .id(input_id)
                    .frame(egui::Frame::NONE)
                    .margin(Vec2::ZERO)
                    .font(theme::regular(FIELD_TEXT))
                    .desired_width(ui.available_width());
                let field = ui.add(editor);
                ui.ctx().accesskit_node_builder(field.id, |node| node.set_label(setting.name()));
            });
        });
        ui.ctx().data_mut(|memory| memory.insert_temp(draft_id, TemplateDraft { source: current.clone(), text: draft.clone() }));
        if saved {
            match setting.with(app.notes.config.clone(), draft) {
                Ok(config) => save_notes(app, config),
                Err(error) => app.notify(format!("{}: {error}", setting.name())),
            }
        }
    });
    ui.add_space(ROW_PAD_Y);
}

fn save_notes(app: &mut PdfCraftApp, config: NotesConfig) {
    if let Err(error) = obsidian::persist_notes(app, config) {
        app.notify(format!("Couldn't save the notes settings: {error}"));
    }
}
