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
//! Sidecars: [`Sidecar`] is the v1 file format and [`store`] reads and writes it; [`marks`]
//! merges two writers' highlights, notes and bookmarks and writes them into a PDF copy.
//!
//! Themes: [`theme`] reads theme families in Zed's format (`<config>/themes.json`,
//! `<config>/themes/*.json`), checks their style keys and resolves `extends`. The profile file
//! format is here too; nothing loads it yet.
//!
//! Library and stats (desktop only): [`library`] keeps `<data>/library.sqlite` (books, covers,
//! collections, tags, bibliographic details) and scans folders as a cancellable job;
//! [`library_meta`] edits a book's details, imports BibTeX and Zotero libraries and looks papers
//! up online when asked; [`library_text`] keeps the full-text index; [`stats`] keeps
//! `<data>/stats.sqlite` (reading sessions, time per page) and turns it into summaries and a
//! calendar. [`ids`], [`bibtex`], [`zotero`] and [`lookup`] read identifiers and references.
//!
//! Nothing here depends on egui, and no config file can crash it.

pub mod bibtex;
mod context;
mod edit;
mod error;
pub mod ids;
mod jsonc;
mod keymap;
mod keystroke;
#[cfg(not(target_arch = "wasm32"))]
pub mod library;
#[cfg(not(target_arch = "wasm32"))]
pub mod library_meta;
#[cfg(not(target_arch = "wasm32"))]
mod library_schema;
#[cfg(not(target_arch = "wasm32"))]
pub mod library_text;
mod load;
#[cfg(not(target_arch = "wasm32"))]
pub mod lookup;
pub mod marks;
#[cfg(not(target_arch = "wasm32"))]
mod migrate;
mod problem;
mod profile;
pub mod reference;
mod registry;
pub mod resolve;
pub mod schema;
pub mod settings;
mod sidecar;
#[cfg(not(target_arch = "wasm32"))]
pub mod stats;
pub mod store;
pub mod theme;
mod validate;
#[cfg(test)]
mod vectors;
#[cfg(not(target_arch = "wasm32"))]
pub mod zotero;

pub use context::{ContextExpr, ContextNode, depth_of, holds, parse_stack};
pub use edit::{set_user_values, set_user_values_if_missing, with_values};
pub use error::{Error, Result};
pub use keymap::{
    BindingTarget, CompileOptions, Compiled, CompiledBinding, CompiledKeymap, KEYMAP_FILE, KeymapBlock, KeymapFile, KeymapSource, MARGIN_KEYMAP,
    MARGIN_KEYMAP_FILE, MAX_KEYMAP_BYTES, compile, compile_shipped,
};
pub use keystroke::{Os, canonical, canonical_chord, is_printable};
pub use load::{CONFIG_DIR_ENV, LoadedSettings, MAX_SETTINGS_BYTES, SETTINGS_FILE, config_dir, read_capped};
pub use problem::{Problem, Severity};
pub use profile::{Profile, ProfileMatch};
pub use registry::{CommandEntry, CommandId, CommandInfo, Group, MARGIN_NAMESPACES, Registry, Source};
pub use settings::{
    ExportTarget, FindOptions, Fit, KeymapBase, Keys, Layout, Library, LibrarySort, LibraryView, NoteColor, NoteUpdateMode, Notes, PageColor, Rail,
    RailSide, Reader, SETTINGS_SCHEMA_URL, SETTINGS_VERSION, Settings, Sidebar, SidebarTab, Stats, SynctexClick, ThemeMode, ThemeModes, ThemeSetting,
    ThemeStyle, Ui, View,
};
pub use sidecar::{
    Bookmark, Highlight, HighlightSource, HighlightStyle, Locator, PageNote, PdfAnnot, Progress, Sidecar, SidecarDoc, SidecarFormat, SidecarObsidian,
    Trim, TrimMargins,
};
pub use theme::{Appearance, DEFAULT_THEME, LIGHT_THEME, THEME_SCHEMA_FILE, THEMES_DIR, THEMES_FILE};
