//! DOIs and arXiv ids found in a document without asking anyone: in its Info dictionary, its
//! XMP packet and its first page's text. Every scan is bounded: a hostile file can make them
//! give up, never run long.

use roxmltree::Document;

/// Text read for an id, from its start.
const MAX_TEXT_SCAN: usize = 256 * 1024;
/// File bytes searched for an XMP packet, and the longest packet read.
const MAX_XMP_SCAN: usize = 64 * 1024 * 1024;
const MAX_XMP_PACKET: usize = 1024 * 1024;
/// Punctuation that ends a sentence after a DOI rather than belonging to it.
const DOI_SUFFIX_CHARS: &str = ".,;:)]}>'\"";
/// Prefixes a DOI is written with.
const DOI_PREFIXES: [&str; 8] =
    ["https://doi.org/", "http://doi.org/", "https://dx.doi.org/", "http://dx.doi.org/", "doi.org/", "dx.doi.org/", "doi:", "doi "];
/// The longest DOI kept, and the longest text read as one (with a resolver prefix).
const MAX_DOI_CHARS: usize = 256;
const MAX_DOI_INPUT_CHARS: usize = 320;
/// How many digits a DOI's registrant code (after `10.`) has.
const DOI_REGISTRANT_DIGITS: std::ops::RangeInclusive<usize> = 4..=9;
/// The longest text read as an arXiv id, and the longest id.
const MAX_ARXIV_INPUT_CHARS: usize = 512;
const MAX_ARXIV_CHARS: usize = 256;
/// A new-style arXiv id's date (`YYMM`) and number lengths; an old-style id's number length.
const ARXIV_DATE_LEN: usize = 4;
const ARXIV_NUMBER_LENS: [usize; 2] = [4, 5];
const ARXIV_OLD_NUMBER_LEN: usize = 7;
/// How far before a DOI `doi` must appear for the DOI to be the document's own.
const DOI_CONTEXT_BYTES: usize = 16;
/// Blank characters allowed between `arXiv:` and the id.
const MAX_ARXIV_GAP: usize = 8;

/// A DOI as Margin keeps it: lower-case, without `doi:` or a resolver URL, trailing punctuation
/// dropped. `None` when `value` is not one.
pub fn normalize_doi(value: &str) -> Option<String> {
    if value.char_indices().nth(MAX_DOI_INPUT_CHARS).is_some() {
        return None;
    }
    let mut candidate = value.trim();
    for prefix in DOI_PREFIXES {
        if candidate.get(..prefix.len()).is_some_and(|part| part.eq_ignore_ascii_case(prefix)) {
            candidate = candidate.get(prefix.len()..)?;
            break;
        }
    }
    candidate = candidate.trim();
    candidate = candidate.trim_end_matches(|character| DOI_SUFFIX_CHARS.contains(character));
    if candidate.chars().count() > MAX_DOI_CHARS
        || candidate.chars().any(char::is_whitespace)
        || candidate.chars().any(|c| matches!(c, '"' | '<' | '>'))
    {
        return None;
    }
    let (prefix, suffix) = candidate.split_once('/')?;
    let digits = prefix.strip_prefix("10.")?;
    if !DOI_REGISTRANT_DIGITS.contains(&digits.len()) || !digits.bytes().all(|byte| byte.is_ascii_digit()) || suffix.is_empty() {
        return None;
    }
    Some(candidate.to_ascii_lowercase())
}

