//! SyncTeX: the map TeX engines write next to a PDF (`<name>.synctex.gz` or `<name>.synctex`,
//! with `-synctex=1`) between places in the sources and boxes on the pages. Forward search goes
//! from a source line to its boxes, inverse search from a point on a page to a source line.
//!
//! The file is untrusted input: decompressed size, line length, record count, box nesting and
//! every number are bounded, and anything malformed is skipped rather than trusted.
//!
//! Page coordinates here are PDF points (big points) from the top-left corner of the page as TeX
//! laid it out, y down; [`page_to_user`] and [`user_to_page`] map them to PDF user space through
//! the page's crop box (TeX output has the crop box equal to the media box).
//!
//! The format is SyncTeX's documented text format (`synctex(5)`, the `synctex_parser` README):
//! a preamble (`Input:`, `Unit:`, `Magnification:`, offsets), `Content:` with one `{page` …
//! `}page` sheet per page holding box records, and a post scriptum that may override the
//! magnification and offsets.

use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};

use pdfcraft_render::PageInfo;

/// Most bytes read from a (decompressed) SyncTeX file. A long book's file is tens of MB.
pub const MAX_DECOMPRESSED: u64 = 256 * 1024 * 1024;
/// Most records kept; past this the rest of the file is ignored.
pub const MAX_RECORDS: usize = 4_000_000;
/// Most input files listed.
pub const MAX_INPUTS: usize = 100_000;
/// A longer line is skipped (input paths are the longest real lines).
pub const MAX_LINE: usize = 64 * 1024;
/// Box nesting deeper than this hangs further boxes on the deepest kept one.
pub const MAX_DEPTH: usize = 4096;
/// Most boxes a forward search returns.
pub const MAX_HITS: usize = 64;

/// TeX scaled points per PDF point: 65536 sp per TeX point, 72.27 TeX points per 72 PDF points.
const SP_PER_BP: f64 = 65_781.76;
/// `Magnification:` is in thousandths.
const MAGNIFICATION_SCALE: f64 = 1000.0;
/// The height given to a hit that has no box of its own (a glyph-sized mark), in PDF points.
const POINT_HIT_HEIGHT: f32 = 10.0;
const NO_PARENT: u32 = u32::MAX;
const NO_COLUMN: i32 = -1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    VBox,
    HBox,
    VoidVBox,
    VoidHBox,
    Kern,
    Glue,
    Math,
    Current,
}

impl Kind {
    fn is_hbox(self) -> bool {
        matches!(self, Self::HBox | Self::VoidHBox)
    }

    fn has_size(self) -> bool {
        matches!(self, Self::VBox | Self::HBox | Self::VoidVBox | Self::VoidHBox)
    }
}

/// One record, in the file's units (scaled by [`Synctex::unit`] to PDF points).
#[derive(Clone, Copy, Debug)]
struct Record {
    kind: Kind,
    tag: u32,
    line: u32,
    column: i32,
    /// 1-based sheet (page) number.
    page: u32,
    h: i32,
    v: i32,
    width: i32,
    height: i32,
    depth: i32,
    /// Index of the enclosing box record, or [`NO_PARENT`].
    parent: u32,
}

/// A parsed SyncTeX file.
#[derive(Debug, Default)]
pub struct Synctex {
    /// Input tag and the source file's path (relative paths resolved against the SyncTeX file's
    /// folder, which is where TeX ran).
    inputs: Vec<(u32, PathBuf)>,
    records: Vec<Record>,
    /// Each sheet's 1-based page number and its records' index range.
    sheets: Vec<(u32, std::ops::Range<usize>)>,
    /// PDF points per file unit, magnification included.
    unit: f64,
    /// PDF points added to every horizontal and vertical position.
    x_offset: f64,
    y_offset: f64,
}

/// Where forward search lands: the first page showing the line, and its boxes there.
#[derive(Clone, Debug, PartialEq)]
pub struct ForwardHit {
    /// 0-based page.
    pub page: usize,
    /// Boxes in page coordinates (PDF points from the top-left, y down): `[x0, y0, x1, y1]`.
    pub boxes: Vec<[f32; 4]>,
    /// The source line the boxes belong to: the one asked for, or the nearest line that has
    /// boxes.
    pub line: u32,
}

