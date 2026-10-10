//! Headless screenshot of the real PdfCraft shell (egui_kittest + wgpu, no window needed).
//!
//! ```text
//! cargo run -p pdfcraft-ui-egui --example shot -- out.png [file.pdf] [--size 1440x900] [--scale 2] [--page 3 --panel pages …]
//! ```
//! `--width N` downscales the image to N pixels wide (README screenshots). `--flavor margin`
//! shows Margin's reader layout (dark theme, like `apps/margin`); `--margin-run ID` then runs a
//! Margin id (`app.stats`, `library.open`…) before the shot; `--click LABEL` and `--hover LABEL`
//! (in the order given, after the runs) click or point at the widget with that accessible label.
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
            Some(verb @ ("click" | "hover")) => pointer.push((verb == "click", args.next().ok_or("--click/--hover LABEL")?)),
            Some(k) => opts.push((k.to_string(), args.next().unwrap_or_default())),
            None => file = Some(a),
        }
    }
    let mut harness = Harness::builder().with_size(size).with_pixels_per_point(scale).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(flavor);
        if flavor == Flavor::Margin {
            app.set_theme_preference(pdfcraft_ui_egui::theme::ThemePreference::Dark);
        }
        if let Some(f) = &file {
            let bytes = std::fs::read(f).unwrap_or_default();
            let name = std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if let Err(e) = app.open_bytes(&name, Some(f.clone()), bytes) {
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
    let start = Instant::now();
    harness.run_steps(4);
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
    for id in &margin_runs {
        if !pdfcraft_ui_egui::margin::run(harness.state_mut(), id, &serde_json::Value::Null) {
            eprintln!("shot: --margin-run {id}: not a Margin id");
        }
        harness.run_steps(6);
    }
    for (click, label) in &pointer {
        let Some(node) = harness.query_by_label(label) else { return Err(format!("--click/--hover {label}: no such widget")) };
        if *click {
            node.click();
        } else {
            node.hover();
        }
        harness.run_steps(6);
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
