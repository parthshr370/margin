//! Trim: pages shown without their white margins, as a view crop (the PDF's boxes are never
//! changed). Auto trim keeps each page's content box, found on the page's own raster the first
//! time it arrives and cached per page; manual trim cuts fixed margins, odd and even pages
//! separately if asked.
//!
//! Crops are fractions of the page's view box `[u0, v0, u1, v1]` (after `/Rotate`, before the
//! view rotation), the same normalised space `PageXform` maps, so every overlay, hit test and
//! selection stays right on a trimmed page.

use std::collections::BTreeMap;

/// The whole page: nothing trimmed.
pub const FULL: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// A manual margin never takes more than this share of the page from one side.
pub const MAX_MARGIN: f32 = 0.45;

/// A sample darker than this (in any channel) is ink. Anti-aliased page edges and faint scan
/// noise stay under it.
const INK_LEVEL: u8 = 0xF0;
/// Premultiplied pixels this transparent are background, whatever their colour.
const OPAQUE_ENOUGH: u8 = 0x80;
/// Every second row and column is sampled: a box one raster pixel off is invisible.
const SAMPLE_STRIDE: usize = 2;
/// A row or column needs this many ink samples to count, so a lone speck doesn't widen the box.
const MIN_INK_SAMPLES: u32 = 2;
/// Room left around the content, as a share of the page width and height.
const PAD: f32 = 0.012;
/// A content box smaller than this share of the page on either side (a blank or nearly blank
/// page) leaves the page whole.
const MIN_CONTENT: f32 = 0.1;
/// Rasters larger than this many pixels are not scanned (a whole page this big is never
/// requested; the guard keeps a bad request from stalling a frame).
const MAX_SCAN_PIXELS: usize = 64 << 20;

/// Margins cut from each side: `[left, top, right, bottom]`, shares of the page.
pub type Margins = [f32; 4];

/// One document's trim.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trim {
    /// Trimming is on (`view.trim`).
    pub on: bool,
    /// Manual margins instead of each page's content box.
    pub manual: bool,
    /// Odd and even pages keep their own manual margins.
    pub separate: bool,
    /// Manual margins of odd pages (1, 3, …; 0-based even indices), or of every page.
    pub odd: Margins,
    /// Manual margins of even pages when `separate`.
    pub even: Margins,
    /// Content boxes found so far, by 0-based page, split by parity (book scans differ).
    found: [BTreeMap<usize, [f32; 4]>; 2],
    /// The first page whose box was found since [`Self::take_learned`].
    learned: Option<usize>,
}

impl Trim {
    /// The part of `page` shown.
    pub fn crop(&self, page: usize) -> [f32; 4] {
        if !self.on {
            return FULL;
        }
        if self.manual {
            return from_margins(self.margins(page));
        }
        self.content_box(page).unwrap_or(FULL)
    }

    /// The manual margins that apply to `page` (0-based).
    pub fn margins(&self, page: usize) -> Margins {
        if self.separate && page % 2 == 1 { self.even } else { self.odd }
    }

    /// Set the manual margins of `page`'s parity (of every page unless `separate`).
    pub fn set_margins(&mut self, page: usize, margins: Margins) {
        let margins = clamp_margins(margins);
        if self.separate && page % 2 == 1 {
            self.even = margins;
        } else {
            self.odd = margins;
        }
    }

    /// `page`'s content box: its own, else the nearest page's of the same parity, else the
    /// nearest page's, so pages not rendered yet are laid out (and fit) like their neighbours.
    fn content_box(&self, page: usize) -> Option<[f32; 4]> {
        let nearest = |map: &BTreeMap<usize, [f32; 4]>| {
            let below = map.range(..=page).next_back();
            let above = map.range(page..).next();
            match (below, above) {
                (Some(b), Some(a)) => Some(if page - b.0 <= a.0 - page { *b.1 } else { *a.1 }),
                (Some(only), None) | (None, Some(only)) => Some(*only.1),
                (None, None) => None,
            }
        };
        let [same, other] = [&self.found[page % 2], &self.found[1 - page % 2]];
        nearest(same).or_else(|| nearest(other))
    }

