//! Opening Margin's config files in the system's text editor, as Zed's "open settings" commands
//! do: `settings.open` (settings.jsonc), `settings.open_default` (the read-only
//! default-settings.jsonc), `themes.open` (themes.json) and `keymap.open` (keymap.jsonc). A file
//! that is missing is made first, from the same commented template the first run writes. Margin
//! reloads each file when it is saved (`keys`).

use std::path::{Path, PathBuf};

use crate::PdfCraftApp;

/// The ids this module runs.
pub(crate) const IDS: [&str; 4] = ["settings.open", "settings.open_default", "themes.open", "keymap.open"];

/// Runs one of [`IDS`]; `false` for any other id.
pub(crate) fn run(app: &mut PdfCraftApp, id: &str) -> bool {
    if !IDS.contains(&id) {
        return false;
    }
    let Some(dir) = app.margin_keys.settings_dir() else {
        app.notify("This system has no settings folder.");
        return true;
    };
    match ensure_file(app, &dir, id) {
        Ok(path) => open(app, &path),
        Err(message) => app.notify(message),
    }
    true
}

/// The file `id` opens, made from its template if it is missing. The defaults file is
/// rewritten whenever it differs, as on every launch.
fn ensure_file(app: &PdfCraftApp, dir: &Path, id: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|err| format!("Couldn't make {} ({err})", dir.display()))?;
    let (path, text) = match id {
        "settings.open" => (dir.join(margin_config::SETTINGS_FILE), Some(margin_config::schema::first_run_settings_jsonc())),
        "themes.open" => (dir.join(margin_config::THEMES_FILE), Some(margin_config::theme::first_run_themes_json())),
        "keymap.open" => {
            let template = app.margin_keys.registry().map(margin_config::schema::first_run_keymap_jsonc);
            (dir.join(margin_config::KEYMAP_FILE), template)
        }
        "settings.open_default" => {
            let path = dir.join(margin_config::schema::DEFAULT_SETTINGS_FILE);
            let text = margin_config::schema::default_settings_jsonc();
            if !std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
                std::fs::write(&path, text).map_err(|err| format!("Couldn't write {} ({err})", path.display()))?;
            }
            return Ok(path);
        }
        other => return Err(format!("{other} opens no config file")),
    };
    if let Some(text) = text {
        super::keys::write_if_missing(&path, &text).map_err(|err| format!("Couldn't write {} ({err})", path.display()))?;
    }
    Ok(path)
}

/// Hands `path` to the system's text editor: `open -t` on macOS, `xdg-open` on Linux. The opener
/// runs detached and is reaped when it exits.
fn open(app: &mut PdfCraftApp, path: &Path) {
    if !app.margin_keys.note_opened(path) {
        return;
    }
    #[cfg(target_arch = "wasm32")]
    app.notify("The web build has no settings folder to open.");
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut command = if cfg!(target_os = "macos") {
            let mut command = std::process::Command::new("open");
            command.arg("-t");
            command
        } else if cfg!(windows) {
            let mut command = std::process::Command::new("rundll32");
            command.arg("url.dll,FileProtocolHandler");
            command
        } else {
            std::process::Command::new("xdg-open")
        };
        let spawned =
            command.arg(path).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
        match spawned {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    if let Err(err) = child.wait() {
                        log::warn!("the config file opener: {err}");
                    }
                });
            }
            Err(err) => app.notify(format!("Couldn't open {} in an editor ({err}).", path.display())),
        }
    }
}
