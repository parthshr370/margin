//! Loading settings: built-in defaults plus the user's `settings.jsonc`, deep-merged (objects
//! merge key by key, arrays and scalars replace) and validated against the generated schema.
//! Loading never fails: every problem falls back to the default and is reported.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde_json::Value;

use crate::jsonc;
use crate::problem::{Problem, Severity};
use crate::schema::settings_schema;
use crate::settings::Settings;
use crate::validate::Validator;

/// The user settings file inside `<config>` (CU-SET-1 layer 2).
pub const SETTINGS_FILE: &str = "settings.jsonc";

/// Largest config file read, in bytes: settings and keymap files are hand-written.
pub const MAX_SETTINGS_BYTES: u64 = 1 << 20;

/// Environment variable that overrides `<config>`, for tests and portable setups.
pub const CONFIG_DIR_ENV: &str = "MARGIN_CONFIG_DIR";

/// Settings plus everything that was wrong while loading them.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedSettings {
    /// The effective settings.
    pub settings: Settings,
    /// Problems, in file order.
    pub problems: Vec<Problem>,
}

/// `<config>`: `$MARGIN_CONFIG_DIR` if set, else the OS config dir plus `margin`
/// (`~/.config/margin` on Linux, ADR-0007). `None` when the OS reports no config dir.
pub fn config_dir() -> Option<PathBuf> {
    match std::env::var_os(CONFIG_DIR_ENV) {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => dirs::config_dir().map(|dir| dir.join("margin")),
    }
}

/// The generated settings schema; the validator walks it (M0.9a).
static SCHEMA: LazyLock<Value> = LazyLock::new(settings_schema);

/// The defaults as JSON: the bottom layer and the "using X" in messages. Serialising a struct of
/// strings, numbers, bools, vectors and string-keyed maps cannot fail.
static DEFAULTS: LazyLock<Value> = LazyLock::new(|| serde_json::to_value(Settings::default()).unwrap_or(Value::Null));

impl Settings {
    /// Loads `<config_dir>/settings.jsonc` over the defaults. A missing file is not a problem.
    pub fn load(config_dir: &Path) -> LoadedSettings {
        let path = config_dir.join(SETTINGS_FILE);
        let shown = path.display().to_string();
        match read_capped(&path, MAX_SETTINGS_BYTES) {
            Ok(Some(text)) => Self::from_user_text(&text, &shown),
            Ok(None) => LoadedSettings { settings: Self::default(), problems: Vec::new() },
            Err(message) => {
                LoadedSettings { settings: Self::default(), problems: vec![file_problem(&shown, 1, 1, format!("{message}; using the defaults"))] }
            }
        }
    }

    /// Merges a user settings text over the defaults. `file` names the source in problems.
    pub fn from_user_text(text: &str, file: &str) -> LoadedSettings {
        let fallback = |problem: Problem| LoadedSettings { settings: Self::default(), problems: vec![problem] };
        let parsed = match jsonc::parse(text) {
            Ok(parsed) => parsed,
            Err(err) => return fallback(file_problem(file, err.position.line, err.position.col, format!("{}; using the defaults", err.message))),
        };
        let user = match parsed.value {
            None => return LoadedSettings { settings: Self::default(), problems: Vec::new() },
            Some(Value::Object(user)) => Value::Object(user),
            Some(_) => return fallback(file_problem(file, 1, 1, "settings must be a JSON object; using the defaults".to_owned())),
        };
        let defaults: &Value = &DEFAULTS;
        let mut validator = Validator { root: &SCHEMA, fallback: defaults, positions: &parsed.positions, file, problems: Vec::new() };
        let cleaned = validator.document(user);
        let mut problems = validator.problems;
        // Objects iterate in key order, not file order: report in file order.
        problems.sort_by_key(|problem| (problem.line, problem.col));
        let merged = merge(defaults.clone(), cleaned);
        match serde_json::from_value(merged) {
            Ok(settings) => LoadedSettings { settings, problems },
            Err(err) => {
                problems.push(file_problem(file, 1, 1, format!("{err}; using the defaults")));
                LoadedSettings { settings: Self::default(), problems }
            }
        }
    }

    /// The value at a dotted key (`view.zoom_steps`, or a whole block such as `view`). Map keys
    /// that contain dots (`ui.rail.tool_options.comment.ink`) are matched whole.
    pub fn get(&self, key: &str) -> Option<Value> {
        let mut node = serde_json::to_value(self).ok()?;
        let mut rest = key;
        while !rest.is_empty() {
            let Value::Object(mut map) = node else { return None };
            // Longest matching key first, so `comment.ink` beats `comment`.
            let cut = std::iter::once(rest.len())
                .chain(rest.match_indices('.').map(|(at, _)| at).collect::<Vec<_>>().into_iter().rev())
                .find(|&cut| rest.get(..cut).is_some_and(|name| map.contains_key(name)))?;
            let name = rest.get(..cut)?;
            node = map.remove(name)?;
            rest = rest.get(cut..).map_or("", |tail| tail.strip_prefix('.').unwrap_or(tail));
        }
        Some(node)
    }
}

