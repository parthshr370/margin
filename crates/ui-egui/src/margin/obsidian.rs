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
//! The reading note is only ever appended to: entries go under `## Highlights`, each behind a
//! `<!-- margin:<key> -->` marker (the annotation's `/NM`), so writing twice adds nothing. Notes
//! are written atomically (temporary file, then rename) and never outside the vault.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
}

impl Default for Notes {
    fn default() -> Self {
        Self {
            vault: None,
            note_template: DEFAULT_NOTE_TEMPLATE.to_string(),
            colors: default_colors(),
            autoadd: BTreeSet::new(),
            launch_obsidian: true,
            palette_published: false,
            watched: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct AutoaddWatch {
    /// The document's edit generation when `keys` was taken.
    generation: u64,
    /// Entry keys of the text-markup comments seen so far.
    keys: BTreeSet<String>,
    /// New highlights not yet written (written once their note is no longer being typed).
    pending: Vec<String>,
}

impl Notes {
    /// The settings as stored in the app's persisted JSON (key `notes`). Autoadd is per document,
    /// in its sidecar.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "vault": self.vault.as_ref().map(|v| v.to_string_lossy().into_owned()),
            "note_template": self.note_template,
            "colors": self.colors.iter().map(|c| [c.name.clone(), hex(c.rgb)]).collect::<Vec<_>>(),
        })
    }

    /// Restore from [`Self::to_json`]. Settings are untrusted: anything invalid keeps its default.
    pub fn restore(&mut self, v: &serde_json::Value) {
        if let Some(vault) = v["vault"].as_str().map(PathBuf::from).filter(|p| p.is_absolute()) {
            self.vault = Some(vault);
        }
        if let Some(template) = v["note_template"].as_str().filter(|t| note_path(t, "title").is_ok()) {
            self.note_template = template.to_string();
        }
        let colors: Vec<NoteColor> = v["colors"]
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
            .unwrap_or_default();
        if !colors.is_empty() {
            self.colors = colors;
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

/// Append `entries` (key, callout) under [`HIGHLIGHTS_HEADING`] of `existing` (`None`: a new note,
/// started with `header`). Entries whose marker is already there are skipped. Returns the new
/// text and how many entries were added; nothing else in the note changes.
pub fn append_entries(existing: Option<&str>, header: &str, entries: &[(String, String)]) -> (String, usize) {
    let mut text = existing.map(str::to_string).unwrap_or_else(|| format!("{header}{HIGHLIGHTS_HEADING}\n"));
    let mut seen = BTreeSet::new();
    let fresh: Vec<String> = entries
        .iter()
        .filter(|(key, _)| {
            let m = marker(key);
            !text.contains(&m) && seen.insert(m)
        })
        .map(|(key, callout)| format!("{}\n{callout}", marker(key)))
        .collect();
    if fresh.is_empty() {
        return (text, 0);
    }
    let heading_end = find_heading(&text).unwrap_or_else(|| {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(HIGHLIGHTS_HEADING);
        text.push('\n');
        text.len()
    });
    // The section runs to the next heading of its level or higher.
    let section_end = next_section(&text, heading_end).unwrap_or(text.len());
    let (before, after) = text.split_at(section_end);
    let mut out = before.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.ends_with("\n\n") {
        out.push('\n');
    }
    out.push_str(&fresh.join("\n"));
    if !after.is_empty() {
        out.push('\n');
        out.push_str(after);
    }
    (out, fresh.len())
}

/// Byte offset just past the `## Highlights` line.
fn find_heading(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end() == HIGHLIGHTS_HEADING {
            return Some(offset);
        }
    }
    None
}

/// Byte offset of the first `#` or `##` heading at or after `from`.
fn next_section(text: &str, from: usize) -> Option<usize> {
    let mut offset = from;
    for line in text.get(from..)?.split_inclusive('\n') {
        if line.starts_with("# ") || line.starts_with("## ") {
            return Some(offset);
        }
        offset += line.len();
    }
    None
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

/// The text-markup comments of a document in reading order (page, then top to bottom).
pub(crate) fn markup_comments(info: &DocInfo) -> Vec<&Annotation> {
    let mut list: Vec<&Annotation> = info.annotations.iter().filter(|a| is_markup(a)).collect();
    list.sort_by(|a, b| a.page.cmp(&b.page).then(b.rect[3].total_cmp(&a.rect[3])).then(a.rect[0].total_cmp(&b.rect[0])));
    list
}

/// Write `contents` to `path` through a temporary file in the same folder and a rename, so the
/// note is never seen half written.
pub fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "the note has no folder"))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = dir.join(format!(".{name}.margin-{}.tmp", std::process::id()));
    let written = std::fs::File::create(&temp).and_then(|mut f| {
        f.write_all(contents.as_bytes())?;
        f.sync_all()
    });
    let result = written.and_then(|()| std::fs::rename(&temp, path));
    if result.is_err()
        && let Err(e) = std::fs::remove_file(&temp)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("couldn't remove the temporary note {}: {e}", temp.display());
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

/// What the notes features need to know about one open document.
struct DocNotes {
    title: String,
    link: PdfLink,
    /// The reading note's vault-relative path, or why there is none.
    note: Result<String, String>,
}

impl DocNotes {
    fn of(notes: &Notes, doc: &Document) -> Self {
        let file = doc.path.as_deref().map(Path::new);
        let name = file.and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| doc.name.clone());
        let title = Path::new(&name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| name.clone());
        let link = PdfLink::for_pdf(notes.vault.as_deref(), file, &name);
        let note = match &notes.vault {
            Some(_) => note_path(&notes.note_template, &title),
            None => Err("Set notes.vault to your Obsidian vault folder first.".to_string()),
        };
        Self { title, link, note }
    }

