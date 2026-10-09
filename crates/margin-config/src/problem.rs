//! `Problem`: one thing wrong with a config file, for the Problems list, the log and a toast
//! (CU-VAL-1). Loading never fails; it returns usable values plus problems.

use std::fmt;

use serde::Serialize;

/// How bad a problem is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The file could not be used; Margin kept the layer below.
    Error,
    /// A value was ignored, replaced or clamped.
    Warning,
    /// Worth knowing, nothing was changed.
    Info,
}

/// One problem in one file, at `line:col` (1-based).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// The file, as shown to the user.
    pub file: String,
    /// 1-based line.
    pub line: u32,
    /// 1-based column, in characters.
    pub col: u32,
    /// The dotted key, empty for the whole file.
    pub key: String,
    /// What was wrong and what Margin did instead.
    pub message: String,
    /// How bad it is.
    pub severity: Severity,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{} {}", self.file, self.line, self.col, self.message)
    }
}
