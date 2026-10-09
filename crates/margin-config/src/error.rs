//! The error type of `margin-config`. Loading user files never returns it (problems are values);
//! it covers the shipped data files and the parsers' building blocks.

/// Every error `margin-config` returns. Variants carry context (file, line, id), not formatted
/// strings.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A context expression (`context`, `when`) does not parse (CU-KEY-4).
    #[error("context `{expr}`: {reason}")]
    Context {
        /// The expression, cut to 80 characters.
        expr: String,
        /// What is wrong.
        reason: &'static str,
    },
    /// A context stack in `name attr=value > …` notation does not parse.
    #[error("context stack `{stack}`: {reason}")]
    Stack {
        /// The stack text, cut to 80 characters.
        stack: String,
        /// What is wrong.
        reason: &'static str,
    },
    /// A keystroke does not parse (CU-KEY-2).
    #[error("keystroke `{keystroke}`: {reason}")]
    Keystroke {
        /// The keystroke text, cut to 80 characters.
        keystroke: String,
        /// What is wrong.
        reason: &'static str,
    },
    /// A JSONC data file shipped with Margin, or a test case file, is malformed.
    #[error("{file}:{line}:{col}: {reason}")]
    Data {
        /// The file.
        file: &'static str,
        /// 1-based line.
        line: u32,
        /// 1-based column.
        col: u32,
        /// What is wrong.
        reason: String,
    },
    /// A command id is not of the form `namespace.object[.verb]` (CU-CMD-1).
    #[error("command id `{id}` is not namespace.object[.verb] with [a-z0-9_] segments")]
    BadId {
        /// The rejected id, cut to 80 characters.
        id: String,
    },
}
/// `Result` with [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;