/// An arXiv id as Margin keeps it, from an id, an `arXiv:` stamp or an arxiv.org link: new style
/// `YYMM.NNNNN` or old style `archive/NNNNNNN`, without its version. `None` when not one.
pub fn normalize_arxiv(value: &str) -> Option<String> {
    if value.char_indices().nth(MAX_ARXIV_INPUT_CHARS).is_some() {
        return None;
    }
    let mut candidate = value.trim();
    for prefix in [
        "https://arxiv.org/abs/",
        "http://arxiv.org/abs/",
        "https://arxiv.org/pdf/",
        "http://arxiv.org/pdf/",
        "arxiv.org/abs/",
        "arxiv.org/pdf/",
        "arxiv:",
    ] {
        if candidate.get(..prefix.len()).is_some_and(|part| part.eq_ignore_ascii_case(prefix)) {
            candidate = candidate.get(prefix.len()..)?;
            break;
        }
    }
    candidate = candidate.split_whitespace().next()?;
    candidate = candidate.trim_end_matches(['.', ',', ';', ':', ')', ']', '}', '>', '\'', '"']);
    if candidate.chars().count() > MAX_ARXIV_CHARS {
        return None;
    }
    if let Some(without_pdf) = candidate.strip_suffix(".pdf").or_else(|| candidate.strip_suffix(".PDF")) {
        candidate = without_pdf;
    }
    let identifier = match candidate.rfind(['v', 'V']) {
        Some(position) => {
            let version = candidate.get(position.checked_add(1)?..)?;
            if !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()) { candidate.get(..position)? } else { candidate }
        }
        None => candidate,
    };
    if let Some((date, number)) = identifier.split_once('.')
        && date.len() == ARXIV_DATE_LEN
        && date.bytes().all(|byte| byte.is_ascii_digit())
    {
        let month = date.get(2..4)?.parse::<u8>().ok()?;
        if !(1..=12).contains(&month) || !ARXIV_NUMBER_LENS.contains(&number.len()) || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        return Some(identifier.to_ascii_lowercase());
    }
    let (category, number) = identifier.split_once('/')?;
    if category.is_empty()
        || number.len() != ARXIV_OLD_NUMBER_LEN
        || !number.bytes().all(|byte| byte.is_ascii_digit())
        || !category.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
    {
        return None;
    }
    Some(identifier.to_ascii_lowercase())
}

fn has_valid_doi_prefix(value: &str) -> bool {
    let Some(suffix) = value.as_bytes().get(3..) else {
        return false;
    };
    let digits = suffix.iter().take_while(|byte| byte.is_ascii_digit()).count();
    DOI_REGISTRANT_DIGITS.contains(&digits)
        && suffix.get(digits) == Some(&b'/')
        && suffix.get(digits.saturating_add(1)..).is_some_and(|rest| !rest.is_empty())
}

/// The DOI in free text: the first one named as a DOI (`doi:`, `doi.org/` just before it), else
/// the first one. Reads at most the first 256 KiB.
pub fn find_doi(text: &str) -> Option<String> {
    let bounded = bounded_prefix(text, MAX_TEXT_SCAN);
    let mut first = None;
    let mut preferred = None;
    let mut start = 0;
    while start < bounded.len() {
        let Some(relative) = bounded.get(start..).and_then(|rest| rest.find("10.")) else { break };
        let candidate_start = start.checked_add(relative)?;
        let tail = bounded.get(candidate_start..)?;
        if !has_valid_doi_prefix(tail) {
            start = candidate_start.saturating_add(3);
            continue;
        }
        let end = tail
            .char_indices()
            .take(MAX_DOI_CHARS.saturating_add(1))
            .find(|(_, character)| character.is_whitespace())
            .map(|(position, _)| position)
            .or_else(|| tail.char_indices().nth(MAX_DOI_CHARS).map(|(position, _)| position))
            .unwrap_or(tail.len());
        let candidate = tail.get(..end)?;
        if let Some(doi) = normalize_doi(candidate) {
            if first.is_none() {
                first = Some(doi.clone());
            }
            let mut context_start = candidate_start.saturating_sub(DOI_CONTEXT_BYTES);
            while !bounded.is_char_boundary(context_start) {
                context_start = context_start.saturating_sub(1);
            }
            let context = bounded.get(context_start..candidate_start).unwrap_or_default().to_ascii_lowercase();
            if context.contains("doi") {
                preferred = Some(doi);
                break;
            }
        }
        start = candidate_start.saturating_add(3);
    }
    preferred.or(first)
}

