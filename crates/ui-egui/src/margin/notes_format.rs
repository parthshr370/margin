//! Pure note formatting. Only a hash-verified, explicitly bounded Margin block may be replaced.
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const HIGHLIGHTS_HEADING: &str = "## Highlights";
const MAX_TEMPLATE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum TemplateContext {
    Text,
    Path,
    FrontMatter,
}

/// Substitution is one pass: a value never becomes executable template syntax.
pub(super) fn render(template: &str, variables: &BTreeMap<String, String>, context: TemplateContext) -> Result<String, String> {
    if template.len() > MAX_TEMPLATE_BYTES {
        return Err("The note template is too large.".into());
    }
    let mut rendered = String::new();
    let mut remaining = template;
    while let Some((literal, tail)) = remaining.split_once("{{") {
        rendered.push_str(literal);
        let (name, rest) = tail.split_once("}}").ok_or("An opening {{ in the note template needs a closing }}.")?;
        if let Some(value) = variables.get(name.trim()) {
            if value.len() > MAX_TEMPLATE_BYTES {
                return Err(format!("The note variable {name} is too large."));
            }
            let safe = match context {
                TemplateContext::Path => super::slug(value),
                TemplateContext::Text => value.replace("<!--", "&lt;!--").replace("-->", "--&gt;").replace('\0', ""),
                TemplateContext::FrontMatter => {
                    let quoted = serde_json::to_string(value).map_err(|error| error.to_string())?;
                    quoted.strip_prefix('"').and_then(|text| text.strip_suffix('"')).unwrap_or_default().to_string()
                }
            };
            rendered.push_str(&safe);
        }
        if rendered.len() > MAX_TEMPLATE_BYTES {
            return Err("The expanded note template is too large.".into());
        }
        remaining = rest;
    }
    rendered.push_str(remaining);
    if rendered.len() > MAX_TEMPLATE_BYTES {
        return Err("The expanded note template is too large.".into());
    }
    Ok(rendered)
}

fn encoded_key(key: &str) -> String {
    super::percent_encode(key, false)
}
pub(super) fn digest(body: &str) -> String {
    let digest = Sha256::digest(body.as_bytes());
    let mut encoded = String::with_capacity(digest.len().saturating_mul(2));
    // Each nibble is at most 15, so these ASCII additions cannot overflow.
    let digit = |nibble: u8| if nibble < 10 { char::from(b'0' + nibble) } else { char::from(b'a' + nibble - 10) };
    for &byte in digest.iter() {
        encoded.push(digit(byte >> 4));
        encoded.push(digit(byte & 0x0f));
    }
    encoded
}
fn block(owner: &str, key: &str, body: &str) -> String {
    let key = encoded_key(key);
    let body = if body.ends_with('\n') { body.to_string() } else { format!("{body}\n") };
    format!("<!-- margin:{key} sha256:{} owner:{} -->\n{body}<!-- /margin:{key} -->\n", digest(&body), digest(owner))
}

#[derive(Default, Debug)]
pub(super) struct SyncReport {
    pub changed: usize,
    pub preserved: Vec<String>,
}

