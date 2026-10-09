//! Changing values in the user's `settings.jsonc` without losing what the user wrote: the file is
//! edited as a concrete syntax tree, so comments, key order and formatting around the changed
//! properties stay as they were ("Set as default" in the Display popover).

use std::path::Path;

use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use serde_json::Value;

use crate::load::{MAX_SETTINGS_BYTES, SETTINGS_FILE, read_capped};

/// Sets each dotted key (`view.page_color`) in `<config_dir>/settings.jsonc` to its value,
/// creating the file and any missing objects. The file is written atomically, and only when the
/// result still parses as Margin's JSONC; a file that does not parse is never rewritten.
pub fn set_user_values(config_dir: &Path, values: &[(&str, Value)]) -> Result<(), String> {
    let path = config_dir.join(SETTINGS_FILE);
    let text = read_capped(&path, MAX_SETTINGS_BYTES)?.unwrap_or_default();
    let edited = with_values(&text, values)?;
    std::fs::create_dir_all(config_dir).map_err(|err| format!("cannot create {} ({err})", config_dir.display()))?;
    crate::store::write_atomically(&path, edited.as_bytes()).map_err(|err| format!("cannot write {} ({err})", path.display()))
}

/// `text` with each dotted key set to its value (see [`set_user_values`]).
pub fn with_values(text: &str, values: &[(&str, Value)]) -> Result<String, String> {
    let source = if text.trim().is_empty() { "{\n}\n" } else { text };
    let root = CstRootNode::parse(source, &jsonc_parser::ParseOptions::default())
        .map_err(|err| format!("settings.jsonc does not parse ({err}); fix it before setting defaults"))?;
    let Some(object) = root.object_value() else {
        return Err("settings.jsonc must hold one JSON object".to_owned());
    };
    for (key, value) in values {
        set(&object, key, value)?;
    }
    let edited = root.to_string();
    crate::jsonc::parse(&edited).map_err(|err| format!("the edited settings would not parse ({})", err.message))?;
    Ok(edited)
}

fn set(object: &CstObject, key: &str, value: &Value) -> Result<(), String> {
    let mut segments = key.split('.').peekable();
    let mut current = object.clone();
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            match current.get(segment) {
                Some(property) => property.set_value(input(value)),
                None => _ = current.append(segment, input(value)),
            }
            return Ok(());
        }
        current = match current.get(segment) {
            Some(property) => property.object_value().ok_or_else(|| format!("settings.jsonc: `{segment}` in `{key}` is not an object"))?,
            None => current.object_value_or_set(segment),
        };
    }
    Err(format!("empty settings key `{key}`"))
}

fn input(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(on) => CstInputValue::Bool(*on),
        Value::Number(number) => CstInputValue::Number(number.to_string()),
        Value::String(text) => CstInputValue::String(text.clone()),
        Value::Array(items) => CstInputValue::Array(items.iter().map(input).collect()),
        Value::Object(map) => CstInputValue::Object(map.iter().map(|(key, value)| (key.clone(), input(value))).collect()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn comments_and_other_keys_survive() {
        let text = "// mine\n{\n  // colour\n  \"view\": { \"page_color\": \"night\", \"zoom\": 120 }, // trailing\n  \"reader\": { \"autosave\": false }\n}\n";
        let edited = with_values(text, &[("view.page_color", json!("sepia")), ("view.layout", json!("two_up"))]).unwrap();
        assert!(edited.contains("// mine") && edited.contains("// colour") && edited.contains("// trailing"), "{edited}");
        let value = crate::jsonc::parse(&edited).unwrap().value.unwrap();
        assert_eq!(value["view"]["page_color"], "sepia");
        assert_eq!(value["view"]["layout"], "two_up");
        assert_eq!(value["view"]["zoom"], 120);
        assert_eq!(value["reader"]["autosave"], false);
    }

    #[test]
    fn the_commented_first_run_file_gains_a_real_block() {
        let text = crate::schema::first_run_settings_jsonc();
        let edited = with_values(&text, &[("view.brightness", json!(90))]).unwrap();
        assert!(edited.starts_with(&text[..text.find('{').unwrap()]), "the header comment stays");
        let loaded = crate::Settings::from_user_text(&edited, "settings.jsonc");
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.settings.view.brightness, 90);
    }

    #[test]
    fn broken_files_and_non_objects_are_refused() {
        assert!(with_values("{ \"view\": ", &[("view.zoom", json!(100))]).is_err());
        assert!(with_values("{ \"view\": 3 }", &[("view.zoom", json!(100))]).is_err());
        assert!(with_values("[]", &[("view.zoom", json!(100))]).is_err());
        let empty = with_values("", &[("view.trim", json!(true))]).unwrap();
        assert_eq!(crate::jsonc::parse(&empty).unwrap().value.unwrap()["view"]["trim"], true);
    }
}
