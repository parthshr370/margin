//! Margin's Tools tab (plan/R10-SURFACES.md §6): Margin's own list over PdfCraft's tool catalogue
//! (`pdfcraft_engine::catalog`), in Margin's surfaces. Only working items show (ready in this
//! build and registered as a command); planned and provider-dependent ones stay out, and so does
//! a group left with none. No catalogue hues, milestone banner or Ready/AI pills: rows are
//! neutral, 40 points high, with the shared hover, focus and disabled states. A picked group's
//! own options (Format text, signatures, the Redact footer) are PdfCraft's
//! (`panels::tool_detail_body`), which draws that group's items through [`section`].

use egui::{Rect, Sense, vec2};
use pdfcraft_engine::catalog::{self, Availability, TOOL_GROUPS, ToolGroup, ToolItem, ToolSection};
use pdfcraft_engine::commands::{self, Needs};

use crate::margin::look::{self, ButtonSize, GAP};
use crate::theme::{self, Tokens};
use crate::{LeftPanel, PdfCraftApp, icons};

/// A row: its height, the inset of its icon, the icon and label sizes.
const ROW_HEIGHT: f32 = 40.0;
const ROW_INSET: f32 = 8.0;
const ICON: f32 = 16.0;
const LABEL_TEXT: f32 = 13.0;
/// A group label ("Modify page"): its size and the space before and after it.
const GROUP_TEXT: f32 = 12.0;
const GROUP_BEFORE: f32 = 24.0;
const GROUP_AFTER: f32 = 8.0;

/// The Tools pane: the working tool groups, or the picked group's items and options.
pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    // Shown now, even if the tab was picked this frame (after `sync`).
    app.left_open = true;
    match app.left {
        LeftPanel::AllTools => list(app, ui, t),
        LeftPanel::Tool(id) => match catalog::group(id).filter(|group| has_working(group)) {
            Some(group) => detail(app, ui, t, group),
            None => app.left = LeftPanel::AllTools,
        },
    }
    // A tool's own options may close PdfCraft's tool panel; in Margin that closes the sidebar.
    if !app.left_open {
        app.margin_chrome.sidebar_open = false;
    }
}

/// Whether Margin offers `item`: ready in this build and a registered command (anything else
/// only says it isn't available yet).
fn working(item: &ToolItem) -> bool {
    item.availability == Availability::Ready && commands::command(item.command).is_some()
}

/// Whether `group` has at least one working item it lists.
fn has_working(group: &ToolGroup) -> bool {
    group.sections.iter().flat_map(|section| section.items).any(|item| working(item) && crate::panels::listed(group, item))
}

fn list(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    look::pane_header(ui, t, "Tools", None, |_| {});
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.add_space(GAP);
        for group in TOOL_GROUPS.iter().filter(|group| has_working(group)) {
            if row(ui, t, group.icon, group.label, Some("chevron-right"), None).clicked() {
                app.left = LeftPanel::Tool(group.id);
            }
        }
    });
}

/// A picked group: the pane header names it, with "All tools" on its right going back.
fn detail(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, group: &'static ToolGroup) {
    let mut back = false;
    look::pane_header(ui, t, group.label, None, |ui| {
        back = look::Button::ghost(ButtonSize::Compact, "All tools").icon("chevron-left").name("Back to all tools").show(ui).clicked();
    });
    if back {
        app.left = LeftPanel::AllTools;
        app.mode = crate::Mode::AllTools;
    }
    crate::panels::tool_detail_body(app, ui, t, group);
}

/// One section of a picked group, Margin's way: its label, then a row per working item; a row
/// whose command can't run on this document now is off and says why. Returns the clicked
/// item's command. Called by `panels::tool_detail_body` in Margin.
pub(crate) fn section(app: &PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, group: &ToolGroup, section: &ToolSection) -> Option<&'static str> {
    let items: Vec<&ToolItem> = section.items.iter().filter(|item| working(item) && crate::panels::listed(group, item)).collect();
    if items.is_empty() {
        return None;
    }
    ui.spacing_mut().item_spacing.y = 0.0;
    ui.add_space(GROUP_BEFORE - GAP);
    ui.horizontal(|ui| {
        ui.add_space(ROW_INSET);
        ui.label(egui::RichText::new(section.title).font(theme::medium(GROUP_TEXT)).color(t.surfaces.heading));
    });
    ui.add_space(GROUP_AFTER);
    let mut clicked = None;
    for item in items {
        let why_not = commands::command(item.command).filter(|spec| !app.command_enabled(spec)).map(|spec| disabled_reason(spec.needs));
        if row(ui, t, item.icon, item.label, None, why_not).clicked() {
            clicked = Some(item.command);
        }
    }
    clicked
}

/// Why a command whose document needs aren't met is off.
fn disabled_reason(needs: Needs) -> &'static str {
    match needs {
        Needs::Document => "Open a document first",
        Needs::Assembly => "This document's security doesn't allow page changes",
        Needs::Modification => "This document's security doesn't allow changes",
        Needs::Annotate => "This document's security doesn't allow comments",
        Needs::FillForms | Needs::HasFields => "This document has no form fields that can be changed",
        Needs::HasComments => "This document has no comments that can be changed",
        Needs::HasRedactions => "Mark something for redaction first",
        Needs::Marks(_) => "This document has none of these to change",
        Needs::Security | Needs::ProtectedSecurity => "This document's security can't be changed",
        Needs::Undo => "Nothing to undo",
        Needs::Redo => "Nothing to redo",
        _ => "Not available for this document now",
    }
}

/// A full-width row: a neutral icon, the label and an optional trailing icon (the group list's
/// chevron), with the shared hover and focus states. `why_not` turns it off, faint, with that
/// reason as its tooltip.
fn row(ui: &mut egui::Ui, t: &Tokens, icon: &str, label: &str, trailing: Option<&str>, why_not: Option<&str>) -> egui::Response {
    let enabled = why_not.is_none();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    if enabled {
        look::row_fill(ui, t, rect, &resp, false);
    }
    let (ink, icon_ink) = if enabled { (t.text, look::icon_tint(t, false, resp.hovered())) } else { (t.text_faint, t.text_faint) };
    let icon_rect = Rect::from_center_size(egui::pos2(rect.left() + ROW_INSET + ICON / 2.0, rect.center().y), vec2(ICON, ICON));
    icons::paint(ui, icon_rect, icon, ICON, icon_ink);
    let text_left = icon_rect.right() + GAP;
    let trailing_width = trailing.map_or(0.0, |_| ICON + GAP);
    let text_width = (rect.right() - ROW_INSET - trailing_width - text_left).max(GAP);
    crate::widgets::row_text(ui, egui::pos2(text_left, rect.center().y), label.to_owned(), theme::regular(LABEL_TEXT), ink, text_width);
    if let Some(trailing) = trailing {
        let trailing_rect = Rect::from_center_size(egui::pos2(rect.right() - ROW_INSET - ICON / 2.0, rect.center().y), vec2(ICON, ICON));
        icons::paint(ui, trailing_rect, trailing, ICON, t.text_muted);
    }
    match why_not {
        Some(reason) => look::tooltip(resp, &format!("{label}: {reason}"), None),
        None => resp,
    }
}
