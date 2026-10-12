//! Themes (CU-THM-1), in Zed's theme family format. A family file holds a list of themes; each
//! has a name, an appearance, the theme it starts from (`extends`) and a flat `style` map of
//! dotted keys to colours. Margin reads Zed's own key names wherever a Margin role matches one
//! (`background`, `text.accent`, …) and keeps its own roles under `margin.`; every other key of
//! Zed's schema (syntax, terminal, players, …) is accepted and has no effect, so a Zed theme file
//! loads as it is.
//!
//! Themes come from the built-ins ([`DEFAULT_THEME`], [`LIGHT_THEME`]), then
//! `<config>/themes.json`, then `<config>/themes/*.json` in file-name order. A name defined again
//! is reported and the later definition wins. Reading never fails: a bad file, theme or value is
//! reported as a [`Problem`] and left out, so the theme below it shows through.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::jsonc::{self, Positions, escape_pointer};
use crate::load::{MAX_SETTINGS_BYTES, read_capped};
use crate::problem::{Problem, Severity};

/// The built-in dark theme: Margin's own Black palette, and what an unset `theme` shows.
pub const DEFAULT_THEME: &str = "Margin Dark";
/// The built-in light theme.
pub const LIGHT_THEME: &str = "Margin Light";
/// The user's theme family inside `<config>`.
pub const THEMES_FILE: &str = "themes.json";
/// The folder inside `<config>` for drop-in theme families (Zed theme files work here).
pub const THEMES_DIR: &str = "themes";
/// The theme schema's file name inside `<config>/schemas/`.
pub const THEME_SCHEMA_FILE: &str = "theme.schema.json";
/// The `$schema` value of `<config>/themes.json`.
pub const THEMES_SCHEMA_URL: &str = "./schemas/theme.schema.json";
/// The largest `margin.radius`: half a control's height.
pub const MAX_RADIUS: u8 = 16;
/// Theme files read from `<config>/themes/`; the rest are reported and ignored.
const MAX_THEME_FILES: usize = 256;
/// Themes read from one family; the rest are reported and ignored.
const MAX_THEMES_PER_FILE: usize = 256;
/// Longest `extends` chain followed before it counts as a loop (STD-RS-06).
const MAX_EXTENDS_DEPTH: usize = 64;
/// What a colour value looks like, for the schema.
const COLOR_PATTERN: &str = "^#([0-9a-fA-F]{6}|[0-9a-fA-F]{8})$";
/// Hex digits per channel and channels in `#rrggbb` and `#rrggbbaa`.
const HEX_PER_CHANNEL: usize = 2;
const OPAQUE_CHANNELS: usize = 3;
const ALPHA_CHANNELS: usize = 4;

/// Light or dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    /// Used when `theme.mode` resolves to dark.
    Dark,
    /// Used when `theme.mode` resolves to light.
    Light,
}

impl Appearance {
    /// The built-in theme of this appearance: what a theme without `extends` starts from.
    pub fn builtin(self) -> &'static str {
        match self {
            Appearance::Dark => DEFAULT_THEME,
            Appearance::Light => LIGHT_THEME,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "dark" => Some(Appearance::Dark),
            "light" => Some(Appearance::Light),
            _ => None,
        }
    }
}

/// The appearance of the built-in theme called `name`, if there is one.
pub fn builtin_appearance(name: &str) -> Option<Appearance> {
    [Appearance::Dark, Appearance::Light].into_iter().find(|appearance| appearance.builtin() == name)
}

macro_rules! style_keys {
    ($($variant:ident = $name:literal: $description:literal,)*) => {
        /// A style key Margin draws with. Declaration order is the order keys apply in, so a key
        /// that sets a whole family of roles (`text.accent`) comes before the keys that refine
        /// one of them (`margin.heading`).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum StyleKey {
            $(#[doc = $description] $variant,)*
        }

        impl StyleKey {
            /// Every key, in the order they apply.
            pub const ALL: &'static [StyleKey] = &[$(StyleKey::$variant,)*];

            /// The key as written in a theme's `style`.
            pub fn name(self) -> &'static str {
                match self {
                    $(StyleKey::$variant => $name,)*
                }
            }

            /// What the key colours, for the schema and the docs.
            pub fn description(self) -> &'static str {
                match self {
                    $(StyleKey::$variant => $description,)*
                }
            }
        }
    };
}

