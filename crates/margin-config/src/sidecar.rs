//! The sidecar v1 file format (CU-SC, CUSTOMIZATION §8). `SidecarStore`, the only reader and
//! writer, is M2.9; the format stays a draft until the PDF daily gate freezes it (M2.24). Unknown
//! fields survive a rewrite (`extra`, CU-SC-3), so every record keeps them.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The `format` value of every sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SidecarFormat {
    /// `"margin-sidecar"`.
    #[serde(rename = "margin-sidecar")]
    MarginSidecar,
}

/// A position in a document, the JSON form of `margin_doc::Locator` (CU-SC-4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Locator {
    /// A PDF position in view pt (ARCHITECTURE §3.1).
    Pdf {
        /// 0-based page index.
        page: u32,
        /// `ViewQuad`s: x1,y1..x4,y4, corners top-left, top-right, bottom-left, bottom-right.
        #[serde(default)]
        quads: Vec<[f64; 8]>,
        /// A point as fractions 0..1 of the page's view box.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<[f64; 2]>,
    },
    /// An EPUB CFI.
    Cfi {
        /// The CFI.
        cfi: String,
    },
    /// A text position in UTF-16 code units (PERF-E6).
    Text {
        /// Start.
        offset: u64,
        /// Length.
        len: u64,
    },
}

/// The document block: identity and hashes (CU-SC-2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SidecarDoc {
    /// ULID made on first open; survives moves and edits
    pub book_id: String,
    /// KOReader partialMD5, hex
    pub sidecar_fingerprint: String,
    /// file size in bytes
    pub size: u64,
    /// BLAKE3 of the whole file at its current revision
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_revision_hash: Option<String>,
    /// "pdf" | "epub" | "md" | "html" | "txt"
    pub format: String,
    /// page count, for paged formats
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    /// document title
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// last known path
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hint: Option<String>,
    /// fields from newer versions, kept on rewrite
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Reading progress.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Progress {
    /// where reading stopped
    pub locator: Locator,
    /// 0..1 through the document
    #[schemars(range(min = 0.0, max = 1.0))]
    pub fraction: f64,
    /// marked finished
    #[serde(default)]
    pub finished: bool,
    /// RFC 3339 UTC
    pub updated: String,
    /// fields from newer versions, kept on rewrite
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Highlight style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HighlightStyle {
    /// Filled highlight.
    Highlight,
    /// Underline.
    Underline,
    /// Strike-out.
    Strikeout,
    /// Squiggly underline.
    Squiggly,
}

/// Where a highlight came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HighlightSource {
    /// Made in Margin.
    Margin,
    /// Imported from a markup annotation in the PDF (CU-SC-5).
    Pdf,
}

/// The PDF annotation behind a highlight (ARCHITECTURE §3.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PdfAnnot {
    /// the annotation's /NM
    pub nm: String,
    /// object number, null until the identity fork API (M1.2b)
    pub obj: Option<u32>,
    /// generation number
    pub r#gen: Option<u16>,
}

/// A highlight record (CU-SC-5, §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Highlight {
    /// ULID
    pub id: String,
    /// where it is
    pub locator: Locator,
    /// crop box x0,y0,x1,y1 (user space), rotation, UserUnit when written
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geom: Option<[f64; 6]>,
    /// the highlighted text
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// highlight style
    pub style: HighlightStyle,
    /// a notes.colors id
    pub color: String,
    /// the colour at creation, used when color is gone from notes.colors
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hex: Option<String>,
    /// the comment
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// outline section at creation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// who made it
    pub source: HighlightSource,
    /// RFC 3339 UTC
    pub created: String,
    /// RFC 3339 UTC
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// the PDF annotation, null when sidecar-only
    #[serde(default)]
    pub pdf_annot: Option<PdfAnnot>,
    /// Obsidian callout state {note, block, written_hash}
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obsidian: Option<Value>,
    /// "deleted_in_pdf" when another app removed it (CU-SC-5)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// fields from newer versions, kept on rewrite
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A reading bookmark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Bookmark {
    /// ULID
    pub id: String,
    /// where it is
    pub locator: Locator,
    /// label, "" for none
    #[serde(default)]
    pub name: String,
    /// RFC 3339 UTC
    pub created: String,
    /// fields from newer versions, kept on rewrite
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A page note not tied to a highlight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PageNote {
    /// ULID
    pub id: String,
    /// where it is
    pub locator: Locator,
    /// crop box, rotation and UserUnit when written
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geom: Option<[f64; 6]>,
    /// the note text
    pub text: String,
    /// RFC 3339 UTC
    pub created: String,
    /// RFC 3339 UTC
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// fields from newer versions, kept on rewrite
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A manual trim: the share of the page cut from each side, `[left, top, right, bottom]`, each
/// `0..0.45` of the page's view box (after `/Rotate`, before the view rotation).
pub type TrimMargins = [f64; 4];

/// How a document's pages are trimmed for display. Whether trimming is on is the `[doc]`
/// setting `view.trim`; this block says how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Trim {
    /// false: each page's content box, found on the rendered page; true: the margins below
    #[serde(default)]
    pub manual: bool,
    /// manual margins of odd pages (1, 3, …), or of every page unless `separate`
    #[serde(default)]
    pub odd: TrimMargins,
    /// manual margins of even pages, used when `separate`
    #[serde(default)]
    pub even: TrimMargins,
    /// odd and even pages keep their own margins (book scans)
    #[serde(default)]
    pub separate: bool,
    /// fields from newer versions, kept on rewrite
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The document's link to Obsidian (CU-OBS-1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SidecarObsidian {
    /// new highlights are added to the document's note as they are made
    #[serde(default)]
    pub auto_add: bool,
    /// fields from newer versions, kept on rewrite
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A sidecar file, `<file>.margin.json` or `<data>/sidecars/<book_id>.margin.json`. A pointer file
/// left by a store migration holds only `format`, `version`, `book_id` and `migrated_to` (CU-SC-7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "Margin sidecar")]
pub struct Sidecar {
    /// always "margin-sidecar"
    pub format: SidecarFormat,
    /// format version; a newer one opens read-only (CU-VAL-4)
    #[schemars(range(min = 1))]
    pub version: u32,
    /// store-level write counter (CU-SC-7)
    #[serde(default)]
    pub revision: u64,
    /// RFC 3339, set on every write
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// ids of deleted records (tombstones)
    #[serde(default)]
    pub deleted: Vec<String>,
    /// the book, on pointer files only
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub book_id: Option<String>,
    /// where the data moved, on pointer files only: {store, path, at}
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrated_to: Option<Value>,
    /// identity and hashes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<SidecarDoc>,
    /// reading progress
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// [doc] settings for this document (CU-SET-1 layer 5)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "settings_reference")]
    pub settings: Option<Value>,
    /// highlights
    #[serde(default)]
    pub highlights: Vec<Highlight>,
    /// bookmarks
    #[serde(default)]
    pub bookmarks: Vec<Bookmark>,
    /// page notes
    #[serde(default)]
    pub notes: Vec<PageNote>,
    /// how pages are trimmed when `view.trim` is on
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim: Option<Trim>,
    /// the document's Obsidian link
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obsidian: Option<SidecarObsidian>,
    /// fields from newer versions, kept on rewrite
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The settings object of a sidecar, checked against the settings schema beside this one.
fn settings_reference(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "$ref": "settings.schema.json" })
}
