//! `Settings`: the one source of truth for settings keys, types, ranges, defaults and descriptions
//! (CU-SET-4). Each field's doc comment is its description in the schema and its `//` comment in
//! `default-settings.jsonc`. Only settings with a runtime reader belong here.
//!
//! `x-margin-scope` tags each top-level block `[doc]` or `[app]` (CU-SET-3); a key inherits its
//! block's scope unless it carries its own tag.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::theme::{Appearance, DEFAULT_THEME, LIGHT_THEME};

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
    /// settings format version supported by this build
    #[schemars(range(min = 1, max = 1))]
    pub version: u32,
    /// the theme: a theme name, used always, or { "mode": "system" | "light" | "dark", "light": name, "dark": name }; built-ins "Margin Dark" and "Margin Light", yours in themes.json and themes/*.json
    #[schemars(extend("x-margin-scope" = "app"))]
    pub theme: ThemeSetting,
    /// style keys over a theme, by theme name: { "Margin Dark": { "text.accent": "#4b8bf5" } }; schemas/theme.schema.json lists the keys
    #[schemars(extend("x-margin-scope" = "app"))]
    pub theme_overrides: BTreeMap<String, ThemeStyle>,
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
    /// the library: folders scanned for PDFs and how the shelves show
    #[schemars(extend("x-margin-scope" = "app"))]
    pub library: Library,
    /// reading statistics in stats.sqlite
    #[schemars(extend("x-margin-scope" = "app"))]
    pub stats: Stats,
    /// reading notes, highlights, snapshots and their export format
    #[schemars(extend("x-margin-scope" = "app"))]
    pub notes: Notes,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: SETTINGS_SCHEMA_URL.to_owned(),
            version: SETTINGS_VERSION,
            theme: ThemeSetting::default(),
            theme_overrides: BTreeMap::new(),
            view: View::default(),
            reader: Reader::default(),
            ui: Ui::default(),
            keys: Keys::default(),
            library: Library::default(),
            stats: Stats::default(),
            notes: Notes::default(),
        }
    }
}

/// The `theme` setting, as in Zed: one theme always, or one per appearance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ThemeSetting {
    /// One theme, whatever the system's appearance.
    Named(String),
    /// A light and a dark theme, and which one shows.
    ByMode(ThemeModes),
}

impl Default for ThemeSetting {
    fn default() -> Self {
        Self::ByMode(ThemeModes::default())
    }
}

impl ThemeSetting {
    /// The theme to show when the system is dark (`system_dark` true), light (false) or says
    /// nothing (`None`, which shows the dark theme), and the slot it fills (dark for a lone name).
    pub fn pick(&self, system_dark: Option<bool>) -> (&str, Appearance) {
        match self {
            Self::Named(name) => (name, Appearance::Dark),
            Self::ByMode(modes) => match (modes.mode, system_dark) {
                (ThemeMode::Light, _) | (ThemeMode::System, Some(false)) => (&modes.light, Appearance::Light),
                (ThemeMode::Dark, _) | (ThemeMode::System, Some(true) | None) => (&modes.dark, Appearance::Dark),
            },
        }
    }
}

/// Which theme of [`ThemeModes`] shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    /// Follow the system's light or dark appearance.
    System,
    /// Always the light theme.
    Light,
    /// Always the dark theme.
    #[default]
    Dark,
}

impl ThemeMode {
    /// The value as written in settings.jsonc.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// `theme` as `{ "mode", "light", "dark" }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeModes {
    /// "system" | "light" | "dark"
    pub mode: ThemeMode,
    /// the theme shown in light mode
    pub light: String,
    /// the theme shown in dark mode
    pub dark: String,
}

impl Default for ThemeModes {
    fn default() -> Self {
        Self { mode: ThemeMode::Dark, light: LIGHT_THEME.to_owned(), dark: DEFAULT_THEME.to_owned() }
    }
}

/// One theme's style keys in `theme_overrides`, as in a theme file's `style`. Loading checks every
/// value; what reaches [`Settings`] is valid.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThemeStyle(pub Map<String, Value>);

impl JsonSchema for ThemeStyle {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ThemeStyle".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        // `style_schema` is always an object, so the conversion cannot fail.
        Schema::try_from(crate::theme::style_schema()).unwrap_or_default()
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

/// The page colour mode.
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
    /// night when the theme in use is dark, paper when it is light; a manual pick sets false for that document
    pub follow_system: bool,
    /// ink and mono: photos the page embeds (and photo-like areas of scans) keep their colours as in night instead of two-tone; vector drawings stay two-tone
    pub keep_figures: bool,
    /// percent; 70..115
    #[schemars(range(min = 70, max = 115))]
    pub brightness: u32,
    /// percent; 70..160, around mid-grey
    #[schemars(range(min = 70, max = 160))]
    pub contrast: u32,
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
            scroll_line_px: 80,
            scroll_screen: 0.85,
            overlap_marker: true,
            smooth_scroll: true,
        }
    }
}