style_keys! {
    Background = "background": "The app's base: the header, the tab strip and the footer (title_bar.background and status_bar.background refine them).",
    EditorBackground = "editor.background": "Behind the pages, and the library's background.",
    PanelBackground = "panel.background": "Permanent side panes: the sidebar and its lists.",
    SurfaceBackground = "surface.background": "Cards: library books, highlight cards, stats cards.",
    ElevatedSurfaceBackground = "elevated_surface.background": "Floating surfaces: the annotate rail, popovers, menus, tooltips, toasts and sheets.",
    TitleBarBackground = "title_bar.background": "The header and the tab strip.",
    StatusBarBackground = "status_bar.background": "The footer under a document.",
    Border = "border": "The edge between regions and around cards and floating surfaces; #00000000 for none.",
    BorderVariant = "border.variant": "Rules between rows and sections, and the outline of inputs, buttons and segmented controls.",
    BorderSelected = "border.selected": "The outline of a selected row, chip, segment, card or tab; #00000000 for none.",
    ElementBackground = "element.background": "Input wells, dropdowns and segmented controls.",
    ElementHover = "element.hover": "The fill of a hovered row or control.",
    ElementActive = "element.active": "The fill of a pressed row or control.",
    ElementSelected = "element.selected": "The fill of a selected row, chip, segment or tab.",
    GhostElementHover = "ghost_element.hover": "The fill of a hovered row or control (applies after element.hover).",
    GhostElementActive = "ghost_element.active": "The fill of a pressed row or control (applies after element.active).",
    GhostElementSelected = "ghost_element.selected": "The fill of a selected row or control (applies after element.selected).",
    Text = "text": "Body text and labels.",
    TextMuted = "text.muted": "Descriptions, secondary columns and inactive labels.",
    TextPlaceholder = "text.placeholder": "Hints, placeholders, counts and the faintest captions.",
    Icon = "icon": "Icons at rest (icon.muted, when set, applies after it).",
    IconMuted = "icon.muted": "Icons at rest.",
    MarginTrack = "margin.track": "Empty progress tracks, slider rails and the activity calendar's empty days.",
    TextAccent = "text.accent": "The accent: it sets every accent role (fills, headings, selected labels, active icons, the focus ring, text selection, find matches, the activity ramp); the keys below refine them.",
    MarginAccent = "margin.accent": "The accent fill alone (primary buttons, switches, progress, the focus ring), with its hover, pressed and activity-ramp shades.",
    BorderFocused = "border.focused": "The keyboard focus ring, a focused field's outline and the text cursor.",
    ElementSelectionBackground = "element.selection_background": "Selected text, on the page and in fields.",
    SearchMatchBackground = "search.match_background": "The matched words in the Find pane's results.",
    MarginAccentHover = "margin.accent.hover": "The accent fill under the pointer.",
    MarginAccentActive = "margin.accent.active": "The accent fill while pressed.",
    MarginOnAccent = "margin.on_accent": "Text and icons on an accent fill.",
    MarginHeading = "margin.heading": "Section titles (Stats, Settings, tool groups).",
    MarginSectionLabel = "margin.section_label": "Small-caps section labels (CONTENTS, RECENT).",
    MarginSelectedText = "margin.selected_text": "The label of a selected row, chip, segment or card.",
    MarginControlBorder = "margin.control_border": "The outline of inputs, secondary buttons and segmented controls (refines border.variant).",
    MarginBorderHover = "margin.border.hover": "The edge of a hovered row, card or control; #00000000 for none.",
    MarginScrim = "margin.scrim": "The shade over the window behind a modal sheet.",
    MarginHeat1 = "margin.heat.1": "The activity ramp's lightest reading day.",
    MarginHeat2 = "margin.heat.2": "The activity ramp's second step.",
    MarginHeat3 = "margin.heat.3": "The activity ramp's third step.",
    MarginHeat4 = "margin.heat.4": "The activity ramp's busiest day.",
    MarginPageShadow = "margin.page_shadow": "The shadow under each page.",
    MarginRadius = "margin.radius": "A control's corner radius in points (rows, buttons, inputs), 0..16; a number, not a colour.",
}

impl StyleKey {
    /// The key called `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|key| key.name() == name)
    }

    /// Whether the key takes a number rather than a colour.
    fn is_number(self) -> bool {
        self == StyleKey::MarginRadius
    }

    fn schema(self) -> Value {
        if self.is_number() {
            json!({ "description": self.description(), "type": ["integer", "null"], "minimum": 0, "maximum": MAX_RADIUS })
        } else {
            json!({ "description": self.description(), "type": ["string", "null"], "pattern": COLOR_PATTERN })
        }
    }
}

