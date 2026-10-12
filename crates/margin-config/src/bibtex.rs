//! BibTeX and BibLaTeX files (as Zotero, Better BibTeX, JabRef and Mendeley export them) read
//! into [`Reference`]s for the library import. The file is untrusted: the parser never panics,
//! skips what it can't read and caps what it keeps.

use crate::reference::{MAX_ABSTRACT_CHARS, MAX_FIELD_CHARS, MAX_KEYWORD_CHARS, Reference, clean};
use std::collections::HashMap;
use std::path::PathBuf;

/// Largest file read.
pub const MAX_BIBTEX_BYTES: usize = 32 << 20;
/// Most entries read.
pub const MAX_ENTRIES: usize = 100_000;
const MAX_VALUE_BYTES: usize = 64 * 1024;
const MAX_FIELDS: usize = 256;
const MAX_BRACE_DEPTH: usize = 64;
/// Most authors kept per entry.
const MAX_AUTHORS: usize = 200;

/// One `@type{key, field = value, …}` entry, values with `@string` macros and `#` joins done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BibEntry {
    /// Lower-case entry type (`article`, `inproceedings`, …).
    pub kind: String,
    pub key: String,
    /// Lower-case field names and their raw (LaTeX) values.
    pub fields: Vec<(String, String)>,
}

fn slice(text: &str, start: usize, end: usize) -> &str {
    text.get(start..end).unwrap_or("")
}

fn skip_space(bytes: &[u8], cursor: &mut usize) {
    while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
        *cursor += 1;
    }
}

fn read_atom(text: &str, cursor: &mut usize) -> String {
    let bytes = text.as_bytes();
    let start = *cursor;
    while bytes.get(*cursor).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-' || *b == b':') {
        *cursor += 1;
    }
    slice(text, start, *cursor).to_ascii_lowercase()
}

fn value(text: &str, cursor: &mut usize, close: u8, macros: &HashMap<String, String>) -> Option<String> {
    let bytes = text.as_bytes();
    let mut output = String::new();
    loop {
        skip_space(bytes, cursor);
        let part = match bytes.get(*cursor).copied()? {
            b'{' => {
                *cursor += 1;
                let start = *cursor;
                let mut depth = 1usize;
                let mut parsed = None;
                while let Some(byte) = bytes.get(*cursor).copied() {
                    if byte == b'\\' {
                        *cursor = cursor.saturating_add(2);
                        continue;
                    }
                    if byte == b'{' {
                        depth += 1;
                        if depth > MAX_BRACE_DEPTH {
                            return None;
                        }
                    }
                    if byte == b'}' {
                        depth -= 1;
                        if depth == 0 {
                            parsed = Some(slice(text, start, *cursor).to_owned());
                            *cursor += 1;
                            break;
                        }
                    }
                    *cursor += 1;
                }
                let mut parsed = parsed?;
                let suffix_start = *cursor;
                while bytes.get(*cursor).is_some_and(|byte| *byte != b',' && *byte != close && *byte != b'#') {
                    *cursor += 1;
                }
                parsed.push_str(slice(text, suffix_start, *cursor).trim_end());
                parsed
            }
            b'"' => {
                *cursor += 1;
                let start = *cursor;
                let mut parsed = None;
                while let Some(byte) = bytes.get(*cursor).copied() {
                    if byte == b'\\' {
                        *cursor = cursor.saturating_add(2);
                        continue;
                    }
                    if byte == b'"' {
                        parsed = Some(slice(text, start, *cursor).to_owned());
                        *cursor += 1;
                        break;
                    }
                    *cursor += 1;
                }
                parsed?
            }
            _ => {
                let token = read_atom(text, cursor);
                if token.is_empty() {
                    return None;
                }
                macros.get(&token).cloned().unwrap_or(token)
            }
        };
        if output.len().saturating_add(part.len()) <= MAX_VALUE_BYTES {
            output.push_str(&part);
        } else {
            let remaining = MAX_VALUE_BYTES.saturating_sub(output.len());
            let mut boundary = remaining.min(part.len());
            while !part.is_char_boundary(boundary) {
                boundary = boundary.saturating_sub(1);
            }
            output.push_str(slice(&part, 0, boundary));
        }
        skip_space(bytes, cursor);
        if bytes.get(*cursor) == Some(&b'#') {
            *cursor += 1;
            continue;
        }
        return Some(output);
    }
}

