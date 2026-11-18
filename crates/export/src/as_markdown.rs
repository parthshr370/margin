//! PDF layout mapping into the shared extracted-text Markdown writer.
use super::layout::{Cell, Item, Page, items};
use craft_markdown::extracted::{TableCell, Writer};
use std::sync::Arc;

#[cfg(test)]
pub(crate) use craft_markdown::extracted::{emphasis, paragraph};

impl TableCell for Cell {
    fn text(&self) -> &str {
        &self.text
    }
    fn bold(&self) -> bool {
        self.bold
    }
    fn italic(&self) -> bool {
        self.italic
    }
    fn span(&self) -> usize {
        self.span
    }
}

/// Markdown (CommonMark with pipe tables), retaining PDF reading order and heading detection.
/// Image omission is this export's policy; formatting and syntax live in the shared writer.
pub fn markdown(pages: &[Page]) -> String {
    let mut writer = Writer::new();
    for item in items(pages) {
        match item {
            Item::Para(runs, level) => {
                // Heading weight comes from the heading marker, as in the HTML export.
                let runs: Vec<_> = runs.iter().map(|r| (r.text.as_str(), r.block.bold && level == 0, r.block.italic)).collect();
                writer.paragraph(&runs, level);
            }
            Item::Table(table) => writer.table(&table.rows),
            Item::Img(_) => {}
            Item::PageBreak => writer.page_break(),
        }
    }
    writer.finish()
}

/// The most source text (paragraph runs and table cells) one [`markdown_note`] may be built
/// from. A longer paper is exported a few pages at a time; the check runs as the text is taken,
/// so a hostile file can't make the note grow without end (escaping at most doubles it).
pub const MAX_MARKDOWN_TEXT_BYTES: usize = 64 << 20;

/// The heading level of the note's title: the paper's own headings start one level below it.
const TITLE_LEVEL: u8 = 1;

/// A Markdown note and the image files it references, in the order the text embeds them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Markdown {
    pub text: String,
    /// File name (`<prefix>image-0001.png`) and encoded bytes, shared with the page.
    pub images: Vec<(String, Arc<[u8]>)>,
}

/// How [`markdown_note`] writes its output.
#[derive(Clone, Copy, Debug)]
pub struct MarkdownOptions<'a> {
    /// The `# ` heading at the top.
    pub title: &'a str,
    /// Obsidian embeds (`![[name]]`) instead of standard image links.
    pub obsidian: bool,
    /// Put in front of every image file name, so several exports can share one folder.
    pub image_prefix: &'a str,
}

/// The pages as a Markdown note (Margin's paper export): a `#` title, the paper's headings from
/// `##`, paragraphs, pipe tables and page separators as [`markdown`] writes them, and each image
/// as a separate file next to the note, embedded where it sits in the reading order. Fails when
/// the source text passes [`MAX_MARKDOWN_TEXT_BYTES`].
pub fn markdown_note(pages: &[Page], options: &MarkdownOptions<'_>) -> Result<Markdown, String> {
    let too_large = || format!("The Markdown would be larger than {} MiB; export fewer pages at a time.", MAX_MARKDOWN_TEXT_BYTES >> 20);
    // The shared writer takes no raw markup, so the text between two images is one writer's
    // output and each image link is joined in between, as its own block.
    let mut blocks: Vec<String> = Vec::new();
    let mut writer = Writer::new();
    writer.paragraph(&[(options.title, false, false)], TITLE_LEVEL);
    let mut images: Vec<(String, Arc<[u8]>)> = Vec::new();
    let mut source_bytes = 0usize;
    for item in items(pages) {
        match item {
            Item::Para(runs, level) => {
                source_bytes = runs.iter().fold(source_bytes, |sum, run| sum.saturating_add(run.text.len()));
                let runs: Vec<_> = runs.iter().map(|r| (r.text.as_str(), r.block.bold && level == 0, r.block.italic)).collect();
                let level = if level == 0 { 0 } else { level.saturating_add(TITLE_LEVEL) };
                writer.paragraph(&runs, level);
            }
            Item::Table(table) => {
                source_bytes = table.rows.iter().flatten().fold(source_bytes, |sum, cell| sum.saturating_add(cell.text.len()));
                writer.table(&table.rows);
            }
            Item::Img(image) => {
                let name = format!("{}image-{:04}.{}", options.image_prefix, images.len().saturating_add(1), image.ext);
                blocks.push(std::mem::take(&mut writer).finish());
                blocks.push(if options.obsidian { format!("![[{name}]]\n") } else { format!("![image](<{name}>)\n") });
                images.push((name, image.bytes.clone()));
            }
            Item::PageBreak => writer.page_break(),
        }
        if source_bytes > MAX_MARKDOWN_TEXT_BYTES {
            return Err(too_large());
        }
    }
    blocks.push(writer.finish());
    // A writer with no blocks finishes as a lone newline: leave those out of the note.
    let text = blocks.into_iter().filter(|block| !block.trim().is_empty()).collect::<Vec<_>>().join("\n");
    Ok(Markdown { text, images })
}

#[cfg(test)]
mod tests {
    use super::paragraph;

    #[test]
    fn empty_or_control_run_does_not_split_styled_runs() {
        assert_eq!(paragraph(&[("hello", true, false), ("", false, false), ("world", true, false)]), "**helloworld**");
        assert_eq!(paragraph(&[("hello", true, false), ("\u{1}", false, false), ("world", true, false)]), "**helloworld**");
    }

    #[test]
    fn empty_styled_run_does_not_affect_neighbor_emphasis() {
        assert_eq!(paragraph(&[("x", true, false), ("", false, true)]), "**x**");
        assert_eq!(paragraph(&[("foo", false, false), (".", true, false), ("", true, true), ("bar", false, false)]), "foo<strong>.</strong>bar");
        // Empty/control runs between differently styled runs still look ahead to the next non-empty run.
        assert_eq!(paragraph(&[("bold", true, false), ("", false, false), ("italic", false, true)]), "<strong>bold</strong>*italic*");
        assert_eq!(paragraph(&[("bold", true, false), ("\u{1}", false, false), (" italic", false, true)]), "**bold** *italic*");
        // Leading empty/control runs do not affect the opening delimiter.
        assert_eq!(paragraph(&[("", false, true), ("\u{1}", true, false), ("lead", true, false)]), "**lead**");
        // All-empty/control paragraphs produce empty output.
        assert_eq!(paragraph(&[("", true, false), ("\u{1}", false, true), ("\u{7f}", false, false)]), "");
    }
}

#[cfg(test)]
#[test]
fn shared_table_span_limit_matches_layout_contract() {
    assert_eq!(craft_markdown::extracted::MAX_SPAN, super::layout::MAX_COLS);
}
