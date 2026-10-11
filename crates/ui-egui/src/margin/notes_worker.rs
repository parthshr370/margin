//! Note jobs (Copy all, Write to note, highlight and paper exports, vault snapshots) run on a
//! worker thread with an immutable snapshot of the document; the UI only polls their progress.
use super::*;
use pdfcraft_engine::export::{ExportSource, MarkdownOptions};

/// The most highlight text (quotes and notes) one Copy all or highlight export takes on; the
/// output of every format is a small multiple of it.
const MAX_HIGHLIGHT_EXPORT_BYTES: usize = 16 << 20;
/// Room for a block's two boundary comments besides its key: hashes, owner, `sha256:` and so on.
const MARKER_ALLOWANCE_BYTES: usize = 192;
/// Snapshots into the vault are rendered at print resolution.
const SNAPSHOT_DPI: f32 = 300.0;
/// PDF user space has 72 units per inch.
const POINTS_PER_INCH: f32 = 72.0;
/// The largest snapshot: 64 million pixels (a full A3 page at 300 DPI is about 17 million).
const MAX_SNAPSHOT_PIXELS: u64 = 64_000_000;

#[derive(Clone, Debug)]
pub(super) enum Work {
    Copy,
    ExportHighlights { path: PathBuf },
    Write { only: Option<Vec<String>>, open: bool, automatic: bool },
    Paper { destination: PaperDestination },
    Snapshot { page: usize, rect: [f32; 4] },
}

#[derive(Clone, Debug)]
pub(super) enum PaperDestination {
    /// `<reading note>-paper.md` beside the reading note; refuses to replace anything.
    Vault,
    /// A file the user picked (the save panel already asked before replacing it).
    File(PathBuf),
}

#[derive(Debug, Default)]
pub(super) struct WorkerState {
    pub done: usize,
    pub total: usize,
    pub result: Option<Result<Completed, String>>,
    pub retry_write: bool,
}

#[derive(Debug)]
pub(super) struct Completed {
    /// What to say; `None` for an automatic write that changed nothing.
    pub message: Option<String>,
    pub clipboard: Option<String>,
    pub uri: Option<String>,
}

impl Completed {
    fn said(message: String) -> Self {
        Self { message: Some(message), clipboard: None, uri: None }
    }
}

struct Input {
    source: ExportSource,
    info: DocInfo,
    loaded: HashMap<usize, Arc<PageText>>,
    notes: Notes,
    context: DocNotes,
    work: Work,
}

pub(super) fn start(app: &mut PdfCraftApp, index: usize, work: Work) -> Result<(), String> {
    if !app.notes.jobs.is_empty() {
        return Err("A note export is still running. Please wait for it to finish.".into());
    }
    refresh_view(app, index);
    let view = app.views.get(index).ok_or("No document is open.")?;
    let doc = app.session.get(view.id).ok_or("No document is open.")?;
    let id = doc.id;
    // Worked out again rather than taken from the cache: the library's title and reading
    // progress change while the document stays open.
    let context = context_for(app, doc);
    let annotations: Vec<Annotation> = match app.notes.cached.get(&id) {
        Some(cached) => cached_markup(&doc.info, &cached.indices).cloned().collect(),
        None => markup_indices(&doc.info).into_iter().filter_map(|index| doc.info.annotations.get(index)).cloned().collect(),
    };
    let input = Input {
        source: doc.export_source(),
        info: DocInfo { pages: doc.info.pages.clone(), annotations, ..DocInfo::default() },
        loaded: view.texts.clone(),
        notes: app.notes.settings_only(),
        context,
        work,
    };
    let state = Arc::new(Mutex::new(WorkerState {
        total: input.info.annotations.len().max(1),
        retry_write: matches!(&input.work, Work::Write { only: None, open: false, .. }),
        ..WorkerState::default()
    }));
    app.notes.jobs.insert(id, state.clone());
    let ctx = app.ctx.clone();
    if let Some(ctx) = &ctx {
        ctx.request_repaint();
    }
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(input, &state)))
            .unwrap_or_else(|_| Err("The note export failed unexpectedly. Your PDF and existing note have been kept.".into()));
        match state.lock() {
            Ok(mut progress) => progress.result = Some(result),
            Err(poisoned) => poisoned.into_inner().result = Some(Err("The note export worker failed. Your PDF is unchanged.".into())),
        }
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
    Ok(())
}

