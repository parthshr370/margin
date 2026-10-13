//! Margin: a reader-first PDF app. It is PdfCraft's interface with `Flavor::Margin`, which swaps
//! the Acrobat-style chrome for a reader layout (`crates/ui-egui/src/margin`).
//!
//! Usage: `margin [--control FILE] [--forward-search FILE.tex:LINE[:COLUMN]] [--key value…] [files…]`.
//! The `--key value` options are PdfCraft's (`apps/pdfcraft/src/main.rs`); `--control` enables
//! the opt-in UI control channel. `--forward-search` (SyncTeX) shows that source line in the one
//! PDF given; a Margin already running takes it and this launch exits.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use pdfcraft_ui_egui::theme::ThemePreference;
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};

/// PdfCraft's macOS open-documents and quit Apple events, used as is: Finder, Open With and the
/// Dock hand files over this way, not as arguments (packaging/margin/macos/Info.plist.in declares
/// PDF as a document type).
#[cfg(target_os = "macos")]
#[path = "../../pdfcraft/src/apple_events.rs"]
mod apple_events;

/// PdfCraft's logger, used as is: standard error plus a rotated log file in the settings folder.
#[path = "../../pdfcraft/src/logging.rs"]
mod logging;

/// Freedesktop app id (the Wayland app id and X11 WM_CLASS): the name of the desktop file and icon
/// in packaging/margin/linux, so desktops match the window to them.
const APP_ID: &str = "io.github.parthshr370.Margin";

/// The app icon: Margin's logo (contributor-original, see ATTRIBUTION.toml), decoded as
/// PdfCraft decodes its own (`eframe::icon_data`).
const APP_ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

/// The settings key inside `app.ron` (PdfCraftApp saves under it); `app.ron` itself is in
/// Margin's own folder.
const STORAGE_KEY: &str = "pdfcraft";

/// The window's size on first start (eframe remembers it afterwards).
const WINDOW_SIZE: [f32; 2] = [1440.0, 900.0];
const MIN_WINDOW_SIZE: [f32; 2] = [820.0, 520.0];

/// Margin keeps its settings, logs and crash recovery apart from PdfCraft's, so both apps can be
/// installed side by side.
fn settings_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = pdfcraft_ui_egui::portable::data_dir() {
        return Some(dir.join("Margin"));
    }
    eframe::storage_dir("Margin")
}

