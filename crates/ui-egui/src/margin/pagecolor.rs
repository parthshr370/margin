//! Page colour (Margin): recolour a rendered page before it becomes a texture.
//!
//! The modes follow the approved mockup (`reader-mockup/index.html`, SVG filters `f-night`,
//! `f-ink`, `f-mono`), so the numbers below are the mockup's filter coefficients:
//! - Paper: the page as rendered.
//! - Sepia: white becomes `#f1e8d0`, ink stays dark (a multiply).
//! - Night: invert, then rotate the hue by 180°, so lightness flips and hue stays (blue boxes stay
//!   blue). Black maps to `#141312`, white to `#ebe6e1`. Figures are left as they are.
//! - Ink: two-tone on luma, `#121212` paper and `#ebe6e1` text.
//! - Mono: black and white with a cut-off (the contrast slider), into the same palette. The cut-off
//!   is a short ramp, not a step, so glyph edges keep their antialiasing.
//!
//! Brightness and contrast apply after the mode, like CSS `brightness()` and `contrast()`. In Ink
//! and Mono, "keep figure hues" draws figures as Night does (unchanged) instead of two-tone.
//! Which parts of a page are figures is decided per page by `margin::figures` and handed to
//! [`PageColor::apply`]; this module supplies its raster tests: blocks found by colourfulness and
//! by how spread out their lightness is (text is two levels, paper and ink; photos are many), see
//! [`detect_figures`] and [`is_photo_like`].
//!
//! Input is the renderer's premultiplied RGBA. Every size comes from the renderer, so nothing
//! here indexes with an unchecked position: rows and pixels are walked with `chunks_exact`.

use serde::{Deserialize, Serialize};

/// The five page colour modes, in the order of the Display popover (keys 1–5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageMode {
    #[default]
    Paper,
    Sepia,
    Night,
    Ink,
    Mono,
}

impl PageMode {
    pub const ALL: [PageMode; 5] = [PageMode::Paper, PageMode::Sepia, PageMode::Night, PageMode::Ink, PageMode::Mono];

    pub fn name(self) -> &'static str {
        match self {
            PageMode::Paper => "Paper",
            PageMode::Sepia => "Sepia",
            PageMode::Night => "Night",
            PageMode::Ink => "Ink",
            PageMode::Mono => "Mono",
        }
    }

    /// One line for the popover, under the swatches.
    pub fn description(self) -> &'static str {
        match self {
            PageMode::Paper => "The page as printed. Brightness and contrast still apply.",
            PageMode::Sepia => "Warm paper for daylight. White becomes #f1e8d0 and ink stays dark.",
            PageMode::Night => "Light flips to dark, hue stays, so blue boxes stay blue. Photos the page embeds keep their colours.",
            PageMode::Ink => "Pure two-tone: #121212 paper, #ebe6e1 text. Color is gone, except figures if kept.",
            PageMode::Mono => "Hard black and white with a cut-off slider. Best for scans and faint print.",
        }
    }

    /// The colour of the page itself (white paper) in this mode: the backdrop drawn before the
    /// raster arrives.
    pub fn paper(self) -> [u8; 3] {
        match self {
            PageMode::Paper => [0xFF, 0xFF, 0xFF],
            PageMode::Sepia => SEPIA_PAPER,
            PageMode::Night => NIGHT_DARK,
            PageMode::Ink => INK_DARK,
            PageMode::Mono => MONO_DARK,
        }
    }

    /// The colour text is drawn in (black ink) in this mode, for the popover swatches.
    pub fn ink(self) -> [u8; 3] {
        match self {
            PageMode::Paper => [0x22, 0x22, 0x22],
            PageMode::Sepia => [0x5B, 0x46, 0x36],
            PageMode::Night | PageMode::Ink | PageMode::Mono => LIGHT_INK,
        }
    }

    /// Light text on a dark page.
    pub fn is_dark(self) -> bool {
        matches!(self, PageMode::Night | PageMode::Ink | PageMode::Mono)
    }

    /// The modes where "keep figure hues" is a choice (Night always keeps photos).
    pub fn offers_keep_hues(self) -> bool {
        matches!(self, PageMode::Ink | PageMode::Mono)
    }
}