fn execute(input: Input, progress: &Arc<Mutex<WorkerState>>) -> Result<Completed, String> {
    match input.work.clone() {
        Work::Paper { destination } => paper(&input, destination),
        Work::Snapshot { page, rect } => snapshot(&input, page, rect),
        Work::Copy => {
            let text = export_highlights(&read_highlights(input, progress)?)?;
            Ok(Completed { message: Some(format!("Copied {}", count(text.highlights, "highlight"))), clipboard: Some(text.text), uri: None })
        }
        Work::ExportHighlights { path } => {
            let text = export_highlights(&read_highlights(input, progress)?)?;
            write_atomically(&path, &text.text).map_err(|error| format!("Couldn't export highlights: {error}"))?;
            Ok(Completed::said(format!("Exported {} to {}", count(text.highlights, "highlight"), path.display())))
        }
        Work::Write { only, open, automatic } => {
            let read = read_highlights(input, progress)?;
            let entries = read
                .highlights
                .iter()
                .filter(|highlight| only.as_ref().is_none_or(|keys| keys.contains(&highlight.key)))
                .map(|highlight| read.context.highlight(&read.notes, highlight).map(|body| (highlight.key.clone(), body)))
                .collect::<Result<Vec<_>, _>>()?;
            let (relative, report) = write_entries(&read.notes, &read.context, &entries, only.is_none())?;
            let uri = if open {
                let vault = read.notes.vault.as_deref().ok_or("No vault is configured.")?;
                Some(obsidian_uri(vault, &relative).ok_or("The vault has no folder name.")?)
            } else {
                None
            };
            Ok(Completed { message: write_message(&relative, &report, automatic), clipboard: None, uri })
        }
    }
}

/// The document's highlights, read on the worker.
struct ReadHighlights {
    highlights: Vec<Highlight>,
    context: DocNotes,
    notes: Notes,
}

/// Every markup comment as a [`Highlight`], extracting the text of pages the view hasn't loaded.
fn read_highlights(input: Input, progress: &Arc<Mutex<WorkerState>>) -> Result<ReadHighlights, String> {
    let mut renderer: Option<PageRenderer> = None;
    let mut extracted = input.loaded;
    let mut highlights = Vec::with_capacity(input.info.annotations.len());
    for annotation in &input.info.annotations {
        if !extracted.contains_key(&annotation.page) {
            let rendered = renderer
                .get_or_insert_with(|| PageRenderer::new(input.source.bytes.clone(), input.source.config.clone()))
                .render(RenderRequest { page: annotation.page, kind: RequestKind::Text, ..RenderRequest::default() });
            if let Some(error) = rendered.error {
                return Err(format!("Couldn't extract page {}: {error}", annotation.page.saturating_add(1)));
            }
            if let Some(text) = rendered.text {
                extracted.insert(annotation.page, text);
            }
        }
        highlights.push(highlight_of(&input.notes, &input.info, annotation, extracted.get(&annotation.page).map(Arc::as_ref)));
        match progress.lock() {
            Ok(mut state) => state.done = highlights.len(),
            Err(_) => return Err("The note export worker lost its progress state.".into()),
        }
    }
    Ok(ReadHighlights { highlights, context: input.context, notes: input.notes })
}

/// "1 highlight", "3 highlights".
fn count(n: usize, what: &str) -> String {
    if n == 1 { format!("1 {what}") } else { format!("{n} {what}s") }
}

/// What a write to the reading note says. An automatic write that changed nothing says nothing;
/// one that only added says "Added to"; otherwise what was added, updated and removed.
fn write_message(relative: &str, report: &format::SyncReport, automatic: bool) -> Option<String> {
    let kept = match report.preserved.len() {
        0 => String::new(),
        n => format!("; left {} you edited (or from an older Margin) unchanged", count(n, "block")),
    };
    if report.changed() == 0 {
        return (!automatic).then(|| format!("{relative} already has every highlight{kept}"));
    }
    if automatic && report.added == report.changed() {
        return Some(format!("Added to {relative}{kept}"));
    }
    Some(format!("Updated {relative}: {}{kept}", report.summary()))
}

