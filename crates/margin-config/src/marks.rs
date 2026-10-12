//! The records a reader adds to a sidecar (CU-SC-5, CU-SC-7): highlights kept only in the sidecar
//! (`pdf_annot: null`, for PDFs Margin can't write), page notes, bookmarks, and the tombstones of
//! deleted ones. Two writers of one sidecar (two windows, a sync tool) merge here instead of
//! overwriting each other: a record is the newer of its copies, and a tombstone removes a record
//! for good. [`copy_with_marks`] writes the sidecar's highlights and notes into a copy of the PDF
//! as real annotations.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pdfcraft_engine::{Edit, Markup, NewAnnotation, NoteIcon, Session, Shape, Style};
use pdfcraft_render::PageInfo;

use crate::sidecar::{Bookmark, Highlight, HighlightStyle, Locator, PageNote, Sidecar};

/// Sidecar-only highlights kept per document (the sidecar is untrusted input).
pub const MAX_HIGHLIGHTS: usize = 50_000;
/// Page notes kept per document.
pub const MAX_NOTES: usize = 10_000;
/// Tombstones kept per document; the oldest go first.
pub const MAX_TOMBSTONES: usize = 100_000;
/// Quadrilaterals one highlight may cover (a page of text is a few hundred lines).
pub const MAX_QUADS: usize = 4_096;

/// Whether `highlight` lives only in the sidecar: no PDF annotation behind it, not removed.
pub fn sidecar_only(highlight: &Highlight) -> bool {
    highlight.pdf_annot.is_none() && highlight.state.is_none()
}

/// A record with an id and a time it last changed.
trait Record: Clone {
    fn id(&self) -> &str;
    /// RFC 3339 UTC of the last change: `modified`, else `created`. Same-format RFC 3339
    /// strings sort by time.
    fn changed(&self) -> &str;
}

impl Record for Highlight {
    fn id(&self) -> &str {
        &self.id
    }
    fn changed(&self) -> &str {
        self.modified.as_deref().unwrap_or(&self.created)
    }
}

impl Record for PageNote {
    fn id(&self) -> &str {
        &self.id
    }
    fn changed(&self) -> &str {
        self.modified.as_deref().unwrap_or(&self.created)
    }
}

impl Record for Bookmark {
    fn id(&self) -> &str {
        &self.id
    }
    fn changed(&self) -> &str {
        self.modified.as_deref().unwrap_or(&self.created)
    }
}

/// Record `id` as deleted, keeping at most [`MAX_TOMBSTONES`] (the oldest go first).
pub fn add_tombstone(deleted: &mut Vec<String>, id: &str) {
    if deleted.iter().any(|known| known == id) {
        return;
    }
    deleted.push(id.to_owned());
    let excess = deleted.len().saturating_sub(MAX_TOMBSTONES);
    if excess > 0 {
        deleted.drain(..excess);
    }
}

/// Forget that `id` was deleted (an undone deletion).
pub fn remove_tombstone(deleted: &mut Vec<String>, id: &str) {
    deleted.retain(|known| known != id);
}

/// Merge `theirs`, a copy of the same sidecar another writer saved since `mine` was read, into
/// `mine`: the union of both sides' highlights, notes and bookmarks, the newer copy of a record
/// both have (ties keep `mine`), minus every record either side deleted. The reading state
/// (`progress`, `settings`, `trim`, `obsidian`, `doc`) stays `mine`: it is this reader's.
/// Unknown top-level fields of `theirs` that `mine` lacks are kept, and `revision` becomes the
/// larger of the two, so the next write counts past both.
pub fn merge(mine: &mut Sidecar, theirs: &Sidecar) {
    for id in &theirs.deleted {
        add_tombstone(&mut mine.deleted, id);
    }
    let deleted: HashSet<&str> = mine.deleted.iter().map(String::as_str).collect();
    merge_records(&mut mine.highlights, &theirs.highlights, &deleted, MAX_HIGHLIGHTS);
    merge_records(&mut mine.notes, &theirs.notes, &deleted, MAX_NOTES);
    merge_records(&mut mine.bookmarks, &theirs.bookmarks, &deleted, MAX_NOTES);
    mine.revision = mine.revision.max(theirs.revision);
    for (key, value) in &theirs.extra {
        mine.extra.entry(key.clone()).or_insert_with(|| value.clone());
    }
}