/// Find options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FindOptions {
    /// match case
    pub case: bool,
    /// match whole words only
    pub whole_word: bool,
}

/// The reader block (`[app]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Reader {
    /// store view changes in the sidecar and reopen at the same page
    pub remember_per_document: bool,
    /// jumps kept for reader.back / reader.forward
    #[schemars(range(max = 10000))]
    pub back_stack: u32,
    /// show "Back to p. N" after a link or search jump
    pub back_pill: bool,
    /// hovering a link previews its target
    pub link_preview: bool,
    /// show the colour/note menu when text is selected
    pub selection_popover: bool,
    /// footer estimates the time left in the document from your recorded reading pace
    pub time_left: bool,
    /// reload when the file changes on disk (a LaTeX build); place, zoom, page colour and history kept. Unsaved edits are never dropped: Margin asks
    pub reload_on_change: bool,
    /// wait this long after the last disk change before reloading (Okular uses 750)
    #[schemars(range(max = 60000))]
    pub reload_debounce_ms: u32,
    /// SyncTeX inverse search: the editor command opening a source line, run without a shell; {file}, {line}, {column} are filled in, quotes group words. "" = off. E.g. "code -g {file}:{line}", "zed {file}:{line}", "nvim --server /tmp/nvim.sock --remote-send '<C-\\><C-N>:e {file}<CR>:{line}<CR>'"
    pub synctex_editor: String,
    /// which click on a page runs reader.synctex_editor (secondary = Cmd on macOS, Ctrl elsewhere)
    pub synctex_click: SynctexClick,
    /// writable PDFs: edits are saved incrementally about 1 s after the last change; false = Ctrl+S only. Never writes on open. Sidecars always autosave.
    pub autosave: bool,
    /// undo depth per document
    #[schemars(range(max = 10000))]
    pub undo_steps: u32,
    /// initial find options
    pub find: FindOptions,
}

impl Default for Reader {
    fn default() -> Self {
        Self {
            remember_per_document: true,
            back_stack: 100,
            back_pill: true,
            link_preview: true,
            selection_popover: true,
            time_left: true,
            reload_on_change: true,
            reload_debounce_ms: 750,
            synctex_editor: String::new(),
            synctex_click: SynctexClick::SecondaryClick,
            autosave: true,
            undo_steps: 100,
            find: FindOptions::default(),
        }
    }
}

/// The click that runs SyncTeX inverse search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SynctexClick {
    /// Cmd+click on macOS, Ctrl+click elsewhere.
    SecondaryClick,
    /// Cmd+Shift+click on macOS (Skim's), Ctrl+Shift+click elsewhere.
    SecondaryShiftClick,
    /// Alt+click (Option+click on macOS).
    AltClick,
}

/// App chrome and motion.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// left pane
    pub sidebar: Sidebar,
    /// annotate rail, visible in annotate mode (mockup:1685)
    pub rail: Rail,
    /// true = no animation: the sidebar, rail, popovers, hover fills, progress bars and screen steps change at once
    pub reduce_motion: bool,
    /// Linux: "margin" = Margin's header row is the title bar, with minimize, maximize and close at its right; "system" = the desktop's title bar and frame. Other systems ignore it
    pub titlebar: Titlebar,
}

/// Who draws the window's title bar on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Titlebar {
    /// Margin: the header row moves the window, its own buttons minimize, maximize and close it.
    #[default]
    Margin,
    /// The desktop's title bar and window frame.
    System,
}

/// The sidebar (CU-UI-3). `visible` and `tab` are the first run's choice only: afterwards Margin
/// reopens the sidebar as it was left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Sidebar {
    /// open on the first start; later starts reopen it as it was left
    pub visible: bool,
    /// the first start's tab; later starts reopen the last tab used (never Find)
    pub tab: SidebarTab,
    /// windows narrower than this get the sidebar over the page instead of beside it; once over the page it docks again only 48 px wider, so a window edge dragged across the width doesn't flip it; 0 = always beside
    #[schemars(range(max = 10_000))]
    pub overlay_below_px: u32,
}

/// The window width under which the sidebar floats over the page (CUSTOMIZATION §1).
const DEFAULT_OVERLAY_BELOW_PX: u32 = 1100;

impl Default for Sidebar {
    fn default() -> Self {
        Self { visible: true, tab: SidebarTab::Contents, overlay_below_px: DEFAULT_OVERLAY_BELOW_PX }
    }
}

/// A sidebar tab a reader can start on (Find opens with a search).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SidebarTab {
    /// The table of contents.
    Contents,
    /// The highlight cards.
    Highlights,
    /// The page thumbnails.
    Pages,
    /// Page bookmarks and the PDF's bookmarks.
    Bookmarks,
    /// The tool catalogue.
    Tools,
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

