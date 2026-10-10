//! `Settings`: the one source of truth for settings keys, types, ranges, defaults and descriptions
//! (CU-SET-4). Each field's doc comment is its description in the schema and its `//` comment in
//! `default-settings.jsonc`. The keys here are the ones M1 and M2 read (`view`, `reader`,
//! `ui.rail`, `keys`, `perf`); later tasks add the rest of CUSTOMIZATION §1.
//!
//! `x-margin-scope` tags each top-level block `[doc]` or `[app]` (CU-SET-3); a key inherits its
//! block's scope unless it carries its own tag.

use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

/// The settings format version written by this build (CU-VAL-4).
pub const SETTINGS_VERSION: u32 = 1;

/// The `$schema` value of a settings file: the copy Margin keeps in `<config>/schemas/`.
pub const SETTINGS_SCHEMA_URL: &str = "./schemas/settings.schema.json";

/// Every setting Margin reads. Missing keys take their default (`#[serde(default)]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// editor completion; Margin writes this folder on start
    #[serde(rename = "$schema")]
    pub schema: String,
    /// settings format version; migrations key off it (CU-VAL-4)
    #[schemars(range(min = 1, max = 1))]
    pub version: u32,
    /// how a document is shown
    #[schemars(extend("x-margin-scope" = "doc"))]
    pub view: View,
    /// behaviour around reading
    #[schemars(extend("x-margin-scope" = "app"))]
    pub reader: Reader,
    /// the app chrome
    #[schemars(extend("x-margin-scope" = "app"))]
    pub ui: Ui,
    /// keymap behaviour; bindings live in keymap.jsonc
    #[schemars(extend("x-margin-scope" = "app"))]
    pub keys: Keys,
    /// rendering and cache budgets; semantics live in PERF-AND-IPC.md
    #[schemars(extend("x-margin-scope" = "app"))]
    pub perf: Perf,
    /// the library: folders scanned for PDFs and how the shelves show
    #[schemars(extend("x-margin-scope" = "app"))]
    pub library: Library,
    /// reading statistics in stats.sqlite
    #[schemars(extend("x-margin-scope" = "app"))]
    pub stats: Stats,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: SETTINGS_SCHEMA_URL.to_owned(),
            version: SETTINGS_VERSION,
            view: View::default(),
            reader: Reader::default(),
            ui: Ui::default(),
            keys: Keys::default(),
            perf: Perf::default(),
            library: Library::default(),
            stats: Stats::default(),
        }
    }
}

/// How pages are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// One continuous column of pages.
    Scroll,
    /// One page at a time.
    Single,
    /// Two pages side by side.
    TwoUp,
}

/// How the page is sized to the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Page width fills the window.
    Width,
    /// The whole page fits in the window.
    Page,
    /// The page is `view.reading_width_px` wide.
    Read,
    /// The width after trim fills the window.
    Content,
    /// `view.zoom` percent.
    Zoom,
}

/// The page colour mode (PERF-O2 `ImageOps`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PageColor {
    /// The page as printed.
    Paper,
    /// Warm paper tones.
    Sepia,
    /// Dark page, light ink; figures keep night colours.
    Night,
    /// Two-tone dark page.
    Ink,
    /// Black and white.
    Mono,
}

