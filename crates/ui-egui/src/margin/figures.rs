//! Figures on a page for Margin's dark page colours: the parts Night leaves as they are and Ink
//! and Mono keep in colour with "keep figure hues". Decided once per page, in page space, from
//! the page's first whole-page raster, and used for that page's whole-page texture and every
//! tile at any zoom, so a photo is never split by a tile edge or recoloured differently at
//! another zoom.
//!
//! Sources, best first:
//! - The images the page draws itself (`Document::page_images`: image XObjects painted by the
//!   page's own content), by their exact boxes. One counts when its pixels are photo-like
//!   (colourful, or many lightness levels): a black-and-white line drawing or a scan of text flips
//!   with the page instead, so it stays readable.
//! - The raster heuristic ([`pagecolor::detect_figures`]), only where those boxes can't answer:
//!   the page reports no images (inline images, images inside form XObjects and documents the
//!   engine can't inspect all report none) or one image covers most of it (a scanned page). It
//!   finds colourful or many-toned blocks, so it can also take a coloured vector chart. It looks
//!   past the page's text marks (highlights, underlines…): a highlight is colourful too, and
//!   took its paragraph for a figure, a paper-white block round it in Night.
//!
//! Vector drawings on a page that reports images are never figures: Night flips them with their
//! hue kept, Ink and Mono draw them two-tone. Finding them is an engine gap, not guessed here.

use std::collections::{HashMap, HashSet};

use pdfcraft_render::{DocInfo, PageInfo, Tile};

use super::pagecolor;

/// A rectangle as shares of the page as rendered (after `/Rotate`, before Margin's trim):
/// `[u0, v0, u1, v1]` from the top left, the frame of a whole-page raster's pixels.
pub(crate) type Share = [f32; 4];

/// An image covering at least this share of the page is a scanned page, not a figure: the
/// heuristic looks inside it instead.
const SCAN_MIN_AREA: f32 = 0.5;
/// An image narrower or shorter than this share of the page (a bullet, an icon, a rule) is not a
/// figure.
const FIGURE_MIN_SIDE: f32 = 0.04;
/// How far an edge may miss a whole pixel and still count as on it (float error in shares).
const PIXEL_SLACK: f32 = 1e-3;

/// One rendered raster: a whole page, or a tile of it at `scale` device pixels per point.
pub(crate) struct Raster {
    pub page: usize,
    pub tile: Option<Tile>,
    pub scale: f32,
    pub size: [usize; 2],
    pub rgba: pdfcraft_render::Pixels,
}

/// One document view's figures, by page.
#[derive(Default)]
pub(crate) struct Figures {
    /// The document's edit generation the boxes below belong to.
    generation: Option<u64>,
    /// The images each page draws, as shares.
    images: HashMap<usize, Vec<Share>>,
    /// The text marks each page carries, as shares: the raster heuristic looks past them.
    marks: HashMap<usize, Vec<Share>>,
    /// Each decided page's figures, as shares.
    decided: HashMap<usize, Vec<Share>>,
    /// Pages that got tiles before their figures were decided: those tiles are out of date once
    /// they are.
    early_tiles: HashSet<usize>,
}

impl Figures {
    /// Forget everything learnt from an older version of the document.
    pub(crate) fn sync_generation(&mut self, generation: u64) {
        if self.generation != Some(generation) {
            self.generation = Some(generation);
            self.images.clear();
            self.marks.clear();
            self.decided.clear();
        }
    }

    /// Look up the images `page` draws and the text marks it carries, once.
    pub(crate) fn learn_page(&mut self, page: usize, info: &DocInfo, images: impl FnOnce() -> Vec<[f64; 4]>) {
        let Some(page_info) = info.pages.get(page) else { return };
        self.images.entry(page).or_insert_with(|| images().into_iter().filter_map(|rect| user_share(page_info, rect)).collect());
        self.marks.entry(page).or_insert_with(|| mark_rects(info, page).into_iter().filter_map(|rect| user_share(page_info, rect)).collect());
    }

    pub(crate) fn knows_images(&self, page: usize) -> bool {
        self.images.contains_key(&page)
    }

    /// Decide `page`'s figures from its whole-page raster if not done yet. True when tiles were
    /// coloured before this decision and must be rendered again.
    pub(crate) fn decide(&mut self, page: usize, rgba: &[u8], width: usize, height: usize) -> bool {
        if self.decided.contains_key(&page) {
            return false;
        }
        let images = self.images.get(&page).map_or(&[][..], Vec::as_slice);
        let marks = self.marks.get(&page).map_or(&[][..], Vec::as_slice);
        self.decided.insert(page, decide(images, marks, rgba, width, height));
        self.early_tiles.remove(&page)
    }