/// Slider ranges, in percent (the mockup's `<input type=range>` bounds).
pub const BRIGHTNESS_RANGE: std::ops::RangeInclusive<u16> = 70..=115;
pub const CONTRAST_RANGE: std::ops::RangeInclusive<u16> = 70..=160;
/// Both sliders reset to 100 %.
pub const NEUTRAL_PERCENT: u16 = 100;

const SEPIA_PAPER: [u8; 3] = [0xF1, 0xE8, 0xD0];
/// `#ebe6e1`: the light text of every dark mode (the chrome's text colour).
const LIGHT_INK: [u8; 3] = [0xEB, 0xE6, 0xE1];
const NIGHT_DARK: [u8; 3] = [0x14, 0x13, 0x12];
const INK_DARK: [u8; 3] = [0x12, 0x12, 0x12];
const MONO_DARK: [u8; 3] = [0x0E, 0x0E, 0x0E];

/// Mono cut-off at contrast 70 % and how far it moves up to 160 % (mockup `setMonoThreshold`).
const MONO_CUTOFF_LOW: f32 = 0.30;
const MONO_CUTOFF_SPAN: f32 = 0.45;
/// Half the width of Mono's ramp from light ink to dark paper, in luma. A hard step (the mockup's
/// 32-entry `discrete` table) snaps every antialiased glyph edge to ink or paper, so text looks
/// jagged and hairlines break up; a ramp keeps the edge levels while page and ink stay flat.
const MONO_RAMP_HALF_WIDTH: f32 = 0.25;

/// Rec. 709 luma weights in parts per 10 000 (the weights of SVG `saturate(0)`).
const LUMA_R: u32 = 2126;
const LUMA_G: u32 = 7152;
const LUMA_B: u32 = 722;
const LUMA_SCALE: u32 = 10_000;

/// Hue rotation by 180° (SVG `feColorMatrix type="hueRotate" values="180"`), in parts per 1000.
const HUE_ROTATE_180: [[i32; 3]; 3] = [[-574, 1430, 144], [426, 430, 144], [426, 1430, -856]];
const HUE_ROTATE_SCALE: i32 = 1000;

/// What the user picked: mode, brightness and contrast in percent, and whether Ink and Mono
/// keep figures in colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct PageColor {
    pub mode: PageMode,
    pub brightness: u16,
    pub contrast: u16,
    pub keep_hues: bool,
}

impl Default for PageColor {
    fn default() -> Self {
        Self { mode: PageMode::Paper, brightness: NEUTRAL_PERCENT, contrast: NEUTRAL_PERCENT, keep_hues: true }
    }
}

impl PageColor {
    /// The same choice with every value inside its slider range (settings files are untrusted).
    pub fn clamped(self) -> Self {
        Self {
            brightness: self.brightness.clamp(*BRIGHTNESS_RANGE.start(), *BRIGHTNESS_RANGE.end()),
            contrast: self.contrast.clamp(*CONTRAST_RANGE.start(), *CONTRAST_RANGE.end()),
            ..self
        }
    }

    /// Paper at 100 % brightness and contrast leaves pixels untouched.
    pub fn is_identity(self) -> bool {
        let c = self.clamped();
        c.mode == PageMode::Paper && c.brightness == NEUTRAL_PERCENT && c.contrast == NEUTRAL_PERCENT
    }

    /// Whether figures are drawn apart from the page: always in Night, in Ink and Mono when their
    /// hues are kept.
    pub fn wants_figures(self) -> bool {
        match self.mode {
            PageMode::Night => true,
            PageMode::Ink | PageMode::Mono => self.keep_hues,
            PageMode::Paper | PageMode::Sepia => false,
        }
    }

    /// Identifies the pixels this choice produces: equal keys give equal output, so a texture
    /// recoloured under one key is current for every choice with the same key. 0 is the identity.
    pub fn key(self) -> u64 {
        if self.is_identity() {
            return 0;
        }
        let c = self.clamped();
        let mode = PageMode::ALL.iter().position(|m| *m == c.mode).unwrap_or(0) as u64 + 1;
        let keep = u64::from(c.keep_hues && c.mode.offers_keep_hues());
        mode | (u64::from(c.brightness) << 8) | (u64::from(c.contrast) << 24) | (keep << 40)
    }

