//! Margin's Obsidian notes (R4): highlights as PDF++ callouts, the document's reading note in
//! the vault, and `obsidian://` links.
//!
//! Formats follow PDF++ (`obsidian-pdf-plus`) and the owner's PDF++ settings: the "Quote in
//! callout" copy template `> [!PDF|{{color}}] {{linkWithDisplay}}` / `> > {{text}}`, the display
//! text `{{file.basename}}, p.{{pageLabel}}`, and the colour set Yellow `#ffd000`, Red `#ea5252`,
//! Note `#086ddd`, Important `#bb61e5`.
//!
//! Links point at the page (`#page=N`). PDF++'s `&selection=` counts PDF.js text-content items,
//! which PdfCraft's glyph list can't reproduce, so a guessed range would mark the wrong words;
//! the highlight itself is a real annotation in the PDF, so Obsidian shows it on that page.
//!
//! Margin owns only explicitly bounded, hash-verified highlight blocks. Edited blocks and
//! legacy unbounded entries are preserved and reported. Writes stay atomic and inside the vault.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[path = "notes_format.rs"]
mod format;
use format::TemplateContext;
#[path = "notes_worker.rs"]
mod worker;
use margin_config::settings::{ExportTarget, NoteUpdateMode};
use worker::WorkerState;

use pdfcraft_engine::{DocId, Document};
use pdfcraft_render::{Annotation, DocInfo, PageInfo, PageRenderer, PageText, RenderConfig, RenderRequest, RequestKind};

use crate::comments::CommentTool;
use crate::margin::look;
use crate::theme::{self, Tokens};
use crate::{Flavor, PdfCraftApp, icons};

/// The PDF++ colour set, in palette order (PDF++ defaults; the owner's `pdf-plus/data.json`).
pub const PDF_PLUS_COLORS: [(&str, [u8; 3]); 4] =
    [("Yellow", [0xff, 0xd0, 0x00]), ("Red", [0xea, 0x52, 0x52]), ("Note", [0x08, 0x6d, 0xdd]), ("Important", [0xbb, 0x61, 0xe5])];
/// Where a document's reading note goes, relative to the vault.
pub const DEFAULT_NOTE_TEMPLATE: &str = "reading/{{title-slug}}.md";
/// The template placeholder for the document title as a file-name slug.
const TITLE_SLUG: &str = "{{title-slug}}";
/// PDF++'s `calloutType` (the owner's setting).
const CALLOUT_TYPE: &str = "PDF";
/// The section of the reading note that Margin appends to.
const HIGHLIGHTS_HEADING: &str = "## Highlights";
/// Opens the marker comment that makes every entry idempotent.
const MARKER_OPEN: &str = "<!-- margin:";
const MARKER_CLOSE: &str = " -->";
/// A reading note larger than this is not a note Margin wrote: refuse rather than load it.
const MAX_NOTE_BYTES: u64 = 16 * 1024 * 1024;
/// Two colours within this many 8-bit steps per channel count as the same named colour.
const COLOR_MATCH_STEPS: u8 = 3;
/// The annotation subtypes that mark text, which are the ones a callout quotes.
const MARKUP_SUBTYPES: [&str; 4] = ["Highlight", "Underline", "StrikeOut", "Squiggly"];
/// The app-state key that remembers "Not now" on the library's setup card.
const SETUP_DISMISSED_KEY: &str = "margin_setup_dismissed";
/// The file in `<data>` that keeps the dismissed setup prompts, written the moment one is
/// dismissed (eframe saves its app state only now and then, and a crash would lose it), and its
/// keys.
const UI_STATE_FILE: &str = "ui-state.json";
const UI_STATE_SETUP_KEY: &str = "setup_dismissed";
const UI_STATE_NOTES_BOX_KEY: &str = "notes_box_dismissed";

/// A named highlight colour (`notes.colors`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteColor {
    pub name: String,
    pub rgb: [u8; 3],
}

impl NoteColor {
    pub(crate) fn rgb_f64(&self) -> [f64; 3] {
        self.rgb.map(|c| f64::from(c) / 255.0)
    }
}

pub fn default_colors() -> Vec<NoteColor> {
    PDF_PLUS_COLORS.iter().map(|(name, rgb)| NoteColor { name: (*name).to_string(), rgb: *rgb }).collect()
}

/// The `notes.*` settings plus the bookkeeping for "Add highlights to this note".
#[derive(Clone, Debug)]
pub struct Notes {
    /// `notes.vault`: the vault's folder (absolute).
    pub vault: Option<PathBuf>,
    /// `notes.note_template`: the reading note's path in the vault; `{{title-slug}}` is filled in.
    pub note_template: String,
    /// `notes.colors`: the highlight palette, the first being the default highlight colour.
    pub colors: Vec<NoteColor>,
    pub config: margin_config::settings::Notes,
    cached: HashMap<DocId, CachedDocument>,
    jobs: HashMap<DocId, Arc<Mutex<WorkerState>>>,
    /// The snapshot rail's vault destination applies to the next completed area selection.
    pub snapshot_to_note: bool,
    /// `notes.autoadd`: paths of the open documents whose new highlights go to their note as
    /// made. Each document's sidecar remembers its flag (`margin::reading`).
    pub autoadd: BTreeSet<String>,
    /// Hand `obsidian://` addresses to the system. Off in tests, which only read
    /// `PdfCraftApp::last_opened_url`, so they never start Obsidian.
    pub launch_obsidian: bool,
    /// Whether the palette and default highlight colour were handed to the comment tools.
    palette_published: bool,
    /// Per open document with autoadd on: what has been seen and what still waits to be written.
    watched: HashMap<DocId, AutoaddWatch>,
    /// The notes block of an older app state, kept (and saved with the app state) until
    /// [`settings_loaded`] has copied it into `settings.jsonc`.
    legacy: Option<serde_json::Value>,
    /// "Not now" on the library's setup card (`margin::welcome`); saved with the app state and
    /// in `<data>/ui-state.json`.
    pub setup_dismissed: bool,
    /// The close button on the Highlights pane's "connect a vault" card; in `ui-state.json`.
    pub notes_box_dismissed: bool,
    /// Whether `ui-state.json` was read this run ([`ensure_ui_state`]).
    ui_state_loaded: bool,
}

impl Default for Notes {
    fn default() -> Self {
        Self {
            vault: None,
            note_template: DEFAULT_NOTE_TEMPLATE.to_string(),
            colors: default_colors(),
            config: margin_config::settings::Notes::default(),
            cached: HashMap::new(),
            jobs: HashMap::new(),
            snapshot_to_note: false,
            autoadd: BTreeSet::new(),
            launch_obsidian: true,
            palette_published: false,
            watched: HashMap::new(),
            legacy: None,
            setup_dismissed: false,
            notes_box_dismissed: false,
            ui_state_loaded: false,
        }
    }
}

#[derive(Clone, Debug)]
struct AutoaddWatch {
    /// The document's edit generation and its sidecar marks' generation.
    generation: (u64, u64),
    dirty: bool,
}

impl Notes {
    /// Use the `notes` block of `settings.jsonc`. A relative vault isn't used (the notes panel
    /// says why), and colours without a name are left out; with none left, the PDF++ palette.
    pub fn apply_config(&mut self, config: &margin_config::settings::Notes) {
        let vault = config.vault.clone().filter(|vault| vault.is_absolute());
        let mut colors: Vec<NoteColor> = config
            .colors
            .iter()
            .filter(|color| !color.name.trim().is_empty())
            .map(|color| NoteColor { name: color.name.trim().to_owned(), rgb: color.rgb })
            .collect();
        if colors.is_empty() {
            colors = default_colors();
        }
        if self.config == *config && self.vault == vault && self.note_template == config.note_template && self.colors == colors {
            return;
        }
        self.vault = vault;
        self.note_template = config.note_template.clone();
        self.colors = colors;
        self.config = config.clone();
        self.palette_published = false;
        self.cached.clear();
    }

    /// Restore the notes block of an older app state. It applies until settings load, then
    /// [`settings_loaded`] migrates it into `settings.jsonc` without replacing what is there.
    /// Settings are untrusted: anything invalid keeps its default.
    pub fn restore(&mut self, v: &serde_json::Value) {
        self.legacy = v.is_object().then(|| v.clone());
        if let Some(vault) = v["vault"].as_str().map(PathBuf::from).filter(|p| p.is_absolute()) {
            self.vault = Some(vault);
        }
        if let Some(template) = v["note_template"].as_str().filter(|t| note_path(t, "title").is_ok()) {
            self.note_template = template.to_string();
        }
        let colors = legacy_colors(v);
        if !colors.is_empty() {
            self.colors = colors;
        }
    }

    /// Margin's app state (`PdfCraftApp::persist`): the older notes block while it waits to be
    /// migrated, and whether the setup card was dismissed. The notes settings themselves live
    /// in `settings.jsonc`.
    pub(crate) fn save_state(&self, state: &mut serde_json::Value) {
        if let Some(legacy) = &self.legacy {
            state["notes"] = legacy.clone();
        }
        if self.setup_dismissed {
            state[SETUP_DISMISSED_KEY] = serde_json::Value::Bool(true);
        }
    }

    /// Restore what [`Self::save_state`] wrote.
    pub(crate) fn restore_state(&mut self, state: &serde_json::Value) {
        self.restore(&state["notes"]);
        // `ui-state.json` may already have said dismissed ([`ensure_ui_state`]); either wins.
        self.setup_dismissed |= state[SETUP_DISMISSED_KEY].as_bool().unwrap_or(false);
    }

    /// The settings alone, for a worker thread: no caches, jobs or per-document state.
    fn settings_only(&self) -> Self {
        Self {
            vault: self.vault.clone(),
            note_template: self.note_template.clone(),
            colors: self.colors.clone(),
            config: self.config.clone(),
            launch_obsidian: false,
            ..Self::default()
        }
    }

    /// The colour name PDF++ uses for `rgb` (lowercase), or its `r,g,b` form when it isn't one of
    /// [`Self::colors`]. No colour means the default (first) colour.
    pub fn color_label(&self, rgb: Option<[f32; 3]>) -> String {
        let Some(rgb) = rgb else { return self.colors.first().map(|c| c.name.to_lowercase()).unwrap_or_default() };
        match self.color_named(rgb) {
            Some(c) => c.name.to_lowercase(),
            None => {
                let bytes = rgb_bytes(rgb);
                format!("{},{},{}", bytes[0], bytes[1], bytes[2])
            }
        }
    }

