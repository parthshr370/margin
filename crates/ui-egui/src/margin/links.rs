//! Positioned navigation and delayed citation previews. One bounded raster band
//! is queued on the document's existing render pool; the frame never renders it.

use egui::{Pos2, Rect, TextureHandle, TextureOptions, vec2};
use pdfcraft_render::{DestView, DocInfo, RenderRequest, RenderedPage, RequestKind, Tile, device_pixels};

use super::look;
use super::pagecolor::{PageColor, detect_figures};
use super::reading::{self, Spot};
use crate::DocView;
use crate::canvas::PREVIEW_TAG;
use crate::theme::Tokens;

/// A destination in this document, as `LinkTarget::Page` carries it: the 0-based page and
/// where on it the view goes.
pub(crate) type Destination = (usize, DestView);

const HOVER_DELAY: f64 = 0.35;
const PREVIEW_WIDTH: f32 = 520.0;
const BAND_HEIGHT_PT: f32 = 260.0;
const MAX_DENSITY: f32 = 3.0;
const EDGE: f32 = 16.0;
const MAX_PREVIEW_SIDE: f32 = 2048.0;
const MIN_ZOOM: f32 = 0.08;
const MAX_ZOOM: f32 = 64.0;

/// Normalised reading coordinates, after both PDF and reader view rotation.
pub(crate) fn destination_spot(info: &DocInfo, (page_index, dest): Destination, rotation: u16) -> Option<Spot> {
    let page = info.pages.get(page_index)?;
    if dest.point() == (None, None) {
        return Some(Spot { page: page_index, y: 0.0, x: None });
    }
    let [x, y] = dest.view_position(page);
    let (x, y) = (x / page.width.max(f32::EPSILON), y / page.height.max(f32::EPSILON));
    let (x, y) = match rotation {
        90 => (1.0 - y, x),
        180 => (1.0 - x, 1.0 - y),
        270 => (y, 1.0 - x),
        _ => (x, y),
    };
    Some(Spot { page: page_index, y: if y.is_finite() { y.clamp(0.0, 1.0) } else { 0.0 }, x: x.is_finite().then(|| x.clamp(0.0, 1.0)) })
}

/// Use the same reading spot and history as Back/Forward, including same-page jumps.
pub(crate) fn jump(view: &mut DocView, info: &DocInfo, destination: Destination) {
    let Some(mut spot) = destination_spot(info, destination, view.rotation) else { return };
    let (page, dest) = destination;
    let origin = view.here();
    view.remember(origin);
    view.show_page(page);
    if let Some(zoom) = dest.zoom().filter(|zoom| zoom.is_finite() && *zoom > 0.0) {
        view.fit = crate::canvas::Fit::None;
        view.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        reading::hold_read_width(view, None);
    }
    // A top-only destination must not pull a wide page sideways.
    if dest.point().0.is_none() {
        spot.x = None;
    }
    reading::go(view, spot);
    view.link_preview.clear();
}

#[derive(Default)]
pub(crate) struct Preview {
    hovered: Option<(usize, Destination)>,
    since: f64,
    anchor: Option<Pos2>,
    request: Option<RenderRequest>,
    source: Option<RenderedPage>,
    texture: Option<TextureHandle>,
    color: Option<PageColor>,
    error: Option<String>,
}