/// The view block (`[doc]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct View {
    /// "scroll" | "single" | "two_up"
    pub layout: Layout,
    /// two_up: page 1 sits alone on the right, like a book (mockup "cover1")
    pub cover_alone: bool,
    /// "width" | "page" | "read" (reading width) | "content" (width after trim) | "zoom"
    pub fit: Fit,
    /// page width for fit "read"; shrinks to fit narrow windows (mockup --pw)
    #[schemars(range(min = 200, max = 4000))]
    pub reading_width_px: u32,
    /// percent, used when fit is "zoom"; 8..6400, the page view's own zoom limits
    #[schemars(range(min = 8, max = 6400))]
    pub zoom: u32,
    /// stops for view.zoom.in/out, the footer's − + and its zoom menu (mockup ZOOMS); each 8..6400
    #[schemars(length(min = 1, max = 64), inner(range(min = 8, max = 6400)))]
    pub zoom_steps: Vec<u32>,
    /// space between pages and between two_up columns
    #[schemars(range(max = 200))]
    pub page_gap_px: u32,
    /// crop the white border around the text
    pub trim: bool,
    /// "paper" | "sepia" | "night" | "ink" | "mono"
    pub page_color: PageColor,
    /// night when the OS is dark, paper when light; a manual pick sets false for that document
    pub follow_system: bool,
    /// ink and mono: figure regions get night colours instead of two-tone (ImageOps.figures, PERF-O2)
    pub keep_figures: bool,
    /// percent; 70..115 (ImageOps.brightness = value / 100)
    #[schemars(range(min = 70, max = 115))]
    pub brightness: u32,
    /// percent; 70..160, around mid-grey (ImageOps.contrast = value / 100)
    #[schemars(range(min = 70, max = 160))]
    pub contrast: u32,
    /// 0.5..2.0; above 1 darkens faded scans
    #[schemars(range(min = 0.5, max = 2.0))]
    pub gamma: f64,
    /// percent; 0 = greyscale; 0..100
    #[schemars(range(max = 100))]
    pub saturation: u32,
    /// mono cut-off, 0..1, used only by mono; lower keeps more ink
    #[schemars(range(min = 0.0, max = 1.0))]
    pub mono_threshold: f64,
    /// 0 = off, else 0.5..1: brighter pixels become white (scan cleanup)
    #[schemars(schema_with = "white_threshold_schema")]
    pub white_threshold: f64,
    /// 0 = off, else 0.5..0.95: light grey, low-chroma pixels become white
    #[schemars(schema_with = "dewatermark_schema")]
    pub dewatermark: f64,
    /// j/k and arrow step in scroll layout
    #[schemars(range(min = 1, max = 2000))]
    pub scroll_line_px: u32,
    /// Space/PgDn step as a fraction of the window height; 0.1..1
    #[schemars(range(min = 0.1, max = 1.0))]
    pub scroll_screen: f64,
    /// after a screen step, mark where the previous screen ended (KOReader)
    pub overlap_marker: bool,
    /// animate screen steps (Space, Page Down); off under ui.reduce_motion
    pub smooth_scroll: bool,
}

/// The mockup's zoom stops (`ZOOMS`), percent.
const DEFAULT_ZOOM_STEPS: [u32; 12] = [50, 67, 75, 90, 100, 110, 125, 150, 175, 200, 250, 300];

impl Default for View {
    fn default() -> Self {
        Self {
            layout: Layout::Scroll,
            cover_alone: false,
            fit: Fit::Read,
            reading_width_px: 820,
            zoom: 100,
            zoom_steps: DEFAULT_ZOOM_STEPS.to_vec(),
            page_gap_px: 10,
            trim: false,
            page_color: PageColor::Night,
            follow_system: true,
            keep_figures: true,
            brightness: 100,
            contrast: 100,
            gamma: 1.0,
            saturation: 100,
            mono_threshold: 0.5,
            white_threshold: 0.0,
            dewatermark: 0.0,
            scroll_line_px: 80,
            scroll_screen: 0.85,
            overlap_marker: true,
            smooth_scroll: true,
        }
    }
}

/// `0` (off) or a number in `min..=max`.
fn off_or_range(min: f64, max: f64) -> Schema {
    json_schema!({
        "type": "number",
        "anyOf": [{ "const": 0 }, { "minimum": min, "maximum": max }]
    })
}

fn white_threshold_schema(_: &mut SchemaGenerator) -> Schema {
    off_or_range(0.5, 1.0)
}

fn dewatermark_schema(_: &mut SchemaGenerator) -> Schema {
    off_or_range(0.5, 0.95)
}

/// What Margin shows on start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Start {
    /// Reopen the last file at its page.
    LastDocument,
    /// The recent list; the library once it exists.
    Home,
    /// Every file open at last quit (FEATURES F-open-16, P2).
    Session,
}

/// Find options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FindOptions {
    /// match case
    pub case: bool,
    /// match whole words only
    pub whole_word: bool,
    /// regular expressions; needs an engine change
    pub regex: bool,
}