/// [`merge`] for one list: `mine` keeps its order, records only `theirs` has follow.
fn merge_records<T: Record>(mine: &mut Vec<T>, theirs: &[T], deleted: &HashSet<&str>, cap: usize) {
    let mut position: HashMap<String, usize> = mine.iter().enumerate().map(|(i, record)| (record.id().to_owned(), i)).collect();
    for record in theirs {
        if let Some(known) = position.get(record.id()).and_then(|&i| mine.get_mut(i)) {
            if record.changed() > known.changed() {
                *known = record.clone();
            }
        } else if mine.len() < cap {
            position.insert(record.id().to_owned(), mine.len());
            mine.push(record.clone());
        }
    }
    mine.retain(|record| !deleted.contains(record.id()));
}

/// The page a PDF locator points at.
pub fn locator_page(locator: &Locator) -> Option<usize> {
    match locator {
        Locator::Pdf { page, .. } => usize::try_from(*page).ok(),
        Locator::Cfi { .. } | Locator::Text { .. } => None,
    }
}

/// The engine's markup kind for a highlight style.
pub fn markup_of(style: HighlightStyle) -> Markup {
    match style {
        HighlightStyle::Highlight => Markup::Highlight,
        HighlightStyle::Underline => Markup::Underline,
        HighlightStyle::Strikeout => Markup::StrikeOut,
        HighlightStyle::Squiggly => Markup::Squiggly,
    }
}

/// The highlight style of an engine markup kind.
pub fn style_of(kind: Markup) -> HighlightStyle {
    match kind {
        Markup::Highlight => HighlightStyle::Highlight,
        Markup::Underline => HighlightStyle::Underline,
        Markup::StrikeOut => HighlightStyle::Strikeout,
        Markup::Squiggly => HighlightStyle::Squiggly,
    }
}

/// `#rrggbb` as 0..1 channels.
pub fn hex_rgb(hex: &str) -> Option<[f64; 3]> {
    let digits = hex.strip_prefix('#')?;
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let channel = |at: usize| digits.get(at..at + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok()).map(|v| f64::from(v) / 255.0);
    Some([channel(0)?, channel(2)?, channel(4)?])
}

/// 0..1 channels as `#rrggbb`.
pub fn rgb_hex(rgb: [f64; 3]) -> String {
    let byte = |c: f64| if c.is_finite() { (c.clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 };
    format!("#{:02x}{:02x}{:02x}", byte(rgb[0]), byte(rgb[1]), byte(rgb[2]))
}

/// A view-space quadrilateral (points, y down, the page's `/Rotate` applied) as PDF user space,
/// corner by corner, so the top-left corner stays the top-left as the page is shown.
fn user_quad(page: &PageInfo, quad: &[f64; 8]) -> Option<[f64; 8]> {
    let mut out = [0.0; 8];
    for corner in 0..4 {
        let (x, y) = (*quad.get(corner * 2)?, *quad.get(corner * 2 + 1)?);
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let [ux, uy] = page.view_to_user(x as f32, y as f32);
        *out.get_mut(corner * 2)? = f64::from(ux);
        *out.get_mut(corner * 2 + 1)? = f64::from(uy);
    }
    Some(out)
}

/// What a copy with the sidecar's marks is made from.
pub struct MarksCopy<'a> {
    /// The PDF's file name, for the engine's messages.
    pub name: &'a str,
    pub bytes: Arc<Vec<u8>>,
    pub password: Option<&'a str>,
    /// Sidecar-only highlights; others are skipped.
    pub highlights: &'a [Highlight],
    pub notes: &'a [PageNote],
    /// How each highlight looks as an annotation.
    pub look: &'a dyn Fn(&Highlight) -> Style,
    /// The look of a page note's sticky note.
    pub note_look: Style,
    /// `/T` of every annotation written.
    pub author: &'a str,
}

/// What [`copy_with_marks`] made.
#[derive(Debug)]
pub struct MarksCopied {
    /// The new PDF: the original bytes plus one incremental update.
    pub bytes: Vec<u8>,
    /// Annotations written.
    pub written: usize,
    /// Records left out: off the document's pages, or without a usable position.
    pub skipped: usize,
}