/// Keys of Zed's theme schema that Margin has no role for. A theme may set them (a Zed theme file
/// does); they are accepted, not checked, and change nothing.
pub const ZED_ONLY_KEYS: &[&str] = &[
    "background.appearance",
    "border.transparent",
    "border.disabled",
    "element.disabled",
    "drop_target.background",
    "drop_target.border",
    "ghost_element.background",
    "ghost_element.disabled",
    "text.disabled",
    "icon.disabled",
    "icon.placeholder",
    "icon.accent",
    "debugger.accent",
    "title_bar.inactive_background",
    "toolbar.background",
    "tab_bar.background",
    "tab.inactive_background",
    "tab.active_background",
    "search.active_match_background",
    "panel.focused_border",
    "panel.indent_guide",
    "panel.indent_guide_hover",
    "panel.indent_guide_active",
    "panel.overlay_background",
    "panel.overlay_hover",
    "pane.focused_border",
    "pane_group.border",
    "scrollbar_thumb.background",
    "scrollbar.thumb.background",
    "scrollbar.thumb.hover_background",
    "scrollbar.thumb.active_background",
    "scrollbar.thumb.border",
    "scrollbar.track.background",
    "scrollbar.track.border",
    "minimap.thumb.background",
    "minimap.thumb.hover_background",
    "minimap.thumb.active_background",
    "minimap.thumb.border",
    "editor.foreground",
    "editor.code_lens.foreground",
    "editor.gutter.background",
    "editor.subheader.background",
    "editor.active_line.background",
    "editor.highlighted_line.background",
    "editor.debugger_active_line.background",
    "editor.line_number",
    "editor.active_line_number",
    "editor.hover_line_number",
    "editor.invisible",
    "editor.wrap_guide",
    "editor.active_wrap_guide",
    "editor.indent_guide",
    "editor.indent_guide_active",
    "editor.document_highlight.read_background",
    "editor.document_highlight.write_background",
    "editor.document_highlight.bracket_background",
    "editor.diff_hunk.added.background",
    "editor.diff_hunk.added.hollow_background",
    "editor.diff_hunk.added.hollow_border",
    "editor.diff_hunk.deleted.background",
    "editor.diff_hunk.deleted.hollow_background",
    "editor.diff_hunk.deleted.hollow_border",
    "terminal.background",
    "terminal.foreground",
    "terminal.ansi.background",
    "terminal.bright_foreground",
    "terminal.dim_foreground",
    "terminal.ansi.black",
    "terminal.ansi.bright_black",
    "terminal.ansi.dim_black",
    "terminal.ansi.red",
    "terminal.ansi.bright_red",
    "terminal.ansi.dim_red",
    "terminal.ansi.green",
    "terminal.ansi.bright_green",
    "terminal.ansi.dim_green",
    "terminal.ansi.yellow",
    "terminal.ansi.bright_yellow",
    "terminal.ansi.dim_yellow",
    "terminal.ansi.blue",
    "terminal.ansi.bright_blue",
    "terminal.ansi.dim_blue",
    "terminal.ansi.magenta",
    "terminal.ansi.bright_magenta",
    "terminal.ansi.dim_magenta",
    "terminal.ansi.cyan",
    "terminal.ansi.bright_cyan",
    "terminal.ansi.dim_cyan",
    "terminal.ansi.white",
    "terminal.ansi.bright_white",
    "terminal.ansi.dim_white",
    "link_text.hover",
    "version_control.added",
    "version_control.deleted",
    "version_control.modified",
    "version_control.renamed",
    "version_control.conflict",
    "version_control.ignored",
    "version_control.word_added",
    "version_control.word_deleted",
    "version_control.conflict_marker.ours",
    "version_control.conflict_marker.theirs",
    "vim.normal.background",
    "vim.insert.background",
    "vim.replace.background",
    "vim.visual.background",
    "vim.visual_line.background",
    "vim.visual_block.background",
    "vim.yank.background",
    "vim.helix_jump_label.foreground",
    "vim.helix_normal.background",
    "vim.helix_select.background",
    "vim.normal.foreground",
    "vim.insert.foreground",
    "vim.replace.foreground",
    "vim.visual.foreground",
    "vim.visual_line.foreground",
    "vim.visual_block.foreground",
    "vim.helix_normal.foreground",
    "vim.helix_select.foreground",
    "conflict",
    "conflict.background",
    "conflict.border",
    "created",
    "created.background",
    "created.border",
    "deleted",
    "deleted.background",
    "deleted.border",
    "error",
    "error.background",
    "error.border",
    "hidden",
    "hidden.background",
    "hidden.border",
    "hint",
    "hint.background",
    "hint.border",
    "ignored",
    "ignored.background",
    "ignored.border",
    "info",
    "info.background",
    "info.border",
    "modified",
    "modified.background",
    "modified.border",
    "predictive",
    "predictive.background",
    "predictive.border",
    "renamed",
    "renamed.background",
    "renamed.border",
    "success",
    "success.background",
    "success.border",
    "unreachable",
    "unreachable.background",
    "unreachable.border",
    "warning",
    "warning.background",
    "warning.border",
    "accents",
    "players",
    "syntax",
];

/// The description the schema gives a key of [`ZED_ONLY_KEYS`].
const ZED_ONLY_DESCRIPTION: &str = "A Zed key Margin has no use for: accepted, no effect.";

/// One style value, checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleValue {
    /// Red, green, blue and alpha (unmultiplied).
    Color([u8; 4]),
    /// A whole number of points (`margin.radius`).
    Points(u8),
}

/// A theme's checked style: each key at most once, in the order keys apply ([`StyleKey`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Style(Vec<(StyleKey, StyleValue)>);

impl Style {
    /// The values, in the order they apply.
    pub fn entries(&self) -> &[(StyleKey, StyleValue)] {
        &self.0
    }

    /// The style of a map that [`check_style_value`] already accepted key by key (a cleaned
    /// `theme_overrides` entry): keys Margin has no role for are skipped.
    pub fn from_checked(map: &Map<String, Value>) -> Self {
        let mut entries: Vec<(StyleKey, StyleValue)> = map
            .iter()
            .filter_map(|(name, value)| {
                let key = StyleKey::from_name(name)?;
                check_style_value(key, value).ok().flatten().map(|checked| (key, checked))
            })
            .collect();
        entries.sort_by_key(|(key, _)| *key);
        Self(entries)
    }
}

/// `#rrggbb` or `#rrggbbaa` as red, green, blue and alpha.
pub fn parse_color(text: &str) -> Option<[u8; 4]> {
    let hex = text.strip_prefix('#')?;
    let channels = hex.len() / HEX_PER_CHANNEL;
    if hex.len() % HEX_PER_CHANNEL != 0 || !(channels == OPAQUE_CHANNELS || channels == ALPHA_CHANNELS) || !hex.is_ascii() {
        return None;
    }
    let mut rgba = [u8::MAX; ALPHA_CHANNELS];
    for (index, channel) in rgba.iter_mut().take(channels).enumerate() {
        let start = index * HEX_PER_CHANNEL;
        *channel = u8::from_str_radix(hex.get(start..start + HEX_PER_CHANNEL)?, 16).ok()?;
    }
    Some(rgba)
}