pub(super) fn write_entries(
    notes: &Notes,
    context: &DocNotes,
    entries: &[(String, String)],
    full: bool,
) -> Result<(String, format::SyncReport), String> {
    let relative = context.note.clone()?;
    let path = resolve_note(notes.vault.as_deref().ok_or("Set notes.vault to your Obsidian vault folder first.")?, &relative)?;
    let existing = read_note(&path)?;
    let header = context.header(notes)?;
    let maximum = usize::try_from(MAX_NOTE_BYTES).map_err(|_| "The note size limit is unavailable on this platform.")?;
    let too_large = "The reading note would exceed Margin's 16 MiB safety limit.";
    entries.iter().try_fold(existing.as_ref().map_or(header.len(), String::len), |size, (key, body)| {
        // Both boundaries carry a percent-encoded key (at most three bytes per input byte).
        size.checked_add(key.len().saturating_mul(6))
            .and_then(|size| size.checked_add(body.len()))
            .and_then(|size| size.checked_add(MARKER_ALLOWANCE_BYTES))
            .filter(|size| *size <= maximum)
            .ok_or(too_large)
    })?;
    let append_only = notes.config.update_mode == NoteUpdateMode::Append;
    let (text, report) = format::synchronize(existing.as_deref(), &header, entries, full, append_only, &context.owner);
    if existing.as_deref() != Some(text.as_str()) {
        if read_note(&path)? != existing {
            return Err(format!("{relative} changed in another editor during export. Nothing was overwritten; write again."));
        }
        write_atomically(&path, &text).map_err(|error| format!("Couldn't write {relative}: {error}"))?;
    }
    Ok((relative, report))
}

/// Highlights in `notes.export_target`'s format, and how many there were.
#[derive(Debug)]
struct Exported {
    text: String,
    highlights: usize,
}

/// `text` added to `output` unless that passes [`MAX_HIGHLIGHT_EXPORT_BYTES`].
fn push_bounded(output: &mut String, text: &str) -> Result<(), String> {
    if output.len().saturating_add(text.len()) > MAX_HIGHLIGHT_EXPORT_BYTES {
        return Err(format!("The highlights come to more than {} MiB; export fewer at a time.", MAX_HIGHLIGHT_EXPORT_BYTES >> 20));
    }
    output.push_str(text);
    Ok(())
}

fn export_highlights(read: &ReadHighlights) -> Result<Exported, String> {
    let ReadHighlights { highlights, context, notes } = read;
    if highlights.is_empty() {
        return Err("This document has no highlights yet.".into());
    }
    let payload = highlights.iter().fold(0usize, |sum, highlight| sum.saturating_add(highlight.text.len()).saturating_add(highlight.note.len()));
    if payload > MAX_HIGHLIGHT_EXPORT_BYTES {
        return Err(format!("The highlights come to more than {} MiB; export fewer at a time.", MAX_HIGHLIGHT_EXPORT_BYTES >> 20));
    }
    let mut output = String::new();
    match notes.config.export_target {
        ExportTarget::Obsidian => {
            for (index, highlight) in highlights.iter().enumerate() {
                if index > 0 {
                    push_bounded(&mut output, "\n")?;
                }
                push_bounded(&mut output, &context.highlight(notes, highlight)?)?;
            }
        }
        ExportTarget::Markdown => {
            for highlight in highlights {
                push_bounded(&mut output, &format!("### {}\n\n> {}\n\n", page_link(context, highlight), plain_inline(&highlight.text)))?;
                for line in highlight.note.lines() {
                    push_bounded(&mut output, &format!("{}\n", plain_inline(line)))?;
                }
                if !highlight.note.is_empty() {
                    push_bounded(&mut output, "\n")?;
                }
            }
        }
        ExportTarget::Logseq => {
            for highlight in highlights {
                push_bounded(&mut output, &format!("- {} {}\n", plain_inline(&highlight.text), page_link(context, highlight)))?;
                for line in highlight.note.lines() {
                    push_bounded(&mut output, &format!("  - {}\n", plain_inline(line)))?;
                }
            }
        }
        ExportTarget::ReadwiseCsv => readwise_csv(&mut output, highlights, context)?,
        ExportTarget::Json => {
            let entries: Vec<serde_json::Value> = highlights
                .iter()
                .map(|highlight| {
                    serde_json::json!({
                        "id": highlight.key,
                        "page": highlight.page.saturating_add(1),
                        "page_label": highlight.label,
                        "color": highlight.color,
                        "text": highlight.text,
                        "note": highlight.note,
                        "date": highlight.date,
                        "link": context.link.markdown(highlight.page, &display_text(&context.title, &highlight.label)),
                    })
                })
                .collect();
            let document = serde_json::json!({ "title": context.title, "authors": context.variables.get("authors"), "highlights": entries });
            output = serde_json::to_string_pretty(&document).map_err(|error| error.to_string())?;
        }
    }
    Ok(Exported { text: output, highlights: highlights.len() })
}