    pub(crate) fn is_decided(&self, page: usize) -> bool {
        self.decided.contains_key(&page)
    }

    /// `page`'s figures for a tile. Before they are decided a tile is coloured without any, and
    /// remembered so it is replaced once they are.
    pub(crate) fn for_tile(&mut self, page: usize) -> &[Share] {
        if !self.decided.contains_key(&page) {
            self.early_tiles.insert(page);
        }
        self.decided.get(&page).map_or(&[][..], Vec::as_slice)
    }

    pub(crate) fn decided(&self, page: usize) -> &[Share] {
        self.decided.get(&page).map_or(&[][..], Vec::as_slice)
    }
}

/// The page box of a user-space rectangle, as shares; `None` when it is off the page or
/// degenerate.
fn user_share(info: &PageInfo, rect: [f64; 4]) -> Option<Share> {
    let [x0, y0, x1, y1] = rect.map(|v| v as f32);
    if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) || info.width <= 0.0 || info.height <= 0.0 {
        return None;
    }
    let [ax, ay] = info.user_to_view(x0, y0);
    let [bx, by] = info.user_to_view(x1, y1);
    let share = [ax.min(bx) / info.width, ay.min(by) / info.height, ax.max(bx) / info.width, ay.max(by) / info.height].map(|v| v.clamp(0.0, 1.0));
    (share[2] > share[0] && share[3] > share[1]).then_some(share)
}

/// The boxes (user space) of `page`'s text marks: each quadrilateral's, or the mark's
/// rectangle when it has none.
fn mark_rects(info: &DocInfo, page: usize) -> Vec<[f64; 4]> {
    let mut rects = Vec::new();
    for a in info.annotations.iter().filter(|a| a.page == page && super::obsidian::is_markup(a)) {
        if a.quads.is_empty() {
            rects.push(a.rect.map(f64::from));
        }
        rects.extend(a.quads.iter().map(|quad| quad_box(*quad)));
    }
    rects
}

/// The box round a `/QuadPoints` quadrilateral (four x, y corners).
fn quad_box(quad: [f32; 8]) -> [f64; 4] {
    let (xs, ys) = ([quad[0], quad[2], quad[4], quad[6]], [quad[1], quad[3], quad[5], quad[7]]);
    let low = |v: [f32; 4]| v.into_iter().fold(f32::INFINITY, f32::min);
    let high = |v: [f32; 4]| v.into_iter().fold(f32::NEG_INFINITY, f32::max);
    [low(xs), low(ys), high(xs), high(ys)].map(f64::from)
}

fn area(share: Share) -> f32 {
    (share[2] - share[0]) * (share[3] - share[1])
}

/// The figures of a page whose whole-page raster is `rgba` (`width`×`height`), which draws
/// `images` and carries the text marks `marks` (shares).
pub(crate) fn decide(images: &[Share], marks: &[Share], rgba: &[u8], width: usize, height: usize) -> Vec<Share> {
    let frame = Frame::whole(width, height);
    let mut figures: Vec<Share> = images
        .iter()
        .filter(|image| area(**image) < SCAN_MIN_AREA && image[2] - image[0] >= FIGURE_MIN_SIDE && image[3] - image[1] >= FIGURE_MIN_SIDE)
        .filter_map(|image| {
            let rect = frame.pixels(*image).filter(|rect| pagecolor::is_photo_like(rgba, width, *rect))?;
            // Never past the image's own box, which is exact.
            let [x0, y0, x1, y1] = frame.share(pagecolor::inked_bounds(rgba, width, rect)?);
            Some([x0.max(image[0]), y0.max(image[1]), x1.min(image[2]), y1.min(image[3])])
        })
        .collect();
    let scanned = images.iter().any(|image| area(*image) >= SCAN_MIN_AREA);
    if images.is_empty() || scanned {
        let unmarked = without_marks(rgba, width, &frame, marks);
        let pixels = unmarked.as_deref().unwrap_or(rgba);
        figures.extend(pagecolor::detect_figures(pixels, width, height).into_iter().map(|rect| frame.share(rect)));
    }
    figures
}

/// `rgba` with the text marks painted paper white, for the raster heuristic; `None` (no copy)
/// when the page has none.
fn without_marks(rgba: &[u8], width: usize, frame: &Frame, marks: &[Share]) -> Option<Vec<u8>> {
    if marks.is_empty() {
        return None;
    }
    let mut pixels = rgba.to_vec();
    for [x0, y0, x1, y1] in frame.all_pixels(marks) {
        for row in pixels.chunks_exact_mut(width * 4).skip(y0).take(y1 - y0) {
            if let Some(span) = row.get_mut(x0 * 4..x1 * 4) {
                span.fill(u8::MAX);
            }
        }
    }
    Some(pixels)
}

