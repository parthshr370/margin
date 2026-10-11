//! Online lookup of a paper's details: Crossref by DOI, the arXiv API by arXiv id. Only ever run
//! when the user asks ("Look up details") or turned `library.metadata_lookup` on: nothing else in
//! Margin talks to the network. Answers are untrusted: sizes are capped, strings cleaned.

use crate::ids::{normalize_arxiv, normalize_doi};
use crate::reference::{MAX_ABSTRACT_CHARS, MAX_FIELD_CHARS, MAX_KEYWORD_CHARS, Reference};
use roxmltree::Document;
use serde_json::Value;
use std::time::Duration;

/// Seconds a lookup waits for an answer.
const TIMEOUT_SECS: u64 = 10;
/// Largest answer read.
const MAX_ANSWER_BYTES: u64 = 4 << 20;
/// The HTTP status of an id the service doesn't know.
const NOT_FOUND: u16 = 404;
/// Most authors kept from an answer.
const MAX_AUTHORS: usize = 128;
/// Most Crossref subjects, and arXiv categories, kept as keywords.
const MAX_SUBJECTS: usize = 128;
const MAX_CATEGORIES: usize = 8;

/// Why a lookup gave no details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookupError {
    /// The service couldn't be reached or is busy (offline, DNS, TLS, timeout, rate limit): it
    /// may work later.
    Offline(String),
    /// The service has no record of this id.
    NotFound(String),
    /// The answer couldn't be read, or there was nothing to ask about.
    Invalid(String),
}

impl std::fmt::Display for LookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Offline(reason) | Self::NotFound(reason) | Self::Invalid(reason) => f.write_str(reason),
        }
    }
}

/// A Crossref `/works/{doi}` answer.
pub fn parse_crossref(json: &str) -> Result<Reference, String> {
    let root: Value = serde_json::from_str(json).map_err(|error| format!("invalid Crossref answer ({error})"))?;
    let message = root.get("message").ok_or_else(|| "Crossref answer has no message".to_owned())?;
    let mut reference = Reference {
        title: first_string(message.get("title")),
        year: ["issued", "published-print", "published-online", "created"].iter().find_map(|key| date_year(message.get(key))),
        doi: message.get("DOI").and_then(Value::as_str).and_then(normalize_doi),
        publisher: first_string(message.get("container-title"))
            .or_else(|| message.get("publisher").and_then(Value::as_str).and_then(|value| clean(value, MAX_FIELD_CHARS))),
        isbn: first_string(message.get("ISBN")),
        abstract_text: message.get("abstract").and_then(Value::as_str).and_then(|value| clean(&strip_markup(value), MAX_ABSTRACT_CHARS)),
        ..Reference::default()
    };
    if let Some(authors) = message.get("author").and_then(Value::as_array) {
        for author in authors.iter().take(MAX_AUTHORS) {
            let name = author.get("name").and_then(Value::as_str).map(str::to_owned).or_else(|| {
                let given = author.get("given").and_then(Value::as_str).unwrap_or("");
                let family = author.get("family").and_then(Value::as_str).unwrap_or("");
                let full = format!("{given} {family}");
                (!full.trim().is_empty()).then_some(full)
            });
            if let Some(name) = name.and_then(|value| clean(&value, MAX_FIELD_CHARS)) {
                reference.authors.push(name);
            }
        }
    }
    if let Some(subjects) = message.get("subject").and_then(Value::as_array) {
        add_keywords(&mut reference, subjects.iter().filter_map(Value::as_str), MAX_SUBJECTS);
    }
    Ok(reference)
}