    /// The palette colour `rgb` is (within [`COLOR_MATCH_STEPS`] per channel), if any.
    pub fn color_named(&self, rgb: [f32; 3]) -> Option<&NoteColor> {
        let bytes = rgb_bytes(rgb);
        self.colors.iter().find(|c| c.rgb.iter().zip(bytes).all(|(a, b)| a.abs_diff(b) <= COLOR_MATCH_STEPS))
    }
}

/// Reads `<data>/ui-state.json` once per run and adds what it says to the dismissed flags (the app
/// state may have said so too). A missing or unreadable file dismisses nothing.
pub(crate) fn ensure_ui_state(app: &mut PdfCraftApp) {
    if app.notes.ui_state_loaded {
        return;
    }
    app.notes.ui_state_loaded = true;
    let Some(path) = ui_state_path(app) else { return };
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let Ok(state) = serde_json::from_str::<serde_json::Value>(&text) else {
        log::warn!("ignoring unreadable {}", path.display());
        return;
    };
    app.notes.setup_dismissed |= state[UI_STATE_SETUP_KEY].as_bool().unwrap_or(false);
    app.notes.notes_box_dismissed |= state[UI_STATE_NOTES_BOX_KEY].as_bool().unwrap_or(false);
}

/// Hides the library's setup card for good and writes that down at once.
pub(crate) fn dismiss_setup(app: &mut PdfCraftApp) {
    ensure_ui_state(app);
    app.notes.setup_dismissed = true;
    write_ui_state(app);
}

/// Hides the Highlights pane's "connect a vault" card for good and writes that down at once.
pub(crate) fn dismiss_notes_box(app: &mut PdfCraftApp) {
    ensure_ui_state(app);
    app.notes.notes_box_dismissed = true;
    write_ui_state(app);
}

fn ui_state_path(app: &PdfCraftApp) -> Option<PathBuf> {
    app.margin_keys.data_dir().map(|dir| dir.join(UI_STATE_FILE))
}

/// Writes both flags to `ui-state.json`, replacing the file in one step. A failure is told in a
/// notice; the flag still holds for this run and the app state keeps the setup card's.
fn write_ui_state(app: &mut PdfCraftApp) {
    let Some(path) = ui_state_path(app) else { return };
    let mut state = serde_json::Map::new();
    state.insert(UI_STATE_SETUP_KEY.to_owned(), serde_json::Value::Bool(app.notes.setup_dismissed));
    state.insert(UI_STATE_NOTES_BOX_KEY.to_owned(), serde_json::Value::Bool(app.notes.notes_box_dismissed));
    let state = serde_json::Value::Object(state);
    let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| write_atomically(&path, &state.to_string()));
    if let Err(err) = written {
        app.notify(format!("Couldn't remember that choice in {}: {err}", path.display()));
    }
}

/// A 0–1 colour as 8-bit channels (out-of-range values clamp).
fn rgb_bytes(rgb: [f32; 3]) -> [u8; 3] {
    rgb.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// A file-name slug: lowercase letters and digits, everything else folded into single dashes.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() { "untitled".to_string() } else { trimmed.to_string() }
}

/// The reading note's vault-relative path (`/`-separated) from `template`. A path that could
/// leave the vault (absolute, a drive, `..`, `.`, backslashes, empty parts) is refused.
pub fn note_path(template: &str, title: &str) -> Result<String, String> {
    let path = template.trim().replace(TITLE_SLUG, &slug(title));
    let refuse = |why: &str| Err(format!("The note path {path} {why}."));
    if path.is_empty() {
        return Err("The note path is empty.".to_string());
    }
    if path.contains('\\') || path.contains('\0') {
        return refuse("may only use / between folders");
    }
    if path.starts_with('/') || path.chars().nth(1) == Some(':') {
        return refuse("must be inside the vault, not an absolute path");
    }
    if path.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
        return refuse("must stay inside the vault (no .. or empty folders)");
    }
    if !path.ends_with(".md") {
        return refuse("must end in .md");
    }
    Ok(path)
}

/// Where the callout links to: a PDF inside the vault (by its vault-relative path, or by name
/// when the document has no file), or a PDF elsewhere on disk (by absolute path).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PdfLink {
    Vault(String),
    External(PathBuf),
}

impl PdfLink {
    pub fn for_pdf(vault: Option<&Path>, pdf: Option<&Path>, name: &str) -> Self {
        let Some(pdf) = pdf else { return Self::Vault(name.to_string()) };
        let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let pdf = canonical(pdf);
        match vault.map(canonical).and_then(|v| pdf.strip_prefix(v).ok().map(Path::to_path_buf)) {
            Some(rel) => Self::Vault(rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")),
            None => Self::External(pdf),
        }
    }

    pub fn is_external(&self) -> bool {
        matches!(self, Self::External(_))
    }

    /// PDF++'s `{{linkWithDisplay}}` to page `page` (0-based): a wikilink for a vault PDF, a
    /// markdown link to the file otherwise.
    pub fn markdown(&self, page: usize, display: &str) -> String {
        let n = page.saturating_add(1);
        match self {
            // Wikilinks can't carry these characters in the target; a percent-encoded markdown
            // link to the same vault file can.
            Self::Vault(rel) if rel.contains(['#', '|', '^', '[', ']']) => {
                format!("[{display}]({}#page={n})", percent_encode(rel, true))
            }
            Self::Vault(rel) => format!("[[{rel}#page={n}|{display}]]"),
            Self::External(abs) => format!("[{display}]({}#page={n})", file_url(abs)),
        }
    }
}

/// `file:///…` for an absolute path, percent-encoded.
fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let rooted = if text.starts_with('/') { text } else { format!("/{text}") };
    format!("file://{}", percent_encode(&rooted, true))
}

/// Percent-encode like `encodeURIComponent` (RFC 3986 unreserved characters stay), keeping `/`
/// when `keep_slash`.
fn percent_encode(text: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') || (keep_slash && b == b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `obsidian://open?vault=<name>&file=<note>`; the vault's name is its folder name.
pub fn obsidian_uri(vault: &Path, note: &str) -> Option<String> {
    let name = vault.file_name()?.to_string_lossy();
    Some(format!("obsidian://open?vault={}&file={}", percent_encode(&name, false), percent_encode(note, false)))
}

/// One text-markup comment, ready to format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Highlight {
    /// The idempotency key: the annotation's `/NM`, or its page and box when it has none.
    pub key: String,
    /// 0-based page.
    pub page: usize,
    /// The page label (`p.<label>` in the display text).
    pub label: String,
    /// PDF++ colour: a name from `notes.colors` (lowercase) or `r,g,b`.
    pub color: String,
    /// The marked text.
    pub text: String,
    /// The comment typed for it.
    pub note: String,
    /// When it was last changed, `YYYY-MM-DD`; empty when the PDF doesn't say.
    pub date: String,
}

/// The calendar date of a PDF date string (`D:YYYYMMDDHHmmSS…`) as `YYYY-MM-DD`, or empty when
/// it isn't one.
fn pdf_date(text: Option<&str>) -> String {
    let Some(text) = text else { return String::new() };
    let digits = text.trim().strip_prefix("D:").unwrap_or(text.trim());
    let (Some(year), Some(month), Some(day)) = (digits.get(0..4), digits.get(4..6), digits.get(6..8)) else { return String::new() };
    let number = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit()).then(|| part.parse::<u32>().ok()).flatten();
    match (number(year), number(month), number(day)) {
        (Some(_), Some(1..=12), Some(1..=31)) => format!("{year}-{month}-{day}"),
        _ => String::new(),
    }
}

/// The display text of a link: PDF++'s `{{file.basename}}, p.{{pageLabel}}`, without the
/// characters that would end a wikilink.
fn display_text(title: &str, label: &str) -> String {
    let clean: String = title
        .chars()
        .map(|c| match c {
            '[' => '(',
            ']' => ')',
            '|' => '-',
            other => other,
        })
        .collect();
    format!("{clean}, p.{label}")
}

/// The PDF++ "Quote in callout" for one highlight:
///
/// ```text
/// > [!PDF|yellow] [[papers/x.pdf#page=3|x, p.3]]
/// > > the marked text
/// >
/// > the note
/// ```
pub fn callout(h: &Highlight, link: &PdfLink, title: &str) -> String {
    let mut out = format!("> [!{CALLOUT_TYPE}|{}] {}\n", h.color, link.markdown(h.page, &display_text(title, &h.label)));
    let quote = h.text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !quote.is_empty() {
        out.push_str(&format!("> > {quote}\n"));
    }
    let note = h.note.trim();
    if !note.is_empty() {
        out.push_str(">\n");
        for line in note.lines() {
            out.push_str(&format!("> {}\n", line.trim_end()).replace("> \n", ">\n"));
        }
    }
    out
}

/// The marker line in front of an entry. Keys are cut to characters a comment can't misread.
fn marker(key: &str) -> String {
    let safe: String =
        key.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '{' | '}') { c } else { '_' }).collect();
    format!("{MARKER_OPEN}{safe}{MARKER_CLOSE}")
}

/// Byte offset just past the `## Highlights` line.
fn find_heading(text: &str) -> Option<usize> {
    let mut offset = 0;
    let mut inside_block = false;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        update_block_boundary(line, &mut inside_block);
        if !inside_block && line.trim_end() == HIGHLIGHTS_HEADING {
            return Some(offset);
        }
    }
    None
}

