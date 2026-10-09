//! The runtime validator. It walks the generated settings schema, so the schema and the runtime
//! accept and reject the same values by construction (M0.9a); a test runs both over the same
//! fixtures to prove it.
//!
//! Rules (CU-VAL-2): an unknown key is dropped with a warning; a wrong type or unknown enum value
//! is dropped, so the layer below shows through, with a warning; a number out of range is
//! clamped with a warning. `null` means "not set at this layer" (CU-SET-2) and is dropped silently.
//!
//! Supported keywords, the subset `schemars` emits for [`crate::Settings`]: `$ref` (local),
//! `type` (string or list), `properties`, `additionalProperties`, `items`, `minItems`,
//! `maxItems`, `minimum`, `maximum`, `enum`, `const`, `oneOf` and `anyOf`.

use serde_json::{Map, Number, Value};

use crate::jsonc::{Positions, escape_pointer};
use crate::problem::{Problem, Severity};

/// Bound on schema `$ref` and nesting depth while validating (STD-RS-06).
const MAX_DEPTH: usize = 64;

/// The schema that accepts anything (`true`), for keys a schema leaves open.
static ANY: Value = Value::Bool(true);

/// Largest edit distance for a "did you mean" hint.
const MAX_SUGGESTION_DISTANCE: usize = 2;

/// One validation run over one file.
pub(crate) struct Validator<'a> {
    /// The root schema; `$ref`s resolve against its `$defs`.
    pub root: &'a Value,
    /// The layer below, for "using X" in messages.
    pub fallback: &'a Value,
    /// Property positions in the file.
    pub positions: &'a Positions,
    /// The file name problems carry.
    pub file: &'a str,
    /// Problems found so far.
    pub problems: Vec<Problem>,
}

/// A path inside the document, kept both as dotted key and JSON pointer.
#[derive(Clone, Default)]
struct KeyPath {
    segments: Vec<String>,
}

impl KeyPath {
    fn child(&self, segment: &str) -> Self {
        let mut segments = self.segments.clone();
        segments.push(segment.to_owned());
        Self { segments }
    }

    fn dotted(&self) -> String {
        self.segments.join(".")
    }

    fn pointer(&self) -> String {
        self.segments.iter().map(|segment| format!("/{}", escape_pointer(segment))).collect()
    }
}