    /// Recolour premultiplied RGBA pixels of a `width`×`height` raster in place, leaving the
    /// `figures` (pixel rectangles `[x0, y0, x1, y1)` of this raster) to brightness and contrast
    /// when the mode [wants figures](Self::wants_figures). A buffer whose length doesn't match
    /// the size is left as it is.
    pub fn apply(self, rgba: &mut [u8], width: usize, height: usize, figures: &[[usize; 4]]) {
        let c = self.clamped();
        if c.is_identity() || width == 0 || height == 0 || width.checked_mul(height).and_then(|n| n.checked_mul(4)) != Some(rgba.len()) {
            return;
        }
        let lut = Luts::new(c);
        let figures = if c.wants_figures() { FigureMask::new(figures) } else { FigureMask::default() };
        for_row_bands(rgba, width, |first_row, band| {
            let mut segments = Vec::new();
            for (dy, row) in band.chunks_exact_mut(width * 4).enumerate() {
                figures.row_segments(first_row + dy, width, &mut segments);
                for &(start, end, op) in &segments {
                    let Some(run) = row.get_mut(start * 4..end * 4) else { continue };
                    for px in run.as_chunks_mut::<4>().0 {
                        lut.pixel(px, op);
                    }
                }
            }
        });
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    /// The page's mode.
    Page,
    /// A photo-like region: only brightness and contrast.
    Figure,
}

/// Precomputed per-channel tables, so each pixel costs a few lookups (and a 3×3 product in
/// Night).
struct Luts {
    mode: PageMode,
    /// Brightness then contrast: what Paper does and what figures get.
    tone: [u8; 256],
    /// Per output channel: Sepia from the source channel, Night from the hue-rotated inverse,
    /// Ink and Mono from luma. Brightness and contrast are folded in.
    channel: [[u8; 256]; 3],
}

impl Luts {
    fn new(c: PageColor) -> Self {
        let brightness = f32::from(c.brightness) / 100.0;
        let contrast = f32::from(c.contrast) / 100.0;
        let tone_f = |v: f32| ((v * brightness).clamp(0.0, 1.0) - 0.5) * contrast + 0.5;
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let tone = std::array::from_fn(|i| to_u8(tone_f(i as f32 / 255.0)));
        let mono_cutoff = MONO_CUTOFF_LOW + (contrast - 0.7) / 0.9 * MONO_CUTOFF_SPAN;
        let channel = std::array::from_fn(|ch| {
            std::array::from_fn(|i| {
                let v = i as f32 / 255.0;
                let dark = channel_unit(c.mode.paper(), ch);
                let light = channel_unit(LIGHT_INK, ch);
                match c.mode {
                    PageMode::Paper => to_u8(tone_f(v)),
                    PageMode::Sepia => to_u8(tone_f(v * channel_unit(SEPIA_PAPER, ch))),
                    // `v` is the hue-rotated inverse: 0 → dark paper, 1 → light ink.
                    PageMode::Night => to_u8(tone_f(dark + v * (light - dark))),
                    // `v` is luma: black ink → light, white paper → dark.
                    PageMode::Ink => to_u8(tone_f(light + v * (dark - light))),
                    PageMode::Mono => {
                        // `v` is luma: darker than the ramp → light ink, lighter → dark paper.
                        let paper_share = ((v - mono_cutoff) / (2.0 * MONO_RAMP_HALF_WIDTH) + 0.5).clamp(0.0, 1.0);
                        to_u8(((light + paper_share * (dark - light)) * brightness).clamp(0.0, 1.0))
                    }
                }
            })
        });
        Self { mode: c.mode, tone, channel }
    }

