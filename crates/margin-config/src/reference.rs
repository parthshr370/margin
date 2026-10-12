//! Bibliographic details of one document, as an import (BibTeX, Zotero), an online lookup
//! (Crossref, arXiv) or the file itself describes it. Every source is untrusted: strings are
//! trimmed, stripped of control characters and capped.

use std::path::PathBuf;

/// Longest title, author, publisher or identifier kept.
pub const MAX_FIELD_CHARS: usize = 512;
/// Longest abstract kept.
pub const MAX_ABSTRACT_CHARS: usize = 8192;
/// Longest keyword (a library tag) kept.
pub const MAX_KEYWORD_CHARS: usize = 64;

/// What a source says about a document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reference {
    pub title: Option<String>,
    /// Display order "First Last", one per author.
    pub authors: Vec<String>,
    pub year: Option<i32>,
    /// Normalised: lower-case, no "doi:" / "https://doi.org/" prefix, e.g. "10.1145/3290605.3300857".
    pub doi: Option<String>,
    /// Normalised: version suffix removed, e.g. "2101.01234" or "hep-th/9901001".
    pub arxiv: Option<String>,
    /// Journal, proceedings or publisher, whichever the source has (journal first).
    pub publisher: Option<String>,
    pub isbn: Option<String>,
    pub abstract_text: Option<String>,
    /// Keywords, to become library tags. Deduplicated case-insensitively.
    pub keywords: Vec<String>,
    /// PDF files the source names (as given; relative ones are resolved by the caller).
    pub files: Vec<PathBuf>,
}

/// `value` trimmed, without control characters, at most `cap` characters.
pub(crate) fn clean(value: &str, cap: usize) -> String {
    value.trim().chars().filter(|c| !c.is_control()).take(cap).collect()
}