/// Readwise's CSV import: the columns its import page documents, in its order. URL stays empty
/// (a local PDF has none); Location is the page number; Date is when the highlight last changed.
fn readwise_csv(output: &mut String, highlights: &[Highlight], context: &DocNotes) -> Result<(), String> {
    const HEADER: &str = "Highlight,Title,Author,URL,Note,Location,Date\r\n";
    push_bounded(output, HEADER)?;
    let author = context.variables.get("authors").map(String::as_str).unwrap_or_default();
    for highlight in highlights {
        let page = highlight.page.saturating_add(1).to_string();
        let fields = [highlight.text.as_str(), context.title.as_str(), author, "", highlight.note.as_str(), page.as_str(), highlight.date.as_str()];
        let row = fields.map(csv_field).join(",");
        push_bounded(output, &row)?;
        push_bounded(output, "\r\n")?;
    }
    Ok(())
}

/// `[p.<label>](<pdf>#page=N)` for the plain Markdown and Logseq exports.
fn page_link(context: &DocNotes, highlight: &Highlight) -> String {
    let target = match &context.link {
        PdfLink::Vault(path) => percent_encode(path, true),
        PdfLink::External(path) => file_url(path),
    };
    format!("[p.{}]({target}#page={})", highlight.label, highlight.page.saturating_add(1))
}

/// Text as one literal Markdown line: no emphasis, links or HTML.
fn plain_inline(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', " ")
}

/// One RFC 4180 field: always quoted, quotes doubled; commas and line breaks stay inside.
fn csv_field(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

fn target_extension(target: ExportTarget) -> &'static str {
    match target {
        ExportTarget::ReadwiseCsv => "csv",
        ExportTarget::Json => "json",
        ExportTarget::Obsidian | ExportTarget::Markdown | ExportTarget::Logseq => "md",
    }
}

/// Asks where to save, then starts the export: the highlights in the export format, or the
/// whole paper as Markdown (`paper`).
pub(super) fn pick_export(app: &mut PdfCraftApp, index: usize, paper: bool) -> Result<(), String> {
    refresh_view(app, index);
    let view = app.views.get(index).ok_or("No document is open.")?;
    let id = view.id;
    let context = app.notes.cached.get(&id).ok_or("The document's note context is not ready.")?;
    let name = if paper {
        format!("{}-paper.md", slug(&context.context.title))
    } else {
        format!("{}-highlights.{}", slug(&context.context.title), target_extension(app.notes.config.export_target))
    };
    let shown = app.ask(crate::pickers::Ask::Save(rfd::AsyncFileDialog::new().set_file_name(name)), Some(id), move |app, paths| {
        let Some(path) = paths.into_iter().next() else { return };
        let Some(index) = app.views.iter().position(|view| view.id == id) else { return };
        let work = if paper { Work::Paper { destination: PaperDestination::File(path) } } else { Work::ExportHighlights { path } };
        if let Err(error) = start(app, index, work) {
            super::super::toast::failed(app, error, None);
        }
    });
    if shown { Ok(()) } else { Err("Couldn't open the export destination picker.".into()) }
}

/// The whole paper as Markdown with its images as files beside it (`<note>-image-0001.png`).
/// Every destination is checked before anything is written, and the images are written before
/// the note, so the note never points at a missing file.
fn paper(input: &Input, destination: PaperDestination) -> Result<Completed, String> {
    let into_vault = matches!(destination, PaperDestination::Vault);
    let path = match destination {
        PaperDestination::Vault => {
            let relative = input.context.note.clone()?;
            let paper_relative = format!("{}-paper.md", relative.strip_suffix(".md").unwrap_or(&relative));
            let path = resolve_note(input.notes.vault.as_deref().ok_or("No vault is configured.")?, &paper_relative)?;
            if path.exists() {
                return Err(format!(
                    "{paper_relative} already exists. Move or rename it to export again; Margin never replaces a note in the vault."
                ));
            }
            path
        }
        PaperDestination::File(path) => path,
    };
    let folder = path.parent().ok_or("The paper export has no destination folder.")?;
    let stem = path.file_stem().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "paper".into());
    let image_prefix = format!("{stem}-");
    let options = MarkdownOptions { title: &input.context.title, obsidian: into_vault, image_prefix: &image_prefix };
    let pages: Vec<usize> = (0..input.source.pages).collect();
    let exported = pdfcraft_engine::export::markdown(&input.source, &pages, &options)?;
    let images: Vec<(PathBuf, &[u8])> = exported.images.iter().map(|(name, bytes)| (folder.join(name), bytes.as_slice())).collect();
    if into_vault && let Some((taken, _)) = images.iter().find(|(image, _)| image.exists()) {
        return Err(format!("{} already exists; Margin never replaces a file in the vault.", taken.display()));
    }
    for (image, bytes) in &images {
        write_bytes_atomically(image, bytes).map_err(|error| format!("Couldn't write {}: {error}", image.display()))?;
    }
    write_atomically(&path, &exported.text).map_err(|error| format!("Couldn't write {}: {error}", path.display()))?;
    let with_images = if images.is_empty() { String::new() } else { format!(" with {}", count(images.len(), "image")) };
    Ok(Completed::said(format!("Exported the paper to {}{with_images}", path.display())))
}