/// Checks one value for `key`. `Ok(None)` for `null`, which leaves the key unset (Zed writes
/// `null` for a colour it doesn't set); `Err` says what is wrong.
pub fn check_style_value(key: StyleKey, value: &Value) -> Result<Option<StyleValue>, String> {
    if value.is_null() {
        return Ok(None);
    }
    if key.is_number() {
        return match value.as_u64().map(u8::try_from) {
            Some(Ok(points)) if points <= MAX_RADIUS => Ok(Some(StyleValue::Points(points))),
            _ => Err(format!("{value} is not a whole number from 0 to {MAX_RADIUS}")),
        };
    }
    match value.as_str().and_then(parse_color) {
        Some(rgba) => Ok(Some(StyleValue::Color(rgba))),
        None => Err(format!("{value} is not a colour (#rrggbb or #rrggbbaa)")),
    }
}

/// Where a problem points: the file, its property positions, the JSON pointer and the dotted key
/// of the object being read.
struct Spot<'a> {
    file: &'a str,
    positions: &'a Positions,
    pointer: String,
    key: String,
}

impl Spot<'_> {
    fn child(&self, segment: &str) -> Self {
        let key = if self.key.is_empty() { segment.to_owned() } else { format!("{}.{segment}", self.key) };
        Spot { file: self.file, positions: self.positions, pointer: format!("{}/{}", self.pointer, escape_pointer(segment)), key }
    }

    fn problem(&self, message: String) -> Problem {
        let position = self.positions.find(&self.pointer);
        Problem { file: self.file.to_owned(), line: position.line, col: position.col, key: self.key.clone(), message, severity: Severity::Warning }
    }
}

/// Reads a `style` map: Margin's keys are checked, Zed's other keys accepted, anything else
/// reported. A bad or unknown value is left out, so the parent's value shows through.
fn read_style(map: &Map<String, Value>, theme: &str, spot: &Spot<'_>) -> (Style, Vec<Problem>) {
    let mut entries = Vec::new();
    let mut problems = Vec::new();
    for (name, value) in map {
        let at = spot.child(name);
        match StyleKey::from_name(name) {
            Some(key) => match check_style_value(key, value) {
                Ok(Some(checked)) => entries.push((key, checked)),
                Ok(None) => {}
                Err(message) => problems.push(at.problem(format!("\"{theme}\": {name} {message}; using the parent theme's value"))),
            },
            None if ZED_ONLY_KEYS.contains(&name.as_str()) => {}
            None => problems.push(at.problem(format!("\"{theme}\": unknown style key {name}; ignored (schemas/{THEME_SCHEMA_FILE} lists the keys)"))),
        }
    }
    entries.sort_by_key(|(key, _)| *key);
    (Style(entries), problems)
}

/// One theme as read from a family file.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeDef {
    /// The theme's name, unique among the themes Margin shows.
    pub name: String,
    /// Light or dark.
    pub appearance: Appearance,
    /// The theme this one starts from; `None` means the built-in of its appearance.
    pub extends: Option<String>,
    /// The checked style.
    pub style: Style,
    /// The file it came from, as shown to the user.
    pub file: String,
    /// Where its `extends` (or, without one, its entry) sits, for problems about its parent.
    line: u32,
    col: u32,
}

impl ThemeDef {
    /// The theme this one starts from.
    pub fn parent(&self) -> &str {
        self.extends.as_deref().unwrap_or_else(|| self.appearance.builtin())
    }

    fn problem(&self, message: String) -> Problem {
        Problem { file: self.file.clone(), line: self.line, col: self.col, key: "extends".to_owned(), message, severity: Severity::Warning }
    }
}

/// Reads one theme family. Every problem is reported; what is usable is kept.
pub fn read_family(text: &str, file: &str) -> (Vec<ThemeDef>, Vec<Problem>) {
    let parsed = match jsonc::parse(text) {
        Ok(parsed) => parsed,
        Err(err) => {
            let problem = Problem {
                file: file.to_owned(),
                line: err.position.line,
                col: err.position.col,
                key: String::new(),
                message: format!("{}; file ignored", err.message),
                severity: Severity::Error,
            };
            return (Vec::new(), vec![problem]);
        }
    };
    let root = Spot { file, positions: &parsed.positions, pointer: String::new(), key: String::new() };
    let mut problems = Vec::new();
    let map = match parsed.value {
        None => return (Vec::new(), problems),
        Some(Value::Object(map)) => map,
        Some(_) => return (Vec::new(), vec![root.problem("a theme file holds one object, { \"themes\": [ ... ] }; file ignored".to_owned())]),
    };
    for (key, value) in &map {
        match key.as_str() {
            "$schema" | "name" | "author" if !value.is_string() => {
                problems.push(root.child(key).problem(format!("{key} expects a string; ignored")));
            }
            "$schema" | "name" | "author" | "themes" => {}
            other => problems.push(root.child(other).problem(format!("unknown key {other}; ignored"))),
        }
    }
    let Some(Value::Array(entries)) = map.get("themes") else {
        problems.push(root.problem("the file has no \"themes\" list; file ignored".to_owned()));
        return (Vec::new(), problems);
    };
    if entries.len() > MAX_THEMES_PER_FILE {
        problems.push(root.child("themes").problem(format!("more than {MAX_THEMES_PER_FILE} themes; the rest are ignored")));
    }
    let list = root.child("themes");
    let mut themes = Vec::new();
    for (index, entry) in entries.iter().enumerate().take(MAX_THEMES_PER_FILE) {
        let (theme, entry_problems) = read_theme(entry, &list.child(&index.to_string()));
        themes.extend(theme);
        problems.extend(entry_problems);
    }
    (themes, problems)
}