impl Preview {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Returns a missing band request; a failed preview stays failed until the
    /// reader hovers another link (not a per-frame render retry).
    pub(crate) fn update(
        &mut self,
        ctx: &egui::Context,
        info: &DocInfo,
        hover: Option<(usize, Pos2, Destination)>,
        enabled: bool,
        bounds: Rect,
    ) -> Option<RenderRequest> {
        let hover = hover.filter(|_| enabled);
        let target = hover.map(|(index, _, destination)| (index, destination));
        if self.hovered != target {
            self.clear();
            self.hovered = target;
            self.since = ctx.input(|input| input.time);
        }
        self.anchor = hover.map(|(_, anchor, _)| anchor);
        let (_, destination) = self.hovered?;
        let elapsed = ctx.input(|input| input.time) - self.since;
        if elapsed < HOVER_DELAY {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64((HOVER_DELAY - elapsed).max(0.0)));
            return None;
        }
        let request = band_request(info, destination, bounds.width(), ctx.pixels_per_point())?;
        if self.request != Some(request) {
            self.request = Some(request);
            self.source = None;
            self.texture = None;
            self.error = None;
        }
        (self.source.is_none() && self.error.is_none()).then_some(request)
    }

    pub(crate) fn receive(&mut self, raster: RenderedPage) {
        if self.request != Some(raster.request) {
            return;
        }
        self.error = raster.error.clone();
        if self.error.is_none() {
            self.source = Some(raster);
            self.texture = None;
        }
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context, info: &DocInfo, bounds: Rect, color: PageColor, tokens: &Tokens) {
        let (Some((_, destination)), Some(anchor), Some(request)) = (self.hovered, self.anchor, self.request) else { return };
        if let Some(source) = &self.source
            && (self.texture.is_none() || self.color != Some(color))
        {
            let size = [source.width as usize, source.height as usize];
            let mut rgba = source.rgba.to_vec();
            let figures = if color.wants_figures() { detect_figures(&rgba, size[0], size[1]) } else { Vec::new() };
            color.apply(&mut rgba, size[0], size[1], &figures);
            let image = egui::ColorImage::from_rgba_premultiplied(size, &rgba);
            self.texture = Some(ctx.load_texture("margin-link-preview", image, TextureOptions::LINEAR));
            self.color = Some(color);
        }
        let Some(tile) = request.tile else { return };
        let width = PREVIEW_WIDTH.min((bounds.width() - EDGE * 2.0).max(1.0));
        let height = (width * tile.h as f32 / tile.w.max(1) as f32).min((bounds.height() - EDGE * 4.0).max(1.0));
        let size = vec2(width, height);
        let mut position = anchor + vec2(EDGE, EDGE);
        position.x = position.x.min(bounds.right() - width - EDGE).max(bounds.left() + EDGE);
        if position.y + height + EDGE * 3.0 > bounds.bottom() {
            position.y = (anchor.y - height - EDGE * 3.0).max(bounds.top() + EDGE);
        }
        let label = info.pages.get(destination.0).map_or("?", |page| page.label.as_str());
        // No fade or movement animation, including with reduce_motion enabled.
        egui::Area::new(egui::Id::new("margin-link-preview")).order(egui::Order::Tooltip).fixed_pos(position).interactable(false).show(ctx, |ui| {
            look::floating_frame(tokens, look::RADIUS_FLOATING).inner_margin(look::GAP as i8).show(ui, |ui| {
                ui.set_width(width);
                ui.label(egui::RichText::new(format!("Page {label} · click the link to jump")).color(tokens.text_muted));
                if let Some(texture) = &self.texture {
                    ui.add(egui::Image::new((texture.id(), size)));
                } else {
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    let [red, green, blue] = color.mode.paper();
                    ui.painter().rect_filled(rect, 0.0, egui::Color32::from_rgb(red, green, blue));
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        self.error.as_deref().unwrap_or("Rendering preview…"),
                        crate::theme::regular(12.0),
                        tokens.text_muted,
                    );
                }
            });
        });
    }
}

