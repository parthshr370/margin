//! `margin-config`: Margin's settings and keymap files.
//!
//! Settings: [`Settings`] is the single authority for keys, types, ranges, defaults and
//! descriptions. [`Settings::load`] merges `<config>/settings.jsonc` over the defaults and never
//! fails: every bad value falls back to the default and is reported as a [`Problem`].
//! [`schema`] generates the JSON schemas and the commented defaults file.
//!
//! Keymap: [`Registry`] knows every bindable id (PdfCraft's `COMMANDS` and `TOOL_GROUPS`, reused
//! unchanged, plus Margin's own ids in `data/commands.jsonc`). [`compile`] turns Zed-format
//! keymap files (the shipped [`MARGIN_KEYMAP`], then the user's `keymap.jsonc`) into one
//! [`CompiledKeymap`]; [`resolve::step`] matches a keystroke against it for a context stack.
//!
//! Theme, profile and sidecar file formats are here too; nothing loads them yet.
//!
//! Nothing here depends on egui, and no config file can crash it.

mod context;
mod error;
mod jsonc;
mod keymap;
mod keystroke;
mod load;
mod problem;
mod profile;
mod registry;
pub mod resolve;
pub mod schema;
mod settings;
mod sidecar;
mod theme;
mod validate;
#[cfg(test)]
mod vectors;

pub use context::{ContextExpr, ContextNode, depth_of, holds, parse_stack};
pub use error::{Error, Result};
pub use keymap::{
    BindingTarget, CompileOptions, Compiled, CompiledBinding, CompiledKeymap, KEYMAP_FILE, KeymapBlock, KeymapFile, KeymapSource,
    MARGIN_KEYMAP, MARGIN_KEYMAP_FILE, MAX_KEYMAP_BYTES, compile, compile_shipped,
};
pub use keystroke::{Os, canonical, canonical_chord, is_printable};
pub use load::{CONFIG_DIR_ENV, LoadedSettings, MAX_SETTINGS_BYTES, SETTINGS_FILE, config_dir, read_capped};
pub use problem::{Problem, Severity};
pub use profile::{Profile, ProfileMatch};
pub use registry::{CommandEntry, CommandId, CommandInfo, Group, MARGIN_NAMESPACES, Registry, Source};
pub use settings::{
    FindOptions, Fit, KeymapBase, Keys, Layout, PageColor, Perf, Rail, RailSide, Reader, SETTINGS_SCHEMA_URL, SETTINGS_VERSION, Settings, Start,
    TileFormat, ToolOptions, Ui, View,
};
pub use sidecar::{Bookmark, Highlight, HighlightSource, HighlightStyle, Locator, PageNote, PdfAnnot, Progress, Sidecar, SidecarDoc, SidecarFormat};
pub use theme::{Appearance, DEFAULT_THEME, Theme};
