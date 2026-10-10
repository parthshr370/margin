//! Headless screenshot of the real PdfCraft shell (egui_kittest + wgpu, no window needed).
//!
//! ```text
//! cargo run -p pdfcraft-ui-egui --example shot -- out.png [file.pdf] [--size 1440x900] [--scale 2] [--page 3 --panel pages …]
//! ```
//! `--width N` downscales the image to N pixels wide (README screenshots). `--flavor margin`
//! shows Margin's reader layout (dark theme, like `apps/margin`); `--margin-run ID` then runs a
//! Margin id (`app.stats`, `library.open`…) before the shot; `--click LABEL` and `--hover LABEL`
//! (in the order given, after the runs) click or point at the widget with that accessible label;
//! `--scroll N` (in the same order) turns the wheel at the window's middle to scroll down N points.
//! `--margin-home DIR` gives Margin `DIR/config` and `DIR/data` (a prepared library or stats
//! history) instead of an empty temporary data folder.
//! Any other `--key value` pair is passed to `PdfCraftApp::set_option` (the same verbs as the
//! desktop app's command-line flags and the future control channel).

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let out = args.next().ok_or("usage: shot <out.png> [file.pdf] [--option value …]")?;
    let (mut file, mut opts) = (None, Vec::new());
    let (mut size, mut scale) = (egui::vec2(1440.0, 900.0), 2.0f32);
    let mut width: Option<u32> = None;
    let mut flavor = Flavor::PdfCraft;
    let mut margin_runs = Vec::new();
    let mut pointer = Vec::new();
    let mut select_text: Option<(usize, usize, usize)> = None;
    let mut margin_home: Option<std::path::PathBuf> = None;
    while let Some(a) = args.next() {
        match a.strip_prefix("--") {
            Some("size") => {
                let v = args.next().unwrap_or_default();
                let (w, h) = v.split_once('x').ok_or("--size WxH")?;
                size = egui::vec2(w.parse().map_err(|_| "bad width")?, h.parse().map_err(|_| "bad height")?);
            }
            Some("scale") => scale = args.next().and_then(|v| v.parse().ok()).ok_or("bad --scale")?,
            Some("width") => width = Some(args.next().and_then(|v| v.parse().ok()).ok_or("bad --width")?),
            Some("flavor") => {
                flavor = match args.next().as_deref() {
                    Some("margin") => Flavor::Margin,
                    Some("pdfcraft") => Flavor::PdfCraft,
                    _ => return Err("--flavor pdfcraft|margin".into()),
                }
            }
            Some("margin-run") => margin_runs.push(args.next().ok_or("--margin-run ID")?),
            Some("margin-home") => margin_home = Some(args.next().ok_or("--margin-home DIR")?.into()),
            Some("select-text") => {
                // `--select-text PAGE:FROM-TO`: 1-based page, glyph indices in reading order.
                let v = args.next().unwrap_or_default();
                let parsed = v.split_once(':').and_then(|(page, glyphs)| {
                    let (from, to) = glyphs.split_once('-')?;
                    Some((page.parse::<usize>().ok()?.checked_sub(1)?, from.parse().ok()?, to.parse().ok()?))
                });
                select_text = Some(parsed.ok_or("--select-text PAGE:FROM-TO")?);
            }
            Some(verb @ ("click" | "hover")) => {
                let label = args.next().ok_or("--click/--hover LABEL")?;
                pointer.push(if verb == "click" { Step::Click(label) } else { Step::Hover(label) });
            }
            Some("scroll") => pointer.push(Step::Scroll(args.next().and_then(|v| v.parse().ok()).ok_or("bad --scroll")?)),
            Some(k) => opts.push((k.to_string(), args.next().unwrap_or_default())),
            None => file = Some(a),
        }
    }
    let mut harness = Harness::builder().with_size(size).with_pixels_per_point(scale).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(flavor);
        if flavor == Flavor::Margin {
            app.set_theme_preference(pdfcraft_ui_egui::theme::ThemePreference::Dark);
            // A shot must not touch the reader's data: no library or stats rows, and the file
            // opens without its path, so its sidecar is neither read nor written.
            match &margin_home {
                Some(home) => {
                    app.margin_keys.set_config_dir(home.join("config"));
                    app.margin_keys.set_data_dir(home.join("data"));
                }
                None => app.margin_keys.set_data_dir(std::env::temp_dir().join(format!("margin-shot-data-{}", std::process::id()))),
            }
        }
        if let Some(f) = &file {
            let bytes = std::fs::read(f).unwrap_or_default();
            let name = std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let path = (flavor == Flavor::PdfCraft).then(|| f.clone());
            if let Err(e) = app.open_bytes(&name, path, bytes) {
                eprintln!("shot: {f}: {e}");
            }
        }
        for (k, v) in &opts {
            if let Err(e) = app.set_option(k, v) {
                eprintln!("shot: --{k} {v}: {e}");
            }
        }
        app
    });
    // Let fonts install, layout settle and background renders arrive.
    harness.run_steps(4);
    settle(&mut harness);
    for id in &margin_runs {
        if !pdfcraft_ui_egui::margin::run(harness.state_mut(), id, &serde_json::Value::Null) {
            eprintln!("shot: --margin-run {id}: not a Margin id");
        }
        harness.run_steps(6);
        settle(&mut harness);
    }
    if let Some((page, from, to)) = select_text {
        let app = harness.state_mut();
        let Some(view) = app.active.and_then(|i| app.views.get_mut(i)) else { return Err("--select-text: no document".into()) };
        view.select_text(page, from, to);
        harness.run_steps(6);
        settle(&mut harness);
    }
    for step in &pointer {
        match step {
            Step::Click(label) | Step::Hover(label) => {
                let Some(node) = harness.query_by_label(label) else { return Err(format!("--click/--hover {label}: no such widget")) };
                if matches!(step, Step::Click(_)) {
                    node.click();
                } else {
                    node.hover();
                }
            }
            Step::Scroll(points) => {
                harness.hover_at((size / 2.0).to_pos2());
                harness.event(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -points),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
        harness.run_steps(6);
        // What the click showed may ask for renders (page text for the Highlights tab's quotes).
        settle(&mut harness);
    }
    let mut image = harness.render()?;
    if let Some(w) = width.filter(|w| *w < image.width()) {
        let h = (image.height() as f64 * w as f64 / image.width() as f64).round() as u32;
        image = image::imageops::resize(&image, w, h, image::imageops::FilterType::Lanczos3);
    }
    image.save(&out).map_err(|e| e.to_string())?;
    eprintln!("shot: wrote {out} ({}×{})", image.width(), image.height());
    Ok(())
}

/// A pointer step after the runs, in the order given.
enum Step {
    Click(String),
    Hover(String),
    /// Points to scroll down at the window's middle.
    Scroll(f32),
}

/// Run frames until no render is pending (at most 20 s).
fn settle(harness: &mut Harness<'_, PdfCraftApp>) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        harness.run_steps(2);
        if !harness.state().render_pending() {
            harness.run_steps(3);
            if !harness.state().render_pending() {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}