/// Where inverse search lands in the sources.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePlace {
    pub file: PathBuf,
    /// 1-based line.
    pub line: u32,
    /// 1-based column, when the TeX engine recorded one.
    pub column: Option<u32>,
}

/// The parser's preamble and post-scriptum values, before they become [`Synctex`]'s scale.
#[derive(Clone, Copy)]
struct Scale {
    unit: i64,
    magnification: i64,
    x_offset: i64,
    y_offset: i64,
}

impl Default for Scale {
    fn default() -> Self {
        Self { unit: 1, magnification: 1000, x_offset: 0, y_offset: 0 }
    }
}

/// The SyncTeX file of `pdf`: `<stem>.synctex.gz`, else `<stem>.synctex`, beside it.
pub fn file_for(pdf: &Path) -> Option<PathBuf> {
    let stem = pdf.file_stem()?.to_string_lossy().into_owned();
    let dir = pdf.parent().unwrap_or_else(|| Path::new(""));
    [format!("{stem}.synctex.gz"), format!("{stem}.synctex")].into_iter().map(|name| dir.join(name)).find(|path| path.is_file())
}

/// Read the SyncTeX file of `pdf`. `Ok(None)` when it has none.
pub fn load_for(pdf: &Path) -> Result<Option<Synctex>, String> {
    let Some(path) = file_for(pdf) else { return Ok(None) };
    let file = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let base = path.parent().map(Path::to_path_buf);
    Synctex::read(BufReader::new(file), base.as_deref()).map(Some).map_err(|e| format!("{}: {e}", path.display()))
}

/// A page point (PDF points from the top-left, y down) in PDF user space.
pub fn page_to_user(page: &PageInfo, x: f32, y: f32) -> [f32; 2] {
    let [x0, _, _, y1] = page.crop;
    [x0 + x, y1 - y]
}

/// A PDF user-space point as a page point (the inverse of [`page_to_user`]).
pub fn user_to_page(page: &PageInfo, x: f32, y: f32) -> [f32; 2] {
    let [x0, _, _, y1] = page.crop;
    [x - x0, y1 - y]
}

impl Synctex {
    /// Parse a SyncTeX file, gzip-compressed or not. `base` is the folder relative input paths
    /// are resolved against.
    pub fn read(reader: impl BufRead, base: Option<&Path>) -> Result<Self, String> {
        let mut reader = reader;
        let gzip = reader.fill_buf().map_err(|e| e.to_string())?.starts_with(&[0x1f, 0x8b]);
        if gzip { Self::parse(BufReader::new(flate2::bufread::MultiGzDecoder::new(reader)), base) } else { Self::parse(reader, base) }
    }

    /// Parse the uncompressed text format.
    pub fn parse(reader: impl BufRead, base: Option<&Path>) -> Result<Self, String> {
        let mut reader = reader.take(MAX_DECOMPRESSED + 1);
        let mut parser = Parser::default();
        let mut line = Vec::new();
        let mut seen_header = false;
        loop {
            line.clear();
            match read_line(&mut reader, &mut line) {
                Ok(LineRead::End) => break,
                Ok(LineRead::TooLong) => continue,
                Ok(LineRead::Line) => {}
                Err(e) => return Err(format!("reading it failed: {e}")),
            }
            let Ok(text) = std::str::from_utf8(&line) else { continue };
            let text = text.trim_end_matches(['\n', '\r']);
            if !seen_header {
                if !text.starts_with("SyncTeX Version:") {
                    return Err("this is not a SyncTeX file".into());
                }
                seen_header = true;
                continue;
            }
            if !parser.line(text, base) {
                break;
            }
        }
        if reader.limit() == 0 {
            return Err(format!("it is larger than {} MB", MAX_DECOMPRESSED / (1024 * 1024)));
        }
        if !seen_header {
            return Err("this is not a SyncTeX file".into());
        }
        Ok(parser.finish())
    }

    /// The source files the PDF was made from.
    pub fn inputs(&self) -> impl Iterator<Item = &Path> {
        self.inputs.iter().map(|(_, path)| path.as_path())
    }

    fn points(&self, value: i32) -> f32 {
        (f64::from(value) * self.unit) as f32
    }

    fn page_x(&self, h: i32) -> f32 {
        (f64::from(h) * self.unit + self.x_offset) as f32
    }

    fn page_y(&self, v: i32) -> f32 {
        (f64::from(v) * self.unit + self.y_offset) as f32
    }