/// Byte offset of the first `#` or `##` heading at or after `from`.
fn next_section(text: &str, from: usize) -> Option<usize> {
    let mut offset = from;
    let mut inside_block = false;
    for line in text.get(from..)?.split_inclusive('\n') {
        update_block_boundary(line, &mut inside_block);
        if !inside_block && (line.starts_with("# ") || line.starts_with("## ")) {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

fn update_block_boundary(line: &str, inside: &mut bool) {
    if (line.starts_with("<!-- margin:") && line.contains(" sha256:")) || line.starts_with("<!-- margin-snapshot:") {
        *inside = true;
    } else if line.starts_with("<!-- /margin:") || line.starts_with("<!-- /margin-snapshot:") {
        *inside = false;
    }
}

/// The text of `page` under a text-markup comment: the glyphs whose centres fall inside one of its
/// quads (or its box), in reading order, words separated by single spaces.
pub fn marked_text(text: &PageText, page: &PageInfo, a: &Annotation) -> String {
    let quads: Vec<[f32; 8]> = if a.quads.is_empty() {
        let [x0, y0, x1, y1] = a.rect;
        vec![[x0, y1, x1, y1, x0, y0, x1, y0]]
    } else {
        a.quads.clone()
    };
    let boxes: Vec<[f32; 4]> = quads
        .iter()
        .map(|q| {
            let corners = [0, 2, 4, 6].map(|i| page.user_to_view(q[i], q[i + 1]));
            let xs = corners.map(|p| p[0]);
            let ys = corners.map(|p| p[1]);
            [
                xs.into_iter().fold(f32::INFINITY, f32::min),
                ys.into_iter().fold(f32::INFINITY, f32::min),
                xs.into_iter().fold(f32::NEG_INFINITY, f32::max),
                ys.into_iter().fold(f32::NEG_INFINITY, f32::max),
            ]
        })
        .collect();
    let mut out = String::new();
    let mut previous: Option<usize> = None;
    for (i, glyph) in text.glyphs.iter().enumerate() {
        let (cx, cy) = ((glyph.rect[0] + glyph.rect[2]) / 2.0, (glyph.rect[1] + glyph.rect[3]) / 2.0);
        if !boxes.iter().any(|b| cx >= b[0] && cx <= b[2] && cy >= b[1] && cy <= b[3]) {
            continue;
        }
        if let Some(p) = previous {
            let same_line = text.line_of.get(p) == text.line_of.get(i);
            let joined = same_line && p + 1 == i && !text.space_before.get(i).copied().unwrap_or(true);
            if !joined {
                out.push(' ');
            }
        }
        out.push_str(&glyph.text);
        previous = Some(i);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn is_markup(a: &Annotation) -> bool {
    a.in_reply_to.is_none() && a.state.is_none() && MARKUP_SUBTYPES.contains(&a.subtype.as_str())
}

/// The idempotency key of a comment: its `/NM`, or its page and box.
pub fn entry_key(a: &Annotation) -> String {
    match a.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => name.to_string(),
        None => {
            let [x0, y0, x1, y1] = a.rect.map(|v| v.round() as i64);
            format!("p{}-{x0}-{y0}-{x1}-{y1}", a.page.saturating_add(1))
        }
    }
}

/// Where the text-markup comments of a document sit in `info.annotations`, in reading order
/// (page, then top to bottom, then left to right).
fn markup_indices(info: &DocInfo) -> Vec<usize> {
    let mut indices: Vec<usize> = info.annotations.iter().enumerate().filter_map(|(index, a)| is_markup(a).then_some(index)).collect();
    indices.sort_by(|left, right| match (info.annotations.get(*left), info.annotations.get(*right)) {
        (Some(a), Some(b)) => a.page.cmp(&b.page).then(b.rect[3].total_cmp(&a.rect[3])).then(a.rect[0].total_cmp(&b.rect[0])),
        _ => std::cmp::Ordering::Equal,
    });
    indices
}

/// Write `contents` to `path` through a temporary file in the same folder and a rename, so the
/// note is never seen half written.
pub fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    write_bytes_atomically(path, contents.as_bytes())
}

fn write_bytes_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let folder = path.parent().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "the export has no folder"))?;
    let name =
        path.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "the export has no file name"))?.to_string_lossy();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(std::io::Error::other)?.as_nanos();
    let temporary = folder.join(format!(".{name}.margin-{}-{stamp}.tmp", std::process::id()));
    // create_new refuses stale files and symlinks; never truncate somebody else's temporary file.
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all()).and_then(|()| std::fs::rename(&temporary, path));
    if result.is_err()
        && let Err(error) = std::fs::remove_file(&temporary)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("Couldn't remove temporary export {}: {error}", temporary.display());
    }
    result
}

/// The note's absolute path: `rel` under `vault`, refused when the vault is missing or when a
/// folder on the way (a symbolic link) leads outside it. Creates the note's folders.
pub fn resolve_note(vault: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = note_path(rel, "")?;
    let vault_real = vault.canonicalize().map_err(|e| format!("The vault folder {} can't be opened: {e}.", vault.display()))?;
    if !vault_real.is_dir() {
        return Err(format!("The vault {} isn't a folder.", vault.display()));
    }
    let full = vault_real.join(&rel);
    let outside = || format!("Margin won't write outside the vault: {rel} leads out of {}.", vault.display());
    // Check the deepest folder that exists before creating any, so a linked folder can't make
    // Margin create folders elsewhere.
    let parent = full.parent().ok_or_else(outside)?;
    let existing = parent.ancestors().find(|p| p.exists()).ok_or_else(outside)?;
    let inside = |p: &Path| p.canonicalize().is_ok_and(|real| real.starts_with(&vault_real));
    if !inside(existing) {
        return Err(outside());
    }
    std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't create the folder for {rel}: {e}."))?;
    if !inside(parent) || (full.exists() && !inside(&full)) {
        return Err(outside());
    }
    Ok(full)
}

/// Read the note at `path` (`None` when it doesn't exist yet).
fn read_note(path: &Path) -> Result<Option<String>, String> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Couldn't read {}: {e}.", path.display())),
        Ok(m) if m.len() > MAX_NOTE_BYTES => return Err(format!("{} is too large to be a reading note.", path.display())),
        Ok(_) => {}
    }
    std::fs::read_to_string(path).map(Some).map_err(|e| format!("Couldn't read {}: {e}.", path.display()))
}

/// Immutable note context; canonical paths are resolved only when the document cache changes.
#[derive(Clone, Debug)]
struct DocNotes {
    title: String,
    link: PdfLink,
    note: Result<String, String>,
    variables: BTreeMap<String, String>,
    owner: String,
}

impl DocNotes {
    fn of(notes: &Notes, doc: &Document) -> Self {
        let file = doc.path.as_deref().map(Path::new);
        let name = file.and_then(Path::file_name).map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| doc.name.clone());
        let title = doc
            .info
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| Path::new(&name).file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| name.clone()));
        let link = PdfLink::for_pdf(notes.vault.as_deref(), file, &name);
        let source = match &link {
            PdfLink::Vault(path) => format!("[[{path}]]"),
            PdfLink::External(path) => file_url(path),
        };
        let owner = if file.is_some() { source.clone() } else { format!("{source}:{:?}", doc.id) };
        let mut variables = BTreeMap::from([
            ("title".into(), title.clone()),
            ("title-slug".into(), slug(&title)),
            ("authors".into(), doc.info.author.clone().unwrap_or_default()),
            ("tags".into(), doc.info.keywords.clone().unwrap_or_default()),
            ("source".into(), source),
            ("date".into(), current_date()),
            ("progress".into(), String::new()),
        ]);
        for name in ["year", "doi", "arxiv", "page", "color", "color-name", "text", "note", "link"] {
            variables.insert(name.into(), String::new());
        }
        variables.insert("citekey".into(), derive_citekey(&variables));
        let note = note_for(notes, &variables);
        Self { title, link, note, variables, owner }
    }

    /// Title, authors and reading progress from the library, which knows them better than the
    /// PDF's own metadata.
    #[cfg(not(target_arch = "wasm32"))]
    fn with_book(mut self, notes: &Notes, book: &margin_config::library::Book) -> Self {
        self.title = book.title.clone();
        self.variables.insert("title".into(), book.title.clone());
        self.variables.insert("title-slug".into(), slug(&book.title));
        self.variables.insert("authors".into(), book.author.clone());
        self.variables.insert("progress".into(), format!("{:.0}", (book.progress * 100.0).clamp(0.0, 100.0)));
        self.variables.insert("citekey".into(), derive_citekey(&self.variables));
        self.note = note_for(notes, &self.variables);
        self
    }

    fn header(&self, notes: &Notes) -> Result<String, String> {
        let mut header = format::render(&notes.config.front_matter_template, &self.variables, TemplateContext::FrontMatter)?;
        header.push_str(&format::render(&notes.config.header_template, &self.variables, TemplateContext::Text)?);
        Ok(header)
    }

    fn highlight(&self, notes: &Notes, highlight: &Highlight) -> Result<String, String> {
        let mut variables = self.variables.clone();
        let color_name = notes
            .colors
            .iter()
            .find(|color| color.name.eq_ignore_ascii_case(&highlight.color))
            .map(|color| color.name.clone())
            .unwrap_or_else(|| highlight.color.clone());
        variables.extend([
            ("page".into(), highlight.label.clone()),
            ("color".into(), highlight.color.clone()),
            ("color-name".into(), color_name),
            ("text".into(), highlight.text.clone()),
            ("note".into(), highlight.note.clone()),
            ("link".into(), self.link.markdown(highlight.page, &display_text(&self.title, &highlight.label))),
            ("callout".into(), callout(highlight, &self.link, &self.title)),
        ]);
        format::render(&notes.config.highlight_template, &variables, TemplateContext::Text)
    }
}

/// The reading note's vault-relative path from `notes.note_template`, or why there is none.
fn note_for(notes: &Notes, variables: &BTreeMap<String, String>) -> Result<String, String> {
    if notes.vault.is_none() {
        return Err(match &notes.config.vault {
            Some(relative) => format!("notes.vault must be a full folder path, not {}", relative.display()),
            None => NO_VAULT_WHY.into(),
        });
    }
    format::render(&notes.note_template, variables, TemplateContext::Path).and_then(|path| note_path(&path, ""))
}

/// The note context of `doc`, with the library's title, authors and progress when the library
/// has the book. Reads the file system (canonical paths): call it when something changed or a
/// job starts, never every frame.
fn context_for(app: &PdfCraftApp, doc: &Document) -> DocNotes {
    let context = DocNotes::of(&app.notes, doc);
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(book) = app.margin_library.books().iter().find(|book| Some(&book.path) == doc.path.as_ref()) {
        return context.with_book(&app.notes, book);
    }
    context
}

