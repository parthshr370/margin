//! Theme files (CU-THM-1). Loading, `extends` resolution and the CSS they become are M3.4; this
//! is the file format the schema describes.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The embedded theme every other theme extends by default.
pub const DEFAULT_THEME: &str = "Margin Dark";

/// Light or dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    /// Used when `theme.mode` resolves to dark.
    Dark,
    /// Used when `theme.mode` resolves to light.
    Light,
}

/// A theme file in `<config>/themes/*.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "Margin theme")]
pub struct Theme {
    /// editor completion: "../schemas/theme.schema.json"
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// unique name, shown in the theme picker
    pub name: String,
    /// "dark" | "light"
    pub appearance: Appearance,
    /// theme whose tokens fill the ones this file leaves out
    #[serde(default = "default_extends")]
    pub extends: String,
    /// flat dotted tokens: bg bg-2 bg-3 bg-4 border border-2 fg fg-2 fg-3 accent accent-soft selection radius font.ui font.mono, highlight.<color id>, page.<mode>.bg|ink|figure
    pub tokens: BTreeMap<String, String>,
}

fn default_extends() -> String {
    DEFAULT_THEME.to_owned()
}