    /// A record's box in page coordinates; a record without a size is a point.
    fn rect(&self, record: &Record) -> [f32; 4] {
        let (x, y) = (self.page_x(record.h), self.page_y(record.v));
        if record.kind.has_size() {
            [x, y - self.points(record.height), x + self.points(record.width), y + self.points(record.depth)]
        } else if record.kind == Kind::Kern {
            [x, y, x + self.points(record.width), y]
        } else {
            [x, y, x, y]
        }
    }

    /// Forward search: the boxes TeX made from `line` (1-based) of `source`, on the first page
    /// they appear. When `line` made no box (a comment, a blank line), the nearest line that did
    /// is used. `column` (1-based) narrows the boxes when the TeX engine recorded columns.
    pub fn forward(&self, source: &Path, line: u32, column: Option<u32>) -> Option<ForwardHit> {
        let tags = self.tags_for(source);
        if tags.is_empty() {
            return None;
        }
        let ours = || self.records.iter().filter(|record| record.page > 0 && tags.contains(&record.tag));
        let nearest = ours().map(|record| record.line).min_by_key(|candidate| (candidate.abs_diff(line), *candidate < line))?;
        let on_line: Vec<&Record> = ours().filter(|record| record.line == nearest).collect();
        let page = on_line.iter().map(|record| record.page).min()?;
        let mut picked: Vec<&Record> = on_line.into_iter().filter(|record| record.page == page).collect();
        if let Some(wanted) = column.and_then(|c| i32::try_from(c).ok())
            && picked.iter().any(|record| record.column > NO_COLUMN)
        {
            let closest = picked.iter().filter(|record| record.column > NO_COLUMN).map(|record| record.column.abs_diff(wanted)).min();
            picked.retain(|record| record.column > NO_COLUMN && Some(record.column.abs_diff(wanted)) == closest);
        }
        let mut boxes: Vec<[f32; 4]> = Vec::new();
        for record in picked {
            // A glue or kern shows as the line box it sits in.
            let shown = if record.kind.is_hbox() {
                Some(record)
            } else {
                self.records.get(record.parent as usize).filter(|parent| parent.kind.is_hbox()).or(Some(record))
            };
            let Some(shown) = shown else { continue };
            let mut rect = self.rect(shown);
            if rect[3] - rect[1] <= f32::EPSILON {
                rect[1] -= POINT_HIT_HEIGHT;
            }
            if !boxes.contains(&rect) {
                boxes.push(rect);
            }
            if boxes.len() >= MAX_HITS {
                break;
            }
        }
        let page = usize::try_from(page).ok()?.checked_sub(1)?;
        Some(ForwardHit { page, boxes, line: nearest })
    }

    /// Inverse search: the source place of the point `(x, y)` (page coordinates) on `page`
    /// (0-based): the innermost line box under it and, inside it, the nearest mark left of the
    /// point. Outside every line box, the nearest record on the page.
    pub fn inverse(&self, page: usize, x: f32, y: f32) -> Option<SourcePlace> {
        let sheet = u32::try_from(page.checked_add(1)?).ok()?;
        let ranges: Vec<std::ops::Range<usize>> = self.sheets.iter().filter(|(p, _)| *p == sheet).map(|(_, range)| range.clone()).collect();
        let on_page = || ranges.iter().flat_map(|range| range.clone()).filter_map(|index| self.records.get(index).map(|record| (index, record)));
        let contains = |rect: [f32; 4]| rect[0] <= x && x <= rect[2] && rect[1] <= y && y <= rect[3];
        let area = |rect: [f32; 4]| (rect[2] - rect[0]) * (rect[3] - rect[1]);
        let inner = on_page()
            .filter(|(_, record)| record.kind.is_hbox() && contains(self.rect(record)))
            .min_by(|a, b| area(self.rect(a.1)).total_cmp(&area(self.rect(b.1))));
        let record = match inner {
            Some((index, line_box)) => {
                let parent = u32::try_from(index).ok();
                let children = on_page().filter(|(_, record)| Some(record.parent) == parent && record.line > 0);
                let left = children
                    .clone()
                    .filter(|(_, record)| self.page_x(record.h) <= x)
                    .max_by(|a, b| self.page_x(a.1.h).total_cmp(&self.page_x(b.1.h)));
                let closest = || children.min_by(|a, b| (self.page_x(a.1.h) - x).abs().total_cmp(&(self.page_x(b.1.h) - x).abs()));
                left.or_else(closest).map_or(line_box, |(_, record)| record)
            }
            None => {
                let distance = |rect: [f32; 4]| {
                    let dx = (rect[0] - x).max(x - rect[2]).max(0.0);
                    let dy = (rect[1] - y).max(y - rect[3]).max(0.0);
                    dx * dx + dy * dy
                };
                on_page().filter(|(_, record)| record.line > 0).min_by(|a, b| distance(self.rect(a.1)).total_cmp(&distance(self.rect(b.1))))?.1
            }
        };
        let file = self.inputs.iter().find(|(tag, _)| *tag == record.tag)?.1.clone();
        let column = u32::try_from(record.column).ok().filter(|c| *c > 0);
        Some(SourcePlace { file, line: record.line, column })
    }