    /// The front matter of a new reading note.
    fn header(&self) -> String {
        let source = match &self.link {
            PdfLink::Vault(rel) => format!("[[{rel}]]"),
            PdfLink::External(abs) => file_url(abs),
        };
        format!("---\nsource: \"{}\"\n---\n\n", source.replace('"', "'"))
    }
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
        let info = &self.doc.info;
        let page_info = info.pages.get(a.page);
        let text = match (self.page_text(a.page), page_info) {
            (Some(t), Some(p)) => marked_text(&t, p, a),
            _ => String::new(),
        };
        Highlight {
            key: entry_key(a),
            page: a.page,
            label: page_info.map(|p| p.label.clone()).unwrap_or_else(|| a.page.saturating_add(1).to_string()),
            color: self.notes.color_label(a.color),
            text,
            note: a.contents.clone().unwrap_or_default(),
        }
    }
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
    /// Append every highlight to the reading note.
    Write,
    /// Turn "Add highlights to this note" on or off for the document.
    ToggleAutoadd,
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
}

impl NotesUi {
    pub(crate) fn ask(&mut self, ctx: &egui::Context, request: Request) {
        self.request = Some(request);
        ctx.request_repaint();
    }
}

impl Default for NotesUi {
    fn default() -> Self {
        Self { enabled: false, autoadd: false, note: Err(String::new()), external: false, request: None }
    }
}

pub(crate) const EXTERNAL_TIP: &str =
    "This PDF isn't in the vault, so the link opens its file at #page=N (file:// form). Move the PDF into the vault for PDF++ links.";

