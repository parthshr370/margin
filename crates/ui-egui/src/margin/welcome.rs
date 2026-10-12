//! The setup card on the library home: connect an Obsidian vault, add a library folder, open a
//! PDF. It shows while the vault or the library folders aren't set, until "Not now" (saved with
//! the app state, `margin_setup_dismissed`). The vault and folders stay changeable from the
//! Settings window, which uses the same actions.
//!
//! Folders are picked with the system's panel on a worker (`pickers`), so the window never
//! blocks, and are written into `settings.jsonc` (`notes.vault`, `library.folders`) through the
//! same comment-keeping edit as every other setting. A new folder is scanned at once: the library
//! rescans when `library.folders` changes.

use std::path::{Path, PathBuf};

use egui::RichText;

use super::look::{self, ButtonSize, GAP, GAP_LARGE};
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

/// The card's corner radius and padding (the library's Continue reading card's).
const CARD_RADIUS: u8 = 12;
const CARD_PAD: i8 = 16;
const TITLE_TEXT: f32 = 16.0;
const STEP_TITLE_TEXT: f32 = 13.5;
const DETAIL_TEXT: f32 = 12.0;
const STEP_ICON: f32 = 18.0;
/// Between a step's title and its detail line.
const DETAIL_GAP: f32 = 2.0;
/// Obsidian keeps a vault's own settings in this folder at its root.
const OBSIDIAN_CONFIG_FOLDER: &str = ".obsidian";

/// Whether the setup card shows: something is still to set and it wasn't dismissed.
pub(crate) fn wanted(app: &PdfCraftApp) -> bool {
    !app.notes.setup_dismissed && (app.notes.vault.is_none() || app.margin_keys.settings().library.folders.is_empty())
}

/// What a click on the card asks for, done after drawing.
enum Step {
    PickVault,
    Disconnect,
    AddFolder,
    Open,
    Dismiss,
}

/// The setup card, at the top of the library home; nothing once [`wanted`] is false.
pub(crate) fn card(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    if !wanted(app) {
        return;
    }
    let vault = app.notes.vault.clone();
    let folders = app.margin_keys.settings().library.folders.clone();
    let open_keys = look::shortcut(app, "file.open");
    let mut step = None;
    let frame = look::card_frame(t).corner_radius(CARD_RADIUS).inner_margin(CARD_PAD);
    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = GAP;
        ui.label(RichText::new("Set up Margin").font(theme::semibold(TITLE_TEXT)).color(t.text));
        ui.label(RichText::new("Each of these can change later in Settings.").font(theme::regular(DETAIL_TEXT)).color(t.text_muted));
        ui.add_space(GAP);
        let (vault_detail, vault_done) = match &vault {
            Some(path) => (format!("Connected: {}", folder_label(path)), true),
            None => ("Reading notes, highlights and snapshots go into it.".to_owned(), false),
        };
        step_row(ui, t, StepText { icon: "book-open", title: "Connect your Obsidian vault", detail: &vault_detail, done: vault_done }, |ui| {
            if vault_done {
                if look::Button::secondary(ButtonSize::Compact, "Disconnect").show(ui).clicked() {
                    step = Some(Step::Disconnect);
                }
                if look::Button::secondary(ButtonSize::Compact, "Change…").show(ui).clicked() {
                    step = Some(Step::PickVault);
                }
            } else if look::Button::primary(ButtonSize::Compact, "Choose vault…").icon("folder-open").show(ui).clicked() {
                step = Some(Step::PickVault);
            }
        });
        let folder_detail = match folders.len() {
            0 => "Margin finds the PDFs in it and its subfolders.".to_owned(),
            _ => folders.iter().map(|folder| crate::folders_ui::folder_name(folder)).collect::<Vec<_>>().join(", "),
        };
        let folders_done = !folders.is_empty();
        step_row(ui, t, StepText { icon: "folder-plus", title: "Add a library folder", detail: &folder_detail, done: folders_done }, |ui| {
            let add = if folders_done {
                look::Button::secondary(ButtonSize::Compact, "Add another…").show(ui)
            } else {
                look::Button::primary(ButtonSize::Compact, "Add folder…").icon("folder-plus").show(ui)
            };
            if add.clicked() {
                step = Some(Step::AddFolder);
            }
        });
        let open_text = StepText { icon: "file-plus", title: "Open a PDF", detail: "Or drop one onto the window.", done: false };
        step_row(ui, t, open_text, |ui| {
            if look::Button::secondary(ButtonSize::Compact, "Open…").shortcut(open_keys.as_deref()).show(ui).clicked() {
                step = Some(Step::Open);
            }
        });
        ui.add_space(GAP);
        let not_now = look::Button::ghost(ButtonSize::Compact, "Not now").tip("Hide this card. Settings keeps the vault and folders.").show(ui);
        if not_now.clicked() {
            step = Some(Step::Dismiss);
        }
    });
    ui.add_space(GAP_LARGE * 2.0);
    match step {
        Some(Step::PickVault) => pick_vault(app),
        Some(Step::Disconnect) => disconnect_vault(app),
        Some(Step::AddFolder) => add_library_folder(app),
        Some(Step::Open) => app.open_dialog(),
        Some(Step::Dismiss) => app.notes.setup_dismissed = true,
        None => {}
    }
}