    /// A whole-page raster of `page` arrived (`size` pixels, premultiplied RGBA): learn its
    /// content box if auto trim is on and the page has none yet. Tiles and thumbnails don't count.
    pub fn observe(&mut self, page: usize, whole_page: bool, size: [usize; 2], rgba: &[u8]) {
        if !self.on || self.manual || !whole_page || self.found[page % 2].contains_key(&page) {
            return;
        }
        let Some(found) = content_box(size, rgba) else { return };
        self.found[page % 2].insert(page, found);
        self.learned = Some(self.learned.map_or(page, |first| first.min(page)));
    }

    /// Auto trim is on: pages' content boxes are wanted.
    pub fn wants_boxes(&self) -> bool {
        self.on && !self.manual
    }

    /// Whether `page`'s own content box is known.
    pub fn knows(&self, page: usize) -> bool {
        self.found[page % 2].contains_key(&page)
    }

    /// The first page whose box was learned since the last call (its neighbours' layout may
    /// have moved).
    pub fn take_learned(&mut self) -> Option<usize> {
        self.learned.take()
    }

    /// Forget every content box (the pages changed).
    pub fn forget_boxes(&mut self) {
        self.found = Default::default();
    }

    /// `frac`, a share of `page`'s height, as a share of the shown (trimmed) part.
    pub fn shown_fraction(&self, page: usize, frac: f32) -> f32 {
        let [_, v0, _, v1] = self.crop(page);
        ((frac - v0) / (v1 - v0).max(f32::EPSILON)).clamp(0.0, 1.0)
    }

    /// A share of the shown part of `page` as a share of the page's height.
    pub fn page_fraction(&self, page: usize, shown: f32) -> f32 {
        let [_, v0, _, v1] = self.crop(page);
        (v0 + shown.clamp(0.0, 1.0) * (v1 - v0)).clamp(0.0, 1.0)
    }
}

/// The crop that `margins` leave.
pub fn from_margins(margins: Margins) -> [f32; 4] {
    let [left, top, right, bottom] = clamp_margins(margins);
    [left, top, 1.0 - right, 1.0 - bottom]
}

/// The margins `crop` cuts.
pub fn to_margins(crop: [f32; 4]) -> Margins {
    clamp_margins([crop[0], crop[1], 1.0 - crop[2], 1.0 - crop[3]])
}

/// Each margin in `0..=MAX_MARGIN`; NaN becomes 0.
pub fn clamp_margins(margins: Margins) -> Margins {
    margins.map(|m| if m.is_finite() { m.clamp(0.0, MAX_MARGIN) } else { 0.0 })
}

