//! Margin's Settings window: PdfCraft's Preferences (`app.preferences`) as a roomy modal of
//! setting rows (theme-lab screen 9, "Settings (new)"). Each row is a name and a muted
//! description on the left and one control on the right; small-caps accent labels group them.
//!
//! The rows write what PdfCraft's Preferences write (`app.language`, `reopen_last_session`,
//! `comment_prefs.author`, the session's JavaScript switch), saved with the app's state as
//! before. Pages and Fit are Margin's own document defaults: Margin opens every document from
//! `settings.jsonc`'s `view` block, so these two rows write `view.layout` and `view.fit` there,
//! through the same edit "Set as default" in the Display popover uses. PdfCraft's workspace mode
//! and its page and zoom defaults have no row: Margin never shows them.

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, CornerRadius, Layout, Rect, RichText, Sense, Stroke, UiBuilder, Vec2, vec2};
use margin_config::{Fit as DocFit, Layout as DocLayout};
use serde_json::Value;

use super::look::{self, ButtonSize, Segment, Surfaces};
use crate::theme::{self, Tokens};
use crate::{Dialog, PdfCraftApp, i18n};

#[cfg(not(target_arch = "wasm32"))]
#[path = "notes_settings.rs"]
mod notes_settings;

/// The sheet's width, and its distance from the window's edges when the window is smaller.
const SHEET_WIDTH: f32 = 680.0;
const SCREEN_INSET: f32 = 24.0;
/// The side padding of the header, body and footer.
const PAD_X: f32 = 28.0;
/// The header's padding above and below the title row, and the title row's height (the close
/// button's).
const HEADER_PAD_TOP: f32 = 22.0;
const HEADER_PAD_BOTTOM: f32 = 6.0;
const HEADER_ROW: f32 = 32.0;
/// The footer's padding above and below the Done button.
const FOOTER_PAD_TOP: f32 = 16.0;
const FOOTER_PAD_BOTTOM: f32 = 20.0;
/// The least room the body keeps in a short window (it scrolls below that).
const MIN_BODY: f32 = 120.0;
/// The space above each section label.
const SECTION_GAP: f32 = 26.0;
/// A setting row: its least height, the padding above and below its contents, the gap between
/// the text column and the control, and the gap between the name and its description.
const ROW_MIN_HEIGHT: f32 = 60.0;
const ROW_PAD_Y: f32 = 14.0;
const ROW_GAP: f32 = 24.0;
const NAME_GAP: f32 = 4.0;
/// The tallest control in a row (dropdown, text field), which rows centre their text against.
const CONTROL_HEIGHT: f32 = look::CONTROL_ROW;
/// Every row's control slot: one width for every row, so a row's text wraps the same on the first
/// frame as on the next, whatever its control shows (a vault name, a folder count, a language).
pub(super) const CONTROL_SLOT: f32 = 340.0;
const NAME_SIZE: f32 = 14.0;
const DESCRIPTION_SIZE: f32 = 12.5;
/// A description's line height as a share of its size (the mockup's 1.45 leading).
const DESCRIPTION_LEADING: f32 = 1.45;
const DESCRIPTION_LINE_HEIGHT: f32 = DESCRIPTION_SIZE * DESCRIPTION_LEADING;
const FIELD_TEXT: f32 = 13.0;
/// The dropdown's width, its text's side padding and its chevron's size.
const DROPDOWN_WIDTH: f32 = 160.0;
const DROPDOWN_PAD_X: f32 = 12.0;
const CHEVRON_SIZE: f32 = 14.0;
/// The name field's width and the padding inside its well.
const NAME_FIELD_WIDTH: f32 = 220.0;