fn main() -> eframe::Result {
    let logger = logging::install();
    std::panic::set_hook(Box::new(|info| {
        let trace = std::backtrace::Backtrace::capture();
        let report = if trace.status() == std::backtrace::BacktraceStatus::Captured {
            format!("internal error: {info}\n{trace}")
        } else {
            format!("internal error: {info}")
        };
        if log::log_enabled!(log::Level::Error) {
            log::error!("{report}");
        } else {
            // `eprintln!` panics on a broken stderr pipe, and a panic inside the panic hook aborts.
            let _ = std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("margin: {report}\n"));
        }
    }));
    let mut files = Vec::new();
    let mut options: Vec<(String, String)> = Vec::new();
    let mut control_file: Option<String> = None;
    let mut forward_search: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" => {
                println!("margin {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--control" => control_file = args.next(),
            "--forward-search" => forward_search = args.next(),
            flag if flag.starts_with("--") => {
                let value = args.next().unwrap_or_default();
                options.push((flag.trim_start_matches("--").to_string(), value));
            }
            _ => files.push(path_from_arg(arg)),
        }
    }
    let forward = match forward_search.map(|location| forward_request(&location, &files)).transpose() {
        Ok(forward) => forward,
        Err(e) => {
            let _ = std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("margin: {e}\n"));
            std::process::exit(2);
        }
    };
    if let Some(request) = &forward {
        match pdfcraft_ui_egui::margin::synctex::hand_off_to_running(request) {
            Ok(pdfcraft_ui_egui::margin::synctex::HandOff::Sent) => return Ok(()),
            Ok(pdfcraft_ui_egui::margin::synctex::HandOff::NotRunning) => {}
            Err(e) => log::warn!("forward search: handing off to the running Margin failed ({e}); opening here"),
        }
    }
    // macOS: the content runs under a transparent title bar and Margin places the traffic lights
    // in its header row (`margin::window_chrome`).
    let integrated = cfg!(target_os = "macos");
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Margin")
        .with_inner_size(WINDOW_SIZE)
        .with_min_inner_size(MIN_WINDOW_SIZE)
        .with_drag_and_drop(true)
        .with_app_id(APP_ID);
    if integrated {
        viewport = viewport.with_fullsize_content_view(true).with_titlebar_shown(false).with_title_shown(false);
    }
    match eframe::icon_data::from_png_bytes(APP_ICON_PNG) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(e) => log::warn!("app icon: {e}"),
    }
    if let (Some(logger), Some(dir)) = (logger, settings_dir()) {
        match logger.attach_dir(&dir.join("logs")) {
            Ok(path) => log::info!("Margin {}, log file {}", env!("CARGO_PKG_VERSION"), path.display()),
            Err(e) => log::warn!("no log file: {e}"),
        }
    }
    let persistence_path = settings_dir().map(|d| d.join("app.ron"));
    let mut native = eframe::NativeOptions { viewport, persistence_path, ..Default::default() };
    configure_gpu(&mut native);
    // Catch the event that launched us as well as later ones; lives until the event loop returns.
    #[cfg(target_os = "macos")]
    let apple_events = apple_events::AppleEvents::install();
    #[cfg(target_os = "macos")]
    let apple_events = &apple_events;
    eframe::run_native(
        "Margin",
        native,
        Box::new(move |cc| {
            pdfcraft_ui_egui::margin::set_version(env!("CARGO_PKG_VERSION"));
            let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
            // A theme that follows the system asks the desktop portal (the native window only).
            #[cfg(target_os = "linux")]
            app.enable_desktop_theme();
            // Margin's defaults; saved settings override them.
            app.set_theme_preference(ThemePreference::Dark);
            if let Some(json) = cc.storage.and_then(|s| s.get_string(STORAGE_KEY)) {
                app.restore(&json);
            }
            app.integrated_titlebar = integrated;
            #[cfg(target_os = "macos")]
            {
                app.os_events = Some(apple_events.connect(&cc.egui_ctx));
                pdfcraft_ui_egui::margin::menu_bar::install(&mut app, &cc.egui_ctx);
            }
            if let Some(file) = &control_file {
                let client = app.attach_control(&cc.egui_ctx);
                match pdfcraft_ui_egui::control::serve(client).and_then(|ep| write_control_file(file, ep.port, &ep.token).map(|()| ep.port)) {
                    // Never the token (AGENTS.md §3): it stays in the owner-only file.
                    Ok(port) => log::info!("UI control channel on 127.0.0.1:{port} (connection details in {file})"),
                    Err(e) => log::error!("--control {file}: {e}"),
                }
            }
            match pdfcraft_ui_egui::margin::synctex::serve(&mut app, &cc.egui_ctx) {
                Ok(()) => {}
                Err(e) => log::info!("forward search inbox not served: {e}"),
            }
            if let Some(request) = forward {
                app.margin_synctex.forward(request);
            }
            if let Some(dir) = settings_dir() {
                app.enable_recovery(pdfcraft_ui_egui::RecoveryStore::new(dir.join("recovery")));
            }
            app.reopen_last_files(&files);
            for f in files {
                app.open_path(&f);
            }
            for (k, v) in options {
                if let Err(e) = app.set_option(&k, &v) {
                    log::warn!("--{k} {v}: {e}");
                }
            }
            Ok(Box::new(app))
        }),
    )
}

/// `--forward-search`'s request: its `FILE.tex:LINE[:COLUMN]` and the one PDF named.
fn forward_request(location: &str, files: &[String]) -> Result<pdfcraft_ui_egui::margin::synctex::ForwardRequest, String> {
    let [pdf] = files else { return Err("--forward-search needs exactly one PDF: margin --forward-search FILE.tex:LINE paper.pdf".into()) };
    pdfcraft_ui_egui::margin::synctex::ForwardRequest::parse(location, pdf)
}

// The helpers below are copied from `apps/pdfcraft/src/main.rs`, where they are tested; keep them
// in step when upstream changes them.

/// A `file://` URI argument (desktop launchers pass `%U`) as a plain path; anything else as is.
fn path_from_arg(arg: String) -> String {
    let Some(rest) = arg.strip_prefix("file://") else { return arg };
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return arg;
    }
    // `file:///C:/x.pdf` on Windows names `C:/x.pdf`. `rest` starts with the one-byte '/', so
    // byte 1 is a char boundary.
    let rest = if cfg!(windows) && rest.as_bytes().get(2) == Some(&b':') { &rest[1..] } else { rest };
    let mut out = Vec::with_capacity(rest.len());
    let mut bytes = rest.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hex = bytes.clone().take(2).collect::<Vec<u8>>();
            if let Some(byte) = std::str::from_utf8(&hex).ok().filter(|h| h.len() == 2).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                bytes.nth(1);
                continue;
            }
        }
        out.push(b);
    }
    String::from_utf8(out).unwrap_or(arg)
}

