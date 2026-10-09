//! Keymap files in Zed's format compiled into the one binding table the UI evaluates. The table
//! keeps load order: at equal depth a later binding wins, so the user's file beats the shipped
//! one, as in Zed.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::ContextExpr;
use crate::jsonc;
use crate::keystroke::{self, Os};
use crate::problem::{Problem, Severity};
use crate::registry::{CommandId, Registry};

/// The shipped keymap. Hand-written; every id it binds runs in the egui app.
pub const MARGIN_KEYMAP: &str = include_str!("../data/keymap.jsonc");

/// How [`MARGIN_KEYMAP`] is named in sources and messages.
pub const MARGIN_KEYMAP_FILE: &str = "built-in keymap";

/// The user keymap file inside `<config>`.
pub const KEYMAP_FILE: &str = "keymap.jsonc";

/// Format version of [`CompiledKeymap`].
pub const COMPILED_KEYMAP_VERSION: u32 = 1;

/// Largest keymap file read, in bytes.
pub const MAX_KEYMAP_BYTES: usize = 1 << 20;

/// What a binding runs: a command id, `[id, args]`, or `null` to unbind (CU-KEY-1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum BindingTarget {
    /// A command without arguments.
    Command(CommandId),
    /// A command and its arguments.
    WithArgs(CommandId, Value),
    /// Unbind: hides bindings of the same keys that rank lower.
    Unbind(()),
}

/// One block of a keymap file (the schema's view of it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeymapBlock {
    /// CU-KEY-4 expression; absent = everywhere, at the lowest depth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Keystroke or chord (`secondary-k`, `g g`) to what it runs.
    #[serde(default)]
    pub bindings: BTreeMap<String, BindingTarget>,
    /// Shipped keymap only: PdfCraft shortcuts Margin leaves unbound, id to reason (CU-KEY-7).
    #[serde(default, rename = "$pdfcraft_ignored", skip_serializing_if = "BTreeMap::is_empty")]
    pub pdfcraft_ignored: BTreeMap<String, String>,
}

/// A block as the compiler reads it: bindings stay raw JSON in file order, so one bad binding
/// drops only itself and `keys_for` lists keys in the order the file gives them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBlock {
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    bindings: serde_json::Map<String, Value>,
    #[serde(default, rename = "$pdfcraft_ignored")]
    pdfcraft_ignored: BTreeMap<String, String>,
}

/// A keymap file: an array of blocks (the `keymap.schema.json` root).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "Margin keymap")]
pub struct KeymapFile(pub Vec<KeymapBlock>);

/// Where a binding was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLine {
    /// The file, as shown to the user.
    pub file: String,
    /// 1-based line of the binding's key.
    pub line: u32,
    /// 1-based column of the binding's key.
    pub col: u32,
}

/// One compiled binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledBinding {
    /// Canonical keystrokes; more than one for a chord.
    pub keys: Vec<String>,
    /// The block's context, `None` for everywhere.
    pub context: Option<ContextExpr>,
    /// The block's context as written, for grouping bindings in the keyboard sheet.
    pub context_text: Option<String>,
    /// The command, `None` for an unbind.
    pub command: Option<String>,
    /// Arguments, `null` when none.
    pub args: Value,
    /// The command's registry `when`.
    pub when: Option<ContextExpr>,
    /// Fires inside a `TextInput`: the command is `in_text`, or the context names `TextInput`.
    pub in_text: bool,
    /// Where it was written.
    pub source: SourceLine,
}

/// The binding table the UI evaluates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledKeymap {
    /// [`COMPILED_KEYMAP_VERSION`].
    pub version: u32,
    /// The OS `secondary-` was resolved for.
    pub os: Os,
    /// `keys.chord_timeout_ms`.
    pub chord_timeout_ms: u32,
    /// Every binding in load order.
    pub bindings: Vec<CompiledBinding>,
}

/// One keymap file to compile.
#[derive(Debug, Clone, Copy)]
pub struct KeymapSource<'a> {
    /// The file, as shown to the user.
    pub file: &'a str,
    /// Its text.
    pub text: &'a str,
}