/// An arXiv API (Atom) answer: its first entry; `Ok(None)` when arXiv has no paper with the id.
pub fn parse_arxiv_atom(xml: &str) -> Result<Option<Reference>, String> {
    let document = Document::parse(xml).map_err(|error| format!("invalid arXiv answer ({error})"))?;
    let Some(entry) = document.descendants().find(|node| node.is_element() && node.tag_name().name() == "entry") else { return Ok(None) };
    let child_text = |name: &str| entry.children().find(|node| node.is_element() && node.tag_name().name() == name).and_then(|node| node.text());
    let title = child_text("title").unwrap_or("");
    if title.trim().eq_ignore_ascii_case("error") || title.trim().is_empty() {
        return Ok(None);
    }
    let mut reference = Reference {
        title: clean(&collapse_whitespace(title), MAX_FIELD_CHARS),
        year: child_text("published").and_then(year_from_date),
        abstract_text: child_text("summary").and_then(|value| clean(&collapse_whitespace(value), MAX_ABSTRACT_CHARS)),
        arxiv: child_text("id").and_then(normalize_arxiv),
        ..Reference::default()
    };
    for node in entry.descendants().filter(|node| node.is_element()) {
        let tag = node.tag_name();
        let local_name = tag.name();
        if local_name == "name"
            && tag.namespace() == Some("http://www.w3.org/2005/Atom")
            && reference.authors.len() < MAX_AUTHORS
            && let Some(name) = node.text().and_then(|value| clean(&collapse_whitespace(value), MAX_FIELD_CHARS))
        {
            reference.authors.push(name);
        }
        if tag.namespace() == Some("http://arxiv.org/schemas/atom") {
            if local_name == "doi" {
                reference.doi = node.text().and_then(normalize_doi);
            } else if local_name == "journal_ref" {
                reference.publisher = node.text().and_then(|value| clean(value, MAX_FIELD_CHARS));
            }
        }
        if local_name == "category"
            && let Some(term) = node.attribute("term")
        {
            add_keywords(&mut reference, std::iter::once(term), MAX_CATEGORIES);
        }
    }
    Ok(Some(reference))
}

/// The Crossref API address for `doi`.
pub fn crossref_url(doi: &str) -> String {
    let encoded: String = doi
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    format!("https://api.crossref.org/works/{encoded}")
}

/// The arXiv API address for `id`.
pub fn arxiv_url(id: &str) -> String {
    let encoded: String = id
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'/' | b'-' | b'_') {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    format!("https://export.arxiv.org/api/query?id_list={encoded}")
}

/// Asks arXiv (for an arXiv id) and Crossref (for the DOI, or else the DOI arXiv gave) about a
/// paper; blocking, so run it on a worker. Crossref's answer wins where both have a field.
pub fn lookup(doi: Option<&str>, arxiv: Option<&str>) -> Result<Reference, LookupError> {
    let mut arxiv_reference = match arxiv {
        Some(id) => {
            let xml = request(&arxiv_url(id), "export.arxiv.org")?;
            let parsed = parse_arxiv_atom(&xml).map_err(LookupError::Invalid)?;
            Some(parsed.ok_or_else(|| LookupError::NotFound(format!("arXiv has no paper {id}")))?)
        }
        None => None,
    };
    let given_doi = arxiv_reference.as_ref().and_then(|reference| reference.doi.clone());
    let Some(doi) = doi.map(str::to_owned).or(given_doi) else {
        return arxiv_reference.ok_or_else(|| LookupError::Invalid("add a DOI or an arXiv id to look this book up".to_owned()));
    };
    match request(&crossref_url(&doi), "api.crossref.org") {
        Ok(json) => {
            let mut crossref = parse_crossref(&json).map_err(LookupError::Invalid)?;
            if let Some(arxiv) = arxiv_reference.take() {
                merge_gaps(&mut crossref, arxiv);
            }
            Ok(crossref)
        }
        // arXiv's DOIs are DataCite's, which Crossref doesn't know: arXiv's answer stands.
        Err(error) => arxiv_reference.ok_or(error),
    }
}

