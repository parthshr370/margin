//! What a file dragged over the reader will do, said before it is dropped: PdfCraft opens a
//! dropped PDF (`PdfCraftApp::open_dropped`, or shows its tab when it is already open) and never
//! adds its pages to the document being read; only Organize's page grid and Combine insert pages,
//! and they draw their own drop marks.

use egui::{Align2, CornerRadius, Stroke};

use crate::PdfCraftApp;
use crate::margin::look::{GAP, GAP_LARGE};
use crate::theme::{self, Tokens};

const CARD_TEXT: f32 = 13.0;
const NOTE_TEXT: f32 = 11.5;
/// The dimming over the window while a file is dragged over it.
const SCRIM_ALPHA: u8 = 120;

/// While files are dragged over the reader (not Organize's grid, which inserts pages), a quiet
/// card in the middle of the window: "Drop to open", and that the open document stays as it is.
pub(crate) fn hint(app: &PdfCraftApp, ctx: &egui::Context) {
    let organizing = app.active.and_then(|index| app.views.get(index)).is_some_and(|view| view.organize);
    let count = ctx.input(|input| input.raw.hovered_files.len());
    if count == 0 || organizing || app.combine_showing() {
        return;
    }
    let t = Tokens::get(ctx);
    let window = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("margin-drop-hint")));
    painter.rect_filled(window, CornerRadius::ZERO, egui::Color32::from_black_alpha(SCRIM_ALPHA));
    let title = if count == 1 { "Drop to open".to_owned() } else { format!("Drop to open {count} files") };
    let note = if app.active.is_some() { "The document you are reading stays as it is." } else { "Each PDF opens in its own tab." };
    let title = painter.layout_no_wrap(title, theme::medium(CARD_TEXT), t.text);
    let note = painter.layout_no_wrap(note.to_owned(), theme::regular(NOTE_TEXT), t.text_muted);
    let size = egui::vec2(title.size().x.max(note.size().x) + 4.0 * GAP_LARGE, title.size().y + note.size().y + GAP + 2.0 * GAP_LARGE);
    let card = Align2::CENTER_CENTER.align_size_within_rect(size, window);
    painter.rect(card, CornerRadius::same(t.radius), t.card, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let title_pos = egui::pos2(card.center().x - title.size().x / 2.0, card.top() + GAP_LARGE);
    let note_pos = egui::pos2(card.center().x - note.size().x / 2.0, title_pos.y + title.size().y + GAP);
    painter.galley(title_pos, title, t.text);
    painter.galley(note_pos, note, t.text_muted);
}
