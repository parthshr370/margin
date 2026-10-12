//! Watches `<config>` for saves of `settings.jsonc`, `keymap.jsonc` and `themes.json`, and
//! `<config>/themes/` for its theme files, so Margin reloads them without a restart. The watcher
//! thread only queues events and wakes the UI; the UI thread reads and applies the files
//! (`keys::MarginKeys::poll`).
//!
//! The folders are watched, not the files: editors often save by writing a new file and renaming
//! it over the old one, which a watch on the old file would miss. The themes folder may not exist
//! when the watch starts, or be removed and made again: [`SettingsWatch::watch_themes`] watches it
//! again after each reload.

use std::path::Path;

/// The files in `<config>` whose changes trigger a reload. `themes` is the drop-in folder itself
/// (made, removed or renamed).
pub(crate) const WATCHED: [&str; 4] =
    [margin_config::SETTINGS_FILE, margin_config::KEYMAP_FILE, margin_config::THEMES_FILE, margin_config::THEMES_DIR];

/// The file extension of a drop-in theme file.
const THEME_EXTENSION: &str = "json";

/// A running watch on the config folder. Dropping it stops the watcher thread.
pub(crate) struct SettingsWatch {
    #[cfg(not(target_arch = "wasm32"))]
    watcher: notify::RecommendedWatcher,
    #[cfg(not(target_arch = "wasm32"))]
    events: std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
}

impl SettingsWatch {
    /// Starts watching `dir` (which must exist) and its themes folder, if there is one. Each event
    /// wakes `ctx` so it is seen at once.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn start(dir: &Path, ctx: &egui::Context) -> Result<Self, String> {
        use notify::Watcher as _;
        let (sender, events) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if sender.send(event).is_err() {
                log::debug!("settings watch: the app stopped listening; dropping a change event");
            }
            ctx.request_repaint();
        })
        .map_err(|err| err.to_string())?;
        watcher.watch(dir, notify::RecursiveMode::NonRecursive).map_err(|err| err.to_string())?;
        let mut watch = Self { watcher, events };
        watch.watch_themes(dir);
        Ok(watch)
    }

    /// The web build has no config folder to watch.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn start(_dir: &Path, _ctx: &egui::Context) -> Result<Self, String> {
        Err("no file watching on the web".to_owned())
    }

    /// (Re)watches `<dir>/themes/` when it exists: after the folder was made, or removed and made
    /// again, the old watch sees nothing. A folder that can't be watched is logged; its files
    /// still load on the next change elsewhere.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn watch_themes(&mut self, dir: &Path) {
        use notify::Watcher as _;
        let folder = dir.join(margin_config::THEMES_DIR);
        if !folder.is_dir() {
            return;
        }
        // Not watched yet is the usual case on the first call, so its error says nothing.
        if let Err(err) = self.watcher.unwatch(&folder) {
            log::debug!("settings watch: {} was not watched yet ({err})", folder.display());
        }
        if let Err(err) = self.watcher.watch(&folder, notify::RecursiveMode::NonRecursive) {
            log::warn!("settings watch: can't watch {} ({err}); restart Margin to apply theme changes", folder.display());
        }
    }

    /// The web build never starts a watch.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn watch_themes(&mut self, _dir: &Path) {}

    /// Drains the queued events: whether a watched file changed. Watcher errors are logged; the
    /// next save still triggers a reload.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn changed(&self) -> bool {
        let mut changed = false;
        for event in self.events.try_iter() {
            match event {
                Ok(event) => changed |= touches_watched(&event),
                Err(err) => log::warn!("settings watch: {err}"),
            }
        }
        changed
    }

    /// The web build never starts a watch.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn changed(&self) -> bool {
        false
    }
}

/// Whether an event writes, creates, renames or removes a watched file: one of [`WATCHED`], or a
/// `.json` file in the themes folder.
#[cfg(not(target_arch = "wasm32"))]
fn touches_watched(event: &notify::Event) -> bool {
    let writes = !matches!(event.kind, notify::EventKind::Access(_));
    let name_of = |path: &Path| path.file_name().and_then(|name| name.to_str()).map(str::to_owned);
    let watched = |path: &std::path::PathBuf| {
        let in_themes = path.parent().and_then(name_of).is_some_and(|parent| parent == margin_config::THEMES_DIR);
        let theme_file =
            in_themes && path.extension().and_then(|extension| extension.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case(THEME_EXTENSION));
        theme_file || name_of(path).is_some_and(|name| WATCHED.contains(&name.as_str()))
    };
    writes && event.paths.iter().any(watched)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn only_writes_to_watched_files_count() {
        let event = |kind, path: &str| notify::Event::new(kind).add_path(std::path::PathBuf::from(path));
        let modify = notify::EventKind::Modify(notify::event::ModifyKind::Any);
        assert!(touches_watched(&event(modify, "/c/margin/keymap.jsonc")));
        assert!(touches_watched(&event(notify::EventKind::Create(notify::event::CreateKind::File), "/c/margin/settings.jsonc")));
        assert!(touches_watched(&event(modify, "/c/margin/themes.json")));
        assert!(touches_watched(&event(modify, "/c/margin/themes/one.json")));
        assert!(touches_watched(&event(notify::EventKind::Remove(notify::event::RemoveKind::Folder), "/c/margin/themes")));
        assert!(!touches_watched(&event(modify, "/c/margin/themes/notes.txt")));
        assert!(!touches_watched(&event(modify, "/c/margin/notes.txt")));
        assert!(!touches_watched(&event(modify, "/c/margin/logs/one.json")));
        assert!(!touches_watched(&event(notify::EventKind::Access(notify::event::AccessKind::Any), "/c/margin/keymap.jsonc")));
    }

    /// A save in the watched folders arrives as an event (the reload path end to end, minus the
    /// UI), the themes folder included.
    #[test]
    fn a_save_is_seen() {
        let dir = std::env::temp_dir().join(format!("margin-watch-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(margin_config::THEMES_DIR)).unwrap();
        let watch = SettingsWatch::start(&dir, &egui::Context::default()).unwrap();
        let seen = |path: std::path::PathBuf| {
            std::fs::write(path, "{}").unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut seen = false;
            while !seen && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
                seen = watch.changed();
            }
            seen
        };
        let keymap = seen(dir.join(margin_config::KEYMAP_FILE));
        let theme = seen(dir.join(margin_config::THEMES_DIR).join("mine.json"));
        std::fs::remove_dir_all(&dir).ok();
        assert!(keymap, "no keymap change event within 5 s");
        assert!(theme, "no theme change event within 5 s");
    }
}
