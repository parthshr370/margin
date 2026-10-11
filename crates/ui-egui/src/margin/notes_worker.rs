//! Export jobs own an immutable working-document snapshot; the UI only polls their progress.
use super::*;
use pdfcraft_engine::export::ExportSource;

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
    Vault,
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
    pub message: String,
    pub clipboard: Option<String>,
    pub uri: Option<String>,
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
    let view = app.views.get(index).ok_or("No document is open.")?;
    let doc = app.session.get(view.id).ok_or("No document is open.")?;
    let id = doc.id;
    let context = app.notes.cached.get(&id).map(|cached| cached.context.clone()).unwrap_or_else(|| DocNotes::of(&app.notes, doc));
    let annotations = app
        .notes
        .cached
        .get(&id)
        .map(|cached| cached_markup(&doc.info, &cached.indices).cloned().collect())
        .unwrap_or_else(|| markup_comments(&doc.info).into_iter().cloned().collect());
    let input = Input {
        source: doc.export_source(),
        info: DocInfo { pages: doc.info.pages.clone(), annotations, ..DocInfo::default() },
        loaded: view.texts.clone(),
        notes: Notes {
            vault: app.notes.vault.clone(),
            note_template: app.notes.note_template.clone(),
            colors: app.notes.colors.clone(),
            config: app.notes.config.clone(),
            cached: HashMap::new(),
            jobs: HashMap::new(),
            snapshot_to_note: false,
            autoadd: BTreeSet::new(),
            launch_obsidian: false,
            palette_published: false,
            watched: HashMap::new(),
        },
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
        work => {
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
                let page = input.info.pages.get(annotation.page);
                let text = match (extracted.get(&annotation.page), page) {
                    (Some(text), Some(page)) => marked_text(text, page, annotation),
                    _ => String::new(),
                };
                highlights.push(Highlight {
                    key: entry_key(annotation),
                    page: annotation.page,
                    label: page.map(|page| page.label.clone()).unwrap_or_else(|| annotation.page.saturating_add(1).to_string()),
                    color: input.notes.color_label(annotation.color),
                    text,
                    note: annotation.contents.clone().unwrap_or_default(),
                });
                match progress.lock() {
                    Ok(mut state) => state.done = highlights.len(),
                    Err(_) => return Err("The note export worker lost its progress state.".into()),
                }
            }
            match work {
                Work::Copy | Work::ExportHighlights { .. } => {
                    if highlights.is_empty() {
                        return Err("This document has no highlights yet.".into());
                    }
                    let text = export_highlights(&highlights, &input.context, &input.notes)?;
                    if matches!(work, Work::Copy) {
                        Ok(Completed { message: format!("Copied {} highlights", highlights.len()), clipboard: Some(text), uri: None })
                    } else {
                        let Work::ExportHighlights { path } = work else {
                            return Err("The highlight export has no output path.".into());
                        };
                        write_atomically(&path, &text).map_err(|error| format!("Couldn't export highlights: {error}"))?;
                        Ok(Completed {
                            message: format!("Exported {} highlights to {}", highlights.len(), path.display()),
                            clipboard: None,
                            uri: None,
                        })
                    }
                }
                Work::Write { only, open, automatic } => {
                    let entries = highlights
                        .iter()
                        .filter(|highlight| only.as_ref().is_none_or(|keys| keys.contains(&highlight.key)))
                        .map(|highlight| input.context.highlight(&input.notes, highlight).map(|body| (highlight.key.clone(), body)))
                        .collect::<Result<Vec<_>, _>>()?;
                    let (relative, report) = write_entries(&input.notes, &input.context, &entries, only.is_none())?;
                    let message = if !report.preserved.is_empty() {
                        format!(
                            "Updated {} blocks in {relative}; preserved {} user-edited or legacy blocks ({})",
                            report.changed,
                            report.preserved.len(),
                            report.preserved.join(", ")
                        )
                    } else if automatic {
                        format!("Added to {relative}")
                    } else if report.changed == 0 {
                        format!("{relative} already has every highlight")
                    } else {
                        format!("Updated {} highlight blocks in {relative}", report.changed)
                    };
                    let uri = if open {
                        Some(
                            obsidian_uri(input.notes.vault.as_deref().ok_or("No vault is configured.")?, &relative)
                                .ok_or("The vault has no folder name.")?,
                        )
                    } else {
                        None
                    };
                    Ok(Completed { message, clipboard: None, uri })
                }
                _ => Err("The note export job has an invalid operation.".into()),
            }
        }
    }
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
    const MARKER_ALLOWANCE_BYTES: usize = 192;
    let estimate = entries.iter().try_fold(existing.as_ref().map_or(header.len(), String::len), |size, (key, body)| {
        // Both boundaries carry a percent-encoded key (at most three bytes per input byte).
        size.checked_add(key.len().saturating_mul(6))
            .and_then(|size| size.checked_add(body.len()))
            .and_then(|size| size.checked_add(MARKER_ALLOWANCE_BYTES))
            .filter(|size| *size <= maximum)
            .ok_or("The reading note would exceed Margin's 16 MiB safety limit.")
    })?;
    if estimate > maximum {
        return Err("The reading note would exceed Margin's 16 MiB safety limit.".into());
    }
    let (text, report) =
        format::synchronize(existing.as_deref(), &header, entries, full, notes.config.update_mode == NoteUpdateMode::Append, &context.owner);
    if existing.as_deref() != Some(text.as_str()) {
        if read_note(&path)? != existing {
            return Err(format!("{relative} changed in another editor during export. Nothing was overwritten; write again."));
        }
        write_atomically(&path, &text).map_err(|error| format!("Couldn't write {relative}: {error}"))?;
    }
    Ok((relative, report))
}