    fn pixel(&self, px: &mut [u8; 4], op: Op) {
        let [r, g, b, a] = *px;
        if a == 0 {
            return;
        }
        let [r, g, b] = unpremultiply([r, g, b], a);
        let out = match (op, self.mode) {
            (Op::Figure, _) | (Op::Page, PageMode::Paper) => [self.tone[r as usize], self.tone[g as usize], self.tone[b as usize]],
            (Op::Page, PageMode::Sepia) => [self.channel[0][r as usize], self.channel[1][g as usize], self.channel[2][b as usize]],
            (Op::Page, PageMode::Night) => {
                let inv = [i32::from(255 - r), i32::from(255 - g), i32::from(255 - b)];
                let rotated = HUE_ROTATE_180.map(|row| {
                    let sum = row[0] * inv[0] + row[1] * inv[1] + row[2] * inv[2];
                    (sum / HUE_ROTATE_SCALE).clamp(0, 255) as usize
                });
                [self.channel[0][rotated[0]], self.channel[1][rotated[1]], self.channel[2][rotated[2]]]
            }
            (Op::Page, PageMode::Ink | PageMode::Mono) => {
                let y = luma(r, g, b) as usize;
                [self.channel[0][y], self.channel[1][y], self.channel[2][y]]
            }
        };
        let [r, g, b] = premultiply(out, a);
        *px = [r, g, b, a];
    }
}

fn channel_unit(rgb: [u8; 3], ch: usize) -> f32 {
    f32::from(rgb.get(ch).copied().unwrap_or(0)) / 255.0
}

fn luma(r: u8, g: u8, b: u8) -> u8 {
    let sum = LUMA_R * u32::from(r) + LUMA_G * u32::from(g) + LUMA_B * u32::from(b);
    (sum / LUMA_SCALE).min(255) as u8
}

fn unpremultiply(rgb: [u8; 3], a: u8) -> [u8; 3] {
    if a == 255 {
        return rgb;
    }
    rgb.map(|c| ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8)
}

fn premultiply(rgb: [u8; 3], a: u8) -> [u8; 3] {
    if a == 255 {
        return rgb;
    }
    rgb.map(|c| ((u32::from(c) * u32::from(a) + 127) / 255) as u8)
}

/// Rasters smaller than this many pixels are recoloured on the calling thread.
const PARALLEL_MIN_PIXELS: usize = 256 * 256;
const MAX_THREADS: usize = 8;

/// Run `f(first_row, rows)` over horizontal bands of the raster, on several threads when it is
/// large. `rgba.len()` is a whole number of rows of `width` pixels.
fn for_row_bands(rgba: &mut [u8], width: usize, f: impl Fn(usize, &mut [u8]) + Sync) {
    let row_bytes = width * 4;
    let rows = rgba.len() / row_bytes.max(1);
    let threads = if cfg!(target_arch = "wasm32") || rows * width < PARALLEL_MIN_PIXELS {
        1
    } else {
        std::thread::available_parallelism().map_or(1, |n| n.get()).min(MAX_THREADS)
    };
    if threads <= 1 {
        f(0, rgba);
        return;
    }
    let band_rows = rows.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (i, band) in rgba.chunks_mut(band_rows * row_bytes).enumerate() {
            let f = &f;
            scope.spawn(move || f(i * band_rows, band));
        }
    });
}

/// Block side in device pixels: about 1/64 of the raster width, within these bounds.
const BLOCK_MIN: usize = 12;
const BLOCK_MAX: usize = 64;
const BLOCKS_ACROSS: usize = 64;
/// A block is colourful when its mean chroma (max − min channel) is at least this.
const FIGURE_MIN_CHROMA: u32 = 64;
/// Lightness histogram bins per block. Text on paper fills two bins (paper and ink) and a little
/// of the rest (antialiasing); a photo spreads out.
const LUMA_BINS: usize = 8;
/// A block is photo-like when more than this share of its pixels lies outside its two fullest
/// lightness bins.
const FIGURE_MIN_SPREAD: f32 = 0.35;
/// A figure is at least this many blocks wide and tall (a highlighted line of text is not).
const FIGURE_MIN_SIDE_BLOCKS: usize = 3;
/// A pixel is blank paper when it is this light and this grey.
const PAPER_MIN_LUMA: u8 = 245;
const PAPER_MAX_CHROMA: u8 = 16;
/// A figure box keeps only the rows and columns that are mostly not blank paper, so the blocks
/// that merely touch a photo (paper, or a line of text beside it) are left out.
const FIGURE_MIN_ROW_INK: f32 = 0.5;

/// Figure regions of a raster, as pixel rectangles `[x0, y0, x1, y1)` sorted by `x0`.
#[derive(Debug, Default)]
struct FigureMask {
    rects: Vec<[usize; 4]>,
}