/// Renders the selected area of `page` (view space, points) at 300 DPI, saves it as a PNG under
/// `notes.snapshot_folder`, and adds an embed and a link to the page to the reading note.
fn snapshot(input: &Input, page: usize, rect: [f32; 4]) -> Result<Completed, String> {
    if page >= input.source.pages || rect.iter().any(|coordinate| !coordinate.is_finite()) {
        return Err("The selected snapshot region is invalid.".into());
    }
    let scale = SNAPSHOT_DPI / POINTS_PER_INCH;
    let [left, top, right, bottom] = rect;
    let page_info = input.info.pages.get(page).ok_or("The snapshot page is missing.")?;
    if left < 0.0 || top < 0.0 || right <= left || bottom <= top || right > page_info.width || bottom > page_info.height {
        return Err("Select a nonempty region inside the page.".into());
    }
    // Finite, non-negative and within the page (checked above), so these casts can't wrap.
    let tile = pdfcraft_render::Tile {
        x: (left * scale).floor() as u32,
        y: (top * scale).floor() as u32,
        w: ((right - left) * scale).ceil() as u32,
        h: ((bottom - top) * scale).ceil() as u32,
    };
    if u64::from(tile.w).saturating_mul(u64::from(tile.h)) > MAX_SNAPSHOT_PIXELS {
        return Err("The selected region is too large at 300 DPI.".into());
    }
    let vault = input.notes.vault.as_deref().ok_or("Set notes.vault before saving a snapshot.")?;
    let note_relative = input.context.note.clone()?;
    let mut renderer = PageRenderer::new(input.source.bytes.clone(), input.source.config.clone());
    let image = renderer.render(RenderRequest { page, kind: RequestKind::Pixels, tile: Some(tile), scale, tag: 0 });
    if let Some(error) = image.error {
        return Err(format!("Couldn't render the snapshot: {error}"));
    }
    let bytes = pdfcraft_engine::export::encode_png(image.width, image.height, &image.rgba)?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|error| error.to_string())?.as_nanos();
    let name = format!("{}-p{}-{stamp}.png", slug(&input.context.title), page.saturating_add(1));
    let relative = format!("{}/{name}", input.notes.config.snapshot_folder.trim_matches('/'));
    let asset = resolve_asset(vault, &relative)?;
    if asset.exists() {
        return Err("The snapshot attachment already exists; nothing was overwritten.".into());
    }
    let note = resolve_note(vault, &note_relative)?;
    let existing = read_note(&note)?;
    let mut text = match &existing {
        Some(text) => text.clone(),
        None => input.context.header(&input.notes)?,
    };
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let link = input.context.link.markdown(page, &display_text(&input.context.title, &page_info.label));
    text.push_str(&format!("\n<!-- margin-snapshot:{name} -->\n![[{relative}]]\n{link}\n<!-- /margin-snapshot:{name} -->\n"));
    write_bytes_atomically(&asset, &bytes).map_err(|error| format!("Couldn't save snapshot: {error}"))?;
    if read_note(&note)? != existing {
        return Err(format!("{note_relative} changed in another editor. Snapshot saved at {relative}, but the note was not overwritten."));
    }
    write_atomically(&note, &text).map_err(|error| format!("Snapshot saved at {relative}, but the note couldn't be updated: {error}"))?;
    Ok(Completed::said(format!("Saved snapshot to {relative} and {note_relative}")))
}

