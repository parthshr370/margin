//! Command ids and the lookup the keymap compiler needs: PdfCraft's
//! `pdfcraft_engine::commands::COMMANDS` and `catalog::TOOL_GROUPS`, reused unchanged, plus
//! Margin's own ids from `data/commands.jsonc`. Margin never redefines a PdfCraft id.

use std::fmt;

use pdfcraft_engine::{catalog, commands};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::ContextExpr;
use crate::error::{Error, Result};
use crate::jsonc;

/// Margin's own commands, as shipped.
pub const MARGIN_COMMANDS: &str = include_str!("../data/commands.jsonc");

/// The repository path of [`MARGIN_COMMANDS`], for messages.
pub const MARGIN_COMMANDS_FILE: &str = "crates/margin-config/data/commands.jsonc";

/// Namespaces Margin may add ids to (CU-CMD-1, ADR-0005), plus `annotation` (the owner's ids for
/// acting on the selected comment; PdfCraft has no such namespace, its comment ids are `comment.*`)
/// and `settings`, `themes` and `keymap` (opening Margin's config files; PdfCraft has none).
pub const MARGIN_NAMESPACES: &[&str] = &["app", "view", "reader", "library", "note", "annotation", "settings", "themes", "keymap"];

/// Longest command id, in bytes (STD-RS-05).
const MAX_ID_BYTES: usize = 128;

/// A command id: `namespace.object[.verb]`, lowercase `[a-z0-9_]` segments (CU-CMD-1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CommandId(String);

impl CommandId {
    /// Checks the form of `id`. It does not check that the id is registered.
    pub fn new(id: &str) -> Result<Self> {
        let mut segments = id.split('.');
        let well_formed = id.len() <= MAX_ID_BYTES
            && id.split('.').count() >= 2
            && segments.all(|segment| !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
        if well_formed { Ok(Self(id.to_owned())) } else { Err(Error::BadId { id: id.chars().take(80).collect() }) }
    }

    /// The id text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The namespace, the part before the first `.`.
    pub fn namespace(&self) -> &str {
        self.0.split('.').next().unwrap_or_default()
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for CommandId {
    type Error = Error;

    fn try_from(id: String) -> Result<Self> {
        Self::new(&id)
    }
}

impl From<CommandId> for String {
    fn from(id: CommandId) -> Self {
        id.0
    }
}

impl JsonSchema for CommandId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CommandId".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "namespace.object[.verb], lowercase [a-z0-9_] segments (CU-CMD-1)",
            "type": "string",
            "pattern": "^[a-z0-9_]+(\\.[a-z0-9_]+)+$",
            "maxLength": MAX_ID_BYTES,
        })
    }
}

/// Palette and menu group (CU-CMD-3; the mockup `CMDS` groups, mockup:1406).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Group {
    /// Display and layout.
    View,
    /// Navigation.
    Go,
    /// Search.
    Find,
    /// Highlights, bookmarks, annotation.
    Mark,
    /// Obsidian notes.
    Obsidian,
    /// Page operations.
    Pages,
    /// Files.
    File,
    /// Tools.
    Tools,
    /// Help.
    Help,
    /// The app shell.
    App,
}

/// One Margin command in `data/commands.jsonc` (CU-CMD-3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandEntry {
    /// The id; only under Margin's namespaces ([`MARGIN_NAMESPACES`]).
    pub id: CommandId,
    /// Palette and menu title.
    pub title: String,
    /// Lucide icon name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Palette group.
    pub group: Group,
    /// CU-KEY-4 expression; false hides the command and lets its key fall through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// The key also fires while a text field has focus.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_text: bool,
    /// JSON Schema of the arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    /// An enum argument; the palette shows one row per value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette_variants: Option<String>,
    /// Extra search words for the palette.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

/// Where a command is defined (CU-CMD-3 `source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// `pdfcraft_engine::commands::COMMANDS`.
    PdfCraft,
    /// An item of `pdfcraft_engine::catalog::TOOL_GROUPS` that is not in `COMMANDS`.
    Catalog,
    /// `data/commands.jsonc`.
    Margin,
}

/// What the keymap compiler needs to know about a command.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandInfo {
    /// Where it is defined.
    pub source: Source,
    /// The key also fires while a text field has focus.
    pub in_text: bool,
    /// Parsed `when`, `None` when it always holds.
    pub when: Option<ContextExpr>,
}

/// PdfCraft's commands and catalog plus Margin's own ids.
#[derive(Debug, Clone)]
pub struct Registry {
    margin: Vec<(CommandEntry, Option<ContextExpr>)>,
}

impl Registry {
    /// Loads Margin's shipped commands. Fails only if `data/commands.jsonc` is broken, which the
    /// crate's tests rule out.
    pub fn load() -> Result<Self> {
        Self::from_text(MARGIN_COMMANDS)
    }

