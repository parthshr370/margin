//! The library's cover textures. Cached JPEG covers are decoded on a worker thread and uploaded a
//! few per frame, so scrolling a large library never decodes on the UI thread. A cover that can't
//! be read is not asked for again: its book is sent to be scanned once (the scan makes the cover
//! again), and it shows the placeholder until then.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;

use egui::TextureHandle;
use margin_config::library::{self as store, Book, Cover};

/// Decoded covers uploaded as textures per frame.
const UPLOADS_PER_FRAME: usize = 6;
/// Most covers asked of the worker and not back yet (the rows on screen, a few screens' worth).
const MAX_IN_FLIGHT: usize = 48;
/// Most covers kept as textures (those drawn longest ago are dropped first).
const MAX_TEXTURES: usize = 200;

/// A decoded cover.
struct CoverTex {
    texture: TextureHandle,
    /// The frame it was last drawn in.
    used: u64,
}

/// A decoded cover as the worker answers it: `None` when the file couldn't be read.
type Answer = (String, Option<egui::ColorImage>);

/// The decoding thread: asked for (book key, cover file), answers (book key, image).
struct Loader {
    ask: mpsc::Sender<(String, PathBuf)>,
    answers: mpsc::Receiver<Answer>,
}

impl Loader {
    fn spawn(ctx: &egui::Context) -> Option<Self> {
        let (ask, asked) = mpsc::channel::<(String, PathBuf)>();
        let (answer, answers) = mpsc::channel::<Answer>();
        let wake = ctx.clone();
        let spawned = std::thread::Builder::new().name("margin-cover-decode".into()).spawn(move || {
            // Ends when the library drops its sender.
            for (key, path) in asked {
                if answer.send((key, decode(&path))).is_err() {
                    break;
                }
                wake.request_repaint();
            }
        });
        match spawned {
            Ok(_) => Some(Self { ask, answers }),
            Err(err) => {
                log::warn!("library: can't start the cover decoder: {err}");
                None
            }
        }
    }
}

/// The cover at `path` as an image. A cover file that is there but can't be decoded is removed
/// (it is only a cache), so the scan that follows makes it again.
fn decode(path: &std::path::Path) -> Option<egui::ColorImage> {
    let decoded = store::load_cover(path);
    if decoded.is_none()
        && path.is_file()
        && let Err(err) = std::fs::remove_file(path)
    {
        log::warn!("library: can't remove the unreadable cover {}: {err}", path.display());
    }
    let (width, height, rgba) = decoded?;
    let size = [usize::try_from(width).ok()?, usize::try_from(height).ok()?];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, &rgba))
}

/// What the library knows about its covers.
#[derive(Default)]
pub(super) struct Covers {
    textures: HashMap<String, CoverTex>,
    loader: Option<Loader>,
    /// Asked of the worker, not back yet: book key → the book's file (scanned if it fails).
    pending: HashMap<String, String>,
    /// Couldn't be read: not asked for again until the next scan ends.
    failed: HashSet<String>,
    /// Sent to be scanned for a new cover; once per run.
    remade: HashSet<String>,
    /// Files of books whose cover couldn't be read, not yet handed to a scan.
    to_rescan: Vec<String>,
    /// Covers a scan may have rewritten since they were decoded: still drawn, and decoded again
    /// the next time they are drawn, the new texture replacing the old when it lands.
    stale: HashSet<String>,
    /// The frame counter, and the egui pass it counts (the library and the Stats sheet share it).
    frame: u64,
    pass: u64,
    /// Covers uploaded this frame.
    uploaded: usize,
}

impl Covers {
    /// Covers decoded and ready to draw.
    pub(super) fn loaded(&self) -> usize {
        self.textures.len()
    }

    /// A scan ended: covers may have been rewritten, and failed ones made again. The covers on
    /// hand stay on screen until their fresh decode replaces them, so nothing flashes back to the
    /// placeholder.
    pub(super) fn scan_ended(&mut self) {
        self.stale = self.textures.keys().cloned().collect();
        self.failed.clear();
    }

