//! Generators: the JSON schemas (`settings`, `keymap`, `commands`, `theme`, `profile`,
//! `sidecar`), the commented `default-settings.jsonc`, and the files Margin writes into
//! `<config>` on first run. Nothing in this crate reads a generated file back.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde_json::{Map, Value};

use crate::jsonc::{self, KeyOrder};
use crate::keymap::KeymapFile;
use crate::profile::Profile;
use crate::registry::{CommandEntry, Registry};
use crate::settings::Settings;
use crate::sidecar::Sidecar;
use crate::theme::Theme;

fn root_schema<T: JsonSchema>() -> Value {
    SchemaSettings::draft2020_12().into_generator().into_root_schema_for::<T>().to_value()
}

/// `settings.schema.json`. Every property also accepts `null`, which means "not set at this
/// layer" (CU-SET-2). The runtime validator walks this same schema.
pub fn settings_schema() -> Value {
    let mut schema = root_schema::<Settings>();
    allow_null_properties(&mut schema);
    if let Some(defs) = schema.get_mut("$defs").and_then(Value::as_object_mut) {
        defs.values_mut().for_each(allow_null_properties);
    }
    set_title(&mut schema, "Margin settings");
    schema
}

/// `theme.schema.json` (CU-THM-1).
pub fn theme_schema() -> Value {
    root_schema::<Theme>()
}

/// `profile.schema.json` (CU-DOC-3).
pub fn profile_schema() -> Value {
    root_schema::<Profile>()
}

/// `sidecar.schema.json` (CU-SC, sidecar v1 draft until the PDF daily gate freezes it, M2.24).
pub fn sidecar_schema() -> Value {
    root_schema::<Sidecar>()
}

/// The schema of a keymap file: the shipped `data/keymap.jsonc` and `<config>/keymap.jsonc`.
pub fn keymap_schema() -> Value {
    root_schema::<KeymapFile>()
}

/// The schema of `data/commands.jsonc`, Margin's own command ids.
pub fn commands_schema() -> Value {
    let mut schema = root_schema::<Vec<CommandEntry>>();
    set_title(&mut schema, "Margin commands");
    schema
}

/// The `keymap.jsonc` written on first run: how the format works, every bindable id, an example,
/// and an empty list (the user's bindings layer over Margin's own keymap).
pub fn first_run_keymap_jsonc(registry: &Registry) -> String {
    let mut out = String::from(FIRST_RUN_KEYMAP_HEADER);
    out.push_str("//\n// Margin's command ids:\n");
    for entry in registry.margin_entries() {
        out.push_str(&format!("//   {:<28} {}\n", entry.id.as_str(), entry.title));
    }
    out.push_str("//\n// PdfCraft's command ids (the palette runs them too):\n");
    for spec in pdfcraft_engine::commands::COMMANDS {
        out.push_str(&format!("//   {:<28} {}\n", spec.id, spec.label));
    }
    out.push_str(FIRST_RUN_KEYMAP_EXAMPLE);
    out
}

/// How keymap files work, for the first-run `keymap.jsonc`.
const FIRST_RUN_KEYMAP_HEADER: &str = "// Margin keymap. Margin reloads this file when you save it.\n\
//\n\
// Zed's format: a list of blocks { \"context\": \"...\", \"bindings\": { \"keys\": \"command id\" } }.\n\
// A binding runs a command id, [\"id\", { arguments }], or null to unbind the keys.\n\
// Your blocks come after Margin's own keymap: in the same context, yours win; a deeper context\n\
// wins over a shallower one, and a block without a context is the shallowest of all.\n\
//\n\
// Keys: ctrl- alt- shift- super- and secondary- (Cmd on macOS, Ctrl elsewhere), then a lower-case\n\
// letter, a digit, a symbol (? / [ = -), or space enter escape tab backspace delete up down left\n\
// right home end pageup pagedown f1..f12. shift- goes only with letters and named keys. A chord is\n\
// keystrokes separated by spaces (\"g g\").\n\
//\n\
// Contexts, outermost first:\n\
//   App > Reader > AnnotateMode > Page > Selection   reading; AnnotateMode while a markup tool is on\n\
//   App > Reader > Page > Annotation                 a comment is selected (on the page or in Highlights)\n\
//   App > Reader > Find > TextInput                  typing in the find bar\n\
//   App > Palette > TextInput                        the command palette\n\
//   App > Dialog > TextInput                         a dialog\n\
// Attributes: App os=linux|windows|macos; Reader format=pdf layout=scroll|single|two_up;\n\
// AnnotateMode tool=<command id>. Context expressions: Reader && layout == single, !Selection,\n\
// Reader > Find, A || B. In a TextInput only commands that work while typing fire, never a\n\
// printable key.\n\
//\n\
// Press ? to list every binding. app.key_context shows the live context and what each key did.\n";

