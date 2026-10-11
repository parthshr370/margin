//! What the footer says about saving (R8 QoL 7): the real state from `reading::save_status`,
//! why a document isn't saved, where the highlights of a PDF Margin doesn't write are kept, and
//! the command that helps (`file.save`, `file.save_as`, "Save a copy with highlights").
//! Presentation only: saving itself is PdfCraft's Save, Margin's autosave and `margin::marks`.

use egui::{Align, Layout};

use super::look;
use super::marks::SAVE_COPY_ID;
use super::reading::{MarksKept, SaveStatus};
use crate::theme::{self, Tokens};

/// The status text's size, as the rest of the footer.
const TEXT_SIZE: f32 = 11.5;

/// Why a changed document isn't saved by itself, beyond its [`SaveStatus`].
pub(crate) struct Context<'a> {
    /// `reader.autosave`.
    pub autosave_on: bool,
    /// The document came from a file (not a new or combined one).
    pub has_file: bool,
    /// `Document::read_only_reason`, when PdfCraft opened it read-only.
    pub read_only_reason: Option<&'a str>,
    /// `file.save`'s key in Margin's keymap.
    pub save_key: Option<&'a str>,
}

/// The footer's save line: the words, their tooltip, whether they need the reader, the command
/// clicking the words runs ("Unsaved · Ctrl+S"), and buttons after them (label, command id).
#[derive(Debug, PartialEq)]
pub(crate) struct SaveLine {
    pub text: String,
    pub tip: String,
    pub alert: bool,
    pub click: Option<&'static str>,
    pub buttons: Vec<(&'static str, &'static str)>,
}

/// The line for `status`.
pub(crate) fn line(status: SaveStatus, why: &Context<'_>) -> SaveLine {
    let key = why.save_key;
    let save_now = key.map_or_else(String::new, |key| format!("; {key} saves now"));
    let with_key = |word: &str| key.map_or_else(|| word.to_owned(), |key| format!("{word} · {key}"));
    let (text, tip, alert, click, buttons) = match status {
        SaveStatus::Saved { autosave: true } => {
            ("Saved".to_owned(), format!("Your marks and edits save into the PDF by themselves{save_now}"), false, None, vec![])
        }
        SaveStatus::Saved { autosave: false } => ("Saved".to_owned(), unsaved_reason(why), false, None, vec![]),
        SaveStatus::Saving => ("Saving…".to_owned(), "Your changes save into the PDF a moment after the last one".to_owned(), false, None, vec![]),
        SaveStatus::Unsaved => (with_key("Unsaved"), unsaved_reason(why), true, Some("file.save"), vec![]),
        SaveStatus::Failed => (
            "Couldn't save".to_owned(),
            "Autosave couldn't write the PDF. Your changes are still here, unsaved: Save tries again, Save as writes a copy elsewhere".to_owned(),
            true,
            None,
            vec![("Save", "file.save"), ("Save as…", "file.save_as")],
        ),
        SaveStatus::ReadOnly { dirty, marks } => {
            let reason = match why.read_only_reason {
                Some(reason) => format!("This PDF opened read-only ({reason}), so it can't be changed or saved in place"),
                None => "The PDF file is read-only, so Margin can't save into it; Save as writes a copy".to_owned(),
            };
            let mut buttons = copy_button(marks);
            if dirty {
                buttons.push(("Save as…", "file.save_as"));
            }
            ("Read-only".to_owned(), format!("{reason}{}", marks_kept(marks)), dirty, None, buttons)
        }
        SaveStatus::Signed { dirty, marks } => {
            let tip = format!(
                "This PDF is signed, so Margin doesn't save it by itself. {} adds your changes as an incremental update, which keeps the signatures valid{}",
                key.unwrap_or("Save"),
                marks_kept(marks)
            );
            if dirty {
                (with_key("Signed · Unsaved"), tip, true, Some("file.save"), copy_button(marks))
            } else {
                ("Signed".to_owned(), tip, false, None, copy_button(marks))
            }
        }
    };
    SaveLine { text, tip, alert, click, buttons }
}

/// Where the highlights of a PDF Margin doesn't write go, for the tooltip.
fn marks_kept(marks: MarksKept) -> &'static str {
    match marks {
        MarksKept::InPdf => "",
        MarksKept::Beside { .. } => {
            ". Your highlights and notes are kept beside it, in its sidecar file; Save copy writes them into a new PDF as real annotations"
        }
        MarksKept::Refused => ". Highlights and notes can't be kept: its sidecar was written by a newer Margin or can't be read",
    }
}