/// The reader block (`[app]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Reader {
    /// store view changes in the sidecar and reopen at the same page
    pub remember_per_document: bool,
    /// "last_document" (reopen the last file at its page) | "home" (recent list; the library once it exists) | "session" (every file open at last quit, FEATURES F-open-16, P2)
    pub start: Start,
    /// jumps kept for reader.back / reader.forward
    #[schemars(range(max = 10000))]
    pub back_stack: u32,
    /// show "Back to p. N" after a link or search jump
    pub back_pill: bool,
    /// hovering a link previews its target
    pub link_preview: bool,
    /// show the colour/note menu when text is selected
    pub selection_popover: bool,
    /// footer shows section and time left from your reading speed
    pub time_left: bool,
    /// reload when the file changes on disk; page, zoom and colour kept
    pub reload_on_change: bool,
    /// wait after the last disk change (Okular uses 750)
    #[schemars(range(max = 60000))]
    pub reload_debounce_ms: u32,
    /// writable PDFs: edits are saved incrementally about 1 s after the last change; false = Ctrl+S only. Never writes on open. Sidecars always autosave.
    pub autosave: bool,
    /// undo depth per document
    #[schemars(range(max = 10000))]
    pub undo_steps: u32,
    /// find options; regex needs an engine change
    pub find: FindOptions,
}

impl Default for Reader {
    fn default() -> Self {
        Self {
            remember_per_document: true,
            start: Start::LastDocument,
            back_stack: 100,
            back_pill: true,
            link_preview: true,
            selection_popover: true,
            time_left: true,
            reload_on_change: true,
            reload_debounce_ms: 750,
            autosave: true,
            undo_steps: 100,
            find: FindOptions::default(),
        }
    }
}

/// The UI block (`[app]`): the rail and motion so far; the rest of CUSTOMIZATION §1 `ui` lands
/// with the tasks that read it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// annotate rail, visible in annotate mode (mockup:1685)
    pub rail: Rail,
    /// true = no animation: the sidebar, rail, popovers, hover fills, progress bars and screen steps change at once
    pub reduce_motion: bool,
}

/// Which side the rail sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RailSide {
    /// The left window edge.
    Left,
    /// The right window edge.
    Right,
}

/// Options of one rail tool (CU-UI-2). Every field has a default because each settings layer is
/// partial and deep-merges (CU-SET-2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ToolOptions {
    /// a notes.colors id
    pub color: String,
    /// stroke width in pt, for width tools
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.25, max = 20.0))]
    pub width: Option<f64>,
}

/// The colour of a tool with no options: `notes.default_color`'s default (CUSTOMIZATION §1).
const DEFAULT_TOOL_COLOR: &str = "yellow";

impl Default for ToolOptions {
    fn default() -> Self {
        Self { color: DEFAULT_TOOL_COLOR.to_owned(), width: None }
    }
}

/// The annotate rail (CU-UI-1, CU-UI-2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Rail {
    /// command ids; "|" = divider
    pub items: Vec<String>,
    /// "left" | "right"
    pub side: RailSide,
    /// per tool: color = notes.colors id, width in pt (mockup:1698)
    pub tool_options: BTreeMap<String, ToolOptions>,
}

/// The mockup's default rail (`RAIL_DEF`).
const DEFAULT_RAIL: [&str; 11] = [
    "reader.tool.select",
    "edit.snapshot",
    "comment.highlight",
    "comment.underline",
    "comment.note",
    "comment.ink",
    "comment.freetext",
    "comment.eraser",
    "|",
    "edit.undo",
    "edit.redo",
];

/// Default stroke of the pen tool, in pt (mockup:1698).
const DEFAULT_INK_WIDTH_PT: f64 = 2.0;