/// One step's icon (a check once done), title and detail, with its buttons on the right.
struct StepText<'a> {
    icon: &'a str,
    title: &'a str,
    detail: &'a str,
    done: bool,
}

fn step_row(ui: &mut egui::Ui, t: &Tokens, text: StepText<'_>, controls: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = GAP_LARGE;
        let (icon, tint) = if text.done { ("circle-check", t.accent) } else { (text.icon, t.icon) };
        ui.add(icons::image(icon, STEP_ICON, tint));
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = DETAIL_GAP;
            ui.label(RichText::new(text.title).font(theme::medium(STEP_TITLE_TEXT)).color(t.text));
            ui.add(egui::Label::new(RichText::new(text.detail).font(theme::regular(DETAIL_TEXT)).color(t.text_muted)).truncate());
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            controls(ui);
        });
    });
}

/// A folder's last name, or its whole path when it has none.
fn folder_label(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// Makes `vault` the notes vault (`notes.vault`). A folder without `.obsidian` is still used,
/// with a warning: Obsidian makes that folder the first time it opens the vault.
pub(crate) fn set_vault(app: &mut PdfCraftApp, vault: PathBuf) -> Result<String, String> {
    if !vault.is_absolute() || !vault.is_dir() {
        return Err(format!("{} isn't a folder Margin can use as a vault.", vault.display()));
    }
    let name = folder_label(&vault);
    let is_vault = vault.join(OBSIDIAN_CONFIG_FOLDER).is_dir();
    let mut config = app.notes.config.clone();
    config.vault = Some(vault);
    super::obsidian::persist_notes(app, config)?;
    Ok(if is_vault {
        format!("Connected the vault {name}")
    } else {
        format!("Connected {name}. It has no .obsidian folder yet: open it as a vault in Obsidian once.")
    })
}

/// Asks for the vault folder, then [`set_vault`].
pub(crate) fn pick_vault(app: &mut PdfCraftApp) {
    let dialog = rfd::AsyncFileDialog::new().set_title("Choose your Obsidian vault");
    app.ask_one(crate::pickers::Ask::Folder(dialog), None, |app, vault| match set_vault(app, vault) {
        Ok(message) => app.notify(message),
        Err(error) => super::toast::failed(app, error, None),
    });
}

/// Turns vault notes off (`notes.vault: null`). Notes already in the vault stay where they are.
pub(crate) fn disconnect_vault(app: &mut PdfCraftApp) {
    let mut config = app.notes.config.clone();
    config.vault = None;
    match super::obsidian::persist_notes(app, config) {
        Ok(()) => app.notify("Disconnected the vault. Notes already written stay in it."),
        Err(error) => super::toast::failed(app, error, None),
    }
}

/// Adds `folder` to `library.folders`; the library scans it at once.
pub(crate) fn add_folder(app: &mut PdfCraftApp, folder: PathBuf) -> Result<String, String> {
    if !folder.is_absolute() || !folder.is_dir() {
        return Err(format!("{} isn't a folder Margin can scan.", folder.display()));
    }
    let name = folder_label(&folder);
    let mut folders = app.margin_keys.settings().library.folders.clone();
    if folders.iter().any(|known| Path::new(known) == folder) {
        return Ok(format!("{name} is already a library folder"));
    }
    folders.push(folder.to_string_lossy().into_owned());
    write_folders(app, folders)?;
    Ok(format!("Added {name}; scanning it for PDFs"))
}

/// Asks for a folder, then [`add_folder`].
pub(crate) fn add_library_folder(app: &mut PdfCraftApp) {
    let dialog = rfd::AsyncFileDialog::new().set_title("Add a library folder");
    app.ask_one(crate::pickers::Ask::Folder(dialog), None, |app, folder| match add_folder(app, folder) {
        Ok(message) => app.notify(message),
        Err(error) => super::toast::failed(app, error, None),
    });
}

/// Takes `folder` out of `library.folders`. Its books stay in the library until removed there.
pub(crate) fn remove_folder(app: &mut PdfCraftApp, folder: &str) {
    let folders: Vec<String> = app.margin_keys.settings().library.folders.iter().filter(|known| known.as_str() != folder).cloned().collect();
    match write_folders(app, folders) {
        Ok(()) => app.notify(format!("{} is no longer a library folder", crate::folders_ui::folder_name(folder))),
        Err(error) => super::toast::failed(app, error, None),
    }
}

fn write_folders(app: &mut PdfCraftApp, folders: Vec<String>) -> Result<(), String> {
    let list = serde_json::Value::Array(folders.into_iter().map(serde_json::Value::String).collect());
    super::obsidian::write_user_settings(app, &[("library.folders", list)])
}