/// A copy of the PDF with the sidecar's highlights as text-markup annotations (their notes as
/// `/Contents`) and its page notes as sticky notes, appended as one incremental update so the
/// original bytes, and any signature over them, stay as they were.
pub fn copy_with_marks(copy: &MarksCopy<'_>) -> Result<MarksCopied, String> {
    let mut session = Session::new();
    let id = session.open(copy.name, None, copy.bytes.clone(), copy.password).map_err(|err| err.to_string())?;
    let pages = session.get(id).map(|doc| doc.info.pages.clone()).unwrap_or_default();
    let mut edits = Vec::new();
    let mut skipped = 0usize;
    for highlight in copy.highlights.iter().filter(|h| sidecar_only(h)) {
        match highlight_annotation(highlight, &pages, copy) {
            Some(annotation) => edits.push(Edit::AddAnnotation(annotation)),
            None => skipped = skipped.saturating_add(1),
        }
    }
    for note in copy.notes {
        match note_annotation(note, &pages, copy) {
            Some(annotation) => edits.push(Edit::AddAnnotation(annotation)),
            None => skipped = skipped.saturating_add(1),
        }
    }
    let written = edits.len();
    if written == 0 {
        return Err("there are no highlights or notes to write".to_owned());
    }
    session.apply(id, Edit::Batch { label: "Add highlights from the sidecar".to_owned(), edits }).map_err(|err| err.to_string())?;
    let bytes = session.save_bytes(id).map_err(|err| err.to_string())?;
    Ok(MarksCopied { bytes: bytes.as_ref().clone(), written, skipped })
}

fn highlight_annotation(highlight: &Highlight, pages: &[PageInfo], copy: &MarksCopy<'_>) -> Option<NewAnnotation> {
    let Locator::Pdf { page, quads, .. } = &highlight.locator else { return None };
    let page_index = usize::try_from(*page).ok()?;
    let info = pages.get(page_index)?;
    let quads: Vec<[f64; 8]> = quads.iter().take(MAX_QUADS).filter_map(|quad| user_quad(info, quad)).collect();
    if quads.is_empty() {
        return None;
    }
    Some(NewAnnotation {
        page: page_index,
        shape: Shape::TextMarkup { kind: markup_of(highlight.style), quads },
        style: (copy.look)(highlight),
        contents: highlight.note.clone().unwrap_or_default(),
        author: copy.author.to_owned(),
    })
}