fn request(url: &str, host: &str) -> Result<String, LookupError> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(os_roots().map_err(LookupError::Offline)?).build())
        .build()
        .new_agent();
    let mut response = agent.get(url).header("User-Agent", "Margin (https://github.com/parthshr370/margin)").call().map_err(|error| match error {
        ureq::Error::StatusCode(NOT_FOUND) => LookupError::NotFound(format!("{host} has no record of it")),
        error => LookupError::Offline(format!("couldn't reach {host} ({error})")),
    })?;
    response
        .body_mut()
        .with_config()
        .limit(MAX_ANSWER_BYTES)
        .read_to_string()
        .map_err(|error| LookupError::Invalid(format!("unreadable answer from {host} ({error})")))
}

fn os_roots() -> Result<ureq::tls::RootCerts, String> {
    let found = rustls_native_certs::load_native_certs();
    let certs: Vec<ureq::tls::Certificate<'static>> =
        found.certs.iter().map(|certificate| ureq::tls::Certificate::from_der(certificate.as_ref()).to_owned()).collect();
    if certs.is_empty() {
        return Err("no trusted certificates found on this system".into());
    }
    Ok(ureq::tls::RootCerts::new_with_certs(&certs))
}

fn first_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_array).and_then(|values| values.first()).and_then(Value::as_str).and_then(|text| clean(text, MAX_FIELD_CHARS))
}

fn date_year(value: Option<&Value>) -> Option<i32> {
    value?.get("date-parts")?.as_array()?.first()?.as_array()?.first()?.as_i64().and_then(|year| i32::try_from(year).ok())
}

/// The earliest year a date is believed for (arXiv's dates are all far later).
const MIN_YEAR: i32 = 1000;

fn year_from_date(value: &str) -> Option<i32> {
    let year = value.get(..4)?.parse::<i32>().ok()?;
    (year >= MIN_YEAR).then_some(year)
}

fn strip_markup(value: &str) -> String {
    let bounded: String = value.chars().take(MAX_ABSTRACT_CHARS).collect();
    if let Ok(document) = Document::parse(&format!("<root>{bounded}</root>")) {
        return document.root_element().descendants().filter_map(|node| node.text()).collect::<Vec<_>>().join(" ");
    }
    let mut output = String::with_capacity(MAX_ABSTRACT_CHARS);
    let mut in_tag = false;
    for character in bounded.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output
}

fn collapse_whitespace(value: &str) -> String {
    value.chars().take(MAX_ABSTRACT_CHARS).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clean(value: &str, maximum: usize) -> Option<String> {
    let mut output = String::new();
    let mut output_chars = 0;
    let mut pending_space = false;
    for character in value.chars() {
        if character.is_control() {
            continue;
        }
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if output_chars >= maximum {
            break;
        }
        if pending_space && output_chars < maximum {
            output.push(' ');
            output_chars += 1;
            pending_space = false;
        }
        if output_chars >= maximum {
            break;
        }
        output.push(character);
        output_chars += 1;
    }
    (!output.is_empty()).then_some(output)
}

fn add_keywords<'a>(reference: &mut Reference, values: impl Iterator<Item = &'a str>, maximum: usize) {
    for value in values {
        if reference.keywords.len() >= maximum {
            break;
        }
        if let Some(keyword) = clean(value, MAX_KEYWORD_CHARS)
            && !reference.keywords.iter().any(|existing| existing.eq_ignore_ascii_case(&keyword))
        {
            reference.keywords.push(keyword);
        }
    }
}