/// The annotate rail (CU-UI-1, CU-UI-2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Rail {
    /// command ids; "|" = divider; unknown ids are skipped
    #[schemars(length(max = 64))]
    pub items: Vec<String>,
    /// "left" | "right"
    pub side: RailSide,
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

impl Default for Rail {
    fn default() -> Self {
        Self { items: DEFAULT_RAIL.iter().map(|&id| id.to_owned()).collect(), side: RailSide::Right }
    }
}

/// The base keymap under the user's `keymap.jsonc` (CU-KEY-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeymapBase {
    /// The built-in keymap only.
    Margin,
    /// Only the user's keymap file (PdfCraft's own shortcuts still work for ids it leaves unbound).
    None,
}

/// The keys block (`[app]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Keys {
    /// "margin" | "none" (only keymap.jsonc)
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
    /// look up the details (title, authors, year, venue, abstract) of books with a DOI or arXiv id
    /// online, on their own, after each scan: their ids go to api.crossref.org and export.arxiv.org.
    /// Off: nothing leaves this computer unless you click "Look up details" in a book's details
    pub metadata_lookup: bool,
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

/// A named highlight colour in the notes palette. A colour without a name is ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct NoteColor {
    /// The colour name used by Margin commands.
    #[schemars(length(min = 1, max = 64))]
    pub name: String,
    /// Red, green and blue channels, each from 0 to 255.
    pub rgb: [u8; 3],
}

/// Where notes are exported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportTarget {
    /// Obsidian Markdown with `obsidian://` links.
    #[default]
    Obsidian,
    /// Plain Markdown.
    Markdown,
    /// Logseq Markdown.
    Logseq,
    /// Readwise CSV.
    ReadwiseCsv,
    /// JSON.
    Json,
}

/// How an existing note is changed by an export.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NoteUpdateMode {
    /// Replace Margin-managed content in the note.
    #[default]
    Update,
    /// Add new content to the end of the note.
    Append,
}

/// Margin's Obsidian and notes export settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Notes {
    /// The Obsidian vault folder, as a full path; `null` or `""` turns vault notes off. A relative
    /// path is not used: the notes panel says why.
    #[serde(deserialize_with = "deserialize_vault")]
    pub vault: Option<PathBuf>,
    /// Vault-relative reading-note path. Common placeholders: `title`, `title-slug`, `authors`,
    /// `year`, `doi`, `arxiv`, `citekey`, `tags`, `date`, `progress`, `page`, `color`,
    /// `color-name`, `text`, `note` and `link`. Values are slugged for paths and substituted once
    /// as literals; unknown or missing placeholders become empty.
    pub note_template: String,
    /// YAML front matter template. It accepts the common placeholders and `source`; values are
    /// JSON/YAML-quoted and escaped before one-pass literal substitution. Unknown or missing
    /// values are empty.
    pub front_matter_template: String,
    /// Optional note header template. It accepts the common placeholders plus `source` and
    /// `callout`; substitutions are one-pass literals, and unknown or missing values are empty.
    pub header_template: String,
    /// Highlight template. It accepts the common placeholders plus `source` and `callout`;
    /// `{{callout}}` is the safely formatted callout. Substitution is one-pass and unknown or
    /// missing values are empty.
    pub highlight_template: String,
    /// Named RGB palette; the first colour is the default.
    #[schemars(length(min = 1, max = 64))]
    pub colors: Vec<NoteColor>,
    /// Default format for note exports.
    pub export_target: ExportTarget,
    /// Folder under the vault where snapshots are stored.
    pub snapshot_folder: String,
    /// How exports change an existing note.
    pub update_mode: NoteUpdateMode,
}

fn deserialize_vault<'de, D>(deserializer: D) -> Result<Option<PathBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let vault = Option::<PathBuf>::deserialize(deserializer)?;
    Ok(vault.filter(|path| !path.as_os_str().is_empty()))
}

const DEFAULT_NOTE_COLORS: [(&str, [u8; 3]); 4] =
    [("Yellow", [0xff, 0xd0, 0x00]), ("Red", [0xea, 0x52, 0x52]), ("Note", [0x08, 0x6d, 0xdd]), ("Important", [0xbb, 0x61, 0xe5])];

impl Default for Notes {
    fn default() -> Self {
        Self {
            vault: None,
            note_template: "reading/{{title-slug}}.md".to_owned(),
            front_matter_template: "---\nsource: \"{{source}}\"\n---\n\n".to_owned(),
            header_template: String::new(),
            highlight_template: "{{callout}}".to_owned(),
            colors: DEFAULT_NOTE_COLORS.iter().map(|(name, rgb)| NoteColor { name: (*name).to_owned(), rgb: *rgb }).collect(),
            export_target: ExportTarget::Obsidian,
            snapshot_folder: "attachments".to_owned(),
            update_mode: NoteUpdateMode::Update,
        }
    }
}
