//! Margin's theme and Display popover: `Flavor::Margin` wears the Margin palette, the popover
//! drives the document's page colour, layout and trim, and PdfCraft is unchanged.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::canvas::PageLayout;
use pdfcraft_ui_egui::margin::display::{self, DisplayState};
use pdfcraft_ui_egui::margin::pagecolor::PageMode;
use pdfcraft_ui_egui::theme::{self, ThemeKind, ThemePreference, Tokens};
use pdfcraft_ui_egui::{Flavor, PdfCraftApp};

/// Two 200×300 pages.
fn fixture() -> Vec<u8> {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 200 300] >>",
        "<< /Type /Page /Parent 2 0 R >>",
        "<< /Type /Page /Parent 2 0 R >>",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

/// A Margin app reading its settings from an empty folder, with the fixture open (no file:
/// nothing is written).
fn margin_app(name: &str) -> PdfCraftApp {
    let mut app = PdfCraftApp::with_flavor(Flavor::Margin);
    let config = std::env::temp_dir().join(format!("margin-display-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&config).ok();
    app.margin_keys.set_config_dir(config);
    app.margin_keys.set_data_dir(std::env::temp_dir().join(format!("margin-display-data-{}", std::process::id())));
    app.open_bytes("doc.pdf", None, fixture()).unwrap();
    app
}

fn app_harness(flavor: Flavor) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1100.0, 800.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::with_flavor(flavor);
        app.set_option("language", "en").unwrap();
        app.set_theme_preference(ThemePreference::Dark);
        app
    });
    h.input_mut().system_theme = Some(egui::Theme::Dark);
    h.run_steps(4);
    h
}

#[test]
fn margin_wears_its_palette_where_pdfcraft_is_dark() {
    let h = app_harness(Flavor::Margin);
    assert_eq!(h.state().theme, ThemeKind::Margin);
    assert_eq!(Tokens::get(&h.ctx).kind, ThemeKind::Margin);
    assert!(h.ctx.global_style().visuals.dark_mode);
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);

    let h = app_harness(Flavor::PdfCraft);
    assert_eq!(h.state().theme, ThemeKind::Dark);
    assert_eq!(Tokens::get(&h.ctx).kind, ThemeKind::Dark);
}

/// The page colour belongs to the document (its sidecar), not to the app's saved state.
#[test]
fn page_color_is_per_document_and_margin_only() {
    let mut margin = margin_app("option");
    margin.set_option("page_color", "sepia").unwrap();
    assert_eq!(margin.views[0].margin.display.color.mode, PageMode::Sepia);
    assert!(!margin.persist().contains("margin_display"), "nothing in the app's saved state");
    let mut empty = PdfCraftApp::with_flavor(Flavor::Margin);
    assert!(empty.set_option("page_color", "night").is_err(), "needs a document");

    let mut pdfcraft = PdfCraftApp::new();
    pdfcraft.open_bytes("doc.pdf", None, fixture()).unwrap();
    assert!(pdfcraft.set_option("page_color", "night").is_err());
    assert!(pdfcraft.set_option("trim", "auto").is_err());
    assert_eq!(pdfcraft.views[0].margin.trim.crop(0), pdfcraft_ui_egui::margin::trim::FULL);
}

/// The popover alone, in a popup frame, over a Margin app with a document open.
fn popover_harness(name: &str) -> Harness<'static, (PdfCraftApp, bool)> {
    let app = margin_app(name);
    // 60 fps: the harness feeds one input event per frame, so a double-click spans a few frames.
    let mut h = Harness::builder().with_size(egui::vec2(440.0, 900.0)).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui, (app, styled): &mut (PdfCraftApp, bool)| {
            // Named font families exist only from the frame after they are installed.
            if !*styled {
                theme::install_fonts(ui.ctx());
                theme::apply(ui.ctx(), ThemeKind::Margin);
                *styled = true;
                return;
            }
            display::popover_frame(ui.style()).show(ui, |ui| display::popover_contents(app, ui));
        },
        (app, false),
    );
    h.run_steps(3);
    h
}

#[test]
fn popover_picks_modes_and_resets_sliders() {
    let mut h = popover_harness("modes");
    let state = |h: &Harness<'static, (PdfCraftApp, bool)>| h.state().0.views[0].margin.display;
    assert_eq!(state(&h), DisplayState::default());

    h.get_by_label("Sepia").click();
    h.run_steps(2);
    assert_eq!(state(&h).color.mode, PageMode::Sepia);
    assert!(!state(&h).follow_system, "a pick by hand turns follow system off");
    assert!(h.query_by_label("Keep figure hues").is_none(), "only Ink and Mono offer it");

    h.get_by_label("Ink").click();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Keep figure hues").click();
    h.run_steps(2);
    assert!(!state(&h).color.keep_hues);

    h.state_mut().0.views[0].margin.display.color.brightness = 80;
    h.state_mut().0.views[0].margin.display.color.contrast = 140;
    h.run_steps(2);
    let sliders = h.get_all_by_role(egui::accesskit::Role::Slider).collect::<Vec<_>>();
    assert_eq!(sliders.len(), 2);
    sliders[0].click();
    sliders[0].click();
    drop(sliders);
    h.run_steps(8);
    assert_eq!(state(&h).color.brightness, 100, "double-click resets brightness");
    assert_eq!(state(&h).color.contrast, 140);

    h.get_by_label("Reset brightness & contrast").click();
    h.run_steps(2);
    assert_eq!(state(&h).color.contrast, 100);

    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Follow system dark mode").click();
    h.run_steps(2);
    assert!(state(&h).follow_system);
}