/// The example and the empty list that end the first-run `keymap.jsonc`.
const FIRST_RUN_KEYMAP_EXAMPLE: &str = "//\n\
// Example: Ctrl+P opens the command palette instead of printing.\n\
//   { \"context\": \"App\", \"bindings\": { \"ctrl-p\": \"view.palette\" } }\n\
[\n\
]\n";

fn set_title(schema: &mut Value, title: &str) {
    if let Some(root) = schema.as_object_mut() {
        root.insert("title".to_owned(), Value::String(title.to_owned()));
    }
}

/// Makes each property of an object schema, and each entry of a map schema, nullable; inline
/// schemas (a map-valued property) get the same treatment inside.
fn allow_null_properties(schema: &mut Value) {
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for property in properties.values_mut() {
            allow_null_properties(property);
            make_nullable(property);
        }
    }
    if let Some(entry @ Value::Object(_)) = schema.get_mut("additionalProperties") {
        allow_null_properties(entry);
        make_nullable(entry);
    }
}

fn make_nullable(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else { return };
    let null = serde_json::json!({ "type": "null" });
    if let Some(reference) = object.remove("$ref") {
        object.insert("anyOf".to_owned(), Value::Array(vec![serde_json::json!({ "$ref": reference }), null]));
    } else if let Some(Value::Array(branches)) = object.get_mut("anyOf") {
        branches.push(null);
    } else if let Some(kind) = object.get_mut("type") {
        match kind {
            Value::String(one) => *kind = Value::Array(vec![Value::String(std::mem::take(one)), Value::String("null".to_owned())]),
            Value::Array(many) if !many.iter().any(|t| t == "null") => many.push(Value::String("null".to_owned())),
            _ => {}
        }
        if let Some(Value::Array(values)) = object.get_mut("enum") {
            values.push(Value::Null);
        }
    }
}

/// The header of `default-settings.jsonc`.
const DEFAULTS_HEADER: &str = "// default-settings.jsonc: GENERATED from crates/margin-config/src/settings.rs. Do not edit.\n\
// Copy a key into <config>/settings.jsonc to change it.\n";

/// The header of the `settings.jsonc` Margin writes when there is none.
const FIRST_RUN_SETTINGS_HEADER: &str = "// Margin settings. Margin reloads this file when you save it.\n\
// Every setting is listed below with its default, commented out. To change one, remove the `// `\n\
// in front of its line and of the lines of its block (\"view\": { ... }), then edit the value.\n\
// A bad value, unknown key or removed setting is reported and ignored; other values still apply.\n";

/// Spaces per nesting level.
const INDENT: usize = 2;

/// `default-settings.jsonc`: every key with its default and its doc comment, one line per leaf.
/// Blocks carry their `[doc]`/`[app]` scope.
pub fn default_settings_jsonc() -> String {
    let mut out = String::from(DEFAULTS_HEADER);
    out.push_str("{\n");
    out.push_str(&defaults_body());
    out.push_str("}\n");
    out
}