/// The raster heuristic's figures of a whole-page raster: pixel rectangles `[x0, y0, x1, y1)`.
pub(super) fn detect_figures(rgba: &[u8], width: usize, height: usize) -> Vec<[usize; 4]> {
    FigureMask::detect(rgba, width, height).rects
}

impl FigureMask {
    fn new(rects: &[[usize; 4]]) -> Self {
        let mut rects = rects.to_vec();
        rects.sort_unstable();
        Self { rects }
    }

    /// Split row `y` of a `width`-pixel raster into runs of page and figure pixels.
    fn row_segments(&self, y: usize, width: usize, out: &mut Vec<(usize, usize, Op)>) {
        out.clear();
        let mut x = 0;
        for &[x0, y0, x1, y1] in &self.rects {
            if !(y0..y1).contains(&y) {
                continue;
            }
            let (start, end) = (x0.min(width).max(x), x1.min(width));
            if end <= start {
                continue;
            }
            if start > x {
                out.push((x, start, Op::Page));
            }
            out.push((start, end, Op::Figure));
            x = end;
        }
        if x < width {
            out.push((x, width, Op::Page));
        }
    }

    /// Mark photo-like blocks, then take the bounding box of every large enough connected group
    /// of them (a photo's flat areas, a sky or a wall, still belong to the photo), shrunk to the
    /// pixels that aren't blank paper.
    fn detect(rgba: &[u8], width: usize, height: usize) -> Self {
        let block = (width / BLOCKS_ACROSS).clamp(BLOCK_MIN, BLOCK_MAX);
        let (cols, rows) = (width.div_ceil(block), height.div_ceil(block));
        let seeds = photo_like_blocks(rgba, width, block, cols * rows);
        let mut seen = vec![false; seeds.len()];
        let mut stack = Vec::new();
        let mut rects = Vec::new();
        for start in 0..seeds.len() {
            if !seeds.get(start).copied().unwrap_or(false) || seen.get(start).copied().unwrap_or(true) {
                continue;
            }
            // Bounding box (in blocks) of this 8-connected group of photo-like blocks.
            let (mut c0, mut r0, mut c1, mut r1) = (usize::MAX, usize::MAX, 0, 0);
            let mut count = 0usize;
            stack.push(start);
            if let Some(s) = seen.get_mut(start) {
                *s = true;
            }
            while let Some(i) = stack.pop() {
                let (col, row) = (i % cols, i / cols);
                (c0, r0, c1, r1) = (c0.min(col), r0.min(row), c1.max(col), r1.max(row));
                count += 1;
                for (dc, dr) in NEIGHBOURS {
                    let (Some(nc), Some(nr)) = (col.checked_add_signed(dc), row.checked_add_signed(dr)) else { continue };
                    if nc >= cols || nr >= rows {
                        continue;
                    }
                    let j = nr * cols + nc;
                    if seeds.get(j).copied().unwrap_or(false) && seen.get(j) == Some(&false) {
                        if let Some(s) = seen.get_mut(j) {
                            *s = true;
                        }
                        stack.push(j);
                    }
                }
            }
            let big_enough = c1 + 1 - c0 >= FIGURE_MIN_SIDE_BLOCKS && r1 + 1 - r0 >= FIGURE_MIN_SIDE_BLOCKS && count >= FIGURE_MIN_SIDE_BLOCKS * 2;
            let block_box = [c0 * block, r0 * block, ((c1 + 1) * block).min(width), ((r1 + 1) * block).min(height)];
            if big_enough && let Some(rect) = inked_bounds(rgba, width, block_box) {
                rects.push(rect);
            }
        }
        rects.sort_unstable();
        Self { rects }
    }
}

/// Colour and lightness statistics of some pixels.
#[derive(Clone, Copy, Default)]
struct Tally {
    chroma: u64,
    bins: [u32; LUMA_BINS],
}

impl Tally {
    fn add(&mut self, px: &[u8; 4]) {
        let Some((rgb, alpha)) = opaque_rgb(px) else { return };
        let [r, g, b] = unpremultiply(rgb, alpha);
        self.chroma += u64::from(r.max(g).max(b) - r.min(g).min(b));
        if let Some(bin) = self.bins.get_mut(usize::from(luma(r, g, b)) * LUMA_BINS / 256) {
            *bin += 1;
        }
    }