/// One entry of a family's `themes` list.
fn read_theme(entry: &Value, spot: &Spot<'_>) -> (Option<ThemeDef>, Vec<Problem>) {
    let Value::Object(map) = entry else { return (None, vec![spot.problem("a theme is an object; ignored".to_owned())]) };
    let Some(name) = map.get("name").and_then(Value::as_str).filter(|name| !name.trim().is_empty()) else {
        return (None, vec![spot.problem("a theme needs a \"name\"; ignored".to_owned())]);
    };
    let Some(appearance) = map.get("appearance").and_then(Value::as_str).and_then(Appearance::from_name) else {
        return (None, vec![spot.child("appearance").problem(format!("\"{name}\": appearance must be \"dark\" or \"light\"; theme ignored"))]);
    };
    let mut problems = Vec::new();
    for key in map.keys().filter(|key| !["name", "appearance", "extends", "style"].contains(&key.as_str())) {
        problems.push(spot.child(key).problem(format!("\"{name}\": unknown key {key}; ignored")));
    }
    let extends = match map.get("extends") {
        None | Some(Value::Null) => None,
        Some(Value::String(parent)) => Some(parent.clone()),
        Some(_) => {
            problems.push(spot.child("extends").problem(format!("\"{name}\": extends expects a theme name; using {}", appearance.builtin())));
            None
        }
    };
    let style = match map.get("style") {
        None | Some(Value::Null) => Style::default(),
        Some(Value::Object(style)) => {
            let (style, style_problems) = read_style(style, name, &spot.child("style"));
            problems.extend(style_problems);
            style
        }
        Some(_) => {
            problems.push(spot.child("style").problem(format!("\"{name}\": style expects an object of keys; ignored")));
            Style::default()
        }
    };
    let at = spot.positions.find(&format!("{}/extends", spot.pointer));
    let def = ThemeDef { name: name.to_owned(), appearance, extends, style, file: spot.file.to_owned(), line: at.line, col: at.col };
    (Some(def), problems)
}

/// One theme file's text as read from `<config>`: `Ok(None)` when it doesn't exist, `Err` when it
/// can't be read. Two reads compare equal when the files say the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeSource {
    /// The file.
    pub path: PathBuf,
    /// Its text.
    pub text: Result<Option<String>, String>,
}

/// Reads `<config>/themes.json`, then every `<config>/themes/*.json` in file-name order.
pub fn read_theme_sources(config_dir: &Path) -> Vec<ThemeSource> {
    let main = config_dir.join(THEMES_FILE);
    let mut sources = vec![ThemeSource { text: read_capped(&main, MAX_SETTINGS_BYTES), path: main }];
    let folder = config_dir.join(THEMES_DIR);
    let entries = match std::fs::read_dir(&folder) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return sources,
        Err(err) => {
            sources.push(ThemeSource { path: folder, text: Err(format!("cannot list the folder ({err})")) });
            return sources;
        }
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file() && path.extension().and_then(|extension| extension.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .collect();
    files.sort();
    if files.len() > MAX_THEME_FILES {
        sources.push(ThemeSource { path: folder, text: Err(format!("more than {MAX_THEME_FILES} theme files; the rest are ignored")) });
    }
    sources.extend(files.into_iter().take(MAX_THEME_FILES).map(|path| ThemeSource { text: read_capped(&path, MAX_SETTINGS_BYTES), path }));
    sources
}

/// Every theme the sources define, in source order, and what was wrong. A name defined again
/// (a built-in's, or an earlier file's) is reported; the later definition wins.
pub fn read_themes(sources: &[ThemeSource]) -> (Vec<ThemeDef>, Vec<Problem>) {
    let mut themes: Vec<ThemeDef> = Vec::new();
    let mut problems = Vec::new();
    for source in sources {
        let shown = source.path.display().to_string();
        match &source.text {
            Ok(Some(text)) => {
                let (family, family_problems) = read_family(text, &shown);
                problems.extend(family_problems);
                themes.extend(family);
            }
            Ok(None) => {}
            Err(message) => problems.push(Problem {
                file: shown,
                line: 1,
                col: 1,
                key: String::new(),
                message: format!("{message}; file ignored"),
                severity: Severity::Error,
            }),
        }
    }
    for (index, theme) in themes.iter().enumerate() {
        let earlier = themes.get(..index).and_then(|before| before.iter().rev().find(|other| other.name == theme.name));
        let message = match (earlier, builtin_appearance(&theme.name)) {
            (Some(other), _) => format!("\"{}\" is defined again (also in {}); this definition wins", theme.name, other.file),
            (None, Some(_)) => format!("\"{}\" replaces the built-in theme of that name", theme.name),
            (None, None) => continue,
        };
        problems.push(Problem { key: "name".to_owned(), ..theme.problem(message) });
    }
    (themes, problems)
}

/// The theme that answers to `name` for a theme at `before` in the list: the last definition of
/// that name before it, else the built-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Found {
    Builtin(Appearance),
    Defined(usize),
}

fn find(themes: &[ThemeDef], name: &str, before: usize) -> Option<Found> {
    let defined = themes.get(..before).and_then(|earlier| earlier.iter().rposition(|theme| theme.name == name));
    defined.map(Found::Defined).or_else(|| builtin_appearance(name).map(Found::Builtin))
}