/// The `settings.jsonc` written on first run: the defaults body, every line commented out, so the
/// file documents every key and sets none (later default changes still reach the user).
pub fn first_run_settings_jsonc() -> String {
    let mut out = String::from(FIRST_RUN_SETTINGS_HEADER);
    out.push_str("{\n");
    out.push_str(&format!("{}\"$schema\": {},\n", " ".repeat(INDENT), Value::String(crate::SETTINGS_SCHEMA_URL.to_owned())));
    for line in defaults_body().lines().filter(|line| !line.trim_start().starts_with("\"$schema\"")) {
        let text = line.trim_start();
        let pad = line.len().saturating_sub(text.len());
        out.push_str(&format!("{}// {text}\n", " ".repeat(pad)));
    }
    out.push_str("}\n");
    out
}

/// The lines between the root braces of the defaults file, keys in struct order.
fn defaults_body() -> String {
    let schema = settings_schema();
    let defaults = Settings::default();
    // Serialising a struct of strings, numbers, bools, vectors and string-keyed maps cannot fail.
    let values = serde_json::to_value(&defaults).unwrap_or(Value::Null);
    let order = serde_json::to_string(&defaults).map(|text| jsonc::key_order(&text)).unwrap_or_default();
    let mut out = String::new();
    if let Value::Object(map) = &values {
        write_object_body(&mut out, &schema, &schema, map, &order, 1);
    }
    out
}

/// Bound on `$ref` and wrapper hops while finding a value's schema (STD-RS-06).
const MAX_SCHEMA_HOPS: usize = 16;

/// Follows `$ref` and the nullable `anyOf: [S, null]` wrapper to the schema of the value.
fn value_schema<'s>(root: &'s Value, schema: &'s Value) -> &'s Value {
    let mut current = schema;
    for _ in 0..MAX_SCHEMA_HOPS {
        if let Some(name) = current.get("$ref").and_then(Value::as_str).and_then(|r| r.strip_prefix("#/$defs/")) {
            match root.get("$defs").and_then(|defs| defs.get(name)) {
                Some(next) => current = next,
                None => return current,
            }
        } else if let Some(Value::Array(branches)) = current.get("anyOf") {
            match branches.iter().find(|b| b.get("type").and_then(Value::as_str) != Some("null")) {
                Some(next) if branches.len() == 2 => current = next,
                _ => return current,
            }
        } else {
            return current;
        }
    }
    current
}

fn write_object_body(out: &mut String, root: &Value, schema: &Value, map: &Map<String, Value>, order: &KeyOrder, depth: usize) {
    let schema = value_schema(root, schema);
    let properties = schema.get("properties").and_then(Value::as_object);
    let entries = schema.get("additionalProperties").filter(|entry| entry.is_object());
    let pad = " ".repeat(depth * INDENT);
    let no_order = KeyOrder::default();
    // Text order first; any key the order lacks follows in map order.
    let ordered = order
        .0
        .iter()
        .map(|(key, inner)| (key, inner))
        .chain(map.keys().filter(|key| !order.0.iter().any(|(k, _)| k == *key)).map(|key| (key, &no_order)));
    let keys: Vec<(&String, &KeyOrder)> = ordered.filter(|(key, _)| map.contains_key(*key)).collect();
    let count = keys.len();
    for (index, (key, inner_order)) in keys.into_iter().enumerate() {
        let Some(value) = map.get(key) else { continue };
        let comma = if index + 1 < count { "," } else { "" };
        let property = properties.and_then(|props| props.get(key));
        let comment = property.map(describe).unwrap_or_default();
        let comment = if comment.is_empty() { String::new() } else { format!(" // {comment}") };
        let nested_struct = property
            .map(|p| value_schema(root, p))
            .filter(|p| p.get("properties").is_some() || p.get("additionalProperties").is_some_and(Value::is_object));
        match (value, nested_struct) {
            (Value::Object(inner), Some(inner_schema)) => {
                out.push_str(&format!("{pad}{}: {{{comment}\n", inline(&Value::String(key.clone()))));
                write_object_body(out, root, inner_schema, inner, inner_order, depth + 1);
                out.push_str(&format!("{pad}}}{comma}\n"));
            }
            _ if entries.is_some() => {
                out.push_str(&format!("{pad}{}: {}{comma}\n", inline(&Value::String(key.clone())), inline(value)));
            }
            _ => {
                out.push_str(&format!("{pad}{}: {}{comma}{comment}\n", inline(&Value::String(key.clone())), inline(value)));
            }
        }
    }
}