    /// Colourful, or lightness spread over many levels (a photo, not text on paper).
    fn photo_like(&self) -> bool {
        let pixels: u32 = self.bins.iter().sum();
        if pixels == 0 {
            return false;
        }
        let mut sorted = self.bins;
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        let top_two = sorted.iter().take(2).sum::<u32>();
        let spread = (pixels - top_two) as f32 / pixels as f32;
        self.chroma / u64::from(pixels) >= u64::from(FIGURE_MIN_CHROMA) || spread > FIGURE_MIN_SPREAD
    }
}

/// Per `block`×`block` cell (row-major, `cells` of them): is it colourful or photo-like?
fn photo_like_blocks(rgba: &[u8], width: usize, block: usize, cells: usize) -> Vec<bool> {
    let cols = width.div_ceil(block);
    let mut tallies = vec![Tally::default(); cells];
    for (y, row) in rgba.chunks_exact(width * 4).enumerate() {
        let base = (y / block) * cols;
        for (x, px) in row.as_chunks::<4>().0.iter().enumerate() {
            if let Some(tally) = tallies.get_mut(base + x / block) {
                tally.add(px);
            }
        }
    }
    tallies.iter().map(Tally::photo_like).collect()
}

/// Whether the pixels of `[x0, y0, x1, y1)` in a `width`-pixel raster are, taken together,
/// colourful or photo-like.
pub(super) fn is_photo_like(rgba: &[u8], width: usize, [x0, y0, x1, y1]: [usize; 4]) -> bool {
    let mut tally = Tally::default();
    for row in rgba.chunks_exact(width * 4).skip(y0).take(y1.saturating_sub(y0)) {
        let Some(run) = row.get(x0 * 4..x1 * 4) else { continue };
        for px in run.as_chunks::<4>().0 {
            tally.add(px);
        }
    }
    tally.photo_like()
}

/// Shrink `[x0, y0, x1, y1)` to its first and last rows, then columns, that are mostly not
/// blank paper. `None` when no row is.
pub(super) fn inked_bounds(rgba: &[u8], width: usize, [x0, y0, x1, y1]: [usize; 4]) -> Option<[usize; 4]> {
    let inked = |x: usize, y: usize| {
        let px = rgba.as_chunks::<4>().0.get(y * width + x)?;
        let (rgb, alpha) = opaque_rgb(px)?;
        let [r, g, b] = unpremultiply(rgb, alpha);
        Some(luma(r, g, b) < PAPER_MIN_LUMA || r.max(g).max(b) - r.min(g).min(b) > PAPER_MAX_CHROMA)
    };
    let mostly = |count: usize, of: usize| of > 0 && count as f32 >= of as f32 * FIGURE_MIN_ROW_INK;
    let row_inked = |y: usize| mostly((x0..x1).filter(|&x| inked(x, y) == Some(true)).count(), x1.saturating_sub(x0));
    let top = (y0..y1).find(|&y| row_inked(y))?;
    let bottom = (top..y1).rev().find(|&y| row_inked(y))? + 1;
    let col_inked = |x: usize| mostly((top..bottom).filter(|&y| inked(x, y) == Some(true)).count(), bottom - top);
    let left = (x0..x1).find(|&x| col_inked(x))?;
    let right = (left..x1).rev().find(|&x| col_inked(x))? + 1;
    Some([left, top, right, bottom])
}

/// A pixel's premultiplied colour and alpha, or `None` when it is fully transparent.
fn opaque_rgb(&[r, g, b, a]: &[u8; 4]) -> Option<([u8; 3], u8)> {
    (a > 0).then_some(([r, g, b], a))
}