    /// The input tags naming `source`: the same path, else the inputs whose paths end the same
    /// way for the most components (TeX may record `./chapter.tex`, another absolute root, or a
    /// link's path). Never touches the file system.
    fn tags_for(&self, source: &Path) -> Vec<u32> {
        let wanted = normalize(source);
        let exact: Vec<u32> = self.inputs.iter().filter(|(_, path)| normalize(path) == wanted).map(|(tag, _)| *tag).collect();
        if !exact.is_empty() {
            return exact;
        }
        let wanted_tail = names_from_end(&wanted);
        let shared = |path: &Path| names_from_end(path).iter().zip(&wanted_tail).take_while(|(a, b)| a == b).count();
        let best = self.inputs.iter().map(|(_, path)| shared(path)).max().unwrap_or(0);
        if best == 0 {
            return Vec::new();
        }
        self.inputs.iter().filter(|(_, path)| shared(path) == best).map(|(tag, _)| *tag).collect()
    }
}

/// The names in `path`, last first.
fn names_from_end(path: &Path) -> Vec<&std::ffi::OsStr> {
    path.components()
        .rev()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect()
}

/// `path` with `.` dropped and `..` applied, without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

enum LineRead {
    Line,
    End,
    /// Longer than [`MAX_LINE`]: skipped up to its end.
    TooLong,
}