/// A property's comment: its scope tag, if any, then its description on one line.
fn describe(property: &Value) -> String {
    let scope = property.get("x-margin-scope").and_then(Value::as_str).map(|scope| format!("[{scope}] "));
    let description = property.get("description").and_then(Value::as_str).unwrap_or_default();
    let one_line = description.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{}{one_line}", scope.unwrap_or_default())
}

/// JSON on one line with a space after `,` and `:` and inside `{ }`, as CUSTOMIZATION §1 writes it.
fn inline(value: &Value) -> String {
    match value {
        Value::Array(items) => format!("[{}]", items.iter().map(inline).collect::<Vec<_>>().join(", ")),
        Value::Object(map) if map.is_empty() => "{}".to_owned(),
        Value::Object(map) => {
            let fields: Vec<String> = map.iter().map(|(k, v)| format!("{}: {}", inline(&Value::String(k.clone())), inline(v))).collect();
            format!("{{ {} }}", fields.join(", "))
        }
        scalar => scalar.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn jsonschema_validator(schema: &Value) -> jsonschema::Validator {
        jsonschema::validator_for(schema).unwrap()
    }

    /// The generated defaults file parses back to exactly the defaults.
    #[test]
    fn default_settings_file_round_trips() {
        let text = default_settings_jsonc();
        let parsed = crate::jsonc::parse(&text).unwrap().value.unwrap();
        assert_eq!(parsed, serde_json::to_value(Settings::default()).unwrap());
        assert_eq!(parsed["notes"]["front_matter_template"], "---\nsource: \"{{source}}\"\n---\n\n");
        assert!(text.contains("\"zoom\": 100, // percent, used when fit is \"zoom\"; 8..6400, the page view's own zoom limits"), "{text}");
        assert!(text.contains("\"view\": { // [doc] how a document is shown"), "{text}");
    }

    /// Keys come out in struct order, not sorted: `view` is documented before `reader`.
    #[test]
    fn default_settings_keep_struct_order() {
        let text = default_settings_jsonc();
        let at = |needle: &str| text.find(needle).unwrap();
        assert!(at("\"$schema\"") < at("\"version\"") && at("\"version\"") < at("\"view\"") && at("\"view\"") < at("\"reader\""), "{text}");
        assert!(at("\"layout\"") < at("\"cover_alone\""), "{text}");
    }

    /// The first-run file sets nothing, and uncommenting every line gives exactly the defaults.
    #[test]
    fn first_run_settings_set_nothing_until_uncommented() {
        let text = first_run_settings_jsonc();
        let loaded = Settings::from_user_text(&text, "settings.jsonc");
        assert_eq!(loaded.problems, []);
        assert_eq!(loaded.settings, Settings::default());
        assert_eq!(crate::jsonc::parse(&text).unwrap().value.unwrap(), json!({ "$schema": crate::SETTINGS_SCHEMA_URL }));
        let uncommented: String = text
            .lines()
            .filter(|line| !line.starts_with("//"))
            .map(|line| {
                let body = line.trim_start();
                let pad = line.len() - body.len();
                format!("{}{}\n", " ".repeat(pad), body.strip_prefix("// ").unwrap_or(body))
            })
            .collect();
        let parsed = crate::jsonc::parse(&uncommented).unwrap().value.unwrap();
        assert_eq!(parsed, serde_json::to_value(Settings::default()).unwrap());
    }

    #[test]
    fn first_run_keymap_is_an_empty_valid_keymap() {
        let registry = Registry::load().unwrap();
        let text = first_run_keymap_jsonc(&registry);
        assert_eq!(crate::jsonc::parse(&text).unwrap().value.unwrap(), json!([]));
        assert!(text.contains("app.key_context"), "Margin ids are listed");
        assert!(text.contains("view.palette"), "PdfCraft ids are listed");
        let options = crate::CompileOptions { os: crate::Os::Linux, chord_timeout_ms: 1000, annotate_letters: true };
        let compiled = crate::compile(&registry, &[crate::KeymapSource { file: "keymap.jsonc", text: &text }], &options);
        assert_eq!(compiled.problems, []);
    }

    #[test]
    fn shipped_files_match_their_schemas() {
        let keymap = jsonschema_validator(&keymap_schema());
        let shipped = crate::jsonc::parse(crate::MARGIN_KEYMAP).unwrap().value.unwrap();
        assert!(keymap.is_valid(&shipped), "{:?}", keymap.iter_errors(&shipped).map(|e| e.to_string()).collect::<Vec<_>>());
        let commands = jsonschema_validator(&commands_schema());
        let shipped = crate::jsonc::parse(crate::registry::MARGIN_COMMANDS).unwrap().value.unwrap();
        assert!(commands.is_valid(&shipped), "{:?}", commands.iter_errors(&shipped).map(|e| e.to_string()).collect::<Vec<_>>());
    }

    #[test]
    fn keymap_schema_rejects_what_the_compiler_drops() {
        let keymap = jsonschema_validator(&keymap_schema());
        for bad in
            [json!({ "bindings": {} }), json!([{ "bindings": { "x": 3 } }]), json!([{ "bindings": { "x": "Not.An.Id" } }]), json!([{ "unknown": 1 }])]
        {
            assert!(!keymap.is_valid(&bad), "{bad}");
        }
        assert!(keymap.is_valid(&json!([{ "context": "Reader", "bindings": { "n": null, "g g": ["view.zoom.set", { "percent": 100 }] } }])));
    }

    /// M0.9a: the runtime validator and the schema accept and reject the same fixtures.
    #[test]
    fn validator_and_schema_agree_on_fixtures() {
        let schema = settings_schema();
        let validator = jsonschema_validator(&schema);
        let defaults = serde_json::to_value(Settings::default()).unwrap();
        let fixtures = [
            (json!({}), true),
            (defaults.clone(), true),
            (json!({ "view": { "zoom": 8 } }), true),
            (json!({ "view": { "zoom": 6400 } }), true),
            (json!({ "view": { "zoom": 400 } }), true),
            (json!({ "view": { "zoom": 7 } }), false),
            (json!({ "view": { "zoom": 6401 } }), false),
            (json!({ "view": { "zoom": 150.0 } }), true),
            (json!({ "view": { "zoom": 150.5 } }), false),
            (json!({ "view": { "zoom": "100" } }), false),
            (json!({ "view": { "zoom": null } }), true),
            (json!({ "view": null }), true),
            (json!({ "view": { "page_gap_px": 0 } }), true),
            (json!({ "view": { "page_gap_px": 201 } }), false),
            (json!({ "view": { "gamma": 1.0 } }), false),
            (json!({ "view": { "white_threshold": 0 } }), false),
            (json!({ "view": { "dewatermark": 0.5 } }), false),
            (json!({ "view": { "zoom_steps": [] } }), false),
            (json!({ "view": { "zoom_steps": [8, 6400] } }), true),
            (json!({ "view": { "zoom_steps": [50, 6401] } }), false),
            (json!({ "view": { "zoom_steps": [50, "x"] } }), false),
            (json!({ "view": { "layout": "two_up" } }), true),
            (json!({ "view": { "layout": "book" } }), false),
            (json!({ "view": { "page_colour": "night" } }), false),
            (json!({ "nope": 1 }), false),
            (json!({ "version": 2 }), false),
            (json!({ "keys": { "chord_timeout_ms": 99 } }), false),
            (json!({ "keys": { "base": "margin" } }), true),
            (json!({ "keys": { "base": "none" } }), true),
            (json!({ "keys": { "base": "sioyek" } }), false),
            (json!({ "keys": { "base": "okular" } }), false),
            (json!({ "ui": { "rail": { "tool_options": {} } } }), false),
            (json!({ "ui": { "rail": { "items": ["edit.undo", "|", "reader.tool.select"] } } }), true),
            (json!({ "ui": { "rail": { "side": "up" } } }), false),
            (json!({ "reader": { "find": { "case": true } } }), true),
            (json!({ "reader": { "find": { "regex": true } } }), false),
            (json!({ "reader": { "start": "home" } }), false),
            (json!({ "reader": { "synctex_click": "alt-click" } }), true),
            (json!({ "reader": { "synctex_click": "secondary-shift-click" } }), true),
            (json!({ "reader": { "synctex_click": "middle-click" } }), false),
            (json!({ "reader": { "synctex_editor": "code -g {file}:{line}" } }), true),
            (json!({ "reader": { "synctex_editor": 1 } }), false),
            (json!({ "reader": { "reload_debounce_ms": 60000 } }), true),
            (json!({ "reader": { "reload_debounce_ms": 60001 } }), false),
            (json!({ "reader": { "link_preview": false, "reload_on_change": false } }), true),
            (json!({ "perf": {} }), false),
            (json!({ "notes": { "vault": "" } }), true),
            (
                json!({ "notes": { "export_target": "readwise_csv", "update_mode": "append", "colors": [{ "name": "Blue", "rgb": [0, 128, 255] }] } }),
                true,
            ),
            (json!({ "notes": { "export_target": "html" } }), false),
            (json!({ "notes": { "colors": [] } }), false),
            (json!({ "notes": { "colors": [{ "name": "", "rgb": [0, 0, 0] }] } }), false),
            (json!({ "notes": { "colors": [{ "name": "Blue", "rgb": [0, 128, 256] }] } }), false),
            (json!({ "notes": { "colors": [{ "name": "x".repeat(65), "rgb": [0, 0, 0] }] } }), false),
            // A relative vault is a valid string; the notes panel reports it instead of using it.
            (json!({ "notes": { "vault": "relative" } }), true),
            (json!({ "library": { "metadata_lookup": true } }), true),
            (json!({ "library": { "metadata_lookup": "yes" } }), false),
        ];
        for (fixture, valid) in fixtures {
            assert_eq!(validator.is_valid(&fixture), valid, "schema on {fixture}");
            let positions = crate::jsonc::Positions::default();
            let mut runtime =
                crate::validate::Validator { root: &schema, fallback: &defaults, positions: &positions, file: "f", problems: Vec::new() };
            runtime.document(fixture.clone());
            assert_eq!(runtime.problems.is_empty(), valid, "runtime on {fixture}: {:?}", runtime.problems);
        }
    }

    /// Every layer is partial (CU-SET-2), so the settings schema requires nothing.
    #[test]
    fn settings_schema_requires_no_key() {
        assert!(!settings_schema().to_string().contains("\"required\""));
    }

    #[test]
    fn unsupported_settings_are_absent_from_generated_defaults_and_schema() {
        let defaults = default_settings_jsonc();
        let schema = settings_schema();
        for key in ["perf", "start", "tool_options", "gamma", "saturation", "mono_threshold", "white_threshold", "dewatermark", "regex"] {
            assert!(!defaults.contains(&format!("\"{key}\"")), "{key} still advertised in defaults");
        }
        assert!(schema.get("properties").and_then(|properties| properties.get("perf")).is_none());
        for definition in ["Perf", "TileFormat", "Start", "ToolOptions"] {
            assert!(schema.get("$defs").and_then(|definitions| definitions.get(definition)).is_none(), "{definition} still in schema");
        }
    }

    #[test]
    fn empty_vault_disables_instead_of_becoming_an_empty_path() {
        let loaded = Settings::from_user_text(r#"{ "notes": { "vault": "" } }"#, "settings.jsonc");
        assert_eq!(loaded.problems, []);
        assert_eq!(loaded.settings.notes.vault, None);
    }

    #[test]
    fn other_schemas_generate() {
        for schema in [theme_schema(), profile_schema(), sidecar_schema()] {
            assert!(schema.get("title").and_then(Value::as_str).is_some_and(|t| !t.is_empty()), "{schema}");
        }
        let theme = jsonschema_validator(&theme_schema());
        assert!(theme.is_valid(&json!({ "name": "Margin Dark", "appearance": "dark", "tokens": { "bg": "#0a0a0a" } })));
        assert!(!theme.is_valid(&json!({ "name": "x", "appearance": "dim", "tokens": {} })));
    }
}