/// The arXiv id in free text, only where it is written as one (`arXiv:`, an arxiv.org link):
/// bare numbers are too often something else. Reads at most the first 256 KiB.
pub fn find_arxiv(text: &str) -> Option<String> {
    let bounded = bounded_prefix(text, MAX_TEXT_SCAN);
    let lower = bounded.to_ascii_lowercase();
    let mut search = 0;
    while search < lower.len() {
        let next = ["arxiv:", "arxiv.org/abs/", "arxiv.org/pdf/"]
            .iter()
            .filter_map(|marker| lower.get(search..)?.find(marker).map(|relative| (search.saturating_add(relative), *marker)))
            .min_by_key(|(position, _)| *position);
        let (marker_start, marker) = next?;
        let id_start = marker_start.checked_add(marker.len())?;
        let after_marker = bounded.get(id_start..)?;
        let value = after_marker.trim_start();
        let skipped = after_marker.len().saturating_sub(value.len());
        if after_marker.get(..skipped)?.chars().count() > MAX_ARXIV_GAP {
            search = id_start;
            continue;
        }
        let end = value.find(char::is_whitespace).unwrap_or(value.len());
        if let Some(id) = normalize_arxiv(value.get(..end)?) {
            return Some(id);
        }
        search = id_start;
    }
    None
}

/// The first XMP packet stored uncompressed in a PDF's bytes, if it is valid UTF-8.
pub fn xmp_packet(bytes: &[u8]) -> Option<&str> {
    let bounded = bytes.get(..bytes.len().min(MAX_XMP_SCAN))?;
    let open = b"<x:xmpmeta";
    let close = b"</x:xmpmeta>";
    let start = find_bytes(bounded, open)?;
    let content_start = start;
    let after_open = start.checked_add(open.len())?;
    let relative_close = find_bytes(bounded.get(after_open..)?, close)?;
    let end = after_open.checked_add(relative_close)?.checked_add(close.len())?;
    if end.checked_sub(content_start)? > MAX_XMP_PACKET {
        return None;
    }
    std::str::from_utf8(bounded.get(content_start..end)?).ok()
}

/// The DOI and arXiv id an XMP packet names (`prism:doi`, `dc:identifier`, …).
pub fn xmp_ids(packet: &str) -> (Option<String>, Option<String>) {
    let Ok(document) = Document::parse(packet) else {
        return (find_doi(packet), None);
    };
    let mut doi = None;
    let mut arxiv = None;
    for node in document.descendants().filter(|node| node.is_element()) {
        let element = node;
        let name = element.tag_name().name();
        for attribute in element.attributes() {
            if attribute.name().eq_ignore_ascii_case("doi") && doi.is_none() {
                doi = normalize_doi(attribute.value());
            }
        }
        if let Some(value) = element.text() {
            let is_identifier = name.eq_ignore_ascii_case("doi")
                || name.eq_ignore_ascii_case("identifier")
                || element.ancestors().any(|ancestor| ancestor.is_element() && ancestor.tag_name().name().eq_ignore_ascii_case("identifier"));
            if doi.is_none() && is_identifier {
                doi = normalize_doi(value);
            }
            if arxiv.is_none()
                && (name.to_ascii_lowercase().contains("arxiv") || value.trim().get(..6).is_some_and(|prefix| prefix.eq_ignore_ascii_case("arxiv:")))
            {
                arxiv = normalize_arxiv(value);
            }
        }
    }
    (doi, arxiv)
}