fn derive_citekey(variables: &BTreeMap<String, String>) -> String {
    let author = variables
        .get("authors")
        .and_then(|authors| authors.split([',', ';']).next())
        .and_then(|author| author.split_whitespace().last())
        .unwrap_or_default();
    let title = variables.get("title").and_then(|title| title.split_whitespace().next()).unwrap_or("untitled");
    format!("{}{}-{}", slug(author), variables.get("year").map(String::as_str).unwrap_or_default(), slug(title))
}

fn current_date() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    // Gregorian civil date from whole UTC days; no local timezone or filesystem queries.
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX / 2).saturating_add(719_468);
    let era = days / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = month_index + if month_index < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[derive(Clone, Debug)]
struct CachedDocument {
    path: Option<String>,
    generation: u64,
    vault: Option<PathBuf>,
    template: String,
    context: DocNotes,
    indices: Arc<Vec<usize>>,
    shown: Result<String, String>,
}

/// Builds [`Highlight`]s for one document, extracting page text on demand.
struct HighlightReader<'a> {
    notes: &'a Notes,
    doc: &'a Document,
    loaded: &'a HashMap<usize, Arc<PageText>>,
    renderer: Option<PageRenderer>,
    extracted: HashMap<usize, Option<Arc<PageText>>>,
}

impl<'a> HighlightReader<'a> {
    fn new(notes: &'a Notes, doc: &'a Document, loaded: &'a HashMap<usize, Arc<PageText>>) -> Self {
        Self { notes, doc, loaded, renderer: None, extracted: HashMap::new() }
    }

    fn page_text(&mut self, page: usize) -> Option<Arc<PageText>> {
        if let Some(t) = self.loaded.get(&page) {
            return Some(t.clone());
        }
        let doc = self.doc;
        let renderer = self.renderer.get_or_insert_with(|| {
            let config = RenderConfig { password: doc.password.as_deref().map(Arc::from), ..RenderConfig::default() };
            PageRenderer::new(doc.bytes.clone(), config)
        });
        self.extracted
            .entry(page)
            .or_insert_with(|| renderer.render(RenderRequest { page, kind: RequestKind::Text, tile: None, scale: 1.0, tag: 0 }).text)
            .clone()
    }

    fn highlight(&mut self, a: &Annotation) -> Highlight {
        let text = self.page_text(a.page);
        highlight_of(self.notes, &self.doc.info, a, text.as_deref())
    }
}

/// The [`Highlight`] of markup comment `a`, its quote read from `text` (its page's text).
fn highlight_of(notes: &Notes, info: &DocInfo, a: &Annotation, text: Option<&PageText>) -> Highlight {
    let page_info = info.pages.get(a.page);
    let quote = match (text, page_info) {
        (Some(text), Some(page)) => marked_text(text, page, a),
        _ => String::new(),
    };
    Highlight {
        key: entry_key(a),
        page: a.page,
        label: page_info.map(|p| p.label.clone()).unwrap_or_else(|| a.page.saturating_add(1).to_string()),
        color: notes.color_label(a.color),
        text: quote,
        note: a.contents.clone().unwrap_or_default(),
        date: pdf_date(a.modified.as_deref()),
    }
}

/// The length of an RFC 3339 timestamp's calendar date (`YYYY-MM-DD`).
const CALENDAR_DATE_LEN: usize = 10;

/// View `view`'s sidecar highlights and page notes (`margin::marks`) as [`Highlight`]s keyed by
/// their ULID. A page note has no quote and takes the palette's "Note" colour.
fn sidecar_entries(notes: &Notes, info: &DocInfo, view: &crate::canvas::DocView) -> Vec<Highlight> {
    use margin_config::marks::locator_page;
    let label = |page: usize| info.pages.get(page).map_or_else(|| page.saturating_add(1).to_string(), |page_info| page_info.label.clone());
    let calendar_date = |created: &str, modified: Option<&str>| modified.unwrap_or(created).get(..CALENDAR_DATE_LEN).unwrap_or_default().to_owned();
    let note_color = notes
        .colors
        .iter()
        .find(|color| color.name.eq_ignore_ascii_case("note"))
        .map_or_else(|| notes.color_label(None), |color| color.name.to_lowercase());
    let highlights = super::marks::sidecar_highlights(view).iter().filter_map(|highlight| {
        let page = locator_page(&highlight.locator)?;
        Some(Highlight {
            key: highlight.id.clone(),
            page,
            label: label(page),
            color: notes.color_label(Some(super::marks::shown_rgb(notes, highlight))),
            text: highlight.text.clone().unwrap_or_default(),
            note: highlight.note.clone().unwrap_or_default(),
            date: calendar_date(&highlight.created, highlight.modified.as_deref()),
        })
    });
    let page_notes = super::marks::page_notes(view).iter().filter_map(|page_note| {
        let page = locator_page(&page_note.locator)?;
        Some(Highlight {
            key: page_note.id.clone(),
            page,
            label: label(page),
            color: note_color.clone(),
            text: String::new(),
            note: page_note.text.clone(),
            date: calendar_date(&page_note.created, page_note.modified.as_deref()),
        })
    });
    highlights.chain(page_notes).collect()
}

/// What a notes button asked for; the app carries it out in [`tick`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Copy the callout of the comment at (page, index).
    CopyCallout(usize, usize),
    /// Open the reading note in Obsidian, first adding the comment at (page, index) if given.
    Open(Option<(usize, usize)>),
    /// Copy every highlight's callout.
    CopyAll,
    /// Synchronize highlights without overwriting user-edited blocks.
    Write,
    /// Turn "Add highlights to this note" on or off for the document.
    ToggleAutoadd,
    SetTarget(ExportTarget),
    ExportHighlights,
    ExportPaper(bool),
    /// Ask for the vault folder (the "connect a vault" card's button).
    ChooseVault,
    /// Hide the "connect a vault" card for good.
    HideVaultPrompt,
}

/// Per-view state the notes buttons read and write (lives in `CommentView`). `enabled` is only
/// ever set in Margin, so PdfCraft draws none of it.
#[derive(Clone, Debug)]
pub struct NotesUi {
    pub enabled: bool,
    pub autoadd: bool,
    /// The reading note as shown ("Vault / reading / title.md"), or why there is none.
    pub note: Result<String, String>,
    /// The PDF isn't in the vault, so callouts link to its file.
    pub external: bool,
    pub request: Option<Request>,
    pub target: ExportTarget,
    pub progress: Option<(usize, usize)>,
    pub(crate) markup_indices: Arc<Vec<usize>>,
    /// The "connect a vault" card was closed (`Notes::notes_box_dismissed`).
    pub vault_prompt_hidden: bool,
}

impl NotesUi {
    pub(crate) fn ask(&mut self, ctx: &egui::Context, request: Request) {
        self.request = Some(request);
        ctx.request_repaint();
    }
}

impl Default for NotesUi {
    fn default() -> Self {
        Self {
            enabled: false,
            autoadd: false,
            note: Err(String::new()),
            external: false,
            request: None,
            target: ExportTarget::Obsidian,
            progress: None,
            markup_indices: Arc::new(Vec::new()),
            vault_prompt_hidden: false,
        }
    }
}

pub(crate) const EXTERNAL_TIP: &str =
    "This PDF isn't in the vault, so the link opens its file at #page=N (file:// form). Move the PDF into the vault for PDF++ links.";

/// "Copy callout" and "Open in Obsidian" in a text-markup comment's menus.
pub(crate) fn menu_items(ui: &mut egui::Ui, notes: &mut NotesUi, a: &Annotation) {
    if !notes.enabled || !is_markup(a) {
        return;
    }
    if look::menu_row(ui, Some("copy"), "Copy callout", None, false)
        .on_hover_text(if notes.external { EXTERNAL_TIP } else { "Copy as a PDF++ callout" })
        .clicked()
    {
        notes.ask(ui.ctx(), Request::CopyCallout(a.page, a.index));
        ui.close();
    }
    if look::menu_row(ui, Some("external-link"), "Open in Obsidian", None, false)
        .on_hover_text("Add this highlight to the reading note and open it in Obsidian")
        .clicked()
    {
        notes.ask(ui.ctx(), Request::Open(Some((a.page, a.index))));
        ui.close();
    }
}

/// The mockup's per-highlight buttons on a highlights-list card (right to left: open, copy).
/// A card that shows its own "…" menu (selected and editable) has them in that menu instead,
/// as its header has no room for both.
pub(crate) fn card_buttons(ui: &mut egui::Ui, notes: &mut NotesUi, a: &Annotation, has_menu: bool) {
    if !notes.enabled || !is_markup(a) || has_menu {
        return;
    }
    if look::icon_button(ui, "external-link", look::SMALL_BUTTON, false, "Open this callout in Obsidian", None).clicked() {
        notes.ask(ui.ctx(), Request::Open(Some((a.page, a.index))));
    }
    let tip = if notes.external { EXTERNAL_TIP } else { "Copy callout" };
    if look::icon_button(ui, "copy", look::SMALL_BUTTON, false, tip, None).clicked() {
        notes.ask(ui.ctx(), Request::CopyCallout(a.page, a.index));
    }
}

/// The notes area at the top of the highlights list. Without a reading note (no vault) it starts
/// with the "connect a vault" card: one line, "Choose vault…" and a close button that hides it
/// for good. Then the reading-note box (the mockup's `.note-link`), padded by the large step,
/// rows the small step apart: the note's path with "Open note" and a fold button on one fixed
/// row, the "Add highlights to this note" switch, the export format and Export, then "Copy all"
/// and "Write to note" sharing one row equally. Folded, only the first row shows.
pub(crate) fn panel_block(ui: &mut egui::Ui, t: &Tokens, notes: &mut NotesUi) {
    if !notes.enabled {
        return;
    }
    if notes.note.is_err() && !notes.vault_prompt_hidden {
        vault_prompt(ui, t, notes);
        ui.add_space(look::GAP);
    }
    let folded_id = egui::Id::new("margin-notes-box-folded");
    let folded = ui.data(|d| d.get_temp::<bool>(folded_id)).unwrap_or(false);
    look::card_frame(t).inner_margin(look::GAP_LARGE).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = egui::vec2(look::GAP, look::GAP);
        if note_row(ui, t, notes, folded) {
            ui.data_mut(|d| d.insert_temp(folded_id, !folded));
        }
        if !folded {
            autoadd_row(ui, t, notes);
            export_row(ui, notes);
            action_row(ui, notes);
        }
    });
    ui.add_space(look::GAP);
}