/// A theme's chain: the built-in at its root, then the defined themes from the root's child to
/// the theme itself. Applying each layer's style over the root gives the theme.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution<'a> {
    /// The built-in at the root.
    pub base: Appearance,
    /// Defined themes, root-most first.
    pub layers: Vec<&'a ThemeDef>,
    /// The theme's own appearance.
    pub appearance: Appearance,
    /// A missing parent or a loop: the chain stops there, on the built-in of that theme's
    /// appearance.
    pub problems: Vec<Problem>,
}

/// The chain of the theme called `name` (its last definition, else the built-in); `None` when no
/// theme has that name.
pub fn resolve<'a>(themes: &'a [ThemeDef], name: &str) -> Option<Resolution<'a>> {
    let start = find(themes, name, themes.len())?;
    let mut layers: Vec<&ThemeDef> = Vec::new();
    let mut seen = HashSet::new();
    let mut problems = Vec::new();
    let mut current = start;
    let base = loop {
        let index = match current {
            Found::Builtin(appearance) => break appearance,
            Found::Defined(index) => index,
        };
        let Some(theme) = themes.get(index) else { break Appearance::Dark };
        if !seen.insert(index) || layers.len() >= MAX_EXTENDS_DEPTH {
            let chain: Vec<&str> = layers.iter().map(|layer| layer.name.as_str()).chain(std::iter::once(theme.name.as_str())).collect();
            let fallback = theme.appearance.builtin();
            problems.push(theme.problem(format!(
                "\"{}\": extends loops ({}); using {fallback} under it",
                chain.first().unwrap_or(&name),
                chain.join(" → ")
            )));
            break theme.appearance;
        }
        layers.push(theme);
        let parent = theme.parent();
        // A theme that reuses its parent's name (one that replaces "Margin Dark" and extends it)
        // means the definition before it, not itself.
        let before = if parent == theme.name { index } else { themes.len() };
        match find(themes, parent, before) {
            Some(next) => current = next,
            None => {
                let fallback = theme.appearance.builtin();
                problems.push(theme.problem(format!("\"{}\" extends \"{parent}\", which no theme is named; using {fallback} under it", theme.name)));
                break theme.appearance;
            }
        }
    };
    let appearance = layers.first().map_or(base, |leaf| leaf.appearance);
    layers.reverse();
    Some(Resolution { base, layers, appearance, problems })
}

/// Every theme name Margin can show, with its appearance: the built-ins, then each defined name
/// once, in the order first defined (its last definition's appearance).
pub fn theme_names(themes: &[ThemeDef]) -> Vec<(String, Appearance)> {
    let mut names: Vec<(String, Appearance)> =
        [Appearance::Dark, Appearance::Light].map(|appearance| (appearance.builtin().to_owned(), appearance)).into();
    for theme in themes {
        match names.iter_mut().find(|(name, _)| *name == theme.name) {
            Some(entry) => entry.1 = theme.appearance,
            None => names.push((theme.name.clone(), theme.appearance)),
        }
    }
    names
}

/// Checks the colours of `settings.theme_overrides` in a cleaned settings value (the validator
/// already dropped unknown keys and wrong types): each bad value is reported and removed, so
/// the theme's own value shows through.
pub(crate) fn check_overrides(settings: &mut Value, positions: &Positions, file: &str) -> Vec<Problem> {
    let mut problems = Vec::new();
    let Some(Value::Object(overrides)) = settings.get_mut("theme_overrides") else { return problems };
    let root = Spot { file, positions, pointer: String::new(), key: String::new() }.child("theme_overrides");
    for (theme, style) in overrides.iter_mut() {
        let Value::Object(style) = style else { continue };
        let at = root.child(theme);
        style.retain(|name, value| {
            let Some(key) = StyleKey::from_name(name) else { return true };
            match check_style_value(key, value) {
                Ok(_) => true,
                Err(message) => {
                    problems.push(at.child(name).problem(format!("theme_overrides \"{theme}\": {name} {message}; using the theme's value")));
                    false
                }
            }
        });
    }
    problems
}

/// The schema of a `style` map (a theme's, and each entry of `settings.theme_overrides`): every
/// Margin key with what it colours, and Zed's other keys, accepted.
pub fn style_schema() -> Value {
    let mut properties = Map::new();
    for key in StyleKey::ALL {
        properties.insert(key.name().to_owned(), key.schema());
    }
    for name in ZED_ONLY_KEYS {
        properties.insert((*name).to_owned(), json!({ "description": ZED_ONLY_DESCRIPTION }));
    }
    json!({
        "description": "Dotted style keys: colours as #rrggbb or #rrggbbaa (null leaves a key unset), margin.radius as a number.",
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    })
}

/// `theme.schema.json`: a theme family, as in Zed.
pub fn family_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Margin theme family",
        "description": "A theme family in Zed's format. Margin reads <config>/themes.json and <config>/themes/*.json.",
        "type": "object",
        "properties": {
            "$schema": { "type": "string", "description": "editor completion" },
            "name": { "type": "string", "description": "the family's name" },
            "author": { "type": "string", "description": "who made it" },
            "themes": { "type": "array", "items": { "$ref": "#/$defs/Theme" }, "description": "the themes; each name shows in the theme pickers" },
        },
        "required": ["themes"],
        "additionalProperties": false,
        "$defs": {
            "Theme": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "minLength": 1, "description": "unique name, as settings.jsonc's theme names it" },
                    "appearance": { "enum": ["dark", "light"], "description": "used when theme.mode resolves to it" },
                    "extends": {
                        "type": ["string", "null"],
                        "description": "the theme whose values fill the keys this one leaves out; default: Margin Dark for a dark theme, Margin Light for a light one"
                    },
                    "style": { "$ref": "#/$defs/Style" },
                },
                "required": ["name", "appearance"],
                "additionalProperties": false,
            },
            "Style": style_schema(),
        },
    })
}