impl Validator<'_> {
    /// Validates a whole document. Returns the cleaned value: every problem removed or clamped.
    pub fn document(&mut self, value: Value) -> Value {
        self.check(self.root, value, &KeyPath::default(), 0).unwrap_or_else(|| Value::Object(Map::new()))
    }

    fn report(&mut self, path: &KeyPath, message: String) {
        let position = self.positions.find(&path.pointer());
        self.problems.push(Problem {
            file: self.file.to_owned(),
            line: position.line,
            col: position.col,
            key: path.dotted(),
            message,
            severity: Severity::Warning,
        });
    }

    fn fallback_text(&self, path: &KeyPath) -> String {
        let found = path.segments.iter().try_fold(self.fallback, |node, segment| node.get(segment));
        found.map_or_else(|| "the default".to_owned(), |value| format!("using {value}"))
    }

    /// Checks `value` against `schema`. `None` means the value is dropped (the problem is
    /// already recorded, or the value was `null`).
    fn check(&mut self, schema: &Value, value: Value, path: &KeyPath, depth: usize) -> Option<Value> {
        if value.is_null() {
            return None;
        }
        if depth > MAX_DEPTH {
            self.report(path, format!("{} nests too deeply; {}", path.dotted(), self.fallback_text(path)));
            return None;
        }
        let Some(schema) = resolve(self.root, schema) else {
            self.report(path, format!("{} has no schema; ignored", path.dotted()));
            return None;
        };
        if let Some(allowed) = allowed_values(schema) {
            if allowed.iter().any(|candidate| json_eq(candidate, &value)) {
                return Some(value);
            }
            let names: Vec<String> = allowed.iter().map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned)).collect();
            let message = format!("{} {value} is not {}; {}", path.dotted(), names.join("|"), self.fallback_text(path));
            self.report(path, message);
            return None;
        }
        let value = self.check_type(schema, value, path)?;
        if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
            let mut real = branches.iter().filter(|branch| branch.get("type").and_then(Value::as_str) != Some("null"));
            match (real.next(), real.next()) {
                // `anyOf: [S, null]` is a nullable key (CU-SET-2): check S itself, with its messages.
                (Some(only), None) => return self.check(only, value, path, depth.saturating_add(1)),
                _ => {
                    let matches = branches.iter().any(|branch| {
                        let mut probe = self.probe();
                        probe.check(branch, value.clone(), path, depth.saturating_add(1)).is_some() && probe.problems.is_empty()
                    });
                    if !matches {
                        let message = format!("{} {value} is outside the allowed values; {}", path.dotted(), self.fallback_text(path));
                        self.report(path, message);
                        return None;
                    }
                }
            }
        }
        match value {
            Value::Number(number) => Some(self.check_range(schema, number, path)),
            Value::Array(items) => self.check_array(schema, items, path, depth),
            Value::Object(map) => Some(self.check_object(schema, map, path, depth)),
            other => Some(other),
        }
    }

    fn probe(&self) -> Validator<'_> {
        Validator { root: self.root, fallback: self.fallback, positions: self.positions, file: self.file, problems: Vec::new() }
    }

    /// Checks `type`; an integer-valued float such as `100.0` counts as an integer, as in JSON
    /// Schema, and is normalised to `100`.
    fn check_type(&mut self, schema: &Value, value: Value, path: &KeyPath) -> Option<Value> {
        let types: Vec<&str> = match schema.get("type") {
            Some(Value::String(one)) => vec![one.as_str()],
            Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
            _ => return Some(value),
        };
        let integer = as_integer(&value);
        let fits = types.iter().any(|ty| match *ty {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "number" => value.is_number(),
            "integer" => integer.is_some(),
            _ => false,
        });
        if !fits {
            let wanted = types.iter().copied().filter(|ty| *ty != "null").collect::<Vec<_>>().join(" or ");
            let message = format!("{} expects {wanted}, got {}; {}", path.dotted(), type_name(&value), self.fallback_text(path));
            self.report(path, message);
            return None;
        }
        let wants_integer = types.contains(&"integer") && !types.contains(&"number");
        match integer {
            Some(normalised) if wants_integer => Some(normalised),
            _ => Some(value),
        }
    }

    fn check_range(&mut self, schema: &Value, number: Number, path: &KeyPath) -> Value {
        let Some(n) = number.as_f64() else { return Value::Number(number) };
        let minimum = schema.get("minimum").and_then(Value::as_f64);
        let maximum = schema.get("maximum").and_then(Value::as_f64);
        let bound = match (minimum, maximum) {
            (Some(min), _) if n < min => schema.get("minimum"),
            (_, Some(max)) if n > max => schema.get("maximum"),
            _ => None,
        };
        match bound.and_then(Value::as_number) {
            Some(limit) => {
                self.report(path, format!("{} {number} clamped to {limit}", path.dotted()));
                Value::Number(limit.clone())
            }
            None => Value::Number(number),
        }
    }

    fn check_array(&mut self, schema: &Value, items: Vec<Value>, path: &KeyPath, depth: usize) -> Option<Value> {
        let count = u64::try_from(items.len()).unwrap_or(u64::MAX);
        let min = schema.get("minItems").and_then(Value::as_u64);
        let max = schema.get("maxItems").and_then(Value::as_u64);
        if min.is_some_and(|min| count < min) || max.is_some_and(|max| count > max) {
            let range = format!("{}..{}", min.unwrap_or(0), max.map_or_else(String::new, |max| max.to_string()));
            let message = format!("{} has {count} items, allowed {range}; {}", path.dotted(), self.fallback_text(path));
            self.report(path, message);
            return None;
        }
        let Some(item_schema) = schema.get("items") else { return Some(Value::Array(items)) };
        let mut checked = Vec::with_capacity(items.len());
        for (index, item) in items.into_iter().enumerate() {
            let item_path = path.child(&index.to_string());
            // A dropped item (wrong type, or null) would silently change the list: drop it whole.
            let item_was_null = item.is_null();
            match self.check(item_schema, item, &item_path, depth.saturating_add(1)) {
                Some(item) => checked.push(item),
                None => {
                    if item_was_null {
                        let message = format!("{} contains null; {}", path.dotted(), self.fallback_text(path));
                        self.report(&item_path, message);
                    }
                    return None;
                }
            }
        }
        Some(Value::Array(checked))
    }

    fn check_object(&mut self, schema: &Value, map: Map<String, Value>, path: &KeyPath, depth: usize) -> Value {
        let properties = schema.get("properties").and_then(Value::as_object);
        let additional = schema.get("additionalProperties");
        let mut checked = Map::new();
        for (key, value) in map {
            let key_path = path.child(&key);
            let property_schema = match (properties.and_then(|props| props.get(&key)), additional) {
                (Some(known), _) => Some(known),
                (None, Some(Value::Bool(false))) => None,
                (None, Some(Value::Object(entry))) if !entry.is_empty() => additional,
                (None, _) => Some(&ANY),
            };
            let Some(property_schema) = property_schema else {
                let hint = properties
                    .and_then(|props| suggest(&key, props.keys()))
                    .map_or_else(String::new, |near| format!(" (did you mean {}?)", path.child(near).dotted()));
                self.report(&key_path, format!("unknown key {}{hint}", key_path.dotted()));
                continue;
            };
            if let Some(value) = self.check(property_schema, value, &key_path, depth.saturating_add(1)) {
                checked.insert(key, value);
            }
        }
        Value::Object(checked)
    }
}