/// Read one line into `buf`, at most [`MAX_LINE`] bytes of it.
fn read_line(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<LineRead> {
    let limit = u64::try_from(MAX_LINE).unwrap_or(u64::MAX);
    let read = reader.by_ref().take(limit).read_until(b'\n', buf)?;
    if read == 0 {
        return Ok(LineRead::End);
    }
    if buf.ends_with(b"\n") || read < MAX_LINE {
        return Ok(LineRead::Line);
    }
    // Skip the rest of the long line.
    let mut rest = Vec::new();
    loop {
        rest.clear();
        let more = reader.by_ref().take(limit).read_until(b'\n', &mut rest)?;
        if more == 0 || rest.ends_with(b"\n") {
            return Ok(LineRead::TooLong);
        }
    }
}

#[derive(Default)]
struct Parser {
    out: Synctex,
    pre: Scale,
    /// Post-scriptum overrides.
    post_magnification: Option<i64>,
    post_x_offset: Option<i64>,
    post_y_offset: Option<i64>,
    in_content: bool,
    in_post: bool,
    page: u32,
    sheet_start: usize,
    /// Open boxes, innermost last.
    open: Vec<u32>,
    /// Boxes opened past [`MAX_DEPTH`] (their closings pop nothing).
    overflow: usize,
}

impl Parser {
    /// Take one line. `false` stops reading (the record cap was reached).
    fn line(&mut self, text: &str, base: Option<&Path>) -> bool {
        if let Some(rest) = text.strip_prefix("Input:") {
            self.input(rest, base);
            return true;
        }
        if text == "Content:" {
            self.in_content = true;
            return true;
        }
        if text.starts_with("Postamble:") {
            self.end_sheet();
            self.in_content = false;
            return true;
        }
        if text.starts_with("Post scriptum:") {
            self.in_post = true;
            return true;
        }
        if !self.in_content {
            self.setting(text);
            return true;
        }
        let mut chars = text.chars();
        let Some(first) = chars.next() else { return true };
        let rest = chars.as_str();
        match first {
            '{' => {
                self.end_sheet();
                self.page = rest.trim().parse().unwrap_or(0);
                self.sheet_start = self.out.records.len();
            }
            '}' => self.end_sheet(),
            ']' | ')' => {
                if self.overflow > 0 {
                    self.overflow -= 1;
                } else {
                    self.open.pop();
                }
            }
            '[' => return self.record(Kind::VBox, rest, true),
            '(' => return self.record(Kind::HBox, rest, true),
            'v' => return self.record(Kind::VoidVBox, rest, false),
            'h' => return self.record(Kind::VoidHBox, rest, false),
            'k' => return self.record(Kind::Kern, rest, false),
            'g' => return self.record(Kind::Glue, rest, false),
            '$' => return self.record(Kind::Math, rest, false),
            'x' => return self.record(Kind::Current, rest, false),
            // `!` byte offsets, forms and anything newer are not needed for search.
            _ => {}
        }
        true
    }

    fn input(&mut self, rest: &str, base: Option<&Path>) {
        if self.out.inputs.len() >= MAX_INPUTS {
            return;
        }
        let Some((tag, path)) = rest.split_once(':') else { return };
        let Ok(tag) = tag.trim().parse::<u32>() else { return };
        if path.is_empty() {
            return;
        }
        let path = Path::new(path);
        let path = match base {
            Some(base) if path.is_relative() => base.join(path),
            _ => path.to_path_buf(),
        };
        self.out.inputs.retain(|(known, _)| *known != tag);
        self.out.inputs.push((tag, normalize(&path)));
    }

    /// A preamble or post-scriptum `Key:value` line.
    fn setting(&mut self, text: &str) {
        let Some((key, value)) = text.split_once(':') else { return };
        let Ok(value) = value.trim().parse::<i64>() else { return };
        match (key, self.in_post) {
            ("Unit", false) => self.pre.unit = value,
            ("Magnification", false) => self.pre.magnification = value,
            ("X Offset", false) => self.pre.x_offset = value,
            ("Y Offset", false) => self.pre.y_offset = value,
            ("Magnification", true) => self.post_magnification = Some(value),
            ("X Offset", true) => self.post_x_offset = Some(value),
            ("Y Offset", true) => self.post_y_offset = Some(value),
            _ => {}
        }
    }

    fn end_sheet(&mut self) {
        if self.page > 0 && self.out.records.len() > self.sheet_start {
            self.out.sheets.push((self.page, self.sheet_start..self.out.records.len()));
        }
        self.page = 0;
        self.sheet_start = self.out.records.len();
        self.open.clear();
        self.overflow = 0;
    }

    /// A record line after its type character: `tag,line[,column]:h,v[:W[,H,D]]`.
    fn record(&mut self, kind: Kind, rest: &str, opens: bool) -> bool {
        if self.out.records.len() >= MAX_RECORDS {
            return false;
        }
        let parsed = parse_record(kind, rest);
        let index = u32::try_from(self.out.records.len()).ok();
        if let (Some(mut record), Some(index), true) = (parsed, index, self.page > 0) {
            record.page = self.page;
            record.parent = self.open.last().copied().unwrap_or(NO_PARENT);
            self.out.records.push(record);
            if opens {
                if self.open.len() < MAX_DEPTH {
                    self.open.push(index);
                } else {
                    self.overflow = self.overflow.saturating_add(1);
                }
            }
        } else if opens {
            // A malformed box still has a closing line: keep the nesting balanced.
            self.overflow = self.overflow.saturating_add(1);
        }
        true
    }

    fn finish(mut self) -> Synctex {
        self.end_sheet();
        let unit = (if self.pre.unit > 0 { self.pre.unit } else { Scale::default().unit }) as f64;
        let magnification = match self.post_magnification.filter(|m| *m > 0) {
            Some(post) => post as f64 / MAGNIFICATION_SCALE * (self.pre.magnification.max(1) as f64 / MAGNIFICATION_SCALE),
            None if self.pre.magnification > 0 => self.pre.magnification as f64 / MAGNIFICATION_SCALE,
            None => 1.0,
        };
        self.out.unit = unit / SP_PER_BP * magnification;
        self.out.x_offset = self.post_x_offset.map_or(self.pre.x_offset as f64 * unit, |sp| sp as f64) / SP_PER_BP;
        self.out.y_offset = self.post_y_offset.map_or(self.pre.y_offset as f64 * unit, |sp| sp as f64) / SP_PER_BP;
        self.out
    }
}

/// `tag,line[,column]:h,v[:W[,H,D]]`, or `None` when a field is missing or out of range.
fn parse_record(kind: Kind, text: &str) -> Option<Record> {
    let mut parts = text.split(':');
    let mut link = parts.next()?.split(',');
    let tag = link.next()?.trim().parse::<u32>().ok()?;
    let line = link.next()?.trim().parse::<i64>().ok()?;
    let line = u32::try_from(line).unwrap_or(0);
    let column = link.next().and_then(|c| c.trim().parse::<i32>().ok()).unwrap_or(NO_COLUMN);
    let mut place = parts.next()?.split(',');
    let h = place.next()?.trim().parse::<i32>().ok()?;
    let v = place.next()?.trim().parse::<i32>().ok()?;
    let size: Vec<i32> = match parts.next() {
        Some(size) => size.split(',').map(|n| n.trim().parse::<i32>().ok()).collect::<Option<Vec<i32>>>()?,
        None => Vec::new(),
    };
    let (width, height, depth) = match (kind.has_size(), size.as_slice()) {
        (true, [w, h, d, ..]) => (*w, *h, *d),
        (true, _) => return None,
        (false, [w, ..]) if kind == Kind::Kern => (*w, 0, 0),
        (false, _) => (0, 0, 0),
    };
    Some(Record { kind, tag, line, column, page: 0, h, v, width, height, depth, parent: NO_PARENT })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One page, two input files; line 10 of main.tex is a line box with glue at x = 1 in and
    /// 2 in; line 12 is a second line box below it; line 3 of chapter.tex is on page 2.
    /// Unit 1 means scaled points: 4736286 sp = 72 bp (1 in).
    const SAMPLE: &str = "SyncTeX Version:1
Input:1:./main.tex
Input:2:/abs/chapter.tex
Output:pdf
Magnification:1000
Unit:1
X Offset:0
Y Offset:0
Content:
!120
{1
[1,1:4736286,47362860:30000000,40000000,0
(1,10:4736286,9472572:30000000,655360,131072
g1,10:4736286,9472572
g1,11:9472572,9472572
k1,11:14208858,9472572:65536
)
(1,12:4736286,11841430:30000000,655360,131072
x1,12:4736286,11841430
)
]
}1
{2
[2,1:4736286,47362860:30000000,40000000,0
(2,3:4736286,9472572:20000000,655360,0
g2,3:4736286,9472572
)
]
}2
Postamble:
Count:12
Post scriptum:
";

    const BP: f32 = 72.0;

    fn sample() -> Synctex {
        Synctex::parse(SAMPLE.as_bytes(), Some(Path::new("/doc"))).unwrap()
    }

    #[test]
    fn inputs_resolve_against_the_synctex_folder() {
        let s = sample();
        let inputs: Vec<&Path> = s.inputs().collect();
        assert_eq!(inputs, [Path::new("/doc/main.tex"), Path::new("/abs/chapter.tex")]);
    }

    #[test]
    fn forward_finds_the_line_box_in_points() {
        let s = sample();
        let hit = s.forward(Path::new("/doc/main.tex"), 10, None).unwrap();
        assert_eq!((hit.page, hit.line), (0, 10));
        assert_eq!(hit.boxes.len(), 1, "{:?}", hit.boxes);
        let [x0, y0, x1, y1] = hit.boxes[0];
        assert!((x0 - BP).abs() < 0.01, "1 in from the left: {x0}");
        let baseline = 2.0 * BP;
        assert!((y0 - (baseline - 10.0 * 0.9963)).abs() < 0.05, "10 TeX pt above the baseline: {y0}");
        assert!(y1 > baseline && x1 > x0);
    }

    #[test]
    fn forward_uses_the_nearest_line_and_any_path_spelling() {
        let s = sample();
        // Line 11 only has glue and kern inside line 10's box: that box is shown.
        let hit = s.forward(Path::new("/elsewhere/main.tex"), 11, None).unwrap();
        assert_eq!(hit.line, 11);
        assert_eq!(hit.boxes, s.forward(Path::new("/doc/main.tex"), 10, None).unwrap().boxes);
        // Line 40 made no box: the nearest one that did (12).
        assert_eq!(s.forward(Path::new("/doc/./main.tex"), 40, None).unwrap().line, 12);
        let other = s.forward(Path::new("chapter.tex"), 3, None).unwrap();
        assert_eq!(other.page, 1);
        assert!(s.forward(Path::new("/doc/missing.tex"), 1, None).is_none());
    }

    #[test]
    fn inverse_picks_the_mark_left_of_the_point() {
        let s = sample();
        let baseline = 2.0 * BP;
        let place = s.inverse(0, 1.5 * BP, baseline - 2.0).unwrap();
        assert_eq!((place.file.as_path(), place.line, place.column), (Path::new("/doc/main.tex"), 10, None));
        let place = s.inverse(0, 2.5 * BP, baseline - 2.0).unwrap();
        assert_eq!(place.line, 11, "past the glue of line 11");
        let place = s.inverse(0, 1.5 * BP, 2.5 * BP - 2.0).unwrap();
        assert_eq!(place.line, 12);
        // Outside every line box: the nearest record.
        assert_eq!(s.inverse(1, 400.0, 700.0).unwrap().file, Path::new("/abs/chapter.tex"));
        assert!(s.inverse(7, 10.0, 10.0).is_none(), "no such page");
    }

    #[test]
    fn forward_then_inverse_round_trips() {
        let s = sample();
        let hit = s.forward(Path::new("/doc/main.tex"), 12, None).unwrap();
        let [x0, y0, _, y1] = hit.boxes[0];
        let place = s.inverse(hit.page, x0 + 1.0, (y0 + y1) / 2.0).unwrap();
        assert_eq!(place.line, 12);
    }

    #[test]
    fn gzip_files_read_like_plain_ones() {
        use std::io::Write as _;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(SAMPLE.as_bytes()).unwrap();
        let bytes = gz.finish().unwrap();
        let s = Synctex::read(bytes.as_slice(), Some(Path::new("/doc"))).unwrap();
        assert_eq!(s.forward(Path::new("/doc/main.tex"), 10, None), sample().forward(Path::new("/doc/main.tex"), 10, None));
    }

    #[test]
    fn magnification_and_unit_scale_positions() {
        let text = SAMPLE.replace("Magnification:1000", "Magnification:2000").replace("Unit:1", "Unit:2");
        let s = Synctex::parse(text.as_bytes(), Some(Path::new("/doc"))).unwrap();
        let x0 = s.forward(Path::new("/doc/main.tex"), 10, None).unwrap().boxes[0][0];
        assert!((x0 - 4.0 * BP).abs() < 0.05, "{x0}");
    }

    #[test]
    fn hostile_input_is_bounded_and_never_panics() {
        assert!(Synctex::parse(&b"not synctex\n"[..], None).is_err());
        assert!(Synctex::parse(&b""[..], None).is_err());
        let garbage = "SyncTeX Version:1\nInput:x:y\nInput:5:\nContent:\n{1\n(1,2:3\n(\n)\n)\n]\n]\nk1,99999999999:1,2:3\ng-1,2:3,4\n(1,2:3,4:5\n$1,2:3,4\n}1\n{\n\u{0}\u{ff}\n";
        let s = Synctex::parse(garbage.as_bytes(), None).unwrap();
        assert!(s.forward(Path::new("y"), 1, Some(3)).is_none());
        // Non-finite points never panic; the math record's tag names no input.
        assert!(s.inverse(0, f32::NAN, f32::INFINITY).is_none());
        assert!(s.inverse(0, 0.0, 0.0).is_none());
        // Deep nesting, a very long line and a huge page number.
        let mut deep = String::from("SyncTeX Version:1\nInput:1:a.tex\nContent:\n{4294967295\n");
        for _ in 0..MAX_DEPTH + 10 {
            deep.push_str("(1,1:0,0:1,1,1\n");
        }
        deep.push_str(&"x".repeat(MAX_LINE * 2));
        deep.push_str("\ng1,7:0,0\n}4294967295\n");
        let s = Synctex::parse(deep.as_bytes(), None).unwrap();
        assert_eq!(s.forward(Path::new("a.tex"), 7, None).map(|hit| hit.page), Some(4_294_967_294));
    }

    #[test]
    fn page_points_map_through_the_crop_box() {
        let page = PageInfo { width: 600.0, height: 800.0, label: "1".into(), crop: [10.0, 20.0, 610.0, 820.0], rotation: 0 };
        assert_eq!(page_to_user(&page, 72.0, 100.0), [82.0, 720.0]);
        assert_eq!(user_to_page(&page, 82.0, 720.0), [72.0, 100.0]);
    }
}