/// "Copy callout" and "Open in Obsidian" in a text-markup comment's menus.
pub(crate) fn menu_items(ui: &mut egui::Ui, notes: &mut NotesUi, a: &Annotation) {
    if !notes.enabled || !is_markup(a) {
        return;
    }
    let copy = ui.button("Copy callout");
    if look::tooltip(copy, if notes.external { EXTERNAL_TIP } else { "Copy as a PDF++ callout" }, None).clicked() {
        notes.ask(ui.ctx(), Request::CopyCallout(a.page, a.index));
        ui.close();
    }
    let open = ui.button("Open in Obsidian");
    if look::tooltip(open, "Add this highlight to the reading note and open it in Obsidian", None).clicked() {
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
    if icons::button(ui, "external-link", CARD_BUTTON, false, "Open this callout in Obsidian").clicked() {
        notes.ask(ui.ctx(), Request::Open(Some((a.page, a.index))));
    }
    let tip = if notes.external { EXTERNAL_TIP } else { "Copy callout" };
    if icons::button(ui, "copy", CARD_BUTTON, false, tip).clicked() {
        notes.ask(ui.ctx(), Request::CopyCallout(a.page, a.index));
    }
}

/// Side of a card's icon buttons (the comment card's own buttons are this size).
const CARD_BUTTON: f32 = 22.0;

/// The reading-note box at the top of the highlights list (the mockup's `.note-link`), on the
/// spacing scale: padded by the large step, rows the small step apart. The note's path with an
/// "Open note" button, the "Add highlights to this note" switch, then "Copy all" and "Write to
/// note" sharing one row equally. Without a reading note (no vault) the path row says why and
/// the switch and "Write to note" are off.
pub(crate) fn panel_block(ui: &mut egui::Ui, t: &Tokens, notes: &mut NotesUi) {
    if !notes.enabled {
        return;
    }
    look::card_frame(t).inner_margin(look::GAP_LARGE).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = egui::vec2(look::GAP, look::GAP);
        note_row(ui, t, notes);
        autoadd_row(ui, t, notes);
        action_row(ui, notes);
    });
    ui.add_space(look::GAP);
}

/// Icon and button sizes and text sizes of the notes box.
const NOTE_ICON: f32 = 14.0;
const NOTE_BUTTON: f32 = 24.0;
const NOTE_PATH_TEXT: f32 = 12.0;
const NOTE_TITLE_TEXT: f32 = 12.5;
const NOTE_SUB_TEXT: f32 = 11.5;
/// Space between the switch's label and the line under it.
const NOTE_SUB_GAP: f32 = 2.0;
const AUTOADD_TITLE: &str = "Add highlights to this note";
const AUTOADD_SUB: &str = "Each new highlight becomes a PDF++ callout";

/// The reading note's path, truncated, and "Open note"; or why there is no note, wrapped.
fn note_row(ui: &mut egui::Ui, t: &Tokens, notes: &mut NotesUi) {
    let width = ui.available_width();
    let path = match &notes.note {
        Ok(path) => path.clone(),
        Err(why) => {
            ui.horizontal_top(|ui| {
                ui.add(icons::image("file-text", NOTE_ICON, t.icon));
                let text = egui::RichText::new(why).font(theme::regular(NOTE_PATH_TEXT)).color(t.text_faint);
                ui.add(egui::Label::new(text).wrap().selectable(false));
            });
            return;
        }
    };
    ui.allocate_ui_with_layout(egui::vec2(width, NOTE_BUTTON), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.add(icons::image("file-text", NOTE_ICON, t.icon));
        let label_width = (ui.available_width() - NOTE_BUTTON - look::GAP).max(0.0);
        ui.allocate_ui_with_layout(egui::vec2(label_width, NOTE_BUTTON), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_width(label_width);
            let text = egui::RichText::new(&path).font(theme::regular(NOTE_PATH_TEXT)).color(t.text_muted);
            ui.add(egui::Label::new(text).truncate().selectable(false));
        });
        if look::icon_button(ui, "external-link", NOTE_BUTTON, false, "Open note in Obsidian", None).clicked() {
            notes.ask(ui.ctx(), Request::Open(None));
        }
    });
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
        let copy_tip = if notes.external { EXTERNAL_TIP } else { "Copy every highlight as PDF++ callouts" };
        if look::tooltip(copy, copy_tip, None).clicked() {
            notes.ask(ui.ctx(), Request::CopyAll);
        }
        let write = ui
            .add_enabled_ui(notes.note.is_ok(), |ui| {
                look::Button::secondary(look::ButtonSize::Compact, "Write to note").icon("file-pen-line").fixed_width(width).show(ui)
            })
            .inner;
        let write_tip = match &notes.note {
            Ok(_) => "Append highlights the note doesn't have yet".to_owned(),
            Err(why) => format!("Write to note: {why}"),
        };
        if look::tooltip(write, &write_tip, None).clicked() {
            notes.ask(ui.ctx(), Request::Write);
        }
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