pub(super) fn export_highlights(highlights: &[Highlight], context: &DocNotes, notes: &Notes) -> Result<String, String> {
    match notes.config.export_target {
        ExportTarget::Obsidian => highlights.iter().map(|highlight| context.highlight(notes, highlight)).collect::<Result<Vec<_>, _>>().map(|blocks| blocks.join("\n")),
        ExportTarget::Markdown | ExportTarget::Logseq => {
            let mut output = String::new();
            for highlight in highlights {
                let link = match &context.link { PdfLink::Vault(path) => format!("[p.{}]({}#page={})", highlight.label, percent_encode(path, true), highlight.page.saturating_add(1)), PdfLink::External(path) => format!("[p.{}]({}#page={})", highlight.label, file_url(path), highlight.page.saturating_add(1)) };
                if notes.config.export_target == ExportTarget::Logseq {
                    output.push_str(&format!("- {} {link}\n", plain_inline(&highlight.text)));
                    if !highlight.note.is_empty() { for line in highlight.note.lines() { output.push_str(&format!("  - {}\n", plain_inline(line))); } }
                } else {
                    output.push_str(&format!("### {link}\n\n> {}\n\n", plain_inline(&highlight.text)));
                    if !highlight.note.is_empty() { for line in highlight.note.lines() { output.push_str(&format!("{}\n", plain_inline(line))); } output.push('\n'); }
                }
            }
            Ok(output)
        }
        ExportTarget::ReadwiseCsv => {
            // https://docs.readwise.io/readwise/docs/importing-highlights (CSV Upload).
            let author = context.variables.get("authors").map(String::as_str).unwrap_or_default();
            let mut output = "Highlight,Book Title,Book Author,Note,Location\r\n".to_string();
            for highlight in highlights {
                output.push_str(&[highlight.text.as_str(), context.title.as_str(), author, highlight.note.as_str(), &highlight.page.saturating_add(1).to_string()].map(csv_field).join(",")); output.push_str("\r\n");
            }
            Ok(output)
        }
        ExportTarget::Json => serde_json::to_string_pretty(&serde_json::json!({ "title": context.title, "authors": context.variables.get("authors"), "highlights": highlights.iter().map(|highlight| serde_json::json!({"id": highlight.key, "page": highlight.page.saturating_add(1), "color": highlight.color, "text": highlight.text, "note": highlight.note, "link": context.link.markdown(highlight.page, &highlight.label)})).collect::<Vec<_>>() })).map_err(|error| error.to_string()),
    }
}
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
fn csv_field(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}
fn target_extension(target: ExportTarget) -> &'static str {
    match target {
        ExportTarget::ReadwiseCsv => "csv",
        ExportTarget::Json => "json",
        _ => "md",
    }
}
pub(super) fn pick_export(app: &mut PdfCraftApp, index: usize, paper: bool) -> Result<(), String> {
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

fn paper(input: &Input, destination: PaperDestination) -> Result<Completed, String> {
    let pages: Vec<usize> = (0..input.source.pages).collect();
    let vault = matches!(destination, PaperDestination::Vault);
    let mut exported = pdfcraft_engine::export::markdown(&input.source, &pages, &input.context.title, vault)?;
    let path = match destination {
        PaperDestination::Vault => {
            let relative = input.context.note.clone()?;
            let paper_relative = format!("{}-paper.md", relative.strip_suffix(".md").unwrap_or(&relative));
            resolve_note(input.notes.vault.as_deref().ok_or("No vault is configured.")?, &paper_relative)?
        }
        PaperDestination::File(path) => path,
    };
    if path.exists() {
        return Err(format!("{} already exists. Move or rename it before exporting; existing notes are never replaced.", path.display()));
    }
    let folder = path.parent().ok_or("The paper export has no destination folder.")?;
    let stem = path.file_stem().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "paper".into());
    let destinations = exported
        .images
        .iter()
        .map(|(name, bytes)| {
            let unique = format!("{stem}-{name}");
            let image_path = if vault {
                let note = input.context.note.clone()?;
                let parent = Path::new(&note)
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .map(|parent| format!("{}/", parent.to_string_lossy()))
                    .unwrap_or_default();
                resolve_asset(input.notes.vault.as_deref().ok_or("No vault is configured.")?, &format!("{parent}{unique}"))?
            } else {
                folder.join(&unique)
            };
            if image_path.exists() {
                return Err(format!("{} already exists; export will not overwrite it.", image_path.display()));
            }
            Ok((name, unique, image_path, bytes))
        })
        .collect::<Result<Vec<_>, String>>()?;
    for (name, unique, image_path, bytes) in destinations {
        super::write_bytes_atomically(&image_path, bytes).map_err(|error| format!("Couldn't write {}: {error}", image_path.display()))?;
        let reference = if vault { format!("![[{name}]]") } else { format!("]({name})") };
        let replacement = if vault { format!("![[{unique}]]") } else { format!("]({unique})") };
        exported.text = exported.text.replace(&reference, &replacement);
    }
    write_atomically(&path, &exported.text).map_err(|error| error.to_string())?;
    Ok(Completed { message: format!("Exported paper to {}", path.display()), clipboard: None, uri: None })
}