/// Draws the Settings window while `app.dialog` is [`Dialog::Preferences`] in Margin; Done, the
/// close button, Esc or a click on the scrim closes it.
pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let width = SHEET_WIDTH.min(screen.width() - 2.0 * SCREEN_INSET).max(0.0) - 2.0 * look::EDGE;
    let chrome_height =
        HEADER_PAD_TOP + HEADER_ROW + HEADER_PAD_BOTTOM + FOOTER_PAD_TOP + look::BUTTON_HEIGHT.max(CONTROL_HEIGHT) + FOOTER_PAD_BOTTOM;
    // The sheet hangs from `sheet_top`, so its body gets what is left under that.
    let body_room = (screen.height() - look::sheet_top(ctx) - SCREEN_INSET - chrome_height).max(MIN_BODY);
    let mut close = false;
    let mut done = false;
    let modal = look::modal(ctx, egui::Id::new("margin-settings"), &t).frame(look::modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        close |= header(ui, &t);
        egui::ScrollArea::vertical().id_salt("margin-settings-body").auto_shrink([false, true]).max_height(body_room).show(ui, |ui| {
            egui::Frame::NONE.inner_margin(egui::Margin::symmetric(PAD_X as i8, 0)).show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                body(app, ui, &t);
            });
        });
        done = footer(ui);
    });
    // Done keeps the template fields' unsaved text; the × and Esc close without it.
    #[cfg(not(target_arch = "wasm32"))]
    if done {
        notes_settings::save_drafts(app, ctx);
    }
    if close || done || modal.should_close() {
        app.dialog = None;
    }
}

/// "Settings" and the close button, the shared sheet header. True when the button was clicked.
fn header(ui: &mut egui::Ui, t: &Tokens) -> bool {
    ui.add_space(HEADER_PAD_TOP);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_ROW), Sense::hover());
    let mut row = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(PAD_X, 0.0))).layout(Layout::left_to_right(Align::Center)));
    let clicked = look::sheet_header(&mut row, t, i18n::t("Settings"));
    ui.add_space(HEADER_PAD_BOTTOM);
    clicked
}

/// Done, on the right. True when clicked.
fn footer(ui: &mut egui::Ui) -> bool {
    ui.add_space(FOOTER_PAD_TOP);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), look::BUTTON_HEIGHT.max(CONTROL_HEIGHT)), Sense::hover());
    let mut row = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(PAD_X, 0.0))).layout(Layout::right_to_left(Align::Center)));
    let done = look::button_primary(&mut row, ButtonSize::Standard, i18n::t("Done")).clicked();
    ui.add_space(FOOTER_PAD_BOTTOM);
    done
}

/// The sections and their rows.
fn body(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    section(ui, t, "General");
    row(ui, t, RowText { id: "language", name: i18n::t("Interface language"), description: None, first: true }, |ui, name| {
        language_dropdown(app, ui, t, name);
    });
    // The web build has no file paths to reopen.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let text = RowText {
            id: "reopen",
            name: i18n::t("Reopen last files"),
            description: Some(i18n::t("Open the files that were open when Margin last closed.")),
            first: false,
        };
        row(ui, t, text, |ui, name| {
            if look::switch(ui, app.reopen_last_session, name).clicked() {
                app.reopen_last_session = !app.reopen_last_session;
            }
        });
    }

    section(ui, t, "Opening documents");
    let defaults = &app.margin_keys.settings().view;
    let (layout, fit) = (defaults.layout, defaults.fit);
    let mut write: Option<(&str, &str)> = None;
    let text = RowText {
        id: "pages",
        name: i18n::t("Pages"),
        description: Some(i18n::t("For documents opened for the first time. Scroll never snaps between pages.")),
        first: true,
    };
    row(ui, t, text, |ui, _| {
        let choices = [(DocLayout::Scroll, "Scroll", "scroll"), (DocLayout::Single, "Single", "single"), (DocLayout::TwoUp, "Two-up", "two_up")];
        let segments = choices.map(|(choice, label, _)| Segment { label: i18n::t(label), selected: layout == choice, tip: None });
        let picked = look::segmented_sized(ui, &segments, CONTROL_HEIGHT).and_then(|i| choices.get(i)).filter(|(choice, _, _)| *choice != layout);
        if let Some((_, _, value)) = picked {
            write = Some(("view.layout", value));
        }
    });
    let text = RowText {
        id: "fit",
        name: i18n::t("Fit"),
        description: Some(i18n::t("For documents opened for the first time. Reading keeps the page at a comfortable reading width.")),
        first: false,
    };
    row(ui, t, text, |ui, _| {
        let reading_tip = i18n::t("A comfortable reading width (view.reading_width_px)");
        let choices =
            [(DocFit::Width, "Width", "width", None), (DocFit::Page, "Page", "page", None), (DocFit::Read, "Reading", "read", Some(reading_tip))];
        let segments = choices.map(|(choice, label, _, tip)| Segment { label: i18n::t(label), selected: fit == choice, tip });
        let picked = look::segmented_sized(ui, &segments, CONTROL_HEIGHT).and_then(|i| choices.get(i)).filter(|(choice, _, _, _)| *choice != fit);
        if let Some((_, _, value, _)) = picked {
            write = Some(("view.fit", value));
        }
    });
    if let Some((key, value)) = write {
        set_document_default(app, key, value);
    }

    section(ui, t, "Comments");
    let text =
        RowText { id: "author", name: i18n::t("Your name"), description: Some(i18n::t("Signed on new comments and highlights.")), first: true };
    row(ui, t, text, |ui, name| name_field(app, ui, name));
    #[cfg(not(target_arch = "wasm32"))]
    notes_settings::sections(app, ui, t);

    section(ui, t, "JavaScript");
    let text = RowText {
        id: "javascript",
        name: i18n::t("Acrobat JavaScript"),
        description: Some(i18n::t("Runs in a sandbox with no file or network access. Format, validate and calculate still work when off.")),
        first: true,
    };
    row(ui, t, text, |ui, name| {
        let on = app.session.javascript();
        if look::switch(ui, on, name).clicked() {
            app.session.set_javascript(!on);
        }
    });
}