const NEIGHBOURS: [(isize, isize); 8] = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 240;
    const H: usize = 320;

    /// A synthetic page: white paper, rows of black "text" bars, and a saturated red block
    /// (a photo stand-in) at x 40..136, y 160..256.
    fn page() -> Vec<u8> {
        let mut rgba = vec![255u8; W * H * 4];
        for y in 0..H {
            for x in 0..W {
                let i = (y * W + x) * 4;
                let text = (20..140).contains(&y) && y % 12 < 3 && (16..224).contains(&x);
                let photo = (160..256).contains(&y) && (40..136).contains(&x);
                let rgb = if photo {
                    [0xD0, 0x20, 0x30]
                } else if text {
                    [0, 0, 0]
                } else {
                    [255, 255, 255]
                };
                rgba[i..i + 3].copy_from_slice(&rgb);
            }
        }
        rgba
    }

    fn at(rgba: &[u8], x: usize, y: usize) -> [u8; 3] {
        let i = (y * W + x) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    }

    fn night() -> PageColor {
        PageColor { mode: PageMode::Night, ..PageColor::default() }
    }

    /// `color` with the raster heuristic's figures, as a page without image boxes gets.
    fn apply_detected(color: PageColor, rgba: &mut [u8], width: usize, height: usize) {
        let figures = detect_figures(rgba, width, height);
        color.apply(rgba, width, height, &figures);
    }

    #[test]
    fn paper_at_neutral_is_untouched() {
        let mut rgba = page();
        PageColor::default().apply(&mut rgba, W, H, &[]);
        assert_eq!(rgba, page());
        assert_eq!(PageColor::default().key(), 0);
    }

    #[test]
    fn night_inverts_text_and_keeps_the_saturated_block() {
        let mut rgba = page();
        apply_detected(night(), &mut rgba, W, H);
        assert_eq!(at(&rgba, 4, 4), NIGHT_DARK, "white paper becomes the dark night paper");
        assert_eq!(at(&rgba, 30, 25), LIGHT_INK, "black text becomes light ink");
        assert_eq!(at(&rgba, 88, 208), [0xD0, 0x20, 0x30], "the photo-like block is not inverted");
        assert_eq!(at(&rgba, 38, 200), NIGHT_DARK, "no light frame around the block");
        assert_eq!(at(&rgba, 88, 258), NIGHT_DARK, "no light frame under the block");
    }

    #[test]
    fn night_keeps_hue_of_pale_diagram_boxes() {
        // A pale blue box (#cfe0f3, the mockup's diagram fill) is not photo-like: it flips to a
        // dark box that is still blue.
        let mut rgba = page();
        for px in rgba.as_chunks_mut::<4>().0 {
            if px[..3] == [255, 255, 255] {
                px[..3].copy_from_slice(&[0xCF, 0xE0, 0xF3]);
            }
        }
        apply_detected(night(), &mut rgba, W, H);
        let [r, g, b] = at(&rgba, 4, 4);
        assert!(b > r && b > g && u32::from(r) + u32::from(g) + u32::from(b) < 3 * 128, "dark and blue: {r} {g} {b}");
    }

    #[test]
    fn ink_is_two_tone_unless_figures_are_kept() {
        let mut flat = page();
        apply_detected(PageColor { mode: PageMode::Ink, keep_hues: false, ..PageColor::default() }, &mut flat, W, H);
        let [r, g, b] = at(&flat, 88, 208);
        assert!(r.abs_diff(g) < 12 && g.abs_diff(b) < 12, "the block turned grey: {r} {g} {b}");
        assert_eq!(at(&flat, 4, 4), INK_DARK);

        let mut kept = page();
        apply_detected(PageColor { mode: PageMode::Ink, keep_hues: true, ..PageColor::default() }, &mut kept, W, H);
        assert_eq!(at(&kept, 88, 208), [0xD0, 0x20, 0x30]);
        let [pr, pg, pb] = at(&kept, 4, 4);
        assert!(pr < 0x20 && pg < 0x20 && pb < 0x20, "paper is dark: {pr} {pg} {pb}");
    }

    #[test]
    fn mono_cutoff_moves_with_contrast() {
        let grey = |contrast| {
            let mut px = vec![0x80, 0x80, 0x80, 0xFF];
            PageColor { mode: PageMode::Mono, contrast, keep_hues: false, ..PageColor::default() }.apply(&mut px, 1, 1, &[]);
            px[0]
        };
        // Mid grey is (almost) paper at the lowest cut-off and ink at the highest.
        assert!(grey(70) < 0x40);
        assert!(grey(160) > 0xC0);
    }

    #[test]
    fn mono_keeps_antialiased_edges() {
        // A glyph edge: black ink, three coverage levels, white paper.
        let levels = [0x00u8, 0x60, 0x80, 0xA0, 0xFF];
        let mut row: Vec<u8> = levels.iter().flat_map(|&v| [v, v, v, 0xFF]).collect();
        PageColor { mode: PageMode::Mono, keep_hues: false, ..PageColor::default() }.apply(&mut row, levels.len(), 1, &[]);
        let out: Vec<u8> = row.as_chunks::<4>().0.iter().map(|px| px[0]).collect();
        assert_eq!(out.first(), Some(&LIGHT_INK[0]), "ink is light");
        assert_eq!(out.last(), Some(&MONO_DARK[0]), "paper is dark");
        assert!(out.windows(2).all(|pair| pair[0] > pair[1]), "edge levels stay distinct and ordered: {out:?}");
    }

    #[test]
    fn brightness_and_contrast_apply_to_paper() {
        let mut px = vec![200, 100, 50, 255];
        PageColor { brightness: 80, ..PageColor::default() }.apply(&mut px, 1, 1, &[]);
        assert_eq!(px, vec![160, 80, 40, 255]);
        let mut px = vec![200, 100, 50, 255];
        PageColor { contrast: 150, ..PageColor::default() }.apply(&mut px, 1, 1, &[]);
        assert_eq!(px, vec![236, 86, 11, 255]);
    }

    #[test]
    fn sepia_tints_white_and_keeps_black() {
        let mut px = vec![255, 255, 255, 255, 0, 0, 0, 255];
        PageColor { mode: PageMode::Sepia, ..PageColor::default() }.apply(&mut px, 2, 1, &[]);
        assert_eq!(px, vec![0xF1, 0xE8, 0xD0, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn transparent_and_premultiplied_pixels_survive() {
        let mut px = vec![0, 0, 0, 0, 128, 128, 128, 128];
        night().apply(&mut px, 2, 1, &[]);
        assert_eq!(&px[..4], &[0, 0, 0, 0]);
        // Half-transparent white: night paper at half alpha, still premultiplied.
        assert_eq!(&px[4..], &[10, 10, 9, 128]);
    }

    #[test]
    fn bad_sizes_are_ignored() {
        let mut px = vec![1, 2, 3, 4, 5];
        night().apply(&mut px, 1, 1, &[]);
        assert_eq!(px, vec![1, 2, 3, 4, 5]);
        let mut empty = Vec::new();
        night().apply(&mut empty, 0, 0, &[]);
        night().apply(&mut empty, usize::MAX, 2, &[]);
    }

    #[test]
    fn keys_tell_outputs_apart_and_clamp() {
        let a = night();
        assert_ne!(a.key(), PageColor { brightness: 90, ..a }.key());
        assert_ne!(a.key(), PageColor { mode: PageMode::Ink, ..a }.key());
        // Keep hues only matters in Ink and Mono.
        assert_eq!(a.key(), PageColor { keep_hues: false, ..a }.key());
        assert_eq!(PageColor { brightness: 5, ..a }.clamped().brightness, 70);
        assert_eq!(PageColor { contrast: 999, ..a }.key(), PageColor { contrast: 160, ..a }.key());
    }

    #[test]
    fn large_rasters_split_across_threads_give_the_same_pixels() {
        let (w, h) = (300, 900);
        let mut big = vec![255u8; w * h * 4];
        for (y, row) in big.chunks_exact_mut(w * 4).enumerate() {
            for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let rgb = if (50..150).contains(&x) && (300..500).contains(&y) {
                    [0xD0 - (x % 32) as u8, 0x20, 0x30 + (y % 32) as u8]
                } else if y % 7 < 2 && x % 5 < 2 {
                    [0, 0, 0]
                } else {
                    [255, 255, 255]
                };
                px[..3].copy_from_slice(&rgb);
            }
        }
        let mut serial = big.clone();
        apply_detected(night(), &mut big, w, h);
        let lut = Luts::new(night());
        let mask = FigureMask::detect(&serial, w, h);
        assert_eq!(mask.rects, vec![[50, 300, 150, 500]]);
        let mut segments = Vec::new();
        for (y, row) in serial.chunks_exact_mut(w * 4).enumerate() {
            mask.row_segments(y, w, &mut segments);
            for (start, end, op) in segments.iter().copied() {
                for px in row[start * 4..end * 4].as_chunks_mut::<4>().0 {
                    lut.pixel(px, op);
                }
            }
        }
        assert_eq!(big, serial);
    }
}