/// The text sizes of the "connect a vault" card.
const PROMPT_TITLE_TEXT: f32 = 12.5;

/// The "connect a vault" card: what a vault is for, why there is none when the setting is wrong,
/// "Choose vault…", and the close button.
fn vault_prompt(ui: &mut egui::Ui, t: &Tokens, notes: &mut NotesUi) {
    let why = notes.note.as_ref().err().filter(|why| why.as_str() != NO_VAULT_WHY).cloned();
    let (choose, closed) = look::banner_card(ui, "Hide vault prompt", |ui| {
        ui.spacing_mut().item_spacing.y = look::GAP;
        let title = egui::RichText::new("Connect an Obsidian vault to keep notes").font(theme::medium(PROMPT_TITLE_TEXT)).color(t.text);
        ui.add(egui::Label::new(title).wrap().selectable(false));
        if let Some(why) = &why {
            ui.add(egui::Label::new(egui::RichText::new(why).font(theme::regular(NOTE_SUB_TEXT)).color(t.text_faint)).wrap().selectable(false));
        }
        look::Button::primary(look::ButtonSize::Compact, "Choose vault…").icon("folder-open").show(ui).clicked()
    });
    if choose {
        notes.ask(ui.ctx(), Request::ChooseVault);
    }
    if closed {
        notes.ask(ui.ctx(), Request::HideVaultPrompt);
    }
}

/// Icon and text sizes of the notes box.
const NOTE_ICON: f32 = 14.0;
const NOTE_PATH_TEXT: f32 = 12.0;
const NOTE_TITLE_TEXT: f32 = 12.5;
const NOTE_SUB_TEXT: f32 = 11.5;
/// Space between the switch's label and the line under it.
const NOTE_SUB_GAP: f32 = 2.0;
const AUTOADD_TITLE: &str = "Add highlights to this note";
const AUTOADD_SUB: &str = "Each new highlight becomes a PDF++ callout";
/// Why there is no reading note when no vault is set (the "connect a vault" card says it better).
const NO_VAULT_WHY: &str = "Set notes.vault to your Obsidian vault folder first.";

/// The box's first row, one compact control tall whatever it says: the reading note's path,
/// truncated, with "Open note"; or why there is none, faint and truncated (the whole reason in
/// its tooltip); then the fold button. True when the fold button was clicked.
fn note_row(ui: &mut egui::Ui, t: &Tokens, notes: &mut NotesUi, folded: bool) -> bool {
    let width = ui.available_width();
    let (text, color, has_note) = match &notes.note {
        Ok(path) => (path.clone(), t.text_muted, true),
        Err(why) => (why.clone(), t.text_faint, false),
    };
    let row = ui.allocate_ui_with_layout(egui::vec2(width, look::BUTTON_HEIGHT), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.add(icons::image("file-text", NOTE_ICON, t.icon));
        let buttons = if has_note { 2.0 } else { 1.0 };
        let label_width = (ui.available_width() - buttons * (look::BUTTON_HEIGHT + look::GAP)).max(0.0);
        ui.allocate_ui_with_layout(egui::vec2(label_width, look::BUTTON_HEIGHT), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_width(label_width);
            let label =
                ui.add(egui::Label::new(egui::RichText::new(&text).font(theme::regular(NOTE_PATH_TEXT)).color(color)).truncate().selectable(false));
            if !has_note {
                label.on_hover_text(&text);
            }
        });
        if has_note && look::icon_button(ui, "external-link", look::BUTTON_HEIGHT, false, "Open note in Obsidian", None).clicked() {
            notes.ask(ui.ctx(), Request::Open(None));
        }
        let (icon, name) = if folded { ("chevron-down", "Show the notes box") } else { ("chevron-up", "Fold the notes box") };
        look::icon_button(ui, icon, look::BUTTON_HEIGHT, false, name, None).clicked()
    });
    row.inner
}

/// "Add highlights to this note" and its explanation, with the switch against the right edge,
/// centred on the text.
fn autoadd_row(ui: &mut egui::Ui, t: &Tokens, notes: &mut NotesUi) {
    let has_note = notes.note.is_ok();
    let width = ui.available_width();
    let text_width = (width - look::SWITCH_SIZE.x - look::GAP_LARGE).max(0.0);
    let (title_color, sub_color) = if has_note { (t.text, t.text_faint) } else { (t.text_faint, t.text_faint) };
    let title = ui.painter().layout(AUTOADD_TITLE.to_owned(), theme::regular(NOTE_TITLE_TEXT), title_color, text_width);
    let sub = ui.painter().layout(AUTOADD_SUB.to_owned(), theme::regular(NOTE_SUB_TEXT), sub_color, text_width);
    let text_height = title.size().y + NOTE_SUB_GAP + sub.size().y;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, text_height.max(look::SWITCH_SIZE.y)), egui::Sense::hover());
    let top = rect.center().y - text_height / 2.0;
    let sub_top = top + title.size().y + NOTE_SUB_GAP;
    ui.painter().galley(egui::pos2(rect.left(), top), title, title_color);
    ui.painter().galley(egui::pos2(rect.left(), sub_top), sub, sub_color);
    let switch_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - look::SWITCH_SIZE.x / 2.0, rect.center().y), look::SWITCH_SIZE);
    // A child placed on the row, not allocated: allocating would move the cursor back up to the
    // switch's bottom edge, and the next row would overlap this one.
    let mut place = ui.new_child(egui::UiBuilder::new().max_rect(switch_rect));
    if place.add_enabled_ui(has_note, |ui| look::switch(ui, notes.autoadd, AUTOADD_TITLE)).inner.clicked() {
        notes.ask(ui.ctx(), Request::ToggleAutoadd);
    }
}

/// "Copy all" and "Write to note", sharing the row's width equally.
fn action_row(ui: &mut egui::Ui, notes: &mut NotesUi) {
    let width = ((ui.available_width() - look::GAP) / 2.0).max(0.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = look::GAP;
        let copy = look::Button::secondary(look::ButtonSize::Compact, "Copy all").icon("copy").fixed_width(width).show(ui);
        let copy_tip = format!("Copy every highlight as {}", target_label(notes.target));
        if look::tooltip(copy, &copy_tip, None).clicked() {
            notes.ask(ui.ctx(), Request::CopyAll);
        }
        let write = ui
            .add_enabled_ui(notes.note.is_ok(), |ui| {
                look::Button::secondary(look::ButtonSize::Compact, "Write to note").icon("file-pen-line").fixed_width(width).show(ui)
            })
            .inner;
        let write_tip = match &notes.note {
            Ok(_) => "Update Margin's highlight blocks; preserve user edits".to_owned(),
            Err(why) => format!("Write to note: {why}"),
        };
        if look::tooltip(write, &write_tip, None).clicked() {
            notes.ask(ui.ctx(), Request::Write);
        }
    });
}

fn target_label(target: ExportTarget) -> &'static str {
    match target {
        ExportTarget::Obsidian => "Obsidian / PDF++",
        ExportTarget::Markdown => "Markdown",
        ExportTarget::Logseq => "Logseq",
        ExportTarget::ReadwiseCsv => "Readwise CSV",
        ExportTarget::Json => "JSON",
    }
}

/// Every export target, in the picker's order.
const TARGETS: [ExportTarget; 5] =
    [ExportTarget::Obsidian, ExportTarget::Markdown, ExportTarget::Logseq, ExportTarget::ReadwiseCsv, ExportTarget::Json];
/// The name screen readers (and tests) find the export target picker by.
pub const TARGET_PICKER_NAME: &str = "Highlight export format";
/// The Export button's label while an export runs, at its widest, so the button keeps one width.
const EXPORTING_TEMPLATE: &str = "Exporting 000 / 000";

/// The highlight export format (also what Copy all copies) and the Export menu, both compact
/// dropdown buttons of a fixed width. A running export shows its progress in the Export button
/// itself, so no row comes and goes in the box.
fn export_row(ui: &mut egui::Ui, notes: &mut NotesUi) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = look::GAP;
        let picker = look::Button::secondary(look::ButtonSize::Compact, target_label(notes.target)).trailing("chevron-down").name(TARGET_PICKER_NAME);
        let labels = TARGETS.map(target_label);
        let picker_width = picker.widest(ui, &labels);
        let picker = picker.fixed_width(picker_width).show(ui);
        let mut selected = notes.target;
        look::popover(&picker, egui::Id::new("margin-export-target")).show(|ui| {
            look::menu_body(ui);
            for target in TARGETS {
                if look::menu_row(ui, None, target_label(target), None, target == selected).clicked() {
                    selected = target;
                    ui.close();
                }
            }
        });
        if selected != notes.target {
            notes.ask(ui.ctx(), Request::SetTarget(selected));
        }
        let label = match notes.progress {
            Some((done, total)) => format!("Exporting {done} / {total}"),
            None => "Export".to_owned(),
        };
        let export = look::Button::secondary(look::ButtonSize::Compact, &label).trailing("chevron-down").name("Export");
        let export_width = export.widest(ui, &["Export", EXPORTING_TEMPLATE]);
        let export = export.fixed_width(export_width).show(ui);
        egui::Popup::menu(&export).show(|ui| {
            look::menu_body(ui);
            if look::menu_row(ui, Some("file-output"), "Export highlights…", None, false).clicked() {
                notes.ask(ui.ctx(), Request::ExportHighlights);
            }
            if look::menu_row(ui, Some("file-text"), "Export paper as Markdown…", None, false).clicked() {
                notes.ask(ui.ctx(), Request::ExportPaper(false));
            }
            let to_vault =
                ui.add_enabled_ui(notes.note.is_ok(), |ui| look::menu_row(ui, Some("folder-open"), "Export paper to vault", None, false)).inner;
            if to_vault.clicked() {
                notes.ask(ui.ctx(), Request::ExportPaper(true));
            }
        });
    });
}

