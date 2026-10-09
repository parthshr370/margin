//! Keystrokes in Zed syntax, normalised to the canonical form ui-egui builds from a key event
//! (`margin::keys`): modifiers in the order
//! `ctrl`, `alt`, `shift`, `super`, then the key. `secondary-` becomes `super-` (Cmd) on macOS
//! and `ctrl-` elsewhere, like PdfCraft's `Shortcut.command`.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The OS a keymap is compiled for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    /// Linux and the BSDs.
    Linux,
    /// Windows.
    Windows,
    /// macOS: `secondary` is Cmd.
    Macos,
}

impl Os {
    /// The OS this binary runs on.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    /// `linux`, `windows` or `macos`, as the compiled table and the `os` attribute write it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Macos => "macos",
        }
    }

    /// Parses `linux`, `windows` or `macos`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "linux" => Some(Self::Linux),
            "windows" => Some(Self::Windows),
            "macos" => Some(Self::Macos),
            _ => None,
        }
    }
}

/// Named keys (CU-KEY-2); `f1`…`f12` are checked apart.
const NAMED_KEYS: &[&str] =
    &["space", "enter", "escape", "tab", "backspace", "delete", "up", "down", "left", "right", "home", "end", "pageup", "pagedown", "menu"];

/// Highest function key.
const MAX_FUNCTION_KEY: u8 = 12;

/// Longest keystroke or chord accepted, in bytes (STD-RS-05).
const MAX_KEYS_BYTES: usize = 256;

/// Most keystrokes in one chord.
const MAX_CHORD_LEN: usize = 4;

fn is_named(key: &str) -> bool {
    NAMED_KEYS.contains(&key) || key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=MAX_FUNCTION_KEY).contains(&n))
}

fn error(keystroke: &str, reason: &'static str) -> Error {
    Error::Keystroke { keystroke: keystroke.chars().take(80).collect(), reason }
}

/// One keystroke (`secondary-shift-s`) in canonical form for `os` (`ctrl-shift-s`).
pub fn canonical(keystroke: &str, os: Os) -> Result<String> {
    let (mut ctrl, mut alt, mut shift, mut sup) = (false, false, false, false);
    let mut rest = keystroke;
    while let Some((modifier, after)) = rest.split_once('-').filter(|(_, after)| !after.is_empty()) {
        match modifier {
            "ctrl" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "super" => sup = true,
            "secondary" if os == Os::Macos => sup = true,
            "secondary" => ctrl = true,
            _ => break,
        }
        rest = after;
    }
    let key = rest;
    let mut chars = key.chars();
    let single = chars.next().filter(|_| chars.next().is_none());
    match single {
        Some(c) if c.is_ascii_uppercase() => return Err(error(keystroke, "letters are lower case; write shift-<letter>")),
        Some(c) if c.is_whitespace() || c.is_control() => return Err(error(keystroke, "write space, tab or enter by name")),
        Some(c) if shift && !c.is_alphabetic() => {
            return Err(error(keystroke, "shift- goes only with letters and named keys; the character already says it"));
        }
        Some(_) => {}
        None if is_named(key) => {}
        None => return Err(error(keystroke, "unknown key name")),
    }
    let modifiers = [(ctrl, "ctrl-"), (alt, "alt-"), (shift, "shift-"), (sup, "super-")];
    Ok(modifiers.iter().filter(|(on, _)| *on).map(|(_, prefix)| *prefix).chain([key]).collect())
}

/// A chord (`g g`): keystrokes separated by spaces, each in canonical form.
pub fn canonical_chord(keys: &str, os: Os) -> Result<Vec<String>> {
    if keys.len() > MAX_KEYS_BYTES {
        return Err(error(keys, "longer than 256 bytes"));
    }
    let strokes = keys.split_whitespace().map(|stroke| canonical(stroke, os)).collect::<Result<Vec<_>>>()?;
    match strokes.len() {
        0 => Err(error(keys, "empty")),
        n if n > MAX_CHORD_LEN => Err(error(keys, "a chord has at most 4 keystrokes")),
        _ => Ok(strokes),
    }
}

/// Whether a canonical keystroke types a character in a text field: no ctrl, alt or super, and
/// a single character or `space` (CU-KEY-5).
pub fn is_printable(stroke: &str) -> bool {
    let mut key = stroke;
    while let Some((modifier, after)) = key.split_once('-').filter(|(_, after)| !after.is_empty()) {
        match modifier {
            "shift" => key = after,
            "ctrl" | "alt" | "super" => return false,
            _ => break,
        }
    }
    key == "space" || key.chars().count() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secondary_follows_the_os() {
        assert_eq!(canonical("secondary-shift-s", Os::Linux).unwrap(), "ctrl-shift-s");
        assert_eq!(canonical("secondary-shift-s", Os::Macos).unwrap(), "shift-super-s");
        assert_eq!(canonical("shift-secondary-s", Os::Windows).unwrap(), "ctrl-shift-s");
    }

    #[test]
    fn dash_and_plus_keys_survive() {
        assert_eq!(canonical("secondary--", Os::Linux).unwrap(), "ctrl--");
        assert_eq!(canonical("secondary-=", Os::Linux).unwrap(), "ctrl-=");
        assert_eq!(canonical("secondary-+", Os::Linux).unwrap(), "ctrl-+");
        assert_eq!(canonical("-", Os::Linux).unwrap(), "-");
    }

    #[test]
    fn shift_only_with_letters_and_named_keys() {
        assert_eq!(canonical("shift-f3", Os::Linux).unwrap(), "shift-f3");
        assert_eq!(canonical("shift-space", Os::Linux).unwrap(), "shift-space");
        assert!(canonical("shift-1", Os::Linux).is_err());
        assert!(canonical("C", Os::Linux).is_err());
        assert!(canonical("f13", Os::Linux).is_err());
        assert!(canonical("ctrl-", Os::Linux).is_err());
    }

    #[test]
    fn chords_split_on_spaces() {
        assert_eq!(canonical_chord("g  g", Os::Linux).unwrap(), ["g", "g"]);
        assert!(canonical_chord("   ", Os::Linux).is_err());
        assert!(canonical_chord("a b c d e", Os::Linux).is_err());
    }

    #[test]
    fn printable_means_types_a_character() {
        assert!(is_printable("n"));
        assert!(is_printable("shift-n"));
        assert!(is_printable("space"));
        assert!(is_printable("?"));
        assert!(!is_printable("enter"));
        assert!(!is_printable("ctrl-k"));
        assert!(!is_printable("shift-f3"));
        assert!(!is_printable("ctrl--"));
    }
}