/// Where a raster's pixels lie in the whole page's pixels at the same scale.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame {
    /// The whole page's size in pixels at the raster's scale.
    full: [f32; 2],
    /// The raster's top-left pixel in the whole page.
    origin: [usize; 2],
    /// The raster's size.
    size: [usize; 2],
}

impl Frame {
    pub(crate) fn whole(width: usize, height: usize) -> Self {
        Self { full: [width as f32, height as f32], origin: [0, 0], size: [width, height] }
    }

    /// The frame of `raster`; a tile's whole page is the page's view size at its scale.
    pub(crate) fn of(raster: &Raster, info: Option<&PageInfo>) -> Self {
        let [width, height] = raster.size;
        match (raster.tile, info) {
            (Some(tile), Some(info)) if raster.scale.is_finite() && raster.scale > 0.0 => Self {
                full: [info.width * raster.scale, info.height * raster.scale],
                origin: [tile.x as usize, tile.y as usize],
                size: [width, height],
            },
            _ => Self::whole(width, height),
        }
    }

    /// The pixels of this raster `share` touches, `[x0, y0, x1, y1)`; `None` when none. Edges
    /// are rounded outwards, as the renderer paints an image's edge pixels whole, and in
    /// whole-page pixels, so neighbouring tiles agree on them.
    pub(crate) fn pixels(&self, share: Share) -> Option<[usize; 4]> {
        let edge = |value: f32, full: f32, origin: usize, size: usize, start: bool| {
            let page_px = value * full;
            let page_px = if start { (page_px + PIXEL_SLACK).floor() } else { (page_px - PIXEL_SLACK).ceil() };
            (page_px.max(0.0) as usize).saturating_sub(origin).min(size)
        };
        let x0 = edge(share[0], self.full[0], self.origin[0], self.size[0], true);
        let y0 = edge(share[1], self.full[1], self.origin[1], self.size[1], true);
        let x1 = edge(share[2], self.full[0], self.origin[0], self.size[0], false);
        let y1 = edge(share[3], self.full[1], self.origin[1], self.size[1], false);
        (x1 > x0 && y1 > y0).then_some([x0, y0, x1, y1])
    }

    /// A whole-page raster's pixel rectangle as shares.
    fn share(&self, [x0, y0, x1, y1]: [usize; 4]) -> Share {
        let [width, height] = self.full.map(|v| v.max(1.0));
        [x0 as f32 / width, y0 as f32 / height, x1 as f32 / width, y1 as f32 / height]
    }