fn note_annotation(note: &PageNote, pages: &[PageInfo], copy: &MarksCopy<'_>) -> Option<NewAnnotation> {
    let Locator::Pdf { page, at, .. } = &note.locator else { return None };
    let page_index = usize::try_from(*page).ok()?;
    let info = pages.get(page_index)?;
    let [across, down] = at.unwrap_or([0.0, 0.0]);
    if !across.is_finite() || !down.is_finite() {
        return None;
    }
    let (x, y) = (across.clamp(0.0, 1.0) as f32 * info.width, down.clamp(0.0, 1.0) as f32 * info.height);
    let [ux, uy] = info.view_to_user(x, y);
    Some(NewAnnotation {
        page: page_index,
        shape: Shape::Note { at: [f64::from(ux), f64::from(uy)], icon: NoteIcon::Note },
        style: copy.note_look.clone(),
        contents: note.text.clone(),
        author: copy.author.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::*;
    use crate::sidecar::HighlightSource;

    fn highlight(id: &str, created: &str, modified: Option<&str>, note: Option<&str>) -> Highlight {
        Highlight {
            id: id.to_owned(),
            locator: Locator::Pdf { page: 0, quads: vec![[20.0, 30.0, 120.0, 30.0, 20.0, 46.0, 120.0, 46.0]], at: None },
            geom: None,
            text: Some("The quick brown fox".to_owned()),
            style: HighlightStyle::Highlight,
            color: "yellow".to_owned(),
            hex: Some("#ffd000".to_owned()),
            note: note.map(str::to_owned),
            section: None,
            source: HighlightSource::Margin,
            created: created.to_owned(),
            modified: modified.map(str::to_owned),
            pdf_annot: None,
            obsidian: None,
            state: None,
            extra: Map::new(),
        }
    }

    fn page_note(id: &str, text: &str) -> PageNote {
        PageNote {
            id: id.to_owned(),
            locator: Locator::Pdf { page: 0, quads: Vec::new(), at: Some([0.25, 0.5]) },
            geom: None,
            text: text.to_owned(),
            created: "2026-10-10T10:00:00Z".to_owned(),
            modified: None,
            extra: Map::new(),
        }
    }

    const T1: &str = "2026-10-10T10:00:00Z";
    const T2: &str = "2026-10-10T11:00:00Z";

    #[test]
    fn merge_unions_records_keeps_the_newer_copy_and_honours_tombstones() {
        let mut mine = Sidecar::new();
        mine.revision = 4;
        mine.highlights =
            vec![highlight("A", T1, None, Some("mine")), highlight("B", T1, Some(T2), Some("mine, newer")), highlight("C", T1, None, None)];
        mine.notes = vec![page_note("N1", "kept")];
        mine.deleted = vec!["D".to_owned()];
        let mut theirs = Sidecar::new();
        theirs.revision = 7;
        theirs.highlights = vec![
            highlight("A", T1, Some(T2), Some("theirs, newer")),
            highlight("B", T1, None, Some("theirs, older")),
            highlight("D", T1, None, None),
            highlight("E", T1, None, None),
        ];
        theirs.notes = vec![page_note("N2", "theirs")];
        theirs.deleted = vec!["C".to_owned()];
        theirs.extra.insert("future".to_owned(), serde_json::json!(1));
        merge(&mut mine, &theirs);
        let ids: Vec<&str> = mine.highlights.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["A", "B", "E"], "C deleted there, D deleted here");
        assert_eq!(mine.highlights[0].note.as_deref(), Some("theirs, newer"));
        assert_eq!(mine.highlights[1].note.as_deref(), Some("mine, newer"));
        assert_eq!(mine.notes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["N1", "N2"]);
        assert_eq!(mine.deleted, ["D", "C"]);
        assert_eq!(mine.revision, 7, "the next write counts past both writers");
        assert_eq!(mine.extra["future"], 1);
    }

    #[test]
    fn tombstones_are_unique_and_capped_oldest_first() {
        let mut deleted = Vec::new();
        add_tombstone(&mut deleted, "x");
        add_tombstone(&mut deleted, "x");
        assert_eq!(deleted, ["x"]);
        for i in 0..MAX_TOMBSTONES {
            add_tombstone(&mut deleted, &i.to_string());
        }
        assert_eq!(deleted.len(), MAX_TOMBSTONES);
        assert_eq!(deleted.first().map(String::as_str), Some("0"), "the oldest one went");
        remove_tombstone(&mut deleted, "0");
        assert_eq!(deleted.first().map(String::as_str), Some("1"));
    }

    #[test]
    fn hex_colours_round_trip_and_bad_ones_are_refused() {
        assert_eq!(hex_rgb("#ffd000").map(rgb_hex).as_deref(), Some("#ffd000"));
        for bad in ["ffd000", "#ffd00", "#ffd0zz", "#ffd0000", "#ééé"] {
            assert_eq!(hex_rgb(bad), None, "{bad}");
        }
        assert_eq!(rgb_hex([f64::NAN, 2.0, -1.0]), "#00ff00");
    }

    /// A one-page 300×200 PDF with a line of text and a valid cross-reference table.
    fn text_pdf() -> Vec<u8> {
        let content = "BT /F1 14 Tf 20 150 Td (The quick brown fox) Tj ET\n";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
        out
    }

    #[test]
    fn a_copy_gets_real_annotations_and_keeps_the_original_bytes() {
        let bytes = Arc::new(text_pdf());
        let mut off_page = highlight("Z", T1, None, None);
        off_page.locator = Locator::Pdf { page: 9, quads: vec![[0.0; 8]], at: None };
        let highlights = [highlight("A", T1, None, Some("why it matters")), off_page];
        let notes = [page_note("N", "a page note")];
        let look = |_: &Highlight| Style { color: [1.0, 0.8, 0.0], opacity: 0.5, width: 1.0, fill: None };
        let copy = MarksCopy {
            name: "text.pdf",
            bytes: bytes.clone(),
            password: None,
            highlights: &highlights,
            notes: &notes,
            look: &look,
            note_look: Style::default(),
            author: "Reader",
        };
        let made = copy_with_marks(&copy).expect("copied");
        assert_eq!((made.written, made.skipped), (2, 1));
        assert!(made.bytes.starts_with(&bytes), "an incremental update after the original bytes");
        let mut session = Session::new();
        let id = session.open("copy.pdf", None, Arc::new(made.bytes), None).expect("the copy opens");
        let doc = session.get(id).expect("open");
        let marked = doc.info.annotations.iter().find(|a| a.subtype == "Highlight").expect("a highlight");
        assert_eq!(marked.contents.as_deref(), Some("why it matters"));
        // View y 30..46 on a 200-pt page is user y 154..170.
        let quad = marked.quads.first().expect("a quad");
        assert!((quad[1] - 170.0).abs() < 0.5 && (quad[5] - 154.0).abs() < 0.5, "{quad:?}");
        let sticky = doc.info.annotations.iter().find(|a| a.subtype == "Text").expect("a sticky note");
        assert_eq!(sticky.contents.as_deref(), Some("a page note"));
    }

    #[test]
    fn a_copy_with_nothing_to_write_is_refused() {
        let look = |_: &Highlight| Style::default();
        let copy = MarksCopy {
            name: "text.pdf",
            bytes: Arc::new(text_pdf()),
            password: None,
            highlights: &[],
            notes: &[],
            look: &look,
            note_look: Style::default(),
            author: "Reader",
        };
        assert!(copy_with_marks(&copy).is_err());
    }
}