    fn from_text(text: &str) -> Result<Self> {
        let data_error =
            |position: jsonc::Position, reason: String| Error::Data { file: MARGIN_COMMANDS_FILE, line: position.line, col: position.col, reason };
        let parsed = jsonc::parse(text).map_err(|err| data_error(err.position, err.message))?;
        let Some(Value::Array(items)) = parsed.value else { return Err(data_error(jsonc::Position::default(), "expected an array".to_owned())) };
        let mut margin = Vec::with_capacity(items.len());
        for (index, item) in items.into_iter().enumerate() {
            let position = parsed.positions.find(&format!("/{index}"));
            let entry: CommandEntry = serde_json::from_value(item).map_err(|err| data_error(position, err.to_string()))?;
            if !MARGIN_NAMESPACES.contains(&entry.id.namespace()) {
                return Err(data_error(position, format!("{} is outside Margin's namespaces (CU-CMD-1)", entry.id)));
            }
            if commands::command(entry.id.as_str()).is_some() || catalog_has(entry.id.as_str()) {
                return Err(data_error(position, format!("{} is a PdfCraft id; Margin never redefines one (CU-CMD-2a)", entry.id)));
            }
            if margin.iter().any(|(known, _): &(CommandEntry, _)| known.id == entry.id) {
                return Err(data_error(position, format!("{} is listed twice", entry.id)));
            }
            let when = entry.when.as_deref().map(ContextExpr::parse).transpose().map_err(|err| data_error(position, err.to_string()))?;
            margin.push((entry, when));
        }
        Ok(Self { margin })
    }

    /// Margin's own entries, in file order.
    pub fn margin_entries(&self) -> impl Iterator<Item = &CommandEntry> {
        self.margin.iter().map(|(entry, _)| entry)
    }

    /// Looks up any registered id.
    pub fn get(&self, id: &str) -> Option<CommandInfo> {
        if let Some((entry, when)) = self.margin.iter().find(|(entry, _)| entry.id.as_str() == id) {
            return Some(CommandInfo { source: Source::Margin, in_text: entry.in_text, when: when.clone() });
        }
        if let Some(spec) = commands::command(id) {
            return Some(CommandInfo { source: Source::PdfCraft, in_text: spec.in_text, when: None });
        }
        // Catalog-only tools have no in_text flag upstream; they stay out of text fields.
        catalog_has(id).then_some(CommandInfo { source: Source::Catalog, in_text: false, when: None })
    }

    /// The title a person sees for `id`: Margin's title, PdfCraft's label, or the catalogue
    /// item's label. `None` for an unknown id.
    pub fn title(&self, id: &str) -> Option<&str> {
        if let Some((entry, _)) = self.margin.iter().find(|(entry, _)| entry.id.as_str() == id) {
            return Some(entry.title.as_str());
        }
        if let Some(spec) = commands::command(id) {
            return Some(spec.label);
        }
        catalog_items().find(|item| item.command == id).map(|item| item.label)
    }
}

fn catalog_items() -> impl Iterator<Item = &'static catalog::ToolItem> {
    catalog::TOOL_GROUPS.iter().flat_map(|group| group.sections).flat_map(|section| section.items)
}

fn catalog_has(id: &str) -> bool {
    catalog_items().any(|item| item.command == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_commands_load() {
        let registry = Registry::load().unwrap();
        assert!(registry.margin_entries().count() > 20);
        assert_eq!(registry.title("view.zoom.in"), Some("Zoom in"));
        assert_eq!(registry.title("view.palette"), Some("Find tools and commands…"));
        assert_eq!(registry.title("no.such"), None);
        assert_eq!(registry.get("view.zoom.in").unwrap().source, Source::Margin);
        assert_eq!(registry.get("view.palette").unwrap().source, Source::PdfCraft);
        assert!(!registry.get("edit.undo").unwrap().in_text, "PdfCraft's ct() leaves undo to text fields");
        assert!(registry.get("no.such").is_none());
    }

    #[test]
    fn ids_follow_cu_cmd_1() {
        for good in ["view.zoom.in", "sign.fill.signature.change", "a1.b_2"] {
            assert!(CommandId::new(good).is_ok(), "{good}");
        }
        for bad in ["view", "View.zoom", "view..zoom", "view.zoom-in", "", "view.zoom."] {
            assert!(CommandId::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn margin_ids_never_redefine_upstream_ones() {
        let err = Registry::from_text(r#"[{ "id": "view.palette", "title": "x", "group": "View" }]"#).unwrap_err();
        assert!(err.to_string().contains("PdfCraft id"), "{err}");
        let err = Registry::from_text(r#"[{ "id": "edit.thing", "title": "x", "group": "View" }]"#).unwrap_err();
        assert!(err.to_string().contains("namespaces"), "{err}");
    }
}
