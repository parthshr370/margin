//! The keymap resolver cases. The cases and their expected results are written by hand in
//! `data/keymap-cases.jsonc`; this module compiles the tables they name, expands their stacks and
//! runs every case through [`crate::resolve::step`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::{ContextNode, parse_stack};
use crate::error::{Error, Result};
use crate::jsonc;
use crate::keymap::{CompileOptions, CompiledKeymap, KeymapSource, MARGIN_KEYMAP, MARGIN_KEYMAP_FILE, compile};
use crate::keystroke::Os;
use crate::registry::Registry;
use crate::resolve::{Outcome, step, timeout};

/// The hand-written cases.
pub const KEYMAP_CASES: &str = include_str!("../data/keymap-cases.jsonc");

/// The repository path of [`KEYMAP_CASES`].
pub const KEYMAP_CASES_FILE: &str = "crates/margin-config/data/keymap-cases.jsonc";

/// The user keymap of the `user` table.
const TEST_USER_KEYMAP: &str = include_str!("../data/keymap-test-user.jsonc");

/// The repository path of [`TEST_USER_KEYMAP`].
const TEST_USER_KEYMAP_FILE: &str = "crates/margin-config/data/keymap-test-user.jsonc";

/// Chord timeout the vector tables carry.
const VECTOR_CHORD_TIMEOUT_MS: u32 = 1000;

/// One input of a case: a key, or the chord timeout firing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseStep {
    /// A canonical keystroke.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The chord timeout fires instead of a key.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timeout: bool,
    /// The expected result.
    pub expect: Outcome,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseSource {
    name: String,
    table: String,
    stack: String,
    steps: Vec<CaseStep>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CasesFile {
    cases: Vec<CaseSource>,
}

/// One case with its stack expanded.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Case {
    /// What it checks.
    pub name: String,
    /// The table it runs against.
    pub table: String,
    /// The focused stack, outermost first.
    pub stack: Vec<ContextNode>,
    /// Inputs and expected results, in order.
    pub steps: Vec<CaseStep>,
}

/// The file Vitest reads: compiled tables and the cases.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Vectors {
    /// Table name to compiled table.
    pub tables: BTreeMap<String, CompiledKeymap>,
    /// Every case.
    pub cases: Vec<Case>,
}

/// Builds the vectors. Fails if a case, a stack or a vector keymap is malformed.
pub fn keymap_vectors() -> Result<Vectors> {
    let registry = Registry::load()?;
    let options = CompileOptions { os: Os::Linux, chord_timeout_ms: VECTOR_CHORD_TIMEOUT_MS, annotate_letters: true };
    let margin = KeymapSource { file: MARGIN_KEYMAP_FILE, text: MARGIN_KEYMAP };
    let user = KeymapSource { file: TEST_USER_KEYMAP_FILE, text: TEST_USER_KEYMAP };
    let mut tables = BTreeMap::new();
    for (name, sources) in [("margin", vec![margin]), ("user", vec![margin, user])] {
        let compiled = compile(&registry, &sources, &options);
        if let Some(problem) = compiled.problems.first() {
            return Err(Error::Data { file: KEYMAP_CASES_FILE, line: problem.line, col: problem.col, reason: format!("table {name}: {problem}") });
        }
        tables.insert(name.to_owned(), compiled.keymap);
    }
    let data_error =
        |position: jsonc::Position, reason: String| Error::Data { file: KEYMAP_CASES_FILE, line: position.line, col: position.col, reason };
    let parsed = jsonc::parse(KEYMAP_CASES).map_err(|err| data_error(err.position, err.message))?;
    let file: CasesFile =
        serde_json::from_value(parsed.value.unwrap_or(Value::Null)).map_err(|err| data_error(jsonc::Position::default(), err.to_string()))?;
    let mut cases = Vec::with_capacity(file.cases.len());
    for (index, case) in file.cases.into_iter().enumerate() {
        let position = parsed.positions.find(&format!("/cases/{index}"));
        if !tables.contains_key(&case.table) {
            return Err(data_error(position, format!("unknown table {}", case.table)));
        }
        if case.steps.iter().any(|step| step.key.is_some() == step.timeout) {
            return Err(data_error(position, "each step has either a key or \"timeout\": true".to_owned()));
        }
        let stack = parse_stack(&case.stack).map_err(|err| data_error(position, err.to_string()))?;
        cases.push(Case { name: case.name, table: case.table, stack, steps: case.steps });
    }
    Ok(Vectors { tables, cases })
}

/// Runs one case with the Rust resolver; returns the outcome of each step.
pub fn run_case(table: &CompiledKeymap, case: &Case) -> Vec<Outcome> {
    let mut pending = Vec::new();
    let mut outcomes = Vec::with_capacity(case.steps.len());
    for input in &case.steps {
        let outcome = match &input.key {
            Some(key) => {
                let next = step(table, &case.stack, &pending, key);
                pending = next.pending;
                next.outcome
            }
            None => {
                let outcome = timeout(table, &case.stack, &pending);
                pending.clear();
                outcome
            }
        };
        outcomes.push(outcome);
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_resolver_passes_every_vector() {
        let vectors = keymap_vectors().unwrap();
        assert!(vectors.cases.len() >= 20);
        for case in &vectors.cases {
            let table = &vectors.tables[&case.table];
            let expected: Vec<&Outcome> = case.steps.iter().map(|step| &step.expect).collect();
            let actual = run_case(table, case);
            assert_eq!(actual.iter().collect::<Vec<_>>(), expected, "case {:?}", case.name);
        }
    }
}