/// A section's small-caps label in the section label colour, after the section gap.
fn section(ui: &mut egui::Ui, t: &Tokens, title: &str) {
    ui.add_space(SECTION_GAP);
    ui.add(egui::Label::new(look::section_caps(t, i18n::t(title), None)).selectable(false));
}

/// What a setting row says: its name, an optional description, and whether it is the first of
/// its section (no divider above it). `id` keeps the row's controls apart from other rows'.
struct RowText<'a> {
    id: &'a str,
    name: &'a str,
    description: Option<&'a str>,
    first: bool,
}

/// One setting row: `text` on the left, wrapping in what the control leaves; the control
/// (`control`, given the row's name for screen readers) on the right; both centred in a row of at
/// least [`ROW_MIN_HEIGHT`].
fn row(ui: &mut egui::Ui, t: &Tokens, text: RowText<'_>, control: impl FnOnce(&mut egui::Ui, &str)) {
    let id = ui.id().with(("margin-settings-row", text.id));
    let width = ui.available_width();
    let control_width = CONTROL_SLOT.min(width);
    let text_width = (width - control_width - ROW_GAP).max(0.0);
    let name = ui.painter().layout(text.name.to_owned(), theme::medium(NAME_SIZE), t.text, text_width);
    let description = text.description.map(|description| {
        let format = TextFormat {
            font_id: theme::regular(DESCRIPTION_SIZE),
            color: t.text_muted,
            line_height: Some(DESCRIPTION_LINE_HEIGHT),
            ..Default::default()
        };
        let mut job = LayoutJob::single_section(description.to_owned(), format);
        job.wrap.max_width = text_width;
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    });
    let text_height = name.size().y + description.as_ref().map_or(0.0, |galley| NAME_GAP + galley.size().y);
    let height = (text_height.max(CONTROL_HEIGHT) + 2.0 * ROW_PAD_Y).max(ROW_MIN_HEIGHT);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if !text.first {
        ui.painter().hline(rect.x_range(), rect.top() + look::EDGE / 2.0, Stroke::new(look::EDGE, t.divider));
    }
    let text_rect = Rect::from_min_size(egui::pos2(rect.left(), rect.center().y - text_height / 2.0), vec2(text_width, text_height));
    let mut column = ui.new_child(UiBuilder::new().max_rect(text_rect).layout(Layout::top_down(Align::Min)));
    column.add(egui::Label::new(name).selectable(false));
    if let Some(galley) = description {
        column.add_space(NAME_GAP);
        column.add(egui::Label::new(galley).selectable(false));
    }
    let control_rect =
        Rect::from_min_size(egui::pos2(rect.right() - control_width, rect.center().y - CONTROL_HEIGHT / 2.0), vec2(control_width, CONTROL_HEIGHT));
    let mut right = ui.new_child(UiBuilder::new().id_salt(id).max_rect(control_rect).layout(Layout::right_to_left(Align::Center)));
    // Controls in a cluster keep a gap between them (the body's spacing is zero).
    right.spacing_mut().item_spacing.x = look::GAP;
    right.set_clip_rect(control_rect.expand(look::GAP).intersect(ui.clip_rect()));
    control(&mut right, text.name);
}