/// A full synchronization removes absent highlights; a partial write never deletes siblings.
/// Legacy unbounded markers cannot prove ownership of text and are deliberately preserved.
pub(super) fn synchronize(
    existing: Option<&str>,
    header: &str,
    entries: &[(String, String)],
    full: bool,
    append_only: bool,
    owner: &str,
) -> (String, SyncReport) {
    let initial = existing.map(str::to_string).unwrap_or_else(|| format!("{header}{HIGHLIGHTS_HEADING}\n"));
    let owner_hash = digest(owner);
    let wanted: BTreeMap<String, (&str, &str)> = entries.iter().map(|(key, body)| (encoded_key(key), (key.as_str(), body.as_str()))).collect();
    let mut seen = BTreeSet::new();
    let mut report = SyncReport::default();
    let mut output = String::new();
    let mut remaining = initial.as_str();
    while let Some(start) = remaining.find("<!-- margin:") {
        let Some(before) = remaining.get(..start) else { break };
        output.push_str(before);
        let Some(marked) = remaining.get(start..) else { break };
        let Some((opening, after_opening)) = marked.split_once('\n') else {
            output.push_str(marked);
            remaining = "";
            break;
        };
        let parsed = opening.trim_end_matches('\r').strip_prefix("<!-- margin:").and_then(|line| line.strip_suffix(" -->"));
        let Some((key, hash)) = parsed.and_then(|line| line.split_once(" sha256:")) else {
            // Old markers name the entry but have neither a digest nor an end boundary.
            if let Some(legacy) = parsed {
                for (encoded, (original, _)) in &wanted {
                    if super::marker(original) == opening.trim_end_matches('\r') {
                        seen.insert(encoded.clone());
                    }
                }
                report.preserved.push(legacy.to_string());
            }
            output.push_str(opening);
            output.push('\n');
            remaining = after_opening;
            continue;
        };
        let Some((hash, block_owner)) = hash.split_once(" owner:") else {
            seen.insert(key.to_string());
            report.preserved.push(key.to_string());
            output.push_str(opening);
            output.push('\n');
            remaining = after_opening;
            continue;
        };
        if block_owner == owner_hash {
            seen.insert(key.to_string());
        }
        let closing = format!("<!-- /margin:{key} -->");
        let Some(end) = after_opening.find(&closing) else {
            report.preserved.push(key.to_string());
            output.push_str(opening);
            output.push('\n');
            remaining = after_opening;
            continue;
        };
        let Some(body) = after_opening.get(..end) else { break };
        let boundary = end.saturating_add(closing.len());
        let Some(after_block) = after_opening.get(boundary..) else { break };
        let after_block = after_block.strip_prefix("\r\n").or_else(|| after_block.strip_prefix('\n')).unwrap_or(after_block);
        let whole_len = marked.len().saturating_sub(after_block.len());
        let whole = marked.get(..whole_len).unwrap_or_default();
        if block_owner != owner_hash {
            output.push_str(whole);
        } else if hash != digest(body) {
            report.preserved.push(key.to_string());
            output.push_str(whole);
        } else if let Some((original, desired)) = wanted.get(key) {
            let replacement = block(owner, original, desired);
            if append_only || replacement == whole {
                output.push_str(whole);
            } else {
                output.push_str(&replacement);
                report.changed = report.changed.saturating_add(1);
            }
        } else if full && !append_only {
            report.changed = report.changed.saturating_add(1);
        } else {
            output.push_str(whole);
        }
        remaining = after_block;
    }
    output.push_str(remaining);
    let mut fresh = String::new();
    for (original, body) in entries {
        if seen.insert(encoded_key(original)) {
            fresh.push_str(&block(owner, original, body));
            fresh.push('\n');
            report.changed = report.changed.saturating_add(1);
        }
    }
    if !fresh.is_empty() {
        let heading_end = super::find_heading(&output).unwrap_or_else(|| {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(HIGHLIGHTS_HEADING);
            output.push('\n');
            output.len()
        });
        let section_end = super::next_section(&output, heading_end).unwrap_or(output.len());
        if let (Some(before), Some(after)) = (output.get(..section_end), output.get(section_end..)) {
            let mut joined = before.to_string();
            if !joined.ends_with('\n') {
                joined.push('\n');
            }
            if !joined.ends_with("\n\n") {
                joined.push('\n');
            }
            joined.push_str(&fresh);
            joined.push_str(after);
            output = joined;
        }
    }
    (output, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edited_recolored_deleted_and_foreign_text() {
        let entries = vec![("id/one".into(), "> [!PDF|yellow] quote\n> note\n".into()), ("two".into(), "> delete\n".into())];
        let (first, report) =
            synchronize(Some("# Paper\n\nForeign thoughts\n\n## Highlights\n\n## Questions\n- why?\n"), "", &entries, true, false, "paper");
        assert_eq!(report.changed, 2);
        let new = vec![("id/one".into(), "> [!PDF|red] quote\n> edited note\n".into())];
        let (second, report) = synchronize(Some(&first), "", &new, true, false, "paper");
        assert_eq!(report.changed, 2);
        assert!(second.contains("> [!PDF|red] quote\n> edited note\n"));
        assert!(!second.contains("> delete"));
        assert!(second.starts_with("# Paper\n\nForeign thoughts\n"));
        assert!(second.ends_with("## Questions\n- why?\n"));
        let (third, report) = synchronize(Some(&second), "", &new, true, false, "paper");
        assert_eq!(third, second);
        assert_eq!(report.changed, 0);
    }
    #[test]
    fn user_edits_are_never_overwritten_or_deleted() {
        let (first, _) = synchronize(None, "", &[("one".into(), "Margin text\n".into())], true, false, "paper");
        let edited = first.replace("Margin text", "User text");
        for entries in [vec![], vec![("one".into(), "New Margin text\n".into())]] {
            let (result, report) = synchronize(Some(&edited), "", &entries, true, false, "paper");
            assert_eq!(result, edited);
            assert_eq!(report.preserved, ["one"]);
        }
    }
    #[test]
    fn partial_append_and_legacy_blocks_preserve_ownership() {
        let (first, _) = synchronize(None, "", &[("one".into(), "original\n".into())], true, false, "paper");
        let (partial, _) = synchronize(Some(&first), "", &[], false, false, "paper");
        assert_eq!(partial, first);
        let (append, _) = synchronize(Some(&first), "", &[("one".into(), "new\n".into())], true, true, "paper");
        assert_eq!(append, first);
        let legacy = "## Highlights\n<!-- margin:one -->\n> old\n\nMy text\n";
        let (kept, report) = synchronize(Some(legacy), "", &[("one".into(), "new\n".into())], true, false, "paper");
        assert_eq!(kept, legacy);
        assert_eq!(report.preserved.len(), 1);
    }
    #[test]
    fn adding_to_a_foreign_note_preserves_it_and_reading_order() {
        let entries = [("z".into(), "> first\n".into()), ("a".into(), "> second\n".into())];
        let (output, report) = synchronize(Some("# Paper\nPersonal thoughts"), "", &entries, false, false, "paper");
        assert_eq!(report.changed, 2);
        assert!(output.starts_with("# Paper\nPersonal thoughts\n\n## Highlights\n\n"));
        assert!(output.find("> first").unwrap() < output.find("> second").unwrap());
    }
    #[test]
    fn same_note_and_same_ids_from_different_papers_do_not_delete_each_other() {
        let (first, _) = synchronize(None, "", &[("same".into(), "Paper one\n".into())], true, false, "one.pdf");
        let (both, _) = synchronize(Some(&first), "", &[("same".into(), "Paper two\n".into())], true, false, "two.pdf");
        assert!(both.contains("Paper one\n") && both.contains("Paper two\n"));
        let (remaining, _) = synchronize(Some(&both), "", &[], true, false, "one.pdf");
        assert!(!remaining.contains("Paper one\n") && remaining.contains("Paper two\n"));
    }
    #[test]
    fn crlf_notes_and_headings_inside_blocks_do_not_duplicate_or_nest() {
        let (first, _) = synchronize(None, "", &[("one".into(), "## Inside a block\n".into())], true, false, "paper");
        let entries = [("one".into(), "## Inside a block\n".into()), ("two".into(), "Another block\n".into())];
        let (both, _) = synchronize(Some(&first), "", &entries, true, false, "paper");
        let closing = both.find("<!-- /margin:one -->").unwrap();
        assert!(closing < both.find("<!-- margin:two").unwrap());
        let windows = both.replace('\n', "\r\n");
        let (kept, report) = synchronize(Some(&windows), "", &entries, true, false, "paper");
        assert_eq!(kept, windows);
        assert_eq!(report.preserved.len(), 2);
    }
    #[test]
    fn templates_are_literal_bounded_and_context_safe() {
        let variables = BTreeMap::from([("title".into(), "../../evil\n{{secret}}<!-- margin:x -->".into())]);
        assert_eq!(render("{{missing}} / {{title}}", &variables, TemplateContext::Text).unwrap(), " / ../../evil\n{{secret}}&lt;!-- margin:x --&gt;");
        let path = render("reading/{{title}}.md", &variables, TemplateContext::Path).unwrap();
        assert!(super::super::note_path(&path, "").is_ok());
        assert!(!render("{{title}}", &variables, TemplateContext::FrontMatter).unwrap().contains('\n'));
        assert!(render("{{title", &variables, TemplateContext::Text).is_err());
        assert!(render(&"x".repeat(MAX_TEMPLATE_BYTES + 1), &variables, TemplateContext::Text).is_err());
    }
}