    /// Every figure's pixels in this raster, for [`pagecolor::PageColor::apply`].
    pub(crate) fn all_pixels(&self, figures: &[Share]) -> Vec<[usize; 4]> {
        figures.iter().filter_map(|share| self.pixels(*share)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::margin::pagecolor::{PageColor, PageMode};

    const W: usize = 200;
    const H: usize = 260;
    const PHOTO: [usize; 4] = [40, 120, 160, 220];

    /// White paper, black text bars, and in `PHOTO` either a smooth colour gradient (`photo`)
    /// or black-and-white stripes (a line drawing).
    fn page(scale: usize, photo: bool) -> Vec<u8> {
        let (w, h) = (W * scale, H * scale);
        let mut rgba = vec![255u8; w * h * 4];
        for (y, row) in rgba.chunks_exact_mut(w * 4).enumerate() {
            for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let (ux, uy) = (x / scale, y / scale);
                let inside = (PHOTO[0]..PHOTO[2]).contains(&ux) && (PHOTO[1]..PHOTO[3]).contains(&uy);
                let rgb = if inside && photo {
                    [(ux + 60) as u8, 0x20, (255 - uy) as u8]
                } else if inside {
                    if ux % 6 < 2 { [0, 0, 0] } else { [255, 255, 255] }
                } else if (20..100).contains(&uy) && uy % 10 < 3 && (16..184).contains(&ux) {
                    [0, 0, 0]
                } else {
                    [255, 255, 255]
                };
                px[..3].copy_from_slice(&rgb);
            }
        }
        rgba
    }

    fn photo_share() -> Share {
        [PHOTO[0] as f32 / W as f32, PHOTO[1] as f32 / H as f32, PHOTO[2] as f32 / W as f32, PHOTO[3] as f32 / H as f32]
    }

    fn night() -> PageColor {
        PageColor { mode: PageMode::Night, ..PageColor::default() }
    }

    #[test]
    fn an_embedded_photo_is_kept_by_its_exact_box() {
        let figures = decide(&[photo_share()], &[], &page(1, true), W, H);
        assert_eq!(figures.len(), 1);
        let got = Frame::whole(W, H).pixels(figures[0]);
        assert_eq!(got, Some(PHOTO));
    }

    #[test]
    fn a_line_drawing_and_a_scan_flip_with_the_page() {
        assert!(decide(&[photo_share()], &[], &page(1, false), W, H).is_empty(), "two-tone drawing");
        // A page-sized image of text: the heuristic finds nothing photo-like in it.
        let mut scan = page(1, false);
        for px in scan.as_chunks_mut::<4>().0 {
            let rgb: [u8; 3] = if px[0] < 128 { [20, 20, 20] } else { [240, 238, 235] };
            px[..3].copy_from_slice(&rgb);
        }
        assert!(decide(&[[0.0, 0.0, 1.0, 1.0]], &[], &scan, W, H).is_empty(), "scanned text");
    }

    /// The highlighted lines of [`highlighted_text`]: text rows 40..70, across the text width.
    const HIGHLIGHT: [usize; 4] = [16, 40, 184, 70];

    /// A page of text (black bars on white, no images) with highlighter yellow under
    /// [`HIGHLIGHT`]'s lines, the antialiased edges of its letters in between.
    fn highlighted_text() -> Vec<u8> {
        let mut rgba = vec![u8::MAX; W * H * 4];
        for (y, row) in rgba.as_chunks_mut::<{ W * 4 }>().0.iter_mut().enumerate() {
            for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let ink = (20..100).contains(&y) && y % 10 < 3 && (16..184).contains(&x);
                let marked = (HIGHLIGHT[0]..HIGHLIGHT[2]).contains(&x) && (HIGHLIGHT[1]..HIGHLIGHT[3]).contains(&y);
                let edge = (x * 7 + y * 3) % 11;
                let rgb: [u8; 3] = match (ink, marked) {
                    (true, true) => [40 + edge as u8 * 9, 34 + edge as u8 * 8, 0],
                    (true, false) => [0, 0, 0],
                    (false, true) => [0xff, 0xd0, 0x00],
                    (false, false) => [0xff, 0xff, 0xff],
                };
                px[..3].copy_from_slice(&rgb);
            }
        }
        rgba
    }

    #[test]
    fn a_highlight_is_not_a_figure() {
        let page = highlighted_text();
        assert!(!decide(&[], &[], &page, W, H).is_empty(), "unmasked, the heuristic takes the highlighted lines for a figure");
        let mark = [HIGHLIGHT[0] as f32 / W as f32, HIGHLIGHT[1] as f32 / H as f32, HIGHLIGHT[2] as f32 / W as f32, HIGHLIGHT[3] as f32 / H as f32];
        assert_eq!(decide(&[], &[mark], &page, W, H), Vec::<Share>::new(), "the mark's lines flip with the page");
    }

    #[test]
    fn tiles_and_the_whole_page_agree_at_any_zoom() {
        // Decided once from a 1× raster, applied to a 3× page and to a tile of it.
        let figures = decide(&[photo_share()], &[], &page(1, true), W, H);
        let scale = 3;
        let (fw, fh) = (W * scale, H * scale);
        let mut whole = page(scale, true);
        night().apply(&mut whole, fw, fh, &Frame::whole(fw, fh).all_pixels(&figures));
        // A tile crossing the photo's left and top edges.
        let tile = Tile { x: 100, y: 330, w: 150, h: 120 };
        let mut tile_px = Vec::new();
        let source = page(scale, true);
        for row in source.chunks_exact(fw * 4).skip(tile.y as usize).take(tile.h as usize) {
            tile_px.extend_from_slice(&row[tile.x as usize * 4..(tile.x + tile.w) as usize * 4]);
        }
        let raster = Raster {
            page: 0,
            tile: Some(tile),
            scale: scale as f32,
            size: [tile.w as usize, tile.h as usize],
            rgba: pdfcraft_render::Pixels::default(),
        };
        let info = PageInfo { width: W as f32, height: H as f32, label: "1".into(), crop: [0.0, 0.0, W as f32, H as f32], rotation: 0 };
        let frame = Frame::of(&raster, Some(&info));
        night().apply(&mut tile_px, tile.w as usize, tile.h as usize, &frame.all_pixels(&figures));
        for (dy, row) in tile_px.chunks_exact(tile.w as usize * 4).enumerate() {
            let y = tile.y as usize + dy;
            let start = (y * fw + tile.x as usize) * 4;
            assert_eq!(row, &whole[start..start + tile.w as usize * 4], "row {y}");
        }
    }
}