/// `$ref` (local, `#/$defs/<name>`) to the schema it names, following chains up to [`MAX_DEPTH`].
fn resolve<'s>(root: &'s Value, schema: &'s Value) -> Option<&'s Value> {
    let mut current = schema;
    for _ in 0..MAX_DEPTH {
        let Some(reference) = current.get("$ref").and_then(Value::as_str) else { return Some(current) };
        let name = reference.strip_prefix("#/$defs/")?;
        current = root.get("$defs")?.get(name)?;
    }
    None
}

/// JSON equality where numbers compare by value, so `0` equals `0.0` as in JSON Schema `const`.
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(xs), Value::Array(ys)) => xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| json_eq(x, y)),
        (Value::Object(xs), Value::Object(ys)) => xs.len() == ys.len() && xs.iter().all(|(k, x)| ys.get(k).is_some_and(|y| json_eq(x, y))),
        _ => a == b,
    }
}

/// The closed set of values a schema allows (`enum`, `const`, or a `oneOf` of those), if any.
/// A `null` member is the layer marker and is ignored.
fn allowed_values(schema: &Value) -> Option<Vec<Value>> {
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return Some(values.iter().filter(|v| !v.is_null()).cloned().collect());
    }
    if let Some(value) = schema.get("const") {
        return Some(vec![value.clone()]);
    }
    let branches = schema.get("oneOf").and_then(Value::as_array)?;
    let mut values = Vec::new();
    for branch in branches {
        match (branch.get("const"), branch.get("enum").and_then(Value::as_array)) {
            (Some(value), _) => values.push(value.clone()),
            (None, Some(many)) => values.extend(many.iter().cloned()),
            (None, None) if branch.get("type").and_then(Value::as_str) == Some("null") => {}
            (None, None) => return None,
        }
    }
    Some(values.into_iter().filter(|v| !v.is_null()).collect())
}

/// The value as a JSON integer, if it is one or is an integer-valued float.
fn as_integer(value: &Value) -> Option<Value> {
    let Value::Number(number) = value else { return None };
    if number.is_i64() || number.is_u64() {
        return Some(value.clone());
    }
    let float = number.as_f64()?;
    // Exact for |float| < 2^53; anything larger is not an integer a setting could hold.
    const MAX_EXACT: f64 = 9_007_199_254_740_992.0;
    if float.is_finite() && float.fract() == 0.0 && float.abs() < MAX_EXACT {
        #[expect(clippy::cast_possible_truncation, reason = "finite, integral and below 2^53, so exact")]
        let int = float as i64;
        return Some(Value::Number(Number::from(int)));
    }
    None
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// The closest known key within [`MAX_SUGGESTION_DISTANCE`] edits.
fn suggest<'k>(key: &str, known: impl Iterator<Item = &'k String>) -> Option<&'k str> {
    known
        .map(|candidate| (edit_distance(key, candidate), candidate))
        .filter(|(distance, _)| *distance <= MAX_SUGGESTION_DISTANCE)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate.as_str())
}

/// Levenshtein distance over chars.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = Vec::with_capacity(b.len().saturating_add(1));
        current.push(i.saturating_add(1));
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous.get(j).copied().unwrap_or(usize::MAX).saturating_add(usize::from(ca != *cb));
            let delete = previous.get(j.saturating_add(1)).copied().unwrap_or(usize::MAX).saturating_add(1);
            let insert = current.last().copied().unwrap_or(usize::MAX).saturating_add(1);
            current.push(substitute.min(delete).min(insert));
        }
        previous = current;
    }
    previous.last().copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_counts_single_edits() {
        assert_eq!(edit_distance("page_colour", "page_color"), 1);
        assert_eq!(edit_distance("zoom", "zoom"), 0);
        assert_eq!(edit_distance("", "abc"), 3);
    }

    #[test]
    fn integer_valued_floats_are_integers() {
        assert_eq!(as_integer(&serde_json::json!(100.0)), Some(serde_json::json!(100)));
        assert_eq!(as_integer(&serde_json::json!(100.5)), None);
    }
}
