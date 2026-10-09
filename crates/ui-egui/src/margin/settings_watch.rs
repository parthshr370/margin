//! Watches `<config>` for saves of `settings.jsonc` and `keymap.jsonc`, so Margin reloads them
//! without a restart. The watcher thread only queues events and wakes the UI; the UI thread reads
//! and applies the files (`keys::MarginKeys::poll`).
//!
//! The folder is watched, not the files: editors often save by writing a new file and renaming it
//! over the old one, which a watch on the old file would miss.

use std::path::Path;

/// The files whose changes trigger a reload.
pub(crate) const WATCHED: [&str; 2] = [margin_config::SETTINGS_FILE, margin_config::KEYMAP_FILE];

/// A running watch on the config folder. Dropping it stops the watcher thread.
pub(crate) struct SettingsWatch {
    #[cfg(not(target_arch = "wasm32"))]
    _watcher: notify::RecommendedWatcher,
    #[cfg(not(target_arch = "wasm32"))]
    events: std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
}

impl SettingsWatch {
    /// Starts watching `dir` (which must exist). Each event wakes `ctx` so it is seen at once.
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
        Ok(Self { _watcher: watcher, events })
    }

    /// The web build has no config folder to watch.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn start(_dir: &Path, _ctx: &egui::Context) -> Result<Self, String> {
        Err("no file watching on the web".to_owned())
    }

    /// Drains the queued events: whether one of [`WATCHED`] changed. Watcher errors are logged;
    /// the next save still triggers a reload.
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

/// Whether an event writes, creates, renames or removes one of the watched files.
#[cfg(not(target_arch = "wasm32"))]
fn touches_watched(event: &notify::Event) -> bool {
    let writes = !matches!(event.kind, notify::EventKind::Access(_));
    writes && event.paths.iter().any(|path| path.file_name().and_then(|name| name.to_str()).is_some_and(|name| WATCHED.contains(&name)))
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
        assert!(!touches_watched(&event(modify, "/c/margin/notes.txt")));
        assert!(!touches_watched(&event(notify::EventKind::Access(notify::event::AccessKind::Any), "/c/margin/keymap.jsonc")));
    }

    /// A save in the watched folder arrives as an event (the reload path end to end, minus the UI).
    #[test]
    fn a_save_is_seen() {
        let dir = std::env::temp_dir().join(format!("margin-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let watch = SettingsWatch::start(&dir, &egui::Context::default()).unwrap();
        std::fs::write(dir.join(margin_config::KEYMAP_FILE), "[]").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut seen = false;
        while !seen && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            seen = watch.changed();
        }
        std::fs::remove_dir_all(&dir).ok();
        assert!(seen, "no change event within 5 s");
    }
}