impl Default for Rail {
    fn default() -> Self {
        let tool = |color: &str, width: Option<f64>| ToolOptions { color: color.to_owned(), width };
        Self {
            items: DEFAULT_RAIL.iter().map(|&id| id.to_owned()).collect(),
            side: RailSide::Right,
            tool_options: BTreeMap::from([
                ("comment.highlight".to_owned(), tool("yellow", None)),
                ("comment.underline".to_owned(), tool("red", None)),
                ("comment.note".to_owned(), tool("note", None)),
                ("comment.ink".to_owned(), tool("yellow", Some(DEFAULT_INK_WIDTH_PT))),
                ("comment.freetext".to_owned(), tool("yellow", None)),
            ]),
        }
    }
}

/// The base keymap under the user's `keymap.jsonc` (CU-KEY-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeymapBase {
    /// The built-in keymap only.
    Margin,
    /// Margin plus the Okular keys; not shipped yet, so the built-in keymap alone.
    Okular,
    /// Margin plus the vim-like Sioyek keys; not shipped yet, so the built-in keymap alone.
    Sioyek,
    /// Only the user's keymap file (PdfCraft's own shortcuts still work for ids it leaves unbound).
    None,
}

/// The keys block (`[app]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Keys {
    /// "margin" | "none" (only keymap.jsonc); "okular" and "sioyek" are not shipped yet and act as "margin"
    pub base: KeymapBase,
    /// wait for the next key of a chord (Zed uses 1 s)
    #[schemars(range(min = 100, max = 10000))]
    pub chord_timeout_ms: u32,
    /// in annotate mode bare letters pick rail tools (mockup Decision 6)
    pub annotate_letters: bool,
    /// show pending chord keys in the footer
    pub show_pending: bool,
}

impl Default for Keys {
    fn default() -> Self {
        Self { base: KeymapBase::Margin, chord_timeout_ms: 1000, annotate_letters: true, show_pending: true }
    }
}

/// How the library lists books.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LibraryView {
    /// Covers in a grid.
    #[default]
    Grid,
    /// One row per book.
    List,
}

/// The library's sort order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LibrarySort {
    /// Last opened first, then last added.
    #[default]
    Recent,
    /// By title.
    Title,
    /// By author.
    Author,
    /// Most read first.
    Progress,
    /// Last added first.
    Added,
}

/// The library block (`[app]`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Library {
    /// folders scanned for PDFs (with their subfolders) on start and on Rescan; absolute paths
    pub folders: Vec<String>,
    /// "grid" | "list"
    pub view: LibraryView,
    /// "recent" | "title" | "author" | "progress" | "added"
    pub sort: LibrarySort,
}

/// The stats block (`[app]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Stats {
    /// record reading sessions and time per page in stats.sqlite
    pub enabled: bool,
}

impl Default for Stats {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// The tile image format (PERF-R6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TileFormat {
    /// Lossless WebP.
    Webp,
    /// PNG, the fallback.
    Png,
}

/// The perf block (`[app]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Perf {
    /// 0 = clamp(cores - 2, 2, 8)
    #[schemars(range(max = 64))]
    pub render_threads: u32,
    /// device px; used only when a page's long side exceeds 4096 px, else one tile per page
    #[schemars(range(min = 256, max = 4096))]
    pub tile_px: u32,
    /// "webp" | "png"
    pub tile_format: TileFormat,
    /// 0 = clamp(RAM / 32, 128, 512)
    #[schemars(range(max = 65536))]
    pub ram_cache_mb: u32,
    /// thumbnails and covers only; no disk tile cache
    #[schemars(range(max = 65536))]
    pub disk_cache_mb: u32,
    /// glyph and line boxes
    #[schemars(range(max = 65536))]
    pub text_cache_mb: u32,
    /// ahead in the scroll direction; half that behind
    #[schemars(range(max = 10))]
    pub prefetch_screens: u32,
    /// thumbnail width in CSS px, rendered at device pixel ratio
    #[schemars(range(min = 32, max = 1024))]
    pub thumb_px: u32,
}

impl Default for Perf {
    fn default() -> Self {
        Self {
            render_threads: 0,
            tile_px: 1024,
            tile_format: TileFormat::Webp,
            ram_cache_mb: 0,
            disk_cache_mb: 512,
            text_cache_mb: 64,
            prefetch_screens: 1,
            thumb_px: 160,
        }
    }
}