/// CU-SET-2: objects deep-merge key by key; arrays and scalars replace.
fn merge(base: Value, over: Value) -> Value {
    match (base, over) {
        (Value::Object(mut base), Value::Object(over)) => {
            for (key, value) in over {
                let merged = match base.remove(&key) {
                    Some(below) => merge(below, value),
                    None => value,
                };
                base.insert(key, merged);
            }
            Value::Object(base)
        }
        (_, over) => over,
    }
}

fn file_problem(file: &str, line: u32, col: u32, message: String) -> Problem {
    Problem { file: file.to_owned(), line, col, key: String::new(), message, severity: Severity::Error }
}

/// Reads a UTF-8 file of at most `max_bytes`. `Ok(None)` when it does not exist; `Err` says why
/// it could not be used, for a problem message.
pub fn read_capped(path: &Path, max_bytes: u64) -> Result<Option<String>, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read the file ({err})")),
    };
    let mut bytes = Vec::new();
    if let Err(err) = file.take(max_bytes.saturating_add(1)).read_to_end(&mut bytes) {
        return Err(format!("cannot read the file ({err})"));
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(format!("larger than {} KiB", max_bytes / 1024));
    }
    String::from_utf8(bytes).map(Some).map_err(|_| "not UTF-8 text".to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn empty_config_dir_loads_the_defaults() {
        let dir = std::env::temp_dir().join(format!("margin-config-empty-{}", std::process::id()));
        let loaded = Settings::load(&dir);
        assert_eq!(loaded.settings, Settings::default());
        assert_eq!(loaded.problems, []);
    }

    #[test]
    fn user_values_replace_arrays_and_merge_objects() {
        let loaded = Settings::from_user_text(
            r#"{ "view": { "zoom_steps": [80, 100, 120] }, "ui": { "rail": { "tool_options": { "comment.ink": { "color": "red", "width": 1.5 } } } } }"#,
            "settings.jsonc",
        );
        assert_eq!(loaded.problems, []);
        assert_eq!(loaded.settings.view.zoom_steps, [80, 100, 120]);
        assert_eq!(loaded.settings.view.zoom, 100, "untouched keys keep their default");
        let rail = &loaded.settings.ui.rail.tool_options;
        assert_eq!(rail["comment.ink"].width, Some(1.5));
        assert_eq!(rail["comment.highlight"].color, "yellow", "other tools keep their defaults");
    }

    #[test]
    fn cu_val_3_example() {
        let loaded = Settings::from_user_text(r#"{ "view": { "zoom": 400, "page_colour": "night", "layout": "book" } }"#, "settings.jsonc");
        let shown: Vec<String> = loaded.problems.iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "settings.jsonc:1:13 view.zoom 400 clamped to 300",
                "settings.jsonc:1:26 unknown key view.page_colour (did you mean view.page_color?)",
                "settings.jsonc:1:50 view.layout \"book\" is not scroll|single|two_up; using \"scroll\"",
            ]
        );
        assert_eq!(loaded.settings.view.zoom, 300);
        assert_eq!(loaded.settings.view.layout, crate::settings::Layout::Scroll);
    }

    #[test]
    fn wrong_types_fall_back_and_null_means_unset() {
        let loaded = Settings::from_user_text(r#"{ "view": { "zoom": "big", "fit": null, "zoom_steps": [100, "x"] }, "keys": 3 }"#, "s");
        assert_eq!(loaded.settings, Settings::default());
        let keys: Vec<&str> = loaded.problems.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, ["view.zoom", "view.zoom_steps.1", "keys"]);
    }

    #[test]
    fn broken_files_give_defaults_and_one_error() {
        for text in ["{", "[1, 2]", "{ \"view\": { \"zoom\": 1e999 } }", "\"x\""] {
            let loaded = Settings::from_user_text(text, "s");
            assert_eq!(loaded.settings, Settings::default(), "{text}");
            assert_eq!(loaded.problems.len(), 1, "{text}: {:?}", loaded.problems);
        }
        assert!(Settings::from_user_text("// nothing yet\n", "s").problems.is_empty());
    }

    #[test]
    fn get_walks_dotted_keys() {
        let settings = Settings::default();
        assert_eq!(settings.get("view.zoom"), Some(json!(100)));
        assert_eq!(settings.get("view.zoom_steps").and_then(|v| v.as_array().map(Vec::len)), Some(12));
        assert_eq!(settings.get("ui.rail.tool_options.comment.ink.width"), Some(json!(2.0)));
        assert_eq!(settings.get("view.nope"), None);
        assert_eq!(settings.get("view.zoom.deeper"), None);
        assert!(settings.get("").is_some_and(|v| v.is_object()));
    }

    #[test]
    fn integer_valued_floats_are_accepted() {
        let loaded = Settings::from_user_text(r#"{ "view": { "zoom": 150.0 } }"#, "s");
        assert_eq!(loaded.problems, []);
        assert_eq!(loaded.settings.view.zoom, 150);
    }
}