fn snapshot(input: &Input, page: usize, rect: [f32; 4]) -> Result<Completed, String> {
    const SNAPSHOT_DPI: f32 = 300.0;
    const MAX_SNAPSHOT_PIXELS: u64 = 64_000_000;
    if page >= input.source.pages || rect.iter().any(|coordinate| !coordinate.is_finite()) {
        return Err("The selected snapshot region is invalid.".into());
    }
    let scale = SNAPSHOT_DPI / 72.0;
    let [left, top, right, bottom] = rect;
    let page_info = input.info.pages.get(page).ok_or("The snapshot page is missing.")?;
    if left < 0.0 || top < 0.0 || right <= left || bottom <= top || right > page_info.width || bottom > page_info.height {
        return Err("Select a nonempty region inside the page.".into());
    }
    let tile = pdfcraft_render::Tile {
        x: (left * scale).floor() as u32,
        y: (top * scale).floor() as u32,
        w: ((right - left) * scale).ceil() as u32,
        h: ((bottom - top) * scale).ceil() as u32,
    };
    if u64::from(tile.w).saturating_mul(u64::from(tile.h)) > MAX_SNAPSHOT_PIXELS {
        return Err("The selected region is too large at 300 DPI.".into());
    }
    let mut renderer = PageRenderer::new(input.source.bytes.clone(), input.source.config.clone());
    let image = renderer.render(RenderRequest { page, kind: RequestKind::Pixels, tile: Some(tile), scale, tag: 0 });
    if let Some(error) = image.error {
        return Err(format!("Couldn't render the snapshot: {error}"));
    }
    let bytes = pdfcraft_engine::export::encode_png(image.width, image.height, &image.rgba)?;
    let vault = input.notes.vault.as_deref().ok_or("Set notes.vault before saving a snapshot.")?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|error| error.to_string())?.as_nanos();
    let name = format!("{}-p{}-{stamp}.png", slug(&input.context.title), page.saturating_add(1));
    let relative = format!("{}/{name}", input.notes.config.snapshot_folder.trim_end_matches('/'));
    let asset = resolve_asset(vault, &relative)?;
    if asset.exists() {
        return Err("The snapshot attachment already exists; nothing was overwritten.".into());
    }
    let note_relative = input.context.note.clone()?;
    let note = resolve_note(vault, &note_relative)?;
    let existing = read_note(&note)?;
    let mut text = existing.clone().unwrap_or(input.context.header(&input.notes)?);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "\n<!-- margin-snapshot:{name} -->\n![[{relative}]]\n{}\n<!-- /margin-snapshot:{name} -->\n",
        input.context.link.markdown(page, &display_text(&input.context.title, &page.saturating_add(1).to_string()))
    ));
    super::write_bytes_atomically(&asset, &bytes).map_err(|error| format!("Couldn't save snapshot: {error}"))?;
    if read_note(&note)? != existing {
        return Err(format!("{note_relative} changed in another editor. Snapshot saved at {relative}, but the note was not overwritten."));
    }
    write_atomically(&note, &text).map_err(|error| format!("Snapshot saved at {relative}, but the note couldn't be updated: {error}"))?;
    Ok(Completed { message: format!("Saved snapshot to {relative} and {note_relative}"), clipboard: None, uri: None })
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
    #[test]
    fn readwise_csv_quotes_commas_newlines_and_quotes() {
        let notes =
            Notes { config: margin_config::settings::Notes { export_target: ExportTarget::ReadwiseCsv, ..Default::default() }, ..Default::default() };
        let context = DocNotes {
            title: "Paper, \"one\"".into(),
            link: PdfLink::Vault("a.pdf".into()),
            note: Ok("a.md".into()),
            variables: BTreeMap::from([("authors".into(), "Author".into())]),
            owner: "a.pdf".into(),
        };
        let highlights = [Highlight {
            key: "id".into(),
            page: 4,
            label: "v".into(),
            color: "yellow".into(),
            text: "a, b\n\"c\"".into(),
            note: "why?\nindeed".into(),
        }];
        assert_eq!(
            export_highlights(&highlights, &context, &notes).unwrap(),
            "Highlight,Book Title,Book Author,Note,Location\r\n\"a, b\n\"\"c\"\"\",\"Paper, \"\"one\"\"\",\"Author\",\"why?\nindeed\",\"5\"\r\n"
        );
    }
}