fn refresh_view(app: &mut PdfCraftApp, i: usize) {
    let Some(view) = app.views.get_mut(i) else { return };
    let Some(doc) = app.session.get(view.id) else { return };
    let d = DocNotes::of(&app.notes, doc);
    let shown = d.note.map(|rel| {
        let vault = app.notes.vault.as_deref().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        std::iter::once(vault.as_str()).chain(rel.split('/')).collect::<Vec<_>>().join(" / ")
    });
    let ui = &mut view.comments.notes;
    ui.enabled = true;
    ui.autoadd = doc.path.as_ref().is_some_and(|p| app.notes.autoadd.contains(p));
    ui.external = d.link.is_external();
    ui.note = shown;
}

fn run(app: &mut PdfCraftApp, i: usize, request: Request) {
    let result = match request {
        Request::CopyCallout(page, index) => copy_callout(app, i, page, index),
        Request::Open(which) => open_in_obsidian(app, i, which),
        Request::CopyAll => copy_all(app, i),
        Request::Write => write_note(app, i, None).map(|(rel, added)| match added {
            0 => format!("{rel} already has every highlight"),
            1 => format!("Wrote 1 callout to {rel}"),
            n => format!("Wrote {n} callouts to {rel}"),
        }),
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
    let d = DocNotes::of(&app.notes, doc);
    let h = HighlightReader::new(&app.notes, doc, &view.texts).highlight(a);
    Ok(callout(&h, &d.link, &d.title))
}

/// The callout of a fresh text selection on `page` of view `i`, without writing a comment:
/// `text` quoted in `color` (`None`: the first of `notes.colors`).
pub fn selection_callout(app: &PdfCraftApp, i: usize, page: usize, text: &str, color: Option<[f32; 3]>) -> Result<String, String> {
    let view = app.views.get(i).ok_or("No document is open.")?;
    let doc = app.session.get(view.id).ok_or("No document is open.")?;
    let d = DocNotes::of(&app.notes, doc);
    let label = doc.info.pages.get(page).map_or_else(|| page.saturating_add(1).to_string(), |p| p.label.clone());
    let h = Highlight { key: String::new(), page, label, color: app.notes.color_label(color), text: text.to_string(), note: String::new() };
    Ok(callout(&h, &d.link, &d.title))
}

fn copy_callout(app: &mut PdfCraftApp, i: usize, page: usize, index: usize) -> Result<String, String> {
    let text = callout_for(app, i, page, index)?;
    copy_to_clipboard(app, text)?;
    Ok("Copied callout".to_string())
}

/// Every highlight of view `i` as callouts, one blank line apart, and how many there are.
pub fn markdown_all(app: &PdfCraftApp, i: usize) -> Result<(String, usize), String> {
    let view = app.views.get(i).ok_or("No document is open.")?;
    let doc = app.session.get(view.id).ok_or("No document is open.")?;
    let d = DocNotes::of(&app.notes, doc);
    let mut reader = HighlightReader::new(&app.notes, doc, &view.texts);
    let list: Vec<String> = markup_comments(&doc.info).into_iter().map(|a| callout(&reader.highlight(a), &d.link, &d.title)).collect();
    Ok((list.join("\n"), list.len()))
}

fn copy_all(app: &mut PdfCraftApp, i: usize) -> Result<String, String> {
    let (text, n) = markdown_all(app, i)?;
    if n == 0 {
        return Err("This document has no highlights yet.".to_string());
    }
    copy_to_clipboard(app, text)?;
    Ok(if n == 1 { "Copied 1 callout".to_string() } else { format!("Copied {n} callouts") })
}

/// Append the highlights of view `i` to its reading note (only the ones whose keys are in
/// `only`, when given). Returns the note's vault-relative path and how many entries were added.
pub fn write_note(app: &PdfCraftApp, i: usize, only: Option<&[String]>) -> Result<(String, usize), String> {
    let view = app.views.get(i).ok_or("No document is open.")?;
    let doc = app.session.get(view.id).ok_or("No document is open.")?;
    let d = DocNotes::of(&app.notes, doc);
    let rel = d.note.clone()?;
    let vault = app.notes.vault.as_deref().ok_or("Set notes.vault to your Obsidian vault folder first.")?;
    let path = resolve_note(vault, &rel)?;
    let mut reader = HighlightReader::new(&app.notes, doc, &view.texts);
    let entries: Vec<(String, String)> = markup_comments(&doc.info)
        .into_iter()
        .filter(|a| only.is_none_or(|keys| keys.contains(&entry_key(a))))
        .map(|a| {
            let h = reader.highlight(a);
            (h.key.clone(), callout(&h, &d.link, &d.title))
        })
        .collect();
    let existing = read_note(&path)?;
    let (text, added) = append_entries(existing.as_deref(), &d.header(), &entries);
    if added > 0 || existing.is_none() {
        write_atomically(&path, &text).map_err(|e| format!("Couldn't write {rel}: {e}."))?;
    }
    Ok((rel, added))
}

fn open_in_obsidian(app: &mut PdfCraftApp, i: usize, which: Option<(usize, usize)>) -> Result<String, String> {
    let key = which.and_then(|(page, index)| {
        let doc = app.session.get(app.views.get(i)?.id)?;
        doc.info.annotations.iter().find(|a| a.page == page && a.index == index && is_markup(a)).map(entry_key)
    });
    let keys: Vec<String> = key.into_iter().collect();
    let (rel, _) = write_note(app, i, Some(&keys))?;
    let vault = app.notes.vault.as_deref().ok_or("Set notes.vault to your Obsidian vault folder first.")?;
    let uri = obsidian_uri(vault, &rel).ok_or("The vault folder has no name Obsidian could know it by.")?;
    launch_uri(app, &uri)?;
    Ok(format!("Opened {rel} in Obsidian"))
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
    let rel = DocNotes::of(&app.notes, doc).note?;
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
    match key {
        "notes.vault" if value.is_empty() => app.notes.vault = None,
        "notes.vault" => {
            let vault = PathBuf::from(value);
            if !vault.is_absolute() || !vault.is_dir() {
                return Err(format!("notes.vault must be an existing folder given by its full path, not {value}"));
            }
            app.notes.vault = Some(vault);
        }
        "notes.note_template" => {
            note_path(value, "title")?;
            app.notes.note_template = value.to_string();
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
            app.notes.colors = colors;
            app.notes.palette_published = false;
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
        }
        other => return Err(format!("unknown option {other}")),
    }
    Ok(())
}

/// Autoadd for view `i`: note new highlights as the document changes, and write the ones whose
/// note isn't being typed any more (the note typed into a new highlight goes into its callout).
fn autoadd(app: &mut PdfCraftApp, i: usize) {
    let Some(view) = app.views.get(i) else { return };
    let id = view.id;
    let Some(doc) = app.session.get(id) else { return };
    if !doc.path.as_ref().is_some_and(|p| app.notes.autoadd.contains(p)) {
        app.notes.watched.remove(&id);
        return;
    }
    let generation = doc.edit_generation();
    let current = || markup_comments(&doc.info).into_iter().map(entry_key).collect::<BTreeSet<_>>();
    let watch = app.notes.watched.entry(id).or_insert_with(|| AutoaddWatch { generation, keys: current(), pending: Vec::new() });
    if watch.generation != generation {
        let now = current();
        for key in now.difference(&watch.keys) {
            if !watch.pending.contains(key) {
                watch.pending.push(key.clone());
            }
        }
        // Undone highlights stop waiting.
        watch.pending.retain(|k| now.contains(k));
        watch.keys = now;
        watch.generation = generation;
    }
    if watch.pending.is_empty() {
        return;
    }
    let typing = view.comments.editing.as_ref().and_then(|(page, index, _)| {
        doc.info.annotations.iter().find(|a| a.page == *page && a.index == *index && a.in_reply_to.is_none()).map(entry_key)
    });
    let ready: Vec<String> = watch.pending.iter().filter(|k| Some(*k) != typing.as_ref()).cloned().collect();
    if ready.is_empty() {
        return;
    }
    watch.pending.retain(|k| !ready.contains(k));
    match write_note(app, i, Some(&ready)) {
        Ok((rel, added)) if added > 0 => app.notify(format!("Added to {rel}")),
        Ok(_) => {}
        // A failed write stays on screen until dismissed; the highlight stays in the PDF and Retry
        // (Write to note) writes it.
        Err(e) => {
            let retry = app.views.get(i).map(|v| super::toast::Action::WriteNote(v.id));
            super::toast::failed(app, e, retry);
        }
    }
}

fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let digits = text.trim().strip_prefix('#')?;
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let channel = |at: usize| digits.get(at..at + 2).and_then(|d| u8::from_str_radix(d, 16).ok());
    Some([channel(0)?, channel(2)?, channel(4)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn highlight(note: &str) -> Highlight {
        Highlight { key: "a1b2".into(), page: 2, label: "3".into(), color: "yellow".into(), text: "the  marked\ntext".into(), note: note.into() }
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
    fn appending_is_idempotent_by_key() {
        let entries = vec![("nm-1".to_string(), "> [!PDF|yellow] one\n".to_string()), ("nm-2".to_string(), "> [!PDF|red] two\n".to_string())];
        let (first, added) = append_entries(None, "---\nsource: \"[[a.pdf]]\"\n---\n\n", &entries);
        assert_eq!(added, 2);
        assert_eq!(
            first,
            "---\nsource: \"[[a.pdf]]\"\n---\n\n## Highlights\n\n<!-- margin:nm-1 -->\n> [!PDF|yellow] one\n\n<!-- margin:nm-2 -->\n> [!PDF|red] two\n"
        );
        let (second, added) = append_entries(Some(&first), "", &entries);
        assert_eq!((second.as_str(), added), (first.as_str(), 0));
        assert_eq!(second.matches("<!-- margin:nm-1 -->").count(), 1);
    }

    #[test]
    fn appending_goes_under_the_highlights_heading_and_keeps_the_rest() {
        let note = "# Paper\n\nmy thoughts\n\n## Highlights\n\n<!-- margin:old -->\n> [!PDF|yellow] old\n\n## Questions\n\n- why?\n";
        let (out, added) = append_entries(Some(note), "", &[("new".into(), "> [!PDF|red] new\n".into())]);
        assert_eq!(added, 1);
        assert_eq!(
            out,
            "# Paper\n\nmy thoughts\n\n## Highlights\n\n<!-- margin:old -->\n> [!PDF|yellow] old\n\n<!-- margin:new -->\n> [!PDF|red] new\n\n## Questions\n\n- why?\n"
        );
        // A note without the heading gets it at the end.
        let (out, _) = append_entries(Some("# Paper\nnotes"), "", &[("k".into(), "> c\n".into())]);
        assert_eq!(out, "# Paper\nnotes\n\n## Highlights\n\n<!-- margin:k -->\n> c\n");
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
    fn settings_round_trip_and_reject_garbage() {
        let notes = Notes { vault: Some(PathBuf::from("/v")), ..Notes::default() };
        let mut back = Notes::default();
        back.restore(&notes.to_json());
        assert_eq!((back.vault, back.colors), (notes.vault.clone(), notes.colors.clone()));
        let mut bad = Notes::default();
        bad.restore(&serde_json::json!({ "vault": "relative", "note_template": "../x.md", "colors": [["", "#zzzzzz"]] }));
        assert_eq!((bad.vault, bad.note_template, bad.colors), (None, DEFAULT_NOTE_TEMPLATE.to_string(), default_colors()));
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