/// The `themes.json` Margin writes on first run: how themes work, then a family with one example
/// theme. Nothing uses the example until `theme` in settings.jsonc names it.
pub fn first_run_themes_json() -> String {
    format!(
        "// Margin themes. Margin reloads this file when you save it.\n\
// A theme family in Zed's format: each theme has a name, an appearance (\"dark\" or \"light\"),\n\
// the theme it starts from (\"extends\": \"{DEFAULT_THEME}\", \"{LIGHT_THEME}\" or any other theme) and\n\
// the style keys it changes. schemas/{THEME_SCHEMA_FILE} lists every key; Zed theme files dropped\n\
// into themes/ work as they are. A bad key or colour is reported and the parent's value used.\n\
//\n\
// Pick a theme in settings.jsonc: \"theme\": \"Example Dark\", or\n\
// \"theme\": {{ \"mode\": \"system\", \"light\": \"{LIGHT_THEME}\", \"dark\": \"Example Dark\" }}.\n\
// To change one colour of a theme instead, use \"theme_overrides\" in settings.jsonc.\n\
{{\n\
  \"$schema\": \"{THEMES_SCHEMA_URL}\",\n\
  \"name\": \"My themes\",\n\
  \"author\": \"\",\n\
  \"themes\": [\n\
    {{\n\
      \"name\": \"Example Dark\",\n\
      \"appearance\": \"dark\",\n\
      \"extends\": \"{DEFAULT_THEME}\",\n\
      \"style\": {{\n\
        \"text.accent\": \"#4b8bf5\",\n\
        \"panel.background\": \"#0b0d12\"\n\
      }}\n\
    }}\n\
  ]\n\
}}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family(themes: Value) -> String {
        json!({ "name": "Test", "themes": themes }).to_string()
    }

    fn color(key: StyleKey, def: &ThemeDef) -> Option<[u8; 4]> {
        def.style.entries().iter().find(|(k, _)| *k == key).and_then(|(_, value)| match value {
            StyleValue::Color(rgba) => Some(*rgba),
            StyleValue::Points(_) => None,
        })
    }

    #[test]
    fn colours_are_six_or_eight_hex_digits() {
        assert_eq!(parse_color("#ff5f1f"), Some([0xFF, 0x5F, 0x1F, 0xFF]));
        assert_eq!(parse_color("#FF5F1F38"), Some([0xFF, 0x5F, 0x1F, 0x38]));
        for bad in ["ff5f1f", "#fff", "#ff5f1", "#ff5f1f3", "#gg0000", "#ff5f1f38aa", "#ééé", ""] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }

    /// A theme extending Margin Dark keeps its own accent and background over the built-in.
    #[test]
    fn a_theme_extending_margin_dark_layers_over_it() {
        let text = family(json!([{ "name": "Mine", "appearance": "dark", "extends": "Margin Dark",
            "style": { "text.accent": "#4b8bf5", "background": "#101010" } }]));
        let (themes, problems) = read_themes(&[ThemeSource { path: "themes.json".into(), text: Ok(Some(text)) }]);
        assert_eq!(problems, []);
        let resolution = resolve(&themes, "Mine").unwrap();
        assert_eq!(resolution.base, Appearance::Dark);
        assert_eq!(resolution.layers.len(), 1);
        assert_eq!(resolution.problems, []);
        let mine = resolution.layers[0];
        assert_eq!(color(StyleKey::TextAccent, mine), Some([0x4B, 0x8B, 0xF5, 0xFF]));
        assert_eq!(color(StyleKey::Background, mine), Some([0x10, 0x10, 0x10, 0xFF]));
        assert!(resolve(&themes, "Margin Light").is_some_and(|built_in| built_in.layers.is_empty()));
        assert!(resolve(&themes, "Nope").is_none());
    }

    /// A loop and a missing parent are reported, and the chain stops on the built-in of the
    /// theme's appearance; the themes' own styles still apply.
    #[test]
    fn loops_and_missing_parents_fall_back_to_the_built_in() {
        let text = family(json!([
            { "name": "A", "appearance": "light", "extends": "B", "style": { "text": "#111111" } },
            { "name": "B", "appearance": "light", "extends": "A" },
            { "name": "Orphan", "appearance": "dark", "extends": "Gone", "style": { "text": "#eeeeee" } },
        ]));
        let (themes, problems) = read_family(&text, "themes.json");
        assert_eq!(problems, []);
        let looped = resolve(&themes, "A").unwrap();
        assert_eq!(looped.base, Appearance::Light);
        assert_eq!(looped.layers.iter().map(|layer| layer.name.as_str()).collect::<Vec<_>>(), ["B", "A"]);
        assert_eq!(looped.problems.len(), 1);
        assert!(looped.problems[0].message.contains("loops (A → B → A)"), "{:?}", looped.problems);
        let orphan = resolve(&themes, "Orphan").unwrap();
        assert_eq!(orphan.base, Appearance::Dark);
        assert_eq!(orphan.layers.len(), 1);
        assert!(orphan.problems[0].message.contains("\"Gone\", which no theme is named; using Margin Dark"), "{:?}", orphan.problems);
        assert_eq!(orphan.problems[0].key, "extends");
    }

    /// A bad colour and an unknown key are reported with their file, line and key, and left out;
    /// the rest of the theme stands.
    #[test]
    fn bad_values_and_unknown_keys_are_reported() {
        let text = "{\n  \"themes\": [\n    { \"name\": \"Mine\", \"appearance\": \"dark\", \"style\": {\n      \"text\": \"red\",\n      \"bakground\": \"#000000\",\n      \"margin.radius\": 40,\n      \"text.muted\": \"#808080\"\n    } }\n  ]\n}\n";
        let (themes, problems) = read_family(text, "themes.json");
        let keys: Vec<(&str, u32)> = problems.iter().map(|problem| (problem.key.as_str(), problem.line)).collect();
        assert_eq!(keys, [("themes.0.style.bakground", 5), ("themes.0.style.margin.radius", 6), ("themes.0.style.text", 4)]);
        assert!(problems.iter().all(|problem| problem.file == "themes.json" && problem.severity == Severity::Warning));
        assert!(problems[2].message.contains("\"red\" is not a colour"), "{problems:?}");
        assert_eq!(themes[0].style.entries(), [(StyleKey::TextMuted, StyleValue::Color([0x80, 0x80, 0x80, 0xFF]))]);
    }

    /// A Zed theme family loads as it is: Zed's keys Margin has no role for, `null` values,
    /// `players` and `syntax` raise no problem.
    #[test]
    fn a_zed_family_loads_without_problems() {
        let text = family(json!([{ "name": "One Dark", "appearance": "dark", "style": {
            "background.appearance": "opaque",
            "accents": [],
            "border": "#464b57ff",
            "background": "#3b414dff",
            "text.accent": "#74ade8ff",
            "editor.background": "#282c33ff",
            "panel.focused_border": null,
            "terminal.ansi.red": "#e06c75ff",
            "players": [{ "cursor": "#74ade8ff", "background": "#74ade8ff", "selection": "#74ade83d" }],
            "syntax": { "comment": { "color": "#5d636fff", "font_style": null, "font_weight": null } },
        } }]));
        let (themes, problems) = read_family(&text, "one.json");
        assert_eq!(problems, []);
        assert_eq!(themes[0].extends, None);
        assert_eq!(themes[0].style.entries().len(), 4);
        assert_eq!(resolve(&themes, "One Dark").unwrap().base, Appearance::Dark);
    }

    /// The same name twice is reported; the later definition wins, and a theme that replaces a
    /// built-in can extend the built-in it replaces.
    #[test]
    fn a_name_defined_again_is_reported_and_the_later_one_wins() {
        let first = family(json!([{ "name": "Mine", "appearance": "dark", "style": { "text": "#111111" } }]));
        let second = family(json!([
            { "name": "Mine", "appearance": "light", "style": { "text": "#222222" } },
            { "name": "Margin Dark", "appearance": "dark", "extends": "Margin Dark", "style": { "text.accent": "#00ff00" } },
        ]));
        let sources = [
            ThemeSource { path: "themes.json".into(), text: Ok(Some(first)) },
            ThemeSource { path: "themes/more.json".into(), text: Ok(Some(second)) },
        ];
        let (themes, problems) = read_themes(&sources);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].message.contains("defined again (also in themes.json)"), "{problems:?}");
        assert!(problems[1].message.contains("replaces the built-in"), "{problems:?}");
        let mine = resolve(&themes, "Mine").unwrap();
        assert_eq!(mine.appearance, Appearance::Light);
        assert_eq!(mine.layers.len(), 1, "the earlier Mine is not its parent");
        let replaced = resolve(&themes, "Margin Dark").unwrap();
        assert_eq!((replaced.base, replaced.layers.len(), replaced.problems.len()), (Appearance::Dark, 1, 0));
        let names = theme_names(&themes);
        assert_eq!(names.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["Margin Dark", "Margin Light", "Mine"]);
    }

    #[test]
    fn broken_files_are_reported_and_skipped() {
        for (text, needle) in [("{ \"themes\": [", "file ignored"), ("[]", "one object"), ("{ \"name\": \"x\" }", "no \"themes\" list")] {
            let (themes, problems) = read_family(text, "bad.json");
            assert!(themes.is_empty());
            assert!(problems.iter().any(|problem| problem.message.contains(needle)), "{text}: {problems:?}");
        }
        let (themes, problems) = read_family(&family(json!([{ "name": "x", "appearance": "dim" }, { "appearance": "dark" }])), "bad.json");
        assert!(themes.is_empty());
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    /// The first-run themes.json parses, matches the schema and loads with no problem.
    #[test]
    fn first_run_themes_file_is_a_valid_family() {
        let text = first_run_themes_json();
        let (themes, problems) = read_family(&text, "themes.json");
        assert_eq!(problems, []);
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].parent(), DEFAULT_THEME);
        let value = jsonc::parse(&text).unwrap().value.unwrap();
        let validator = jsonschema::validator_for(&family_schema()).unwrap();
        assert!(validator.is_valid(&value), "{:?}", validator.iter_errors(&value).map(|e| e.to_string()).collect::<Vec<_>>());
        assert!(!validator.is_valid(&json!({ "themes": [{ "name": "x", "appearance": "dark", "style": { "text": "red" } }] })));
        assert!(!validator.is_valid(&json!({ "themes": [{ "name": "x", "appearance": "dark", "style": { "nope": "#000000" } }] })));
    }
}