#[test]
fn popover_sets_two_up_with_a_cover_and_a_manual_trim() {
    let mut h = popover_harness("layout");
    assert!(h.query_by_label("Cover page alone").is_none(), "only Two-up has a cover page");
    h.get_by_label("Two-up").click();
    h.run_steps(2);
    assert_eq!(h.state().0.views[0].layout, PageLayout::TwoUp);
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Cover page alone").click();
    h.run_steps(2);
    assert!(h.state().0.views[0].cover);

    h.get_by_label("Manual").click();
    h.run_steps(2);
    let trim = &h.state().0.views[0].margin.trim;
    assert!(trim.on && trim.manual);
    // Drag across the middle of the preview: keep its central part.
    let page = h.get_by_label("Trim preview: All pages").rect();
    let (from, to) = (page.lerp_inside(egui::vec2(0.2, 0.1)), page.lerp_inside(egui::vec2(0.8, 0.9)));
    h.hover_at(from);
    h.run_steps(1);
    h.drag_at(from);
    h.run_steps(1);
    for k in 1..=4 {
        h.hover_at(from + (to - from) * (k as f32 / 4.0));
        h.run_steps(1);
    }
    h.drop_at(to);
    h.run_steps(2);
    let [left, top, right, bottom] = h.state().0.views[0].margin.trim.odd;
    for (got, want) in [(left, 0.2), (top, 0.1), (right, 0.2), (bottom, 0.1)] {
        assert!((got - want).abs() < 0.03, "margins {:?}", h.state().0.views[0].margin.trim.odd);
    }
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Odd and even pages separately").click();
    h.run_steps(2);
    assert!(h.query_by_label("Trim preview: Odd pages").is_some() && h.query_by_label("Trim preview: Even pages").is_some());
    h.get_by_label("Off").click();
    h.run_steps(2);
    assert!(!h.state().0.views[0].margin.trim.on);
}

/// R8 QoL 12's narrow window: the default the review measured, 1024 × 640.
const SHORT_WINDOW: egui::Vec2 = egui::vec2(1024.0, 640.0);
/// A rect edge this close is the same column.
const SAME_COLUMN: f32 = 0.5;

/// R8 QoL 12: Pages, Fit and Trim start their choices on one x, the sliders from the same
/// start; in a short window the expanded popover (Two-up's cover row, Manual trim) stays inside
/// the window and its body scrolls, so the last switch can be reached and used.
#[test]
fn display_rows_line_up_and_scroll_in_a_short_window() {
    let mut h = Harness::builder().with_size(SHORT_WINDOW).build_eframe(|_cc| {
        let mut app = margin_app("short");
        app.set_option("language", "en").unwrap();
        app
    });
    h.run_steps(4);
    h.get_by_label("Page colour and layout").click();
    h.run_steps(3);
    h.get_by_label("Two-up").click();
    h.run_steps(2);
    h.get_by_label("Manual").click();
    h.run_steps(4);
    let starts = ["Scroll", "Width", "Off"].map(|label| h.get_by_label(label).rect().left());
    assert!(starts.iter().all(|x| (x - starts[0]).abs() < SAME_COLUMN), "choice starts {starts:?}");
    // Brightness and contrast, above the Pages row (the footer's scrubber is a slider too).
    let pages_row = h.get_by_label("Scroll").rect().top();
    let sliders: Vec<egui::Rect> =
        h.get_all_by_role(egui::accesskit::Role::Slider).map(|node| node.rect()).filter(|r| r.bottom() < pages_row).collect();
    assert_eq!(sliders.len(), 2, "{sliders:?}");
    assert!(sliders.iter().all(|r| (r.left() - sliders[0].left()).abs() < SAME_COLUMN), "slider starts {sliders:?}");
    assert!(sliders[0].left() <= starts[0] && starts[0] - sliders[0].left() < SEGMENT_SLACK, "sliders {sliders:?}, choices {starts:?}");

    let follow = |h: &Harness<'static, PdfCraftApp>| {
        h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Follow system dark mode").rect()
    };
    assert!(follow(&h).bottom() > SHORT_WINDOW.y, "the expanded popover is taller than the window: {:?}", follow(&h));
    h.hover_at(h.get_by_label("Width").rect().center());
    for _ in 0..20 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -200.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(1);
    }
    h.run_steps(10);
    assert!(follow(&h).bottom() < SHORT_WINDOW.y, "the body scrolled its last switch into the window: {:?}", follow(&h));
    let following = h.state().views[0].margin.display.follow_system;
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Follow system dark mode").click();
    h.run_steps(3);
    assert_ne!(h.state().views[0].margin.display.follow_system, following, "the switch works");
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Follow system dark mode");
}

/// How far a segmented control's first choice sits inside its well (`look::segmented`), plus
/// slack: the slider's track starts at the well's edge.
const SEGMENT_SLACK: f32 = 6.0;