    /// Starts a frame once per egui pass: takes what the worker decoded, within the upload
    /// budget. Returns the files of books whose cover couldn't be read, to scan once.
    pub(super) fn begin_frame(&mut self, ctx: &egui::Context) -> Vec<String> {
        let pass = ctx.cumulative_pass_nr();
        if self.pass == pass {
            return Vec::new();
        }
        self.pass = pass;
        self.frame = self.frame.wrapping_add(1);
        self.uploaded = 0;
        let mut answers = Vec::new();
        let mut died = false;
        if let Some(loader) = &self.loader {
            while answers.len() < UPLOADS_PER_FRAME {
                match loader.answers.try_recv() {
                    Ok(answer) => answers.push(answer),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        died = true;
                        break;
                    }
                }
            }
        }
        if died {
            // The next request starts another worker.
            log::warn!("library: the cover decoder stopped; restarting it");
            self.pending.clear();
            self.loader = None;
        }
        if answers.len() == UPLOADS_PER_FRAME {
            // More may be waiting.
            ctx.request_repaint();
        }
        for (key, image) in answers {
            let file = self.pending.remove(&key);
            match image {
                Some(image) => self.upload(ctx, key, image),
                None => self.unreadable(key, file),
            }
        }
        std::mem::take(&mut self.to_rescan)
    }

    /// The cover of the book with `key` couldn't be read: not asked for again, and its file
    /// (`file`) is scanned once so the scan makes the cover again.
    fn unreadable(&mut self, key: String, file: Option<String>) {
        if self.remade.insert(key.clone())
            && let Some(file) = file
        {
            self.to_rescan.push(file);
        }
        self.failed.insert(key);
    }

    fn upload(&mut self, ctx: &egui::Context, key: String, image: egui::ColorImage) {
        self.uploaded = self.uploaded.saturating_add(1);
        let texture = ctx.load_texture(format!("cover-{key}"), image, egui::TextureOptions::LINEAR);
        self.textures.insert(key, CoverTex { texture, used: self.frame });
    }

    /// The book's cover texture if decoded; else asks for it (decoded on this thread when
    /// `inline`, for tests) and returns `None` for now. A stale cover is returned as it is while
    /// its new decode is asked for.
    pub(super) fn texture(&mut self, ctx: &egui::Context, data: &std::path::Path, book: &Book, inline: bool) -> Option<TextureHandle> {
        let frame = self.frame;
        let current = self.textures.get_mut(&book.book_id).map(|cover| {
            cover.used = frame;
            cover.texture.clone()
        });
        if current.is_some() && !self.stale.contains(&book.book_id) {
            return current;
        }
        if book.cover != Cover::Cached || self.failed.contains(&book.book_id) || self.pending.contains_key(&book.book_id) {
            return current;
        }
        let path = store::cover_path(data, &book.book_id);
        if inline {
            if self.uploaded >= UPLOADS_PER_FRAME {
                ctx.request_repaint();
                return current;
            }
            self.stale.remove(&book.book_id);
            match decode(&path) {
                Some(image) => self.upload(ctx, book.book_id.clone(), image),
                None => self.unreadable(book.book_id.clone(), Some(book.path.clone())),
            }
            return self.textures.get(&book.book_id).map(|cover| cover.texture.clone());
        }
        if self.pending.len() >= MAX_IN_FLIGHT {
            return current;
        }
        if self.loader.is_none() {
            self.loader = Loader::spawn(ctx);
        }
        let Some(loader) = self.loader.as_ref() else { return current };
        if loader.ask.send((book.book_id.clone(), path)).is_ok() {
            self.stale.remove(&book.book_id);
            self.pending.insert(book.book_id.clone(), book.path.clone());
        }
        current
    }

    /// Drops the covers drawn longest ago once there are more than [`MAX_TEXTURES`].
    pub(super) fn trim(&mut self) {
        let Some(excess) = self.textures.len().checked_sub(MAX_TEXTURES).filter(|excess| *excess > 0) else { return };
        let mut ages: Vec<(u64, String)> = self.textures.iter().map(|(key, cover)| (cover.used, key.clone())).collect();
        ages.sort_unstable();
        for (_, key) in ages.into_iter().take(excess) {
            self.textures.remove(&key);
        }
    }
}