/// Reuse the note's canonical vault guard for non-Markdown attachments, including symlinks.
fn resolve_asset(vault: &Path, relative: &str) -> Result<PathBuf, String> {
    let safe = note_path(&format!("{relative}.md"), "")?;
    let probe = resolve_note(vault, &safe)?;
    let path = probe.with_extension("");
    let root = vault.canonicalize().map_err(|error| error.to_string())?;
    if path.exists() && !path.canonicalize().is_ok_and(|real| real.starts_with(&root)) {
        return Err("The attachment path leads outside the vault.".into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> DocNotes {
        DocNotes {
            title: "Paper, \"one\"".into(),
            link: PdfLink::Vault("a.pdf".into()),
            note: Ok("a.md".into()),
            variables: BTreeMap::from([("authors".into(), "Author".into())]),
            owner: "a.pdf".into(),
        }
    }

    fn read(target: ExportTarget, highlights: Vec<Highlight>) -> ReadHighlights {
        let notes = Notes { config: margin_config::settings::Notes { export_target: target, ..Default::default() }, ..Default::default() };
        ReadHighlights { highlights, context: context(), notes }
    }

    fn highlight(text: &str, note: &str) -> Highlight {
        Highlight {
            key: "id".into(),
            page: 4,
            label: "v".into(),
            color: "yellow".into(),
            text: text.into(),
            note: note.into(),
            date: "2026-10-11".into(),
        }
    }

    #[test]
    fn readwise_csv_has_the_importer_columns_and_quotes_fields() {
        let exported = export_highlights(&read(ExportTarget::ReadwiseCsv, vec![highlight("a, b\n\"c\"", "why?\nindeed")])).unwrap();
        assert_eq!(
            exported.text,
            "Highlight,Title,Author,URL,Note,Location,Date\r\n\"a, b\n\"\"c\"\"\",\"Paper, \"\"one\"\"\",\"Author\",\"\",\"why?\nindeed\",\"5\",\"2026-10-11\"\r\n"
        );
        assert_eq!(exported.highlights, 1);
    }

    #[test]
    fn every_target_formats_the_quote_and_note() {
        for (target, expected) in [
            (ExportTarget::Markdown, "### [p.v](a.pdf#page=5)\n\n> \\[x\\]\n\nmine\n\n"),
            (ExportTarget::Logseq, "- \\[x\\] [p.v](a.pdf#page=5)\n  - mine\n"),
        ] {
            assert_eq!(export_highlights(&read(target, vec![highlight("[x]", "mine")])).unwrap().text, expected, "{target:?}");
        }
        let json = export_highlights(&read(ExportTarget::Json, vec![highlight("[x]", "mine")])).unwrap().text;
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["highlights"][0]["text"], "[x]");
        assert_eq!(parsed["highlights"][0]["page"], 5);
        let obsidian = export_highlights(&read(ExportTarget::Obsidian, vec![highlight("[x]", "mine")])).unwrap().text;
        assert!(obsidian.starts_with("> [!PDF|yellow] [[a.pdf#page=5|Paper, \"one\", p.v]]\n> > [x]\n"), "{obsidian}");
    }

    #[test]
    fn exports_refuse_payloads_past_their_budget() {
        let huge = highlight(&"x".repeat(MAX_HIGHLIGHT_EXPORT_BYTES), "y");
        assert!(export_highlights(&read(ExportTarget::Markdown, vec![huge])).unwrap_err().contains("export fewer"));
        assert!(export_highlights(&read(ExportTarget::Markdown, Vec::new())).unwrap_err().contains("no highlights"));
    }

    #[test]
    fn automatic_writes_that_change_nothing_stay_quiet() {
        let nothing = format::SyncReport::default();
        assert_eq!(write_message("r.md", &nothing, true), None);
        assert_eq!(write_message("r.md", &nothing, false).as_deref(), Some("r.md already has every highlight"));
        let added = format::SyncReport { added: 1, ..Default::default() };
        assert_eq!(write_message("r.md", &added, true).as_deref(), Some("Added to r.md"));
        assert_eq!(write_message("r.md", &added, false).as_deref(), Some("Updated r.md: 1 added"));
        let recolored = format::SyncReport { updated: 1, preserved: vec!["k".into()], ..Default::default() };
        assert_eq!(
            write_message("r.md", &recolored, true).as_deref(),
            Some("Updated r.md: 1 updated; left 1 block you edited (or from an older Margin) unchanged")
        );
    }
}
