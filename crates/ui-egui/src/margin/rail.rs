//! The annotate rail: a floating strip at the left of the page area with the reading tools.
//! It takes the place of PdfCraft's quick-action bar in the Margin flavor. Each button runs an
//! existing command; the selected comment tool's colour and width controls follow underneath.

use egui::{Color32, CornerRadius, Rect, Sense, Stroke, vec2};
use pdfcraft_engine::commands;

use crate::comments::CommentTool;
use crate::theme::Tokens;
use crate::{PdfCraftApp, QuickTool, icons};

/// One rail entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailItem {
    /// The Select tool (V). PdfCraft has no registry command for it.
    Select,
    /// A registered command id.
    Command(&'static str),
    Divider,
}

/// The rail, top to bottom (the mockup's default rail).
pub const RAIL: &[RailItem] = &[
    RailItem::Select,
    RailItem::Command("edit.snapshot"),
    RailItem::Command("comment.highlight"),
    RailItem::Command("comment.underline"),
    RailItem::Command("comment.note"),
    RailItem::Command("comment.ink"),
    RailItem::Command("comment.freetext"),
    RailItem::Command("comment.eraser"),
    RailItem::Divider,
    RailItem::Command("edit.undo"),
    RailItem::Command("edit.redo"),
];

const BUTTON: f32 = 34.0;
/// Distance from the page area's left edge.
const INSET: f32 = 14.0;

impl RailItem {
    fn icon(self) -> &'static str {
        match self {
            Self::Select => "mouse-pointer-2",
            Self::Command("edit.snapshot") => "square-dashed-mouse-pointer",
            Self::Command(id) => commands::command(id).map_or("square", |c| c.icon),
            Self::Divider => "minus",
        }
    }

    /// Its tooltip: the command's name, and its key when it has one.
    fn tooltip(self) -> String {
        let mac = cfg!(target_os = "macos");
        match self {
            Self::Select => "Select  (V)".to_string(),
            Self::Command("edit.snapshot") => "Area snapshot: copy a region of the page".to_string(),
            Self::Command(id) => match commands::command(id) {
                Some(spec) => {
                    let label = crate::i18n::menu_label(spec.id, spec.label);
                    match spec.shortcut {
                        Some(s) => format!("{label}  ({})", s.label(mac)),
                        None => label,
                    }
                }
                None => id.to_string(),
            },
            Self::Divider => String::new(),
        }
    }

    /// Whether this tool is the one in use.
    fn active(self, tool: QuickTool) -> bool {
        match self {
            Self::Select => tool == QuickTool::Select,
            Self::Command("edit.snapshot") => tool == QuickTool::Snapshot,
            Self::Command(id) => CommentTool::from_command(id).is_some_and(|c| tool == QuickTool::Comment(c)),
            Self::Divider => false,
        }
    }

    /// Run it: what clicking the button does.
    pub fn run(self, app: &mut PdfCraftApp) {
        match self {
            Self::Select => app.quick_tool = QuickTool::Select,
            Self::Command(id) => {
                app.execute(id);
            }
            Self::Divider => {}
        }
    }
}

/// Draw the rail over the page area `area` (the canvas calls this instead of its quick bar).
pub(crate) fn show(app: &mut PdfCraftApp, area: Rect, ui: &mut egui::Ui) {
    if !app.margin_chrome.rail_open {
        return;
    }
    let t = Tokens::get(ui.ctx());
    egui::Area::new(egui::Id::new("margin-rail"))
        .order(egui::Order::Middle)
        .pivot(egui::Align2::LEFT_CENTER)
        .fixed_pos(area.left_center() + vec2(INSET, 0.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::NONE
                .fill(t.card)
                .stroke(Stroke::new(1.0, t.border))
                .corner_radius(CornerRadius::same(12))
                .shadow(egui::Shadow { offset: [0, 10], blur: 40, spread: 0, color: Color32::from_black_alpha(if t.dark() { 128 } else { 30 }) })
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.vertical(|ui| {
                        let mut clicked = None;
                        for item in RAIL {
                            if *item == RailItem::Divider {
                                divider(ui, &t);
                                continue;
                            }
                            let enabled = match item {
                                RailItem::Command(id) => commands::command(id).is_some_and(|spec| app.command_enabled(spec)),
                                _ => true,
                            };
                            let r = ui
                                .add_enabled_ui(enabled, |ui| icons::button(ui, item.icon(), BUTTON, item.active(app.quick_tool), &item.tooltip()))
                                .inner;
                            if r.clicked() {
                                clicked = Some(*item);
                            }
                        }
                        if let Some(item) = clicked {
                            item.run(app);
                        }
                        if let QuickTool::Comment(tool) = app.quick_tool {
                            divider(ui, &t);
                            crate::comments::quick_bar_controls(ui, tool, &mut app.comment_prefs);
                        }
                    });
                });
        });
}

fn divider(ui: &mut egui::Ui, t: &Tokens) {
    let (r, _) = ui.allocate_exact_size(vec2(BUTTON, 9.0), Sense::hover());
    ui.painter().hline(r.x_range().shrink(6.0), r.center().y, Stroke::new(1.0, t.divider));
}