/// The padded box around a raster's ink, as shares of the raster; `None` when the page is
/// (almost) blank or the buffer doesn't match `size`.
pub fn content_box(size: [usize; 2], rgba: &[u8]) -> Option<[f32; 4]> {
    let [w, h] = size;
    let pixels = w.checked_mul(h)?;
    if w == 0 || h == 0 || pixels > MAX_SCAN_PIXELS || rgba.len() < pixels.checked_mul(4)? {
        return None;
    }
    let mut rows = vec![0u32; h];
    let mut cols = vec![0u32; w];
    for y in (0..h).step_by(SAMPLE_STRIDE) {
        let line = rgba.get(y * w * 4..(y + 1) * w * 4)?;
        let (pixels, _) = line.as_chunks::<4>();
        for (x, &[r, g, b, a]) in pixels.iter().enumerate().step_by(SAMPLE_STRIDE) {
            if a >= OPAQUE_ENOUGH && r.min(g).min(b) < INK_LEVEL {
                rows[y] += 1;
                cols[x] += 1;
            }
        }
    }
    let span = |counts: &[u32]| {
        let first = counts.iter().position(|&n| n >= MIN_INK_SAMPLES)?;
        let last = counts.iter().rposition(|&n| n >= MIN_INK_SAMPLES)?;
        Some((first, last + 1))
    };
    let ((x0, x1), (y0, y1)) = (span(&cols)?, span(&rows)?);
    let (fw, fh) = (w as f32, h as f32);
    let found = [x0 as f32 / fw - PAD, y0 as f32 / fh - PAD, x1 as f32 / fw + PAD, y1 as f32 / fh + PAD].map(|v| v.clamp(0.0, 1.0));
    (found[2] - found[0] >= MIN_CONTENT && found[3] - found[1] >= MIN_CONTENT).then_some(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white `w`×`h` raster with black ink in the pixel box `[x0, y0, x1, y1)`.
    fn page(w: usize, h: usize, ink: [usize; 4]) -> Vec<u8> {
        let mut rgba = vec![255u8; w * h * 4];
        for y in ink[1]..ink[3] {
            for x in ink[0]..ink[2] {
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 3].fill(0);
            }
        }
        rgba
    }

    #[test]
    fn the_content_box_is_the_ink_plus_padding() {
        let found = content_box([200, 100], &page(200, 100, [40, 20, 160, 80])).unwrap();
        let expect = [0.2 - PAD, 0.2 - PAD, 0.8 + PAD, 0.8 + PAD];
        for (got, want) in found.iter().zip(expect) {
            assert!((got - want).abs() < 0.02, "{found:?}");
        }
        assert_eq!(content_box([200, 100], &page(200, 100, [0, 0, 0, 0])), None, "blank page");
        assert_eq!(content_box([200, 100], &page(200, 100, [99, 49, 101, 51])), None, "a speck");
        assert_eq!(content_box([200, 100], &[255; 16]), None, "short buffer");
        assert_eq!(content_box([0, 100], &[]), None);
    }

    #[test]
    fn unknown_pages_borrow_the_nearest_box_of_their_parity() {
        let mut trim = Trim { on: true, ..Trim::default() };
        assert_eq!(trim.crop(3), FULL);
        trim.observe(2, true, [100, 100], &page(100, 100, [10, 10, 60, 90]));
        trim.observe(5, true, [100, 100], &page(100, 100, [40, 10, 90, 90]));
        trim.observe(7, false, [100, 100], &page(100, 100, [0, 0, 100, 100]));
        assert_eq!(trim.take_learned(), Some(2));
        assert_eq!(trim.take_learned(), None);
        assert_eq!(trim.crop(0), trim.crop(2), "same parity");
        assert_eq!(trim.crop(9), trim.crop(5));
        assert_ne!(trim.crop(2), trim.crop(5));
        trim.on = false;
        assert_eq!(trim.crop(2), FULL);
    }

    #[test]
    fn manual_margins_by_parity_and_fractions() {
        let mut trim = Trim { on: true, manual: true, ..Trim::default() };
        trim.set_margins(0, [0.1, 0.2, 0.1, 0.2]);
        assert_eq!(trim.crop(1), trim.crop(0), "one set for every page");
        trim.separate = true;
        trim.set_margins(1, [0.3, 0.0, 0.0, 0.9]);
        assert_eq!(trim.even, [0.3, 0.0, 0.0, MAX_MARGIN]);
        assert_eq!(trim.crop(1), [0.3, 0.0, 1.0, 1.0 - MAX_MARGIN]);
        let shown = trim.shown_fraction(0, 0.5);
        assert!((trim.page_fraction(0, shown) - 0.5).abs() < 1e-6);
        assert_eq!(trim.shown_fraction(0, 0.0), 0.0);
        let back = to_margins(from_margins([0.1, 0.2, 0.3, 0.4]));
        assert!(back.iter().zip([0.1, 0.2, 0.3, 0.4]).all(|(got, want)| (got - want).abs() < 1e-6), "{back:?}");
        assert_eq!(clamp_margins([f32::NAN, -1.0, 2.0, 0.2]), [0.0, 0.0, MAX_MARGIN, 0.2]);
    }
}