/// Every entry of a BibTeX file. Fails only for a file over [`MAX_BIBTEX_BYTES`]; a malformed
/// entry is skipped.
pub fn parse(text: &str) -> Result<Vec<BibEntry>, String> {
    if text.len() > MAX_BIBTEX_BYTES {
        return Err("BibTeX input exceeds 32 MiB".to_owned());
    }
    let mut macros = HashMap::new();
    for (name, month) in ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"].iter().zip([
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ]) {
        macros.insert((*name).to_owned(), month.to_owned());
    }
    let bytes = text.as_bytes();
    let mut cursor = 0usize;
    let mut entries = Vec::new();
    while cursor < bytes.len() && entries.len() < MAX_ENTRIES {
        if bytes.get(cursor) != Some(&b'@') {
            cursor += 1;
            continue;
        }
        cursor += 1;
        skip_space(bytes, &mut cursor);
        let kind = read_atom(text, &mut cursor);

        skip_space(bytes, &mut cursor);
        let Some(open) = bytes.get(cursor).copied() else { break };
        if open != b'{' && open != b'(' {
            continue;
        }
        cursor += 1;
        let close = if open == b'{' { b'}' } else { b')' };
        if kind == "comment" || kind == "preamble" {
            let mut depth = 1usize;
            while let Some(byte) = bytes.get(cursor).copied() {
                cursor += 1;
                if byte == open {
                    depth += 1;
                }
                if byte == close {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            continue;
        }
        if kind == "string" {
            skip_space(bytes, &mut cursor);
            let name = read_atom(text, &mut cursor);
            skip_space(bytes, &mut cursor);
            if bytes.get(cursor) == Some(&b'=') {
                cursor += 1;
                if let Some(expanded) = value(text, &mut cursor, close, &macros) {
                    macros.insert(name, expanded);
                }
            }
            continue;
        }
        let key_start = cursor;
        while bytes.get(cursor).is_some_and(|byte| *byte != b',' && *byte != close) {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b',') {
            continue;
        }
        let key = slice(text, key_start, cursor).trim().to_owned();
        cursor += 1;
        let mut fields = Vec::new();
        let mut malformed = false;
        while cursor < bytes.len() {
            skip_space(bytes, &mut cursor);
            while bytes.get(cursor) == Some(&b',') {
                cursor += 1;
                skip_space(bytes, &mut cursor);
            }
            if bytes.get(cursor) == Some(&close) {
                cursor += 1;
                break;
            }
            let name = read_atom(text, &mut cursor);
            skip_space(bytes, &mut cursor);
            if name.is_empty() || bytes.get(cursor) != Some(&b'=') {
                malformed = true;

                break;
            }
            cursor += 1;
            if let Some(field_value) = value(text, &mut cursor, close, &macros) {
                if fields.len() < MAX_FIELDS {
                    fields.push((name, field_value));
                }
            } else {
                malformed = true;
                break;
            }
        }
        if malformed {
            while cursor < bytes.len() && bytes.get(cursor) != Some(&b'@') {
                cursor += 1;
            }
        } else {
            entries.push(BibEntry { kind, key, fields });
        }
    }
    Ok(entries)
}

fn accent(mark: char, letter: char) -> Option<char> {
    let result = match (mark, letter.to_ascii_lowercase()) {
        ('"', 'a') => 'ä',
        ('"', 'e') => 'ë',
        ('"', 'i') => 'ï',
        ('"', 'o') => 'ö',
        ('"', 'u') => 'ü',
        ('"', 'y') => 'ÿ',
        ('\'', 'a') => 'á',
        ('\'', 'e') => 'é',
        ('\'', 'i') => 'í',
        ('\'', 'o') => 'ó',
        ('\'', 'u') => 'ú',
        ('`', 'a') => 'à',
        ('`', 'e') => 'è',
        ('`', 'i') => 'ì',
        ('`', 'o') => 'ò',
        ('`', 'u') => 'ù',
        ('^', 'a') => 'â',
        ('^', 'e') => 'ê',
        ('^', 'i') => 'î',
        ('^', 'o') => 'ô',
        ('^', 'u') => 'û',
        ('~', 'a') => 'ã',
        ('~', 'n') => 'ñ',
        ('~', 'o') => 'õ',
        ('c', 'c') => 'ç',
        ('v', 's') => 'š',
        ('v', 'z') => 'ž',
        ('=', 'a') => 'ā',
        ('=', 'e') => 'ē',
        ('=', 'i') => 'ī',
        ('=', 'o') => 'ō',
        ('=', 'u') => 'ū',
        ('.', 'z') => 'ż',
        ('u', 'a') => 'ă',
        ('H', 'o') => 'ő',
        _ => return None,
    };
    Some(if letter.is_uppercase() { result.to_uppercase().next().unwrap_or(result) } else { result })
}

/// A LaTeX field value as plain text: braces dropped, escapes and accents turned into characters.
pub fn latex_to_text(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::new();
    let mut pos = 0usize;
    while let Some(ch) = chars.get(pos).copied() {
        if ch == '{' || ch == '}' {
            pos += 1;
            continue;
        }
        if ch == '~' {
            out.push(' ');
            pos += 1;
            continue;
        }
        if ch == '-' && chars.get(pos.saturating_add(1)) == Some(&'-') {
            if chars.get(pos.saturating_add(2)) == Some(&'-') {
                out.push('—');
                pos += 3;
            } else {
                out.push('–');
                pos += 2;
            }
            continue;
        }
        if ch != '\\' {
            out.push(ch);
            pos += 1;
            continue;
        }
        pos += 1;
        let Some(command) = chars.get(pos).copied() else { break };
        if "&%_$#".contains(command) {
            out.push(command);
            pos += 1;
            continue;
        }
        // `\c`, `\v`, `\u` and `\H` are accents only before a non-letter (`\c{c}`, `\v s`), not
        // the start of a longer command (`\cite`, `\url`).
        let letter_accent = "cvuH".contains(command) && chars.get(pos.saturating_add(1)).is_some_and(|next| next.is_ascii_alphabetic());
        if "\"'`^~cv=.uH".contains(command) && !letter_accent {
            pos += 1;
            while chars.get(pos).is_some_and(|c| c.is_whitespace() || *c == '{') {
                pos += 1;
            }
            if let Some(letter) = chars.get(pos).copied() {
                out.push(accent(command, letter).unwrap_or(letter));
                pos += 1;
            }
            continue;
        }
        let start = pos;
        while chars.get(pos).is_some_and(|c| c.is_ascii_alphabetic()) {
            pos += 1;
        }
        let name: String = chars.iter().skip(start).take(pos.saturating_sub(start)).collect();
        let replacement = match name.as_str() {
            "ss" => Some('ß'),
            "o" => Some('ø'),
            "O" => Some('Ø'),
            "aa" => Some('å'),
            "AA" => Some('Å'),
            "ae" => Some('æ'),
            "AE" => Some('Æ'),
            "oe" => Some('œ'),
            "OE" => Some('Œ'),
            "l" => Some('ł'),
            "L" => Some('Ł'),
            _ => None,
        };
        if let Some(replacement) = replacement {
            out.push(replacement);
        }
        if pos == start {
            pos += 1;
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn field<'a>(entry: &'a BibEntry, name: &str) -> Option<&'a str> {
    entry.fields.iter().find(|(field_name, _)| field_name == name).map(|(_, value)| value.as_str())
}
fn optional(value: Option<&str>, cap: usize) -> Option<String> {
    value.map(|text| clean(text, cap)).filter(|text| !text.is_empty())
}
fn normal_doi(value: &str) -> Option<String> {
    crate::ids::normalize_doi(value)
}
fn arxiv_id(value: &str) -> Option<String> {
    crate::ids::normalize_arxiv(value)
}
fn authors(value: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    let chars: Vec<char> = value.chars().collect();
    let mut pos = 0usize;
    while pos < chars.len() {
        if chars.get(pos) == Some(&'{') {
            depth += 1;
        }
        if chars.get(pos) == Some(&'}') {
            depth -= 1;
        }
        if depth == 0 && chars.get(pos..pos.saturating_add(5)).is_some_and(|part| part.iter().collect::<String>().eq_ignore_ascii_case(" and ")) {
            let name: String = chars.iter().skip(start).take(pos.saturating_sub(start)).collect();
            add_author(&name, &mut names);
            pos += 5;
            start = pos;
            continue;
        }
        pos += 1;
    }
    let name: String = chars.iter().skip(start).collect();
    add_author(&name, &mut names);
    names
}
fn add_author(name: &str, output: &mut Vec<String>) {
    if output.len() >= MAX_AUTHORS {
        return;
    }
    let name = name.trim();
    if name.eq_ignore_ascii_case("others") || name.is_empty() {
        return;
    }
    if name.starts_with('{') && name.ends_with('}') {
        output.push(clean(&latex_to_text(name), MAX_FIELD_CHARS));
        return;
    }
    let pieces: Vec<&str> = name.split(',').map(str::trim).collect();
    let display = if pieces.len() >= 3 {
        format!("{} {} {}", pieces.get(2).copied().unwrap_or(""), pieces.first().copied().unwrap_or(""), pieces.get(1).copied().unwrap_or(""))
    } else if pieces.len() == 2 {
        format!("{} {}", pieces.get(1).copied().unwrap_or(""), pieces.first().copied().unwrap_or(""))
    } else {
        name.to_owned()
    };
    let display = clean(&latex_to_text(&display), MAX_FIELD_CHARS);
    if !display.is_empty() {
        output.push(display);
    }
}

/// What a BibTeX entry says about its document.
pub fn reference(entry: &BibEntry) -> Reference {
    let date = field(entry, "date");
    let year = field(entry, "year")
        .and_then(|y| {
            y.as_bytes().windows(4).find_map(|digits| {
                let s = std::str::from_utf8(digits).ok()?;
                let year = s.parse::<i32>().ok()?;
                (1000..=2999).contains(&year).then_some(year)
            })
        })
        .or_else(|| date.and_then(|d| d.get(..4)?.parse::<i32>().ok()));
    let url = field(entry, "url");
    let doi = field(entry, "doi")
        .and_then(normal_doi)
        .or_else(|| url.and_then(|u| u.to_ascii_lowercase().find("doi.org/").and_then(|idx| normal_doi(u.get(idx..).unwrap_or("")))));
    let arxiv = field(entry, "eprint")
        .filter(|_| {
            field(entry, "archiveprefix").is_some_and(|v| v.eq_ignore_ascii_case("arxiv"))
                || field(entry, "eprinttype").is_some_and(|v| v.eq_ignore_ascii_case("arxiv"))
        })
        .and_then(arxiv_id)
        .or_else(|| [url, field(entry, "arxivid"), field(entry, "arxiv")].into_iter().flatten().find_map(arxiv_id));
    let publisher = [field(entry, "journal"), field(entry, "journaltitle"), field(entry, "booktitle"), field(entry, "publisher")]
        .into_iter()
        .flatten()
        .find_map(|v| optional(Some(&latex_to_text(v)), MAX_FIELD_CHARS));
    let isbn = field(entry, "isbn").and_then(|v| v.split([',', ';']).next()).and_then(|v| optional(Some(v), MAX_FIELD_CHARS));
    let mut keywords = Vec::new();
    for value in [field(entry, "keywords"), field(entry, "mendeley-tags"), field(entry, "tags")].into_iter().flatten() {
        for keyword in value.split([',', ';']) {
            let keyword = clean(&latex_to_text(keyword), MAX_KEYWORD_CHARS);
            if !keyword.is_empty() && !keywords.iter().any(|existing: &String| existing.eq_ignore_ascii_case(&keyword)) {
                keywords.push(keyword);
            }
        }
    }
    let files = field(entry, "file").map(parse_files).unwrap_or_default();
    Reference {
        title: optional(field(entry, "title").map(latex_to_text).as_deref(), MAX_FIELD_CHARS),
        authors: field(entry, "author").map(authors).unwrap_or_default(),
        year,
        doi,
        arxiv,
        publisher,
        isbn,
        abstract_text: optional(field(entry, "abstract").map(latex_to_text).as_deref(), MAX_ABSTRACT_CHARS),
        keywords,
        files,
    }
}
/// Whether a file field's last part is its type (JabRef's `PDF`, Zotero's `application/pdf`).
fn is_file_type(part: &str) -> bool {
    let part = part.trim();
    part.eq_ignore_ascii_case("pdf") || (part.contains('/') && !part.to_ascii_lowercase().ends_with(".pdf"))
}

/// Whether a file field's first part is a Windows drive letter (`C` of `C:\papers\x.pdf`).
fn is_drive(part: &str) -> bool {
    let mut chars = part.chars();
    chars.next().is_some_and(|letter| letter.is_ascii_alphabetic()) && chars.next().is_none()
}

/// The PDFs a `file` field names. Exports write `description:path:type` (JabRef escapes `:` and
/// `\` in the path as `\:` and `\\`), a bare path, or several of them separated by `;`.
fn parse_files(value: &str) -> Vec<PathBuf> {
    const ESCAPED_COLON: char = '\u{1f}';
    let mut result = Vec::new();
    for part in value.split(';') {
        let normalized = part.replace("\\:", &ESCAPED_COLON.to_string()).replace("\\\\", "\\");
        let mut fields: Vec<&str> = normalized.split(':').collect();
        if fields.len() > 1 && fields.last().is_some_and(|last| is_file_type(last)) {
            fields.pop();
        }
        let path_fields = match fields.first() {
            Some(first) if fields.len() > 1 && !is_drive(first) => fields.get(1..).unwrap_or_default(),
            _ => fields.as_slice(),
        };
        let path = path_fields.join(":").replace(ESCAPED_COLON, ":");
        let path = path.trim();
        if path.to_ascii_lowercase().ends_with(".pdf") {
            result.push(PathBuf::from(path));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_export_and_metadata() {
        let input = r#"@string{venue = "Proc" # "eedings"}
@article{one,title={{BERT}}: Pre-training,author={García, Jr, Ana and {Research Group}},year=2020,doi={10.1234/ABC},keywords={nlp, language}}
@article{two,title="Arxiv",eprint={2101.01234v2},archivePrefix={arXiv},month=jan}
@inproceedings{three,booktitle=venue,file={paper:C\:\\papers\\x.pdf:PDF}}"#;
        let entries = parse(input).unwrap();
        assert_eq!(entries.len(), 3, "{entries:?}");
        assert_eq!(entries.first().map(|e| reference(e).year), Some(Some(2020)));
        let article = reference(entries.first().unwrap());
        assert_eq!(article.title.as_deref(), Some("BERT: Pre-training"));
        assert_eq!(article.authors, ["Ana García Jr", "Research Group"]);
        assert_eq!(article.doi.as_deref(), Some("10.1234/abc"));
        assert_eq!(article.keywords, ["nlp", "language"]);
        assert_eq!(entries.get(1).and_then(|e| field(e, "month")), Some("January"));
        assert_eq!(entries.get(1).map(reference).and_then(|record| record.arxiv), Some("2101.01234".to_owned()));
        assert_eq!(entries.get(2).and_then(|e| field(e, "booktitle")), Some("Proceedings"));
        let proceedings = reference(entries.get(2).unwrap());
        assert_eq!(proceedings.files.first().map(|path| path.to_string_lossy().into_owned()), Some("C:\\papers\\x.pdf".to_owned()));
    }

    #[test]
    fn file_fields_of_every_exporter_name_their_pdfs() {
        let names = |value: &str| parse_files(value).into_iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(names("Full Text PDF:/home/ada/Zotero/storage/AB12/paper.pdf:application/pdf"), ["/home/ada/Zotero/storage/AB12/paper.pdf"]);
        assert_eq!(names("/papers/plain.pdf"), ["/papers/plain.pdf"]);
        assert_eq!(names(":papers/rel.pdf:PDF;Snapshot:/x/page.html:text/html"), ["papers/rel.pdf"], "only PDFs");
        assert_eq!(names("C\\:\\\\a\\\\b.pdf"), ["C:\\a\\b.pdf"]);
        assert!(names("").is_empty() && names(":::;;;").is_empty());
    }

    #[test]
    fn latex_becomes_plain_text() {
        assert_eq!(latex_to_text(r#"Schr{\"o}dinger \c{c}a \v{s}ta --- \url{x} \cite{y}"#), "Schrödinger ça šta — x y");
        assert_eq!(latex_to_text(r#"\"{U}ber \ss{} 50\% \&"#), "Über ß 50% &");
        assert_eq!(latex_to_text("\\"), "", "a lone backslash at the end");
    }

    #[test]
    fn macros_and_bad_entries_are_bounded() {
        let many = (0..MAX_FIELDS + 10).map(|i| format!("f{i}={{v}}")).collect::<Vec<_>>().join(",");
        let entries = parse(&format!("@misc{{k,{many}}}")).unwrap();
        assert_eq!(entries.first().map(|entry| entry.fields.len()), Some(MAX_FIELDS));
        let long = format!("@misc{{k,title={{{}}}}}", "é".repeat(MAX_VALUE_BYTES));
        let entries = parse(&long).unwrap();
        assert!(entries.first().and_then(|entry| field(entry, "title")).is_some_and(|title| title.len() <= MAX_VALUE_BYTES));
        assert!(parse(&" ".repeat(MAX_BIBTEX_BYTES + 1)).is_err(), "a file over the cap is refused");
        let authors = (0..MAX_AUTHORS + 50).map(|i| format!("Name{i}, First")).collect::<Vec<_>>().join(" and ");
        let entries = parse(&format!("@misc{{k,author={{{authors}}}}}")).unwrap();
        assert_eq!(entries.first().map(|entry| reference(entry).authors.len()), Some(MAX_AUTHORS));
    }
    #[test]
    fn hostile_inputs_do_not_panic() {
        for text in ["@article{x,title={", &format!("@a{{x,f={{{}}}}}", "{".repeat(10000)), &"@".repeat(1 << 20), &"x".repeat(200 << 10)] {
            let _ = parse(text);
        }
        let mut seed = 7u32;
        let mut bytes = Vec::new();
        for _ in 0..4096 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            bytes.push((seed >> 24) as u8);
        }
        let garbage = String::from_utf8_lossy(&bytes);
        let _ = parse(&garbage);
    }
}