fn merge_gaps(target: &mut Reference, fallback: Reference) {
    target.title = target.title.take().or(fallback.title);
    if target.authors.is_empty() {
        target.authors = fallback.authors;
    }
    target.year = target.year.or(fallback.year);
    target.doi = target.doi.take().or(fallback.doi);
    target.arxiv = target.arxiv.take().or(fallback.arxiv);
    target.publisher = target.publisher.take().or(fallback.publisher);
    target.isbn = target.isbn.take().or(fallback.isbn);
    target.abstract_text = target.abstract_text.take().or(fallback.abstract_text);
    for keyword in fallback.keywords {
        if !target.keywords.iter().any(|existing| existing.eq_ignore_ascii_case(&keyword)) {
            target.keywords.push(keyword);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CROSSREF: &str = r#"{"message":{"title":["A useful paper"],"author":[{"given":"Ada","family":"Lovelace"},{"name":"Grace Hopper"}],"issued":{"date-parts":[[2020,2,3]]},"DOI":"10.1145/3290605.3300857","container-title":["Proceedings of Tests"],"publisher":"Publisher","ISBN":["978-1-2345-6789-0"],"abstract":"<jats:p>First <jats:italic>clear</jats:italic> abstract.</jats:p><jats:p>Second paragraph.</jats:p>","subject":["Computing","Computing","History"]}}"#;
    const ATOM: &str = r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom"><entry><id>http://arxiv.org/abs/1706.03762v1</id><published>2017-06-12T17:00:00Z</published><title>Attention Is All You Need</title><summary>A summary.</summary><author><name>Ada Lovelace</name></author><arxiv:doi>10.48550/arXiv.1706.03762</arxiv:doi><arxiv:journal_ref>NeurIPS</arxiv:journal_ref><category term="cs.CL"/><category term="cs.LG"/></entry></feed>"#;
    const ARXIV_ERROR: &str = r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom"><entry><title>Error</title></entry></feed>"#;

    #[test]
    fn parses_crossref_metadata() {
        let reference = parse_crossref(CROSSREF).expect("valid synthetic Crossref JSON");
        assert_eq!(reference.title.as_deref(), Some("A useful paper"));
        assert_eq!(reference.authors, ["Ada Lovelace", "Grace Hopper"]);
        assert_eq!(reference.year, Some(2020));
        assert_eq!(reference.doi.as_deref(), Some("10.1145/3290605.3300857"));
        assert_eq!(reference.publisher.as_deref(), Some("Proceedings of Tests"));
        assert_eq!(reference.isbn.as_deref(), Some("978-1-2345-6789-0"));
        assert!(reference.abstract_text.as_deref().is_some_and(|text| text.contains("clear") && !text.contains("<jats:")));
        assert_eq!(reference.keywords, ["Computing", "History"]);
    }

    #[test]
    fn parses_arxiv_atom_metadata() {
        let reference = parse_arxiv_atom(ATOM).expect("valid synthetic Atom feed").expect("one paper");
        assert_eq!(reference.title.as_deref(), Some("Attention Is All You Need"));
        assert_eq!(reference.authors, ["Ada Lovelace"]);
        assert_eq!(reference.year, Some(2017));
        assert_eq!(reference.arxiv.as_deref(), Some("1706.03762"));
        assert_eq!(reference.doi.as_deref(), Some("10.48550/arxiv.1706.03762"));
        assert_eq!(reference.publisher.as_deref(), Some("NeurIPS"));
        assert_eq!(reference.keywords, ["cs.CL", "cs.LG"]);
    }

    #[test]
    fn rejects_missing_crossref_message_and_arxiv_error() {
        assert!(parse_crossref("{}").is_err());
        assert_eq!(parse_arxiv_atom(ARXIV_ERROR), Ok(None));
        assert_eq!(parse_arxiv_atom("<feed/>"), Ok(None));
    }

    #[test]
    fn generates_encoded_urls() {
        assert_eq!(crossref_url("10.1234/a b"), "https://api.crossref.org/works/10.1234/a%20b");
        assert_eq!(arxiv_url("2101.01234"), "https://export.arxiv.org/api/query?id_list=2101.01234");
    }

    #[test]
    fn hostile_answers_fail_without_panicking() {
        assert!(parse_crossref(&"x".repeat(2 * 1024 * 1024)).is_err());
        assert!(parse_arxiv_atom(&"x".repeat(2 * 1024 * 1024)).is_err());
        assert!(parse_arxiv_atom("\u{0}\u{1}").is_err());
    }

    #[test]
    #[ignore = "needs network access"]
    fn live_lookup() {
        assert!(lookup(Some("10.1038/nature14539"), Some("1706.03762")).is_ok());
    }
}
