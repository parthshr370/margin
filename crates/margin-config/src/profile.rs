//! Profile files (CU-DOC-3). Matching and applying them is M3.1; this is the file format the
//! schema describes.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// When a profile applies: every field present must hold.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfileMatch {
    /// glob on the document path, `~` expanded
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// document formats, e.g. ["pdf"]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Vec<String>>,
    /// a library tag
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// at least one of these must match
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub any: Option<Vec<ProfileMatch>>,
}

/// A profile in `<config>/profiles/<name>.jsonc`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "Margin profile")]
pub struct Profile {
    /// editor completion: "../schemas/profile.schema.json"
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// shown in reader.profile.list
    pub name: String,
    /// higher applies later and wins
    #[serde(default)]
    pub priority: i32,
    /// when the profile applies
    #[serde(rename = "match")]
    pub matches: ProfileMatch,
    /// [doc] settings keys only, as in settings.jsonc
    #[schemars(schema_with = "settings_reference")]
    pub settings: Value,
}

/// The settings object of a profile, checked against the settings schema beside this one.
fn settings_reference(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "$ref": "settings.schema.json" })
}