/// The interface language: Auto (the system's) or one of PdfCraft's languages, as PdfCraft's
/// Preferences offer them. The rest of the window relabels at once.
fn language_dropdown(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, name: &str) {
    field_style(ui, t);
    let selected = i18n::Lang::from_code(&app.language).map_or(i18n::t("Auto"), i18n::Lang::name);
    let before = app.language.clone();
    let chevron = t.icon;
    let dropdown = egui::ComboBox::from_id_salt("interface-language")
        .width(DROPDOWN_WIDTH)
        .icon(move |ui, rect, _, _| crate::icons::paint(ui, rect, "chevron-down", CHEVRON_SIZE, chevron))
        .selected_text(RichText::new(selected).font(theme::regular(FIELD_TEXT)).color(t.text))
        .show_ui(ui, |ui| {
            if ui.selectable_value(&mut app.language, i18n::AUTO.to_string(), i18n::t("Auto")).clicked() {
                ui.close();
            }
            for language in i18n::Lang::all() {
                if ui.selectable_value(&mut app.language, language.code().to_string(), language.name()).clicked() {
                    ui.close();
                }
            }
        });
    dropdown.response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, ui.is_enabled(), name));
    if app.language != before {
        i18n::set_current(i18n::Lang::from_pref(&app.language));
    }
}

/// A dropdown's button in Margin's input well: the base fill and the control outline, the
/// accent while its list is open.
fn field_style(ui: &mut egui::Ui, t: &Tokens) {
    let edge = Surfaces::of(t).control_edge;
    let spacing = ui.spacing_mut();
    spacing.interact_size.y = CONTROL_HEIGHT;
    spacing.button_padding.x = DROPDOWN_PAD_X;
    let widgets = &mut ui.visuals_mut().widgets;
    for state in [&mut widgets.inactive, &mut widgets.hovered, &mut widgets.active, &mut widgets.open] {
        state.weak_bg_fill = t.field;
        state.bg_stroke = Stroke::new(look::EDGE, edge);
        state.corner_radius = CornerRadius::same(t.radius);
        state.expansion = 0.0;
    }
    widgets.open.bg_stroke = Stroke::new(look::EDGE, t.accent);
}

/// The name new comments are signed with, in Margin's one text field (a control tall).
fn name_field(app: &mut PdfCraftApp, ui: &mut egui::Ui, name: &str) {
    let id = egui::Id::new("margin-settings-author");
    look::field_well(ui, id, vec2(NAME_FIELD_WIDTH, CONTROL_HEIGHT), |ui| {
        let edit = egui::TextEdit::singleline(&mut app.comment_prefs.author)
            .id(id)
            .frame(egui::Frame::NONE)
            .margin(Vec2::ZERO)
            .font(theme::regular(FIELD_TEXT))
            .desired_width(ui.available_width())
            .char_limit(crate::MAX_AUTHOR_CHARS);
        let resp = ui.add(edit);
        ui.ctx().accesskit_node_builder(resp.id, |node| node.set_label(name));
    });
}

/// Writes one of Margin's document defaults (`view.layout`, `view.fit`) into `settings.jsonc`,
/// keeping its comments, and reads the file again so this window and the next document opened
/// follow at once. A failure is told in a notice.
fn set_document_default(app: &mut PdfCraftApp, key: &str, value: &str) {
    let Some(dir) = app.margin_keys.settings_dir() else {
        app.notify(i18n::t("This system has no settings folder."));
        return;
    };
    match margin_config::set_user_values(&dir, &[(key, Value::from(value))]) {
        Ok(()) => super::keys::reload_now(app),
        Err(err) => app.notify(format!("Couldn't set the default: {err}")),
    }
}

/// Whether Margin draws `dialog` itself (the hook in `dialogs::show`).
pub(crate) fn owns(app: &PdfCraftApp, dialog: Dialog) -> bool {
    app.flavor == crate::Flavor::Margin && dialog == Dialog::Preferences
}