/// Write the control endpoint so that only the current user can read the token: a fresh
/// owner-only file beside `path`, renamed over it (never written through a planted link).
fn write_control_file(path: &str, port: u16, token: &str) -> std::io::Result<()> {
    use std::io::{ErrorKind, Write};
    const ATTEMPTS: u32 = 16;
    let json = serde_json::json!({ "port": port, "token": token, "pid": std::process::id() }).to_string();
    let path = std::path::Path::new(path);
    let name = path.file_name().ok_or_else(|| std::io::Error::new(ErrorKind::InvalidInput, "not a file name"))?;
    let random = std::hash::RandomState::new();
    for i in 0..ATTEMPTS {
        let mut staged = std::ffi::OsString::from(".");
        staged.push(name);
        staged.push(format!(".{:016x}.tmp", std::hash::BuildHasher::hash_one(&random, i)));
        let staged = path.with_file_name(staged);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = match opts.open(&staged) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) if e.kind() == ErrorKind::PermissionDenied && staged.symlink_metadata().is_ok() => continue,
            Err(e) => return Err(e),
        };
        let written = {
            let mut file = file;
            file.write_all(json.as_bytes())
        };
        let done = written.and_then(|()| std::fs::rename(&staged, path));
        if done.is_err() {
            let _ = std::fs::remove_file(&staged);
        }
        return done;
    }
    Err(std::io::Error::new(ErrorKind::AlreadyExists, "no free name for the control file's temporary copy"))
}

/// Draw on the integrated GPU unless `WGPU_POWER_PREF` says otherwise, and on Linux on a GPU that
/// drives a monitor (PdfCraft issues #8 and #37).
fn configure_gpu(native: &mut eframe::NativeOptions) {
    let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut native.wgpu_options.wgpu_setup else { return };
    if std::env::var_os("WGPU_POWER_PREF").is_none() {
        setup.power_preference = eframe::wgpu::PowerPreference::LowPower;
        #[cfg(target_os = "linux")]
        {
            let displays = linux_display_gpus(std::path::Path::new("/sys/class/drm"));
            if !displays.is_empty() {
                setup.native_adapter_selector = Some(std::sync::Arc::new(move |adapters, surface| {
                    let usable: Vec<&eframe::wgpu::Adapter> = adapters.iter().filter(|a| surface.is_none_or(|s| a.is_surface_supported(s))).collect();
                    let infos: Vec<(u32, u32, eframe::wgpu::DeviceType)> = usable
                        .iter()
                        .map(|a| {
                            let info = a.get_info();
                            (info.vendor, info.device, info.device_type)
                        })
                        .collect();
                    pick_adapter(&infos, &displays)
                        .and_then(|i| usable.get(i))
                        .map(|a| (*a).clone())
                        .ok_or_else(|| "no GPU can draw to this window".to_string())
                }));
            }
        }
    }
    if cfg!(target_os = "windows") && std::env::var_os("WGPU_BACKEND").is_none() {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12 | eframe::wgpu::Backends::GL;
    }
}

/// PCI `(vendor, device)` ids of the GPUs with a connected monitor, read from the DRM connectors.
#[cfg(target_os = "linux")]
fn linux_display_gpus(drm: &std::path::Path) -> Vec<(u32, u32)> {
    const MAX_CONNECTORS: usize = 256;
    let read_hex = |p: std::path::PathBuf| -> Option<u32> {
        let s = std::fs::read_to_string(p).ok()?;
        u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
    };
    let mut gpus = Vec::new();
    let Ok(entries) = std::fs::read_dir(drm) else { return gpus };
    for entry in entries.flatten().take(MAX_CONNECTORS) {
        let name = entry.file_name();
        let Some((card, _connector)) = name.to_str().and_then(|n| n.split_once('-')) else { continue };
        let connected = std::fs::read_to_string(entry.path().join("status")).is_ok_and(|s| s.trim() == "connected");
        if !connected {
            continue;
        }
        let device = drm.join(card).join("device");
        if let (Some(v), Some(d)) = (read_hex(device.join("vendor")), read_hex(device.join("device")))
            && !gpus.contains(&(v, d))
        {
            gpus.push((v, d));
        }
    }
    gpus
}

/// Index of the adapter to draw with: one that drives a display first, then the most frugal kind.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn pick_adapter(adapters: &[(u32, u32, eframe::wgpu::DeviceType)], displays: &[(u32, u32)]) -> Option<usize> {
    use eframe::wgpu::DeviceType;
    adapters
        .iter()
        .enumerate()
        .min_by_key(|(_, (vendor, device, kind))| {
            let drives_display = displays.contains(&(*vendor, *device));
            let frugality = match kind {
                DeviceType::IntegratedGpu => 0,
                DeviceType::DiscreteGpu => 1,
                DeviceType::Other => 2,
                DeviceType::VirtualGpu => 3,
                DeviceType::Cpu => 4,
            };
            (!drives_display, frugality)
        })
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    #[test]
    fn path_from_arg_decodes_file_uris_and_leaves_paths_alone() {
        assert_eq!(super::path_from_arg("file:///home/alice/my%20report.pdf".to_string()), "/home/alice/my report.pdf");
        assert_eq!(super::path_from_arg("report.pdf".to_string()), "report.pdf");
        assert_eq!(super::path_from_arg("file://server/share/a.pdf".to_string()), "file://server/share/a.pdf");
    }
}
