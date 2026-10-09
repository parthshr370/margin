//! JSONC text to a `serde_json::Value`, keeping where each property sits so problems can name
//! `file:line:col` (CU-VAL-1). Comments and trailing commas are allowed, as in VS Code and Zed
//! settings files; the JSON5 extras (hex, `+1`, `.5`, `NaN`, single quotes, bare keys) are not.

use std::collections::HashMap;

use jsonc_parser::ast;
use jsonc_parser::common::Ranged;
use serde_json::{Map, Number, Value};

/// A 1-based line and column in a file. Columns count characters, not bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position {
    /// 1-based line.
    pub line: u32,
    /// 1-based column, in characters.
    pub col: u32,
}

/// Where each property name starts, keyed by JSON pointer (`/view/zoom`).
#[derive(Debug, Default)]
pub struct Positions(HashMap<String, Position>);

impl Positions {
    /// The position of the property at `pointer`, or of its nearest parent that has one.
    pub fn find(&self, pointer: &str) -> Position {
        let mut current = pointer;
        loop {
            if let Some(position) = self.0.get(current) {
                return *position;
            }
            match current.rfind('/') {
                Some(cut) => current = current.get(..cut).unwrap_or_default(),
                None => return Position { line: 1, col: 1 },
            }
        }
    }
}

/// Why JSONC text could not be read.
#[derive(Debug)]
pub struct SyntaxError {
    /// Where the parser stopped.
    pub position: Position,
    /// The parser's message, without its position suffix.
    pub message: String,
}

/// A parsed document: its value (`None` for an empty file) and the property positions.
#[derive(Debug)]
pub struct Parsed {
    /// The document's value; `None` when the file holds only whitespace and comments.
    pub value: Option<Value>,
    /// Where each property name starts.
    pub positions: Positions,
}

fn strict_options() -> jsonc_parser::ParseOptions {
    jsonc_parser::ParseOptions {
        allow_comments: true,
        allow_loose_object_property_names: false,
        allow_trailing_commas: true,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    }
}

/// Parses JSONC text. Nesting is bounded by the parser (512 levels, STD-RS-06).
pub fn parse(text: &str) -> Result<Parsed, SyntaxError> {
    let parsed = jsonc_parser::parse_to_ast(text, &jsonc_parser::CollectOptions::default(), &strict_options()).map_err(|err| SyntaxError {
        position: Position { line: saturating_u32(err.line_display()), col: saturating_u32(err.column_display()) },
        message: err.kind().to_string(),
    })?;
    let lines = LineIndex::new(text);
    let mut positions = Positions::default();
    let value = match parsed.value {
        Some(root) => Some(convert(&root, &mut String::new(), &lines, &mut positions)?),
        None => None,
    };
    Ok(Parsed { value, positions })
}

fn convert(node: &ast::Value<'_>, pointer: &mut String, lines: &LineIndex, positions: &mut Positions) -> Result<Value, SyntaxError> {
    Ok(match node {
        ast::Value::StringLit(lit) => Value::String(lit.value.to_string()),
        ast::Value::BooleanLit(lit) => Value::Bool(lit.value),
        ast::Value::NullKeyword(_) => Value::Null,
        ast::Value::NumberLit(lit) => {
            let number: Number = serde_json::from_str(lit.value)
                .map_err(|_| SyntaxError { position: lines.position(lit.range.start), message: format!("`{}` is not a JSON number", lit.value) })?;
            Value::Number(number)
        }
        ast::Value::Array(array) => {
            let mut items = Vec::with_capacity(array.elements.len());
            for (index, element) in array.elements.iter().enumerate() {
                let len = pointer.len();
                pointer.push('/');
                pointer.push_str(&index.to_string());
                positions.0.insert(pointer.clone(), lines.position(element.start()));
                items.push(convert(element, pointer, lines, positions)?);
                pointer.truncate(len);
            }
            Value::Array(items)
        }
        ast::Value::Object(object) => {
            let mut map = Map::new();
            for property in &object.properties {
                let name = property.name.as_str();
                let len = pointer.len();
                pointer.push('/');
                pointer.push_str(&escape_pointer(name));
                positions.0.insert(pointer.clone(), lines.position(property.name.start()));
                let value = convert(&property.value, pointer, lines, positions)?;
                pointer.truncate(len);
                map.insert(name.to_owned(), value);
            }
            Value::Object(map)
        }
    })
}

/// Escapes one JSON pointer segment (RFC 6901).
pub fn escape_pointer(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// The property names of a JSON object in text order, with the same for each object-valued
/// property. `serde_json::Map` sorts its keys (the workspace does not enable `preserve_order`),
/// so generated files take their order from serialised text instead.
#[derive(Debug, Default)]
pub struct KeyOrder(pub Vec<(String, KeyOrder)>);

/// The key order of `text`'s root object; empty when it does not parse or is not an object.
pub fn key_order(text: &str) -> KeyOrder {
    let parsed = jsonc_parser::parse_to_ast(text, &jsonc_parser::CollectOptions::default(), &strict_options());
    parsed.ok().and_then(|parsed| parsed.value).map_or_else(KeyOrder::default, |root| order_of(&root))
}

fn order_of(node: &ast::Value<'_>) -> KeyOrder {
    match node {
        ast::Value::Object(object) => KeyOrder(object.properties.iter().map(|p| (p.name.as_str().to_owned(), order_of(&p.value))).collect()),
        _ => KeyOrder::default(),
    }
}

fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Byte offset to line and column.
struct LineIndex<'a> {
    text: &'a str,
    /// Byte offset of the start of each line.
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    fn new(text: &'a str) -> Self {
        let starts = std::iter::once(0).chain(text.match_indices('\n').map(|(at, _)| at.saturating_add(1))).collect();
        Self { text, starts }
    }

    fn position(&self, offset: usize) -> Position {
        let line = self.starts.partition_point(|&start| start <= offset).saturating_sub(1);
        let start = self.starts.get(line).copied().unwrap_or(0);
        let col = self.text.get(start..offset).map_or(0, |prefix| prefix.chars().count());
        Position { line: saturating_u32(line.saturating_add(1)), col: saturating_u32(col.saturating_add(1)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_point_at_property_names() {
        let parsed = parse("{ \"view\": { \"zoom\": 400,\n  \"fit\": \"page\" } } // tail").unwrap();
        assert_eq!(parsed.positions.find("/view/zoom"), Position { line: 1, col: 13 });
        assert_eq!(parsed.positions.find("/view/fit"), Position { line: 2, col: 3 });
        assert_eq!(parsed.positions.find("/view/fit/deeper"), Position { line: 2, col: 3 });
        assert_eq!(parsed.value.unwrap()["view"]["zoom"], 400);
    }

    #[test]
    fn json5_extras_are_syntax_errors() {
        for text in ["{ zoom: 1 }", "{ \"a\": 0x10 }", "{ \"a\": 'x' }", "{ \"a\": NaN }", "{ \"a\": .5 }"] {
            assert!(parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn comments_trailing_commas_and_empty_files_parse() {
        assert!(parse("// only a comment\n").unwrap().value.is_none());
        assert_eq!(parse("{ \"a\": [1, 2,], /* c */ }").unwrap().value.unwrap()["a"][1], 2);
    }

    #[test]
    fn syntax_error_reports_line_and_column() {
        let err = parse("{\n  \"a\": 1\n  \"b\": 2 }").unwrap_err();
        assert_eq!(err.position.line, 2, "the parser stops after the value that lacks a comma");
    }
}