fn bounded_prefix(text: &str, maximum: usize) -> &str {
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or("")
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let first = needle.first().copied()?;
    let mut offset: usize = 0;
    while offset.checked_add(needle.len())? <= haystack.len() {
        let relative = haystack.get(offset..)?.iter().position(|byte| *byte == first)?;
        offset = offset.checked_add(relative)?;
        if haystack.get(offset..)?.starts_with(needle) {
            return Some(offset);
        }
        offset = offset.checked_add(1)?;
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_doi_forms() {
        assert_eq!(normalize_doi("https://doi.org/10.1145/3290605.3300857."), Some("10.1145/3290605.3300857".to_owned()));
        assert_eq!(find_doi("/Subject (Journal, 2020, doi:10.1016/j.x.2020.01.001)"), Some("10.1016/j.x.2020.01.001".to_owned()));
        assert_eq!(find_doi("Header https://doi.org/10.1145/3290605.3300857."), Some("10.1145/3290605.3300857".to_owned()));
        assert_eq!(normalize_doi("10.5 /x"), None);
    }

    #[test]
    fn normalizes_arxiv_forms() {
        assert_eq!(normalize_arxiv("arXiv:2101.01234v3"), Some("2101.01234".to_owned()));
        assert_eq!(normalize_arxiv("https://arxiv.org/abs/2101.01234v2"), Some("2101.01234".to_owned()));
        assert_eq!(normalize_arxiv("arxiv.org/pdf/2101.01234.pdf"), Some("2101.01234".to_owned()));
        assert_eq!(normalize_arxiv("hep-th/9901001v1"), Some("hep-th/9901001".to_owned()));
        assert_eq!(normalize_arxiv("math.GT/0309136"), Some("math.gt/0309136".to_owned()));
        assert_eq!(normalize_arxiv("arXiv:2101.01234v2 [cs.CL] 4 Jan 2021"), Some("2101.01234".to_owned()));
        assert_eq!(normalize_arxiv("2101.01234"), Some("2101.01234".to_owned()));
        assert_eq!(normalize_arxiv("arXiv:soon"), None);
        assert_eq!(normalize_arxiv("1234.5678"), None);
        assert_eq!(normalize_arxiv("arXiv:2113.01234"), None);
    }

    #[test]
    fn finds_only_prefixed_arxiv_ids() {
        assert_eq!(find_arxiv("2101.01234"), None);
        assert_eq!(find_arxiv("First page arXiv:2101.01234v2 [cs.CL] 4 Jan 2021"), Some("2101.01234".to_owned()));
    }

    #[test]
    fn parses_xmp_identifier_forms() {
        let prism = r#"<x:xmpmeta xmlns:x="x" xmlns:prism="p"><rdf:RDF xmlns:rdf="r"><rdf:Description prism:doi="10.1145/3290605.3300857"/></rdf:RDF></x:xmpmeta>"#;
        assert_eq!(xmp_ids(prism).0, Some("10.1145/3290605.3300857".to_owned()));
        let rdf_bag = r#"<x:xmpmeta xmlns:x="x" xmlns:dc="d" xmlns:rdf="r"><dc:identifier><rdf:Bag><rdf:li>doi:10.1145/3290605.3300857</rdf:li></rdf:Bag></dc:identifier></x:xmpmeta>"#;
        assert_eq!(xmp_ids(rdf_bag).0, Some("10.1145/3290605.3300857".to_owned()));
    }

    #[test]
    fn extracts_bounded_utf8_xmp_packet() {
        let bytes = b"noise<x:xmpmeta>ok</x:xmpmeta>tail";
        assert_eq!(xmp_packet(bytes), Some("<x:xmpmeta>ok</x:xmpmeta>"));
        assert_eq!(xmp_packet(b"<x:xmpmeta>\xff</x:xmpmeta>"), None);
        assert_eq!(xmp_packet(b"garbage"), None);
    }

    #[test]
    fn hostile_text_is_bounded_and_safe() {
        let huge = "10.1".repeat(50 * 1024 * 1024 / 4);
        assert_eq!(find_doi(&huge), None);
        assert_eq!(find_arxiv(&"x".repeat(300 * 1024)), None);
        assert_eq!(xmp_ids("not xml \u{0} DOI:10.1234/okay"), (Some("10.1234/okay".to_owned()), None));
    }
}