/// Settings and platform the compiler needs.
#[derive(Debug, Clone, Copy)]
pub struct CompileOptions {
    /// Resolves `secondary-`.
    pub os: Os,
    /// `keys.chord_timeout_ms`.
    pub chord_timeout_ms: u32,
    /// `keys.annotate_letters`; false drops bare-letter `AnnotateMode` bindings.
    pub annotate_letters: bool,
}

/// The compiled table and what went wrong while building it. Never fails: a bad file, block or
/// binding is dropped and reported.
#[derive(Debug, Clone)]
pub struct Compiled {
    /// The binding table.
    pub keymap: CompiledKeymap,
    /// Problems, in file order.
    pub problems: Vec<Problem>,
    /// `$pdfcraft_ignored` entries seen, id to reason.
    pub pdfcraft_ignored: BTreeMap<String, String>,
}

/// Compiles `sources` in load order (shipped first, the user's file last).
pub fn compile(registry: &Registry, sources: &[KeymapSource<'_>], options: &CompileOptions) -> Compiled {
    let mut compiled = Compiled {
        keymap: CompiledKeymap { version: COMPILED_KEYMAP_VERSION, os: options.os, chord_timeout_ms: options.chord_timeout_ms, bindings: Vec::new() },
        problems: Vec::new(),
        pdfcraft_ignored: BTreeMap::new(),
    };
    for source in sources {
        let first_problem = compiled.problems.len();
        compile_file(registry, *source, options, &mut compiled);
        // Object keys iterate sorted, not in file order: report in file order.
        if let Some(problems) = compiled.problems.get_mut(first_problem..) {
            problems.sort_by_key(|problem| (problem.line, problem.col));
        }
    }
    compiled
}

/// The shipped table for `options`, from [`MARGIN_KEYMAP`] alone.
pub fn compile_shipped(registry: &Registry, options: &CompileOptions) -> Compiled {
    compile(registry, &[KeymapSource { file: MARGIN_KEYMAP_FILE, text: MARGIN_KEYMAP }], options)
}

fn compile_file(registry: &Registry, source: KeymapSource<'_>, options: &CompileOptions, out: &mut Compiled) {
    let mut report = |position: jsonc::Position, message: String, severity: Severity| {
        let (line, col) = (position.line, position.col);
        out.problems.push(Problem { file: source.file.to_owned(), line, col, key: String::new(), message, severity });
    };
    if source.text.len() > MAX_KEYMAP_BYTES {
        report(jsonc::Position { line: 1, col: 1 }, "larger than 1 MiB; file ignored".to_owned(), Severity::Error);
        return;
    }
    let parsed = match jsonc::parse(source.text) {
        Ok(parsed) => parsed,
        Err(err) => return report(err.position, format!("{}; file ignored", err.message), Severity::Error),
    };
    let blocks = match parsed.value {
        None => return,
        Some(Value::Array(blocks)) => blocks,
        Some(_) => return report(jsonc::Position { line: 1, col: 1 }, "a keymap is an array of blocks; file ignored".to_owned(), Severity::Error),
    };
    for (index, block) in blocks.into_iter().enumerate() {
        let block_pointer = format!("/{index}");
        let block: RawBlock = match serde_json::from_value(block) {
            Ok(block) => block,
            Err(err) => {
                report(parsed.positions.find(&block_pointer), format!("block {index}: {err}; block ignored"), Severity::Warning);
                continue;
            }
        };
        out.pdfcraft_ignored.extend(block.pdfcraft_ignored);
        let context = match block.context.as_deref().map(ContextExpr::parse).transpose() {
            Ok(context) => context,
            Err(err) => {
                report(parsed.positions.find(&format!("{block_pointer}/context")), format!("{err}; block ignored"), Severity::Warning);
                continue;
            }
        };
        let names_text_input = context.as_ref().is_some_and(|expr| expr.mentions("TextInput"));
        let annotate_block = context.as_ref().is_some_and(|expr| expr.mentions("AnnotateMode"));
        let mut block_bindings = Vec::with_capacity(block.bindings.len());
        for (keys_text, target) in block.bindings {
            let position = parsed.positions.find(&format!("{block_pointer}/bindings/{}", jsonc::escape_pointer(&keys_text)));
            let keys = match keystroke::canonical_chord(&keys_text, options.os) {
                Ok(keys) => keys,
                Err(err) => {
                    report(position, format!("{err}; binding dropped"), Severity::Warning);
                    continue;
                }
            };
            if annotate_block && !options.annotate_letters && is_bare_letter(&keys) {
                continue;
            }
            let target: BindingTarget = match serde_json::from_value(target) {
                Ok(target) => target,
                Err(_) => {
                    let message = "expected a command id, [id, args] or null; binding dropped".to_owned();
                    report(position, message, Severity::Warning);
                    continue;
                }
            };
            let (command, args) = match target {
                BindingTarget::Command(id) => (Some(id), Value::Null),
                BindingTarget::WithArgs(id, args) => (Some(id), args),
                BindingTarget::Unbind(()) => (None, Value::Null),
            };
            let info = match &command {
                Some(id) => match registry.get(id.as_str()) {
                    Some(info) => Some(info),
                    None => {
                        report(position, format!("unknown command id {id}; binding dropped"), Severity::Warning);
                        continue;
                    }
                },
                None => None,
            };
            // An unbind may hide a binding wherever that binding fires, text fields included.
            let in_text = names_text_input || info.as_ref().is_none_or(|info| info.in_text);
            block_bindings.push(CompiledBinding {
                keys,
                context: context.clone(),
                context_text: block.context.clone(),
                command: command.map(String::from),
                args,
                when: info.and_then(|info| info.when),
                in_text,
                source: SourceLine { file: source.file.to_owned(), line: position.line, col: position.col },
            });
        }
        // Bindings iterate in key order; keep the file's order for the keyboard sheet.
        block_bindings.sort_by_key(|binding| (binding.source.line, binding.source.col));
        out.keymap.bindings.extend(block_bindings);
    }
}

fn is_bare_letter(keys: &[String]) -> bool {
    matches!(keys, [only] if only.len() == 1 && only.bytes().all(|b| b.is_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use pdfcraft_engine::commands::{COMMANDS, Shortcut};

    use super::*;

    fn options() -> CompileOptions {
        CompileOptions { os: Os::Linux, chord_timeout_ms: 1000, annotate_letters: true }
    }

    #[test]
    fn shipped_keymap_compiles_without_problems() {
        let compiled = compile_shipped(&Registry::load().unwrap(), &options());
        assert_eq!(compiled.problems, [], "{:#?}", compiled.problems);
        assert!(compiled.keymap.bindings.len() > 50);
        let zoom = compiled.keymap.bindings.iter().find(|b| b.keys == ["ctrl-="]).unwrap();
        assert_eq!(zoom.command.as_deref(), Some("view.zoom.in"));
        assert!(zoom.source.line > 1);
    }

    /// The modal annotate letters pick the comment tools (and select / snapshot).
    #[test]
    fn annotate_letters_pick_comment_tools() {
        let compiled = compile_shipped(&Registry::load().unwrap(), &options());
        let tool = |letter: &str| {
            let binding = compiled.keymap.bindings.iter().find(|b| b.keys == [letter] && b.context_text.as_deref() == Some("AnnotateMode"));
            binding.and_then(|b| b.command.clone()).unwrap_or_default()
        };
        let expected = [
            ("v", "reader.tool.select"),
            ("c", "edit.snapshot"),
            ("h", "comment.highlight"),
            ("u", "comment.underline"),
            ("s", "comment.strikeout"),
            ("q", "comment.squiggly"),
            ("n", "comment.note"),
            ("p", "comment.ink"),
            ("t", "comment.freetext"),
            ("r", "comment.square"),
            ("e", "comment.eraser"),
        ];
        for (letter, id) in expected {
            assert_eq!(tool(letter), id, "{letter}");
        }
    }

    /// Bindings keep the order the file writes them in, not key order.
    #[test]
    fn bindings_keep_file_order() {
        let text = "[{ \"context\": \"Reader\", \"bindings\": { \"z\": \"view.zoom.in\", \"a\": \"view.zoom.out\" } }]";
        let compiled = compile(&Registry::load().unwrap(), &[KeymapSource { file: "k", text }], &options());
        let keys: Vec<&str> = compiled.keymap.bindings.iter().map(|b| b.keys[0].as_str()).collect();
        assert_eq!(keys, ["z", "a"]);
        assert_eq!(compiled.keymap.bindings[0].context_text.as_deref(), Some("Reader"));
    }

    /// PdfCraft's `Shortcut` as a Zed keystroke: `command` is `secondary-`, `mac_ctrl` adds `ctrl-`.
    fn zed_keystroke(shortcut: &Shortcut) -> String {
        let mut text = String::new();
        if shortcut.mac_ctrl {
            text.push_str("ctrl-");
        }
        if shortcut.command {
            text.push_str("secondary-");
        }
        if shortcut.shift {
            text.push_str("shift-");
        }
        text.push_str(&shortcut.key.to_lowercase());
        text
    }

    /// CU-KEY-7: every PdfCraft shortcut is bound to the same id in margin.jsonc on every OS, or
    /// listed in `$pdfcraft_ignored` with a reason.
    #[test]
    fn every_pdfcraft_shortcut_is_bound_or_ignored() {
        let registry = Registry::load().unwrap();
        for os in [Os::Linux, Os::Windows, Os::Macos] {
            let compiled = compile_shipped(&registry, &CompileOptions { os, ..options() });
            for spec in COMMANDS {
                let Some(shortcut) = &spec.shortcut else { continue };
                if compiled.pdfcraft_ignored.get(spec.id).is_some_and(|reason| !reason.trim().is_empty()) {
                    continue;
                }
                let keys = keystroke::canonical_chord(&zed_keystroke(shortcut), os).unwrap();
                let bound = compiled.keymap.bindings.iter().any(|b| b.keys == keys && b.command.as_deref() == Some(spec.id));
                assert!(bound, "{os:?}: PdfCraft {} ({}) is neither bound nor in $pdfcraft_ignored", spec.id, shortcut.label(os == Os::Macos));
            }
        }
    }

    #[test]
    fn bad_bindings_are_dropped_with_a_line() {
        let text = "[\n { \"context\": \"Reader\", \"bindings\": {\n  \"ctrl-k\": \"no.such\",\n  \"shift-1\": \"view.zoom.in\",\n  \"x\": \"view.zoom.in\" } },\n { \"context\": \"Reader &&\" }\n]";
        let compiled = compile(&Registry::load().unwrap(), &[KeymapSource { file: "keymap.jsonc", text }], &options());
        let lines: Vec<u32> = compiled.problems.iter().map(|p| p.line).collect();
        assert_eq!(lines, [3, 4, 6], "{:#?}", compiled.problems);
        assert_eq!(compiled.keymap.bindings.len(), 1);
    }

    #[test]
    fn annotate_letters_off_drops_bare_letters() {
        let registry = Registry::load().unwrap();
        let on = compile_shipped(&registry, &options());
        let off = compile_shipped(&registry, &CompileOptions { annotate_letters: false, ..options() });
        let annotate = |c: &Compiled| c.keymap.bindings.iter().filter(|b| b.context.as_ref().is_some_and(|e| e.mentions("AnnotateMode"))).count();
        assert!(annotate(&on) > 0);
        assert_eq!(annotate(&off), 0);
    }

    #[test]
    fn broken_files_never_panic() {
        for text in ["", "{", "{}", "[1]", "[{ \"bindings\": 3 }]", "[{ \"bindings\": { \"a\": [1, 2] } }]"] {
            let compiled = compile(&Registry::load().unwrap(), &[KeymapSource { file: "k", text }], &options());
            assert!(compiled.keymap.bindings.is_empty(), "{text}");
        }
    }
}