/// The palette the colour swatches show in Margin (PdfCraft's own swatches otherwise).
pub(crate) fn swatches(ctx: &egui::Context) -> Option<Vec<(String, [f64; 3])>> {
    ctx.data(|d| d.get_temp::<Arc<Vec<(String, [f64; 3])>>>(palette_id())).map(|p| p.as_ref().clone())
}

fn palette_id() -> egui::Id {
    egui::Id::new("margin-notes-palette")
}

/// Once per frame in Margin: hand the palette to the comment tools, refresh every view's notes
/// state, carry out the buttons' requests and write new highlights for documents with autoadd on.
pub fn tick(app: &mut PdfCraftApp) {
    if app.flavor != Flavor::Margin {
        return;
    }
    if !app.notes.palette_published {
        publish_palette(app);
    }
    ensure_ui_state(app);
    poll_jobs(app);
    for i in 0..app.views.len() {
        refresh_view(app, i);
        if let Some(request) = app.views.get_mut(i).and_then(|v| v.comments.notes.request.take()) {
            run(app, i, request);
        }
        autoadd(app, i);
    }
}

fn publish_palette(app: &mut PdfCraftApp) {
    let Some(ctx) = app.ctx.clone() else { return };
    if let Some(first) = app.notes.colors.first() {
        app.comment_prefs.set_color(CommentTool::Highlight, first.rgb_f64());
    }
    let palette: Vec<(String, [f64; 3])> = app.notes.colors.iter().map(|c| (c.name.clone(), c.rgb_f64())).collect();
    ctx.data_mut(|d| d.insert_temp(palette_id(), Arc::new(palette)));
    app.notes.palette_published = true;
}

/// Brings view `i`'s notes state up to date. The note context and the list of highlights are
/// worked out again only when the document, its edits or the notes settings changed; every
/// other frame reads the cache.
fn refresh_view(app: &mut PdfCraftApp, i: usize) {
    let Some(id) = app.views.get(i).map(|view| view.id) else { return };
    let Some(doc) = app.session.get(id) else { return };
    let stale = app.notes.cached.get(&doc.id).is_none_or(|cached| {
        cached.path != doc.path
            || cached.generation != doc.edit_generation()
            || cached.vault != app.notes.vault
            || cached.template != app.notes.note_template
    });
    if stale {
        let context = context_for(app, doc);
        let shown = context.note.clone().map(|relative| {
            let vault = app.notes.vault.as_deref().and_then(Path::file_name).map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            std::iter::once(vault.as_str()).chain(relative.split('/')).collect::<Vec<_>>().join(" / ")
        });
        let cached = CachedDocument {
            path: doc.path.clone(),
            generation: doc.edit_generation(),
            vault: app.notes.vault.clone(),
            template: app.notes.note_template.clone(),
            context,
            indices: Arc::new(markup_indices(&doc.info)),
            shown,
        };
        app.notes.cached.insert(id, cached);
    }
    let autoadd = doc.path.as_ref().is_some_and(|path| app.notes.autoadd.contains(path));
    let Some(cached) = app.notes.cached.get(&id) else { return };
    let Some(view) = app.views.get_mut(i) else { return };
    let notes_ui = &mut view.comments.notes;
    notes_ui.enabled = true;
    notes_ui.autoadd = autoadd;
    notes_ui.external = cached.context.link.is_external();
    if notes_ui.note != cached.shown {
        notes_ui.note = cached.shown.clone();
    }
    notes_ui.target = app.notes.config.export_target;
    notes_ui.markup_indices = cached.indices.clone();
    notes_ui.vault_prompt_hidden = app.notes.notes_box_dismissed;
    notes_ui.progress = app.notes.jobs.get(&id).and_then(|job| job.lock().ok().map(|state| (state.done, state.total)));
}

fn poll_jobs(app: &mut PdfCraftApp) {
    let completed: Vec<_> = app
        .notes
        .jobs
        .iter()
        .filter_map(|(id, job)| match job.lock() {
            Ok(mut state) => {
                let retry = state.retry_write;
                state.result.take().map(|result| (*id, retry, result))
            }
            Err(_) => Some((*id, false, Err("The note worker stopped unexpectedly. Your PDF is unchanged.".into()))),
        })
        .collect();
    for (id, retry_write, result) in completed {
        app.notes.jobs.remove(&id);
        match result {
            Ok(completed) => {
                let result = completed
                    .clipboard
                    .map_or(Ok(()), |text| copy_to_clipboard(app, text))
                    .and_then(|()| completed.uri.map_or(Ok(()), |uri| launch_uri(app, &uri)));
                match result {
                    // An automatic write that changed nothing says nothing.
                    Ok(()) => {
                        if let Some(message) = completed.message {
                            app.notify(message);
                        }
                    }
                    Err(error) => super::toast::failed(app, error, None),
                }
            }
            Err(error) => super::toast::failed(app, error, retry_write.then_some(super::toast::Action::WriteNote(id))),
        }
    }
    if !app.notes.jobs.is_empty()
        && let Some(ctx) = &app.ctx
    {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
    app.notes.cached.retain(|id, _| app.session.get(*id).is_some());
}

pub(crate) fn cached_markup<'a>(info: &'a DocInfo, indices: &'a [usize]) -> impl Iterator<Item = &'a Annotation> {
    indices.iter().filter_map(|index| info.annotations.get(*index))
}

pub fn save_snapshot_to_note(app: &mut PdfCraftApp, index: usize, page: usize, rect: [f32; 4]) -> Result<(), String> {
    refresh_view(app, index);
    worker::start(app, index, worker::Work::Snapshot { page, rect })
}

fn run(app: &mut PdfCraftApp, i: usize, request: Request) {
    let result = match request {
        // Neither says anything when done: the picker and the card going away are the answer.
        Request::ChooseVault => {
            // The web build has no folders to pick; its card's button does nothing there.
            #[cfg(not(target_arch = "wasm32"))]
            super::welcome::pick_vault(app);
            return;
        }
        Request::HideVaultPrompt => {
            dismiss_notes_box(app);
            return;
        }
        Request::CopyCallout(page, index) => copy_callout(app, i, page, index),
        Request::Open(which) => {
            let only = which
                .map(|(page, index)| {
                    app.views
                        .get(i)
                        .and_then(|view| app.session.get(view.id))
                        .and_then(|doc| {
                            doc.info
                                .annotations
                                .iter()
                                .find(|annotation| annotation.page == page && annotation.index == index && is_markup(annotation))
                        })
                        .map(entry_key)
                        .into_iter()
                        .collect()
                })
                .or_else(|| Some(Vec::new()));
            worker::start(app, i, worker::Work::Write { only, open: true, automatic: false }).map(|()| "Writing reading note…".into())
        }
        Request::CopyAll => worker::start(app, i, worker::Work::Copy).map(|()| "Preparing highlights…".into()),
        Request::Write => {
            worker::start(app, i, worker::Work::Write { only: None, open: false, automatic: false }).map(|()| "Writing reading note…".into())
        }
        Request::ExportHighlights => worker::pick_export(app, i, false).map(|()| "Choose a highlight export destination".into()),
        Request::ExportPaper(true) => {
            worker::start(app, i, worker::Work::Paper { destination: worker::PaperDestination::Vault }).map(|()| "Exporting paper…".into())
        }
        Request::ExportPaper(false) => worker::pick_export(app, i, true).map(|()| "Choose a paper export destination".into()),
        Request::SetTarget(target) => {
            let mut config = app.notes.config.clone();
            config.export_target = target;
            persist_notes(app, config).map(|()| "Highlight export target updated".into())
        }
        Request::ToggleAutoadd => toggle_autoadd(app, i),
    };
    match result {
        Ok(message) => app.notify(message),
        // A failure stays until dismissed; a failed write offers to write again.
        Err(message) => {
            let retry = app.views.get(i).filter(|_| request == Request::Write).map(|v| super::toast::Action::WriteNote(v.id));
            super::toast::failed(app, message, retry);
        }
    }
}

fn copy_to_clipboard(app: &PdfCraftApp, text: String) -> Result<(), String> {
    let ctx = app.ctx.as_ref().ok_or("There is no window to copy from.")?;
    ctx.copy_text(text);
    Ok(())
}

/// The callout of the comment at (page, index) of view `i`.
pub fn callout_for(app: &PdfCraftApp, i: usize, page: usize, index: usize) -> Result<String, String> {
    let view = app.views.get(i).ok_or("No document is open.")?;
    let doc = app.session.get(view.id).ok_or("No document is open.")?;
    let a = doc.info.annotations.iter().find(|a| a.page == page && a.index == index && is_markup(a)).ok_or("That comment doesn't mark text.")?;
    let d = context_for(app, doc);
    let h = HighlightReader::new(&app.notes, doc, &view.texts).highlight(a);
    Ok(callout(&h, &d.link, &d.title))
}

/// The callout of a fresh text selection on `page` of view `i`, without writing a comment:
/// `text` quoted in `color` (`None`: the first of `notes.colors`).
pub fn selection_callout(app: &PdfCraftApp, i: usize, page: usize, text: &str, color: Option<[f32; 3]>) -> Result<String, String> {
    let view = app.views.get(i).ok_or("No document is open.")?;
    let doc = app.session.get(view.id).ok_or("No document is open.")?;
    let d = context_for(app, doc);
    let label = doc.info.pages.get(page).map_or_else(|| page.saturating_add(1).to_string(), |p| p.label.clone());
    let h = Highlight {
        key: String::new(),
        page,
        label,
        color: app.notes.color_label(color),
        text: text.to_string(),
        note: String::new(),
        date: String::new(),
    };
    Ok(callout(&h, &d.link, &d.title))
}

fn copy_callout(app: &mut PdfCraftApp, i: usize, page: usize, index: usize) -> Result<String, String> {
    let text = callout_for(app, i, page, index)?;
    copy_to_clipboard(app, text)?;
    Ok("Copied callout".to_string())
}

/// Hand an `obsidian://` address to the system's handler for its scheme (Obsidian registers it).
/// `PdfCraftApp::open_url` won't do: it opens the default web browser with the address, which
/// then asks whether to open Obsidian. The address is Margin's own, built by [`obsidian_uri`].
fn launch_uri(app: &mut PdfCraftApp, uri: &str) -> Result<(), String> {
    app.last_opened_url = Some(uri.to_string());
    if !app.notes.launch_obsidian {
        return Ok(());
    }
    #[cfg(target_arch = "wasm32")]
    {
        app.open_url(uri);
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut command = if cfg!(target_os = "macos") {
            std::process::Command::new("open")
        } else if cfg!(windows) {
            let mut c = std::process::Command::new("rundll32");
            c.arg("url.dll,FileProtocolHandler");
            c
        } else {
            std::process::Command::new("xdg-open")
        };
        let mut child = command
            .arg(uri)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Couldn't ask the system to open Obsidian: {e}."))?;
        // Reap the opener when it exits so it doesn't linger as a zombie.
        std::thread::spawn(move || {
            if let Err(e) = child.wait() {
                log::warn!("the obsidian:// opener: {e}");
            }
        });
        Ok(())
    }
}

fn toggle_autoadd(app: &mut PdfCraftApp, i: usize) -> Result<String, String> {
    let view = app.views.get(i).ok_or("No document is open.")?;
    let doc = app.session.get(view.id).ok_or("No document is open.")?;
    let path = doc.path.clone().ok_or("Save the PDF first: autoadd remembers documents by their file.")?;
    let rel = context_for(app, doc).note?;
    let id = view.id;
    if app.notes.autoadd.remove(&path) {
        app.notes.watched.remove(&id);
        return Ok("Highlights stay in the PDF only. Copy callouts by hand.".to_string());
    }
    app.notes.autoadd.insert(path);
    Ok(format!("New highlights go to {rel}"))
}

/// `notes.vault`, `notes.note_template`, `notes.colors` (`Name=#rrggbb,…`) and `notes.autoadd`
/// (`on`/`off`, for the active document).
pub fn set_option(app: &mut PdfCraftApp, key: &str, value: &str) -> Result<(), String> {
    let value = value.trim();
    // CLI options arrive before the first frame; load existing templates instead of replacing
    // them with the runtime defaults when changing only the vault or palette.
    let mut config = app.margin_keys.settings_for_open().notes;
    match key {
        "notes.vault" if value.is_empty() => config.vault = None,
        "notes.vault" => {
            let vault = PathBuf::from(value);
            if !vault.is_absolute() || !vault.is_dir() {
                return Err(format!("notes.vault must be an existing folder given by its full path, not {value}"));
            }
            config.vault = Some(vault);
        }
        "notes.note_template" => {
            note_path(value, "title")?;
            config.note_template = value.to_string();
        }
        "notes.colors" => {
            let colors = value
                .split(',')
                .map(|pair| {
                    let (name, color) = pair.split_once('=')?;
                    Some(NoteColor { name: name.trim().to_string(), rgb: parse_hex(color)? }).filter(|c| !c.name.is_empty())
                })
                .collect::<Option<Vec<_>>>()
                .filter(|c| !c.is_empty())
                .ok_or("notes.colors: Name=#rrggbb pairs separated by commas")?;
            config.colors = colors.into_iter().map(|color| margin_config::settings::NoteColor { name: color.name, rgb: color.rgb }).collect();
        }
        "notes.autoadd" => {
            let on = match value {
                "on" => true,
                "off" => false,
                _ => return Err("notes.autoadd must be on or off".into()),
            };
            let (i, _) = app.active_ids().ok_or("`notes.autoadd` needs an open document")?;
            let is_on =
                app.views.get(i).and_then(|v| app.session.get(v.id)).and_then(|d| d.path.clone()).is_some_and(|p| app.notes.autoadd.contains(&p));
            if on != is_on {
                toggle_autoadd(app, i)?;
            }
            return Ok(());
        }
        other => return Err(format!("unknown option {other}")),
    }
    persist_notes(app, config)
}

/// Every annotation edit schedules a full synchronization, including recolours and deletions.
/// Wait until comment typing finishes; keep the pending generation if another job is running.
fn autoadd(app: &mut PdfCraftApp, i: usize) {
    let Some(view) = app.views.get(i) else { return };
    let id = view.id;
    let Some(doc) = app.session.get(id) else { return };
    if !doc.path.as_ref().is_some_and(|path| app.notes.autoadd.contains(path)) {
        app.notes.watched.remove(&id);
        return;
    }
    let generation = (doc.edit_generation(), super::marks::generation(view));
    let watch = app.notes.watched.entry(id).or_insert(AutoaddWatch { generation, dirty: false });
    if watch.generation != generation {
        watch.generation = generation;
        watch.dirty = true;
    }
    if !watch.dirty || view.comments.editing.is_some() || !app.notes.jobs.is_empty() {
        return;
    }
    watch.dirty = false;
    if let Err(error) = worker::start(app, i, worker::Work::Write { only: None, open: false, automatic: true }) {
        super::toast::failed(app, error, Some(super::toast::Action::WriteNote(id)));
    }
}

fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let digits = text.trim().strip_prefix('#')?;
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let channel = |at: usize| digits.get(at..at + 2).and_then(|d| u8::from_str_radix(d, 16).ok());
    Some([channel(0)?, channel(2)?, channel(4)?])
}