/// "Save copy…" when the sidecar holds highlights or notes a copy can take.
fn copy_button(marks: MarksKept) -> Vec<(&'static str, &'static str)> {
    match marks {
        MarksKept::Beside { any: true } => vec![("Save copy…", SAVE_COPY_ID)],
        MarksKept::Beside { any: false } | MarksKept::InPdf | MarksKept::Refused => Vec::new(),
    }
}

/// Why Margin isn't saving this document by itself.
fn unsaved_reason(why: &Context<'_>) -> String {
    let save = why.save_key.unwrap_or("Save");
    if !why.autosave_on {
        format!("Autosave is off (reader.autosave): {save} saves into the PDF")
    } else if !why.has_file {
        "This document has no file yet: Save picks where it goes".to_owned()
    } else {
        format!("These changes are from before this session, so Margin doesn't save them by itself: {save} writes them")
    }
}

/// Draw `line` right to left (the buttons, then the words): every action is a compact secondary
/// button, plain status ("Saved") stays text. Returns the command clicked.
pub(crate) fn show(ui: &mut egui::Ui, t: &Tokens, line: &SaveLine) -> Option<&'static str> {
    let mut clicked = None;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        for (label, command) in line.buttons.iter().rev() {
            if look::Button::secondary(look::ButtonSize::Compact, label).tip(label).show(ui).clicked() {
                clicked = Some(*command);
            }
        }
        match line.click {
            Some(command) => {
                let resp = look::Button::secondary(look::ButtonSize::Compact, &line.text).tip(&line.tip).show(ui);
                if resp.clicked() {
                    clicked = Some(command);
                }
            }
            None => {
                let color = if line.alert { t.accent_text } else { t.text_muted };
                let words = egui::RichText::new(&line.text).font(theme::regular(TEXT_SIZE)).color(color);
                look::tooltip(ui.label(words), &line.tip, None);
            }
        }
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn why(autosave_on: bool) -> Context<'static> {
        Context { autosave_on, has_file: true, read_only_reason: None, save_key: Some("Ctrl+S") }
    }

    #[test]
    fn each_state_says_why_and_offers_the_right_save() {
        let saved = line(SaveStatus::Saved { autosave: true }, &why(true));
        assert_eq!((saved.text.as_str(), saved.alert), ("Saved", false));
        assert!(saved.tip.contains("by themselves"), "{}", saved.tip);
        let off = line(SaveStatus::Unsaved, &why(false));
        assert_eq!(off.text, "Unsaved · Ctrl+S");
        assert!(off.tip.contains("Autosave is off") && off.alert);
        assert_eq!((off.click, off.buttons.len()), (Some("file.save"), 0));
        let clean_off = line(SaveStatus::Saved { autosave: false }, &why(false));
        assert!(!clean_off.tip.contains("by themselves"), "no autosave promise when it is off: {}", clean_off.tip);
        let failed = line(SaveStatus::Failed, &why(true));
        assert_eq!(failed.buttons, [("Save", "file.save"), ("Save as…", "file.save_as")]);
        assert!(failed.alert);
        let read_only =
            line(SaveStatus::ReadOnly { dirty: true, marks: MarksKept::InPdf }, &Context { read_only_reason: Some("encrypted"), ..why(true) });
        assert_eq!(read_only.text, "Read-only");
        assert!(read_only.tip.contains("encrypted"));
        assert_eq!(read_only.buttons, [("Save as…", "file.save_as")]);
        let signed = line(SaveStatus::Signed { dirty: false, marks: MarksKept::InPdf }, &why(true));
        assert_eq!(signed.text, "Signed");
        assert!(signed.tip.contains("signatures valid") && signed.click.is_none() && signed.buttons.is_empty());
        assert_eq!(line(SaveStatus::Signed { dirty: true, marks: MarksKept::InPdf }, &why(true)).click, Some("file.save"));
    }

    #[test]
    fn highlights_kept_beside_the_pdf_are_said_and_offer_a_copy() {
        let beside = line(SaveStatus::ReadOnly { dirty: false, marks: MarksKept::Beside { any: true } }, &why(true));
        assert!(beside.tip.contains("kept beside it"), "{}", beside.tip);
        assert_eq!(beside.buttons, [("Save copy…", SAVE_COPY_ID)]);
        let nothing_yet = line(SaveStatus::Signed { dirty: false, marks: MarksKept::Beside { any: false } }, &why(true));
        assert!(nothing_yet.tip.contains("kept beside it") && nothing_yet.buttons.is_empty());
        let refused = line(SaveStatus::ReadOnly { dirty: false, marks: MarksKept::Refused }, &why(true));
        assert!(refused.tip.contains("can't be kept") && refused.buttons.is_empty(), "{}", refused.tip);
    }
}