fn band_request(info: &DocInfo, (page_index, dest): Destination, available_width: f32, density: f32) -> Option<RenderRequest> {
    let page = info.pages.get(page_index)?;
    if !page.width.is_finite() || !page.height.is_finite() || page.width <= 0.0 || page.height <= 0.0 {
        return None;
    }
    let width = PREVIEW_WIDTH.min((available_width - EDGE * 2.0).max(1.0));
    let scale = width * density.clamp(1.0, MAX_DENSITY) / page.width;
    let scale = scale.min(MAX_PREVIEW_SIDE / BAND_HEIGHT_PT);
    let y = if dest.point().1.is_some() { dest.view_position(page)[1] } else { 0.0 };
    let top = if y.is_finite() { y.clamp(0.0, (page.height - 1.0).max(0.0)) } else { 0.0 };
    let height = BAND_HEIGHT_PT.min(page.height - top);
    let tile = Tile { x: 0, y: (top * scale).floor() as u32, w: device_pixels(page.width, scale), h: device_pixels(height, scale) };
    Some(RenderRequest { page: page_index, kind: RequestKind::Pixels, tile: Some(tile), scale, tag: PREVIEW_TAG })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfcraft_render::PageInfo;

    fn info() -> DocInfo {
        DocInfo {
            pages: vec![PageInfo { width: 600.0, height: 800.0, crop: [0.0, 0.0, 600.0, 800.0], rotation: 0, label: "1".into() }],
            ..Default::default()
        }
    }

    #[test]
    fn section_tracking_distinguishes_two_starts_on_one_page() {
        let mut info = info();
        info.outline = [700.0, 400.0]
            .into_iter()
            .enumerate()
            .map(|(index, top)| pdfcraft_render::OutlineItem {
                title: format!("Section {index}"),
                page: Some(0),
                view: DestView::FitH { top: Some(top) },
                children: Vec::new(),
                open: false,
            })
            .collect();
        let section = |y| super::super::contents::current_section_at(&info, Spot { page: 0, y, x: None }, 0);
        assert_eq!(section(0.0), None);
        assert_eq!(section(0.125), Some(vec![0]));
        assert_eq!(section(0.49), Some(vec![0]));
        assert_eq!(section(0.5), Some(vec![1]));
    }

    #[test]
    fn bands_start_at_destination_and_stay_bounded() {
        let info = info();
        let at = |top| (0, DestView::FitH { top });
        let request = band_request(&info, at(Some(400.0)), 1200.0, 2.0).unwrap();
        let tile = request.tile.unwrap();
        assert_eq!(tile.y, (400.0 * request.scale).floor() as u32);
        assert!(tile.w <= MAX_PREVIEW_SIDE as u32 && tile.h <= MAX_PREVIEW_SIDE as u32);
        assert_eq!(band_request(&info, at(None), 1200.0, 2.0).unwrap().tile.unwrap().y, 0);
        let edge = band_request(&info, at(Some(-1000.0)), 1200.0, 2.0).unwrap().tile.unwrap();
        assert!(edge.h > 0 && edge.h < tile.h);
        assert_eq!(band_request(&info, (usize::MAX, DestView::FitH { top: Some(400.0) }), 1200.0, 2.0), None);
    }

    #[test]
    fn previews_wait_for_hover_and_honour_the_setting() {
        let ctx = egui::Context::default();
        let info = info();
        let bounds = Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 900.0));
        let destination = (0, DestView::FitH { top: Some(400.0) });
        let hover = Some((0, egui::pos2(100.0, 200.0), destination));
        let mut preview = Preview::default();
        let mut frame = |time, enabled| {
            ctx.begin_pass(egui::RawInput { time: Some(time), ..Default::default() });
            let request = preview.update(&ctx, &info, hover, enabled, bounds);
            ctx.end_pass().textures_delta.clear();
            request
        };
        assert_eq!(frame(0.0, true), None);
        assert_eq!(frame(HOVER_DELAY / 2.0, true), None);
        assert!(frame(HOVER_DELAY, true).is_some());
        assert_eq!(frame(HOVER_DELAY * 2.0, false), None);
        assert!(preview.hovered.is_none() && preview.request.is_none());
    }

    #[test]
    fn positioned_jumps_restore_the_origin_and_zoom_with_back() {
        let info = info();
        let mut view = DocView::new(pdfcraft_engine::DocId(1), &info, crate::canvas::ViewDefaults::default());
        view.margin.history.exact = true;
        reading::go(&mut view, Spot { page: 0, y: 0.2, x: Some(0.5) });
        let origin = view.here();
        jump(&mut view, &info, (0, DestView::Xyz { left: None, top: Some(400.0), zoom: Some(2.0) }));
        assert_eq!(reading::place(&view), (0, 0.5));
        assert_eq!(view.zoom, 2.0);
        assert_eq!(view.back, [origin]);
        view.view_history(false);
        assert_eq!(view.here(), origin);
        view.view_history(true);
        assert_eq!(reading::place(&view), (0, 0.5));
        assert_eq!(view.zoom, 2.0);
    }

    /// Contributor-original, two-column paper generated in memory, not a shipped asset.
    fn paper() -> Vec<u8> {
        let mut first = String::from("BT /F1 20 Tf 40 745 Td (A Two-Column Citation Study) Tj ET\n");
        let mut second = String::from(
            "BT /F1 18 Tf 40 745 Td (1. References and method) Tj ET\nBT /F1 18 Tf 40 420 Td (2. Figure and discussion) Tj ET\n0.15 0.5 0.75 rg 40 250 520 140 re f\nBT /F1 14 Tf 1 1 1 rg 70 320 Td (Figure 1: citation preview target region) Tj ET\n",
        );
        for column in [40, 320] {
            for line in 0..18 {
                let y = 700 - line * 24;
                first.push_str(&format!("BT /F1 11 Tf {column} {y} Td (Reading context remains on this page.) Tj ET\n"));
                second.push_str(&format!("BT /F1 11 Tf 0 g {column} {} Td (Destination content in two columns.) Tj ET\n", 700 - line * 14));
            }
        }
        first.push_str("BT /F1 12 Tf 0.1 0.3 0.8 rg 40 230 Td (See Figure 1 [1]) Tj ET\n");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R /Outlines 9 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 6 0 R /Resources << /Font << /F1 5 0 R >> >> /Annots [8 0 R] >>"
                .to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 7 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{first}endstream", first.len()),
            format!("<< /Length {} >>\nstream\n{second}endstream", second.len()),
            "<< /Type /Annot /Subtype /Link /Rect [38 227 128 244] /Dest [4 0 R /XYZ null 430 null] >>".to_owned(),
            "<< /First 10 0 R /Last 11 0 R /Count 2 >>".to_owned(),
            "<< /Title (References and method) /Parent 9 0 R /Dest [4 0 R /FitH 745] /Next 11 0 R >>".to_owned(),
            "<< /Title (Figure and discussion) /Parent 9 0 R /Dest [4 0 R /FitH 430] /Prev 10 0 R >>".to_owned(),
        ];
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF", objects.len() + 1).as_bytes());
        pdf
    }

    #[test]
    fn hovering_a_paper_citation_renders_a_band_then_click_and_back_work() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::{NodeT, Queryable};
        let mut harness = Harness::builder().with_size(vec2(1400.0, 1000.0)).build_eframe(|_| {
            let mut app = crate::PdfCraftApp::with_flavor(crate::Flavor::Margin);
            app.set_theme_preference(crate::theme::ThemePreference::Dark);
            app.margin_keys.set_data_dir(std::env::temp_dir().join(format!("margin-link-preview-test-{}", std::process::id())));
            app.margin_keys.set_config_dir(std::env::temp_dir().join(format!("margin-link-preview-config-{}", std::process::id())));
            app.open_bytes("citation-study.pdf", None, paper()).unwrap();
            app.set_option("page_color", "paper").unwrap();
            app.views[0].set_fit(crate::canvas::Fit::Page);
            app
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !harness.state().views[0].texts.contains_key(&0) {
            assert!(std::time::Instant::now() < deadline, "page text did not arrive");
            harness.run_steps(3);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let view = &harness.state().views[0];
        let glyph = view.texts.get(&0).unwrap().find("[1]")[0].start;
        let citation = view.glyph_screen_pos(0, glyph).unwrap();
        harness.hover_at(citation);
        harness.run_steps(30);
        while harness.state().views[0].link_preview.texture.is_none() {
            assert!(std::time::Instant::now() < deadline, "citation preview did not arrive");
            harness.run_steps(3);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let preview = &harness.state().views[0].link_preview;
        assert_eq!(preview.source.as_ref().unwrap().request.page, 1);
        assert!(preview.source.as_ref().unwrap().height > 100);
        assert_eq!(harness.state().views[0].current, 0, "hover must not navigate");
        if let Ok(directory) = std::env::var("MARGIN_LINK_SHOTS") {
            std::fs::create_dir_all(&directory).unwrap();
            harness.render().unwrap().save(std::path::Path::new(&directory).join("citation-hover-paper.png")).unwrap();
            harness.state_mut().set_option("page_color", "night").unwrap();
            harness.run_steps(6);
            harness.render().unwrap().save(std::path::Path::new(&directory).join("citation-hover-night.png")).unwrap();
        }
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton {
                pos: citation,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run_steps(1);
        }
        harness.run_steps(6);
        assert_eq!(harness.state().views[0].current, 1);
        let info = &harness.state().session.get(harness.state().views[0].id).unwrap().info;
        let spot = reading::spot(&harness.state().views[0]);
        assert_eq!(super::super::contents::current_section_at(info, spot, 0), Some(vec![1]));
        assert_eq!(harness.state().views[0].back.len(), 1);
        harness.state_mut().views[0].view_history(false);
        harness.run_steps(6);
        assert_eq!(harness.state().views[0].current, 0);
        harness.get_by_label("References and method").click();
        harness.run_steps(6);
        let spot = reading::spot(&harness.state().views[0]);
        let info = &harness.state().session.get(harness.state().views[0].id).unwrap().info;
        assert_eq!(super::super::contents::current_section_at(info, spot, 0), Some(vec![0]));
        assert_eq!(harness.get_by_label("References and method").accesskit_node().toggled(), Some(egui::accesskit::Toggled::True));
        assert_eq!(harness.get_by_label("Figure and discussion").accesskit_node().toggled(), Some(egui::accesskit::Toggled::False));
        harness.get_by_label("Figure and discussion").click();
        harness.run_steps(6);
        let spot = reading::spot(&harness.state().views[0]);
        assert_eq!(spot.page, 1);
        assert!((spot.y - 370.0 / 800.0).abs() < 0.02);
        assert_eq!(harness.get_by_label("Figure and discussion").accesskit_node().toggled(), Some(egui::accesskit::Toggled::True));
        if let Ok(directory) = std::env::var("MARGIN_LINK_SHOTS") {
            harness.render().unwrap().save(std::path::Path::new(&directory).join("contents-second-section-same-page.png")).unwrap();
        }
    }
}