/// The colours of an older app state's notes block: `[["Name", "#rrggbb"], …]`.
fn legacy_colors(v: &serde_json::Value) -> Vec<NoteColor> {
    v["colors"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|pair| {
                    let name = pair.get(0)?.as_str()?.trim();
                    let rgb = parse_hex(pair.get(1)?.as_str()?)?;
                    (!name.is_empty()).then(|| NoteColor { name: name.to_string(), rgb })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Settings were read (first frame, a reload, or Margin's own write): use the `notes` block,
/// then move an older app state's notes into `settings.jsonc`.
pub(crate) fn settings_loaded(app: &mut PdfCraftApp) {
    let config = app.margin_keys.settings().notes.clone();
    app.notes.apply_config(&config);
    if let Err(error) = migrate_legacy(app) {
        // The app state keeps the old block, so the next start tries again.
        log::warn!("couldn't move the notes settings into settings.jsonc: {error}");
    }
}

/// Copies what an older app state's notes block set (vault, note path, colours) into
/// `settings.jsonc`, leaving every key the user already wrote alone, then forgets the block.
fn migrate_legacy(app: &mut PdfCraftApp) -> Result<(), String> {
    let Some(source) = app.notes.legacy.clone() else { return Ok(()) };
    let mut legacy = Notes::default();
    legacy.restore(&source);
    let mut values = Vec::new();
    if let Some(vault) = &legacy.vault {
        values.push(("notes.vault", serde_json::Value::String(vault.to_string_lossy().into_owned())));
    }
    if legacy.note_template != DEFAULT_NOTE_TEMPLATE {
        values.push(("notes.note_template", serde_json::Value::String(legacy.note_template.clone())));
    }
    let colors = legacy_colors(&source);
    if !colors.is_empty() {
        let colors = colors.iter().map(|color| serde_json::json!({ "name": color.name, "rgb": color.rgb })).collect();
        values.push(("notes.colors", serde_json::Value::Array(colors)));
    }
    if !values.is_empty() {
        let dir = settings_folder(app)?;
        margin_config::set_user_values_if_missing(&dir, &values)?;
    }
    app.notes.legacy = None;
    if !values.is_empty() {
        super::keys::reload_now(app);
    }
    Ok(())
}

/// The settings folder, with `settings.jsonc` created from the commented first-run file when it
/// doesn't exist yet: a value written before the first frame (a command-line option, the
/// migration) must not leave a file holding only that value.
fn settings_folder(app: &PdfCraftApp) -> Result<PathBuf, String> {
    let dir = app.margin_keys.settings_dir().ok_or("This system has no settings folder.")?;
    let path = dir.join(margin_config::SETTINGS_FILE);
    if !path.exists() {
        std::fs::create_dir_all(&dir).map_err(|error| format!("Couldn't create {} ({error}).", dir.display()))?;
        margin_config::store::write_atomically(&path, margin_config::schema::first_run_settings_jsonc().as_bytes())
            .map_err(|error| format!("Couldn't write {} ({error}).", path.display()))?;
    }
    Ok(dir)
}

/// Writes dotted `settings.jsonc` keys, keeping the file's comments, and applies them at once.
pub(crate) fn write_user_settings(app: &mut PdfCraftApp, values: &[(&str, serde_json::Value)]) -> Result<(), String> {
    let dir = settings_folder(app)?;
    margin_config::set_user_values(&dir, values)?;
    // Before the first frame nothing is loaded yet, and reload_now does nothing: apply directly.
    super::keys::reload_now(app);
    let config = app.margin_keys.settings_for_open().notes;
    app.notes.apply_config(&config);
    Ok(())
}

/// Saves the notes settings that differ from the ones in use into `settings.jsonc`.
pub(crate) fn persist_notes(app: &mut PdfCraftApp, mut config: margin_config::settings::Notes) -> Result<(), String> {
    if config.vault.as_ref().is_some_and(|vault| vault.as_os_str().is_empty()) {
        config.vault = None;
    } else if config.vault.as_ref().is_some_and(|vault| !vault.is_absolute()) {
        return Err("notes.vault must be an absolute folder path".to_owned());
    }
    let wanted = serde_json::to_value(&config).map_err(|error| format!("Couldn't save the notes settings ({error})."))?;
    let current = serde_json::to_value(&app.notes.config).map_err(|error| format!("Couldn't save the notes settings ({error})."))?;
    let (Some(wanted), Some(current)) = (wanted.as_object(), current.as_object()) else {
        return Err("The notes settings aren't a JSON object.".to_owned());
    };
    let changed: Vec<(String, serde_json::Value)> =
        wanted.iter().filter(|(key, value)| current.get(*key) != Some(*value)).map(|(key, value)| (format!("notes.{key}"), value.clone())).collect();
    if changed.is_empty() {
        return Ok(());
    }
    let values: Vec<(&str, serde_json::Value)> = changed.iter().map(|(key, value)| (key.as_str(), value.clone())).collect();
    write_user_settings(app, &values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn highlight(note: &str) -> Highlight {
        Highlight {
            key: "a1b2".into(),
            page: 2,
            label: "3".into(),
            color: "yellow".into(),
            text: "the  marked\ntext".into(),
            note: note.into(),
            date: String::new(),
        }
    }

    #[test]
    fn callout_is_pdf_plus_quote_in_callout() {
        let link = PdfLink::Vault("papers/attention.pdf".into());
        assert_eq!(
            callout(&highlight(""), &link, "attention"),
            "> [!PDF|yellow] [[papers/attention.pdf#page=3|attention, p.3]]\n> > the marked text\n"
        );
        assert_eq!(
            callout(&highlight("why it matters\n\nsecond line"), &link, "attention"),
            "> [!PDF|yellow] [[papers/attention.pdf#page=3|attention, p.3]]\n> > the marked text\n>\n> why it matters\n>\n> second line\n"
        );
    }

    #[test]
    fn callout_outside_the_vault_links_the_file() {
        let link = PdfLink::External(PathBuf::from("/home/me/My Papers/a b.pdf"));
        assert_eq!(
            callout(&highlight(""), &link, "a b"),
            "> [!PDF|yellow] [a b, p.3](file:///home/me/My%20Papers/a%20b.pdf#page=3)\n> > the marked text\n"
        );
    }

    #[test]
    fn wikilink_breaking_names_use_a_markdown_link() {
        let link = PdfLink::Vault("x#1|y.pdf".into());
        assert_eq!(link.markdown(0, "x, p.1"), "[x, p.1](x%231%7Cy.pdf#page=1)");
        assert_eq!(display_text("a [draft] | b", "iv"), "a (draft) - b, p.iv");
    }

    #[test]
    fn colors_are_named_like_pdf_plus() {
        let notes = Notes::default();
        assert_eq!(notes.color_label(Some([1.0, 0.8157, 0.0])), "yellow");
        assert_eq!(notes.color_label(Some([0x08 as f32 / 255.0, 0x6d as f32 / 255.0, 0xdd as f32 / 255.0])), "note");
        assert_eq!(notes.color_label(Some([0.0, 1.0, 0.0])), "0,255,0");
        assert_eq!(notes.color_label(None), "yellow");
    }

    #[test]
    fn note_paths_stay_in_the_vault() {
        assert_eq!(note_path(DEFAULT_NOTE_TEMPLATE, "Attention Is All You Need!").unwrap(), "reading/attention-is-all-you-need.md");
        assert_eq!(note_path("{{title-slug}}.md", "").unwrap(), "untitled.md");
        for bad in ["../x.md", "reading/../../x.md", "/etc/x.md", "C:/x.md", "a\\b.md", "a//b.md", "./x.md", "reading/x.txt", ""] {
            assert!(note_path(bad, "t").is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn markers_cannot_close_early() {
        assert_eq!(marker("a-->b\n"), "<!-- margin:a--_b_ -->");
    }

    #[test]
    fn obsidian_uri_encodes_vault_and_file() {
        assert_eq!(
            obsidian_uri(Path::new("/home/me/Mimir's Vault"), "reading/a b.md").unwrap(),
            "obsidian://open?vault=Mimir%27s%20Vault&file=reading%2Fa%20b.md"
        );
    }

    #[test]
    fn notes_outside_the_vault_are_refused() {
        let root = std::env::temp_dir().join(format!("margin-notes-{}", std::process::id()));
        let vault = root.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        assert!(resolve_note(&vault, "../escape.md").is_err());
        let ok = resolve_note(&vault, "reading/x.md").unwrap();
        assert!(ok.starts_with(vault.canonicalize().unwrap()) && ok.parent().unwrap().is_dir());
        #[cfg(unix)]
        {
            std::fs::create_dir_all(root.join("outside")).unwrap();
            std::os::unix::fs::symlink(root.join("outside"), vault.join("linked")).unwrap();
            assert!(resolve_note(&vault, "linked/sub/x.md").is_err(), "a linked folder can't lead out");
            assert!(!root.join("outside/sub").exists(), "nothing is created outside");
        }
        write_atomically(&ok, "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&ok).unwrap(), "hello");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn legacy_runtime_restore_validates_and_yields_to_settings() {
        let mut restored = Notes::default();
        restored.restore(&serde_json::json!({ "vault": "/v", "note_template": "legacy.md", "colors": [["Blue", "#0080ff"]] }));
        assert_eq!(restored.vault.as_deref(), Some(Path::new("/v")));
        assert_eq!(restored.colors.first().map(|color| color.name.as_str()), Some("Blue"));
        restored.apply_config(&margin_config::settings::Notes::default());
        assert_eq!(restored.vault, None);
        assert_eq!(restored.colors, default_colors());
        let mut bad = Notes::default();
        bad.restore(&serde_json::json!({ "vault": "relative", "note_template": "../x.md", "colors": [["", "#zzzzzz"]] }));
        assert_eq!((bad.vault, bad.note_template, bad.colors), (None, DEFAULT_NOTE_TEMPLATE.to_string(), default_colors()));
    }

    #[test]
    fn settings_with_a_relative_vault_or_nameless_colours_fall_back_and_say_why() {
        let mut notes = Notes::default();
        let config = margin_config::settings::Notes {
            vault: Some(PathBuf::from("relative/vault")),
            colors: vec![margin_config::settings::NoteColor { name: " ".into(), rgb: [1, 2, 3] }],
            ..Default::default()
        };
        notes.apply_config(&config);
        assert_eq!(notes.vault, None, "a relative vault is never used");
        assert_eq!(notes.colors, default_colors(), "no named colour left: the PDF++ palette");
        let why = note_for(&notes, &BTreeMap::new()).unwrap_err();
        assert!(why.contains("full folder path") && why.contains("relative/vault"), "{why}");
    }

    fn margin_app(dir: &Path) -> PdfCraftApp {
        let _ = std::fs::remove_dir_all(dir);
        let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
        app.margin_keys.set_config_dir(dir.to_path_buf());
        app
    }

    #[test]
    fn a_relative_vault_is_refused_before_anything_is_written() {
        let dir = std::env::temp_dir().join(format!("margin-notes-relative-vault-{}", std::process::id()));
        let mut app = margin_app(&dir);
        let config = margin_config::settings::Notes { vault: Some("relative".into()), ..Default::default() };
        assert_eq!(persist_notes(&mut app, config).unwrap_err(), "notes.vault must be an absolute folder path");
        assert!(!dir.exists(), "nothing is created for a refused value");
    }

    #[test]
    fn notes_settings_land_in_the_commented_first_run_file_and_apply_at_once() {
        let dir = std::env::temp_dir().join(format!("margin-notes-persist-{}", std::process::id()));
        let mut app = margin_app(&dir);
        let config = margin_config::settings::Notes { export_target: ExportTarget::Json, ..app.notes.config.clone() };
        persist_notes(&mut app, config).unwrap();
        let text = std::fs::read_to_string(dir.join(margin_config::SETTINGS_FILE)).unwrap();
        assert!(text.contains("//"), "the commented first-run file was kept: {text}");
        assert_eq!(margin_config::Settings::load(&dir).settings.notes.export_target, ExportTarget::Json);
        assert_eq!(app.notes.config.export_target, ExportTarget::Json, "applied without waiting for a frame");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn legacy_app_state_notes_move_into_settings_once_without_replacing_explicit_values() {
        let dir = std::env::temp_dir().join(format!("margin-notes-migrate-{}", std::process::id()));
        let mut app = margin_app(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(margin_config::SETTINGS_FILE), r#"{ "notes": { "note_template": "mine/{{title-slug}}.md" } }"#).unwrap();
        let vault = std::env::temp_dir().canonicalize().unwrap();
        let legacy = serde_json::json!({ "notes": { "vault": vault, "note_template": "old/{{title-slug}}.md", "colors": [["Blue", "#0080ff"]] }, "margin_setup_dismissed": true });
        app.notes.restore_state(&legacy);
        settings_loaded(&mut app);
        let settings = margin_config::Settings::load(&dir).settings.notes;
        assert_eq!(settings.vault.as_deref(), Some(vault.as_path()));
        assert_eq!(settings.note_template, "mine/{{title-slug}}.md", "the user's own value wins");
        assert_eq!(settings.colors.first().map(|color| color.name.as_str()), Some("Blue"));
        let mut state = serde_json::json!({});
        app.notes.save_state(&mut state);
        assert_eq!(state, serde_json::json!({ "margin_setup_dismissed": true }), "the old block is gone once migrated");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pdf_dates_become_calendar_dates() {
        assert_eq!(pdf_date(Some("D:20261011093000+02'00'")), "2026-10-11");
        assert_eq!(pdf_date(Some("20240229")), "2024-02-29");
        for bad in [None, Some(""), Some("D:2026"), Some("D:20261311"), Some("D:2026-10-11"), Some("D:2026é011")] {
            assert_eq!(pdf_date(bad), "", "{bad:?}");
        }
    }

    #[test]
    fn marked_text_reads_the_glyphs_under_the_quads() {
        let glyph = |text: &str, x: f32| pdfcraft_render::TextGlyph {
            text: text.into(),
            rect: [x, 10.0, x + 8.0, 20.0],
            quad: [[x, 10.0], [x + 8.0, 10.0], [x + 8.0, 20.0], [x, 20.0]],
            direction: [1.0, 0.0],
        };
        let text = PageText {
            glyphs: vec![glyph("a", 0.0), glyph("b", 8.0), glyph("c", 24.0), glyph("d", 32.0), glyph("e", 48.0)],
            line_of: vec![0; 5],
            space_before: vec![false, false, true, false, true],
        };
        let page = PageInfo { width: 100.0, height: 100.0, label: "1".into(), crop: [0.0, 0.0, 100.0, 100.0], rotation: 0 };
        let a = Annotation {
            page: 0,
            subtype: "Highlight".into(),
            author: None,
            contents: None,
            modified: None,
            name: None,
            in_reply_to: None,
            // User space, y up: view y 10–20 is user y 80–90; x 6–42 covers b, c and d.
            rect: [6.0, 80.0, 42.0, 90.0],
            color: None,
            index: 0,
            state: None,
            quads: vec![[6.0, 90.0, 42.0, 90.0, 6.0, 80.0, 42.0, 80.0]],
            locked: false,
            intent: None,
            fill_sign: false,
        };
        assert_eq!(marked_text(&text, &page, &a), "b cd");
    }
}
